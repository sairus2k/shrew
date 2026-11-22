//! Local changes going out to Jira: the processor of the `sync_queue` outbox.

use crate::{
    AppState, SyncEvent, adf,
    auth::Config,
    db::{
        assignable, issues,
        sync_queue::{self, QueueEntry},
        transitions::{self, Position},
        worklogs,
    },
    events::rejection,
    models::{IssueKey, WorklogTarget},
    sync::{
        issue_record,
        jira_client::{JiraClient, JiraError},
        tempo_client::{TempoClient, TempoError},
    },
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

const MAX_RETRIES: i64 = 3;
// The wait before the first retry; each later one waits twice as long. The
// tests go through every retry, so they cannot wait whole seconds.
const BACKOFF_BASE: Duration = Duration::from_millis(if cfg!(test) { 10 } else { 1000 });

enum Failure {
    /// Jira could not be asked: the entry is as deliverable as before.
    NotAsked,
    /// Jira may accept the same request later.
    Transient,
    /// Jira will answer the same request the same way.
    Rejected,
}

/// Sends the queued changes to Jira, oldest first, and adds the keys of the
/// issues whose changes Jira accepted to `delivered`. An entry Jira rejects is
/// marked `failed`, rolled back and announced with `SyncEvent::ChangeRejected`,
/// and the pass goes on.
/// The pass ends with an error when Jira cannot be reached or rejects the
/// token: the entries then stay `pending` for a later pass. A rejected Tempo
/// token only holds back the worklogs, which stay `pending` in their order.
pub(crate) async fn process_queue(
    state: &AppState,
    config: &Config,
    delivered: &mut Vec<IssueKey>,
) -> anyhow::Result<()> {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    // A token Tempo rejected in an earlier pass is not sent again: the worklogs
    // wait, the rest goes on
    let tempo_rejected = |state: &AppState| {
        config.worklog_target == WorklogTarget::Tempo
            && state.rejected_tempo_token.lock().unwrap().as_deref()
                == Some(config.tempo_api_token.as_str())
    };
    // A retried entry is claimed again: it is still the oldest one
    while let Some(entry) = sync_queue::claim_next(&state.pool, tempo_rejected(state)).await? {
        let error = match deliver(&client, state, config, &entry).await {
            Ok(delivered_as) => {
                // One transaction: an issue must not stay pending with nothing left to send,
                // and a comment must not be missing from both the outbox and the cache
                let mut tx = crate::db::begin(&state.pool).await?;
                sync_queue::mark_synced(&mut *tx, entry.id).await?;
                match &delivered_as {
                    Delivered::Comment(comment) => {
                        issues::add_comment(&mut tx, &entry.issue_key, comment).await?;
                    }
                    Delivered::Worklog {
                        local_id,
                        remote_id,
                    } => worklogs::confirm(&mut *tx, local_id, remote_id).await?,
                    Delivered::TempoWorklog { local_id, tempo_id } => {
                        worklogs::confirm_tempo(&mut *tx, local_id, *tempo_id).await?;
                    }
                    Delivered::Nothing => {}
                }
                issues::confirm(&mut *tx, &entry.issue_key).await?;
                tx.commit().await?;
                if !delivered.contains(&entry.issue_key) {
                    delivered.push(entry.issue_key);
                }
                continue;
            }
            Err(error) => error,
        };
        let message = format!("{error:#}");
        if matches!(error.downcast_ref(), Some(TempoError::Unauthorized)) {
            // Only the worklogs need the Tempo token: the rest of the queue
            // is still delivered, and the cycle goes on to read what Jira has
            sync_queue::release(&state.pool, entry.id).await?;
            if !tempo_rejected(state) {
                *state.rejected_tempo_token.lock().unwrap() = Some(config.tempo_api_token.clone());
                state.events.send(SyncEvent::TempoTokenRejected).ok();
            }
            continue;
        }
        match failure(&error) {
            Failure::NotAsked => {
                sync_queue::release(&state.pool, entry.id).await?;
                return Err(error);
            }
            Failure::Transient if entry.retry_count < MAX_RETRIES => {
                sync_queue::release_for_retry(&state.pool, entry.id, &message).await?;
                tokio::time::sleep(BACKOFF_BASE * 2u32.pow(entry.retry_count as u32)).await;
            }
            Failure::Transient | Failure::Rejected => {
                let mut later = Vec::new();
                if matches!(entry.entity_type.as_str(), "transition" | "assignee") {
                    sync_queue::remove(&state.pool, entry.id).await?;
                    // The entry is gone whether or not the rollback works: with
                    // Jira out of reach the change stays as the user left it
                    let refused = matches!(
                        error.downcast_ref(),
                        Some(JiraError::Status(status, _)) if status.is_client_error()
                    );
                    // A failed rollback leaves the issue `pending` with nothing in the
                    // outbox, which `roll_back_unconfirmed` looks for in every cycle:
                    // the issue row is the marker, not the entry
                    later = roll_back(
                        state,
                        &client,
                        &entry.issue_key,
                        &entry.entity_type,
                        refused,
                    )
                    .await
                    .unwrap_or_default();
                } else {
                    sync_queue::mark_failed(&state.pool, entry.id, &message).await?;
                    if entry.entity_type == "worklog" {
                        let payload: Value = serde_json::from_str(&entry.payload)?;
                        let local_id = payload["local_id"].as_str().unwrap_or_default();
                        // After the project was unwatched no row is left to show the
                        // refusal on (the user is told by the event below)
                        if !worklogs::mark_failed(&state.pool, local_id).await? {
                            sync_queue::remove(&state.pool, entry.id).await?;
                        }
                    }
                }
                // A refused comment moves from the pending ones to the refused ones
                let updated = SyncEvent::IssuesUpdated(vec![entry.issue_key.clone()]);
                state.events.send(updated).ok();
                let event = rejection(&entry.issue_key, &entry.entity_type, &message);
                state.events.send(event).ok();
                for event in later {
                    state.events.send(event).ok();
                }
            }
        }
    }
    Ok(())
}

/// Why the status changes queued behind one that failed are not sent: they were
/// chosen from the status the failed one was to produce, which Jira does not have.
const EARLIER_REJECTED: &str = "an earlier status change of this issue did not reach Jira";

/// Reads the issue again and stores it as Jira has it, so that a change the
/// user made and Jira refused does not stay on screen. Jira has not changed
/// the issue, so its `updated` is no newer than the stored row's. `entity_type`
/// is the type of the entry that failed: what the user changed by the other
/// kind of entry, while it waits in the outbox, stays as it is (see `upsert`).
///
/// With `refused` (Jira answered with a 4xx) what was cached to offer the
/// change is dropped, because something the cache did not know about may be the
/// reason. For a transition that is the menu of the status the issue is in:
/// the status the transition started from, unless a teammate moved the issue
/// first, and then the menu of that status is as good as any to refetch. For an
/// assignee it is the list of people the issue can be assigned to.
///
/// The status changes of the issue still waiting in the outbox are dropped with
/// a failed transition and returned as rejections, for the caller to announce:
/// the status Jira has is the only one that does not contradict them. They are
/// dropped in the transaction that stores the issue, so that one the user
/// queues while Jira is being read is not left with a status that this
/// overwrites. A failed assignee change drops nothing: the other assignee
/// changes were chosen from a list that does not depend on it.
async fn roll_back(
    state: &AppState,
    client: &JiraClient,
    key: &IssueKey,
    entity_type: &str,
    refused: bool,
) -> anyhow::Result<Vec<SyncEvent>> {
    let mut issue = client.issue(key, issue_record::ISSUE_FIELDS).await?;
    issue_record::mark_replies(client, std::slice::from_mut(&mut issue)).await?;
    let record = issue_record::issue_record(&issue)?;
    let mut tx = crate::db::begin(&state.pool).await?;
    // An issue the cache does not have belongs to a project that is no longer
    // watched: upserting it would bring it back, and nothing syncs it there
    if !issues::mark_stale(&mut *tx, &record.id).await? {
        return Ok(Vec::new());
    }
    let status_change = entity_type == "transition";
    let later = if status_change {
        sync_queue::drop_unsent_transitions(&mut *tx, key).await?
    } else {
        0
    };
    let moved_to = issues::store(&mut tx, &record)
        .await?
        .moved_from
        .map(|_| record.key.clone());
    if refused && !status_change {
        assignable::forget(&mut *tx, key).await?;
    }
    if refused
        && status_change
        && let Some(issue_type) = &record.issue_type
    {
        let position = Position {
            project_key: record.project_key.clone(),
            issue_type: issue_type.clone(),
            status: record.status.clone(),
        };
        transitions::forget(&mut *tx, &position).await?;
    }
    tx.commit().await?;
    // The callers announce the key the issue had; a view open under it
    // follows the move only if the new one is announced too
    Ok(moved_to
        .map(|key| SyncEvent::IssuesUpdated(vec![key]))
        .into_iter()
        .chain((0..later).map(|_| rejection(key, "transition", EARLIER_REJECTED)))
        .collect())
}

/// Rolls back the transitions whose rollback could not read Jira at the time.
/// Whether Jira refused them is not known any more, so the cached menu of the
/// status the issue is in is dropped as after a refusal. An issue that Jira
/// cannot be asked about stays as it is and is tried again in the next cycle.
pub(crate) async fn roll_back_unconfirmed(
    state: &AppState,
    config: &Config,
    changed: &mut Vec<IssueKey>,
) {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let Ok(keys) = issues::unconfirmed(&state.pool).await else {
        return;
    };
    for key in keys {
        let Ok(later) = roll_back(state, &client, &key, "transition", true).await else {
            continue;
        };
        for event in later {
            state.events.send(event).ok();
        }
        if !changed.contains(&key) {
            changed.push(key);
        }
    }
}

// The request is built here, not when the entry is queued: the entry holds
// what the user did, and how that reaches Jira can depend on the settings at
// the time of delivery: a worklog queued before the user switched the target
// goes to the target set now.
//
// What Jira created is returned: the cache holds a comment until the next poll
// brings the issue, so that it does not vanish in between, and a worklog keeps
// the id Jira gave it.
async fn deliver(
    client: &JiraClient,
    state: &AppState,
    config: &Config,
    entry: &QueueEntry,
) -> anyhow::Result<Delivered> {
    let payload: Value = serde_json::from_str(&entry.payload)?;
    match entry.entity_type.as_str() {
        "transition" => {
            client.transition_issue(&entry.issue_key, &payload).await?;
            Ok(Delivered::Nothing)
        }
        "assignee" => {
            client.assign_issue(&entry.issue_key, &payload).await?;
            Ok(Delivered::Nothing)
        }
        "comment" => {
            let text = payload["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("a comment entry without text"))?;
            let mut body = json!({ "body": adf::from_plain_text(text) });
            if let Some(parent) = payload["parent_id"].as_str() {
                body["parentId"] = parent.into();
            }
            Ok(Delivered::Comment(
                client.add_comment(&entry.issue_key, &body).await?,
            ))
        }
        "worklog" => {
            let local_id = payload["local_id"].as_str().unwrap_or_default().to_owned();
            if config.worklog_target == WorklogTarget::Tempo {
                let tempo_id = post_to_tempo(client, state, config, entry, &payload).await?;
                return Ok(Delivered::TempoWorklog { local_id, tempo_id });
            }
            let started =
                worklogs::started_at_for_jira(payload["started_at"].as_str().unwrap_or_default())
                    .map_err(|error| {
                    anyhow::anyhow!("a worklog entry with an unreadable start: {error}")
                })?;
            let mut body = json!({
                "timeSpentSeconds": payload["time_spent_seconds"],
                "started": started,
            });
            if let Some(comment) = payload["comment"].as_str() {
                body["comment"] = adf::from_plain_text(comment);
            }
            let created = client.add_worklog(&entry.issue_key, &body).await?;
            let remote_id = created["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Jira answered a worklog without an id"))?
                .to_owned();
            Ok(Delivered::Worklog {
                local_id,
                remote_id,
            })
        }
        other => anyhow::bail!("unknown outbox entry type `{other}`"),
    }
}

/// The base URL is not a setting: the Tempo token goes only to Tempo. Tests
/// point it at a mock server through the state.
pub(crate) fn tempo_url(state: &AppState) -> String {
    #[cfg(test)]
    return state.tempo_url.lock().unwrap().clone();
    #[cfg(not(test))]
    {
        let _ = state;
        crate::auth::TEMPO_URL.to_owned()
    }
}

/// Returns Tempo's id of the worklog.
async fn post_to_tempo(
    client: &JiraClient,
    state: &AppState,
    config: &Config,
    entry: &QueueEntry,
    payload: &Value,
) -> anyhow::Result<i64> {
    // Tempo takes the numeric id of the issue, not its key
    let issue_id: i64 = issues::id_of(&state.pool, &entry.issue_key)
        .await?
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("{} is not in the cache", entry.issue_key))?;
    let started =
        chrono::DateTime::parse_from_rfc3339(payload["started_at"].as_str().unwrap_or_default())
            .map_err(|error| {
                anyhow::anyhow!("a worklog entry with an unreadable start: {error}")
            })?;
    // Tempo takes a date and a time without an offset and reads them in the
    // time zone of the author's Jira profile. Asked for each worklog: the
    // user can change the profile at any time.
    let time_zone: chrono_tz::Tz = client
        .myself()
        .await?
        .time_zone
        .and_then(|name| name.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Jira did not say the time zone of the profile"))?;
    let started = started.with_timezone(&time_zone);
    let mut body = json!({
        "issueId": issue_id,
        "authorAccountId": config.account_id,
        "timeSpentSeconds": payload["time_spent_seconds"],
        "startDate": started.format("%Y-%m-%d").to_string(),
        "startTime": started.format("%H:%M:%S").to_string(),
    });
    if let Some(comment) = payload["comment"].as_str() {
        body["description"] = comment.into();
    }
    Ok(TempoClient::new(&tempo_url(state), &config.tempo_api_token)
        .create_worklog(&body)
        .await?)
}

enum Delivered {
    Nothing,
    Comment(Value),
    Worklog { local_id: String, remote_id: String },
    TempoWorklog { local_id: String, tempo_id: i64 },
}

fn failure(error: &anyhow::Error) -> Failure {
    match error.downcast_ref() {
        Some(JiraError::Unauthorized | JiraError::Http(_)) => Failure::NotAsked,
        Some(JiraError::Status(status, _)) if transient(*status) => Failure::Transient,
        _ => match error.downcast_ref() {
            Some(TempoError::Http(_)) => Failure::NotAsked,
            Some(TempoError::Status(status, _)) if transient(*status) => Failure::Transient,
            _ => Failure::Rejected,
        },
    }
}

fn transient(status: StatusCode) -> bool {
    status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api,
        db::{sync_state, timer},
        models::{RunningTimer, StatusCategory, SyncStatus, Transition, User},
        start_syncing_without_keychain, start_without_keychain,
        sync::inbound,
    };
    use serde_json::{Value, json};
    use std::{path::Path, sync::Arc};
    use tokio::sync::mpsc::UnboundedReceiver;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{basic_auth, method, path},
    };

    const TOKEN: &str = "secret-token";

    fn config(site_url: &str) -> Config {
        Config {
            site_url: site_url.to_owned(),
            email: "ada@example.com".to_owned(),
            account_id: "5b10a".to_owned(),
            display_name: None,
            worklog_target: Default::default(),
            shortcut_hints: true,
            api_token: TOKEN.to_owned(),
            tempo_api_token: String::new(),
        }
    }

    /// Queues the transition `transition_id` of `key` and returns the entry's id.
    async fn enqueue(state: &AppState, key: &str, transition_id: &str) -> i64 {
        let payload = json!({ "transition": { "id": transition_id }, "fields": {} });
        sqlx::query_scalar(
            "INSERT INTO sync_queue (entity_type, issue_key, operation, payload)
             VALUES ('transition', ?, 'create', ?) RETURNING id",
        )
        .bind(key)
        .bind(payload.to_string())
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// The status, the retry count and the error of an entry.
    async fn entry(state: &AppState, id: i64) -> (String, i64, Option<String>) {
        sqlx::query_as("SELECT status, retry_count, error_msg FROM sync_queue WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn entry_count(state: &AppState, id: i64) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM sync_queue WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// Jira's transition call for `key`.
    fn transition(key: &str) -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(format!("/rest/api/3/issue/{key}/transitions")))
    }

    /// The paths and bodies of the requests Jira received, in order.
    async fn requests(server: &MockServer) -> Vec<(String, Value)> {
        let requests = server.received_requests().await.unwrap();
        requests
            .iter()
            .map(|request| {
                let body = serde_json::from_slice(&request.body).unwrap_or_default();
                (request.url.path().to_owned(), body)
            })
            .collect()
    }

    fn received(events: &mut UnboundedReceiver<SyncEvent>) -> Vec<String> {
        let mut received = Vec::new();
        while let Ok(event) = events.try_recv() {
            received.push(format!("{event:?}"));
        }
        received
    }

    fn rejected(key: &str, action: &str, reason: &str) -> String {
        format!(
            "ChangeRejected {{ key: IssueKey({key:?}), action: {action:?}, reason: {reason:?} }}"
        )
    }

    fn keys(keys: &[&str]) -> Vec<IssueKey> {
        keys.iter().map(|key| IssueKey::from(*key)).collect()
    }

    #[tokio::test]
    async fn entries_are_delivered_in_creation_order() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        for key in ["WEB-1", "WEB-2"] {
            transition(key)
                .and(basic_auth("ada@example.com", TOKEN))
                .respond_with(ResponseTemplate::new(204))
                .mount(&server)
                .await;
        }
        let first = enqueue(&state, "WEB-2", "11").await;
        let second = enqueue(&state, "WEB-1", "21").await;
        let third = enqueue(&state, "WEB-2", "31").await;

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        let sent: Vec<_> = requests(&server)
            .await
            .into_iter()
            .map(|(path, body)| (path, body["transition"]["id"].clone()))
            .collect();
        assert_eq!(
            sent,
            [
                (
                    "/rest/api/3/issue/WEB-2/transitions".to_owned(),
                    json!("11")
                ),
                (
                    "/rest/api/3/issue/WEB-1/transitions".to_owned(),
                    json!("21")
                ),
                (
                    "/rest/api/3/issue/WEB-2/transitions".to_owned(),
                    json!("31")
                ),
            ]
        );
        assert_eq!(delivered, keys(&["WEB-2", "WEB-1"]));
        for id in [first, second, third] {
            assert_eq!(entry(&state, id).await, ("synced".to_owned(), 0, None));
        }
        let unstamped: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_queue WHERE synced_at IS NULL")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(unstamped, 0);
        assert!(received(&mut events).is_empty());
    }

    #[tokio::test]
    async fn a_failed_entry_is_retried_until_jira_accepts_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let id = enqueue(&state, "WEB-1", "11").await;

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(entry(&state, id).await, ("synced".to_owned(), 2, None));
        assert_eq!(delivered, keys(&["WEB-1"]));
        assert!(received(&mut events).is_empty());
    }

    #[tokio::test]
    async fn an_entry_that_keeps_failing_is_given_up_after_three_retries() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        // The first attempt and three retries
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(503).set_body_string("down for maintenance"))
            .expect(4)
            .mount(&server)
            .await;
        transition("WEB-2")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let failing = enqueue(&state, "WEB-1", "11").await;
        let next = enqueue(&state, "WEB-2", "21").await;

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        let error = "Jira returned 503 Service Unavailable: down for maintenance";
        assert_eq!(entry_count(&state, failing).await, 0);
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected("WEB-1", "status change", error)
            ]
        );
        // The entries behind it are not held up
        assert_eq!(entry(&state, next).await.0, "synced");
        assert_eq!(delivered, keys(&["WEB-2"]));

        // A later pass leaves the failed entry alone
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_entry_jira_rejects_fails_without_a_retry() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("no such transition"))
            .expect(1)
            .mount(&server)
            .await;
        let id = enqueue(&state, "WEB-1", "11").await;

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        let error = "Jira returned 400 Bad Request: no such transition";
        assert_eq!(entry_count(&state, id).await, 0);
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected("WEB-1", "status change", error)
            ]
        );
        assert!(delivered.is_empty());
    }

    #[tokio::test]
    async fn entries_wait_while_jira_is_unreachable() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        let id = enqueue(&state, "WEB-1", "11").await;
        // A port that was free a moment ago: nothing listens on it. A dropped
        // `MockServer` would not do, wiremock keeps its listener for reuse.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let offline = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let mut delivered = Vec::new();
        let error = process_queue(&state, &config(&offline), &mut delivered)
            .await
            .unwrap_err();

        assert!(matches!(error.downcast_ref(), Some(JiraError::Http(_))));
        assert_eq!(entry(&state, id).await, ("pending".to_owned(), 0, None));

        let server = MockServer::start().await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(entry(&state, id).await, ("synced".to_owned(), 0, None));
        assert_eq!(delivered, keys(&["WEB-1"]));
        assert!(received(&mut events).is_empty());
    }

    #[tokio::test]
    async fn an_entry_interrupted_by_a_crash_is_delivered_after_the_restart() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let interrupted = enqueue(&state, "WEB-1", "11").await;
        let waiting = enqueue(&state, "WEB-2", "21").await;
        // The app went down between claiming the first entry and Jira's answer
        claim(&state).await;
        state.pool.close().await;

        for key in ["WEB-1", "WEB-2"] {
            transition(key)
                .respond_with(ResponseTemplate::new(204))
                .expect(1)
                .mount(&server)
                .await;
        }
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(entry(&state, interrupted).await.0, "synced");
        assert_eq!(entry(&state, waiting).await.0, "synced");
        assert_eq!(delivered, keys(&["WEB-1", "WEB-2"]));
    }

    async fn claim(state: &AppState) {
        let claimed = sync_queue::claim_next(&state.pool, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry(state, claimed.id).await.0, "in_progress");
    }

    /// A core with a running sync engine that has finished the initial sync
    /// with `server`, where no search finds anything. The engine sees the
    /// account only once `sign_in` is called, so a test can queue entries first.
    async fn syncing(
        server: &MockServer,
        data_dir: &Path,
    ) -> (Arc<AppState>, UnboundedReceiver<SyncEvent>) {
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": [] })))
            .mount(server)
            .await;
        crate::sync::no_worklog_changes().mount(server).await;
        crate::sync::no_sprint_field().mount(server).await;
        start_syncing_without_keychain(data_dir).await.unwrap()
    }

    async fn sign_in(state: &AppState, server: &MockServer) {
        *state.config.write().await = Some(config(&server.uri()));
        sync_state::set_synced(&state.pool, &["WEB"], inbound::now_ms().unwrap())
            .await
            .unwrap();
        state.sync_waker.send(()).unwrap();
    }

    /// The events of one sync cycle, up to the one that ends it.
    async fn cycle(events: &mut UnboundedReceiver<SyncEvent>) -> Vec<String> {
        let mut cycle = Vec::new();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("the sync cycle did not end")
                .unwrap();
            let last = matches!(event, SyncEvent::Completed | SyncEvent::TokenRejected);
            cycle.push(format!("{event:?}"));
            if last {
                return cycle;
            }
        }
    }

    #[tokio::test]
    async fn a_sign_out_stops_a_cycle_that_waits_for_jira() {
        let server = MockServer::start().await;
        let never = ResponseTemplate::new(200)
            .set_body_json(json!({ "issues": [] }))
            .set_delay(Duration::from_secs(3600));
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(never)
            .mount(&server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_syncing_without_keychain(data_dir.path())
            .await
            .unwrap();
        sign_in(&state, &server).await;
        assert!(matches!(events.recv().await, Some(SyncEvent::Started)));

        tokio::time::timeout(Duration::from_secs(5), api::sign_out(&state))
            .await
            .expect("the sign-out waited for the sync")
            .unwrap();

        assert_eq!(api::account(&state).await, None);
    }

    #[tokio::test]
    async fn a_sync_cycle_learns_the_name_of_the_user_a_config_does_not_have() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/myself"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada Lovelace" })),
            )
            // The cycle after the one that learnt the name does not ask
            .expect(1)
            .mount(&server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = syncing(&server, data_dir.path()).await;
        sign_in(&state, &server).await;
        cycle(&mut events).await;

        let name = |state: Arc<AppState>| async move {
            let config = state.config.read().await;
            config.as_ref().unwrap().display_name.clone()
        };
        assert_eq!(name(state.clone()).await.as_deref(), Some("Ada Lovelace"));
        let saved = std::fs::read_to_string(data_dir.path().join("config.json")).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(saved["display_name"], "Ada Lovelace");

        state.sync_waker.send(()).unwrap();
        cycle(&mut events).await;
    }

    #[tokio::test]
    async fn a_sync_cycle_flushes_the_queue_before_it_polls() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = syncing(&server, data_dir.path()).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let id = enqueue(&state, "WEB-1", "11").await;

        sign_in(&state, &server).await;

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        assert_eq!(entry(&state, id).await.0, "synced");
        let paths: Vec<String> = requests(&server)
            .await
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(
            paths[..2],
            [
                "/rest/api/3/issue/WEB-1/transitions",
                "/rest/api/3/search/jql"
            ]
        );
    }

    fn start() -> Transition {
        Transition {
            id: "21".to_owned(),
            name: "Start".to_owned(),
            to_status: "In Progress".to_owned(),
            to_status_category: StatusCategory::InProgress,
            required_fields: Vec::new(),
        }
    }

    fn review() -> Transition {
        Transition {
            id: "31".to_owned(),
            name: "Review".to_owned(),
            to_status: "In Review".to_owned(),
            to_status_category: StatusCategory::InProgress,
            ..start()
        }
    }

    /// Puts WEB-1 into the cache, in To Do.
    async fn store_issue(state: &AppState) {
        let issue = issue_record::issue_record(&json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "issuetype": { "name": "Bug" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .unwrap();
        issues::upsert(&state.pool, &issue).await.unwrap();
    }

    /// What Jira answers when it is asked for WEB-1, which it still has in To Do.
    fn jira_has_web_1_in_to_do() -> Mock {
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "10001",
                "key": "WEB-1",
                "fields": {
                    "summary": "Fix login",
                    "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                    "project": { "key": "WEB" },
                    "issuetype": { "name": "Bug" },
                    "updated": "2026-03-29T01:30:00.000+0100",
                },
            })))
    }

    /// What the transition cache holds for WEB-1's workflow, by status.
    async fn cached_statuses(state: &AppState) -> Vec<String> {
        sqlx::query_scalar("SELECT from_status FROM transition_cache ORDER BY from_status")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    async fn cache_transitions_of(state: &AppState, statuses: &[&str]) {
        for status in statuses {
            let position = crate::db::transitions::Position {
                project_key: "WEB".to_owned(),
                issue_type: "Bug".to_owned(),
                status: (*status).to_owned(),
            };
            crate::db::transitions::put(&state.pool, &position, &[start()])
                .await
                .unwrap();
        }
    }

    /// The status of WEB-1 and whether it waits for Jira.
    async fn issue(state: &AppState) -> (String, SyncStatus) {
        let issue = issues::get(&state.pool, &"WEB-1".into()).await;
        let issue = issue.unwrap().unwrap();
        (issue.status, issue.sync_status)
    }

    #[tokio::test]
    async fn an_issue_stays_pending_until_jira_has_its_last_transition() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        // Jira takes the first transition, then stops accepting the token
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(401))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let key = IssueKey::from("WEB-1");
        api::transition_issue(&state, &key, &start(), &json!({}))
            .await
            .unwrap();
        api::transition_issue(&state, &key, &review(), &json!({}))
            .await
            .unwrap();

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap_err();
        assert_eq!(
            issue(&state).await,
            ("In Review".to_owned(), SyncStatus::Pending)
        );

        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        assert_eq!(
            issue(&state).await,
            ("In Review".to_owned(), SyncStatus::Synced)
        );
        let sent: Vec<Value> = requests(&server)
            .await
            .into_iter()
            .map(|(_, body)| body["transition"]["id"].clone())
            .collect();
        assert_eq!(sent, [json!("21"), json!("31"), json!("31")]);
    }

    #[tokio::test]
    async fn a_transition_is_sent_without_waiting_for_the_next_poll() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = syncing(&server, data_dir.path()).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        sign_in(&state, &server).await;
        cycle(&mut events).await;
        store_issue(&state).await;

        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        // The next poll is three minutes away: only a woken engine ends this
        // wait before `cycle` gives up
        while issue(&state).await.1 == SyncStatus::Pending {
            cycle(&mut events).await;
        }
        assert_eq!(issue(&state).await.0, "In Progress");
    }

    #[tokio::test]
    async fn entries_wait_for_a_new_token_after_a_401() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = syncing(&server, data_dir.path()).await;
        // Jira has stopped accepting the saved token. It is asked once: a
        // second request with that token would fail the test.
        transition("WEB-1")
            .and(basic_auth("ada@example.com", TOKEN))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        transition("WEB-1")
            .and(basic_auth("ada@example.com", "new-token"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/myself"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" })),
            )
            .mount(&server)
            .await;
        let id = enqueue(&state, "WEB-1", "11").await;

        sign_in(&state, &server).await;

        assert_eq!(cycle(&mut events).await, ["Started", "TokenRejected"]);
        assert_eq!(entry(&state, id).await, ("pending".to_owned(), 0, None));

        api::renew_token(&state, "new-token").await.unwrap();

        assert_eq!(
            cycle(&mut events).await,
            [
                "Started",
                r#"IssuesUpdated([IssueKey("WEB-1")])"#,
                "Completed"
            ]
        );
        assert_eq!(entry(&state, id).await, ("synced".to_owned(), 0, None));
    }

    fn comment_call(key: &str) -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(format!("/rest/api/3/issue/{key}/comment")))
    }

    /// WEB-1's comments: whether each waits for Jira, and its Markdown.
    async fn comments(state: &AppState) -> Vec<(bool, String)> {
        let detail =
            issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost", "You").await;
        let detail = detail.unwrap().unwrap();
        assert_eq!(detail.comment_total, detail.comments.len());
        detail
            .comments
            .into_iter()
            .map(|comment| (comment.pending, comment.body_md))
            .collect()
    }

    /// The comment Jira answers a post with, and later sends with the issue.
    fn created_comment() -> Value {
        json!({
            "id": "10100",
            "author": { "displayName": "Ada Lovelace" },
            "created": "2026-03-30T10:00:00.000+0100",
            "body": {
                "type": "doc",
                "version": 1,
                "content": [{ "type": "paragraph", "content": [
                    { "type": "text", "text": "Seen on staging" },
                    { "type": "hardBreak" },
                    { "type": "text", "text": "and in production" },
                ] }],
            },
        })
    }

    #[tokio::test]
    async fn a_comment_is_sent_as_adf_and_the_issue_keeps_it_once_jira_has_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        comment_call("WEB-1")
            .and(basic_auth("ada@example.com", TOKEN))
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&server)
            .await;
        api::add_comment(
            &state,
            &"WEB-1".into(),
            "Seen on staging\nand in production",
        )
        .await
        .unwrap();
        let pending = (true, "Seen on staging\\\nand in production".to_owned());
        assert_eq!(comments(&state).await, [pending]);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        let sent = requests(&server).await;
        assert_eq!(
            sent,
            [(
                "/rest/api/3/issue/WEB-1/comment".to_owned(),
                json!({ "body": {
                    "type": "doc",
                    "version": 1,
                    "content": [{ "type": "paragraph", "content": [
                        { "type": "text", "text": "Seen on staging" },
                        { "type": "hardBreak" },
                        { "type": "text", "text": "and in production" },
                    ] }],
                } })
            )]
        );
        let confirmed = (false, "Seen on staging\\\nand in production".to_owned());
        assert_eq!(comments(&state).await, std::slice::from_ref(&confirmed));
        assert_eq!(delivered, keys(&["WEB-1"]));

        // The next poll brings the issue with the comment Jira has stored
        let polled = issue_record::issue_record(&json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-30T10:00:00.000+0100",
                "comment": { "total": 1, "comments": [created_comment()] },
            },
        }))
        .unwrap();
        assert!(issues::upsert(&state.pool, &polled).await.unwrap());
        assert_eq!(comments(&state).await, [confirmed]);
    }

    #[tokio::test]
    async fn a_reply_is_sent_with_its_parent_and_stays_in_the_thread_once_jira_has_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let mut second = created_comment();
        second["id"] = json!("10101");
        let discussed = json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
                "comment": { "total": 2, "comments": [created_comment(), second] },
            },
        });
        let record = issue_record::issue_record(&discussed).unwrap();
        issues::upsert(&state.pool, &record).await.unwrap();
        // Jira sends the parent of the comment it has created as a number
        let mut created = created_comment();
        created["id"] = json!("10102");
        created["parentId"] = json!(10100);
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(created))
            .mount(&server)
            .await;
        api::reply_to_comment(&state, &"WEB-1".into(), "10100", "Which build?")
            .await
            .unwrap();

        process_queue(&state, &config(&server.uri()), &mut Vec::new())
            .await
            .unwrap();

        let sent = requests(&server).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1["parentId"], "10100");
        assert_eq!(
            sent[0].1["body"]["content"][0]["content"][0]["text"],
            "Which build?"
        );
        let detail =
            issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost", "You").await;
        let thread: Vec<(Option<String>, bool)> = detail
            .unwrap()
            .unwrap()
            .comments
            .into_iter()
            .map(|comment| (comment.id, comment.reply_to.is_some()))
            .collect();
        assert_eq!(
            thread,
            [
                (Some("10100".to_owned()), false),
                (Some("10102".to_owned()), true),
                (Some("10101".to_owned()), false),
            ]
        );
    }

    #[tokio::test]
    async fn a_comment_written_offline_is_delivered_after_reconnect() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        api::add_comment(&state, &"WEB-1".into(), "Written on the train")
            .await
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let offline = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&offline), &mut delivered)
            .await
            .unwrap_err();

        let waiting = (true, "Written on the train".to_owned());
        assert_eq!(comments(&state).await, [waiting]);

        let server = MockServer::start().await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&server)
            .await;
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(delivered, keys(&["WEB-1"]));
        let sent = requests(&server).await;
        assert_eq!(
            sent[0].1["body"]["content"][0]["content"][0]["text"],
            "Written on the train"
        );
        assert_eq!(comments(&state).await.len(), 1);
        assert!(!comments(&state).await[0].0);
    }

    fn worklog_call(key: &str) -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(format!("/rest/api/3/issue/{key}/worklog")))
    }

    /// 2026-03-29 08:15:00 UTC
    const STARTED: i64 = 1_774_772_100;

    /// Every worklog of WEB-1: the remote id, the sync status, the seconds and the start.
    async fn worklog_rows(state: &AppState) -> Vec<(Option<String>, String, i64, String)> {
        sqlx::query_as(
            "SELECT remote_id, sync_status, time_spent_sec, started_at FROM worklogs
             WHERE issue_key = 'WEB-1' ORDER BY created_at, rowid",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn log_time_stores_the_worklog_and_a_target_neutral_entry_together() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;

        api::log_time(&state, &"WEB-1".into(), 5400, STARTED, Some("  Pairing  "))
            .await
            .unwrap();

        let started = "2026-03-29T08:15:00.000Z".to_owned();
        assert_eq!(
            worklog_rows(&state).await,
            [(None, "pending".to_owned(), 5400, started.clone())]
        );
        let (local_id, comment): (String, Option<String>) =
            sqlx::query_as("SELECT local_id, comment FROM worklogs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(comment.as_deref(), Some("Pairing"));
        let queued: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT entity_type, issue_key, status, payload FROM sync_queue")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "worklog");
        assert_eq!(queued[0].1, "WEB-1");
        assert_eq!(queued[0].2, "pending");
        // Nothing in the entry is Jira's request: the target builds it when it sends
        let payload: Value = serde_json::from_str(&queued[0].3).unwrap();
        assert_eq!(
            payload,
            json!({
                "local_id": local_id,
                "time_spent_seconds": 5400,
                "started_at": started,
                "comment": "Pairing",
            })
        );
        assert_eq!(
            received(&mut events),
            ["IssuesUpdated([IssueKey(\"WEB-1\")])"]
        );
    }

    #[tokio::test]
    async fn log_time_refuses_what_cannot_be_delivered_and_stores_nothing() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;

        let unknown = api::log_time(&state, &"WEB-9".into(), 60, STARTED, None).await;
        let too_short = api::log_time(&state, &"WEB-1".into(), 59, STARTED, None).await;

        assert_eq!(unknown.unwrap_err().to_string(), "issue WEB-9 not found");
        assert_eq!(
            too_short.unwrap_err().to_string(),
            "the time spent cannot be less than a minute"
        );
        let rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM worklogs) + (SELECT COUNT(*) FROM sync_queue)",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn log_time_leaves_no_worklog_when_its_entry_cannot_be_stored() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        sqlx::query("DROP TABLE sync_queue")
            .execute(&state.pool)
            .await
            .unwrap();

        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap_err();

        assert!(worklog_rows(&state).await.is_empty());
    }

    #[tokio::test]
    async fn a_worklog_is_sent_with_its_time_start_and_comment_and_keeps_the_remote_id() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        worklog_call("WEB-1")
            .and(basic_auth("ada@example.com", TOKEN))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "30001" })))
            .expect(1)
            .mount(&server)
            .await;
        api::log_time(&state, &"WEB-1".into(), 5400, STARTED, Some("Pairing"))
            .await
            .unwrap();

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            requests(&server).await,
            [(
                "/rest/api/3/issue/WEB-1/worklog".to_owned(),
                json!({
                    "timeSpentSeconds": 5400,
                    "started": "2026-03-29T08:15:00.000+0000",
                    "comment": { "type": "doc", "version": 1, "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "Pairing" }] },
                    ] },
                })
            )]
        );
        assert_eq!(
            worklog_rows(&state).await,
            [(
                Some("30001".to_owned()),
                "synced".to_owned(),
                5400,
                "2026-03-29T08:15:00.000Z".to_owned()
            )]
        );
        assert_eq!(delivered, keys(&["WEB-1"]));
        let waiting: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_queue WHERE status != 'synced'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(waiting, 0);
    }

    #[tokio::test]
    async fn a_worklog_without_a_comment_is_sent_without_one() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "30001" })))
            .mount(&server)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, Some("   "))
            .await
            .unwrap();

        process_queue(&state, &config(&server.uri()), &mut Vec::new())
            .await
            .unwrap();

        assert_eq!(
            requests(&server).await[0].1,
            json!({ "timeSpentSeconds": 600, "started": "2026-03-29T08:15:00.000+0000" })
        );
    }

    #[tokio::test]
    async fn a_worklog_created_offline_is_delivered_after_reconnect_with_its_original_start() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        api::log_time(&state, &"WEB-1".into(), 1800, STARTED, None)
            .await
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let offline = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&offline), &mut delivered)
            .await
            .unwrap_err();

        assert_eq!(worklog_rows(&state).await[0].0, None);
        assert_eq!(worklog_rows(&state).await[0].1, "pending");

        // Long after the work was done
        let server = MockServer::start().await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "30002" })))
            .expect(1)
            .mount(&server)
            .await;
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            requests(&server).await[0].1["started"],
            "2026-03-29T08:15:00.000+0000"
        );
        assert_eq!(worklog_rows(&state).await[0].0.as_deref(), Some("30002"));
        assert_eq!(worklog_rows(&state).await[0].1, "synced");
        assert_eq!(delivered, keys(&["WEB-1"]));
    }

    #[tokio::test]
    async fn a_rejected_worklog_is_marked_failed_and_announced() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("no permission"))
            .expect(1)
            .mount(&server)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(worklog_rows(&state).await[0].0, None);
        assert_eq!(worklog_rows(&state).await[0].1, "failed");
        let status: String = sqlx::query_scalar("SELECT status FROM sync_queue")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "failed");
        assert!(delivered.is_empty());
        let events = received(&mut events);
        assert_eq!(events[0], "IssuesUpdated([IssueKey(\"WEB-1\")])");
        assert!(events[1].starts_with("ChangeRejected") && events[1].contains("worklog"));
    }

    /// Logs ten minutes on WEB-1 and lets Jira refuse them. Returns the entry
    /// that `api::retry_worklog` and `api::discard_worklog` take.
    async fn refused_worklog(state: &AppState, server: &MockServer) -> i64 {
        // `api::get_issue` reads the site from the account
        *state.config.write().await = Some(config(&server.uri()));
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("no permission"))
            .up_to_n_times(1)
            .mount(server)
            .await;
        api::log_time(state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        process_queue(state, &config(&server.uri()), &mut Vec::new())
            .await
            .unwrap();
        let issue = api::get_issue(state, &"WEB-1".into()).await.unwrap();
        let rejected = issue.worklogs[0].rejected.clone().unwrap();
        assert_eq!(
            rejected.reason,
            "Jira returned 400 Bad Request: no permission"
        );
        rejected.entry_id
    }

    #[tokio::test]
    async fn a_refused_worklog_says_why_and_a_retry_delivers_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        let entry_id = refused_worklog(&state, &server).await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "30001" })))
            .mount(&server)
            .await;

        api::retry_worklog(&state, entry_id).await.unwrap();

        assert_eq!(worklog_rows(&state).await[0].1, "pending");
        process_queue(&state, &config(&server.uri()), &mut Vec::new())
            .await
            .unwrap();
        let rows = worklog_rows(&state).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].0.as_deref(), rows[0].1.as_str()),
            (Some("30001"), "synced")
        );
        let issue = api::get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.worklogs[0].rejected, None);
    }

    #[tokio::test]
    async fn a_refused_worklog_can_be_discarded_with_its_outbox_entry() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        let entry_id = refused_worklog(&state, &server).await;

        api::discard_worklog(&state, entry_id).await.unwrap();

        assert!(worklog_rows(&state).await.is_empty());
        let entries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_queue")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(entries, 0);
        // The entry is gone, so there is nothing to discard twice
        assert!(api::discard_worklog(&state, entry_id).await.is_err());
    }

    #[tokio::test]
    async fn a_rejected_transition_shows_the_status_jira_has_and_drops_its_cached_menu() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        cache_transitions_of(&state, &["To Do", "In Progress"]).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .mount(&server)
            .await;
        jira_has_web_1_in_to_do().mount(&server).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        assert_eq!(
            issue(&state).await,
            ("In Progress".to_owned(), SyncStatus::Pending)
        );
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        // Jira's `updated` did not change, and the row still goes back
        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Synced)
        );
        // Only the menu of the status the issue is in is refetched
        assert_eq!(cached_statuses(&state).await, ["In Progress"]);
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "status change",
                    "Jira returned 400 Bad Request: Resolution is required"
                )
            ]
        );
    }

    async fn entry_statuses(state: &AppState) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT status, error_msg FROM sync_queue ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    const REFUSED: &str = "Jira returned 400 Bad Request: Resolution is required";
    const NOT_SENT: &str = "an earlier status change of this issue did not reach Jira";

    #[tokio::test]
    async fn a_transition_queued_behind_a_rejected_one_fails_with_it_and_is_announced() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        // Only the first one reaches Jira
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .expect(1)
            .mount(&server)
            .await;
        jira_has_web_1_in_to_do().mount(&server).await;
        let key = IssueKey::from("WEB-1");
        api::transition_issue(&state, &key, &start(), &json!({}))
            .await
            .unwrap();
        api::transition_issue(&state, &key, &review(), &json!({}))
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Synced)
        );
        assert_eq!(entry_statuses(&state).await, []);
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected("WEB-1", "status change", REFUSED),
                rejected("WEB-1", "status change", NOT_SENT)
            ]
        );
        assert!(delivered.is_empty());
    }

    #[tokio::test]
    async fn a_transition_chosen_while_a_rollback_reads_jira_fails_instead_of_being_overwritten() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .expect(1)
            .mount(&server)
            .await;
        // Slow enough for the user to choose a status before the answer arrives
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(500))
                    .set_body_json(json!({
                        "id": "10001",
                        "key": "WEB-1",
                        "fields": {
                            "summary": "Fix login",
                            "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                            "project": { "key": "WEB" },
                            "issuetype": { "name": "Bug" },
                            "updated": "2026-03-29T01:30:00.000+0100",
                        },
                    })),
            )
            .mount(&server)
            .await;
        let key = IssueKey::from("WEB-1");
        api::transition_issue(&state, &key, &start(), &json!({}))
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        let config = config(&server.uri());
        let queue = process_queue(&state, &config, &mut delivered);
        let user = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            api::transition_issue(&state, &key, &review(), &json!({}))
                .await
                .unwrap();
        };
        let (processed, ()) = tokio::join!(queue, user);
        processed.unwrap();

        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Synced)
        );
        assert_eq!(entry_statuses(&state).await, []);
        assert!(received(&mut events).contains(&rejected("WEB-1", "status change", NOT_SENT)));
    }

    #[tokio::test]
    async fn a_transition_given_up_on_after_server_errors_is_rolled_back_but_keeps_its_menu() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        cache_transitions_of(&state, &["To Do"]).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        jira_has_web_1_in_to_do().mount(&server).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Synced)
        );
        assert_eq!(cached_statuses(&state).await, ["To Do"]);
    }

    #[tokio::test]
    async fn a_rollback_that_could_not_read_jira_is_done_when_jira_answers_again() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        cache_transitions_of(&state, &["To Do", "In Progress"]).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        jira_has_web_1_in_to_do().mount(&server).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        received(&mut events);
        let config = config(&server.uri());

        let mut delivered = Vec::new();
        process_queue(&state, &config, &mut delivered)
            .await
            .unwrap();

        // The status Jira refused stays until Jira can be read
        assert_eq!(
            issue(&state).await,
            ("In Progress".to_owned(), SyncStatus::Pending)
        );
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "status change",
                    "Jira returned 400 Bad Request: Resolution is required"
                )
            ]
        );

        let mut changed = Vec::new();
        roll_back_unconfirmed(&state, &config, &mut changed).await;

        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Synced)
        );
        assert_eq!(changed, keys(&["WEB-1"]));
        assert_eq!(cached_statuses(&state).await, ["In Progress"]);
        // The user was told once, and nothing is left to roll back
        process_queue(&state, &config, &mut delivered)
            .await
            .unwrap();
        roll_back_unconfirmed(&state, &config, &mut changed).await;
        assert_eq!(received(&mut events), Vec::<String>::new());
        assert_eq!(changed, keys(&["WEB-1"]));
    }

    #[tokio::test]
    async fn a_status_waiting_in_the_outbox_is_not_rolled_back() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        jira_has_web_1_in_to_do().mount(&server).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        let mut changed = Vec::new();
        roll_back_unconfirmed(&state, &config(&server.uri()), &mut changed).await;

        assert_eq!(
            issue(&state).await,
            ("In Progress".to_owned(), SyncStatus::Pending)
        );
        assert!(changed.is_empty());
        assert!(requests(&server).await.is_empty());
    }

    #[tokio::test]
    async fn a_rejection_for_an_issue_jira_no_longer_has_is_still_announced() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(404).set_body_string("Issue Does Not Exist"))
            .mount(&server)
            .await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        // Nothing to read the status from: the row stays as it was
        assert_eq!(issue(&state).await.0, "In Progress");
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "status change",
                    "Jira returned 404 Not Found: Issue Does Not Exist"
                )
            ]
        );
    }

    #[tokio::test]
    async fn a_rejected_comment_keeps_its_text_until_it_is_retried_or_discarded() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(403).set_body_string("No permission"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&server)
            .await;
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        let reason = "Jira returned 403 Forbidden: No permission";
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected("WEB-1", "comment", reason)
            ]
        );
        let detail =
            issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost", "You").await;
        let detail = detail.unwrap().unwrap();
        assert_eq!(detail.comment_total, 1);
        let [comment] = &detail.comments[..] else {
            panic!("expected the refused comment, got {:?}", detail.comments)
        };
        assert_eq!(comment.body_md, "Seen on staging");
        assert!(!comment.pending);
        let refused = comment.rejected.clone().unwrap();
        assert_eq!(refused.reason, reason);
        // A refused comment is not a change Jira is still to receive
        assert_eq!(api::unsent_changes(&state).await.unwrap(), 0);

        api::retry_comment(&state, refused.entry_id).await.unwrap();
        assert_eq!(
            comments(&state).await,
            [(true, "Seen on staging".to_owned())]
        );
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        assert_eq!(
            comments(&state).await,
            [(false, "Seen on staging\\\nand in production".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_refused_comment_can_be_discarded_and_only_a_refused_one() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();
        api::add_comment(&state, &"WEB-1".into(), "Second")
            .await
            .unwrap();
        let second: i64 = sqlx::query_scalar("SELECT MAX(id) FROM sync_queue")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let mut delivered = Vec::new();
        // Both are refused; a third one is queued after the pass
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        api::add_comment(&state, &"WEB-1".into(), "Third")
            .await
            .unwrap();
        let third: i64 = sqlx::query_scalar("SELECT MAX(id) FROM sync_queue")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        api::discard_comment(&state, second).await.unwrap();

        let left: Vec<String> = comments(&state)
            .await
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(left, ["Seen on staging", "Third"]);
        // Not refused: it is still on its way
        api::discard_comment(&state, third).await.unwrap_err();
        api::retry_comment(&state, third).await.unwrap_err();
        assert_eq!(comments(&state).await.len(), 2);
    }

    #[tokio::test]
    async fn a_refused_comment_outlives_the_rollback_of_a_transition_of_its_issue() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .mount(&server)
            .await;
        jira_has_web_1_in_to_do().mount(&server).await;
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        api::transition_issue(&state, &"WEB-1".into(), &review(), &json!({}))
            .await
            .unwrap();

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            entry_statuses(&state).await,
            [(
                "failed".to_owned(),
                Some("Jira returned 403 Forbidden: ".to_owned())
            )]
        );
        let detail =
            issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost", "You").await;
        let comment = &detail.unwrap().unwrap().comments[0];
        let refused = comment.rejected.clone().unwrap();
        assert_eq!(comment.body_md, "Seen on staging");
        api::discard_comment(&state, refused.entry_id)
            .await
            .unwrap();
        assert_eq!(entry_statuses(&state).await, []);
    }

    #[tokio::test]
    async fn the_migration_removes_failed_transitions_and_keeps_failed_comments() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        for (entity_type, status) in [
            ("transition", "failed"),
            ("transition", "pending"),
            ("comment", "failed"),
        ] {
            sqlx::query(
                "INSERT INTO sync_queue (entity_type, issue_key, operation, payload, status)
                 VALUES (?, 'WEB-1', 'create', '{}', ?)",
            )
            .bind(entity_type)
            .bind(status)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        sqlx::raw_sql(include_str!(
            "../../migrations/0006_drop_failed_transitions.sql"
        ))
        .execute(&state.pool)
        .await
        .unwrap();

        let left: Vec<(String, String)> =
            sqlx::query_as("SELECT entity_type, status FROM sync_queue ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            left,
            [
                ("transition".to_owned(), "pending".to_owned()),
                ("comment".to_owned(), "failed".to_owned())
            ]
        );
    }

    /// Stops watching WEB, which deletes its cached issues.
    async fn unwatch_web(state: &AppState, server: &MockServer) {
        *state.config.write().await = Some(config(&server.uri()));
        api::watch_projects(state, &["API".to_owned()])
            .await
            .unwrap();
        assert!(
            issues::get(&state.pool, &"WEB-1".into())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_change_queued_for_a_removed_project_is_still_delivered() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&server)
            .await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        unwatch_web(&state, &server).await;
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(delivered, keys(&["WEB-1"]));
        assert_eq!(api::unsent_changes(&state).await.unwrap(), 0);
        assert!(
            issues::get(&state.pool, &"WEB-1".into())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(received(&mut events), Vec::<String>::new());
    }

    async fn worklogs_of(state: &AppState, key: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM worklogs WHERE issue_key = ?")
            .bind(key)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn worklog_entries(state: &AppState) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT status FROM sync_queue WHERE entity_type = 'worklog' ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn unwatching_a_project_removes_its_worklogs_and_timer_but_still_delivers_the_pending_worklog()
     {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "30002"})))
            .expect(1)
            .mount(&server)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO worklogs (remote_id, issue_key, time_spent_sec, started_at, sync_status)
             VALUES ('1', 'WEB-1', 60, '2026-03-29T08:15:00.000Z', 'synced'),
                    ('2', 'API-1', 60, '2026-03-29T08:15:00.000Z', 'synced')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        api::start_timer(
            &state,
            &RunningTimer {
                issue_key: "WEB-1".into(),
                started_at: STARTED,
            },
        )
        .await
        .unwrap();

        unwatch_web(&state, &server).await;

        assert_eq!(worklogs_of(&state, "WEB-1").await, 0);
        assert_eq!(worklogs_of(&state, "API-1").await, 1);
        assert!(api::running_timer(&state).await.unwrap().is_none());
        assert_eq!(worklog_entries(&state).await, ["pending"]);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(delivered, keys(&["WEB-1"]));
        assert_eq!(worklog_entries(&state).await, ["synced"]);
        assert_eq!(worklogs_of(&state, "WEB-1").await, 0);
    }

    #[tokio::test]
    async fn a_refused_worklog_of_a_removed_project_leaves_no_entry_that_nothing_can_show() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("no permission"))
            .expect(1)
            .mount(&server)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        api::log_time(&state, &"WEB-1".into(), 900, STARTED + 3600, None)
            .await
            .unwrap();
        // The first one was refused before the project was removed
        let first: i64 = sqlx::query_scalar("SELECT MIN(id) FROM sync_queue")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE sync_queue SET status = 'failed' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();

        unwatch_web(&state, &server).await;

        assert_eq!(worklog_entries(&state).await, ["pending"]);
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert!(delivered.is_empty());
        assert!(worklog_entries(&state).await.is_empty());
        assert!(
            received(&mut events)
                .iter()
                .any(|event| event.starts_with("ChangeRejected") && event.contains("worklog"))
        );
    }

    #[tokio::test]
    async fn a_rejected_change_for_a_removed_project_is_announced_and_does_not_bring_the_issue_back()
     {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .mount(&server)
            .await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(403).set_body_string("No permission"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&server)
            .await;
        // What the rollback reads: the issue must not come back with it
        jira_has_web_1_in_to_do().mount(&server).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();
        api::add_comment(&state, &"WEB-1".into(), "and in production")
            .await
            .unwrap();
        unwatch_web(&state, &server).await;
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        // The rejected entries did not stall the one behind them
        assert_eq!(delivered, keys(&["WEB-1"]));
        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM sync_queue ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(statuses, ["failed", "synced"]);
        assert!(
            issues::get(&state.pool, &"WEB-1".into())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "status change",
                    "Jira returned 400 Bad Request: Resolution is required"
                ),
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "comment",
                    "Jira returned 403 Forbidden: No permission"
                ),
            ]
        );

        // Watching the project again brings the refused comment back to where
        // it can be retried or discarded
        store_issue(&state).await;
        let detail =
            issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost", "You").await;
        let comment = &detail.unwrap().unwrap().comments[0];
        assert_eq!(comment.body_md, "Seen on staging");
        assert!(comment.rejected.is_some());
    }

    fn grace() -> User {
        User {
            account_id: "7c20b".to_owned(),
            display_name: "Grace Hopper".to_owned(),
        }
    }

    /// Jira's assignee call for `key`.
    fn assignee_call(key: &str) -> wiremock::MockBuilder {
        Mock::given(method("PUT")).and(path(format!("/rest/api/3/issue/{key}/assignee")))
    }

    /// WEB-1 as Jira has it: in To Do, assigned to Ada.
    fn web_1_of_ada() -> Value {
        json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "assignee": { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                "project": { "key": "WEB" },
                "issuetype": { "name": "Bug" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    fn jira_has_web_1_assigned_to_ada() -> Mock {
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(web_1_of_ada()))
    }

    /// Puts WEB-1 into the cache as Jira has it, assigned to Ada.
    async fn store_issue_of_ada(state: &AppState) {
        let record = issue_record::issue_record(&web_1_of_ada()).unwrap();
        issues::upsert(&state.pool, &record).await.unwrap();
    }

    /// The assignee of WEB-1 and whether it waits for Jira.
    async fn assignee(state: &AppState) -> (Option<String>, SyncStatus) {
        let issue = issues::get(&state.pool, &"WEB-1".into()).await;
        let issue = issue.unwrap().unwrap();
        (issue.assignee_name, issue.sync_status)
    }

    fn ada_name() -> Option<String> {
        Some("Ada Lovelace".to_owned())
    }

    fn grace_name() -> Option<String> {
        Some("Grace Hopper".to_owned())
    }

    async fn cache_people_of(state: &AppState, keys: &[&str]) {
        for key in keys {
            crate::db::assignable::put(&state.pool, &(*key).into(), &[grace()])
                .await
                .unwrap();
        }
    }

    async fn cached_people(state: &AppState) -> Vec<String> {
        sqlx::query_scalar("SELECT issue_key FROM assignable_users ORDER BY issue_key")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_assignee_is_sent_by_account_id_and_the_issue_keeps_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        assignee_call("WEB-1")
            .and(basic_auth("ada@example.com", TOKEN))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            requests(&server).await,
            [(
                "/rest/api/3/issue/WEB-1/assignee".to_owned(),
                json!({ "accountId": "7c20b" })
            )]
        );
        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Synced));
        assert_eq!(delivered, keys(&["WEB-1"]));
        assert!(received(&mut events).is_empty());
    }

    #[tokio::test]
    async fn an_issue_is_unassigned_by_sending_null() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        api::assign_issue(&state, &"WEB-1".into(), None)
            .await
            .unwrap();
        assert_eq!(assignee(&state).await, (None, SyncStatus::Pending));

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            requests(&server).await,
            [(
                "/rest/api/3/issue/WEB-1/assignee".to_owned(),
                json!({ "accountId": null })
            )]
        );
        assert_eq!(assignee(&state).await, (None, SyncStatus::Synced));
    }

    #[tokio::test]
    async fn a_rejected_assignee_shows_the_assignee_jira_has_and_drops_the_cached_people() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        cache_transitions_of(&state, &["To Do", "In Progress"]).await;
        cache_people_of(&state, &["WEB-1", "WEB-2"]).await;
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("user not found"))
            .expect(1)
            .mount(&server)
            .await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Pending));
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        // Jira's `updated` did not change, and the row still goes back
        assert_eq!(assignee(&state).await, (ada_name(), SyncStatus::Synced));
        assert_eq!(entry_statuses(&state).await, []);
        // The refused person is not offered again from the cache; the menus are not touched
        assert_eq!(cached_people(&state).await, ["WEB-2"]);
        assert_eq!(cached_statuses(&state).await, ["In Progress", "To Do"]);
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "assignee change",
                    "Jira returned 400 Bad Request: user not found"
                )
            ]
        );
        assert!(delivered.is_empty());
    }

    #[tokio::test]
    async fn an_assignee_given_up_on_after_server_errors_is_rolled_back_but_keeps_its_people() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        cache_people_of(&state, &["WEB-1"]).await;
        // The first attempt and three retries
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(503).set_body_string("down"))
            .expect(4)
            .mount(&server)
            .await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(assignee(&state).await, (ada_name(), SyncStatus::Synced));
        assert_eq!(cached_people(&state).await, ["WEB-1"]);
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "assignee change",
                    "Jira returned 503 Service Unavailable: down"
                )
            ]
        );
    }

    #[tokio::test]
    async fn an_assignee_chosen_offline_is_delivered_after_reconnect() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        // A port that was free a moment ago: nothing listens on it
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let offline = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let mut delivered = Vec::new();
        let error = process_queue(&state, &config(&offline), &mut delivered)
            .await
            .unwrap_err();

        assert!(matches!(error.downcast_ref(), Some(JiraError::Http(_))));
        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Pending));
        assert_eq!(entry_statuses(&state).await[0].0, "pending");

        let server = MockServer::start().await;
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Synced));
        assert_eq!(delivered, keys(&["WEB-1"]));
    }

    #[tokio::test]
    async fn an_assignee_rollback_that_could_not_read_jira_is_done_when_jira_answers_again() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("user not found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        received(&mut events);
        let config = config(&server.uri());

        let mut delivered = Vec::new();
        process_queue(&state, &config, &mut delivered)
            .await
            .unwrap();

        // The assignee Jira refused stays until Jira can be read, and the issue
        // is found by its row: nothing is left in the outbox
        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Pending));
        assert_eq!(entry_statuses(&state).await, []);

        let mut changed = Vec::new();
        roll_back_unconfirmed(&state, &config, &mut changed).await;

        assert_eq!(assignee(&state).await, (ada_name(), SyncStatus::Synced));
        assert_eq!(changed, keys(&["WEB-1"]));
    }

    /// WEB-1 changed by the user in two ways at once: in In Progress, assigned
    /// to Grace, and nothing sent yet. Jira has it in To Do, assigned to Ada.
    async fn both_changes_waiting(state: &AppState) {
        store_issue_of_ada(state).await;
        let key = IssueKey::from("WEB-1");
        api::transition_issue(state, &key, &start(), &json!({}))
            .await
            .unwrap();
        api::assign_issue(state, &key, Some(&grace()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_status_rollback_leaves_the_assignee_that_still_waits_in_the_outbox() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        both_changes_waiting(&state).await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        // The transition is the one that failed: its entry is gone
        sqlx::query("DELETE FROM sync_queue WHERE entity_type = 'transition'")
            .execute(&state.pool)
            .await
            .unwrap();
        let client = JiraClient::new(&server.uri(), "ada@example.com", TOKEN);

        roll_back(&state, &client, &"WEB-1".into(), "transition", true)
            .await
            .unwrap();

        // The status is Jira's, the assignee is still the user's, and the issue still waits
        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Pending)
        );
        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Pending));
        assert_eq!(entry_statuses(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn an_assignee_rollback_leaves_the_status_that_still_waits_in_the_outbox() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        both_changes_waiting(&state).await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        sqlx::query("DELETE FROM sync_queue WHERE entity_type = 'assignee'")
            .execute(&state.pool)
            .await
            .unwrap();
        cache_transitions_of(&state, &["To Do"]).await;
        let client = JiraClient::new(&server.uri(), "ada@example.com", TOKEN);

        let later = roll_back(&state, &client, &"WEB-1".into(), "assignee", true)
            .await
            .unwrap();

        // The assignee is Jira's, the status is still the user's, and the issue still waits
        assert_eq!(
            issue(&state).await,
            ("In Progress".to_owned(), SyncStatus::Pending)
        );
        assert_eq!(assignee(&state).await, (ada_name(), SyncStatus::Pending));
        // The transition is not dropped, and the menu it was chosen from stays
        assert!(later.is_empty());
        assert_eq!(entry_statuses(&state).await.len(), 1);
        assert_eq!(cached_statuses(&state).await, ["To Do"]);
    }

    #[tokio::test]
    async fn a_rejected_transition_does_not_take_the_queued_assignee_with_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        both_changes_waiting(&state).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .expect(1)
            .mount(&server)
            .await;
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        // Jira has the status it had and the assignee the user chose
        assert_eq!(
            issue(&state).await,
            ("To Do".to_owned(), SyncStatus::Synced)
        );
        assert_eq!(assignee(&state).await, (grace_name(), SyncStatus::Synced));
        assert_eq!(delivered, keys(&["WEB-1"]));
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected("WEB-1", "status change", REFUSED),
            ]
        );
    }

    #[tokio::test]
    async fn a_rejected_assignee_does_not_take_the_queued_transition_with_it() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        let key = IssueKey::from("WEB-1");
        api::assign_issue(&state, &key, Some(&grace()))
            .await
            .unwrap();
        api::transition_issue(&state, &key, &start(), &json!({}))
            .await
            .unwrap();
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("user not found"))
            .expect(1)
            .mount(&server)
            .await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        jira_has_web_1_assigned_to_ada().mount(&server).await;
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();

        // The status the user chose reached Jira, and the assignee is Jira's
        assert_eq!(
            issue(&state).await,
            ("In Progress".to_owned(), SyncStatus::Synced)
        );
        assert_eq!(assignee(&state).await, (ada_name(), SyncStatus::Synced));
        assert_eq!(delivered, keys(&["WEB-1"]));
        assert_eq!(
            received(&mut events),
            [
                r#"IssuesUpdated([IssueKey("WEB-1")])"#.to_owned(),
                rejected(
                    "WEB-1",
                    "assignee change",
                    "Jira returned 400 Bad Request: user not found"
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_poll_does_not_overwrite_an_assignee_that_waits_in_the_outbox() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        // A teammate changed the issue after the user did, so Jira's version is newer
        let mut newer = web_1_of_ada();
        newer["fields"]["summary"] = json!("Fix login on Safari");
        newer["fields"]["updated"] = json!("2026-03-30T01:30:00.000+0100");
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": [newer] })))
            .mount(&server)
            .await;

        let config = config(&server.uri());
        *state.config.write().await = Some(config.clone());
        sync_state::set_synced(&state.pool, &["WEB"], 1)
            .await
            .unwrap();
        let mut changed = Vec::new();
        inbound::poll_updated_issues(&state, &config, &["WEB".to_owned()], 1, &mut changed)
            .await
            .unwrap();

        let row = issues::get(&state.pool, &"WEB-1".into()).await;
        let row = row.unwrap().unwrap();
        assert_eq!(row.summary, "Fix login on Safari");
        assert_eq!(row.assignee_name, grace_name());
        assert_eq!(row.sync_status, SyncStatus::Pending);
        assert_eq!(changed, keys(&["WEB-1"]));
    }

    #[tokio::test]
    async fn a_poll_after_the_assignee_was_delivered_brings_what_jira_has() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        // Jira has the assignee under the name of its profile
        let mut newer = web_1_of_ada();
        newer["fields"]["assignee"] =
            json!({ "accountId": "7c20b", "displayName": "Grace B. Hopper" });
        newer["fields"]["updated"] = json!("2026-03-30T01:30:00.000+0100");
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": [newer] })))
            .mount(&server)
            .await;

        let config = config(&server.uri());
        *state.config.write().await = Some(config.clone());
        sync_state::set_synced(&state.pool, &["WEB"], 1)
            .await
            .unwrap();
        let mut changed = Vec::new();
        inbound::poll_updated_issues(&state, &config, &["WEB".to_owned()], 1, &mut changed)
            .await
            .unwrap();

        assert_eq!(
            assignee(&state).await,
            (Some("Grace B. Hopper".to_owned()), SyncStatus::Synced)
        );
    }

    fn tempo_config(state: &AppState, jira: &MockServer, tempo: &MockServer) -> Config {
        *state.tempo_url.lock().unwrap() = tempo.uri();
        Config {
            worklog_target: WorklogTarget::Tempo,
            tempo_api_token: "tempo-secret".to_owned(),
            ..config(&jira.uri())
        }
    }

    /// The profile Jira answers `GET /myself` with: UTC+2 at `STARTED`.
    async fn mount_profile(jira: &MockServer) {
        Mock::given(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accountId": "5b10a",
                "displayName": "Ada",
                "timeZone": "Europe/Paris",
            })))
            .mount(jira)
            .await;
    }

    fn tempo_worklog_call() -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path("/worklogs"))
    }

    /// The part of Tempo's answer to a created worklog that matters here. It
    /// has no id of the Jira worklog Tempo mirrors the worklog into.
    fn tempo_created(tempo_id: i64) -> Value {
        json!({
            "self": format!("https://api.tempo.io/4/worklogs/{tempo_id}"),
            "tempoWorklogId": tempo_id,
            "issue": { "id": 10001 },
            "timeSpentSeconds": 5400,
            "startDate": "2026-03-29",
            "startTime": "08:15:00",
            "startDateTimeUtc": "2026-03-29T08:15:00Z",
        })
    }

    async fn tempo_ids(state: &AppState) -> Vec<Option<i64>> {
        sqlx::query_scalar("SELECT tempo_id FROM worklogs ORDER BY created_at, rowid")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    async fn worklog_requests(server: &MockServer) -> usize {
        let requests = requests(server).await;
        requests
            .iter()
            .filter(|(path, _)| path.contains("worklog"))
            .count()
    }

    #[tokio::test]
    async fn with_tempo_as_the_target_a_worklog_goes_to_tempo_and_not_to_jira() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        tempo_worklog_call()
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer tempo-secret",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(tempo_created(7)))
            .expect(1)
            .mount(&tempo)
            .await;
        api::log_time(&state, &"WEB-1".into(), 5400, STARTED, Some("Pairing"))
            .await
            .unwrap();

        let mut delivered = Vec::new();
        process_queue(&state, &tempo_config(&state, &jira, &tempo), &mut delivered)
            .await
            .unwrap();

        assert_eq!(
            requests(&tempo).await,
            [(
                "/worklogs".to_owned(),
                json!({
                    "issueId": 10001,
                    "authorAccountId": "5b10a",
                    "timeSpentSeconds": 5400,
                    "startDate": "2026-03-29",
                    // 08:15 UTC in the time zone of the profile
                    "startTime": "10:15:00",
                    "description": "Pairing",
                })
            )]
        );
        assert_eq!(worklog_requests(&jira).await, 0);
        let rows = worklog_rows(&state).await;
        assert_eq!(rows[0].0, None, "Jira's id comes with the mirror");
        assert_eq!(rows[0].1, "synced");
        assert_eq!(tempo_ids(&state).await, [Some(7)]);
        assert_eq!(delivered, keys(&["WEB-1"]));
    }

    #[tokio::test]
    async fn a_worklog_goes_to_the_target_set_when_the_queue_is_flushed() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        worklog_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "30001" })))
            .mount(&jira)
            .await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(200).set_body_json(tempo_created(8)))
            .mount(&tempo)
            .await;
        // Queued while the target was Jira
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();

        // The target is Tempo when the queue is flushed
        let mut delivered = Vec::new();
        process_queue(&state, &tempo_config(&state, &jira, &tempo), &mut delivered)
            .await
            .unwrap();
        assert_eq!(worklog_requests(&jira).await, 0);
        assert_eq!(requests(&tempo).await.len(), 1);

        api::log_time(&state, &"WEB-1".into(), 60, STARTED + 7200, None)
            .await
            .unwrap();
        process_queue(&state, &config(&jira.uri()), &mut delivered)
            .await
            .unwrap();
        assert_eq!(worklog_requests(&jira).await, 1);
        assert_eq!(requests(&tempo).await.len(), 1);
    }

    #[tokio::test]
    async fn a_rejected_tempo_token_is_reported_and_the_worklog_stays_queued() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&tempo)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &tempo_config(&state, &jira, &tempo), &mut delivered)
            .await
            .unwrap();

        assert_eq!(received(&mut events), ["TempoTokenRejected"]);
        let status: String = sqlx::query_scalar("SELECT status FROM sync_queue")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(worklog_rows(&state).await[0].1, "pending");
        assert!(delivered.is_empty());
    }

    /// A signed-in core with a worklog queued and a Tempo that answers 401
    async fn with_a_rejected_tempo_token() -> (
        Arc<AppState>,
        UnboundedReceiver<SyncEvent>,
        MockServer,
        MockServer,
        tempfile::TempDir,
    ) {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        api::sign_in(&state, &jira.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        api::save_tempo_token(&state, "tempo-secret").await.unwrap();
        store_issue(&state).await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(401))
            .mount(&tempo)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        received(&mut events);
        (state, events, jira, tempo, data_dir)
    }

    #[tokio::test]
    async fn a_rejected_tempo_token_is_reported_and_tried_once_however_many_cycles_follow() {
        let (state, mut events, jira, tempo, _data_dir) = with_a_rejected_tempo_token().await;
        let config = tempo_config(&state, &jira, &tempo);

        for _ in 0..3 {
            process_queue(&state, &config, &mut Vec::new())
                .await
                .unwrap();
        }

        assert_eq!(received(&mut events), ["TempoTokenRejected"]);
        assert_eq!(requests(&tempo).await.len(), 1);
        assert_eq!(worklog_rows(&state).await[0].1, "pending");
    }

    #[tokio::test]
    async fn saving_a_token_makes_the_next_cycle_try_tempo_again() {
        let (state, mut events, jira, tempo, _data_dir) = with_a_rejected_tempo_token().await;
        let config = tempo_config(&state, &jira, &tempo);
        process_queue(&state, &config, &mut Vec::new())
            .await
            .unwrap();
        received(&mut events);

        // Even the same token is tried again: saving it says it was fixed at Tempo
        api::save_tempo_token(&state, "tempo-secret").await.unwrap();
        process_queue(&state, &config, &mut Vec::new())
            .await
            .unwrap();

        assert_eq!(requests(&tempo).await.len(), 2);
        assert_eq!(received(&mut events), ["TempoTokenRejected"]);

        tempo.reset().await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(200).set_body_json(tempo_created(7)))
            .mount(&tempo)
            .await;
        api::save_tempo_token(&state, "new-secret").await.unwrap();
        let fixed = Config {
            tempo_api_token: "new-secret".to_owned(),
            ..config
        };
        process_queue(&state, &fixed, &mut Vec::new())
            .await
            .unwrap();

        assert_eq!(tempo_ids(&state).await, [Some(7)]);
        assert!(received(&mut events).is_empty());
    }

    #[tokio::test]
    async fn choosing_the_target_makes_the_next_cycle_try_tempo_again() {
        let (state, mut events, jira, tempo, _data_dir) = with_a_rejected_tempo_token().await;
        let config = tempo_config(&state, &jira, &tempo);
        process_queue(&state, &config, &mut Vec::new())
            .await
            .unwrap();
        received(&mut events);

        api::set_worklog_target(&state, WorklogTarget::Tempo)
            .await
            .unwrap();
        process_queue(&state, &config, &mut Vec::new())
            .await
            .unwrap();

        assert_eq!(requests(&tempo).await.len(), 2);
        assert_eq!(received(&mut events), ["TempoTokenRejected"]);
    }

    #[tokio::test]
    async fn a_rejected_tempo_token_does_not_hold_back_what_does_not_need_tempo() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&jira)
            .await;
        comment_call("WEB-1")
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&jira)
            .await;
        assignee_call("WEB-1")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&jira)
            .await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&tempo)
            .await;
        // The worklogs are queued around the others, the first one oldest
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        enqueue(&state, "WEB-1", "21").await;
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();
        api::log_time(&state, &"WEB-1".into(), 1200, STARTED + 7200, None)
            .await
            .unwrap();
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &tempo_config(&state, &jira, &tempo), &mut delivered)
            .await
            .unwrap();

        assert_eq!(delivered, keys(&["WEB-1"]));
        assert_eq!(received(&mut events), ["TempoTokenRejected"]);
        let left: Vec<(String, String)> =
            sqlx::query_as("SELECT entity_type, status FROM sync_queue ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        let pending = |entity_type: &str| (entity_type.to_owned(), "pending".to_owned());
        let synced = |entity_type: &str| (entity_type.to_owned(), "synced".to_owned());
        assert_eq!(
            left,
            [
                pending("worklog"),
                synced("transition"),
                synced("comment"),
                pending("worklog"),
                synced("assignee"),
            ]
        );

        // The token is replaced: both worklogs go out, the older one first
        let fixed = Config {
            tempo_api_token: "new-secret".to_owned(),
            ..tempo_config(&state, &jira, &tempo)
        };
        tempo.reset().await;
        for tempo_id in [7, 8] {
            tempo_worklog_call()
                .respond_with(ResponseTemplate::new(200).set_body_json(tempo_created(tempo_id)))
                .up_to_n_times(1)
                .expect(1)
                .mount(&tempo)
                .await;
        }
        process_queue(&state, &fixed, &mut delivered).await.unwrap();

        let sent: Vec<Value> = requests(&tempo)
            .await
            .iter()
            .map(|(_, body)| body["timeSpentSeconds"].clone())
            .collect();
        assert_eq!(sent, [json!(600), json!(1200)]);
        assert!(received(&mut events).is_empty());
    }

    #[tokio::test]
    async fn a_tempo_token_without_the_worklog_scope_is_reported_and_the_worklog_stays_queued() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(403).set_body_string(
                "This operation requires one of the following scopes: worklogs:manage.",
            ))
            .expect(1)
            .mount(&tempo)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();
        received(&mut events);

        let mut delivered = Vec::new();
        process_queue(&state, &tempo_config(&state, &jira, &tempo), &mut delivered)
            .await
            .unwrap();

        assert_eq!(received(&mut events), ["TempoTokenRejected"]);
        assert_eq!(worklog_rows(&state).await[0].1, "pending");
    }

    #[tokio::test]
    async fn a_403_from_tempo_that_is_not_about_the_token_marks_the_worklog_failed() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(403).set_body_json(
                json!({ "errors": [{ "message": "You do not have permission to log work" }] }),
            ))
            .expect(1)
            .mount(&tempo)
            .await;
        api::log_time(&state, &"WEB-1".into(), 600, STARTED, None)
            .await
            .unwrap();

        let mut delivered = Vec::new();
        process_queue(&state, &tempo_config(&state, &jira, &tempo), &mut delivered)
            .await
            .unwrap();

        assert_eq!(worklog_rows(&state).await[0].1, "failed");
    }

    #[tokio::test]
    async fn a_worklog_posted_to_tempo_is_not_stored_twice_when_the_poll_returns_it() {
        let (jira, tempo) = (MockServer::start().await, MockServer::start().await);
        mount_profile(&jira).await;
        crate::sync::no_sprint_field().mount(&jira).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue(&state).await;
        *state.config.write().await = Some(tempo_config(&state, &jira, &tempo));
        let now = inbound::now_ms().unwrap();
        sync_state::set_synced(&state.pool, &["WEB"], now)
            .await
            .unwrap();
        // The pass for deleted issues would find WEB-1 missing from this Jira
        sync_state::reconciled(&state.pool, now).await.unwrap();
        tempo_worklog_call()
            .respond_with(ResponseTemplate::new(200).set_body_json(tempo_created(7)))
            .expect(1)
            .mount(&tempo)
            .await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": [] })))
            .mount(&jira)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/worklog/updated"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "values": [{ "worklogId": 30001, "updatedTime": 9000 }],
                        "until": 9000, "lastPage": true }),
            ))
            .mount(&jira)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/worklog/deleted"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "values": [], "until": 9000, "lastPage": true })),
            )
            .mount(&jira)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/worklog/list"))
            .and(wiremock::matchers::query_param("expand", "properties"))
            // The mirror as Jira sends it: Tempo's id in a property, and a
            // fixed text in place of the description
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "id": "30001",
                "issueId": "10001",
                "author": { "accountId": "5b10a", "displayName": "Ada" },
                "timeSpentSeconds": 5400,
                "started": "2026-03-29T08:15:00.000+0000",
                "comment": { "type": "doc", "version": 1, "content": [
                    { "type": "paragraph", "content": [{ "type": "text", "text": "time-tracking" }] },
                ] },
                "properties": [{ "key": "tempo", "value": { "shadow": "true", "tempo_id": 7 } }],
            }])))
            .mount(&jira)
            .await;
        api::log_time(&state, &"WEB-1".into(), 5400, STARTED, Some("Pairing"))
            .await
            .unwrap();

        crate::sync::engine::sync_cycle(&state, &mut true, &mut None)
            .await
            .unwrap();

        let rows = worklog_rows(&state).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0.as_deref(), Some("30001"));
        assert_eq!(rows[0].1, "synced");
        let comment: Option<String> = sqlx::query_scalar("SELECT comment FROM worklogs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(comment.as_deref(), Some("Pairing"));
    }

    /// The poll brings WEB-1 as API-9: Jira moved it to another watched project.
    async fn move_web_1_to_api(state: &AppState, server: &MockServer) {
        let mut moved = web_1_of_ada();
        moved["key"] = json!("API-9");
        moved["fields"]["project"]["key"] = json!("API");
        moved["fields"]["updated"] = json!("2026-03-30T10:00:00.000+0100");
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": [moved] })))
            .mount(server)
            .await;
        let config = config(&server.uri());
        *state.config.write().await = Some(config.clone());
        let last_sync_at = inbound::now_ms().unwrap();
        sync_state::set_synced(&state.pool, &["WEB", "API"], last_sync_at)
            .await
            .unwrap();
        let watched = ["WEB".to_owned(), "API".to_owned()];
        inbound::poll_updated_issues(state, &config, &watched, last_sync_at, &mut Vec::new())
            .await
            .unwrap();
    }

    /// The status and the assignee of API-9 and whether it waits for Jira.
    async fn api_9(state: &AppState) -> (String, Option<String>, SyncStatus) {
        let issue = issues::get(&state.pool, &"API-9".into()).await;
        let issue = issue.unwrap().unwrap();
        (issue.status, issue.assignee_name, issue.sync_status)
    }

    #[tokio::test]
    async fn a_transition_queued_before_a_move_is_delivered_under_the_new_key() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        move_web_1_to_api(&state, &server).await;

        // The status the user chose is still shown, on the moved issue
        let chosen = ("In Progress".to_owned(), ada_name(), SyncStatus::Pending);
        assert_eq!(api_9(&state).await, chosen);
        transition("API-9")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        assert_eq!(delivered, keys(&["API-9"]));
        let confirmed = ("In Progress".to_owned(), ada_name(), SyncStatus::Synced);
        assert_eq!(api_9(&state).await, confirmed);
    }

    #[tokio::test]
    async fn an_assignee_queued_before_a_move_is_delivered_under_the_new_key() {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        api::assign_issue(&state, &"WEB-1".into(), Some(&grace()))
            .await
            .unwrap();

        move_web_1_to_api(&state, &server).await;

        // The assignee the user chose is still shown, on the moved issue
        let chosen = ("To Do".to_owned(), grace_name(), SyncStatus::Pending);
        assert_eq!(api_9(&state).await, chosen);
        assignee_call("API-9")
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        assert_eq!(delivered, keys(&["API-9"]));
        let confirmed = ("To Do".to_owned(), grace_name(), SyncStatus::Synced);
        assert_eq!(api_9(&state).await, confirmed);
    }

    #[tokio::test]
    async fn a_comment_queued_before_a_move_is_delivered_under_the_new_key() {
        async fn comments_of_api_9(state: &AppState) -> Vec<(bool, String)> {
            let detail =
                issues::get_detail(&state.pool, &"API-9".into(), "http://localhost", "You").await;
            let comments = detail.unwrap().unwrap().comments;
            comments
                .into_iter()
                .map(|comment| (comment.pending, comment.body_md))
                .collect()
        }
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        api::add_comment(&state, &"WEB-1".into(), "Seen on staging")
            .await
            .unwrap();

        move_web_1_to_api(&state, &server).await;

        // The comment still waits in the moved issue
        let text = "Seen on staging".to_owned();
        assert_eq!(comments_of_api_9(&state).await, [(true, text)]);
        comment_call("API-9")
            .respond_with(ResponseTemplate::new(201).set_body_json(created_comment()))
            .expect(1)
            .mount(&server)
            .await;
        let mut delivered = Vec::new();
        process_queue(&state, &config(&server.uri()), &mut delivered)
            .await
            .unwrap();
        assert_eq!(delivered, keys(&["API-9"]));
        let stored = "Seen on staging\\\nand in production".to_owned();
        assert_eq!(comments_of_api_9(&state).await, [(false, stored)]);
    }

    #[tokio::test]
    async fn a_move_first_seen_by_the_re_read_after_a_refusal_takes_its_changes_and_its_old_key_along()
     {
        let server = MockServer::start().await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, mut events) = start_without_keychain(data_dir.path()).await.unwrap();
        store_issue_of_ada(&state).await;
        sqlx::raw_sql(
            "INSERT INTO worklogs (remote_id, issue_key, time_spent_sec, started_at, sync_status)
                 VALUES ('40001', 'WEB-1', 600, '2026-03-29T08:15:00.000Z', 'synced');
                 INSERT INTO sync_queue (entity_type, issue_key, operation, payload, status)
                 VALUES ('comment', 'WEB-1', 'create', '{}', 'failed');
                 INSERT INTO running_timer (id, issue_key, started_at)
                 VALUES (1, 'WEB-1', 1774771200);",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();
        transition("WEB-1")
            .respond_with(ResponseTemplate::new(400).set_body_string("moved"))
            .mount(&server)
            .await;
        // Jira answers the old key with the issue under its new one
        let mut moved = web_1_of_ada();
        moved["key"] = json!("API-9");
        moved["fields"]["project"]["key"] = json!("API");
        moved["fields"]["updated"] = json!("2026-03-30T10:00:00.000+0100");
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(moved))
            .mount(&server)
            .await;
        received(&mut events);

        process_queue(&state, &config(&server.uri()), &mut Vec::new())
            .await
            .unwrap();

        assert_eq!(
            issues::key_of(&state.pool, "10001").await.unwrap(),
            Some("API-9".into())
        );
        let worklogs: Vec<String> = sqlx::query_scalar("SELECT issue_key FROM worklogs")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(worklogs, ["API-9"]);
        let running = timer::get(&state.pool).await.unwrap().unwrap();
        assert_eq!(running.issue_key, "API-9".into());
        let queued: Vec<String> = sqlx::query_scalar("SELECT issue_key FROM sync_queue")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(queued, ["API-9"]);
        // The key a view may be open under, and the one it follows to
        let events = received(&mut events);
        assert_eq!(events[0], r#"IssuesUpdated([IssueKey("WEB-1")])"#);
        assert!(events[1].starts_with("ChangeRejected"), "{events:?}");
        assert_eq!(events[2], r#"IssuesUpdated([IssueKey("API-9")])"#);
    }
}
