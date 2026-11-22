use crate::models::IssueKey;
use reqwest::{RequestBuilder, Response, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, time::Duration};

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
    /// The IANA name of the profile's time zone, such as `Europe/Paris`.
    #[serde(default)]
    pub time_zone: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JiraUser {
    pub account_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Project {
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
struct Field {
    id: String,
    schema: Option<FieldSchema>,
}

#[derive(Deserialize)]
struct FieldSchema {
    custom: Option<String>,
}

// The type of the sprint field of Jira Software, whatever the field is named
const SPRINT_FIELD_TYPE: &str = "com.pyxis.greenhopper.jira:gh-sprint";

/// One page of `GET /worklog/updated`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorklogPage {
    pub values: Vec<UpdatedWorklog>,
    /// The time to ask from for the next page, or to continue from after the last.
    pub until: i64,
    pub last_page: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdatedWorklog {
    pub worklog_id: i64,
}

#[derive(Deserialize)]
struct ApproximateCount {
    count: u64,
}

#[derive(Deserialize)]
struct StatusOnly {
    fields: StatusField,
}

#[derive(Deserialize)]
struct StatusField {
    status: StatusName,
}

#[derive(Deserialize)]
struct StatusName {
    name: String,
}

#[derive(Deserialize)]
struct TransitionList {
    transitions: Vec<JiraTransition>,
}

#[derive(Deserialize)]
pub(crate) struct JiraTransition {
    pub id: String,
    pub name: String,
    pub to: TargetStatus,
    /// The fields of the transition's screen, by field id. Empty when it has none.
    #[serde(default)]
    pub fields: BTreeMap<String, TransitionField>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TargetStatus {
    pub name: String,
    pub status_category: TargetCategory,
}

#[derive(Deserialize)]
pub(crate) struct TargetCategory {
    pub key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransitionField {
    pub required: bool,
    #[serde(default)]
    pub has_default_value: bool,
    pub name: String,
    pub schema: Value,
    pub allowed_values: Option<Vec<Value>>,
}

/// Jira explains a refusal as `{"errorMessages": [..], "errors": {field: message}}`.
/// The user is shown the messages; any other body stays as it is.
fn readable_error(body: String) -> String {
    let Ok(answer) = serde_json::from_str::<Value>(&body) else {
        return body;
    };
    let general = answer["errorMessages"].as_array().into_iter().flatten();
    let by_field = answer["errors"]
        .as_object()
        .into_iter()
        .flat_map(|errors| errors.values());
    let messages: Vec<&str> = general.chain(by_field).filter_map(Value::as_str).collect();
    if messages.is_empty() {
        body
    } else {
        messages.join(" ")
    }
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

    /// The id of the field that holds the sprints of an issue: a custom field,
    /// so each site has its own. `None` for a site without sprints.
    pub(crate) async fn sprint_field(&self) -> Result<Option<String>, JiraError> {
        let fields: Vec<Field> = self.send(self.http.get(self.url("/field"))).await?;
        let sprint = fields.into_iter().find(|field| {
            let custom = field
                .schema
                .as_ref()
                .and_then(|schema| schema.custom.as_deref());
            custom == Some(SPRINT_FIELD_TYPE)
        });
        Ok(sprint.map(|field| field.id))
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

    /// The name of the status the issue has in Jira.
    pub(crate) async fn status(&self, key: &IssueKey) -> Result<String, JiraError> {
        let request = self
            .http
            .get(self.url(&format!("/issue/{key}")))
            .query(&[("fields", "status")]);
        let issue: StatusOnly = self.send(request).await?;
        Ok(issue.fields.status.name)
    }

    /// The issue as Jira has it now, with `fields`.
    pub(crate) async fn issue(&self, key: &IssueKey, fields: &[&str]) -> Result<Value, JiraError> {
        let request = self
            .http
            .get(self.url(&format!("/issue/{key}")))
            .query(&[("fields", fields.join(","))]);
        self.send(request).await
    }

    /// The transitions the issue offers from the status it has in Jira, each
    /// with the fields of its screen.
    pub(crate) async fn transitions(
        &self,
        key: &IssueKey,
    ) -> Result<Vec<JiraTransition>, JiraError> {
        let request = self
            .http
            .get(self.url(&format!("/issue/{key}/transitions")))
            .query(&[("expand", "transitions.fields")]);
        let list: TransitionList = self.send(request).await?;
        Ok(list.transitions)
    }

    /// `body` is the request body of Jira's transition call: the transition id
    /// and the fields its screen requires.
    pub(crate) async fn transition_issue(
        &self,
        key: &IssueKey,
        body: &Value,
    ) -> Result<(), JiraError> {
        let url = self.url(&format!("/issue/{key}/transitions"));
        // Jira answers 204 with no body
        self.execute(self.http.post(url).json(body)).await?;
        Ok(())
    }

    /// `body` is the request body of Jira's comment call. Returns the comment
    /// as Jira stored it.
    pub(crate) async fn add_comment(
        &self,
        key: &IssueKey,
        body: &Value,
    ) -> Result<Value, JiraError> {
        let url = self.url(&format!("/issue/{key}/comment"));
        self.send(self.http.post(url).json(body)).await
    }

    /// `body` is the request body of Jira's worklog call. Returns the worklog
    /// as Jira stored it.
    pub(crate) async fn add_worklog(
        &self,
        key: &IssueKey,
        body: &Value,
    ) -> Result<Value, JiraError> {
        let url = self.url(&format!("/issue/{key}/worklog"));
        self.send(self.http.post(url).json(body)).await
    }

    /// The worklogs of every issue that changed since `since` (Unix
    /// milliseconds), by id, one page at a time.
    pub(crate) async fn updated_worklogs(&self, since: i64) -> Result<WorklogPage, JiraError> {
        let request = self
            .http
            .get(self.url("/worklog/updated"))
            .query(&[("since", since.to_string())]);
        self.send(request).await
    }

    /// The ids of the worklogs deleted since `since` (Unix milliseconds), one
    /// page at a time. Jira keeps this list apart from `updated_worklogs`.
    pub(crate) async fn deleted_worklogs(&self, since: i64) -> Result<WorklogPage, JiraError> {
        let request = self
            .http
            .get(self.url("/worklog/deleted"))
            .query(&[("since", since.to_string())]);
        self.send(request).await
    }

    /// Every worklog of the issue with its properties, as Jira sent them. For an
    /// issue the cache has just got: `updated_worklogs` reports changes only.
    pub(crate) async fn issue_worklogs(&self, key: &IssueKey) -> Result<Vec<Value>, JiraError> {
        #[derive(Deserialize)]
        struct Page {
            total: usize,
            worklogs: Vec<Value>,
        }
        let mut worklogs = Vec::new();
        loop {
            let request = self
                .http
                .get(self.url(&format!("/issue/{key}/worklog")))
                .query(&[
                    ("startAt", worklogs.len().to_string()),
                    ("maxResults", "1000".to_owned()),
                    ("expand", "properties".to_owned()),
                ]);
            let page: Page = self.send(request).await?;
            let empty = page.worklogs.is_empty();
            worklogs.extend(page.worklogs);
            if empty || worklogs.len() >= page.total {
                return Ok(worklogs);
            }
        }
    }

    /// The worklogs with these ids and their properties, as Jira sent them.
    /// Jira takes at most a thousand ids, which is also the size of a page of
    /// `updated_worklogs`.
    pub(crate) async fn worklogs(&self, ids: &[i64]) -> Result<Vec<Value>, JiraError> {
        let request = self
            .http
            .post(self.url("/worklog/list"))
            .query(&[("expand", "properties")])
            .json(&json!({ "ids": ids }));
        self.send(request).await
    }

    /// The people Jira lets the issue be assigned to. Jira looks at the first
    /// thousand users only and filters those, so one request with the largest
    /// page is all there is to ask for.
    pub(crate) async fn assignable_users(
        &self,
        key: &IssueKey,
    ) -> Result<Vec<JiraUser>, JiraError> {
        let request = self.http.get(self.url("/user/assignable/search")).query(&[
            ("issueKey", key.to_string()),
            ("maxResults", "1000".to_owned()),
        ]);
        self.send(request).await
    }

    /// `body` is the request body of Jira's assignee call: the `accountId` of
    /// the new assignee, or `null` to leave the issue unassigned.
    pub(crate) async fn assign_issue(&self, key: &IssueKey, body: &Value) -> Result<(), JiraError> {
        let url = self.url(&format!("/issue/{key}/assignee"));
        // Jira answers 204 with no body
        self.execute(self.http.put(url).json(body)).await?;
        Ok(())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, JiraError> {
        Ok(self.execute(request).await?.json().await?)
    }

    /// Returns the response once Jira has answered with a success status.
    async fn execute(&self, request: RequestBuilder) -> Result<Response, JiraError> {
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
                    return Err(JiraError::Status(status, readable_error(body)));
                }
                _ => return Ok(response),
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
        let projects = client.projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].key, "PROJ");
        assert_eq!(projects[0].name, "Project");
        assert_eq!(
            client
                .search_page("project = PROJ", &["summary"], None)
                .await
                .unwrap()
                .issues
                .len(),
            1
        );
        assert_eq!(
            client.approximate_count("project = PROJ").await.unwrap(),
            2340
        );
    }

    #[tokio::test]
    async fn the_sprint_field_is_found_by_its_type_and_a_site_without_it_has_none() {
        let server = MockServer::start().await;
        let fields = json!([
            { "id": "summary", "name": "Summary", "schema": { "type": "string", "system": "summary" } },
            { "id": "issuekey", "name": "Key" },
            // Named by an admin: the name does not tell what the field is
            { "id": "customfield_10020", "name": "Iteration", "schema": {
                "type": "array", "items": "json",
                "custom": "com.pyxis.greenhopper.jira:gh-sprint", "customId": 10020,
            } },
        ]);
        Mock::given(method("GET"))
            .and(path("/rest/api/3/field"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&fields))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/field"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(&fields.as_array().unwrap()[..2]),
            )
            .mount(&server)
            .await;

        let with_sprints = client(&server).sprint_field().await.unwrap();
        let without = client(&server).sprint_field().await.unwrap();

        assert_eq!(with_sprints.as_deref(), Some("customfield_10020"));
        assert_eq!(without, None);
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
    async fn search_page_returns_the_token_of_the_next_page_and_sends_it_back() {
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

        let client = client(&server);
        let first = client.search_page(jql, &["summary"], None).await.unwrap();
        let second = client
            .search_page(jql, &["summary"], first.next_page_token.as_deref())
            .await
            .unwrap();

        assert_eq!(first.issues, issues(0..100));
        assert_eq!(second.issues, issues(100..150));
        assert_eq!(second.next_page_token, None);
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

    #[tokio::test]
    async fn the_assignable_users_of_an_issue_are_asked_for_by_its_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/user/assignable/search"))
            .and(basic_auth(EMAIL, TOKEN))
            .and(query_param("issueKey", "WEB-1"))
            .and(query_param("maxResults", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "accountId": "5b10a", "displayName": "Ada", "active": true, "self": "x" },
                { "accountId": "7c20b", "displayName": "Grace", "active": true },
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let users = client(&server)
            .assignable_users(&"WEB-1".into())
            .await
            .unwrap();

        let users: Vec<_> = users
            .iter()
            .map(|user| (user.account_id.as_str(), user.display_name.as_str()))
            .collect();
        assert_eq!(users, [("5b10a", "Ada"), ("7c20b", "Grace")]);
    }

    #[test]
    fn jiras_error_answer_is_reduced_to_its_messages() {
        let refused = r#"{"errorMessages":["Worklog must not be null."],"errors":{"timeLogged":"You must indicate the time spent working."}}"#;
        assert_eq!(
            readable_error(refused.to_owned()),
            "Worklog must not be null. You must indicate the time spent working."
        );
        assert_eq!(
            readable_error(r#"{"errorMessages":[],"errors":{}}"#.to_owned()),
            r#"{"errorMessages":[],"errors":{}}"#
        );
        assert_eq!(readable_error("No permission".to_owned()), "No permission");
    }

    #[tokio::test]
    async fn an_assignee_is_set_by_account_id_and_unset_by_null() {
        let server = MockServer::start().await;
        for body in [
            json!({ "accountId": "5b10a" }),
            json!({ "accountId": null }),
        ] {
            Mock::given(method("PUT"))
                .and(path("/rest/api/3/issue/WEB-1/assignee"))
                .and(basic_auth(EMAIL, TOKEN))
                .and(body_json(body))
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        let client = client(&server);

        client
            .assign_issue(&"WEB-1".into(), &json!({ "accountId": "5b10a" }))
            .await
            .unwrap();
        client
            .assign_issue(&"WEB-1".into(), &json!({ "accountId": null }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_refused_assignee_is_a_status_error_with_jiras_reason() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/WEB-1/assignee"))
            .respond_with(ResponseTemplate::new(400).set_body_string("user not found"))
            .mount(&server)
            .await;

        let error = client(&server)
            .assign_issue(&"WEB-1".into(), &json!({ "accountId": "nobody" }))
            .await
            .unwrap_err();

        assert!(
            matches!(error, JiraError::Status(StatusCode::BAD_REQUEST, body) if body == "user not found")
        );
    }
}
