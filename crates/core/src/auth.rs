use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

const CONFIG_FILE: &str = "config.json";
const KEYCHAIN_SERVICE: &str = "dev.shrew.app";
const KEYCHAIN_ACCOUNT: &str = "jira-api-token";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Config {
    pub site_url: String, // https://{site}.atlassian.net
    pub email: String,
    pub account_id: String, // from GET /myself
    // The fields below default, so a config written before they existed still loads
    #[serde(default)]
    pub watched_projects: Vec<String>, // project keys
    // When the last sync started, in Unix milliseconds. `None` until the initial
    // sync has finished
    #[serde(default)]
    pub last_sync_at: Option<i64>,
    // When the last pass for deleted and moved issues started, in Unix
    // milliseconds. 0 until the first one
    #[serde(default)]
    pub last_reconcile_at: i64,
    // Lives in the token store, never in the config file
    #[serde(skip)]
    pub api_token: String,
}

/// Where the API token is kept. The calls block: run them off the async threads.
pub(crate) trait TokenStore: Send + Sync {
    fn load(&self) -> anyhow::Result<Option<String>>;
    fn save(&self, token: &str) -> anyhow::Result<()>;
}

/// The macOS Keychain, under the given service name.
pub(crate) struct Keychain(pub &'static str);

impl Keychain {
    pub(crate) fn of_the_app() -> Self {
        Self(KEYCHAIN_SERVICE)
    }

    fn entry(&self) -> keyring::Result<keyring::Entry> {
        keyring::Entry::new(self.0, KEYCHAIN_ACCOUNT)
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
}

/// The saved credentials, or `None` when the user has to sign in.
// An unreadable config or a Keychain that refuses access is not fatal: signing
// in again writes both anew.
pub(crate) async fn load(data_dir: &Path, tokens: Arc<dyn TokenStore>) -> Option<Config> {
    // The config is read first, so a fresh install never queries the Keychain
    let json = tokio::fs::read(data_dir.join(CONFIG_FILE)).await.ok()?;
    let config: Config = serde_json::from_slice(&json).ok()?;
    let api_token = tokio::task::spawn_blocking(move || tokens.load())
        .await
        .ok()?
        .ok()??;
    Some(Config {
        api_token,
        ..config
    })
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

/// Writes the config file only: the token store is not touched.
pub(crate) async fn save_config(data_dir: &Path, config: &Config) -> anyhow::Result<()> {
    tokio::fs::write(
        data_dir.join(CONFIG_FILE),
        serde_json::to_vec_pretty(config)?,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_keeps_the_token() {
        // A service of its own, so the token of the real app is never touched
        let keychain = Keychain("dev.shrew.app.test");
        keychain.entry().unwrap().delete_credential().ok();
        assert_eq!(keychain.load().unwrap(), None);

        keychain.save("first").unwrap();
        keychain.save("second").unwrap();
        assert_eq!(keychain.load().unwrap().as_deref(), Some("second"));

        keychain.entry().unwrap().delete_credential().unwrap();
    }
}
