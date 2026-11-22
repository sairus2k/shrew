use std::{path::Path, sync::Arc};
use tokio::sync::{Mutex, RwLock, mpsc, watch};

pub mod api;
pub mod models;

mod adf;
mod auth;
mod db; // the UI cannot reach SQLite
mod events;
mod state;
mod sync; // or the Jira client
pub use events::SyncEvent;
pub use state::AppState; // an opaque handle: its fields are pub(crate)

// Must be called inside the tokio runtime: it spawns the sync engine
pub async fn start(
    data_dir: &Path,
) -> anyhow::Result<(Arc<AppState>, mpsc::UnboundedReceiver<SyncEvent>)> {
    start_with(
        data_dir,
        Arc::new(auth::Keychain::of_the_app()),
        Arc::new(auth::Keychain::tempo_of_the_app()),
    )
    .await
}

/// `start` with the API token kept in a file under `data_dir`, so that tests
/// never read or write the user's Keychain, and with no sync engine: a UI test
/// moves the data itself, and an engine polling on its own thread would make
/// the test's event order depend on timing.
#[cfg(any(test, feature = "test-support"))]
pub async fn start_without_keychain(
    data_dir: &Path,
) -> anyhow::Result<(Arc<AppState>, mpsc::UnboundedReceiver<SyncEvent>)> {
    let tokens = auth::TokenFile(data_dir.join("api-token"));
    let tempo_tokens = auth::TokenFile(data_dir.join("tempo-token"));
    let (state, events_rx, _wake_rx) =
        open(data_dir, Arc::new(tokens), Arc::new(tempo_tokens)).await?;
    Ok((state, events_rx))
}

/// `start_without_keychain` with the sync engine running.
#[cfg(test)]
pub(crate) async fn start_syncing_without_keychain(
    data_dir: &Path,
) -> anyhow::Result<(Arc<AppState>, mpsc::UnboundedReceiver<SyncEvent>)> {
    let tokens = auth::TokenFile(data_dir.join("api-token"));
    let tempo_tokens = auth::TokenFile(data_dir.join("tempo-token"));
    start_with(data_dir, Arc::new(tokens), Arc::new(tempo_tokens)).await
}

async fn start_with(
    data_dir: &Path,
    tokens: Arc<dyn auth::TokenStore>,
    tempo_tokens: Arc<dyn auth::TokenStore>,
) -> anyhow::Result<(Arc<AppState>, mpsc::UnboundedReceiver<SyncEvent>)> {
    let (state, events_rx, wake_rx) = open(data_dir, tokens, tempo_tokens).await?;
    tokio::spawn(sync::engine::run(state.clone(), wake_rx));
    Ok((state, events_rx))
}

async fn open(
    data_dir: &Path,
    tokens: Arc<dyn auth::TokenStore>,
    tempo_tokens: Arc<dyn auth::TokenStore>,
) -> anyhow::Result<(
    Arc<AppState>,
    mpsc::UnboundedReceiver<SyncEvent>,
    watch::Receiver<()>,
)> {
    tokio::fs::create_dir_all(data_dir).await?;
    let pool = db::init(data_dir).await?;
    let config = auth::load(data_dir, tokens.clone(), tempo_tokens.clone()).await;
    // A database filled by a build that did not record whose data it holds
    if let Some(config) = &config
        && db::owner(&pool).await?.is_none()
    {
        let owner = models::Account {
            site_url: config.site_url.clone(),
            email: config.email.clone(),
            account_id: config.account_id.clone(),
        };
        db::set_owner(&pool, &owner).await?;
    }
    let (events, events_rx) = mpsc::unbounded_channel();
    let (sync_waker, wake_rx) = watch::channel(());
    let state = AppState {
        pool,
        events,
        config: RwLock::new(config),
        data_dir: data_dir.to_owned(),
        sync_waker,
        // The window opens focused, and the UI reports every change
        window_focused: watch::channel(true).0,
        #[cfg(test)]
        idle_time: Default::default(),
        tokens,
        tempo_tokens,
        rejected_tempo_token: Default::default(),
        #[cfg(test)]
        tempo_url: std::sync::Mutex::new(auth::TEMPO_URL.to_owned()),
        sync_running: Mutex::new(()),
        signed_out: watch::channel(()).0,
    };
    Ok((Arc::new(state), events_rx, wake_rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_creates_the_data_dir_and_the_event_channel() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join("missing").join("nested");

        let (state, mut events) = start(&data_dir).await.unwrap();
        assert!(data_dir.is_dir());

        state.events.send(SyncEvent::Started).unwrap();
        assert!(matches!(events.recv().await, Some(SyncEvent::Started)));
    }

    #[tokio::test]
    async fn start_creates_the_database_and_migrates_it_once() {
        let data_dir = tempfile::tempdir().unwrap();

        let (state, _events) = start(data_dir.path()).await.unwrap();
        assert!(data_dir.path().join("shrew.db").is_file());
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master
             WHERE name IN ('issues', 'issues_fts', 'projects', 'users') ORDER BY name",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(tables, ["issues", "issues_fts", "projects", "users"]);
        state.pool.close().await;

        // A second start must not run the applied migrations again
        let (state, _events) = start(data_dir.path()).await.unwrap();
        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(applied, 12);
        state.pool.close().await;
    }
}
