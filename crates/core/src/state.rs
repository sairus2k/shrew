use crate::{
    SyncEvent,
    auth::{Config, TokenStore},
};
use sqlx::SqlitePool;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{RwLock, mpsc, watch};

pub struct AppState {
    pub(crate) pool: SqlitePool,
    // Every committed change to local data is announced here; the UI listens
    pub(crate) events: mpsc::UnboundedSender<SyncEvent>,
    // `None` until the user signs in
    pub(crate) config: RwLock<Option<Config>>,
    pub(crate) data_dir: PathBuf,
    // Sending wakes the sync engine before its poll interval is over
    pub(crate) sync_waker: watch::Sender<()>,
    pub(crate) tokens: Arc<dyn TokenStore>,
}

#[cfg(feature = "test-support")]
impl AppState {
    pub fn emit(&self, event: SyncEvent) {
        self.events.send(event).ok();
    }

    /// Stores issues the way a sync does. Each one is an element of a Jira
    /// search page. No event is sent.
    pub async fn store_issues(&self, issues: &[serde_json::Value]) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        for issue in issues {
            let record = crate::sync::inbound::issue_record(issue)?;
            crate::db::issues::upsert(&mut *tx, &record).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
