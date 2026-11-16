use crate::SyncEvent;
use sqlx::SqlitePool;
use tokio::sync::mpsc;

pub struct AppState {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "nothing in the core queries the database yet")
    )]
    pub(crate) pool: SqlitePool,
    // Every committed change to local data is announced here; the UI listens
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        expect(dead_code, reason = "nothing in the core sends events yet")
    )]
    pub(crate) events: mpsc::UnboundedSender<SyncEvent>,
}

#[cfg(feature = "test-support")]
impl AppState {
    pub fn emit(&self, event: SyncEvent) {
        self.events.send(event).ok();
    }
}
