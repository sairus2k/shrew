//! The line at the bottom of the main screen that tells whether the issues on
//! screen are current.

use crate::runtime::{Core, spawn};
use app_core::{SyncEvent, api};
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct SyncStatus {
    running: bool,
    last_failed: bool,
    // Unix milliseconds
    last_completed: Option<i64>,
    _load: Task<()>,
    _tick: Task<()>,
}

impl SyncStatus {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sync = cx.global::<Core>().sync.clone();
        cx.subscribe(&sync, |this, _, event: &SyncEvent, cx| {
            match event {
                SyncEvent::Started => this.running = true,
                SyncEvent::Completed => {
                    this.running = false;
                    this.last_failed = false;
                    this.last_completed = Some(now_ms());
                }
                SyncEvent::Error(_) | SyncEvent::TokenRejected => {
                    this.running = false;
                    this.last_failed = true;
                }
                _ => return,
            }
            cx.notify();
        })
        .detach();

        // What the previous run of the app saved, until this one completes a sync
        let saved = spawn(
            cx,
            |state| async move { Ok(api::last_sync_at(&state).await) },
        );
        let _load = cx.spawn(async move |this, cx| {
            let saved = saved.await.ok().flatten();
            this.update(cx, |this, cx| {
                this.last_completed = this.last_completed.or(saved);
                cx.notify();
            })
            .ok();
        });
        // "3 min ago" grows old while no sync event arrives, as when the sync is paused
        let _tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            running: false,
            last_failed: false,
            last_completed: None,
            _load,
            _tick,
        }
    }
}

fn now_ms() -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH);
    now.map_or(0, |now| now.as_millis() as i64)
}

fn status_text(running: bool, last_failed: bool, last_completed: Option<i64>, now: i64) -> String {
    let state = if running {
        Some("Syncing…")
    } else if last_failed {
        Some("Last sync failed")
    } else {
        None
    };
    let ago = last_completed.map(|completed| ago(now - completed));
    match (state, ago) {
        (Some(state), Some(ago)) => format!("{state} · last synced {ago}"),
        (Some(state), None) => state.to_owned(),
        (None, Some(ago)) => format!("Synced {ago}"),
        (None, None) => "Not synced yet".to_owned(),
    }
}

fn ago(ms: i64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        ..1 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    }
}

impl Render for SyncStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = SharedString::from(status_text(
            self.running,
            self.last_failed,
            self.last_completed,
            now_ms(),
        ));
        div()
            .id("sync-status")
            .role(Role::Status)
            .test_support()
            .aria_label(text.clone())
            .h(px(28.))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(text)
    }
}

#[cfg(test)]
mod tests {
    use super::status_text;
    use crate::{
        issue_list::tests::synced,
        runtime::Core,
        sign_in::tests::{in_window, launch, tokio_runtime, wait_until},
    };
    use app_core::SyncEvent;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};

    fn emit(cx: &mut TestAppContext, event: SyncEvent) {
        cx.update(|cx| cx.global::<Core>().state.emit(event));
    }

    fn wait_for_status(cx: &mut TestAppContext, window: AnyWindowHandle, expected: &str) {
        wait_until(cx, window, |window| {
            window.find("sync-status").label() == Some(expected)
        });
    }

    #[test]
    fn the_status_names_the_running_sync_the_last_failure_and_the_last_completion() {
        const MIN: i64 = 60_000;
        let now = 100 * 24 * 60 * MIN;
        let text = |running, failed, minutes_ago: Option<i64>| {
            status_text(running, failed, minutes_ago.map(|m| now - m * MIN), now)
        };

        assert_eq!(text(false, false, None), "Not synced yet");
        assert_eq!(text(true, false, None), "Syncing…");
        assert_eq!(text(false, false, Some(0)), "Synced just now");
        assert_eq!(text(false, false, Some(59)), "Synced 59 min ago");
        assert_eq!(text(false, false, Some(60)), "Synced 1 h ago");
        assert_eq!(text(false, false, Some(3 * 1440)), "Synced 3 d ago");
        assert_eq!(
            text(true, true, Some(4)),
            "Syncing… · last synced 4 min ago"
        );
        assert_eq!(
            text(false, true, Some(4)),
            "Last sync failed · last synced 4 min ago"
        );
        // The clock was set back since the last sync
        assert_eq!(text(false, false, Some(-5)), "Synced just now");
    }

    #[gpui_kit::test]
    fn the_window_follows_the_sync_and_notifies_about_its_errors(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        // The data dir says the last sync was in 1970
        let data_dir = synced(&tokio, &[]);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            let status = window.find("sync-status");
            status
                .label()
                .is_some_and(|label| label.starts_with("Synced ") && label.ends_with(" d ago"))
        });

        emit(cx, SyncEvent::Started);
        wait_until(cx, window, |window| {
            let status = window.find("sync-status");
            status
                .label()
                .is_some_and(|label| label.starts_with("Syncing… · last synced "))
        });

        emit(cx, SyncEvent::Completed);
        wait_for_status(cx, window, "Synced just now");
        let notifications = |cx: &mut TestAppContext| {
            in_window(cx, window, |window, cx| window.notifications(cx).len())
        };
        assert_eq!(notifications(cx), 0);

        emit(cx, SyncEvent::Started);
        emit(cx, SyncEvent::Error("Jira returned 503".to_owned()));
        wait_for_status(cx, window, "Last sync failed · last synced just now");
        assert_eq!(notifications(cx), 1);

        // Jira stays down: the same error is not announced every three minutes
        emit(cx, SyncEvent::Error("Jira returned 503".to_owned()));
        emit(cx, SyncEvent::Error("Jira returned 502".to_owned()));
        cx.run_until_parked();
        assert_eq!(notifications(cx), 2);

        // After a sync that worked, it is news again
        emit(cx, SyncEvent::Completed);
        emit(cx, SyncEvent::Error("Jira returned 502".to_owned()));
        wait_for_status(cx, window, "Last sync failed · last synced just now");
        assert_eq!(notifications(cx), 3);
    }
}
