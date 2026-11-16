//! Reads the transitions of an issue through the transition cache.

use crate::{
    AppState,
    db::transitions::{self as cache, Position},
    models::{IssueKey, RequiredField, Transition},
    sync::{
        inbound,
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
        return Ok((fetch(&client, key).await?, None));
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
            let transitions = fetch_and_store(&state.pool, &client, key, &position).await?;
            Ok((transitions, None))
        }
    }
}

async fn fetch_and_store(
    pool: &SqlitePool,
    client: &JiraClient,
    key: &IssueKey,
    position: &Position,
) -> anyhow::Result<Vec<Transition>> {
    let transitions = fetch(client, key).await?;
    cache::put(pool, position, &transitions).await?;
    Ok(transitions)
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
        to_status_category: inbound::status_category(&transition.to.status_category.key),
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
    use crate::{api, models::StatusCategory, start_without_keychain};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
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
        api::sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        for issue in issues {
            let record = inbound::issue_record(issue).unwrap();
            crate::db::issues::upsert(&state.pool, &record)
                .await
                .unwrap();
        }
        (data_dir, state)
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

    #[tokio::test]
    async fn only_the_fields_the_user_must_fill_in_are_kept() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server, &[issue(1, "Bug", "In Review")]).await;
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
}
