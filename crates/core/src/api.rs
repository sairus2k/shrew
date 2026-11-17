//! The async functions the UI calls. They need the tokio runtime.

use crate::{
    AppState, SyncEvent, auth, db,
    models::{Account, IssueDetail, IssueKey, IssueRow, Project, SetupStep, Transition},
    sync::{
        inbound,
        jira_client::{JiraClient, JiraError},
        transitions,
    },
};
use anyhow::{Context, bail};
use serde_json::json;

/// The account the app is signed in to, or `None` when it has no credentials.
pub async fn account(state: &AppState) -> Option<Account> {
    let config = state.config.read().await;
    config.as_ref().map(|config| Account {
        site_url: config.site_url.clone(),
        email: config.email.clone(),
        account_id: config.account_id.clone(),
    })
}

/// Checks the credentials with Jira and saves them only if Jira accepts them.
/// The error says what went wrong in words meant for the user.
pub async fn sign_in(
    state: &AppState,
    site_url: &str,
    email: &str,
    api_token: &str,
) -> anyhow::Result<Account> {
    let (site_url, email, api_token) = (site_url.trim(), email.trim(), api_token.trim());
    if site_url.is_empty() || email.is_empty() || api_token.is_empty() {
        bail!("Enter the site URL, the email and the API token");
    }
    let site_url = site_url.trim_end_matches('/');
    let site_url = if site_url.contains("://") {
        site_url.to_owned()
    } else {
        format!("https://{site_url}")
    };

    let myself = match JiraClient::new(&site_url, email, api_token).myself().await {
        Ok(myself) => myself,
        Err(JiraError::Unauthorized) => bail!("Jira rejected the email or the API token"),
        Err(JiraError::Status(status, _)) => {
            bail!("{site_url} answered {status}. Check the site URL")
        }
        Err(JiraError::Http(error)) => bail!("Could not reach {site_url}: {error}"),
    };

    let config = auth::Config {
        site_url,
        email: email.to_owned(),
        account_id: myself.account_id,
        api_token: api_token.to_owned(),
        watched_projects: Vec::new(),
        last_sync_at: None,
        pending_projects: Vec::new(),
        last_reconcile_at: 0,
    };
    // What a sign-out that failed halfway, or a config deleted by hand, left behind
    db::clear(&state.pool).await?;
    auth::save(&state.data_dir, state.tokens.clone(), &config).await?;
    *state.config.write().await = Some(config);
    Ok(account(state).await.expect("the config was just set"))
}

/// How many local changes Jira has not received yet. A sign-out discards them.
pub async fn unsent_changes(state: &AppState) -> anyhow::Result<i64> {
    Ok(db::sync_queue::unsent(&state.pool).await?)
}

/// Forgets the account: deletes the saved credentials and everything cached
/// from Jira, unsent changes included. `setup_step` is `SignIn` afterwards.
pub async fn sign_out(state: &AppState) -> anyhow::Result<()> {
    state.signed_out.send_replace(());
    let _no_sync = state.sync_running.lock().await;
    let mut config = state.config.write().await;
    auth::remove(&state.data_dir, state.tokens.clone()).await?;
    *config = None;
    db::clear(&state.pool).await?;
    Ok(())
}

/// Replaces the API token after `SyncEvent::TokenRejected`. The token is
/// checked with Jira and saved only if Jira accepts it; the sync then resumes.
/// The error says what went wrong in words meant for the user.
pub async fn renew_token(state: &AppState, api_token: &str) -> anyhow::Result<()> {
    let api_token = api_token.trim();
    if api_token.is_empty() {
        bail!("Enter the API token");
    }
    let config = state.config.read().await.clone().context("Sign in first")?;
    let site_url = &config.site_url;
    match JiraClient::new(site_url, &config.email, api_token)
        .myself()
        .await
    {
        Ok(_) => {}
        Err(JiraError::Unauthorized) => bail!("Jira rejected the API token"),
        Err(JiraError::Status(status, _)) => bail!("{site_url} answered {status}"),
        Err(JiraError::Http(error)) => bail!("Could not reach {site_url}: {error}"),
    }

    let mut saved = state.config.write().await;
    // Only the token is replaced: a sync may have moved its times meanwhile
    let current = saved.as_mut().context("Sign in first")?;
    current.api_token = api_token.to_owned();
    auth::save(&state.data_dir, state.tokens.clone(), current).await?;
    drop(saved);
    state.sync_waker.send(()).ok();
    Ok(())
}

/// When the last sync that completed had started, in Unix milliseconds.
/// `None` until the initial sync has finished.
pub async fn last_sync_at(state: &AppState) -> Option<i64> {
    state.config.read().await.as_ref()?.last_sync_at
}

pub async fn setup_step(state: &AppState) -> SetupStep {
    match &*state.config.read().await {
        None => SetupStep::SignIn,
        Some(config) if config.watched_projects.is_empty() => SetupStep::ChooseProjects,
        Some(config) if config.last_sync_at.is_none() || !config.pending_projects.is_empty() => {
            SetupStep::InitialSync
        }
        Some(_) => SetupStep::Ready,
    }
}

/// The projects the account can see, ordered by name.
pub async fn projects(state: &AppState) -> anyhow::Result<Vec<Project>> {
    let config = state.config.read().await.clone().context("Sign in first")?;
    let projects = JiraClient::new(&config.site_url, &config.email, &config.api_token)
        .projects()
        .await?;
    Ok(projects
        .into_iter()
        .map(|project| Project {
            key: project.key,
            name: project.name,
        })
        .collect())
}

/// Saves the projects whose issues the app keeps locally. After the initial
/// sync the added projects wait for `initial_sync` to download them, and the
/// cached issues of the removed ones are deleted.
pub async fn watch_projects(state: &AppState, project_keys: &[String]) -> anyhow::Result<()> {
    if project_keys.is_empty() {
        bail!("Choose at least one project");
    }
    let mut saved = state.config.write().await;
    let mut config = saved.clone().context("Sign in first")?;
    if config.last_sync_at.is_some() {
        let added = project_keys
            .iter()
            .filter(|key| !config.watched_projects.contains(key));
        config
            .pending_projects
            .retain(|key| project_keys.contains(key));
        for key in added {
            if !config.pending_projects.contains(key) {
                config.pending_projects.push(key.clone());
            }
        }
    }
    config.watched_projects = project_keys.to_vec();
    auth::save_config(&state.data_dir, &config).await?;
    *saved = Some(config);

    // Under the config lock, as the pass for deleted issues is
    let mut removed = Vec::new();
    let mut tx = state.pool.begin().await?;
    for issue in db::issues::cached(&mut *tx).await? {
        if !project_keys.contains(&issue.project_key) {
            db::issues::delete(&mut *tx, &issue.id).await?;
            removed.push(issue.key);
        }
    }
    tx.commit().await?;
    if !removed.is_empty() {
        state.events.send(SyncEvent::IssuesUpdated(removed)).ok();
    }
    Ok(())
}

/// Downloads every issue of the watched projects, reporting through
/// `SyncEvent::InitialSyncProgress` and `SyncEvent::InitialSyncComplete`.
/// After an error or a quit, calling it again completes the sync.
pub async fn initial_sync(state: &AppState) -> anyhow::Result<()> {
    inbound::initial_sync(state).await
}

/// The projects whose issues the app keeps locally, in the order they were chosen.
pub async fn watched_projects(state: &AppState) -> Vec<String> {
    match &*state.config.read().await {
        Some(config) => config.watched_projects.clone(),
        None => Vec::new(),
    }
}

/// Every issue in the local cache, or only those of `project`, the one Jira
/// changed last first.
pub async fn list_issues(state: &AppState, project: Option<&str>) -> anyhow::Result<Vec<IssueRow>> {
    Ok(db::issues::list(&state.pool, project).await?)
}

/// One issue from the local cache, with its description and comments as Markdown.
pub async fn get_issue(state: &AppState, key: &IssueKey) -> anyhow::Result<IssueDetail> {
    let site_url = match &*state.config.read().await {
        Some(config) => config.site_url.clone(),
        None => bail!("Sign in first"),
    };
    db::issues::get_detail(&state.pool, key, &site_url)
        .await?
        .with_context(|| format!("issue {key} not found"))
}

/// The transitions the issue offers from its current status. They come from
/// the local cache; Jira is asked only for a status of a workflow that was
/// never opened before.
pub async fn get_transitions(state: &AppState, key: &IssueKey) -> anyhow::Result<Vec<Transition>> {
    let (transitions, _refresh) = transitions::get(state, key).await?;
    Ok(transitions)
}

/// Moves the issue to the status `transition` leads to, locally and at once:
/// Jira gets the change from the outbox. `fields` holds the values of the
/// transition's required fields, by field id.
pub async fn transition_issue(
    state: &AppState,
    key: &IssueKey,
    transition: &Transition,
    fields: &serde_json::Value,
) -> anyhow::Result<()> {
    let payload = json!({ "transition": { "id": transition.id }, "fields": fields });
    // One transaction: a crash must not leave a status Jira will never hear
    // of, or an outbox entry for a status the user does not see
    let mut tx = state.pool.begin().await?;
    let found = db::issues::set_pending_status(
        &mut *tx,
        key,
        &transition.to_status,
        transition.to_status_category,
    )
    .await?;
    if !found {
        bail!("issue {key} not found");
    }
    db::sync_queue::enqueue(&mut *tx, "transition", key, &payload.to_string()).await?;
    tx.commit().await?;

    state
        .events
        .send(SyncEvent::IssuesUpdated(vec![key.clone()]))
        .ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Queues a plain-text comment on the issue. It shows in the issue as pending
/// at once, and Jira gets it from the outbox. The text is trimmed; a comment
/// without any is refused.
pub async fn add_comment(state: &AppState, key: &IssueKey, text: &str) -> anyhow::Result<()> {
    let text = text.trim();
    if text.is_empty() {
        bail!("a comment cannot be empty");
    }
    let payload = json!({ "text": text });
    db::sync_queue::enqueue(&state.pool, "comment", key, &payload.to_string()).await?;

    state
        .events
        .send(SyncEvent::IssuesUpdated(vec![key.clone()]))
        .ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Queues a comment Jira refused again, as it was written. `entry_id` comes
/// from `Comment::rejected`.
pub async fn retry_comment(state: &AppState, entry_id: i64) -> anyhow::Result<()> {
    let key = db::sync_queue::retry_comment(&state.pool, entry_id)
        .await?
        .with_context(|| format!("entry {entry_id} is not a refused comment"))?;
    state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
    state.sync_waker.send(()).ok();
    Ok(())
}

/// Throws away a comment Jira refused. `entry_id` comes from `Comment::rejected`.
pub async fn discard_comment(state: &AppState, entry_id: i64) -> anyhow::Result<()> {
    let key = db::sync_queue::discard_comment(&state.pool, entry_id)
        .await?
        .with_context(|| format!("entry {entry_id} is not a refused comment"))?;
    state.events.send(SyncEvent::IssuesUpdated(vec![key])).ok();
    Ok(())
}

/// The 50 best matches of `query` in the local cache, best first. Every word
/// must match a key, a summary or a description, as a word, a stem or a prefix.
pub async fn search_issues(
    state: &AppState,
    query: &str,
    project: Option<&str>,
) -> anyhow::Result<Vec<IssueRow>> {
    Ok(db::issues::search(&state.pool, query, project, 50).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Comment, StatusCategory, SyncStatus},
        start_without_keychain,
    };
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{basic_auth, method, path},
    };

    async fn jira(myself: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(myself)
            .mount(&server)
            .await;
        server
    }

    fn myself() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" }))
    }

    fn nothing_is_saved(data_dir: &std::path::Path, state: &AppState) {
        assert!(!data_dir.join("config.json").exists());
        assert_eq!(state.tokens.load().unwrap(), None);
    }

    #[tokio::test]
    async fn a_fresh_install_has_no_account() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        assert_eq!(account(&state).await, None);
    }

    #[tokio::test]
    async fn sign_in_saves_the_account_and_keeps_the_token_out_of_the_config() {
        let server = MockServer::start().await;
        // Only the credentials the user typed get an answer
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .and(basic_auth("ada@example.com", "secret-token"))
            .respond_with(myself())
            .mount(&server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let typed_url = format!(" {}/ ", server.uri());
        let signed_in = sign_in(&state, &typed_url, "ada@example.com", "secret-token")
            .await
            .unwrap();

        let expected = Account {
            site_url: server.uri(),
            email: "ada@example.com".to_owned(),
            account_id: "5b10a".to_owned(),
        };
        assert_eq!(signed_in, expected);
        assert_eq!(account(&state).await, Some(expected));

        let config = std::fs::read_to_string(data_dir.path().join("config.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(
            config,
            json!({
                "site_url": server.uri(),
                "email": "ada@example.com",
                "account_id": "5b10a",
                "watched_projects": [],
                "last_sync_at": null,
                "pending_projects": [],
                "last_reconcile_at": 0,
            })
        );
        assert_eq!(
            state.tokens.load().unwrap().as_deref(),
            Some("secret-token")
        );
    }

    #[tokio::test]
    async fn a_restart_uses_the_saved_credentials() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let signed_in = sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        assert_eq!(account(&state).await, Some(signed_in));
        let config = state.config.read().await;
        assert_eq!(config.as_ref().unwrap().api_token, "secret-token");
    }

    fn project_list() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "isLast": true,
            "values": [
                { "id": "10001", "key": "API", "name": "Backend" },
                { "id": "10000", "key": "WEB", "name": "Website" },
            ],
        }))
    }

    #[tokio::test]
    async fn the_watched_projects_are_chosen_from_the_accessible_ones_and_saved() {
        let server = jira(myself()).await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/project/search"))
            .and(basic_auth("ada@example.com", "secret-token"))
            .respond_with(project_list())
            .mount(&server)
            .await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await, SetupStep::SignIn);
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        assert_eq!(setup_step(&state).await, SetupStep::ChooseProjects);

        let accessible = projects(&state).await.unwrap();
        assert_eq!(
            accessible,
            [
                Project {
                    key: "API".to_owned(),
                    name: "Backend".to_owned()
                },
                Project {
                    key: "WEB".to_owned(),
                    name: "Website".to_owned()
                },
            ]
        );
        assert!(watched_projects(&state).await.is_empty());
        watch_projects(&state, &["WEB".to_owned()]).await.unwrap();

        assert_eq!(watched_projects(&state).await, ["WEB"]);
        assert_eq!(setup_step(&state).await, SetupStep::InitialSync);
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await, SetupStep::InitialSync);
        let config = state.config.read().await;
        assert_eq!(config.as_ref().unwrap().watched_projects, ["WEB"]);
    }

    #[tokio::test]
    async fn watching_no_project_is_refused() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        let error = watch_projects(&state, &[]).await.unwrap_err();

        assert_eq!(error.to_string(), "Choose at least one project");
        assert_eq!(setup_step(&state).await, SetupStep::ChooseProjects);
    }

    #[tokio::test]
    async fn a_config_saved_before_project_selection_existed_still_loads() {
        let data_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            data_dir.path().join("config.json"),
            r#"{ "site_url": "https://x.atlassian.net", "email": "ada@example.com", "account_id": "5b10a" }"#,
        )
        .unwrap();
        std::fs::write(data_dir.path().join("api-token"), "secret-token").unwrap();

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        assert_eq!(setup_step(&state).await, SetupStep::ChooseProjects);
    }

    fn paragraph(text: &str) -> serde_json::Value {
        json!({
            "type": "doc",
            "version": 1,
            "content": [{ "type": "paragraph", "content": [{ "type": "text", "text": text }] }],
        })
    }

    /// A signed-in core whose cache holds `issue`, an element of a search page.
    async fn core_with(issue: serde_json::Value) -> (tempfile::TempDir, std::sync::Arc<AppState>) {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        let record = inbound::issue_record(&issue).unwrap();
        db::issues::upsert(&state.pool, &record).await.unwrap();
        (data_dir, state)
    }

    #[tokio::test]
    async fn an_issue_is_read_with_its_fields_description_and_comments() {
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "description": paragraph("Login *fails*"),
                "status": { "name": "In Review", "statusCategory": { "key": "indeterminate" } },
                "assignee": { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                "reporter": { "accountId": "7c20b", "displayName": "Grace Hopper" },
                "priority": { "name": "High" },
                "labels": ["auth", "regression"],
                "comment": {
                    "total": 3,
                    "comments": [
                        {
                            "author": { "accountId": "7c20b", "displayName": "Grace Hopper" },
                            "created": "2026-03-28T09:15:00.000+0100",
                            "body": paragraph("Seen on staging"),
                        },
                        {
                            "author": { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                            "created": "2026-03-29T01:30:00.000+0100",
                            "body": paragraph("Fixed"),
                        },
                    ],
                },
                "project": { "key": "WEB" },
                "issuetype": { "name": "Bug" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;
        let site_url = account(&state).await.unwrap().site_url;

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();

        let comment = |author: &str, created: &str, body: &str| Comment {
            author_name: author.to_owned(),
            created: created.to_owned(),
            body_md: body.to_owned(),
            pending: false,
            rejected: None,
        };
        assert_eq!(
            issue,
            IssueDetail {
                key: "WEB-1".into(),
                summary: "Fix login".to_owned(),
                status: "In Review".to_owned(),
                status_category: StatusCategory::InProgress,
                sync_status: SyncStatus::Synced,
                issue_type: Some("Bug".to_owned()),
                assignee_name: Some("Ada Lovelace".to_owned()),
                reporter_name: Some("Grace Hopper".to_owned()),
                priority: Some("High".to_owned()),
                labels: vec!["auth".to_owned(), "regression".to_owned()],
                description_md: "Login \\*fails\\*".to_owned(),
                comments: vec![
                    comment(
                        "Grace Hopper",
                        "2026-03-28T09:15:00.000+0100",
                        "Seen on staging"
                    ),
                    comment("Ada Lovelace", "2026-03-29T01:30:00.000+0100", "Fixed"),
                ],
                comment_total: 3,
                url: format!("{site_url}/browse/WEB-1"),
            }
        );
    }

    #[tokio::test]
    async fn an_issue_without_the_optional_fields_is_read_with_them_empty() {
        // What a cache filled before the detail view existed holds, too
        let (_data_dir, state) = core_with(json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "description": null,
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "assignee": null,
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        }))
        .await;

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();

        assert_eq!(issue.issue_type, None);
        assert_eq!(issue.assignee_name, None);
        assert_eq!(issue.reporter_name, None);
        assert_eq!(issue.priority, None);
        assert_eq!(issue.labels, [] as [&str; 0]);
        assert_eq!(issue.description_md, "");
        assert_eq!(issue.comments, []);
        assert_eq!(issue.comment_total, 0);

        let error = get_issue(&state, &"WEB-2".into()).await.unwrap_err();
        assert_eq!(error.to_string(), "issue WEB-2 not found");
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

    fn todo_issue() -> serde_json::Value {
        json!({
            "id": "10001",
            "key": "WEB-1",
            "fields": {
                "summary": "Fix login",
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "project": { "key": "WEB" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    /// The type and the payload of every outbox entry that waits for Jira.
    async fn queued(state: &AppState) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT entity_type, payload FROM sync_queue WHERE status = 'pending' ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_transition_changes_the_status_at_once_and_queues_it_for_jira() {
        let (_data_dir, state) = core_with(todo_issue()).await;
        let fields = json!({ "resolution": { "id": "10000" } });

        transition_issue(&state, &"WEB-1".into(), &start(), &fields)
            .await
            .unwrap();

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.status, "In Progress");
        assert_eq!(issue.status_category, StatusCategory::InProgress);
        assert_eq!(issue.sync_status, SyncStatus::Pending);
        let queued = queued(&state).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "transition");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].1).unwrap(),
            json!({ "transition": { "id": "21" }, "fields": fields })
        );
    }

    #[tokio::test]
    async fn a_transition_that_cannot_be_queued_leaves_the_issue_as_it_was() {
        let (_data_dir, state) = core_with(todo_issue()).await;
        sqlx::query(
            "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'the outbox is broken'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let error = transition_issue(&state, &"WEB-1".into(), &start(), &json!({}))
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("the outbox is broken"),
            "{error}"
        );
        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(issue.status, "To Do");
        assert_eq!(issue.sync_status, SyncStatus::Synced);
    }

    #[tokio::test]
    async fn a_transition_of_an_issue_the_cache_does_not_have_queues_nothing() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        let error = transition_issue(&state, &"WEB-2".into(), &start(), &json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "issue WEB-2 not found");
        assert_eq!(queued(&state).await, []);
    }

    #[tokio::test]
    async fn a_comment_shows_as_pending_at_once_and_is_queued_for_jira() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        add_comment(&state, &"WEB-1".into(), "  Seen on staging\nand *prod*\n ")
            .await
            .unwrap();

        let issue = get_issue(&state, &"WEB-1".into()).await.unwrap();
        assert_eq!(
            issue.comments,
            [Comment {
                author_name: "You".to_owned(),
                created: String::new(),
                body_md: "Seen on staging\\\nand \\*prod\\*".to_owned(),
                pending: true,
                rejected: None,
            }]
        );
        assert_eq!(issue.comment_total, 1);
        // The issue itself has no change of its own to wait for
        assert_eq!(issue.sync_status, SyncStatus::Synced);
        let queued = queued(&state).await;
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0, "comment");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].1).unwrap(),
            json!({ "text": "Seen on staging\nand *prod*" })
        );
    }

    #[tokio::test]
    async fn an_empty_comment_is_refused_and_queues_nothing() {
        let (_data_dir, state) = core_with(todo_issue()).await;

        for text in ["", " \n\t "] {
            let error = add_comment(&state, &"WEB-1".into(), text)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "a comment cannot be empty");
        }

        assert_eq!(queued(&state).await, []);
    }

    #[tokio::test]
    async fn signing_out_forgets_the_account_and_a_restart_asks_to_sign_in() {
        let (data_dir, state) = core_with(todo_issue()).await;
        let fields = json!({});
        transition_issue(&state, &"WEB-1".into(), &start(), &fields)
            .await
            .unwrap();
        let position = db::transitions::Position {
            project_key: "WEB".to_owned(),
            issue_type: "Task".to_owned(),
            status: "To Do".to_owned(),
        };
        db::transitions::put(&state.pool, &position, &[start()])
            .await
            .unwrap();
        assert_eq!(unsent_changes(&state).await.unwrap(), 1);

        sign_out(&state).await.unwrap();

        nothing_is_saved(data_dir.path(), &state);
        assert_eq!(account(&state).await, None);
        assert_eq!(setup_step(&state).await, SetupStep::SignIn);
        assert!(list_issues(&state, None).await.unwrap().is_empty());
        assert!(
            search_issues(&state, "login", None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(unsent_changes(&state).await.unwrap(), 0);
        let cached = db::transitions::get(&state.pool, &position).await.unwrap();
        assert!(cached.is_none());
        state.pool.close().await;

        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(setup_step(&state).await, SetupStep::SignIn);
    }

    #[tokio::test]
    async fn the_same_account_signs_in_again_without_a_restart() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        let first = sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();
        sign_out(&state).await.unwrap();

        let again = sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        assert_eq!(again, first);
        assert_eq!(account(&state).await, Some(first));
        assert_eq!(setup_step(&state).await, SetupStep::ChooseProjects);
    }

    #[tokio::test]
    async fn another_account_does_not_get_the_cache_of_the_previous_one() {
        let (_data_dir, state) = core_with(todo_issue()).await;
        sign_out(&state).await.unwrap();
        let other = ResponseTemplate::new(200)
            .set_body_json(json!({ "accountId": "7c20b", "displayName": "Grace" }));
        let server = jira(other).await;

        let signed_in = sign_in(&state, &server.uri(), "grace@example.com", "her-token")
            .await
            .unwrap();

        assert_eq!(signed_in.account_id, "7c20b");
        assert_eq!(setup_step(&state).await, SetupStep::ChooseProjects);
        assert!(list_issues(&state, None).await.unwrap().is_empty());
        assert!(
            search_issues(&state, "login", None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(state.tokens.load().unwrap().as_deref(), Some("her-token"));
    }

    #[tokio::test]
    async fn signing_in_starts_from_an_empty_cache() {
        // The user deleted the config by hand, as they had to before sign-out existed
        let (data_dir, state) = core_with(todo_issue()).await;
        std::fs::remove_file(data_dir.path().join("config.json")).unwrap();
        state.pool.close().await;
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();
        assert_eq!(list_issues(&state, None).await.unwrap().len(), 1);
        let server = jira(myself()).await;

        sign_in(&state, &server.uri(), "ada@example.com", "secret-token")
            .await
            .unwrap();

        assert!(list_issues(&state, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejected_credentials_are_explained_and_not_saved() {
        let server = jira(ResponseTemplate::new(401)).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let error = sign_in(&state, &server.uri(), "ada@example.com", "wrong")
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Jira rejected the email or the API token"
        );
        assert_eq!(account(&state).await, None);
        nothing_is_saved(data_dir.path(), &state);
    }

    #[tokio::test]
    async fn a_wrong_site_is_explained_and_not_saved() {
        let not_jira = jira(ResponseTemplate::new(404)).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let error = sign_in(&state, &not_jira.uri(), "ada@example.com", "token")
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} answered 404 Not Found. Check the site URL",
                not_jira.uri()
            )
        );

        // Nothing listens on port 1
        let error = sign_in(&state, "http://127.0.0.1:1", "ada@example.com", "token")
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Could not reach http://127.0.0.1:1: "),
            "{error}"
        );

        nothing_is_saved(data_dir.path(), &state);
    }

    #[tokio::test]
    async fn an_empty_field_is_refused_without_a_request() {
        let server = jira(myself()).await;
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        let error = sign_in(&state, &server.uri(), "ada@example.com", "  ")
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Enter the site URL, the email and the API token"
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_site_typed_without_a_scheme_gets_https() {
        let data_dir = tempfile::tempdir().unwrap();
        let (state, _events) = start_without_keychain(data_dir.path()).await.unwrap();

        // Port 1 refuses the connection; the error names the URL that was tried
        let error = sign_in(&state, "127.0.0.1:1/", "ada@example.com", "token")
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("Could not reach https://127.0.0.1:1: "),
            "{error}"
        );
    }
}
