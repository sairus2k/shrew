//! Issues coming from Jira into the local cache.

use crate::{
    AppState, SyncEvent, adf,
    auth::{self, Config},
    db::{
        issues::{self, IssueRecord, JiraTimestamp},
        sync_queue, timer,
        worklogs::{self, RemoteWorklog},
    },
    models::{IssueKey, StatusCategory},
    sync::{jira_client::JiraClient, outbound},
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

const ISSUE_GONE: &str = "The issue no longer exists in Jira or is not visible to you";
const COMMENT_DISCARDED: &str =
    "The issue no longer exists in Jira or is not visible to you, so the comment was discarded";

pub(crate) const ISSUE_FIELDS: &[&str] = &[
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
        status_category: status_category(&fields.status.status_category.key),
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

/// An issue the sync cannot read is announced and left out: failing the page
/// would make every later cycle fail on the same issue.
fn readable_record(state: &AppState, issue: &Value) -> Option<IssueRecord> {
    match issue_record(issue) {
        Ok(record) => Some(record),
        Err(error) => {
            state
                .events
                .send(SyncEvent::IssueSkipped {
                    key: issue["key"].as_str().unwrap_or("unknown").to_owned(),
                    reason: format!("{error:#}"),
                })
                .ok();
            None
        }
    }
}

/// `key` is the key of a status category in Jira.
pub(crate) fn status_category(key: &str) -> StatusCategory {
    match key {
        "indeterminate" => StatusCategory::InProgress,
        "done" => StatusCategory::Done,
        // "new", and "undefined" for a status with no category
        _ => StatusCategory::Todo,
    }
}

/// Downloads every issue of the watched projects, or after the first sync only
/// of the pending ones. The sync counts as done only once `last_sync_at` is
/// saved and no project is pending, so after an error or a quit the next call
/// starts over; issues that are already stored are skipped by the upsert.
pub(crate) async fn initial_sync(state: &AppState) -> anyhow::Result<()> {
    let config = state.config.read().await.clone().context("Sign in first")?;
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    // An issue that changes while the pages are read can be missed by them. The
    // first incremental sync starts from this moment and picks it up.
    let started_at = now_ms()?;

    let to_download = match config.last_sync_at {
        Some(_) => &config.pending_projects,
        None => &config.watched_projects,
    };
    for project_key in to_download {
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
            let records: Vec<IssueRecord> = page
                .issues
                .iter()
                .filter_map(|issue| readable_record(state, issue))
                .collect();
            // The worklog poll of the first setup starts from zero and brings
            // them all; one that has moved on never will
            let listed = match config.last_sync_at {
                Some(_) => worklogs_of(&client, records.iter()).await?,
                None => Vec::new(),
            };
            // Held until the commit, as in `poll_updated_issues`: a page is
            // either stored before `watch_projects` deletes the rows of a
            // removed project or not stored at all
            let saved = state.config.read().await;
            let watched = saved
                .as_ref()
                .is_some_and(|current| current.watched_projects.contains(project_key));
            if !watched {
                break;
            }
            // One transaction per page: a commit per issue would make the disk
            // the slowest part of the sync
            let mut tx = state.pool.begin().await?;
            for record in &records {
                issues::upsert(&mut *tx, record).await?;
                synced += 1;
            }
            store_worklogs(state, &mut tx, &listed, &mut Vec::new()).await?;
            tx.commit().await?;
            drop(saved);
            report(synced);

            // The approximate total is never the reason to stop
            if page.next_page_token.is_none() {
                break;
            }
            page_token = page.next_page_token;
        }

        // The saved config may have moved on meanwhile: only this project is ours to clear
        let mut saved = state.config.write().await;
        if let Some(current) = saved.as_mut() {
            current.pending_projects.retain(|key| key != project_key);
            auth::save_config(&state.data_dir, current).await?;
        }
    }

    let mut saved = state.config.write().await;
    if let Some(current) = saved.as_mut() {
        current.last_sync_at.get_or_insert(started_at);
        auth::save_config(&state.data_dir, current).await?;
    }
    drop(saved);
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
        let records: Vec<IssueRecord> = page
            .issues
            .iter()
            .filter_map(|issue| readable_record(state, issue))
            .collect();
        let mut entering = Vec::new();
        for record in &records {
            if issues::key_of(&state.pool, &record.id).await?.is_none() {
                entering.push(record);
            }
        }
        let listed = worklogs_of(&client, entering.into_iter()).await?;
        let mut written = Vec::new();
        // Held until the commit: `watch_projects` changes the list and deletes
        // the rows of a removed project under the write lock, so a page is
        // either stored before that delete or filtered by the new list
        let saved = state.config.read().await;
        let watched = saved.as_ref().map_or(&[][..], |c| &c.watched_projects[..]);
        let mut tx = state.pool.begin().await?;
        for record in records {
            if !watched.contains(&record.project_key) {
                continue;
            }
            // The upsert overwrites the old key, so it has to be read first:
            // a view open under it would otherwise never hear of the move
            let old_key = issues::key_of(&mut *tx, &record.id).await?;
            if issues::upsert(&mut *tx, &record).await? {
                let moved_from = old_key.filter(|old| *old != record.key);
                if let Some(old) = &moved_from {
                    worklogs::rename_issue(&mut tx, old, &record.key).await?;
                    timer::rename_issue(&mut *tx, old, &record.key).await?;
                }
                written.push(record.key);
                written.extend(moved_from);
            }
        }
        store_worklogs(state, &mut tx, &listed, &mut Vec::new()).await?;
        tx.commit().await?;
        drop(saved);
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraWorklog {
    id: String,
    issue_id: String,
    author: Option<JiraAuthor>,
    time_spent_seconds: i64,
    started: String,
    comment: Option<Value>,
    #[serde(default)]
    properties: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JiraAuthor {
    display_name: Option<String>,
}

fn remote_worklog(worklog: &Value) -> anyhow::Result<RemoteWorklog> {
    let JiraWorklog {
        id,
        issue_id,
        author,
        time_spent_seconds,
        started,
        comment,
        properties,
    } = JiraWorklog::deserialize(worklog)
        .with_context(|| format!("unexpected worklog from Jira: {}", worklog["id"]))?;
    let comment = adf::to_plain_text(comment.as_ref());
    Ok(RemoteWorklog {
        started_at: worklogs::started_at_from_jira(&started).map_err(|error| {
            anyhow::anyhow!("worklog {id} has an unreadable `started`: {error}")
        })?,
        remote_id: id,
        issue_id,
        author_name: author
            .and_then(|author| author.display_name)
            .unwrap_or_else(|| "Unknown".to_owned()),
        time_spent_sec: time_spent_seconds,
        comment: (!comment.is_empty()).then_some(comment),
        // Tempo marks the Jira worklog it mirrors one of its own into with
        // the property `tempo`: `{ "shadow": "true", "tempo_id": 3 }`
        tempo_id: properties
            .iter()
            .find(|property| property["key"] == "tempo")
            .and_then(|property| property["value"]["tempo_id"].as_i64()),
    })
}

/// Stores the worklogs as Jira sent them, those of issues the cache has. The
/// keys of the issues whose worklogs were written are added to `changed`.
async fn store_worklogs(
    state: &AppState,
    conn: &mut sqlx::SqliteConnection,
    listed: &[Value],
    changed: &mut Vec<IssueKey>,
) -> anyhow::Result<()> {
    for worklog in listed {
        // An unreadable worklog is announced and left out, as an unreadable issue is
        let record = match remote_worklog(worklog) {
            Ok(record) => record,
            Err(error) => {
                let skipped = SyncEvent::IssueSkipped {
                    key: format!("worklog {}", worklog["id"].as_str().unwrap_or("unknown")),
                    reason: format!("{error:#}"),
                };
                state.events.send(skipped).ok();
                continue;
            }
        };
        if let Some(key) = worklogs::upsert_remote(conn, &record).await?
            && !changed.contains(&key)
        {
            changed.push(key);
        }
    }
    Ok(())
}

/// The worklogs of the issues on a page, asked for before the page is stored: a
/// failure then leaves the page unstored, and the next poll brings it again.
/// `worklog/updated` reports changes only, so an issue that enters the cache
/// after the first worklog poll gets its existing worklogs from here.
async fn worklogs_of(
    client: &JiraClient,
    records: impl Iterator<Item = &IssueRecord>,
) -> anyhow::Result<Vec<Value>> {
    let mut listed = Vec::new();
    for record in records {
        listed.extend(client.issue_worklogs(&record.key).await?);
    }
    Ok(listed)
}

/// Stores the worklogs Jira changed since the last poll, those of issues the
/// cache has, and moves the saved time forward. The keys of the issues whose
/// worklogs were written are added to `changed`.
pub(crate) async fn poll_updated_worklogs(
    state: &AppState,
    config: &Config,
    changed: &mut Vec<IssueKey>,
) -> anyhow::Result<()> {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let mut since = config.last_worklog_sync_at;
    loop {
        let page = client.updated_worklogs(since).await?;
        let ids: Vec<i64> = page
            .values
            .iter()
            .map(|worklog| worklog.worklog_id)
            .collect();
        let listed = if ids.is_empty() {
            Vec::new()
        } else {
            client.worklogs(&ids).await?
        };
        let mut tx = state.pool.begin().await?;
        store_worklogs(state, &mut tx, &listed, changed).await?;
        tx.commit().await?;
        // Jira's time, not the local clock: the next poll asks from where this one ended
        since = page.until.max(since);
        if page.last_page {
            break;
        }
    }

    let mut saved = state.config.write().await;
    // A sign-in during the poll starts a new setup: its sync time is not ours to set
    if let Some(current) = saved
        .as_mut()
        .filter(|current| current.last_worklog_sync_at == config.last_worklog_sync_at)
    {
        current.last_worklog_sync_at = since;
        auth::save_config(&state.data_dir, current).await?;
    }
    Ok(())
}

/// Removes the worklogs Jira deleted since the last poll and moves the saved
/// time forward. The keys of the issues that lost a worklog are added to `changed`.
pub(crate) async fn poll_deleted_worklogs(
    state: &AppState,
    config: &Config,
    changed: &mut Vec<IssueKey>,
) -> anyhow::Result<()> {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let mut since = config.last_worklog_delete_sync_at;
    loop {
        let page = client.deleted_worklogs(since).await?;
        let ids: Vec<i64> = page
            .values
            .iter()
            .map(|worklog| worklog.worklog_id)
            .collect();
        let mut tx = state.pool.begin().await?;
        for key in worklogs::delete_remote(&mut tx, &ids).await? {
            if !changed.contains(&key) {
                changed.push(key);
            }
        }
        tx.commit().await?;
        since = page.until.max(since);
        if page.last_page {
            break;
        }
    }

    let mut saved = state.config.write().await;
    // A sign-in during the poll starts a new setup: its sync time is not ours to set
    if let Some(current) = saved
        .as_mut()
        .filter(|current| current.last_worklog_delete_sync_at == config.last_worklog_delete_sync_at)
    {
        current.last_worklog_delete_sync_at = since;
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
    let mut rejected = Vec::new();
    let mut tx = state.pool.begin().await?;
    // Ids that only Jira has are new issues: the incremental poll stores them
    for issue in issues::cached(&mut *tx).await? {
        if config.watched_projects.contains(&issue.project_key) && !remote.contains(&issue.id) {
            issues::delete(&mut *tx, &issue.id).await?;
            worklogs::delete_for_issue(&mut *tx, &issue.key).await?;
            timer::delete_for_issue(&mut *tx, &issue.key).await?;
            // Nothing is left to send them to, and no view could show a refused
            // comment: the user is told about each one, the unsent comments included
            let entries = sync_queue::remove_for_issue(&mut *tx, &issue.key).await?;
            rejected.extend(entries.iter().map(|entity_type| {
                let reason = match entity_type.as_str() {
                    "comment" => COMMENT_DISCARDED,
                    _ => ISSUE_GONE,
                };
                outbound::rejection(&issue.key, entity_type, reason)
            }));
            removed.push(issue.key);
        }
    }
    tx.commit().await?;
    for event in rejected {
        state.events.send(event).ok();
    }

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
        matchers::{body_json, method, path, path_regex, query_param},
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

    /// Issues with no worklogs: what Jira answers for every issue a test does not
    /// give worklogs to. A mock of a test's own has a higher priority.
    fn no_issue_worklogs() -> Mock {
        Mock::given(method("GET"))
            .and(path_regex(r"^/rest/api/3/issue/[A-Z]+-\d+/worklog$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "startAt": 0, "total": 0, "worklogs": [] })),
            )
            .with_priority(10)
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
    async fn an_initial_sync_stops_downloading_a_project_that_was_unwatched_meanwhile() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = watching(&server, data_dir.path(), &["API", "WEB"]).await;
        mount_count(&server, "API", 2).await;
        let delay = std::time::Duration::from_millis(500);
        // The second page is not mounted: asking for it would fail the sync
        let first = page(&[issue(4, "API")], Some("tok")).set_delay(delay);
        mount_page(&server, "API", None, first).await;
        mount_count(&server, "WEB", 1).await;
        mount_page(&server, "WEB", None, page(&[issue(1, "WEB")], None)).await;

        let syncing = tokio::spawn({
            let state = state.clone();
            async move { api::initial_sync(&state).await }
        });
        while !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.url.path().ends_with("/search/jql"))
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let saving = std::time::Instant::now();
        api::watch_projects(&state, &["WEB".to_owned()])
            .await
            .unwrap();
        assert!(saving.elapsed() < delay);
        syncing.await.unwrap().unwrap();

        assert_eq!(stored_keys(&state).await, ["WEB-1"]);
        assert_eq!(
            received(&mut events),
            [
                "API 0 / ~2",
                "WEB 0 / ~1",
                "WEB 1 / ~1",
                "InitialSyncComplete",
            ]
        );
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
    }

    fn without_summary(mut issue: Value) -> Value {
        issue["fields"].as_object_mut().unwrap().remove("summary");
        issue
    }

    #[tokio::test]
    async fn an_initial_sync_skips_an_unreadable_issue_and_reports_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = watching(&server, data_dir.path(), &["WEB"]).await;
        mount_count(&server, "WEB", 3).await;
        let issues = [
            issue(1, "WEB"),
            without_summary(issue(2, "WEB")),
            issue(3, "WEB"),
        ];
        mount_page(&server, "WEB", None, page(&issues, None)).await;

        api::initial_sync(&state).await.unwrap();

        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-3"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
        let received = received(&mut events);
        assert_eq!(received.last().unwrap(), "InitialSyncComplete");
        let skipped: Vec<_> = received
            .iter()
            .filter(|event| event.starts_with("IssueSkipped"))
            .collect();
        assert_eq!(skipped.len(), 1, "{received:?}");
        assert!(skipped[0].contains("WEB-2") && skipped[0].contains("summary"));
    }

    #[tokio::test]
    async fn the_initial_sync_progress_does_not_count_a_skipped_issue() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = watching(&server, data_dir.path(), &["WEB"]).await;
        mount_count(&server, "WEB", 3).await;
        let first = [issue(1, "WEB"), without_summary(issue(2, "WEB"))];
        mount_page(&server, "WEB", None, page(&first, Some("tok"))).await;
        mount_page(&server, "WEB", Some("tok"), page(&[issue(3, "WEB")], None)).await;

        api::initial_sync(&state).await.unwrap();

        let progress: Vec<_> = received(&mut events)
            .into_iter()
            .filter(|event| event.starts_with("WEB "))
            .collect();
        assert_eq!(progress, ["WEB 0 / ~3", "WEB 1 / ~3", "WEB 2 / ~3"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
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

    /// A core that watches `projects`, with every project's issues downloaded:
    /// WEB-1 and WEB-2, API-3.
    async fn downloaded(
        server: &MockServer,
        data_dir: &Path,
        projects: &[&str],
    ) -> (Arc<AppState>, UnboundedReceiver<SyncEvent>) {
        let (state, mut events) = watching(server, data_dir, projects).await;
        if projects.contains(&"WEB") {
            mount_count(server, "WEB", 2).await;
            mount_page(
                server,
                "WEB",
                None,
                page(&[issue(1, "WEB"), issue(2, "WEB")], None),
            )
            .await;
        }
        if projects.contains(&"API") {
            mount_count(server, "API", 1).await;
            mount_page(server, "API", None, page(&[issue(3, "API")], None)).await;
        }
        api::initial_sync(&state).await.unwrap();
        received(&mut events);
        (state, events)
    }

    #[tokio::test]
    async fn a_project_added_after_setup_is_downloaded_and_the_cached_ones_are_not() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        // Each page of WEB is expected once: a second download would fail the test
        let (state, mut events) = downloaded(&server, data_dir.path(), &["WEB"]).await;
        let synced_at = last_sync_at(&state).await;
        let new_projects = ["WEB".to_owned(), "API".to_owned()];

        api::watch_projects(&state, &new_projects).await.unwrap();

        assert_eq!(api::watched_projects(&state).await, ["WEB", "API"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::InitialSync);
        mount_count(&server, "API", 1).await;
        mount_page(&server, "API", None, page(&[issue(3, "API")], None)).await;
        no_issue_worklogs().mount(&server).await;

        api::initial_sync(&state).await.unwrap();

        assert_eq!(stored_keys(&state).await, ["API-3", "WEB-1", "WEB-2"]);
        assert_eq!(
            received(&mut events),
            ["API 0 / ~1", "API 1 / ~1", "InitialSyncComplete"]
        );
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
        assert_eq!(last_sync_at(&state).await, synced_at);
    }

    #[tokio::test]
    async fn a_download_of_an_added_project_interrupted_by_a_quit_completes_on_the_next_start() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = downloaded(&server, data_dir.path(), &["WEB"]).await;
        api::watch_projects(&state, &["WEB".to_owned(), "API".to_owned()])
            .await
            .unwrap();
        mount_count(&server, "API", 2).await;
        mount_page(&server, "API", None, page(&[issue(3, "API")], Some("tok"))).await;
        mount_page(&server, "API", Some("tok"), ResponseTemplate::new(503)).await;
        no_issue_worklogs().mount(&server).await;

        let error = api::initial_sync(&state).await.unwrap_err();

        assert!(error.to_string().contains("503"), "{error}");
        assert_eq!(api::setup_step(&state).await, SetupStep::InitialSync);
        state.pool.close().await;

        server.reset().await;
        mount_count(&server, "API", 2).await;
        mount_page(&server, "API", None, page(&[issue(3, "API")], Some("tok"))).await;
        mount_page(&server, "API", Some("tok"), page(&[issue(4, "API")], None)).await;
        no_issue_worklogs().mount(&server).await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(api::watched_projects(&state).await, ["WEB", "API"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::InitialSync);

        // Only API is asked for: Jira answers 404 to any other query
        api::initial_sync(&state).await.unwrap();

        assert_eq!(
            stored_keys(&state).await,
            ["API-3", "API-4", "WEB-1", "WEB-2"]
        );
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
    }

    #[tokio::test]
    async fn a_project_that_is_no_longer_watched_leaves_the_list_and_the_search() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = downloaded(&server, data_dir.path(), &["WEB", "API"]).await;
        let found = api::search_issues(&state, "flaky", None).await.unwrap();
        assert_eq!(found.len(), 3);

        api::watch_projects(&state, &["WEB".to_owned()])
            .await
            .unwrap();

        assert_eq!(api::watched_projects(&state).await, ["WEB"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-2"]);
        let listed = api::list_issues(&state, None).await.unwrap();
        assert_eq!(listed.len(), 2);
        let found = api::search_issues(&state, "flaky", None).await.unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|row| row.project_key == "WEB"));
        assert_eq!(
            received(&mut events),
            ["IssuesUpdated([IssueKey(\"API-3\")])"]
        );
    }

    #[tokio::test]
    async fn watching_no_project_after_setup_changes_nothing() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = downloaded(&server, data_dir.path(), &["WEB", "API"]).await;

        let error = api::watch_projects(&state, &[]).await.unwrap_err();

        assert_eq!(error.to_string(), "Choose at least one project");
        assert_eq!(api::watched_projects(&state).await, ["WEB", "API"]);
        assert_eq!(api::setup_step(&state).await, SetupStep::Ready);
        assert_eq!(stored_keys(&state).await, ["API-3", "WEB-1", "WEB-2"]);
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
        crate::sync::no_worklog_changes().mount(server).await;
        no_issue_worklogs().mount(server).await;
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
    async fn a_poll_that_started_before_a_project_was_unwatched_does_not_store_its_issues() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB")];
        let (state, mut events) = synced(&server, data_dir.path(), &stored).await;
        let polled = [changed(issue(1, "WEB"), LATER, "Renamed"), issue(5, "API")];
        let delay = std::time::Duration::from_millis(500);
        poll(JUST_SYNCED, page(&polled, None).set_delay(delay))
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();
        // The request is out, so the cycle runs with the old project list
        while !server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| String::from_utf8_lossy(&request.body).contains("updated >="))
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let saving = std::time::Instant::now();
        api::watch_projects(&state, &["WEB".to_owned()])
            .await
            .unwrap();
        assert!(saving.elapsed() < delay);
        cycle(&mut events).await;

        assert_eq!(summary(&state, "WEB-1").await, "Renamed");
        assert_eq!(stored_keys(&state).await, ["WEB-1", "WEB-2"]);
        let listed = api::list_issues(&state, None).await.unwrap();
        assert!(listed.iter().all(|row| row.project_key == "WEB"));
        let found = api::search_issues(&state, "flaky", None).await.unwrap();
        assert!(found.iter().all(|row| row.project_key == "WEB"));
    }

    #[tokio::test]
    async fn a_poll_skips_an_unreadable_issue_and_still_completes() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        let before = last_sync_at(&state).await;
        let mut unreadable = issue(2, "WEB");
        unreadable["fields"]["updated"] = json!("yesterday");
        let polled = [
            changed(issue(1, "WEB"), LATER, "Renamed"),
            unreadable,
            without_summary(issue(3, "WEB")),
            issue(5, "API"),
        ];
        poll(JUST_SYNCED, page(&polled, None)).mount(&server).await;

        state.sync_waker.send(()).unwrap();

        let cycle = cycle(&mut events).await;
        assert_eq!(cycle.len(), 5, "{cycle:?}");
        assert_eq!(cycle[0], "Started");
        assert!(cycle[1].starts_with("IssueSkipped") && cycle[1].contains("WEB-2"));
        assert!(cycle[1].contains("updated"));
        assert!(cycle[2].starts_with("IssueSkipped") && cycle[2].contains("WEB-3"));
        assert!(cycle[2].contains("summary"));
        assert_eq!(
            cycle[3],
            r#"IssuesUpdated([IssueKey("WEB-1"), IssueKey("API-5")])"#
        );
        assert_eq!(cycle[4], "Completed");
        assert_eq!(stored_keys(&state).await, ["API-5", "WEB-1"]);
        assert_eq!(summary(&state, "WEB-1").await, "Renamed");
        assert!(last_sync_at(&state).await > before);
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
        tokio::time::advance(crate::sync::engine::ACTIVE_INTERVAL).await;
        tokio::time::resume();

        assert_eq!(
            cycle(&mut events).await[1],
            r#"IssuesUpdated([IssueKey("WEB-1")])"#
        );
    }

    /// Moves only the timer of the engine, as the test above does.
    async fn pass(time: std::time::Duration) {
        tokio::time::pause();
        tokio::time::advance(time).await;
        tokio::time::resume();
    }

    async fn assert_no_cycle(events: &mut UnboundedReceiver<SyncEvent>) {
        let wait = std::time::Duration::from_millis(300);
        let event = tokio::time::timeout(wait, events.recv()).await;
        assert!(event.is_err(), "a sync cycle started: {event:?}");
    }

    #[tokio::test]
    async fn a_window_in_the_background_is_polled_after_fifteen_minutes() {
        use crate::sync::engine::{ACTIVE_INTERVAL, BACKGROUND_INTERVAL};
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        state.set_window_focused(false);

        pass(ACTIVE_INTERVAL).await;
        assert_no_cycle(&mut events).await;

        pass(BACKGROUND_INTERVAL - ACTIVE_INTERVAL).await;
        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
    }

    #[tokio::test]
    async fn an_idle_user_is_polled_after_thirty_minutes() {
        use crate::sync::engine::{BACKGROUND_INTERVAL, IDLE_AFTER, IDLE_INTERVAL};
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        // The window is still focused: the user walked away from it
        *state.idle_time.lock().unwrap() = IDLE_AFTER;

        pass(BACKGROUND_INTERVAL).await;
        assert_no_cycle(&mut events).await;

        pass(IDLE_INTERVAL - BACKGROUND_INTERVAL).await;
        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
    }

    #[tokio::test]
    async fn focusing_the_window_after_a_longer_interval_syncs_right_away() {
        use crate::sync::engine::ACTIVE_INTERVAL;
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        state.set_window_focused(false);
        pass(ACTIVE_INTERVAL * 2).await;
        assert_no_cycle(&mut events).await;

        // No time passes: the focus alone starts the cycle
        state.set_window_focused(true);

        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
    }

    #[tokio::test]
    async fn a_user_back_from_idle_is_synced_at_the_next_look() {
        use crate::sync::engine::{ACTIVE_INTERVAL, ACTIVITY_CHECK, IDLE_AFTER};
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        *state.idle_time.lock().unwrap() = IDLE_AFTER;
        pass(ACTIVE_INTERVAL * 2).await;
        assert_no_cycle(&mut events).await;

        *state.idle_time.lock().unwrap() = std::time::Duration::ZERO;
        pass(ACTIVITY_CHECK).await;

        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
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
                r#"IssuesUpdated([IssueKey("API-9"), IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        assert_eq!(stored_keys(&state).await, ["API-9"]);
        let row = issues::get(&state.pool, &"API-9".into()).await.unwrap();
        assert_eq!(row.unwrap().project_key, "API");
    }

    /// Jira's answer to `GET /worklog/updated?since={since}`.
    fn worklogs_updated(since: i64, ids: &[i64], until: i64, last_page: bool) -> Mock {
        let values: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "worklogId": id, "updatedTime": until }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/rest/api/3/worklog/updated"))
            .and(query_param("since", since.to_string()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({ "values": values, "until": until, "lastPage": last_page }),
                ),
            )
            .with_priority(1)
    }

    /// Jira's answer to `GET /worklog/deleted?since={since}`, which must be asked once.
    fn worklogs_deleted(since: i64, ids: &[i64], until: i64, last_page: bool) -> Mock {
        let values: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "worklogId": id, "updatedTime": until }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/rest/api/3/worklog/deleted"))
            .and(query_param("since", since.to_string()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({ "values": values, "until": until, "lastPage": last_page }),
                ),
            )
            .with_priority(1)
            .expect(1)
    }

    /// A worklog as `POST /worklog/list` sends it: 10:15 in a UTC+2 profile.
    fn jira_worklog(id: &str, issue_id: &str, seconds: i64) -> Value {
        json!({
            "id": id,
            "issueId": issue_id,
            "author": { "accountId": "7c20b", "displayName": "Grace Hopper" },
            "timeSpentSeconds": seconds,
            "started": "2026-03-29T10:15:00.000+0200",
            "comment": { "type": "doc", "version": 1, "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "Pairing" }] },
            ] },
        })
    }

    #[test]
    fn a_worklog_whose_author_has_no_name_is_attributed_to_unknown() {
        let mut worklog = jira_worklog("1", "1", 60);
        worklog["author"] = json!({ "accountId": "7c20b" });
        assert_eq!(remote_worklog(&worklog).unwrap().author_name, "Unknown");

        worklog.as_object_mut().unwrap().remove("author");
        assert_eq!(remote_worklog(&worklog).unwrap().author_name, "Unknown");
    }

    fn worklog_list(ids: &[i64], worklogs: &[Value]) -> Mock {
        Mock::given(method("POST"))
            .and(path("/rest/api/3/worklog/list"))
            .and(body_json(json!({ "ids": ids })))
            .respond_with(ResponseTemplate::new(200).set_body_json(worklogs))
            .expect(1)
    }

    /// Every cached worklog: the issue, the remote id, the seconds, the start and the comment.
    async fn stored_worklogs(
        state: &AppState,
    ) -> Vec<(String, Option<String>, i64, String, Option<String>)> {
        sqlx::query_as(
            "SELECT issue_key, remote_id, time_spent_sec, started_at, comment FROM worklogs
             ORDER BY issue_key, remote_id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    async fn saved_worklog_time(state: &AppState) -> i64 {
        let config = state.config.read().await;
        config.as_ref().unwrap().last_worklog_sync_at
    }

    #[tokio::test]
    async fn a_poll_stores_the_worklogs_jira_changed_and_continues_from_where_it_ended() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        // Two pages; worklog 3 is on an issue the cache does not have
        worklogs_updated(0, &[1, 2], 5_000, false)
            .mount(&server)
            .await;
        worklog_list(
            &[1, 2],
            &[jira_worklog("1", "1", 3600), jira_worklog("2", "1", 900)],
        )
        .mount(&server)
        .await;
        worklogs_updated(5_000, &[3], 9_000, true)
            .mount(&server)
            .await;
        worklog_list(&[3], &[jira_worklog("3", "77", 60)])
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        // 10:15 at UTC+2 is 08:15 UTC
        let started = "2026-03-29T08:15:00.000Z";
        let pairing = Some("Pairing".to_owned());
        assert_eq!(
            stored_worklogs(&state).await,
            [
                (
                    "WEB-1".to_owned(),
                    Some("1".to_owned()),
                    3600,
                    started.to_owned(),
                    pairing.clone()
                ),
                (
                    "WEB-1".to_owned(),
                    Some("2".to_owned()),
                    900,
                    started.to_owned(),
                    pairing
                ),
            ]
        );
        let authors: Vec<Option<String>> =
            sqlx::query_scalar("SELECT author_name FROM worklogs ORDER BY remote_id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            authors,
            ["Grace Hopper"; 2].map(|name| Some(name.to_owned()))
        );
        assert_eq!(saved_worklog_time(&state).await, 9_000);
        let saved = std::fs::read(data_dir.path().join("config.json")).unwrap();
        let saved: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(saved["last_worklog_sync_at"], json!(9_000));

        // The next cycle asks from the end of the last page, and a worklog Jira
        // changed meanwhile replaces the stored one
        let changed_worklog = jira_worklog("1", "1", 7200);
        worklogs_updated(9_000, &[1], 9_500, true)
            .mount(&server)
            .await;
        worklog_list(&[1], &[changed_worklog]).mount(&server).await;
        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        let rows = stored_worklogs(&state).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].2, 7200);
        assert_eq!(saved_worklog_time(&state).await, 9_500);
    }

    #[tokio::test]
    async fn a_worklog_deleted_in_jira_is_removed_and_one_not_yet_delivered_is_kept() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        sqlx::query(
            "INSERT INTO worklogs (remote_id, issue_key, time_spent_sec, started_at, sync_status)
             VALUES ('1', 'WEB-1', 60, '2026-03-29T08:15:00.000Z', 'synced'),
                    ('2', 'WEB-1', 60, '2026-03-29T08:15:00.000Z', 'synced'),
                    ('3', 'WEB-1', 60, '2026-03-29T08:15:00.000Z', 'synced'),
                    (NULL, 'WEB-1', 120, '2026-03-29T09:00:00.000Z', 'pending'),
                    (NULL, 'WEB-1', 180, '2026-03-29T10:00:00.000Z', 'failed')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        // Two pages; worklog 99 was never cached
        worklogs_deleted(0, &[1], 5_000, false).mount(&server).await;
        worklogs_deleted(5_000, &[2, 99], 9_000, true)
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        let remaining: Vec<(Option<String>, i64)> = sqlx::query_as(
            "SELECT remote_id, time_spent_sec FROM worklogs ORDER BY time_spent_sec",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            remaining,
            [(Some("3".to_owned()), 60), (None, 120), (None, 180)]
        );
        let saved = std::fs::read(data_dir.path().join("config.json")).unwrap();
        let saved: Value = serde_json::from_slice(&saved).unwrap();
        assert_eq!(saved["last_worklog_delete_sync_at"], json!(9_000));

        // The next cycle asks from the end of the last page, which `expect(1)` checks
        worklogs_deleted(9_000, &[], 9_500, true)
            .mount(&server)
            .await;
        state.sync_waker.send(()).unwrap();

        assert_eq!(cycle(&mut events).await, ["Started", "Completed"]);
        assert_eq!(
            state
                .config
                .read()
                .await
                .as_ref()
                .unwrap()
                .last_worklog_delete_sync_at,
            9_500
        );
    }

    #[tokio::test]
    async fn a_worklog_created_here_is_not_stored_twice_when_the_poll_returns_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/worklog"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "30001" })))
            .expect(1)
            .mount(&server)
            .await;
        // The worklog posted in this cycle comes back in the same cycle's poll
        let mut returned = jira_worklog("30001", "1", 5400);
        returned["comment"] = json!(null);
        worklogs_updated(0, &[30001], 9_000, true)
            .mount(&server)
            .await;
        worklog_list(&[30001], &[returned]).mount(&server).await;
        // 2026-03-29 08:15:00 UTC
        api::log_time(&state, &"WEB-1".into(), 5400, 1_774_772_100, None)
            .await
            .unwrap();

        // The first event is `log_time`'s own
        let events = cycle(&mut events).await;
        assert_eq!(events.len(), 4, "{events:?}");

        let rows = stored_worklogs(&state).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].1.as_deref(), Some("30001"));
        let status: String = sqlx::query_scalar("SELECT sync_status FROM worklogs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "synced");
    }

    /// Jira's answer to `GET /issue/{key}/worklog`, one page of `total` worklogs.
    fn issue_worklogs(key: &str, start_at: usize, total: usize, worklogs: &[Value]) -> Mock {
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/issue/{key}/worklog")))
            .and(query_param("startAt", start_at.to_string()))
            .and(query_param("expand", "properties"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "startAt": start_at, "total": total, "worklogs": worklogs }),
            ))
            .expect(1)
    }

    #[tokio::test]
    async fn the_worklogs_of_a_project_added_after_setup_are_stored_once() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        // WEB is cached already: asking Jira for the worklogs of its issues would get a 404
        let (state, mut events) = downloaded(&server, data_dir.path(), &["WEB"]).await;
        api::watch_projects(&state, &["WEB".to_owned(), "API".to_owned()])
            .await
            .unwrap();
        mount_count(&server, "API", 1).await;
        mount_page(&server, "API", None, page(&[issue(3, "API")], None)).await;
        issue_worklogs("API-3", 0, 1, &[jira_worklog("31", "3", 1800)])
            .mount(&server)
            .await;

        api::initial_sync(&state).await.unwrap();

        let rows = stored_worklogs(&state).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].0.as_str(), rows[0].2), ("API-3", 1800));

        // The worklog poll that follows reports the same worklog: still one row
        received(&mut events);
        listing(ids(1..10)).mount(&server).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        worklogs_updated(0, &[31], 9_000, true).mount(&server).await;
        worklog_list(&[31], &[jira_worklog("31", "3", 1800)])
            .mount(&server)
            .await;
        crate::sync::no_worklog_changes().mount(&server).await;
        state.sync_waker.send(()).unwrap();
        cycle(&mut events).await;

        assert_eq!(stored_worklogs(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn the_worklogs_of_an_issue_created_after_setup_are_stored_with_the_issue() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        poll(JUST_SYNCED, page(&[issue(1, "WEB"), issue(5, "API")], None))
            .mount(&server)
            .await;
        // Two pages of worklogs; WEB-1 is cached, so only API-5 is asked
        issue_worklogs("API-5", 0, 2, &[jira_worklog("51", "5", 60)])
            .mount(&server)
            .await;
        issue_worklogs("API-5", 1, 2, &[jira_worklog("52", "5", 120)])
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("API-5")])"#,
                "Completed"
            ]
        );
        let rows = stored_worklogs(&state).await;
        let seconds: Vec<_> = rows.iter().map(|row| (row.0.as_str(), row.2)).collect();
        assert_eq!(seconds, [("API-5", 60), ("API-5", 120)]);
    }

    #[tokio::test]
    async fn an_issue_whose_worklogs_cannot_be_read_is_stored_again_by_the_next_poll() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        poll(JUST_SYNCED, page(&[issue(5, "API")], None))
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/issue/API-5/worklog"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();
        let failed = cycle(&mut events).await;

        assert!(failed.last().unwrap().starts_with("Error"), "{failed:?}");
        assert_eq!(stored_keys(&state).await, ["WEB-1"]);

        issue_worklogs("API-5", 0, 1, &[jira_worklog("51", "5", 60)])
            .mount(&server)
            .await;
        state.sync_waker.send(()).unwrap();
        cycle(&mut events).await;

        assert_eq!(stored_keys(&state).await, ["API-5", "WEB-1"]);
        assert_eq!(stored_worklogs(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn an_unreadable_worklog_is_announced_and_the_others_are_stored() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        poll(JUST_SYNCED, page(&[], None)).mount(&server).await;
        let mut unreadable = jira_worklog("1", "1", 60);
        unreadable["started"] = json!("yesterday");
        worklogs_updated(0, &[1, 2], 9_000, true)
            .mount(&server)
            .await;
        worklog_list(&[1, 2], &[unreadable, jira_worklog("2", "1", 900)])
            .mount(&server)
            .await;

        state.sync_waker.send(()).unwrap();

        let cycle = cycle(&mut events).await;
        assert_eq!(cycle.len(), 4, "{cycle:?}");
        assert!(cycle[1].starts_with("IssueSkipped") && cycle[1].contains("worklog 1"));
        assert_eq!(stored_worklogs(&state).await.len(), 1);
        assert_eq!(saved_worklog_time(&state).await, 9_000);
    }

    #[tokio::test]
    async fn the_worklogs_of_a_moved_issue_follow_its_new_key() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = synced(&server, data_dir.path(), &[issue(1, "WEB")]).await;
        // A delivered worklog, and one that Jira refused and the outbox still holds
        sqlx::raw_sql(
            "INSERT INTO worklogs (remote_id, issue_key, time_spent_sec, started_at, sync_status)
                 VALUES ('40001', 'WEB-1', 600, '2026-03-29T08:15:00.000Z', 'synced');
                 INSERT INTO sync_queue (entity_type, issue_key, operation, payload, status)
                 VALUES ('worklog', 'WEB-1', 'create', '{}', 'failed');
                 INSERT INTO running_timer (id, issue_key, started_at)
                 VALUES (1, 'WEB-1', 1774771200);",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let mut moved = changed(issue(1, "API"), LATER, "Moved");
        moved["key"] = json!("API-9");
        poll(JUST_SYNCED, page(&[moved], None)).mount(&server).await;

        state.sync_waker.send(()).unwrap();
        cycle(&mut events).await;

        let rows = stored_worklogs(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "API-9");
        let running = timer::get(&state.pool).await.unwrap().unwrap();
        assert_eq!(running.issue_key, "API-9".into());
        assert_eq!(running.started_at, 1_774_771_200);
        let queued: Vec<String> = sqlx::query_scalar("SELECT issue_key FROM sync_queue")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(queued, ["API-9"]);
    }

    #[tokio::test]
    async fn the_worklogs_of_an_issue_removed_by_reconciliation_are_removed_too() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB")];
        let (state, _events) = synced(&server, data_dir.path(), &stored).await;
        sqlx::raw_sql(
            "INSERT INTO worklogs (issue_key, time_spent_sec, started_at)
                 VALUES ('WEB-1', 600, '2026-03-29T08:15:00.000Z'),
                        ('WEB-2', 600, '2026-03-29T08:15:00.000Z');
                 INSERT INTO running_timer (id, issue_key, started_at)
                 VALUES (1, 'WEB-2', 1774771200);",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        listing(ids(1..2)).with_priority(1).mount(&server).await;
        let config = state.config.read().await.clone().unwrap();

        reconcile_removed_issues(&state, &config).await.unwrap();

        let rows = stored_worklogs(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "WEB-1");
        // The timer ran on the issue that is gone
        assert_eq!(timer::get(&state.pool).await.unwrap(), None);
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
        crate::sync::no_worklog_changes().mount(&server).await;
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
    async fn the_entries_of_a_removed_issue_are_removed_and_the_user_is_told() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let stored = [issue(1, "WEB"), issue(2, "WEB")];
        let (state, mut events) = synced(&server, data_dir.path(), &stored).await;
        listing(ids(1..2)).with_priority(1).mount(&server).await;
        let (gone, kept) = (IssueKey::from("WEB-2"), IssueKey::from("WEB-1"));
        let payload = json!({ "transition": { "id": "11" }, "fields": {} }).to_string();
        let comment = json!({ "text": "Seen on staging" }).to_string();
        let refused = json!({ "text": "Refused before" }).to_string();
        crate::db::sync_queue::enqueue(&state.pool, "transition", &gone, &payload)
            .await
            .unwrap();
        crate::db::sync_queue::enqueue(&state.pool, "comment", &gone, &comment)
            .await
            .unwrap();
        // Jira refused this one while the issue still existed: it was shown with
        // Retry and Discard, which the removal takes away
        crate::db::sync_queue::enqueue(&state.pool, "comment", &gone, &refused)
            .await
            .unwrap();
        // The issue that still exists keeps its refused comment
        let kept_comment = json!({ "text": "Kept" }).to_string();
        crate::db::sync_queue::enqueue(&state.pool, "comment", &kept, &kept_comment)
            .await
            .unwrap();
        sqlx::query("UPDATE sync_queue SET status = 'failed' WHERE payload = ? OR payload = ?")
            .bind(&refused)
            .bind(&kept_comment)
            .execute(&state.pool)
            .await
            .unwrap();
        let config = state.config.read().await.clone().unwrap();

        let removed = reconcile_removed_issues(&state, &config).await.unwrap();

        assert_eq!(removed, std::slice::from_ref(&gone));
        let reason = "The issue no longer exists in Jira or is not visible to you";
        let discarded = format!("{reason}, so the comment was discarded");
        assert_eq!(
            received(&mut events),
            [
                format!(
                    "ChangeRejected {{ key: IssueKey(\"WEB-2\"), action: \"status change\", reason: {reason:?} }}"
                ),
                format!(
                    "ChangeRejected {{ key: IssueKey(\"WEB-2\"), action: \"comment\", reason: {discarded:?} }}"
                ),
                format!(
                    "ChangeRejected {{ key: IssueKey(\"WEB-2\"), action: \"comment\", reason: {discarded:?} }}"
                ),
            ]
        );
        let entries: Vec<(String, String)> =
            sqlx::query_as("SELECT issue_key, status FROM sync_queue ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(entries, [("WEB-1".to_owned(), "failed".to_owned())]);
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
