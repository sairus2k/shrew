//! The async functions the UI calls. They need the tokio runtime.

use crate::{
    AppState, auth,
    models::Account,
    sync::jira_client::{JiraClient, JiraError},
};
use anyhow::bail;

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
    };
    auth::save(&state.data_dir, state.tokens.clone(), &config).await?;
    *state.config.write().await = Some(config);
    Ok(account(state).await.expect("the config was just set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start_without_keychain;
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
