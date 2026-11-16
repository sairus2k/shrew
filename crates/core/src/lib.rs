use std::{path::Path, sync::Arc};
use tokio::sync::mpsc;

pub mod api;
pub mod models;

mod db; // the UI cannot reach SQLite
mod events;
mod state;
mod sync; // or the Jira client
pub use events::SyncEvent;
pub use state::AppState; // an opaque handle: its fields are pub(crate)

pub async fn start(
    data_dir: &Path,
) -> anyhow::Result<(Arc<AppState>, mpsc::UnboundedReceiver<SyncEvent>)> {
    tokio::fs::create_dir_all(data_dir).await?;
    let pool = db::init(data_dir).await?;
    let (events, events_rx) = mpsc::unbounded_channel();
    Ok((Arc::new(AppState { pool, events }), events_rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_creates_the_data_dir_and_the_event_channel() {
        let data_dir = std::env::temp_dir()
            .join(format!("app-core-test-{}", std::process::id()))
            .join("nested");

        let (state, mut events) = start(&data_dir).await.unwrap();
        assert!(data_dir.is_dir());

        state.events.send(SyncEvent::Started).unwrap();
        assert!(matches!(events.recv().await, Some(SyncEvent::Started)));

        std::fs::remove_dir_all(data_dir.parent().unwrap()).unwrap();
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
        assert_eq!(applied, 2);
        state.pool.close().await;
    }
}
