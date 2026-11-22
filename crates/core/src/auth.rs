use crate::{db::sync_state::SyncState, models::WorklogTarget};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

const CONFIG_FILE: &str = "config.json";
const NEW_CONFIG_FILE: &str = "config.json.new";
const KEYCHAIN_SERVICE: &str = "dev.shrew.app";
const KEYCHAIN_ACCOUNT: &str = "jira-api-token";
const TEMPO_KEYCHAIN_ACCOUNT: &str = "tempo-api-token";
pub(crate) const TEMPO_URL: &str = "https://api.tempo.io/4";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Config {
    pub site_url: String, // https://{site}.atlassian.net
    pub email: String,
    pub account_id: String, // from GET /myself
    // Defaults, so a config written before it existed still loads
    #[serde(default)]
    pub worklog_target: WorklogTarget,
    #[serde(default = "shown")]
    pub shortcut_hints: bool,
    // Lives in the token store, never in the config file
    #[serde(skip)]
    pub api_token: String,
    // Same; empty when the user has not saved a Tempo token
    #[serde(skip)]
    pub tempo_api_token: String,
}

fn shown() -> bool {
    true
}

/// Where the API token is kept. The calls block: run them off the async threads.
pub(crate) trait TokenStore: Send + Sync {
    fn load(&self) -> anyhow::Result<Option<String>>;
    fn save(&self, token: &str) -> anyhow::Result<()>;
    /// Succeeds when there is no token to delete.
    fn delete(&self) -> anyhow::Result<()>;
}

/// One item of the macOS Keychain: the service name and the account name.
pub(crate) struct Keychain(pub &'static str, pub &'static str);

impl Keychain {
    pub(crate) fn of_the_app() -> Self {
        Self(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
    }

    pub(crate) fn tempo_of_the_app() -> Self {
        Self(KEYCHAIN_SERVICE, TEMPO_KEYCHAIN_ACCOUNT)
    }

    fn entry(&self) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(self.0, self.1)
    }
}

impl TokenStore for Keychain {
    fn load(&self) -> anyhow::Result<Option<String>> {
        match self.entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, token: &str) -> anyhow::Result<()> {
        Ok(self.entry()?.set_password(token)?)
    }

    fn delete(&self) -> anyhow::Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// A plain file. Tests use it so that they never touch the user's Keychain.
#[cfg(any(test, feature = "test-support"))]
pub(crate) struct TokenFile(pub std::path::PathBuf);

#[cfg(any(test, feature = "test-support"))]
impl TokenStore for TokenFile {
    fn load(&self) -> anyhow::Result<Option<String>> {
        match std::fs::read_to_string(&self.0) {
            Ok(token) => Ok(Some(token)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, token: &str) -> anyhow::Result<()> {
        Ok(std::fs::write(&self.0, token)?)
    }

    fn delete(&self) -> anyhow::Result<()> {
        match std::fs::remove_file(&self.0) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}

/// The saved account, or `None` when the user has to sign in. `api_token` is
/// empty when the store does not give the token.
// A missing token and a Keychain that refuses access read the same. Neither
// makes the account unknown: its cache and its unsent changes are still on
// disk, and the user is asked for the token alone.
pub(crate) async fn load(
    data_dir: &Path,
    tokens: Arc<dyn TokenStore>,
    tempo_tokens: Arc<dyn TokenStore>,
) -> Option<Config> {
    // The config is read first, so a fresh install never queries the Keychain
    let json = tokio::fs::read(data_dir.join(CONFIG_FILE)).await.ok()?;
    let config: Config = serde_json::from_slice(&json).ok()?;
    let api_token = tokio::task::spawn_blocking(move || tokens.load())
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .unwrap_or_default();
    // A Tempo token that cannot be read is the same as none: Tempo then
    // cannot be the target, and the user saves the token again
    let tempo_api_token = tokio::task::spawn_blocking(move || tempo_tokens.load())
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .unwrap_or_default();
    Some(Config {
        api_token,
        tempo_api_token,
        ..config
    })
}

/// The sync state of a config written before the database held it, or `None`
/// for a config without it. `db::sync_state::import` takes it over, and the
/// next write of the config drops it from the file.
pub(crate) async fn legacy_sync_state(data_dir: &Path) -> Option<SyncState> {
    let json = tokio::fs::read(data_dir.join(CONFIG_FILE)).await.ok()?;
    let config: serde_json::Value = serde_json::from_slice(&json).ok()?;
    // Such a config always has the list, empty before the projects were chosen
    config.get("watched_projects")?;
    serde_json::from_value(config).ok()
}

pub(crate) async fn save(
    data_dir: &Path,
    tokens: Arc<dyn TokenStore>,
    config: &Config,
) -> anyhow::Result<()> {
    let api_token = config.api_token.clone();
    tokio::task::spawn_blocking(move || tokens.save(&api_token)).await??;
    save_config(data_dir, config).await
}

/// Deletes the tokens and the config file, with what a write of it that
/// stopped halfway left. Succeeds when they are already gone.
pub(crate) async fn remove(
    data_dir: &Path,
    tokens: Arc<dyn TokenStore>,
    tempo_tokens: Arc<dyn TokenStore>,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || tokens.delete()).await??;
    tokio::task::spawn_blocking(move || tempo_tokens.delete()).await??;
    for file in [CONFIG_FILE, NEW_CONFIG_FILE] {
        match tokio::fs::remove_file(data_dir.join(file)).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Writes the config file only: the token store is not touched.
// Through a second file and a rename: a crash or a power loss at any moment
// leaves the complete old config or the complete new one, never a part
pub(crate) async fn save_config(data_dir: &Path, config: &Config) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(config)?;
    let data_dir = data_dir.to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let new_file = data_dir.join(NEW_CONFIG_FILE);
        let mut file = std::fs::File::create(&new_file)?;
        file.write_all(&json)?;
        // The content has to be on the disk before the name points at it
        file.sync_all()?;
        std::fs::rename(&new_file, data_dir.join(CONFIG_FILE))?;
        // The rename is a change of the directory
        std::fs::File::open(&data_dir)?.sync_all()
    })
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_keeps_the_token_until_it_is_deleted() {
        // A service of its own, so the token of the real app is never touched
        let keychain = Keychain("dev.shrew.app.test", KEYCHAIN_ACCOUNT);
        keychain.entry().unwrap().delete_credential().ok();
        assert_eq!(keychain.load().unwrap(), None);

        keychain.save("first").unwrap();
        keychain.save("second").unwrap();
        assert_eq!(keychain.load().unwrap().as_deref(), Some("second"));

        keychain.delete().unwrap();
        assert_eq!(keychain.load().unwrap(), None);
        // There is nothing left to delete
        keychain.delete().unwrap();
    }

    fn account_config() -> Config {
        serde_json::from_value(serde_json::json!({
            "site_url": "https://acme.atlassian.net",
            "email": "ada@example.com",
            "account_id": "5b10a",
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_config_write_that_stopped_halfway_leaves_the_config_written_before() {
        let data_dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(TokenFile(data_dir.path().join("api-token")));
        let tempo_tokens = Arc::new(TokenFile(data_dir.path().join("tempo-token")));
        let mut config = account_config();
        config.api_token = "secret".to_owned();
        save(data_dir.path(), tokens.clone(), &config)
            .await
            .unwrap();
        // The new content is written beside the config and takes its name
        // only when it is complete: a crash before that leaves this
        let new_file = data_dir.path().join(NEW_CONFIG_FILE);
        assert!(!new_file.exists());
        std::fs::write(&new_file, r#"{ "site_url": "https://ac"#).unwrap();

        let loaded = load(data_dir.path(), tokens.clone(), tempo_tokens.clone()).await;
        assert_eq!(loaded.as_ref(), Some(&config));

        // The next write is not stopped by what the crash left
        config.worklog_target = WorklogTarget::Tempo;
        save_config(data_dir.path(), &config).await.unwrap();
        let loaded = load(data_dir.path(), tokens, tempo_tokens).await;
        assert_eq!(loaded, Some(config));
        assert!(!new_file.exists());
    }

    #[tokio::test]
    async fn removing_the_account_leaves_no_file_that_names_it() {
        let data_dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(TokenFile(data_dir.path().join("api-token")));
        let tempo_tokens = Arc::new(TokenFile(data_dir.path().join("tempo-token")));
        let mut config = account_config();
        config.api_token = "secret".to_owned();
        save(data_dir.path(), tokens.clone(), &config)
            .await
            .unwrap();
        // A write of the config that stopped halfway
        let new_file = data_dir.path().join(NEW_CONFIG_FILE);
        std::fs::write(&new_file, r#"{ "email": "ada@example.com""#).unwrap();

        remove(data_dir.path(), tokens.clone(), tempo_tokens.clone())
            .await
            .unwrap();

        assert_eq!(std::fs::read_dir(data_dir.path()).unwrap().count(), 0);
        // There is nothing left to delete
        remove(data_dir.path(), tokens, tempo_tokens).await.unwrap();
    }

    #[tokio::test]
    async fn a_config_without_its_token_still_names_the_account() {
        let data_dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(TokenFile(data_dir.path().join("api-token")));
        let tempo_tokens = Arc::new(TokenFile(data_dir.path().join("tempo-token")));
        save_config(data_dir.path(), &account_config())
            .await
            .unwrap();

        let loaded = load(data_dir.path(), tokens, tempo_tokens).await;

        assert_eq!(loaded, Some(account_config()));
    }

    #[tokio::test]
    async fn a_tempo_url_left_by_an_earlier_build_is_neither_used_nor_written_back() {
        let data_dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(TokenFile(data_dir.path().join("api-token")));
        tokens.save("secret").unwrap();
        let old = serde_json::json!({
            "site_url": "https://acme.atlassian.net",
            "email": "ada@example.com",
            "account_id": "5b10a",
            "tempo_url": "https://elsewhere.example/4",
        });
        std::fs::write(data_dir.path().join(CONFIG_FILE), old.to_string()).unwrap();
        let tempo_tokens = Arc::new(TokenFile(data_dir.path().join("tempo-token")));

        let config = load(data_dir.path(), tokens, tempo_tokens)
            .await
            .expect("an old config still loads");
        save_config(data_dir.path(), &config).await.unwrap();

        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data_dir.path().join(CONFIG_FILE)).unwrap())
                .unwrap();
        assert!(saved.get("tempo_url").is_none());
        assert_eq!(
            crate::sync::outbound::tempo_url(
                &crate::start_without_keychain(data_dir.path())
                    .await
                    .unwrap()
                    .0
            ),
            TEMPO_URL
        );
    }
}
