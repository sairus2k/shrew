//! Local changes going out to Jira: the processor of the `sync_queue` outbox.

use crate::{
    AppState, SyncEvent, adf,
    auth::Config,
    db::{
        issues,
        sync_queue::{self, QueueEntry},
        transitions::{self, Position},
    },
    models::IssueKey,
    sync::{
        inbound,
        jira_client::{JiraClient, JiraError},
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
/// token: the entries then stay `pending` for a later pass.
pub(crate) async fn process_queue(
    state: &AppState,
    config: &Config,
    delivered: &mut Vec<IssueKey>,
) -> anyhow::Result<()> {
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    // A retried entry is claimed again: it is still the oldest one
    while let Some(entry) = sync_queue::claim_next(&state.pool).await? {
        let error = match deliver(&client, &entry).await {
            Ok(created) => {
                // One transaction: an issue must not stay pending with nothing left to send,
                // and a comment must not be missing from both the outbox and the cache
                let mut tx = state.pool.begin().await?;
                sync_queue::mark_synced(&mut *tx, entry.id).await?;
                if let Some(comment) = &created {
                    issues::add_comment(&mut tx, &entry.issue_key, comment).await?;
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
                sync_queue::mark_failed(&state.pool, entry.id, &message).await?;
                if entry.entity_type == "transition" {
                    // The entry is failed whether or not the rollback works: with
                    // Jira out of reach the status stays as the user left it
                    let refused = matches!(
                        error.downcast_ref(),
                        Some(JiraError::Status(status, _)) if status.is_client_error()
                    );
                    roll_back(state, &client, &entry.issue_key, refused)
                        .await
                        .ok();
                }
                // A refused comment moves from the pending ones to the refused ones
                let updated = SyncEvent::IssuesUpdated(vec![entry.issue_key.clone()]);
                state.events.send(updated).ok();
                let event = rejection(&entry.issue_key, &entry.entity_type, &message);
                state.events.send(event).ok();
            }
        }
    }
    Ok(())
}

/// What the user is told about an entry that will not reach Jira.
pub(crate) fn rejection(key: &IssueKey, entity_type: &str, reason: &str) -> SyncEvent {
    let action = match entity_type {
        "transition" => "status change",
        other => other,
    };
    SyncEvent::ChangeRejected {
        key: key.clone(),
        action: action.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Reads the issue again and stores it as Jira has it, so that a status the
/// user chose and Jira refused does not stay on screen. Jira has not changed
/// the issue, so its `updated` is no newer than the stored row's.
///
/// With `refused` (Jira answered with a 4xx) the cached menu of the status the
/// issue is in is dropped, because a field or a validator it did not know about
/// may be the reason. That is the status the transition started from, unless a
/// teammate moved the issue first, and then the menu of that status is as good
/// as any to refetch.
async fn roll_back(
    state: &AppState,
    client: &JiraClient,
    key: &IssueKey,
    refused: bool,
) -> anyhow::Result<()> {
    let issue = client.issue(key, inbound::ISSUE_FIELDS).await?;
    let record = inbound::issue_record(&issue)?;
    let mut tx = state.pool.begin().await?;
    issues::mark_stale(&mut *tx, &record.id).await?;
    issues::upsert(&mut *tx, &record).await?;
    if refused && let Some(issue_type) = &record.issue_type {
        let position = Position {
            project_key: record.project_key.clone(),
            issue_type: issue_type.clone(),
            status: record.status.clone(),
        };
        transitions::forget(&mut *tx, &position).await?;
    }
    tx.commit().await?;
    Ok(())
}

// The request is built here, not when the entry is queued: the entry holds
// what the user did, and how that reaches Jira can depend on the settings at
// the time of delivery.
//
// The comment Jira created is returned: the cache holds it until the next poll
// brings the issue, so that it does not vanish in between.
async fn deliver(client: &JiraClient, entry: &QueueEntry) -> anyhow::Result<Option<Value>> {
    let payload: Value = serde_json::from_str(&entry.payload)?;
    match entry.entity_type.as_str() {
        "transition" => {
            client.transition_issue(&entry.issue_key, &payload).await?;
            Ok(None)
        }
        "comment" => {
            let text = payload["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("a comment entry without text"))?;
            let body = json!({ "body": adf::from_plain_text(text) });
            Ok(Some(client.add_comment(&entry.issue_key, &body).await?))
        }
        other => anyhow::bail!("unknown outbox entry type `{other}`"),
    }
}

fn failure(error: &anyhow::Error) -> Failure {
    match error.downcast_ref() {
        Some(JiraError::Unauthorized | JiraError::Http(_)) => Failure::NotAsked,
        Some(JiraError::Status(status, _))
            if status.is_server_error()
                || matches!(
                    *status,
                    StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
                ) =>
        {
            Failure::Transient
        }
        _ => Failure::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api,
        models::{StatusCategory, SyncStatus, Transition},
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
            watched_projects: vec!["WEB".to_owned()],
            last_sync_at: Some(inbound::now_ms().unwrap()),
            pending_projects: Vec::new(),
            last_reconcile_at: 0,
            api_token: TOKEN.to_owned(),
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
        assert_eq!(
            entry(&state, failing).await,
            ("failed".to_owned(), 3, Some(error.to_owned()))
        );
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
        assert_eq!(
            entry(&state, id).await,
            ("failed".to_owned(), 0, Some(error.to_owned()))
        );
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
        let claimed = sync_queue::claim_next(&state.pool).await.unwrap().unwrap();
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
        start_syncing_without_keychain(data_dir).await.unwrap()
    }

    async fn sign_in(state: &AppState, server: &MockServer) {
        *state.config.write().await = Some(config(&server.uri()));
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
        let issue = inbound::issue_record(&json!({
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
        let detail = issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost").await;
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
        let polled = inbound::issue_record(&json!({
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
        let detail = issues::get_detail(&state.pool, &"WEB-1".into(), "http://localhost").await;
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
}
