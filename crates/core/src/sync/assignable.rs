//! Reads the people an issue can be assigned to through the assignee cache.

use crate::{
    AppState,
    db::assignable as cache,
    models::{IssueKey, User},
    sync::jira_client::JiraClient,
};
use anyhow::Context;
use sqlx::SqlitePool;
use tokio::task::JoinHandle;

/// Returns the people and, when the cache entry was stale, the task that is
/// refreshing it.
pub(crate) async fn get(
    state: &AppState,
    key: &IssueKey,
) -> anyhow::Result<(Vec<User>, Option<JoinHandle<()>>)> {
    let config = state.config.read().await.clone().context("Sign in first")?;
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);

    match cache::get(&state.pool, key).await? {
        Some(cached) => {
            let refresh = cached.stale.then(|| {
                let (pool, key) = (state.pool.clone(), key.clone());
                tokio::spawn(async move {
                    // Offline or refused: the entry stays and the next open tries again
                    fetch_and_store(&pool, &client, &key).await.ok();
                })
            });
            Ok((cached.users, refresh))
        }
        None => Ok((fetch_and_store(&state.pool, &client, key).await?, None)),
    }
}

async fn fetch_and_store(
    pool: &SqlitePool,
    client: &JiraClient,
    key: &IssueKey,
) -> anyhow::Result<Vec<User>> {
    let users: Vec<User> = client
        .assignable_users(key)
        .await?
        .into_iter()
        .map(|user| User {
            account_id: user.account_id,
            display_name: user.display_name,
        })
        .collect();
    cache::put(pool, key, &users).await?;
    Ok(users)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api, start_without_keychain, sync::issue_record};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    fn issue(id: u32) -> Value {
        json!({
            "id": id.to_string(),
            "key": format!("WEB-{id}"),
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    /// A core that is signed in to `server` and holds WEB-1 and WEB-2 in its cache.
    async fn core_with(server: &MockServer) -> (tempfile::TempDir, Arc<AppState>) {
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
        for id in [1, 2] {
            let record = issue_record::issue_record(&issue(id)).unwrap();
            crate::db::issues::upsert(&state.pool, &record)
                .await
                .unwrap();
        }
        (data_dir, state)
    }

    /// Jira's answer for `key`: one person per name.
    fn lists(key: &str, names: &[&str]) -> Mock {
        let users: Vec<Value> = names
            .iter()
            .map(|name| json!({ "accountId": format!("id-{name}"), "displayName": name }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/rest/api/3/user/assignable/search"))
            .and(query_param("issueKey", key))
            .respond_with(ResponseTemplate::new(200).set_body_json(users))
    }

    async fn names(state: &AppState, key: &str) -> Vec<String> {
        let users = api::assignable_users(state, &key.into()).await.unwrap();
        users.into_iter().map(|user| user.display_name).collect()
    }

    async fn cached_keys(state: &AppState) -> Vec<String> {
        sqlx::query_scalar("SELECT issue_key FROM assignable_users ORDER BY issue_key")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_miss_asks_jira_once_and_a_hit_works_offline() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;
        lists("WEB-1", &["Ada", "Grace"])
            .expect(1)
            .mount(&server)
            .await;

        let fetched = api::assignable_users(&state, &"WEB-1".into())
            .await
            .unwrap();

        assert_eq!(
            fetched[1],
            User {
                account_id: "id-Grace".to_owned(),
                display_name: "Grace".to_owned()
            }
        );
        // Dropping the server checks the one expected request and takes Jira away
        drop(server);
        assert_eq!(names(&state, "WEB-1").await, ["Ada", "Grace"]);
    }

    #[tokio::test]
    async fn an_answer_is_filed_under_the_issue_it_was_asked_for() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;
        lists("WEB-1", &["Ada"]).mount(&server).await;
        lists("WEB-2", &["Grace"]).mount(&server).await;

        assert_eq!(names(&state, "WEB-1").await, ["Ada"]);
        assert_eq!(names(&state, "WEB-2").await, ["Grace"]);

        assert_eq!(names(&state, "WEB-1").await, ["Ada"]);
        assert_eq!(cached_keys(&state).await, ["WEB-1", "WEB-2"]);
    }

    #[tokio::test]
    async fn a_miss_without_jira_is_an_error_and_stores_nothing() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;

        api::assignable_users(&state, &"WEB-1".into())
            .await
            .unwrap_err();

        assert_eq!(cached_keys(&state).await, [] as [&str; 0]);
    }

    #[tokio::test]
    async fn an_issue_the_cache_does_not_have_is_not_asked_about() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;
        lists("WEB-3", &["Ada"]).expect(0).mount(&server).await;

        let error = api::assignable_users(&state, &"WEB-3".into())
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "issue WEB-3 not found");
        assert_eq!(cached_keys(&state).await, [] as [&str; 0]);
    }

    async fn age_the_entries(state: &AppState) {
        sqlx::query("UPDATE assignable_users SET fetched_at = datetime('now', '-61 minutes')")
            .execute(&state.pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_stale_entry_is_returned_at_once_and_refreshed_in_the_background() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;
        lists("WEB-1", &["Ada"])
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert_eq!(names(&state, "WEB-1").await, ["Ada"]);
        // A teammate joins the project, and the entry ages
        lists("WEB-1", &["Ada", "Grace"])
            .expect(1)
            .mount(&server)
            .await;
        age_the_entries(&state).await;

        let (stale, refresh) = get(&state, &"WEB-1".into()).await.unwrap();

        assert_eq!(stale.len(), 1);
        refresh.expect("a stale entry is refreshed").await.unwrap();
        let (fresh, refresh) = get(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(fresh.len(), 2);
        assert!(refresh.is_none(), "a fresh entry is not fetched again");
    }

    #[tokio::test]
    async fn a_stale_entry_survives_a_refresh_that_fails() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;
        lists("WEB-1", &["Ada"])
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert_eq!(names(&state, "WEB-1").await, ["Ada"]);
        age_the_entries(&state).await;

        let (_, refresh) = get(&state, &"WEB-1".into()).await.unwrap();
        refresh.expect("a stale entry is refreshed").await.unwrap();

        assert_eq!(names(&state, "WEB-1").await, ["Ada"]);
    }

    #[tokio::test]
    async fn signing_out_forgets_the_people() {
        let server = MockServer::start().await;
        let (_data_dir, state) = core_with(&server).await;
        lists("WEB-1", &["Ada"]).mount(&server).await;
        names(&state, "WEB-1").await;

        api::sign_out(&state).await.unwrap();

        assert_eq!(cached_keys(&state).await, [] as [&str; 0]);
    }
}
