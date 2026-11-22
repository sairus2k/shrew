//! Reads the transitions of an issue through the transition cache.

use crate::{
    AppState,
    db::{
        issues, sync_state,
        transitions::{self as cache, Position},
    },
    models::{IssueKey, RequiredField, Transition},
    sync::{
        issue_record,
        jira_client::{JiraClient, JiraTransition},
    },
};
use anyhow::Context;
use sqlx::SqlitePool;
use tokio::task::JoinHandle;

/// Returns the transitions and, when the cache entry was stale, the task that
/// is refreshing it.
pub(crate) async fn get(
    state: &AppState,
    key: &IssueKey,
) -> anyhow::Result<(Vec<Transition>, Option<JoinHandle<()>>)> {
    let config = state.config.read().await.clone().context("Sign in first")?;
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);

    // The position is read once, before any request: an optimistic transition
    // made while Jira answers must not file the answer under the new status
    let Some(position) = cache::position(&state.pool, key).await? else {
        // A cache filled by an older version holds issues without their type,
        // so there is nothing to file the transitions under
        let local = issues::get(&state.pool, key)
            .await?
            .with_context(|| format!("{key} is not in the cache"))?
            .status;
        let (jira_status, transitions) = fetch_confirmed(&client, key).await?;
        ensure_same_status(key, &jira_status, &local)?;
        return Ok((transitions, None));
    };
    match cache::get(&state.pool, &position).await? {
        Some(cached) => {
            let refresh = cached.stale.then(|| {
                let (pool, key) = (state.pool.clone(), key.clone());
                tokio::spawn(async move {
                    // Offline or refused: the entry stays and the next open tries again
                    fetch_and_store(&pool, &client, &key, &position).await.ok();
                })
            });
            Ok((cached.transitions, refresh))
        }
        None => {
            let (jira_status, transitions) =
                fetch_and_store(&state.pool, &client, key, &position).await?;
            ensure_same_status(key, &jira_status, &position.status)?;
            Ok((transitions, None))
        }
    }
}

// Each position costs the three requests of a confirmed fetch
const PRELOAD_PER_PASS: i64 = 10;

/// Stores the menus of the statuses that cached issues are in and the cache
/// has no entry for, so that the menu of a status is there before the user
/// moves an issue to it: until the outbox has delivered the move, Jira cannot
/// be asked about that status through the moved issue.
pub(crate) async fn preload(state: &AppState) {
    let Some(config) = state.config.read().await.clone() else {
        return;
    };
    // The initial sync is still filling the cache, with requests of its own
    if !matches!(sync_state::load(&state.pool).await, Ok(sync) if sync.last_sync_at.is_some()) {
        return;
    }
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let Ok(missing) = cache::missing(&state.pool, PRELOAD_PER_PASS).await else {
        return;
    };
    for (key, position) in missing {
        // Offline, a rejected token or a Jira in trouble would fail the other
        // questions too, so the rest waits for the next pass
        if fetch_and_store(&state.pool, &client, &key, &position)
            .await
            .is_err()
        {
            return;
        }
    }
}

fn ensure_same_status(key: &IssueKey, jira_status: &str, local: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        jira_status == local,
        "{key} is in {jira_status} in Jira but {local} here: its transitions are not known yet"
    );
    Ok(())
}

/// The transitions with the status Jira reported them for.
async fn fetch_confirmed(
    client: &JiraClient,
    key: &IssueKey,
) -> anyhow::Result<(String, Vec<Transition>)> {
    // The transitions endpoint does not name the status it answers for, so the
    // status is read on both sides of it: a move in between is not confirmed
    let before = client.status(key).await?;
    let transitions = fetch(client, key).await?;
    let after = client.status(key).await?;
    anyhow::ensure!(before == after, "{key} changed status while it was read");
    Ok((before, transitions))
}

/// Files the answer under the status Jira reported, which is not always the
/// local one. Returns that status with the transitions.
async fn fetch_and_store(
    pool: &SqlitePool,
    client: &JiraClient,
    key: &IssueKey,
    position: &Position,
) -> anyhow::Result<(String, Vec<Transition>)> {
    let (before, transitions) = fetch_confirmed(client, key).await?;
    let position = Position {
        status: before.clone(),
        ..position.clone()
    };
    cache::put(pool, &position, &transitions).await?;
    Ok((before, transitions))
}

async fn fetch(client: &JiraClient, key: &IssueKey) -> anyhow::Result<Vec<Transition>> {
    let transitions = client.transitions(key).await?;
    Ok(transitions.into_iter().map(from_jira).collect())
}

fn from_jira(transition: JiraTransition) -> Transition {
    Transition {
        id: transition.id,
        name: transition.name,
        to_status: transition.to.name,
        to_status_category: issue_record::status_category(&transition.to.status_category.key),
        required_fields: transition
            .fields
            .into_iter()
            // Jira fills in a field that has a default by itself
            .filter(|(_, field)| field.required && !field.has_default_value)
            .map(|(id, field)| RequiredField {
                id,
                name: field.name,
                schema: field.schema,
                allowed_values: field.allowed_values,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api, models::StatusCategory, start_syncing_without_keychain, start_without_keychain,
        sync::inbound,
    };
    use serde_json::{Value, json};
    use std::{sync::Arc, time::Duration};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, path_regex, query_param},
    };

    fn issue(id: u32, issue_type: &str, status: &str) -> Value {
        json!({
            "id": id.to_string(),
            "key": format!("WEB-{id}"),
            "fields": {
                "summary": "Fix login",
                "status": { "name": status, "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "issuetype": { "name": issue_type },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    /// A core that is signed in to `server` and holds `issues` in its cache.
    async fn core_with(
        server: &MockServer,
        issues: &[Value],
    ) -> (tempfile::TempDir, Arc<AppState>) {
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" })),
            )
            .mount(server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in_with(&state, server, issues).await;
        (data_dir, state)
    }

    async fn sign_in_with(state: &AppState, server: &MockServer, issues: &[Value]) {
        api::sign_in(state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        for issue in issues {
            let record = issue_record::issue_record(issue).unwrap();
            crate::db::issues::upsert(&state.pool, &record)
                .await
                .unwrap();
        }
    }

    /// Jira's status for `key`; without it Jira would answer nothing about the issue.
    fn in_jira(key: &str, status: &str) -> Mock {
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/issue/{key}")))
            .and(query_param("fields", "status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "key": key,
                "fields": { "status": { "name": status } },
            })))
    }

    /// Jira's answer for `key`: one transition per name, none with a screen.
    fn offers(key: &str, names: &[&str]) -> Mock {
        let transitions: Vec<Value> = names
            .iter()
            .map(|name| {
                json!({
                    "id": format!("id-{name}"),
                    "name": name,
                    "to": { "name": name, "statusCategory": { "key": "indeterminate" } },
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/rest/api/3/issue/{key}/transitions")))
            .and(query_param("expand", "transitions.fields"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "transitions": transitions })),
            )
    }

    async fn names(state: &AppState, key: &str) -> Vec<String> {
        let transitions = api::get_transitions(state, &key.into()).await.unwrap();
        transitions.into_iter().map(|t| t.name).collect()
    }

    async fn cached_entries(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM transition_cache")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_miss_asks_jira_once_and_a_hit_works_offline() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start", "Close"])
            .expect(1)
            .mount(&server)
            .await;

        let fetched = api::get_transitions(&state, &"WEB-1".into()).await.unwrap();

        assert_eq!(
            fetched[0],
            Transition {
                id: "id-Start".to_owned(),
                name: "Start".to_owned(),
                to_status: "Start".to_owned(),
                to_status_category: StatusCategory::InProgress,
                required_fields: Vec::new(),
            }
        );
        assert_eq!(fetched.len(), 2);

        // Dropping the server checks the one expected request and takes Jira away
        drop(server);
        let cached = api::get_transitions(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(cached, fetched);
    }

    #[tokio::test]
    async fn a_miss_without_jira_is_an_error_and_stores_nothing() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;

        api::get_transitions(&state, &"WEB-1".into())
            .await
            .unwrap_err();

        assert_eq!(cached_entries(&state).await, 0);
    }

    #[tokio::test]
    async fn issues_at_the_same_position_share_one_entry() {
        let server = MockServer::start().await;
        let issues = [
            issue(1, "Bug", "To Do"),
            issue(2, "Bug", "To Do"),
            issue(3, "Bug", "In Review"),
            issue(4, "Story", "To Do"),
        ];
        let (_data_dir, state) = core_with(&server, &issues).await;
        // WEB-2 has no answer in Jira: it can only be served by the entry of WEB-1
        in_jira("WEB-1", "To Do").mount(&server).await;
        in_jira("WEB-3", "In Review").mount(&server).await;
        in_jira("WEB-4", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).expect(1).mount(&server).await;
        offers("WEB-3", &["Approve"]).mount(&server).await;
        offers("WEB-4", &["Refine"]).mount(&server).await;

        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
        assert_eq!(names(&state, "WEB-2").await, ["Start"]);
        assert_eq!(cached_entries(&state).await, 1);

        // Another status or another type is another position
        assert_eq!(names(&state, "WEB-3").await, ["Approve"]);
        assert_eq!(names(&state, "WEB-4").await, ["Refine"]);
        assert_eq!(cached_entries(&state).await, 3);
    }

    #[tokio::test]
    async fn a_stale_entry_is_returned_at_once_and_refreshed_in_the_background() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"])
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
        // The workflow changes in Jira, and the entry ages
        offers("WEB-1", &["Begin"]).expect(1).mount(&server).await;
        sqlx::query("UPDATE transition_cache SET fetched_at = datetime('now', '-61 minutes')")
            .execute(&state.pool)
            .await
            .unwrap();

        let (stale, refresh) = get(&state, &"WEB-1".into()).await.unwrap();

        // Jira already answers "Begin": the old entry did not wait for it
        assert_eq!(stale[0].name, "Start");
        refresh.expect("a stale entry is refreshed").await.unwrap();
        let (fresh, refresh) = get(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(fresh[0].name, "Begin");
        assert!(refresh.is_none(), "a fresh entry is not fetched again");
    }

    #[tokio::test]
    async fn a_stale_entry_survives_a_refresh_that_fails() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"])
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
        sqlx::query("UPDATE transition_cache SET fetched_at = datetime('now', '-61 minutes')")
            .execute(&state.pool)
            .await
            .unwrap();

        let (_, refresh) = get(&state, &"WEB-1".into()).await.unwrap();
        refresh.expect("a stale entry is refreshed").await.unwrap();

        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
    }

    async fn forget_type(state: &AppState, key: &str) {
        sqlx::query("UPDATE issues SET issue_type = NULL WHERE key = ?")
            .bind(key)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_issue_without_its_type_is_served_when_jira_agrees_and_nothing_is_stored() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        forget_type(&state, "WEB-1").await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;

        assert_eq!(names(&state, "WEB-1").await, ["Start"]);

        assert_eq!(cached_entries(&state).await, 0);
    }

    #[tokio::test]
    async fn an_issue_without_its_type_is_not_served_the_transitions_of_another_status() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        forget_type(&state, "WEB-1").await;
        in_jira("WEB-1", "In Review").mount(&server).await;
        offers("WEB-1", &["Approve"]).mount(&server).await;

        api::get_transitions(&state, &"WEB-1".into())
            .await
            .unwrap_err();

        assert_eq!(cached_entries(&state).await, 0);
    }

    async fn move_locally(state: &AppState, key: &str, to_status: &str) {
        sqlx::query("UPDATE issues SET status = ? WHERE key = ?")
            .bind(to_status)
            .bind(key)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn stored_under(state: &AppState) -> Vec<String> {
        sqlx::query_scalar("SELECT from_status FROM transition_cache ORDER BY from_status")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_miss_after_an_optimistic_transition_is_not_filed_under_the_local_status() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        // The outbox has not sent the move to "In Review" yet
        move_locally(&state, "WEB-1", "In Review").await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;

        api::get_transitions(&state, &"WEB-1".into())
            .await
            .unwrap_err();

        assert_eq!(stored_under(&state).await, ["To Do"]);
    }

    #[tokio::test]
    async fn a_miss_after_a_teammate_moved_the_issue_serves_the_right_status_later() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "In Review").mount(&server).await;
        offers("WEB-1", &["Approve"]).expect(1).mount(&server).await;

        api::get_transitions(&state, &"WEB-1".into())
            .await
            .unwrap_err();
        assert_eq!(stored_under(&state).await, ["In Review"]);

        // The poll stores the teammate's move; the entry stored above serves it
        move_locally(&state, "WEB-1", "In Review").await;
        assert_eq!(names(&state, "WEB-1").await, ["Approve"]);
    }

    #[tokio::test]
    async fn a_status_that_changes_during_the_fetch_stores_nothing() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do")
            .up_to_n_times(1)
            .mount(&server)
            .await;
        in_jira("WEB-1", "In Review").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;

        api::get_transitions(&state, &"WEB-1".into())
            .await
            .unwrap_err();

        assert_eq!(cached_entries(&state).await, 0);
    }

    #[tokio::test]
    async fn a_refresh_after_a_teammate_moved_the_issue_leaves_the_local_entry_alone() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do")
            .up_to_n_times(2)
            .mount(&server)
            .await;
        offers("WEB-1", &["Start"])
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
        sqlx::query("UPDATE transition_cache SET fetched_at = datetime('now', '-61 minutes')")
            .execute(&state.pool)
            .await
            .unwrap();
        in_jira("WEB-1", "In Review").mount(&server).await;
        offers("WEB-1", &["Approve"]).mount(&server).await;

        let (_, refresh) = get(&state, &"WEB-1".into()).await.unwrap();
        refresh.expect("a stale entry is refreshed").await.unwrap();

        assert_eq!(stored_under(&state).await, ["In Review", "To Do"]);
        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
    }

    #[tokio::test]
    async fn only_the_fields_the_user_must_fill_in_are_kept() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "In Review")]).await;
        in_jira("WEB-1", "In Review").mount(&server).await;
        let resolutions =
            json!([{ "id": "10000", "name": "Fixed" }, { "id": "10001", "name": "Won't Do" }]);
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .and(query_param("expand", "transitions.fields"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "transitions": [{
                    "id": "31",
                    "name": "Close",
                    "to": { "name": "Done", "statusCategory": { "key": "done" } },
                    "fields": {
                        "resolution": {
                            "required": true,
                            "hasDefaultValue": false,
                            "name": "Resolution",
                            "schema": { "type": "resolution", "system": "resolution" },
                            "allowedValues": resolutions,
                        },
                        "customfield_10050": {
                            "required": true,
                            "hasDefaultValue": false,
                            "name": "Root cause",
                            "schema": { "type": "string" },
                        },
                        "priority": {
                            "required": true,
                            "hasDefaultValue": true,
                            "name": "Priority",
                            "schema": { "type": "priority" },
                            "allowedValues": [{ "id": "3", "name": "Medium" }],
                        },
                        "assignee": {
                            "required": false,
                            "hasDefaultValue": false,
                            "name": "Assignee",
                            "schema": { "type": "user" },
                        },
                    },
                }],
            })))
            .mount(&server)
            .await;

        let fetched = api::get_transitions(&state, &"WEB-1".into()).await.unwrap();
        let cached = api::get_transitions(&state, &"WEB-1".into()).await.unwrap();

        let expected = Transition {
            id: "31".to_owned(),
            name: "Close".to_owned(),
            to_status: "Done".to_owned(),
            to_status_category: StatusCategory::Done,
            required_fields: vec![
                RequiredField {
                    id: "customfield_10050".to_owned(),
                    name: "Root cause".to_owned(),
                    schema: json!({ "type": "string" }),
                    allowed_values: None,
                },
                RequiredField {
                    id: "resolution".to_owned(),
                    name: "Resolution".to_owned(),
                    schema: json!({ "type": "resolution", "system": "resolution" }),
                    allowed_values: Some(resolutions.as_array().unwrap().clone()),
                },
            ],
        };
        assert_eq!(fetched, [expected]);
        // The entry read back from SQLite is the one that was fetched
        assert_eq!(cached, fetched);
    }

    async fn finish_initial_sync(state: &AppState) {
        sync_state::finish_initial_sync(&state.pool, inbound::now_ms().unwrap())
            .await
            .unwrap();
    }

    /// `core_with` after the initial sync, which is when the preload starts.
    async fn synced_core_with(
        server: &MockServer,
        issues: &[Value],
    ) -> (tempfile::TempDir, Arc<AppState>) {
        let (data_dir, state) = core_with(server, issues).await;
        finish_initial_sync(&state).await;
        (data_dir, state)
    }

    /// How many times Jira was asked what a status offers.
    async fn menus_asked_for(server: &MockServer) -> usize {
        let requests = server.received_requests().await.unwrap();
        requests
            .iter()
            .filter(|request| {
                request.method.as_str() == "GET" && request.url.path().ends_with("/transitions")
            })
            .count()
    }

    #[tokio::test]
    async fn a_preload_stores_the_menu_of_every_status_a_cached_issue_is_in() {
        let server = MockServer::start().await;
        let issues = [
            issue(1, "Bug", "To Do"),
            issue(2, "Bug", "In Review"),
            issue(3, "Story", "To Do"),
        ];
        let (_data_dir, state) = synced_core_with(&server, &issues).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        in_jira("WEB-2", "In Review").mount(&server).await;
        in_jira("WEB-3", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;
        offers("WEB-2", &["Approve"]).mount(&server).await;
        offers("WEB-3", &["Refine"]).mount(&server).await;

        preload(&state).await;

        // Nobody opened a menu, and Jira is gone: the menus are there
        drop(server);
        assert_eq!(names(&state, "WEB-1").await, ["Start"]);
        assert_eq!(names(&state, "WEB-2").await, ["Approve"]);
        assert_eq!(names(&state, "WEB-3").await, ["Refine"]);
        // WEB-1 moved to the status of WEB-2 offline has the menu of that status
        move_locally(&state, "WEB-1", "In Review").await;
        assert_eq!(names(&state, "WEB-1").await, ["Approve"]);
    }

    #[tokio::test]
    async fn a_preload_asks_once_about_a_position_and_not_about_one_in_the_cache() {
        let server = MockServer::start().await;
        let issues = [
            issue(1, "Bug", "To Do"),
            issue(2, "Bug", "To Do"),
            issue(3, "Bug", "In Review"),
        ];
        let (_data_dir, state) = synced_core_with(&server, &issues).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        in_jira("WEB-2", "To Do").mount(&server).await;
        in_jira("WEB-3", "In Review").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;
        offers("WEB-2", &["Start"]).mount(&server).await;
        offers("WEB-3", &["Approve"]).mount(&server).await;
        assert_eq!(names(&state, "WEB-3").await, ["Approve"]);
        assert_eq!(menus_asked_for(&server).await, 1);

        preload(&state).await;
        assert_eq!(menus_asked_for(&server).await, 2);
        assert_eq!(stored_under(&state).await, ["In Review", "To Do"]);

        // Nothing is missing any more
        preload(&state).await;
        assert_eq!(menus_asked_for(&server).await, 2);
    }

    #[tokio::test]
    async fn a_preload_files_the_menu_under_the_status_jira_has() {
        let server = MockServer::start().await;
        let (_data_dir, state) = synced_core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        // A teammate moved the issue, and the poll has not stored it yet
        in_jira("WEB-1", "In Review").mount(&server).await;
        offers("WEB-1", &["Approve"]).mount(&server).await;

        preload(&state).await;

        assert_eq!(stored_under(&state).await, ["In Review"]);
    }

    #[tokio::test]
    async fn a_preload_does_not_ask_through_an_issue_whose_change_waits_in_the_outbox() {
        let server = MockServer::start().await;
        let (_data_dir, state) = synced_core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;
        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        preload(&state).await;

        assert_eq!(menus_asked_for(&server).await, 0);
        assert_eq!(cached_entries(&state).await, 0);
    }

    #[tokio::test]
    async fn a_preload_asks_about_ten_positions_and_leaves_the_rest_to_the_next_one() {
        let server = MockServer::start().await;
        let issues: Vec<Value> = (1..=11)
            .map(|id| issue(id, "Bug", &format!("Step {id}")))
            .collect();
        let (_data_dir, state) = synced_core_with(&server, &issues).await;
        for id in 1..=11 {
            let key = format!("WEB-{id}");
            in_jira(&key, &format!("Step {id}")).mount(&server).await;
            offers(&key, &["Next"]).mount(&server).await;
        }

        preload(&state).await;
        assert_eq!(cached_entries(&state).await, 10);

        preload(&state).await;
        assert_eq!(cached_entries(&state).await, 11);
    }

    #[tokio::test]
    async fn a_preload_ends_at_the_first_question_jira_does_not_answer() {
        let server = MockServer::start().await;
        let issues = [issue(1, "Bug", "To Do"), issue(2, "Bug", "In Review")];
        let (_data_dir, state) = synced_core_with(&server, &issues).await;
        Mock::given(method("GET"))
            .and(path_regex("^/rest/api/3/issue/WEB-[12]$"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;

        preload(&state).await;

        assert_eq!(cached_entries(&state).await, 0);
    }

    #[tokio::test]
    async fn nothing_is_preloaded_before_the_initial_sync_has_finished() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;

        preload(&state).await;

        assert_eq!(menus_asked_for(&server).await, 0);
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

    /// A core like `synced_core_with` whose sync engine is running. Every
    /// search of Jira finds `issues`, so that a cycle changes nothing.
    async fn syncing_core_with(
        server: &MockServer,
        issues: &[Value],
    ) -> (tempfile::TempDir, Arc<AppState>) {
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" })),
            )
            .mount(server)
            .await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": issues })))
            .mount(server)
            .await;
        crate::sync::no_worklog_changes().mount(server).await;
        crate::sync::no_sprint_field().mount(server).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_syncing_without_keychain(data_dir.path())
            .await
            .unwrap();
        sign_in_with(&state, server, issues).await;
        finish_initial_sync(&state).await;
        state.sync_waker.send(()).unwrap();
        (data_dir, state)
    }

    /// Waits for what the engine does on its own task.
    async fn wait_until<F: Future<Output = bool>>(what: &str, mut reached: impl FnMut() -> F) {
        let waiting = async {
            while !reached().await {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap_or_else(|_| panic!("{what}"));
    }

    #[tokio::test]
    async fn the_engine_preloads_the_menus_after_a_sync_cycle() {
        let server = MockServer::start().await;
        in_jira("WEB-1", "To Do").mount(&server).await;
        offers("WEB-1", &["Start"]).mount(&server).await;

        let (_data_dir, state) = syncing_core_with(&server, &[issue(1, "Bug", "To Do")]).await;

        wait_until("the menu of To Do was not preloaded", || async {
            stored_under(&state).await == ["To Do"]
        })
        .await;
    }

    #[tokio::test]
    async fn the_menu_of_the_new_status_is_stored_once_jira_accepts_a_transition() {
        let server = MockServer::start().await;
        // The first preload reads the status twice; the transition comes after it
        in_jira("WEB-1", "To Do")
            .up_to_n_times(2)
            .mount(&server)
            .await;
        offers("WEB-1", &["Start"])
            .up_to_n_times(1)
            .mount(&server)
            .await;
        in_jira("WEB-1", "In Progress").mount(&server).await;
        offers("WEB-1", &["Review"]).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let (_data_dir, state) = syncing_core_with(&server, &[issue(1, "Bug", "To Do")]).await;
        wait_until("the menu of To Do was not preloaded", || async {
            stored_under(&state).await == ["To Do"]
        })
        .await;

        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        // No other issue is in In Progress, and nobody opens the menu
        wait_until("the menu of In Progress was not stored", || async {
            stored_under(&state).await == ["In Progress", "To Do"]
        })
        .await;
        drop(server);
        assert_eq!(names(&state, "WEB-1").await, ["Review"]);
    }

    /// Jira never answers what status WEB-2 is in, so a preload never ends.
    async fn preload_that_hangs(server: &MockServer) -> (tempfile::TempDir, Arc<AppState>) {
        in_jira("WEB-1", "To Do").mount(server).await;
        offers("WEB-1", &["Start"]).mount(server).await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-2"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3600)))
            .mount(server)
            .await;
        let issues = [issue(1, "Bug", "To Do"), issue(2, "Bug", "In Review")];
        let (data_dir, state) = syncing_core_with(server, &issues).await;
        wait_until("the preload did not start", || async {
            let requests = server.received_requests().await.unwrap();
            requests
                .iter()
                .any(|request| request.url.path() == "/rest/api/3/issue/WEB-2")
        })
        .await;
        (data_dir, state)
    }

    #[tokio::test]
    async fn a_change_made_during_a_preload_is_sent_without_waiting_for_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let (_data_dir, state) = preload_that_hangs(&server).await;

        api::transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap();

        wait_until("the transition waited for the preload", || async {
            let requests = server.received_requests().await.unwrap();
            requests
                .iter()
                .any(|request| request.method.as_str() == "POST")
        })
        .await;
    }

    #[tokio::test]
    async fn a_sign_out_stops_a_preload_that_waits_for_jira() {
        let server = MockServer::start().await;
        let (_data_dir, state) = preload_that_hangs(&server).await;

        tokio::time::timeout(Duration::from_secs(5), api::sign_out(&state))
            .await
            .expect("the sign-out waited for the preload")
            .unwrap();

        assert_eq!(cached_entries(&state).await, 0);
    }
}
