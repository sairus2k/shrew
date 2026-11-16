#![cfg_attr(
    not(test),
    expect(dead_code, reason = "neither auth nor the sync engine calls Jira yet")
)]

use reqwest::{RequestBuilder, Response, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fmt, time::Duration};

const PAGE_SIZE: u32 = 100;
// Jira serves pages this large only when no field but the id is asked for
const ID_PAGE_SIZE: u32 = 5000;
const MAX_RATE_LIMIT_RETRIES: u32 = 3;

#[derive(Debug)]
pub(crate) enum JiraError {
    /// The API token is wrong or has expired (401).
    Unauthorized,
    Status(StatusCode, String),
    Http(reqwest::Error),
}

impl fmt::Display for JiraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("Jira rejected the API token"),
            Self::Status(status, body) => write!(f, "Jira returned {status}: {body}"),
            Self::Http(error) => write!(f, "Jira request failed: {error}"),
        }
    }
}

impl std::error::Error for JiraError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for JiraError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Myself {
    pub account_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Project {
    pub id: String,
    pub key: String,
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPage {
    values: Vec<Project>,
    is_last: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchPage {
    pub issues: Vec<Value>,
    /// `None` on the last page.
    pub next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdPage {
    issues: Vec<IssueId>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct IssueId {
    id: String,
}

#[derive(Deserialize)]
struct ApproximateCount {
    count: u64,
}

pub(crate) struct JiraClient {
    http: reqwest::Client,
    base_url: String,
    email: String,
    api_token: String,
}

impl JiraClient {
    /// `site_url` is the site root, such as `https://example.atlassian.net`.
    pub(crate) fn new(site_url: &str, email: &str, api_token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: format!("{}/rest/api/3", site_url.trim_end_matches('/')),
            email: email.to_owned(),
            api_token: api_token.to_owned(),
        }
    }

    pub(crate) async fn myself(&self) -> Result<Myself, JiraError> {
        self.send(self.http.get(self.url("/myself"))).await
    }

    pub(crate) async fn projects(&self) -> Result<Vec<Project>, JiraError> {
        let mut projects = Vec::new();
        loop {
            let request = self.http.get(self.url("/project/search")).query(&[
                ("orderBy", "name".to_owned()),
                ("maxResults", "50".to_owned()),
                ("startAt", projects.len().to_string()),
            ]);
            let page: ProjectPage = self.send(request).await?;
            // An empty page would never advance startAt
            let done = page.is_last || page.values.is_empty();
            projects.extend(page.values);
            if done {
                return Ok(projects);
            }
        }
    }

    /// Returns the issues of all pages as Jira sent them, with only `fields` filled in.
    pub(crate) async fn search(&self, jql: &str, fields: &[&str]) -> Result<Vec<Value>, JiraError> {
        let mut issues = Vec::new();
        let mut next_page_token = None;
        loop {
            let page = self
                .search_page(jql, fields, next_page_token.as_deref())
                .await?;
            issues.extend(page.issues);
            next_page_token = page.next_page_token;
            if next_page_token.is_none() {
                return Ok(issues);
            }
        }
    }

    /// One page of a search. Pass `None` for the first page, then the token of
    /// the page before.
    // Jira rejects parallel pagination: a page is reachable only through the
    // token of the one before it
    pub(crate) async fn search_page(
        &self,
        jql: &str,
        fields: &[&str],
        page_token: Option<&str>,
    ) -> Result<SearchPage, JiraError> {
        self.send(self.search_request(jql, fields, PAGE_SIZE, page_token))
            .await
    }

    /// The ids of every issue `jql` matches. An error on any page fails the
    /// whole listing: the caller never sees a part of it.
    pub(crate) async fn issue_ids(&self, jql: &str) -> Result<Vec<String>, JiraError> {
        let mut ids = Vec::new();
        let mut next_page_token: Option<String> = None;
        loop {
            let request =
                self.search_request(jql, &["id"], ID_PAGE_SIZE, next_page_token.as_deref());
            let page: IdPage = self.send(request).await?;
            ids.extend(page.issues.into_iter().map(|issue| issue.id));
            next_page_token = page.next_page_token;
            if next_page_token.is_none() {
                return Ok(ids);
            }
        }
    }

    fn search_request(
        &self,
        jql: &str,
        fields: &[&str],
        max_results: u32,
        page_token: Option<&str>,
    ) -> RequestBuilder {
        let mut body = json!({ "jql": jql, "fields": fields, "maxResults": max_results });
        if let Some(token) = page_token {
            body["nextPageToken"] = Value::String(token.to_owned());
        }
        self.http.post(self.url("/search/jql")).json(&body)
    }

    /// An estimate, good for a progress bar only. Jira requires a bounded JQL.
    pub(crate) async fn approximate_count(&self, jql: &str) -> Result<u64, JiraError> {
        let request = self
            .http
            .post(self.url("/search/approximate-count"))
            .json(&json!({ "jql": jql }));
        let count: ApproximateCount = self.send(request).await?;
        Ok(count.count)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, JiraError> {
        let request = request.basic_auth(&self.email, Some(&self.api_token));
        let mut retries = 0;
        loop {
            let response = request
                .try_clone()
                .expect("request bodies are JSON, never streams")
                .send()
                .await?;
            match response.status() {
                StatusCode::UNAUTHORIZED => return Err(JiraError::Unauthorized),
                StatusCode::TOO_MANY_REQUESTS if retries < MAX_RATE_LIMIT_RETRIES => {
                    retries += 1;
                    tokio::time::sleep(retry_after(&response)).await;
                }
                status if !status.is_success() => {
                    let body = response.text().await.unwrap_or_default();
                    return Err(JiraError::Status(status, body));
                }
                _ => return Ok(response.json().await?),
            }
        }
    }
}

fn retry_after(response: &Response) -> Duration {
    let seconds = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        // Jira sends the header as whole seconds; without it, wait a little anyway
        .unwrap_or(1);
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{basic_auth, body_json, method, path, query_param},
    };

    const EMAIL: &str = "dev@example.com";
    const TOKEN: &str = "test-token";

    fn client(server: &MockServer) -> JiraClient {
        JiraClient::new(&server.uri(), EMAIL, TOKEN)
    }

    fn issues(range: std::ops::Range<u32>) -> Vec<Value> {
        range.map(|n| json!({ "id": n.to_string() })).collect()
    }

    #[tokio::test]
    async fn every_endpoint_sends_basic_auth() {
        let server = MockServer::start().await;
        // Only authenticated requests match, so a missing header fails the call with a 404
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .and(basic_auth(EMAIL, TOKEN))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "accountId": "abc", "displayName": "Dev", "emailAddress": EMAIL }),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/project/search"))
            .and(basic_auth(EMAIL, TOKEN))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "isLast": true,
                "values": [{ "id": "10000", "key": "PROJ", "name": "Project" }],
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search/jql"))
            .and(basic_auth(EMAIL, TOKEN))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "issues": issues(0..1) })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/search/approximate-count"))
            .and(basic_auth(EMAIL, TOKEN))
            .and(body_json(json!({ "jql": "project = PROJ" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 2340 })))
            .mount(&server)
            .await;
        let client = client(&server);

        let myself = client.myself().await.unwrap();
        assert_eq!(myself.account_id, "abc");
        assert_eq!(myself.display_name, "Dev");
        let projects = client.projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, "10000");
        assert_eq!(projects[0].key, "PROJ");
        assert_eq!(projects[0].name, "Project");
        assert_eq!(
            client
                .search("project = PROJ", &["summary"])
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            client.approximate_count("project = PROJ").await.unwrap(),
            2340
        );
    }

    #[tokio::test]
    async fn projects_follow_start_at_until_the_last_page() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/project/search"))
            .and(query_param("startAt", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "isLast": false,
                "values": [{ "id": "1", "key": "A", "name": "Alpha" }],
            })))
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/project/search"))
            .and(query_param("startAt", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "isLast": true,
                "values": [{ "id": "2", "key": "B", "name": "Beta" }],
            })))
            .mount(&server)
            .await;

        let projects = client(&server).projects().await.unwrap();

        let keys: Vec<_> = projects
            .iter()
            .map(|project| project.key.as_str())
            .collect();
        assert_eq!(keys, ["A", "B"]);
    }

    #[tokio::test]
    async fn search_follows_next_page_token_and_returns_all_pages() {
        let server = MockServer::start().await;
        let jql = "project IN (PROJ)";
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_json(
                json!({ "jql": jql, "fields": ["summary"], "maxResults": 100 }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "issues": issues(0..100), "nextPageToken": "tok123" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_json(json!({
                "jql": jql, "fields": ["summary"], "maxResults": 100, "nextPageToken": "tok123",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "issues": issues(100..150) })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let found = client(&server).search(jql, &["summary"]).await.unwrap();

        assert_eq!(found, issues(0..150));
    }

    #[tokio::test]
    async fn issue_ids_lists_every_page_or_nothing() {
        let server = MockServer::start().await;
        let jql = "project IN (PROJ)";
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_json(
                json!({ "jql": jql, "fields": ["id"], "maxResults": 5000 }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "issues": issues(0..2), "nextPageToken": "tok123" })),
            )
            .mount(&server)
            .await;
        let second_page = || {
            Mock::given(path("/rest/api/3/search/jql")).and(body_json(json!({
                "jql": jql, "fields": ["id"], "maxResults": 5000, "nextPageToken": "tok123",
            })))
        };
        second_page()
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        second_page()
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "issues": issues(2..3) })),
            )
            .mount(&server)
            .await;
        let client = client(&server);

        let failed = client.issue_ids(jql).await.unwrap_err();
        let ids = client.issue_ids(jql).await.unwrap();

        assert!(matches!(
            failed,
            JiraError::Status(StatusCode::SERVICE_UNAVAILABLE, _)
        ));
        assert_eq!(ids, ["0", "1", "2"]);
    }

    #[tokio::test]
    async fn a_429_is_retried_after_the_retry_after_delay() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/search/approximate-count"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/search/approximate-count"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 7 })))
            .mount(&server)
            .await;

        let started = Instant::now();
        let count = client(&server)
            .approximate_count("project = PROJ")
            .await
            .unwrap();

        assert_eq!(count, 7);
        assert!(started.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn a_401_is_a_distinct_authentication_error() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/project/search"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let client = client(&server);

        let unauthorized = client.myself().await.unwrap_err();
        let forbidden = client.projects().await.unwrap_err();

        assert!(matches!(unauthorized, JiraError::Unauthorized));
        assert!(
            matches!(&forbidden, JiraError::Status(StatusCode::FORBIDDEN, body) if body == "forbidden")
        );
    }
}
