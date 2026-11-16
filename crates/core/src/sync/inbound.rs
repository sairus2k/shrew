//! Issues coming from Jira into the local cache.

use crate::{
    AppState, SyncEvent, adf,
    auth::{self, Config},
    db::issues::{self, IssueRecord, JiraTimestamp},
    models::{IssueKey, StatusCategory},
    sync::jira_client::JiraClient,
};
use anyhow::Context;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};

// How far before the last sync a poll looks. Jira's search index lags behind a
// write, and the local clock is not Jira's: an issue changed just before a poll
// can be missing from it. The upsert drops what is already stored.
const POLL_OVERLAP_MS: i64 = 60_000;

const RECONCILE_INTERVAL_MS: i64 = 60 * 60 * 1000;

const ISSUE_FIELDS: &[&str] = &[
    "summary",
    "description",
    "status",
    "assignee",
    "reporter",
    "priority",
    "labels",
    "comment",
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
    let started_at = now_ms()?;

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

/// Stores the issues Jira changed since `last_sync_at` and moves the saved sync
/// time forward. The keys of the rows that changed are added to `changed` page
/// by page, so the caller can announce them even when a later page fails.
pub(crate) async fn poll_updated_issues(
    state: &AppState,
    config: &Config,
    last_sync_at: i64,
    changed: &mut Vec<IssueKey>,
) -> anyhow::Result<()> {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let started_at = now_ms()?;
    let jql = incremental_jql(&config.watched_projects, last_sync_at, started_at);

    let mut page_token = None;
    loop {
        let page = client
            .search_page(&jql, ISSUE_FIELDS, page_token.as_deref())
            .await?;
        let mut written = Vec::new();
        let mut tx = state.pool.begin().await?;
        for issue in &page.issues {
            let record = issue_record(issue)?;
            if issues::upsert(&mut *tx, &record).await? {
                written.push(record.key);
            }
        }
        tx.commit().await?;
        changed.extend(written);

        if page.next_page_token.is_none() {
            break;
        }
        page_token = page.next_page_token;
    }

    let mut saved = state.config.write().await;
    // A sign-in during the poll starts a new setup: its sync time is not ours to set
    if let Some(current) = saved
        .as_mut()
        .filter(|current| current.last_sync_at == Some(last_sync_at))
    {
        current.last_sync_at = Some(started_at);
        auth::save_config(&state.data_dir, current).await?;
    }
    Ok(())
}

// The span is relative ("-5m"), which Jira counts back from its own clock. A
// JQL date string would be read in the time zone of the user's Jira profile.
fn incremental_jql(watched_projects: &[String], last_sync_at: i64, now: i64) -> String {
    // Rounded up to whole minutes, and at least one if the clock was set back
    let minutes = ((now - last_sync_at + POLL_OVERLAP_MS) / 60_000 + 1).max(1);
    format!(
        "{} AND updated >= \"-{minutes}m\" ORDER BY updated DESC",
        in_projects(watched_projects),
    )
}

fn in_projects(watched_projects: &[String]) -> String {
    let projects: Vec<String> = watched_projects
        .iter()
        .map(|key| format!("\"{key}\""))
        .collect();
    format!("project IN ({})", projects.join(", "))
}

/// Whether the pass for deleted and moved issues should run in this sync
/// cycle: in the first one after the app started, then once an hour.
pub(crate) fn reconcile_due(config: &Config, reconciled_since_start: bool, now: i64) -> bool {
    // A saved time in the future means the clock was set back: waiting for it
    // to pass could take days
    !reconciled_since_start
        || !(0..RECONCILE_INTERVAL_MS).contains(&(now - config.last_reconcile_at))
}

/// Removes the cached issues that Jira no longer has in the watched projects:
/// deleted, moved to an unwatched project or hidden from the user. Returns
/// their keys.
pub(crate) async fn reconcile_removed_issues(
    state: &AppState,
    config: &Config,
) -> anyhow::Result<Vec<IssueKey>> {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let started_at = now_ms()?;
    // A failed page ends the pass here. A partial listing would read as "the
    // rest was deleted".
    let remote: HashSet<String> = client
        .issue_ids(&in_projects(&config.watched_projects))
        .await?
        .into_iter()
        .collect();

    let mut saved = state.config.write().await;
    // After a sign-in or a change of the watched projects during the listing,
    // the cache is no longer the one these ids describe
    let Some(current) = saved.as_mut().filter(|current| {
        current.site_url == config.site_url
            && current.account_id == config.account_id
            && current.watched_projects == config.watched_projects
    }) else {
        return Ok(Vec::new());
    };

    let mut removed = Vec::new();
    let mut tx = state.pool.begin().await?;
    // Ids that only Jira has are new issues: the incremental poll stores them
    for issue in issues::cached(&mut *tx).await? {
        if config.watched_projects.contains(&issue.project_key) && !remote.contains(&issue.id) {
            issues::delete(&mut *tx, &issue.id).await?;
            removed.push(issue.key);
        }
    }
    tx.commit().await?;

    current.last_reconcile_at = started_at;
    auth::save_config(&state.data_dir, current).await?;
    Ok(removed)
}

pub(crate) fn now_ms() -> anyhow::Result<i64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api,
        models::{SetupStep, SyncStatus},
        start_syncing_without_keychain, start_without_keychain,
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
        let (state, events) = start_syncing_without_keychain(data_dir).await.unwrap();
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

    /// A core that watches WEB and API and has finished the initial sync,
    /// which stored `issues`.
    async fn synced(
        server: &MockServer,
        data_dir: &Path,
        issues: &[Value],
    ) -> (Arc<AppState>, UnboundedReceiver<SyncEvent>) {
        let (state, mut events) = watching(server, data_dir, &["WEB", "API"]).await;
        mount_count(server, "WEB", 0).await;
        mount_page(server, "WEB", None, page(issues, None)).await;
        mount_count(server, "API", 0).await;
        mount_page(server, "API", None, page(&[], None)).await;
        api::initial_sync(&state).await.unwrap();
        received(&mut events);
        listing(ids(1..10)).mount(server).await;
        (state, events)
    }

    /// The id listing of the watched projects, answered with `response`. The
    /// one `synced` mounts has every id a test stores, so its pass removes
    /// nothing; a test that mounts its own gives it a higher priority.
    fn listing(response: ResponseTemplate) -> Mock {
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_json(json!({
                "jql": "project IN (\"WEB\", \"API\")",
                "fields": ["id"],
                "maxResults": 5000,
            })))
            .respond_with(response)
    }

    fn ids(range: std::ops::Range<u32>) -> ResponseTemplate {
        let issues: Vec<Value> = range.map(|id| json!({ "id": id.to_string() })).collect();
        page(&issues, None)
    }

    async fn last_reconcile_at(state: &AppState) -> i64 {
        state
            .config
            .read()
            .await
            .as_ref()
            .unwrap()
            .last_reconcile_at
    }

    async fn last_sync_at(state: &AppState) -> i64 {
        let config = state.config.read().await;
        config.as_ref().unwrap().last_sync_at.unwrap()
    }

    /// The poll for the last `minutes`, answered with `response`.
    fn poll(minutes: u32, response: ResponseTemplate) -> Mock {
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_json(json!({
                "jql": format!(
                    "project IN (\"WEB\", \"API\") AND updated >= \"-{minutes}m\" ORDER BY updated DESC"
                ),
                "fields": ISSUE_FIELDS,
                "maxResults": 100,
            })))
            .respond_with(response)
    }

    // A poll right after a sync: the overlap, rounded up
    const JUST_SYNCED: u32 = 2;

    fn changed(mut issue: Value, updated: &str, summary: &str) -> Value {
        issue["fields"]["updated"] = json!(updated);
        issue["fields"]["summary"] = json!(summary);
        issue
    }

    /// The events of one sync cycle, up to the one that ends it.
    async fn cycle(events: &mut UnboundedReceiver<SyncEvent>) -> Vec<String> {
        let mut cycle = Vec::new();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .expect("the sync cycle did not end")
                .unwrap();
            let last = matches!(
                event,
                SyncEvent::Completed | SyncEvent::Error(_) | SyncEvent::TokenRejected
            );
            cycle.push(format!("{event:?}"));
            if last {
                return cycle;
            }
        }
    }

    async fn summary(state: &AppState, key: &str) -> String {
        let row = issues::get(&state.pool, &key.into()).await.unwrap();
        row.unwrap().summary
    }

    const LATER: &str = "2026-03-29T09:00:00.000+0000";
    const EARLIER: &str = "2026-03-28T09:00:00.000+0000";

    #[tokio::test]
    async fn a_poll_stores_the_issues_jira_changed_since_the_last_sync() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB")];
        let (state, mut events) = synced(&server, data_dir.path(), &stored).await;
        let before = last_sync_at(&state).await;
        // The overlap brings WEB-2 back although it has not changed
        let polled = [
            changed(issue(1, "WEB"), LATER, "Renamed"),
            issue(2, "WEB"),
            issue(5, "API"),
        ];
        poll(JUST_SYNCED, page(&polled, None)).mount(&server).await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1"), IssueKey("API-5")])"#,
                "Completed",
            ]
        );
        assert_eq!(summary(&state, "WEB-1").await, "Renamed");
        assert_eq!(stored_keys(&state).await, ["API-5", "WEB-1", "WEB-2"]);
        let now = last_sync_at(&state).await;
        assert!(now > before);
        let saved = std::fs::read(data_dir.path().join("config.json")).unwrap();
        let saved: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(saved["last_sync_at"], json!(now));
    }

    #[tokio::test]
    async fn a_poll_runs_by_itself_when_the_interval_is_over() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (_state, mut events) = synced(&server, data_dir.path(), &[]).await;
        // Only the timer of the engine is moved: the wall clock, which the span
        // of the poll is counted from, stays
        poll(JUST_SYNCED, page(&[issue(1, "WEB")], None))
            .mount(&server)
            .await;
        assert!(events.try_recv().is_err());

        tokio::time::pause();
        tokio::time::advance(crate::sync::engine::POLL_INTERVAL).await;
        tokio::time::resume();

        assert_eq!(
            cycle(&mut events).await[1],
            r#"IssuesUpdated([IssueKey("WEB-1")])"#
        );
    }

    #[tokio::test]
    async fn a_poll_reads_every_page() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[]).await;
        // The second page is asked for with the token, so the first mock no longer matches
        poll(JUST_SYNCED, page(&[issue(1, "WEB")], Some("tok")))
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/search/jql"))
            .and(wiremock::matchers::body_partial_json(
                json!({ "nextPageToken": "tok" }),
            ))
            .respond_with(page(&[issue(2, "WEB")], None))
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await[1],
            r#"IssuesUpdated([IssueKey("WEB-1"), IssueKey("WEB-2")])"#
        );
    }

    #[tokio::test]
    async fn a_poll_keeps_the_local_row_when_jira_sends_an_older_version() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        let stale = changed(issue(1, "WEB"), EARLIER, "Stale title");
        poll(JUST_SYNCED, page(&[stale], None)).mount(&server).await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
        assert_eq!(summary(&state, "WEB-1").await, "Issue number 1");
    }

    #[tokio::test]
    async fn an_issue_moved_to_another_watched_project_keeps_its_row() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        let mut moved = changed(issue(1, "API"), LATER, "Moved");
        moved["key"] = json!("API-9");
        poll(JUST_SYNCED, page(&[moved], None)).mount(&server).await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("API-9")])"#,
                "Completed"
            ]
        );
        assert_eq!(stored_keys(&state).await, ["API-9"]);
        let row = issues::get(&state.pool, &"API-9".into()).await.unwrap();
        assert_eq!(row.unwrap().project_key, "API");
    }

    #[tokio::test]
    async fn a_failed_poll_reports_the_error_and_the_next_one_still_runs() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        let before = last_sync_at(&state).await;
        poll(JUST_SYNCED, ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let renamed = changed(issue(1, "WEB"), LATER, "Renamed");
        poll(JUST_SYNCED, page(&[renamed], None))
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        let failed = cycle(&mut events).await;
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert_eq!(failed[0], "Started");
        assert!(failed[1].starts_with("Error(") && failed[1].contains("503"));
        // The failed poll did not move the sync time: the next one asks for the same span
        assert_eq!(last_sync_at(&state).await, before);

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        assert_eq!(summary(&state, "WEB-1").await, "Renamed");
    }

    #[tokio::test]
    async fn the_first_poll_after_a_restart_continues_from_the_saved_sync_time() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        state.pool.close().await;
        // The app was last synced three hours ago
        let config_file = data_dir.path().join("config.json");
        let mut config: Value =
            serde_json::from_slice(&std::fs::read(&config_file).unwrap()).unwrap();
        config["last_sync_at"] = json!(now_ms().unwrap() - 3 * 60 * 60 * 1000);
        std::fs::write(&config_file, config.to_string()).unwrap();

        // Only a poll for those three hours is answered; any other gets a 404
        server.reset().await;
        let renamed = changed(issue(1, "WEB"), LATER, "Renamed");
        poll(182, page(&[renamed], None)).mount(&server).await;
        listing(ids(1..2)).mount(&server).await;
        let (state, mut events) = start_syncing_without_keychain(data_dir.path())
            .await
            .unwrap();

        // Nothing wakes the engine: it polls on start
        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        assert_eq!(summary(&state, "WEB-1").await, "Renamed");
    }

    #[tokio::test]
    async fn a_401_pauses_the_sync_until_a_new_token_is_saved() {
        use wiremock::matchers::basic_auth;
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        // Jira has stopped accepting the saved token. It is asked once: a
        // second request with that token would fail the test.
        Mock::given(path("/rest/api/3/search/jql"))
            .and(basic_auth("ada@example.com", "secret-token"))
            .respond_with(ResponseTemplate::new(401))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/myself"))
            .and(basic_auth("ada@example.com", "mistyped"))
            .respond_with(ResponseTemplate::new(401))
            .with_priority(1)
            .mount(&server)
            .await;
        let renamed = changed(issue(1, "WEB"), LATER, "Renamed");
        poll(JUST_SYNCED, page(&[renamed], None))
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(cycle(&mut events).await, ["Started", "TokenRejected"]);

        state.sync_waker.send(()).unwrap();
        let error = api::renew_token(&state, "mistyped").await.unwrap_err();

        assert_eq!(error.to_string(), "Jira rejected the API token");
        assert_eq!(
            state.tokens.load().unwrap().as_deref(),
            Some("secret-token")
        );
        let quiet = tokio::time::timeout(std::time::Duration::from_millis(200), events.recv());
        assert!(quiet.await.is_err(), "the paused engine sent an event");

        api::renew_token(&state, " new-token ").await.unwrap();

        // Nothing but the saved token wakes the engine
        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        assert_eq!(state.tokens.load().unwrap().as_deref(), Some("new-token"));
    }

    #[tokio::test]
    async fn an_issue_deleted_in_jira_is_removed_from_the_cache_and_from_search() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB")];
        let (state, mut events) = synced(&server, data_dir.path(), &stored).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        // Jira no longer has WEB-2, and has an issue the cache has not seen yet
        listing(page(&[json!({ "id": "1" }), json!({ "id": "7" })], None))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(last_reconcile_at(&state).await, 0);

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-2")])"#,
                "Completed"
            ]
        );
        // The unseen issue is left to the poll
        assert_eq!(stored_keys(&state).await, ["WEB-1"]);
        let found = issues::search(&state.pool, "number", None, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "WEB-1".into());
        let passed_at = last_reconcile_at(&state).await;
        assert!(passed_at > 0);
        let saved = std::fs::read(data_dir.path().join("config.json")).unwrap();
        let saved: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(saved["last_reconcile_at"], json!(passed_at));

        // The next cycle is within the hour: the listing is not asked for again
        state.sync_waker.send(()).unwrap();

        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
    }

    #[tokio::test]
    async fn an_issue_moved_to_an_unwatched_project_is_removed() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB")];
        let (state, mut events) = synced(&server, data_dir.path(), &stored).await;
        // WEB-2 became OPS-5. The move changed it, but the poll asks for the
        // watched projects only, and so does the listing.
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        listing(ids(1..2)).with_priority(1).mount(&server).await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-2")])"#,
                "Completed"
            ]
        );
        assert_eq!(stored_keys(&state).await, ["WEB-1"]);
    }

    #[tokio::test]
    async fn a_partial_listing_removes_nothing() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB"), issue(3, "WEB")];
        let (state, mut events) = synced(&server, data_dir.path(), &stored).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        // The first page has WEB-1, the second fails once and then has WEB-2:
        // only WEB-3 is gone
        listing(page(&[json!({ "id": "1" })], Some("more")))
            .with_priority(1)
            .mount(&server)
            .await;
        let second_page = || {
            Mock::given(path("/rest/api/3/search/jql")).and(wiremock::matchers::body_partial_json(
                json!({ "nextPageToken": "more" }),
            ))
        };
        second_page()
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        second_page()
            .respond_with(page(&[json!({ "id": "2" })], None))
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        let failed = cycle(&mut events).await;
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(failed[1].starts_with("Error(") && failed[1].contains("503"));
        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-2", "WEB-3"]);
        assert_eq!(last_reconcile_at(&state).await, 0);

        // The pass is still due, and the next cycle runs it in full
        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-3")])"#,
                "Completed"
            ]
        );
        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-2"]);
    }

    #[test]
    fn the_pass_runs_on_start_then_hourly() {
        let mut config: Config = serde_json::from_value(json!({
            "site_url": "http://localhost",
            "email": "ada@example.com",
            "account_id": "5b10a",
        }))
        .unwrap();
        const HOUR: i64 = 60 * 60 * 1000;
        config.last_reconcile_at = 10 * HOUR;

        // The app has just started: the saved time does not matter
        assert!(reconcile_due(&config, false, 10 * HOUR + 1));
        assert!(!reconcile_due(&config, true, 10 * HOUR + 1));
        assert!(!reconcile_due(&config, true, 11 * HOUR - 1));
        assert!(reconcile_due(&config, true, 11 * HOUR));
        // The clock was set back since the last pass
        assert!(reconcile_due(&config, true, 9 * HOUR));
    }

    #[test]
    fn a_poll_asks_for_whole_minutes_since_the_last_sync_plus_the_overlap() {
        let watched = ["WEB".to_owned()];
        let jql = |since, now| incremental_jql(&watched, since, now);

        assert_eq!(
            jql(0, 180_000),
            r#"project IN ("WEB") AND updated >= "-5m" ORDER BY updated DESC"#
        );
        assert!(jql(0, 179_999).contains(r#""-4m""#));
        // The clock was set back since the last sync
        assert!(jql(10_000_000, 0).contains(r#""-1m""#));
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
