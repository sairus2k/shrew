use crate::{
    SyncEvent,
    auth::{Config, TokenStore},
};
use sqlx::SqlitePool;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{RwLock, mpsc};

pub struct AppState {
    pub(crate) pool: SqlitePool,
    // Every committed change to local data is announced here; the UI listens
    pub(crate) events: mpsc::UnboundedSender<SyncEvent>,
    // `None` until the user signs in
    pub(crate) config: RwLock<Option<Config>>,
    pub(crate) data_dir: PathBuf,
    pub(crate) tokens: Arc<dyn TokenStore>,
}

#[cfg(feature = "test-support")]
impl AppState {
    pub fn emit(&self, event: SyncEvent) {
        self.events.send(event).ok();
    }
}
