//! Issues coming from Jira into the local cache.

use crate::{
    AppState, SyncEvent, adf, auth,
    db::issues::{self, IssueRecord, JiraTimestamp},
    models::StatusCategory,
    sync::jira_client::JiraClient,
};
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

const ISSUE_FIELDS: &[&str] = &[
    "summary",
    "description",
    "status",
    "assignee",
    "project",
    "issuetype",
    "updated",
];

#[derive(Deserialize)]
struct JiraIssue {
    id: String,
    key: String,
    fields: Fields,
}

#[derive(Deserialize)]
struct Fields {
    summary: String,
    description: Option<Value>,
    status: Status,
    assignee: Option<User>,
    project: ProjectRef,
    issuetype: Option<IssueType>,
    updated: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    name: String,
    status_category: Category,
}

#[derive(Deserialize)]
struct Category {
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    account_id: String,
    display_name: String,
}

#[derive(Deserialize)]
struct ProjectRef {
    key: String,
}

#[derive(Deserialize)]
struct IssueType {
    name: String,
}

/// `issue` is one element of a search page, fetched with `ISSUE_FIELDS`.
pub(crate) fn issue_record(issue: &Value) -> anyhow::Result<IssueRecord> {
    let JiraIssue { id, key, fields } = JiraIssue::deserialize(issue)
        .with_context(|| format!("unexpected issue from Jira: {}", issue["key"]))?;
    let description_text = adf::to_plain_text(fields.description.as_ref());
    let (assignee_id, assignee_name) = fields
        .assignee
        .map(|user| (user.account_id, user.display_name))
        .unzip();
    Ok(IssueRecord {
        id,
        key: key.as_str().into(),
        summary: fields.summary,
        description_text: (!description_text.is_empty()).then_some(description_text),
        status: fields.status.name,
        status_category: match fields.status.status_category.key.as_str() {
            "indeterminate" => StatusCategory::InProgress,
            "done" => StatusCategory::Done,
            // "new", and "undefined" for a status with no category
            _ => StatusCategory::Todo,
        },
        assignee_id,
        assignee_name,
        project_key: fields.project.key,
        issue_type: fields.issuetype.map(|issue_type| issue_type.name),
        raw_json: issue.to_string(),
        // chrono is built without `std`, so its error is not a `std::error::Error`
        remote_updated_at: JiraTimestamp::parse(&fields.updated)
            .map_err(|error| anyhow::anyhow!("{key} has an unreadable `updated`: {error}"))?,
    })
}

/// Downloads every issue of the watched projects. The sync counts as done only
/// once `last_sync_at` is saved, so after an error or a quit the next call
/// starts over; issues that are already stored are skipped by the upsert.
pub(crate) async fn initial_sync(state: &AppState) -> anyhow::Result<()> {
    let mut config = state.config.read().await.clone().context("Sign in first")?;
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    // An issue that changes while the pages are read can be missed by them. The
    // first incremental sync starts from this moment and picks it up.
    let started_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;

    for project_key in &config.watched_projects {
        let jql = format!("project = \"{project_key}\" ORDER BY updated DESC");
        let approximate_total = client.approximate_count(&jql).await?;
        let mut synced = 0;
        let report = |synced| {
            let progress = SyncEvent::InitialSyncProgress {
                project_key: project_key.clone(),
                synced,
                approximate_total,
            };
            state.events.send(progress).ok();
        };

        report(synced);
        let mut page_token = None;
        loop {
            let page = client
                .search_page(&jql, ISSUE_FIELDS, page_token.as_deref())
                .await?;
            // One transaction per page: a commit per issue would make the disk
            // the slowest part of the sync
            let mut tx = state.pool.begin().await?;
            for issue in &page.issues {
                issues::upsert(&mut *tx, &issue_record(issue)?).await?;
            }
            tx.commit().await?;
            synced += page.issues.len() as u64;
            report(synced);

            // The approximate total is never the reason to stop
            if page.next_page_token.is_none() {
                break;
            }
            page_token = page.next_page_token;
        }
    }

    config.last_sync_at = Some(started_at);
    auth::save_config(&state.data_dir, &config).await?;
    *state.config.write().await = Some(config);
    state.events.send(SyncEvent::InitialSyncComplete).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api,
        models::{SetupStep, SyncStatus},
        start_without_keychain,
    };
    use serde_json::json;
    use std::{path::Path, sync::Arc};
    use tokio::sync::mpsc::UnboundedReceiver;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, method, path},
    };

    fn issue(id: u32, project: &str) -> Value {
        json!({
            "id": id.to_string(),
            "key": format!("{project}-{id}"),
            "fields": {
                "summary": format!("Issue number {id}"),
                "description": {
                    "type": "doc",
                    "version": 1,
                    "content": [{
                        "type": "paragraph",
                        "content": [{ "type": "text", "text": "Flaky login" }],
                    }],
                },
                "status": { "name": "In Review", "statusCategory": { "key": "indeterminate" } },
                "assignee": { "accountId": "5b10a", "displayName": "Ada" },
                "project": { "key": project },
                "issuetype": { "name": "Bug" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    async fn mount_count(server: &MockServer, project: &str, count: u64) {
        Mock::given(path("/rest/api/3/search/approximate-count"))
            .and(body_json(json!({
                "jql": format!("project = \"{project}\" ORDER BY updated DESC"),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": count })))
            .mount(server)
            .await;
    }

    /// The page of `project` that is asked for with `token`, answered with `response`.
    async fn mount_page(
        server: &MockServer,
        project: &str,
        token: Option<&str>,
        response: ResponseTemplate,
    ) {
        let mut body = json!({
            "jql": format!("project = \"{project}\" ORDER BY updated DESC"),
            "fields": ISSUE_FIELDS,
            "maxResults": 100,
        });
        if let Some(token) = token {
            body["nextPageToken"] = json!(token);
        }
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_json(body))
            .respond_with(response)
            .expect(1)
            .mount(server)
            .await;
    }

    fn page(issues: &[Value], next_page_token: Option<&str>) -> ResponseTemplate {
        let mut body = json!({ "issues": issues });
        if let Some(token) = next_page_token {
            body["nextPageToken"] = json!(token);
        }
        ResponseTemplate::new(200).set_body_json(body)
    }

    /// A core that is signed in to `server` and watches `projects`.
    async fn watching(
        server: &MockServer,
        data_dir: &Path,
        projects: &[&str],
    ) -> (Arc<AppState>, UnboundedReceiver<SyncEvent>) {
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" })),
            )
            .mount(server)
            .await;
        let (state, events) = start_without_keychain(data_dir).await.unwrap();
        api::sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        let projects: Vec<String> = projects.iter().map(|key| key.to_string()).collect();
        api::watch_projects(&state, &projects).await.unwrap();
        (state, events)
    }

    fn received(events: &mut UnboundedReceiver<SyncEvent>) -> Vec<String> {
        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            received.push(match event {
                SyncEvent::InitialSyncProgress {
                    project_key,
                    synced,
                    approximate_total,
                } => format!("{project_key} {synced} / ~{approximate_total}"),
                other => format!("{other:?}"),
            });
        }
        received
    }

    async fn stored_keys(state: &AppState) -> Vec<String> {
        sqlx::query_scalar("SELECT key FROM issues ORDER BY key")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn initial_sync_stores_every_page_of_every_watched_project() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = watching(&server, data_dir.path(), &["WEB", "API"]).await;
        // The estimate for WEB is off: the pages hold 3 issues, not 5
        mount_count(&server, "WEB", 5).await;
        let first = [issue(1, "WEB"), issue(2, "WEB")];
        mount_page(&server, "WEB", None, page(&first, Some("tok"))).await;
        mount_page(&server, "WEB", Some("tok"), page(&[issue(3, "WEB")], None)).await;
        mount_count(&server, "API", 1).await;
        mount_page(&server, "API", None, page(&[issue(4, "API")], None)).await;
        assert_eq!(api::setup_step(&state).await, SetupStep::InitialSync);

        api::initial_sync(&state).await.unwrap();

        assert_eq!(
            stored_keys(&state).await,
            ["API-4", "WEB-1", "WEB-2", "WEB-3"]
        );
        assert_eq!(
            received(&mut events),
            [
                "WEB 0 / ~5",
                "WEB 2 / ~5",
                "WEB 3 / ~5",
                "API 0 / ~1",
                "API 1 / ~1",
                "InitialSyncComplete",
            ]
        );
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);

        let stored = issues::get(&state.pool, &"WEB-1".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, "1");
        assert_eq!(stored.summary, "Issue number 1");
        assert_eq!(stored.status, "In Review");
        assert_eq!(stored.status_category, StatusCategory::InProgress);
        assert_eq!(stored.assignee_name.as_deref(), Some("Ada"));
        assert_eq!(stored.project_key, "WEB");
        assert_eq!(stored.sync_status, SyncStatus::Synced);
        // The description went through ADF into the search index
        let found = issues::search(&state.pool, "flaky", Some("API"), 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "API-4".into());
    }

    #[tokio::test]
    async fn an_interrupted_initial_sync_is_completed_by_the_next_start() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = watching(&server, data_dir.path(), &["WEB"]).await;
        mount_count(&server, "WEB", 3).await;
        let first = [issue(1, "WEB"), issue(2, "WEB")];
        mount_page(&server, "WEB", None, page(&first, Some("tok"))).await;
        mount_page(&server, "WEB", Some("tok"), ResponseTemplate::new(503)).await;

        let error = api::initial_sync(&state).await.unwrap_err();

        assert!(error.to_string().contains("503"), "{error}");
        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-2"]);
        assert_eq!(received(&mut events), ["WEB 0 / ~3", "WEB 2 / ~3"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::InitialSync);
        state.pool.close().await;

        // Jira is back, and the app is started again
        server.reset().await;
        mount_count(&server, "WEB", 3).await;
        mount_page(&server, "WEB", None, page(&first, Some("tok"))).await;
        mount_page(&server, "WEB", Some("tok"), page(&[issue(3, "WEB")], None)).await;
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(api::setup_step(&state).await, SetupStep::InitialSync);

        api::initial_sync(&state).await.unwrap();

        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-2", "WEB-3"]);
        assert_eq!(received(&mut events).last().unwrap(), "InitialSyncComplete");
        state.pool.close().await;

        // A finished sync is not repeated by the start after it
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
    }

    #[test]
    fn an_issue_with_only_the_required_fields_is_accepted() {
        let mut bare = issue(7, "WEB");
        bare["fields"]["description"] = Value::Null;
        bare["fields"]["assignee"] = Value::Null;
        bare["fields"]["issuetype"] = Value::Null;
        bare["fields"]["status"]["statusCategory"]["key"] = json!("undefined");

        let record = issue_record(&bare).unwrap();

        assert_eq!(record.description_text, None);
        assert_eq!(record.assignee_id, None);
        assert_eq!(record.assignee_name, None);
        assert_eq!(record.issue_type, None);
        assert_eq!(record.status_category, StatusCategory::Todo);
    }

    #[test]
    fn status_categories_follow_jira() {
        for (jira, ours) in [
            ("new", StatusCategory::Todo),
            ("indeterminate", StatusCategory::InProgress),
            ("done", StatusCategory::Done),
        ] {
            let mut issue = issue(7, "WEB");
            issue["fields"]["status"]["statusCategory"]["key"] = json!(jira);
            assert_eq!(issue_record(&issue).unwrap().status_category, ours);
        }
    }
}
