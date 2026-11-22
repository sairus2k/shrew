//! The line at the bottom of the main screen that tells whether the issues on
//! screen are current.

use crate::runtime::{Core, spawn};
use app_core::{SyncEvent, api};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct SyncStatus {
    running: bool,
    last_failed: bool,
    // Unix milliseconds
    last_completed: Option<i64>,
    // Issues the sync could not read, with the reason, until a later sync stores them
    skipped: BTreeMap<String, String>,
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
                SyncEvent::IssueSkipped { key, reason } => {
                    this.skipped.insert(key.clone(), reason.clone());
                }
                SyncEvent::IssuesUpdated(keys) => {
                    let before = this.skipped.len();
                    this.skipped
                        .retain(|skipped, _| !keys.contains(&skipped.as_str().into()));
                    if this.skipped.len() == before {
                        return;
                    }
                }
                _ => return,
            }
            cx.notify();
        })
        .detach();

        // What the previous run of the app saved, until this one completes a sync
        let saved = spawn(cx, |state| async move { api::last_sync_at(&state).await });
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
            skipped: BTreeMap::new(),
            _load,
            _tick,
        }
    }
}

fn skipped_reasons(skipped: &BTreeMap<String, String>) -> String {
    let lines: Vec<_> = skipped
        .iter()
        .map(|(key, reason)| format!("{key}: {reason}"))
        .collect();
    lines.join("\n")
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
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let text = SharedString::from(status_text(
            self.running,
            self.last_failed,
            self.last_completed,
            now_ms(),
        ));
        let status = div()
            .id("sync-status")
            .role(Role::Status)
            .test_support()
            .aria_label(text.clone())
            // A request made while a cycle runs would start one more after it
            .when(!self.running, |status| {
                status
                    .cursor_pointer()
                    .tooltip(|window, cx| Tooltip::new("Sync now").build(window, cx))
                    .on_click(|_, _, cx| api::sync_now(&cx.global::<Core>().state))
            })
            .child(text);
        let skipped = (!self.skipped.is_empty()).then(|| {
            let keys = self.skipped.keys().cloned().collect::<Vec<_>>().join(", ");
            let label = SharedString::from(format!("Skipped {keys}"));
            let reasons = SharedString::from(skipped_reasons(&self.skipped));
            div()
                .id("skipped-issues")
                .test_support()
                .aria_label(label.clone())
                .tooltip(move |window, cx| Tooltip::new(reasons.clone()).build(window, cx))
                .child(label)
        });
        div().flex().gap_2().child(status).children(skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::{skipped_reasons, status_text};
    use crate::{
        harness::{in_window, launch, tokio_runtime, wait_until},
        issue_list::tests::synced,
        runtime::Core,
    };
    use app_core::SyncEvent;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};
    use std::collections::BTreeMap;

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

    #[gpui_kit::test]
    fn a_click_on_the_status_asks_for_a_sync_unless_one_is_running(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[]);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            window.try_find("sync-status").is_some()
        });
        let mut sync_requests = cx.update(|cx| cx.global::<Core>().state.sync_requests());
        assert!(!sync_requests.has_changed().unwrap());

        in_window(cx, window, |window, cx| window.click("sync-status", cx));
        assert!(sync_requests.has_changed().unwrap());
        sync_requests.mark_unchanged();

        emit(cx, SyncEvent::Started);
        wait_until(cx, window, |window| {
            let status = window.find("sync-status");
            status
                .label()
                .is_some_and(|label| label.starts_with("Syncing…"))
        });
        in_window(cx, window, |window, cx| window.click("sync-status", cx));
        assert!(!sync_requests.has_changed().unwrap());

        emit(cx, SyncEvent::Completed);
        wait_for_status(cx, window, "Synced just now");
        in_window(cx, window, |window, cx| window.click("sync-status", cx));
        assert!(sync_requests.has_changed().unwrap());
    }

    fn skip(cx: &mut TestAppContext, key: &str, reason: &str) {
        emit(
            cx,
            SyncEvent::IssueSkipped {
                key: key.to_owned(),
                reason: reason.to_owned(),
            },
        );
    }

    #[gpui_kit::test]
    fn the_window_names_the_skipped_issues_until_a_sync_reads_them(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[]);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            window.try_find("sync-status").is_some()
        });
        let skipped = |cx: &mut TestAppContext| {
            in_window(cx, window, |window, _| {
                window
                    .try_find("skipped-issues")
                    .and_then(|skipped| skipped.label().map(str::to_owned))
            })
        };
        assert_eq!(skipped(cx), None);

        skip(
            cx,
            "WEB-2",
            "unexpected issue from Jira: missing field `summary`",
        );
        wait_until(cx, window, |window| {
            window.try_find("skipped-issues").is_some()
        });
        assert_eq!(skipped(cx).as_deref(), Some("Skipped WEB-2"));

        // The same issue is skipped again by every cycle: nothing new to tell
        skip(
            cx,
            "WEB-2",
            "unexpected issue from Jira: missing field `summary`",
        );
        skip(cx, "WEB-3", "WEB-3 has an unreadable `updated`");
        wait_until(cx, window, |window| {
            window.find("skipped-issues").label() == Some("Skipped WEB-2, WEB-3")
        });
        let notifications = in_window(cx, window, |window, cx| window.notifications(cx).len());
        assert_eq!(notifications, 0);

        // A later sync stored WEB-2
        emit(cx, SyncEvent::IssuesUpdated(vec!["WEB-2".into()]));
        wait_until(cx, window, |window| {
            window.find("skipped-issues").label() == Some("Skipped WEB-3")
        });
        emit(cx, SyncEvent::IssuesUpdated(vec!["WEB-3".into()]));
        wait_until(cx, window, |window| {
            window.try_find("skipped-issues").is_none()
        });
    }

    #[test]
    fn the_reasons_name_each_skipped_issue() {
        let skipped = BTreeMap::from([
            ("WEB-2".to_owned(), "no summary".to_owned()),
            ("WEB-3".to_owned(), "bad date".to_owned()),
        ]);
        assert_eq!(
            skipped_reasons(&skipped),
            "WEB-2: no summary\nWEB-3: bad date"
        );
    }
}
