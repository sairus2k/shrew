//! The background loop that keeps the cache in step with Jira.

use crate::{
    AppState, SyncEvent,
    auth::{self, Config},
    db::sync_state,
    sync::{
        inbound,
        jira_client::{JiraClient, JiraError},
        outbound, transitions,
    },
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

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state_id: i32, event_type: u32) -> f64;
}

#[cfg(target_os = "macos")]
fn system_idle_time() -> Duration {
    const COMBINED_SESSION_STATE: i32 = 0;
    const ANY_INPUT_EVENT: u32 = u32::MAX;
    // SAFETY: the function takes two plain integers and only reads a counter
    let seconds =
        unsafe { CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT) };
    Duration::try_from_secs_f64(seconds).unwrap_or_default()
}

/// LASTINPUTINFO of the Windows API.
#[cfg(windows)]
#[repr(C)]
struct LastInputInfo {
    size: u32,
    // In the milliseconds of GetTickCount
    tick: u32,
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetLastInputInfo(info: *mut LastInputInfo) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
}

#[cfg(windows)]
fn system_idle_time() -> Duration {
    let mut info = LastInputInfo {
        size: size_of::<LastInputInfo>() as u32,
        tick: 0,
    };
    // SAFETY: `info` is a valid LASTINPUTINFO with its size set, and
    // GetTickCount takes nothing
    let now = unsafe {
        if GetLastInputInfo(&mut info) == 0 {
            return Duration::ZERO;
        }
        GetTickCount()
    };
    // The tick count starts anew from zero after 49.7 days
    Duration::from_millis(now.wrapping_sub(info.tick).into())
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
        let cycle = sync_cycle(&state, &mut reconciled, &mut rejected_token).await;
        let synced_at = Instant::now();
        match cycle {
            Err(error) => {
                state
                    .events
                    .send(SyncEvent::Error(format!("{error:#}")))
                    .ok();
            }
            // Jira is reachable and takes the token
            Ok(()) if rejected_token.is_none() => {
                tokio::select! {
                    _ = preload_transitions(&state) => {}
                    // The cycle that was asked for does not wait for the preload
                    _ = waker.changed() => continue,
                }
            }
            Ok(()) => {}
        }
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

/// One pass of the transition preload, cut short when the user signs out.
/// It is not part of a cycle, so that a cycle is as long as before, and holds
/// the lock of one, so that a sign-out wipes the cache only after it stopped.
pub(crate) async fn preload_transitions(state: &AppState) {
    let mut signed_out = state.signed_out.subscribe();
    let _running = state.sync_running.lock().await;
    tokio::select! {
        _ = transitions::preload(state) => {}
        _ = signed_out.changed() => {}
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
    let sync = sync_state::load(&state.pool).await?;
    let Some(last_sync_at) = sync.last_sync_at else {
        return Ok(());
    };

    // The token store did not give the token. Jira would answer 401 to the
    // request, so none is made until `api::renew_token` saves one.
    if config.api_token.is_empty() {
        *rejected_token = Some(String::new());
    }
    if rejected_token.as_ref() == Some(&config.api_token) {
        return Ok(());
    }

    state.events.send(SyncEvent::Started).ok();
    let mut changed = Vec::new();
    let synced = async {
        // Local changes go out first: the poll then reads what Jira made of them
        outbound::process_queue(state, &config, &mut changed).await?;
        outbound::roll_back_unconfirmed(state, &config, &mut changed).await;
        let watched = &sync.watched_projects;
        inbound::poll_updated_issues(state, &config, watched, last_sync_at, &mut changed).await?;
        inbound::poll_updated_worklogs(state, &config, sync.last_worklog_sync_at, &mut changed)
            .await?;
        let deleted_since = sync.last_worklog_delete_sync_at;
        inbound::poll_deleted_worklogs(state, &config, deleted_since, &mut changed).await?;
        // Deleted and moved issues never match `updated >=`: the poll cannot see them
        if inbound::reconcile_due(sync.last_reconcile_at, *reconciled, inbound::now_ms()?) {
            changed.extend(inbound::reconcile_removed_issues(state, &config, watched).await?);
            *reconciled = true;
        }
        // After the issue poll, which brings the issues a sprint was given
        inbound::refresh_sprints(state, &config, watched, &mut changed).await?;
        learn_display_name(state, &config).await;
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

/// Asks Jira for the name of the user when the config has none, as one
/// written before the name was kept. A failure waits for the next cycle: the
/// name only replaces "You" on what the user wrote here.
async fn learn_display_name(state: &AppState, config: &Config) {
    if config.display_name.is_some() {
        return;
    }
    let client = JiraClient::new(&config.site_url, &config.email, &config.api_token);
    let Ok(myself) = client.myself().await else {
        return;
    };
    let mut saved = state.config.write().await;
    if let Some(current) = saved.as_mut() {
        current.display_name = myself.display_name;
        auth::save_config(&state.data_dir, current).await.ok();
    }
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
