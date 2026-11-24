//! The running timer. It lives in SQLite; this entity holds a copy for
//! rendering and sends every start and stop to the core. It may run without
//! an issue: the user chooses one at the stop or while it runs.

use crate::keys;
use crate::runtime::spawn;
use crate::theme::tabular_digits;
use app_core::{
    api,
    models::{IssueKey, RunningTimer},
};
use chrono::{Local, TimeZone};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

enum Write {
    Start(RunningTimer),
    Stop {
        // The moment of the click, not of the write
        at: i64,
        // The issue the user chose for a timer without one
        issue: Option<IssueKey>,
    },
    Choose(IssueKey),
    Discard,
}

pub struct ActiveTimer {
    running: Option<RunningTimer>,
    // The moment the user stopped a timer without an issue, while the issue
    // the time belongs to is being chosen. The timer runs on until then.
    stopping: Option<i64>,
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

/// What only the user can answer about a timer without an issue.
pub enum TimerQuestion {
    /// Which issue gets the time of the timer stopped at `stopped_at`.
    IssueForStop { stopped_at: i64 },
    /// Which issue the running timer goes on for.
    IssueForRunning,
    /// A start on the issue would end the timer without an issue: whether it
    /// gets that time or the time is discarded.
    StartOver(IssueKey),
}

impl EventEmitter<TimerWriteFailed> for ActiveTimer {}
impl EventEmitter<TimerQuestion> for ActiveTimer {}

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
                            Write::Stop { at, issue } => {
                                api::stop_timer(&state, at, issue.as_ref()).await
                            }
                            Write::Choose(key) => api::choose_timer_issue(&state, &key).await,
                            Write::Discard => api::discard_timer(&state).await,
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
            stopping: None,
            clicked: false,
            writes,
            _tick: Task::ready(()),
            _load,
            _writer,
        }
    }

    /// Starts the timer on `issue_key`. A timer of another issue is replaced
    /// and its time is not logged; over a timer without an issue the user is
    /// asked first, as its time has no other way to be logged.
    pub fn start(&mut self, issue_key: IssueKey, cx: &mut Context<Self>) {
        if self.runs_without_issue() {
            cx.emit(TimerQuestion::StartOver(issue_key));
            return;
        }
        self.start_anew(Some(issue_key), cx);
    }

    /// Starts the timer on `issue_key`, or without an issue, replacing any
    /// timer that runs without logging its time.
    pub fn start_anew(&mut self, issue_key: Option<IssueKey>, cx: &mut Context<Self>) {
        let timer = RunningTimer {
            issue_key,
            started_at: unix_now(),
        };
        self.clicked = true;
        self.stopping = None;
        self.writes.send(Write::Start(timer.clone())).ok();
        self.set_running(Some(timer), cx);
    }

    /// Stops the timer. A timer without an issue with time to log asks for
    /// the issue first and runs on until it is chosen or the time discarded.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(running) = &self.running else {
            return;
        };
        let now = unix_now();
        if running.issue_key.is_none() && running.loggable_at(now) {
            // A second stop, as from the menu bar, finds the question open
            // and keeps the moment of the first
            if self.stopping.is_none() {
                self.stopping = Some(now);
                cx.emit(TimerQuestion::IssueForStop { stopped_at: now });
                cx.notify();
            }
            return;
        }
        self.write_stop(now, None, cx);
    }

    /// Logs the time up to `stopped_at`, the stop that asked for the issue,
    /// to `issue_key`.
    pub fn stop_on(&mut self, issue_key: IssueKey, stopped_at: i64, cx: &mut Context<Self>) {
        if self.runs_without_issue() {
            self.write_stop(stopped_at, Some(issue_key), cx);
        }
    }

    fn write_stop(&mut self, at: i64, issue: Option<IssueKey>, cx: &mut Context<Self>) {
        self.clicked = true;
        self.stopping = None;
        self.writes.send(Write::Stop { at, issue }).ok();
        self.set_running(None, cx);
    }

    /// The user left the question about the issue of the stop without
    /// answering it: the timer runs on.
    pub fn keep_running(&mut self, cx: &mut Context<Self>) {
        if self.stopping.take().is_some() {
            cx.notify();
        }
    }

    /// Stops the timer without logging its time.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        if self.running.is_none() {
            return;
        }
        self.clicked = true;
        self.stopping = None;
        self.writes.send(Write::Discard).ok();
        self.set_running(None, cx);
    }

    /// Gives the timer without an issue the issue `issue_key`; it goes on
    /// from its first start.
    pub fn choose_issue(&mut self, issue_key: IssueKey, cx: &mut Context<Self>) {
        let Some(running) = self.running.as_mut().filter(|t| t.issue_key.is_none()) else {
            return;
        };
        running.issue_key = Some(issue_key.clone());
        self.clicked = true;
        self.stopping = None;
        self.writes.send(Write::Choose(issue_key)).ok();
        cx.notify();
    }

    /// Asks the user for the issue of the timer that runs without one.
    pub fn ask_for_issue(&mut self, cx: &mut Context<Self>) {
        if self.runs_without_issue() {
            cx.emit(TimerQuestion::IssueForRunning);
        }
    }

    /// Stops the timer when it runs on `issue_key`, and starts it there otherwise.
    pub fn toggle(&mut self, issue_key: IssueKey, cx: &mut Context<Self>) {
        match &self.running {
            Some(timer) if timer.issue_key.as_ref() == Some(&issue_key) => self.stop(cx),
            _ => self.start(issue_key, cx),
        }
    }

    /// Stops the running timer, or starts one without an issue.
    pub fn toggle_without_issue(&mut self, cx: &mut Context<Self>) {
        match self.running {
            Some(_) => self.stop(cx),
            None => self.start_anew(None, cx),
        }
    }

    pub fn running(&self) -> Option<&RunningTimer> {
        self.running.as_ref()
    }

    /// The moment of a stop that waits for the issue to be chosen.
    pub fn stopping(&self) -> Option<i64> {
        self.stopping
    }

    /// Moves the start of the shown timer `secs` back, as if it had run that
    /// long; the saved one keeps its start.
    #[cfg(test)]
    pub fn backdate(&mut self, secs: i64) {
        if let Some(running) = &mut self.running {
            running.started_at -= secs;
        }
    }

    fn runs_without_issue(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|timer| timer.issue_key.is_none())
    }

    /// The issue the timer runs on.
    pub fn issue(&self) -> Option<&IssueKey> {
        self.running.as_ref()?.issue_key.as_ref()
    }

    /// The timer in one line that stays the same while it runs: "WEB-1 since 14:05".
    pub fn caption(&self) -> Option<String> {
        self.running.as_ref().map(|timer| {
            let started = local_time(timer.started_at);
            format!("{} since {started}", issue_name(timer))
        })
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
        // A stop waits for its issue only while a timer without one runs: the
        // timer saved after a failed write may be another
        if running
            .as_ref()
            .is_none_or(|timer| timer.issue_key.is_some())
        {
            self.stopping = None;
        }
        self.running = running;
        cx.notify();
    }
}

/// What names a timer that runs without an issue in place of its key.
pub const NO_ISSUE: &str = "No issue chosen";

fn issue_name(timer: &RunningTimer) -> String {
    timer
        .issue_key
        .as_ref()
        .map_or_else(|| NO_ISSUE.to_owned(), ToString::to_string)
}

/// "14:05".
pub fn local_time(unix: i64) -> String {
    Local
        .timestamp_opt(unix, 0)
        .single()
        .map_or_else(String::new, |at| at.format("%H:%M").to_string())
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// "1:02:03", or "2:03" under an hour.
pub fn clock(seconds: u64) -> String {
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
        let here = running
            .as_ref()
            .filter(|timer| timer.issue_key.as_ref() == Some(&self.key));
        let elsewhere = running
            .as_ref()
            .filter(|timer| timer.issue_key.as_ref() != Some(&self.key));
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
                .children(keys::hint(&keys::ToggleTimer, cx))
                .on_click(move |_, _, cx| timer.update(cx, |timer, cx| timer.stop(cx))),
            None => Button::new("start-timer")
                .small()
                .label("Start timer")
                .children(keys::hint(&keys::ToggleTimer, cx))
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
                let text = match &timer.issue_key {
                    Some(key) => format!("Timer running on {key}"),
                    None => "Timer running without an issue".to_owned(),
                };
                line("timer-elsewhere", text)
            }))
            .child(button)
    }
}

/// The user asked for the issue of the running timer.
pub struct TimerIssueClicked(pub IssueKey);

/// The running timer in the status bar, so that it is seen with any issue
/// open or none: its issue, the time elapsed and a stop. With none running,
/// the start of a timer without an issue.
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
        let timer = self.timer.clone();
        let Some(running) = self.timer.read(cx).running.clone() else {
            return div().child(
                Button::new("start-timer-without-issue")
                    .ghost()
                    .xsmall()
                    .label("Start timer")
                    .children(keys::hint(&keys::StartOrStopTimer, cx))
                    .on_click(move |_, _, cx| {
                        timer.update(cx, |timer, cx| timer.toggle_without_issue(cx))
                    }),
            );
        };
        let name = SharedString::from(issue_name(&running));
        let elapsed = SharedString::from(clock(running.elapsed_secs(unix_now())));
        let ask = self.timer.clone();
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
                    .aria_label(name.clone())
                    .cursor_pointer()
                    .text_color(match running.issue_key {
                        Some(_) => cx.theme().foreground,
                        None => cx.theme().muted_foreground,
                    })
                    .on_click(cx.listener(move |_, _, _, cx| match &running.issue_key {
                        Some(key) => cx.emit(TimerIssueClicked(key.clone())),
                        None => ask.update(cx, |timer, cx| timer.ask_for_issue(cx)),
                    }))
                    .child(name),
            )
            .children(keys::hint(&keys::OpenTimerIssue, cx))
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
                    .children(keys::hint(&keys::StartOrStopTimer, cx))
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
        crate::harness::no_sprint_field().mount(&server).await;
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
                issue_key: Some("WEB-1".into()),
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
            running(&tokio, &state).is_some_and(|timer| timer.issue_key == Some("WEB-1".into()))
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
            running(&tokio, &state).is_some_and(|timer| timer.issue_key == Some("WEB-2".into()))
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
                .is_some_and(|t| t.issue_key == Some("WEB-2".into()))
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
        assert_eq!(shown.unwrap().issue_key, Some("WEB-2".into()));
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

    /// The timer without an issue the previous run of the app left running.
    fn running_without_issue_since(tokio: &Runtime, data_dir: &std::path::Path, started_at: i64) {
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir).await.unwrap();
            let timer = RunningTimer {
                issue_key: None,
                started_at,
            };
            api::start_timer(&state, &timer).await.unwrap();
        });
    }

    fn launched_with_two_issues() -> (Runtime, MockServer, tempfile::TempDir) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(
            &tokio,
            &server.uri(),
            &[issue(1, "Fix login"), issue(2, "Broken checkout")],
        );
        (tokio, server, data_dir)
    }

    fn worklogs_of(tokio: &Runtime, state: &AppState, key: &str) -> Vec<(i64, i64)> {
        let key: IssueKey = key.into();
        tokio
            .block_on(api::get_issue(state, &key))
            .unwrap()
            .worklogs
            .iter()
            .map(|worklog| (worklog.time_spent_secs as i64, worklog.started_at))
            .collect()
    }

    fn search_picker(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) {
        wait_until(cx, window, |window| {
            window.try_find("issue-picker-search").is_some()
        });
        in_window(cx, window, |window, cx| {
            window.click("issue-picker-search", cx);
            window.input(text, cx);
        });
    }

    #[gpui_kit::test]
    fn a_timer_without_an_issue_starts_from_the_status_bar_and_says_no_issue_is_chosen(
        cx: &mut TestAppContext,
    ) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            window.try_find("start-timer-without-issue").is_some()
        });

        click(cx, window, "start-timer-without-issue");

        wait_until(cx, window, |window| {
            status_bar_timer(window)
                .is_some_and(|(issue, elapsed)| issue == "No issue chosen" && elapsed == "0:00")
        });
        let state = state(cx);
        wait_until(cx, window, |_| {
            running(&tokio, &state).is_some_and(|timer| timer.issue_key.is_none())
        });
        // An open issue says the timer is not its own
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| {
            assert_eq!(
                window.find("timer-elsewhere").label(),
                Some("Timer running without an issue")
            );
        });
    }

    #[gpui_kit::test]
    fn n_starts_a_timer_without_an_issue_with_an_issue_open_and_stops_it(cx: &mut TestAppContext) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        // The keys go to the list, as after a click on a row
        in_window(cx, window, |window, cx| window.press("n", cx));

        let state = state(cx);
        wait_until(cx, window, |_| {
            running(&tokio, &state).is_some_and(|timer| timer.issue_key.is_none())
        });
        in_window(cx, window, |window, _| {
            assert_eq!(status_bar_timer(window).unwrap().0, "No issue chosen");
            // Its own timer is still to start
            assert!(window.try_find("start-timer").is_some());
        });

        // Within its first minute there is no time to give to an issue
        in_window(cx, window, |window, cx| window.press("n", cx));
        wait_until(cx, window, |_| running(&tokio, &state).is_none());
        in_window(cx, window, |window, cx| {
            assert!(!window.has_active_dialog(cx));
        });
    }

    #[gpui_kit::test]
    fn stopping_a_timer_without_an_issue_logs_the_time_up_to_the_stop_to_the_issue_chosen(
        cx: &mut TestAppContext,
    ) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let started_at = unix_now() - 6_000;
        running_without_issue_since(&tokio, data_dir.path(), started_at);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());

        click(cx, window, "stop-running-timer");

        wait_until(cx, window, |window| {
            window
                .try_find("picked-time")
                .and_then(|time| time.label().map(|label| label.starts_with("1:40:0")))
                == Some(true)
        });
        // The issue is found by the words of its summary
        search_picker(cx, window, "checkout");
        wait_until(cx, window, |window| {
            window.try_find("pick-WEB-2").is_some() && window.try_find("pick-WEB-1").is_none()
        });
        // The time is the time up to the stop, not up to the choice
        std::thread::sleep(Duration::from_millis(2100));
        click(cx, window, "pick-WEB-2");

        let state = state(cx);
        wait_until(cx, window, |_| {
            !worklogs_of(&tokio, &state, "WEB-2").is_empty()
        });
        let logged = worklogs_of(&tokio, &state, "WEB-2");
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].1, started_at);
        assert!((6_000..=6_001).contains(&logged[0].0), "{logged:?}");
        assert_eq!(running(&tokio, &state), None);
        in_window(cx, window, |window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(status_bar_timer(window).is_none());
        });
    }

    #[gpui_kit::test]
    fn an_issue_is_found_by_its_key_and_enter_picks_it(cx: &mut TestAppContext) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        running_without_issue_since(&tokio, data_dir.path(), unix_now() - 600);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());
        click(cx, window, "stop-running-timer");

        search_picker(cx, window, "WEB-1");
        wait_until(cx, window, |window| {
            window.try_find("pick-WEB-1").is_some() && window.try_find("pick-WEB-2").is_none()
        });
        in_window(cx, window, |window, cx| window.press("enter", cx));

        let state = state(cx);
        wait_until(cx, window, |_| {
            worklogs_of(&tokio, &state, "WEB-1").len() == 1
        });
    }

    #[gpui_kit::test]
    fn leaving_the_choice_keeps_the_timer_running_or_discards_its_time_as_the_user_says(
        cx: &mut TestAppContext,
    ) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let started_at = unix_now() - 6_000;
        running_without_issue_since(&tokio, data_dir.path(), started_at);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());
        let state = state(cx);
        let without_issue = Some(RunningTimer {
            issue_key: None,
            started_at,
        });

        click(cx, window, "stop-running-timer");
        wait_until(cx, window, |window| {
            window.try_find("keep-timer-running").is_some()
        });
        click(cx, window, "keep-timer-running");

        wait_until(cx, window, |window| {
            window.try_find("issue-picker").is_none()
        });
        in_window(cx, window, |window, _| {
            assert_eq!(status_bar_timer(window).unwrap().0, "No issue chosen");
        });
        assert_eq!(running(&tokio, &state), without_issue);

        // Escape leaves it as the button does: nothing is lost
        click(cx, window, "stop-running-timer");
        wait_until(cx, window, |window| {
            window.try_find("issue-picker").is_some()
        });
        in_window(cx, window, |window, cx| window.press("escape", cx));
        wait_until(cx, window, |window| {
            window.try_find("issue-picker").is_none()
        });
        assert_eq!(running(&tokio, &state), without_issue);

        click(cx, window, "stop-running-timer");
        wait_until(cx, window, |window| {
            window.try_find("discard-timer").is_some()
        });
        click(cx, window, "discard-timer");

        wait_until(cx, window, |_| running(&tokio, &state).is_none());
        in_window(cx, window, |window, _| {
            assert!(status_bar_timer(window).is_none());
        });
        assert!(worklogs_of(&tokio, &state, "WEB-1").is_empty());
        assert!(worklogs_of(&tokio, &state, "WEB-2").is_empty());
    }

    #[gpui_kit::test]
    fn the_issue_is_chosen_for_a_running_timer_and_it_goes_on_from_its_first_start(
        cx: &mut TestAppContext,
    ) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let started_at = unix_now() - 6_000;
        running_without_issue_since(&tokio, data_dir.path(), started_at);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());

        click(cx, window, "running-timer-issue");
        wait_until(cx, window, |window| window.try_find("pick-WEB-1").is_some());
        in_window(cx, window, |window, _| {
            // Nothing is stopped, so there is no time to discard
            assert!(window.try_find("discard-timer").is_none());
            assert!(window.try_find("picked-time").is_none());
        });
        click(cx, window, "pick-WEB-1");

        let state = state(cx);
        wait_until(cx, window, |_| {
            running(&tokio, &state)
                == Some(RunningTimer {
                    issue_key: Some("WEB-1".into()),
                    started_at,
                })
        });
        wait_until(cx, window, |window| {
            status_bar_timer(window).is_some_and(|(issue, _)| issue == "WEB-1")
        });
        assert!(worklogs_of(&tokio, &state, "WEB-1").is_empty());
    }

    #[gpui_kit::test]
    fn a_timer_without_an_issue_is_still_running_after_a_restart(cx: &mut TestAppContext) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            window.try_find("start-timer-without-issue").is_some()
        });
        click(cx, window, "start-timer-without-issue");
        let first_state = state(cx);
        wait_until(cx, window, |_| running(&tokio, &first_state).is_some());
        let started = running(&tokio, &first_state).unwrap();
        std::thread::sleep(Duration::from_millis(2100));

        let window = reopen(cx, window, &tokio, data_dir.path());

        wait_until(cx, window, |window| {
            status_bar_timer(window).is_some_and(|(issue, elapsed)| {
                issue == "No issue chosen" && !["0:00", "0:01"].contains(&elapsed.as_str())
            })
        });
        let state = state(cx);
        assert_eq!(running(&tokio, &state), Some(started));
    }

    fn start_over_web_2(cx: &mut TestAppContext, window: AnyWindowHandle) {
        open(cx, window, "WEB-2");
        click(cx, window, "start-timer");
        wait_until(cx, window, |window| {
            window
                .try_find("start-over-question")
                .is_some_and(|question| {
                    question
                        .label()
                        .unwrap()
                        .starts_with("It has run for 1:40:")
                })
        });
    }

    #[gpui_kit::test]
    fn a_start_on_an_issue_over_a_timer_without_one_asks_what_becomes_of_its_time(
        cx: &mut TestAppContext,
    ) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let started_at = unix_now() - 6_000;
        running_without_issue_since(&tokio, data_dir.path(), started_at);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());
        let state = state(cx);
        let without_issue = Some(RunningTimer {
            issue_key: None,
            started_at,
        });

        start_over_web_2(cx, window);
        click(cx, window, "cancel-start-over");
        wait_until(cx, window, |window| {
            window.try_find("start-over-question").is_none()
        });
        assert_eq!(running(&tokio, &state), without_issue);

        // The time goes to the issue, and the timer on with it
        start_over_web_2(cx, window);
        click(cx, window, "give-time-to-issue");
        wait_until(cx, window, |_| {
            running(&tokio, &state)
                == Some(RunningTimer {
                    issue_key: Some("WEB-2".into()),
                    started_at,
                })
        });
        wait_until(cx, window, |window| {
            window.try_find("stop-timer").is_some()
                && elapsed(window).is_some_and(|shown| shown.starts_with("1:40:"))
        });
    }

    #[gpui_kit::test]
    fn a_start_on_an_issue_over_a_timer_without_one_discards_its_time_when_told_so(
        cx: &mut TestAppContext,
    ) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        running_without_issue_since(&tokio, data_dir.path(), unix_now() - 6_000);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());
        let state = state(cx);

        start_over_web_2(cx, window);
        click(cx, window, "start-over");

        wait_until(cx, window, |_| {
            running(&tokio, &state).is_some_and(|timer| {
                timer.issue_key == Some("WEB-2".into()) && timer.elapsed_secs(unix_now()) < 60
            })
        });
        assert!(worklogs_of(&tokio, &state, "WEB-2").is_empty());
    }

    #[gpui_kit::test]
    fn an_issue_the_cache_no_longer_has_is_reported_and_the_timer_runs_on(cx: &mut TestAppContext) {
        let (tokio, _server, data_dir) = launched_with_two_issues();
        let started_at = unix_now() - 6_000;
        running_without_issue_since(&tokio, data_dir.path(), started_at);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| status_bar_timer(window).is_some());
        click(cx, window, "stop-running-timer");
        wait_until(cx, window, |window| window.try_find("pick-WEB-2").is_some());
        let state = state(cx);
        // Gone from the cache while the picker shows it
        tokio
            .block_on(state.execute("DELETE FROM issues WHERE key = 'WEB-2'"))
            .unwrap();

        click(cx, window, "pick-WEB-2");

        wait_until(cx, window, |window| {
            window.try_find("notification").is_some()
        });
        wait_until(cx, window, |window| {
            status_bar_timer(window).is_some_and(|(issue, _)| issue == "No issue chosen")
        });
        assert_eq!(
            running(&tokio, &state),
            Some(RunningTimer {
                issue_key: None,
                started_at,
            })
        );
    }
}
