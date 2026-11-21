//! The background loop that keeps the cache in step with Jira.

use crate::{
    AppState, SyncEvent,
    sync::{inbound, jira_client::JiraError, outbound},
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

pub(crate) const ACTIVE_INTERVAL: Duration = Duration::from_secs(3 * 60);
pub(crate) const BACKGROUND_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub(crate) const IDLE_INTERVAL: Duration = Duration::from_secs(30 * 60);
// Without keyboard or mouse input anywhere on the Mac for this long, the user
// is taken to be away from it
pub(crate) const IDLE_AFTER: Duration = Duration::from_secs(5 * 60);
// Nothing announces that the user is back at the Mac, so the wait for the next
// poll looks again this often. A look makes no request.
pub(crate) const ACTIVITY_CHECK: Duration = Duration::from_secs(60);

/// How long after a cycle the next one starts. `idle_for` is the time since
/// the last input anywhere on the Mac; being away outweighs a focused window.
pub(crate) fn poll_interval(window_focused: bool, idle_for: Duration) -> Duration {
    if idle_for >= IDLE_AFTER {
        IDLE_INTERVAL
    } else if window_focused {
        ACTIVE_INTERVAL
    } else {
        BACKGROUND_INTERVAL
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state_id: i32, event_type: u32) -> f64;
}

fn system_idle_time() -> Duration {
    const COMBINED_SESSION_STATE: i32 = 0;
    const ANY_INPUT_EVENT: u32 = u32::MAX;
    // SAFETY: the function takes two plain integers and only reads a counter
    let seconds =
        unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT) };
    Duration::try_from_secs_f64(seconds).unwrap_or_default()
}

/// Tests set the idle time through the state: the real one depends on who
/// touched the machine that runs them.
fn idle_time(state: &AppState) -> Duration {
    #[cfg(test)]
    return *state.idle_time.lock().unwrap();
    #[cfg(not(test))]
    {
        let _ = state;
        system_idle_time()
    }
}

pub(crate) async fn run(state: Arc<AppState>, mut waker: watch::Receiver<()>) {
    let mut window_focused = state.window_focused.subscribe();
    // The pass for deleted and moved issues runs on the first full cycle after
    // the start, whatever its saved time says
    let mut reconciled = false;
    // The token Jira answered 401 to. The app cannot renew it, so no cycle runs
    // until the config holds another one.
    let mut rejected_token = None;
    loop {
        if let Err(error) = sync_cycle(&state, &mut reconciled, &mut rejected_token).await {
            state
                .events
                .send(SyncEvent::Error(format!("{error:#}")))
                .ok();
        }
        let synced_at = Instant::now();
        loop {
            let due = synced_at + poll_interval(*window_focused.borrow(), idle_time(&state));
            let now = Instant::now();
            if now >= due {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(due.min(now + ACTIVITY_CHECK)) => {}
                // The interval is counted again: a window focused after a
                // longer one is due at once
                _ = window_focused.changed() => {}
                _ = waker.changed() => break,
            }
        }
    }
}

/// One cycle, cut short when the user signs out.
pub(crate) async fn sync_cycle(
    state: &AppState,
    reconciled: &mut bool,
    rejected_token: &mut Option<String>,
) -> anyhow::Result<()> {
    // Subscribed before the lock: a sign-out that comes while this waits for
    // the lock has finished by the time the cycle starts, and a later one is seen
    let mut signed_out = state.signed_out.subscribe();
    let _running = state.sync_running.lock().await;
    tokio::select! {
        result = cycle(state, reconciled, rejected_token) => result,
        _ = signed_out.changed() => Ok(()),
    }
}

async fn cycle(
    state: &AppState,
    reconciled: &mut bool,
    rejected_token: &mut Option<String>,
) -> anyhow::Result<()> {
    // Until the initial sync has finished there is no time to continue from
    let Some(config) = state.config.read().await.clone() else {
        return Ok(());
    };
    let Some(last_sync_at) = config.last_sync_at else {
        return Ok(());
    };

    if rejected_token.as_ref() == Some(&config.api_token) {
        return Ok(());
    }

    state.events.send(SyncEvent::Started).ok();
    let mut changed = Vec::new();
    let synced = async {
        // Local changes go out first: the poll then reads what Jira made of them
        outbound::process_queue(state, &config, &mut changed).await?;
        outbound::roll_back_unconfirmed(state, &config, &mut changed).await;
        inbound::poll_updated_issues(state, &config, last_sync_at, &mut changed).await?;
        inbound::poll_updated_worklogs(state, &config, &mut changed).await?;
        inbound::poll_deleted_worklogs(state, &config, &mut changed).await?;
        // Deleted and moved issues never match `updated >=`: the poll cannot see them
        if inbound::reconcile_due(&config, *reconciled, inbound::now_ms()?) {
            changed.extend(inbound::reconcile_removed_issues(state, &config).await?);
            *reconciled = true;
        }
        anyhow::Ok(())
    }
    .await;
    // The pages stored before a failure are committed, and the next poll will
    // not report them as changed
    if !changed.is_empty() {
        state.events.send(SyncEvent::IssuesUpdated(changed)).ok();
    }
    if let Err(error) = &synced
        && matches!(error.downcast_ref(), Some(JiraError::Unauthorized))
    {
        *rejected_token = Some(config.api_token);
        state.events.send(SyncEvent::TokenRejected).ok();
        return Ok(());
    }
    synced?;
    state.events.send(SyncEvent::Completed).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUST_NOW: Duration = Duration::ZERO;

    #[test]
    fn a_focused_window_is_polled_every_three_minutes() {
        assert_eq!(poll_interval(true, JUST_NOW), Duration::from_secs(180));
    }

    #[test]
    fn a_window_in_the_background_is_polled_every_fifteen_minutes() {
        assert_eq!(poll_interval(false, JUST_NOW), Duration::from_secs(900));
    }

    #[test]
    fn an_idle_user_is_polled_every_thirty_minutes_whatever_the_focus() {
        let idle = Duration::from_secs(5 * 60);
        assert_eq!(poll_interval(true, idle), Duration::from_secs(1800));
        assert_eq!(poll_interval(false, idle), Duration::from_secs(1800));
    }

    #[test]
    fn the_user_is_not_idle_before_five_minutes_without_input() {
        let almost = Duration::from_secs(5 * 60 - 1);
        assert_eq!(poll_interval(true, almost), ACTIVE_INTERVAL);
        assert_eq!(poll_interval(false, almost), BACKGROUND_INTERVAL);
    }

    #[test]
    fn the_system_reports_the_time_since_the_last_input() {
        // Zero is also what a failed call is turned into, and no real input is
        // that recent
        assert!(system_idle_time() > Duration::ZERO);
    }
}
