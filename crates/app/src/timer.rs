//! The running timer. It lives in SQLite; this entity holds a copy for
//! rendering and sends every start and stop to the core.

use crate::runtime::spawn;
use crate::theme::tabular_digits;
use app_core::{
    api,
    models::{IssueKey, RunningTimer},
};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

enum Write {
    Start(RunningTimer),
    // The moment of the click, not of the write
    Stop(i64),
}

pub struct ActiveTimer {
    running: Option<RunningTimer>,
    // A click is newer than the initial load, which must not overwrite it
    clicked: bool,
    writes: mpsc::UnboundedSender<Write>,
    _tick: Task<()>,
    _load: Task<()>,
    // The only writer: one task takes the clicks in the order they were made, so
    // a start and a stop never overtake each other on their way to SQLite
    _writer: Task<()>,
}

/// The core could not save a start or a stop; the text is for the user.
pub struct TimerWriteFailed(pub String);

impl EventEmitter<TimerWriteFailed> for ActiveTimer {}

impl ActiveTimer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (writes, mut queue) = mpsc::unbounded_channel();
        let _writer = cx.spawn(async move |this, cx| {
            let mut failed = false;
            while let Some(write) = queue.recv().await {
                let done = cx.update(|cx| {
                    spawn(cx, |state| async move {
                        match write {
                            Write::Start(timer) => api::start_timer(&state, &timer).await,
                            Write::Stop(stopped_at) => api::stop_timer(&state, stopped_at).await,
                        }
                    })
                });
                if let Err(error) = done.await {
                    failed = true;
                    let message = format!("Could not save the timer: {error}");
                    this.update(cx, |_, cx| cx.emit(TimerWriteFailed(message)))
                        .ok();
                }
                // Once the queue is drained: the clicks that came after a failed
                // one were drawn on top of a state the database never reached
                if failed && queue.is_empty() {
                    failed = false;
                    let saved = cx.update(|cx| {
                        spawn(cx, |state| async move { api::running_timer(&state).await })
                    });
                    if let Ok(saved) = saved.await {
                        this.update(cx, |this, cx| this.set_running(saved, cx)).ok();
                    }
                }
            }
        });
        // The timer that was running when the app quit
        let saved = spawn(cx, |state| async move { api::running_timer(&state).await });
        let _load = cx.spawn(async move |this, cx| {
            if let Ok(Some(timer)) = saved.await {
                this.update(cx, |this, cx| {
                    if !this.clicked {
                        this.set_running(Some(timer), cx)
                    }
                })
                .ok();
            }
        });
        Self {
            running: None,
            clicked: false,
            writes,
            _tick: Task::ready(()),
            _load,
            _writer,
        }
    }

    /// A timer that is already running is replaced and its time is not logged.
    pub fn start(&mut self, issue_key: IssueKey, cx: &mut Context<Self>) {
        let timer = RunningTimer {
            issue_key,
            started_at: unix_now(),
        };
        self.clicked = true;
        self.writes.send(Write::Start(timer.clone())).ok();
        self.set_running(Some(timer), cx);
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.running.is_none() {
            return;
        }
        self.clicked = true;
        self.writes.send(Write::Stop(unix_now())).ok();
        self.set_running(None, cx);
    }

    fn set_running(&mut self, running: Option<RunningTimer>, cx: &mut Context<Self>) {
        // The loop only repaints; the elapsed time comes from the start. Assigning
        // the task drops the previous loop.
        self._tick = match running {
            Some(_) => cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }),
            None => Task::ready(()),
        };
        self.running = running;
        cx.notify();
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// "1:02:03", or "2:03" under an hour.
fn clock(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours == 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("{hours}:{minutes:02}:{seconds:02}")
    }
}

/// Start and stop for one issue, with the time elapsed while it runs.
pub struct TimerControl {
    key: IssueKey,
    timer: Entity<ActiveTimer>,
    _timer_changed: Subscription,
}

impl TimerControl {
    pub fn new(key: IssueKey, timer: Entity<ActiveTimer>, cx: &mut Context<Self>) -> Self {
        let _timer_changed = cx.observe(&timer, |_, _, cx| cx.notify());
        Self {
            key,
            timer,
            _timer_changed,
        }
    }
}

impl Render for TimerControl {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.timer.read(cx).running.clone();
        let here = running.as_ref().filter(|timer| timer.issue_key == self.key);
        let elsewhere = running.as_ref().filter(|timer| timer.issue_key != self.key);
        let line = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .aria_label(text.clone())
                .text_sm()
                .font_features(tabular_digits())
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        let timer = self.timer.clone();
        let key = self.key.clone();
        let button = match here {
            Some(_) => Button::new("stop-timer")
                .small()
                .label("Stop")
                .on_click(move |_, _, cx| timer.update(cx, |timer, cx| timer.stop(cx))),
            None => Button::new("start-timer")
                .small()
                .label("Start timer")
                .on_click(move |_, _, cx| {
                    timer.update(cx, |timer, cx| timer.start(key.clone(), cx))
                }),
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .children(
                here.map(|timer| line("timer-elapsed", clock(timer.elapsed_secs(unix_now())))),
            )
            .children(elsewhere.map(|timer| {
                line(
                    "timer-elsewhere",
                    format!("Timer running on {}", timer.issue_key),
                )
            }))
            .child(button)
    }
}

/// The user asked for the issue of the running timer.
pub struct TimerIssueClicked(pub IssueKey);

/// The running timer in the status bar, so that it is seen with any issue
/// open or none: its issue, the time elapsed and a stop.
pub struct TimerStatus {
    timer: Entity<ActiveTimer>,
    _timer_changed: Subscription,
}

impl EventEmitter<TimerIssueClicked> for TimerStatus {}

impl TimerStatus {
    pub fn new(timer: Entity<ActiveTimer>, cx: &mut Context<Self>) -> Self {
        let _timer_changed = cx.observe(&timer, |_, _, cx| cx.notify());
        Self {
            timer,
            _timer_changed,
        }
    }
}

impl Render for TimerStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(running) = self.timer.read(cx).running.clone() else {
            return div();
        };
        let key = SharedString::from(running.issue_key.to_string());
        let elapsed = SharedString::from(clock(running.elapsed_secs(unix_now())));
        let timer = self.timer.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(7.))
                    .flex_none()
                    .rounded_full()
                    .bg(cx.theme().red),
            )
            .child(
                div()
                    .id("running-timer-issue")
                    .test_support()
                    .aria_label(key.clone())
                    .cursor_pointer()
                    .text_color(cx.theme().foreground)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(TimerIssueClicked(running.issue_key.clone()))
                    }))
                    .child(key),
            )
            .child(
                div()
                    .id("running-timer-elapsed")
                    .test_support()
                    .aria_label(elapsed.clone())
                    .font_features(tabular_digits())
                    .text_color(cx.theme().foreground)
                    .child(elapsed),
            )
            .child(
                Button::new("stop-running-timer")
                    .ghost()
                    .xsmall()
                    .label("Stop")
                    .on_click(move |_, _, cx| timer.update(cx, |timer, cx| timer.stop(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveTimer, clock, unix_now};
    use crate::harness::{in_window, launch, no_worklog_changes, tokio_runtime, wait_until};
    use crate::issue_list::tests::{issue, synced_on};
    use crate::runtime::Core;
    use app_core::{
        AppState, api,
        models::{IssueKey, RunningTimer},
    };
    use gpui_kit::AppContext;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, ScrollDelta, TestAppContext, Window, point, px};
    use serde_json::json;
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::runtime::Runtime;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    #[test]
    fn the_elapsed_time_is_shown_as_a_clock() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(125), "2:05");
        assert_eq!(clock(3723), "1:02:03");
    }

    async fn jira() -> MockServer {
        let server = MockServer::start().await;
        no_worklog_changes().mount(&server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "issues": [issue(1, "Fix login"), issue(2, "Broken checkout")] }),
            ))
            .mount(&server)
            .await;
        server
    }

    /// The timer the previous run of the app left running on WEB-1.
    fn running_since(tokio: &Runtime, data_dir: &std::path::Path, started_at: i64) {
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir).await.unwrap();
            let timer = RunningTimer {
                issue_key: "WEB-1".into(),
                started_at,
            };
            api::start_timer(&state, &timer).await.unwrap();
        });
    }

    fn state(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| cx.global::<Core>().state.clone())
    }

    fn running(tokio: &Runtime, state: &AppState) -> Option<RunningTimer> {
        tokio.block_on(api::running_timer(state)).unwrap()
    }

    fn open(cx: &mut TestAppContext, window: AnyWindowHandle, key: &'static str) {
        let row = format!("issue-{key}");
        wait_until(cx, window, |window| window.try_find(row.clone()).is_some());
        in_window(cx, window, |window, cx| window.click(row.clone(), cx));
        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some(key)
                && (window.try_find("start-timer").is_some()
                    || window.try_find("stop-timer").is_some())
        });
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
        in_window(cx, window, |window, cx| window.click(id, cx));
    }

    fn elapsed(window: &Window) -> Option<String> {
        window
            .try_find("timer-elapsed")
            .map(|line| line.label().unwrap().to_owned())
    }

    fn worklog_lines(window: &Window) -> Vec<String> {
        (0..5)
            .map_while(|index| window.try_find(format!("worklog-{index}")))
            .map(|worklog| worklog.label().unwrap().to_owned())
            .collect()
    }

    fn scroll_to_worklogs(cx: &mut TestAppContext, window: AnyWindowHandle) {
        in_window(cx, window, |window, cx| {
            window.scroll(
                "issue-detail",
                ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                cx,
            );
        });
    }

    #[gpui_kit::test]
    fn a_timer_counts_every_second_and_stopped_within_a_minute_it_logs_nothing(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| assert_eq!(elapsed(window), None));

        click(cx, window, "start-timer");
        wait_until(cx, window, |window| {
            elapsed(window).is_some() && window.try_find("stop-timer").is_some()
        });
        let state = state(cx);
        wait_until(cx, window, |_| {
            running(&tokio, &state).is_some_and(|timer| timer.issue_key == "WEB-1".into())
        });

        // The repaint is on GPUI's clock and the elapsed time on the wall clock
        std::thread::sleep(Duration::from_millis(1100));
        cx.executor().advance_clock(Duration::from_secs(1));
        wait_until(cx, window, |window| {
            elapsed(window).is_some_and(|shown| shown != "0:00")
        });

        click(cx, window, "stop-timer");
        wait_until(cx, window, |window| {
            window.try_find("start-timer").is_some() && elapsed(window).is_none()
        });
        wait_until(cx, window, |_| running(&tokio, &state).is_none());
        // Jira takes no worklog shorter than a minute
        let key: IssueKey = "WEB-1".into();
        let issue = tokio.block_on(api::get_issue(&state, &key)).unwrap();
        assert!(issue.worklogs.is_empty());
    }

    #[gpui_kit::test]
    fn stopping_a_timer_logs_its_time_as_a_pending_worklog(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        wait_until(cx, window, |window| window.try_find("stop-timer").is_some());

        click(cx, window, "stop-timer");
        wait_until(cx, window, |window| {
            window.try_find("start-timer").is_some() && elapsed(window).is_none()
        });
        scroll_to_worklogs(cx, window);
        wait_until(cx, window, |window| {
            worklog_lines(window)
                .iter()
                .any(|line| line.starts_with("You, ") && line.ends_with(", 1h 40m, pending"))
        });
        let state = state(cx);
        assert_eq!(running(&tokio, &state), None);
    }

    #[gpui_kit::test]
    fn starting_a_timer_on_another_issue_replaces_the_running_one_without_logging_it(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-2");
        wait_until(cx, window, |window| {
            window
                .try_find("timer-elsewhere")
                .is_some_and(|line| line.label() == Some("Timer running on WEB-1"))
        });

        click(cx, window, "start-timer");

        let state = state(cx);
        wait_until(cx, window, |_| {
            running(&tokio, &state).is_some_and(|timer| timer.issue_key == "WEB-2".into())
        });
        let key: IssueKey = "WEB-1".into();
        let first = tokio.block_on(api::get_issue(&state, &key)).unwrap();
        assert!(first.worklogs.is_empty());
    }

    #[gpui_kit::test]
    fn a_start_and_a_stop_clicked_at_once_reach_the_database_in_click_order(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-2");

        // Start on WEB-2 replaces the old timer, and the stop that follows ends it
        // at once. Overtaken, the stop would log the 6000 s of WEB-1 and the start
        // would leave a timer on WEB-2.
        in_window(cx, window, |window, cx| {
            window.click("start-timer", cx);
            window.click("stop-timer", cx);
        });

        let state = state(cx);
        let key: IssueKey = "WEB-1".into();
        wait_until(cx, window, |_| {
            running(&tokio, &state).is_none()
                && tokio
                    .block_on(api::get_issue(&state, &key))
                    .unwrap()
                    .worklogs
                    .is_empty()
        });
    }

    #[gpui_kit::test]
    fn a_stop_that_cannot_be_saved_is_reported_and_the_timer_still_shows_as_running(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        wait_until(cx, window, |window| window.try_find("stop-timer").is_some());
        let state = state(cx);
        tokio
            .block_on(state.execute(
                "CREATE TRIGGER timer_is_stuck BEFORE DELETE ON running_timer
                 BEGIN SELECT RAISE(ABORT, 'the timer is stuck'); END",
            ))
            .unwrap();

        click(cx, window, "stop-timer");

        wait_until(cx, window, |window| {
            window.try_find("notification").is_some()
        });
        // The click took the control to "Start timer" at once; the failure takes it back
        wait_until(cx, window, |window| {
            window.try_find("stop-timer").is_some() && elapsed(window).is_some()
        });
        in_window(cx, window, |window, cx| {
            assert_eq!(window.notifications(cx).len(), 1);
            assert!(status_bar_timer(window).is_some());
        });
        assert!(running(&tokio, &state).is_some());
    }

    #[gpui_kit::test]
    fn the_timer_is_still_running_after_a_restart_and_counts_from_its_start(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        click(cx, window, "start-timer");
        let first_state = state(cx);
        wait_until(cx, window, |_| running(&tokio, &first_state).is_some());
        let started = running(&tokio, &first_state).unwrap();
        // Time passes while the app is closed
        std::thread::sleep(Duration::from_millis(2100));

        let window = reopen(cx, window, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        // Counting from the start, not from the launch
        wait_until(cx, window, |window| {
            elapsed(window).is_some_and(|shown| !["0:00", "0:01"].contains(&shown.as_str()))
        });
        let state = state(cx);
        assert_eq!(running(&tokio, &state), Some(started));
        in_window(cx, window, |window, _| {
            assert!(window.try_find("stop-timer").is_some());
        });
    }

    #[gpui_kit::test]
    fn a_start_clicked_before_the_initial_load_ends_is_not_overwritten_by_it(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        crate::runtime::tests::init_core(cx, &tokio, data_dir.path());
        cx.executor().allow_parking();

        // The load of WEB-1 is under way when the click on WEB-2 comes
        let timer = cx.update(|cx| cx.new(ActiveTimer::new));
        timer.update(cx, |timer, cx| timer.start("WEB-2".into(), cx));

        let state = state(cx);
        let started = Instant::now();
        loop {
            cx.run_until_parked();
            let saved = running(&tokio, &state);
            let shown = timer.read_with(cx, |timer, _| timer.running.clone());
            if saved
                .as_ref()
                .is_some_and(|t| t.issue_key == "WEB-2".into())
            {
                assert_eq!(shown, saved);
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the start was not saved"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Let the load finish; the control must not go back to WEB-1
        for _ in 0..20 {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        let shown = timer.read_with(cx, |timer, _| timer.running.clone());
        assert_eq!(shown, running(&tokio, &state));
        assert_eq!(shown.unwrap().issue_key, "WEB-2".into());
    }

    fn status_bar_timer(window: &Window) -> Option<(String, String)> {
        let issue = window.try_find("running-timer-issue")?;
        let elapsed = window.find("running-timer-elapsed");
        Some((
            issue.label().unwrap().to_owned(),
            elapsed.label().unwrap().to_owned(),
        ))
    }

    #[gpui_kit::test]
    fn the_status_bar_shows_the_running_timer_whichever_issue_is_open_and_stops_it(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());

        // No issue is open
        wait_until(cx, window, |window| {
            status_bar_timer(window)
                .is_some_and(|(issue, elapsed)| issue == "WEB-1" && elapsed.starts_with("1:40:"))
        });
        open(cx, window, "WEB-2");
        in_window(cx, window, |window, _| {
            assert_eq!(status_bar_timer(window).unwrap().0, "WEB-1");
        });

        click(cx, window, "stop-running-timer");

        wait_until(cx, window, |window| status_bar_timer(window).is_none());
        let state = state(cx);
        let key: IssueKey = "WEB-1".into();
        wait_until(cx, window, |_| {
            running(&tokio, &state).is_none()
                && tokio
                    .block_on(api::get_issue(&state, &key))
                    .unwrap()
                    .worklogs
                    .len()
                    == 1
        });
    }

    #[gpui_kit::test]
    fn the_issue_of_the_running_timer_opens_from_the_status_bar(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        running_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-2");
        wait_until(cx, window, |window| status_bar_timer(window).is_some());

        click(cx, window, "running-timer-issue");

        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some("WEB-1")
        });
    }

    /// Closes the window and the core under it and starts the app again on the
    /// same data dir, as quitting and launching it does.
    fn reopen(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        tokio: &Runtime,
        data_dir: &std::path::Path,
    ) -> AnyWindowHandle {
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        launch(cx, tokio, data_dir)
    }
}
