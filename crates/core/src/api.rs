//! The async functions the UI calls. They need the tokio runtime.

use crate::{
    AppState, auth, db,
    models::{Account, IssueDetail, IssueKey, IssueRow, Project, SetupStep},
    sync::{
        inbound,
        jira_client::{JiraClient, JiraError},
    },
};
use anyhow::{Context, bail};

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
        last_reconcile_at: 0,
    };
    auth::save(&state.data_dir, state.tokens.clone(), &config).await?;
    *state.config.write().await = Some(config);
    Ok(account(state).await.expect("the config was just set"))
}

pub async fn setup_step(state: &AppState) -> SetupStep {
    match &*state.config.read().await {
        None => SetupStep::SignIn,
        Some(config) if config.watched_projects.is_empty() => SetupStep::ChooseProjects,
        Some(config) if config.last_sync_at.is_none() => SetupStep::InitialSync,
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

/// Saves the projects whose issues the app keeps locally.
pub async fn watch_projects(state: &AppState, project_keys: &[String]) -> anyhow::Result<()> {
    if project_keys.is_empty() {
        bail!("Choose at least one project");
    }
    let mut saved = state.config.write().await;
    let mut config = saved.clone().context("Sign in first")?;
    config.watched_projects = project_keys.to_vec();
    auth::save_config(&state.data_dir, &config).await?;
    *saved = Some(config);
    Ok(())
}

/// Downloads every issue of the watched projects, reporting through
/// `SyncEvent::InitialSyncProgress` and `SyncEvent::InitialSyncComplete`.
/// After an error or a quit, calling it again completes the sync.
pub async fn initial_sync(state: &AppState) -> anyhow::Result<()> {
    inbound::initial_sync(state).await
}

/// Every issue in the local cache, the one Jira changed last first.
pub async fn list_issues(state: &AppState) -> anyhow::Result<Vec<IssueRow>> {
    Ok(db::issues::list(&state.pool).await?)
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
        models::{Comment, StatusCategory},
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
        watch_projects(&state, &["WEB".to_owned()]).await.unwrap();

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
        };
        assert_eq!(
            issue,
            IssueDetail {
                key: "WEB-1".into(),
                summary: "Fix login".to_owned(),
                status: "In Review".to_owned(),
                status_category: StatusCategory::InProgress,
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
