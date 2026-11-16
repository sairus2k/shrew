//! The background loop that keeps the cache in step with Jira.

use crate::{AppState, SyncEvent, sync::inbound};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(180);

pub(crate) async fn run(state: Arc<AppState>, mut waker: watch::Receiver<()>) {
    loop {
        if let Err(error) = sync_cycle(&state).await {
            state
                .events
                .send(SyncEvent::Error(format!("{error:#}")))
                .ok();
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
            _ = waker.changed() => {}
        }
    }
}

async fn sync_cycle(state: &AppState) -> anyhow::Result<()> {
    // Until the initial sync has finished there is no time to continue from
    let Some(config) = state.config.read().await.clone() else {
        return Ok(());
    };
    let Some(last_sync_at) = config.last_sync_at else {
        return Ok(());
    };

    state.events.send(SyncEvent::Started).ok();
    let mut changed = Vec::new();
    let polled = inbound::poll_updated_issues(state, &config, last_sync_at, &mut changed).await;
    // The pages stored before a failure are committed, and the next poll will
    // not report them as changed
    if !changed.is_empty() {
        state.events.send(SyncEvent::IssuesUpdated(changed)).ok();
    }
    polled?;
    state.events.send(SyncEvent::Completed).ok();
    Ok(())
}
