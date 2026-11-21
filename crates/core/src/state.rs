use crate::{
    SyncEvent,
    auth::{Config, TokenStore},
};
use sqlx::SqlitePool;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{Mutex, RwLock, mpsc, watch};

pub struct AppState {
    pub(crate) pool: SqlitePool,
    // Every committed change to local data is announced here; the UI listens
    pub(crate) events: mpsc::UnboundedSender<SyncEvent>,
    // `None` until the user signs in
    pub(crate) config: RwLock<Option<Config>>,
    pub(crate) data_dir: PathBuf,
    // Sending wakes the sync engine before its poll interval is over
    pub(crate) sync_waker: watch::Sender<()>,
    // Whether the app's window is the focused one; sets the poll interval
    pub(crate) window_focused: watch::Sender<bool>,
    // Stands in for the system's idle time; see `engine::idle_time`
    #[cfg(test)]
    pub(crate) idle_time: StdMutex<std::time::Duration>,
    pub(crate) tokens: Arc<dyn TokenStore>,
    pub(crate) tempo_tokens: Arc<dyn TokenStore>,
    // The Tempo token that Tempo answered 401 to. Worklogs wait while it is the
    // saved one, and the event is not repeated. Saving a token or choosing the
    // target clears it.
    pub(crate) rejected_tempo_token: StdMutex<Option<String>>,
    // Only tests can move it, to a mock server; see `outbound::tempo_url`
    #[cfg(test)]
    pub(crate) tempo_url: StdMutex<String>,
    // Held by a sync cycle while it runs. A sign-out takes it before it wipes
    // the cache, so a cycle of the old account cannot write into it afterwards.
    pub(crate) sync_running: Mutex<()>,
    // A sign-out sends here, and the running sync cycle stops at once: its
    // request to Jira may never return
    pub(crate) signed_out: watch::Sender<()>,
}

impl AppState {
    /// Called by the UI when the window gains or loses focus.
    pub fn set_window_focused(&self, focused: bool) {
        self.window_focused.send_replace(focused);
    }
}

#[cfg(feature = "test-support")]
impl AppState {
    pub fn window_focused(&self) -> bool {
        *self.window_focused.borrow()
    }

    /// Changes each time the sync engine is asked for a cycle: a core started
    /// without the engine has nothing else to show for the request.
    pub fn sync_requests(&self) -> watch::Receiver<()> {
        self.sync_waker.subscribe()
    }

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

    /// Runs one cycle of the sync engine and the transition preload that
    /// follows it: a core started without the engine syncs only when the test
    /// says so.
    pub async fn sync_once(&self) -> anyhow::Result<()> {
        crate::sync::engine::sync_cycle(self, &mut true, &mut None).await?;
        crate::sync::engine::preload_transitions(self).await;
        Ok(())
    }

    /// Runs `sql` on the cache, for a state that no API leads to.
    pub async fn execute(&self, sql: &str) -> anyhow::Result<()> {
        sqlx::raw_sql(sql).execute(&self.pool).await?;
        Ok(())
    }
}
