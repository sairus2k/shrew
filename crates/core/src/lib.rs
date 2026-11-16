use std::{path::Path, sync::Arc};
use tokio::sync::mpsc;

pub mod api;
pub mod models;

mod events;
mod state;
pub use events::SyncEvent;
pub use state::AppState; // an opaque handle: its fields are pub(crate)

pub async fn start(
    data_dir: &Path,
) -> anyhow::Result<(Arc<AppState>, mpsc::UnboundedReceiver<SyncEvent>)> {
    tokio::fs::create_dir_all(data_dir).await?;
    let (events, events_rx) = mpsc::unbounded_channel();
    Ok((Arc::new(AppState { events }), events_rx))
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
}
