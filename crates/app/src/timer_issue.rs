//! The questions about a timer that runs without an issue: which issue its
//! time belongs to, and what becomes of that time when the timer is started
//! on an issue.

use crate::runtime::spawn;
use crate::timer::{ActiveTimer, clock, local_time, unix_now};
use app_core::{
    api,
    models::{IssueFilter, IssueKey, IssueRow},
};
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

#[derive(Clone, Copy)]
enum Purpose {
    /// The timer was stopped at `stopped_at`: the issue gets the time up to then.
    Stop { started_at: i64, stopped_at: i64 },
    /// The timer goes on for the issue.
    Running,
}

/// The cached issues found by their key or the words of their summary, one
/// of which gets the time of the timer.
pub struct IssuePicker {
    timer: Entity<ActiveTimer>,
    purpose: Purpose,
    query: Entity<InputState>,
    issues: Vec<IssueRow>,
    _load: Task<()>,
    _query_changed: Subscription,
}

impl IssuePicker {
    fn new(
        timer: Entity<ActiveTimer>,
        purpose: Purpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Key or words of the summary"));
        let _query_changed = cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.load(cx),
                // The best match is the first
                InputEvent::PressEnter { .. } => {
                    if let Some(issue) = this.issues.first() {
                        this.pick(issue.key.clone(), window, cx);
                    }
                }
                _ => {}
            },
        );
        let mut this = Self {
            timer,
            purpose,
            query,
            issues: Vec::new(),
            _load: Task::ready(()),
            _query_changed,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_owned();
        let load = spawn(cx, move |state| async move {
            let filter = IssueFilter::default();
            // One letter matches too much to be a search, as in the issue list
            let mut issues = match query.chars().count() >= 2 {
                true => api::search_issues(&state, &query, &filter, None).await?,
                false => api::list_issues(&state, &filter, None).await?,
            };
            issues.truncate(api::SEARCH_LIMIT);
            anyhow::Ok(issues)
        });
        // Assigning the task drops the previous one, so a slower older query
        // never replaces newer rows
        self._load = cx.spawn(async move |this, cx| {
            if let Ok(issues) = load.await {
                this.update(cx, |this, cx| {
                    this.issues = issues;
                    cx.notify();
                })
                .ok();
            }
        });
    }

    fn pick(&mut self, key: IssueKey, window: &mut Window, cx: &mut Context<Self>) {
        let purpose = self.purpose;
        self.timer.update(cx, |timer, cx| match purpose {
            Purpose::Stop { stopped_at, .. } => timer.stop_on(key, stopped_at, cx),
            Purpose::Running => timer.choose_issue(key, cx),
        });
        window.close_dialog(cx);
    }
}

impl Render for IssuePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.issues.iter().map(|issue| {
            let label = format!("{} {}", issue.key, issue.summary);
            let key = issue.key.clone();
            div()
                .id(SharedString::from(format!("pick-{}", issue.key)))
                .test_support()
                .aria_label(label)
                .px_2()
                .py_1()
                .flex()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(cx.theme().list_hover))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.pick(key.clone(), window, cx)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(cx.theme().muted_foreground)
                        .child(issue.key.to_string()),
                )
                .child(div().min_w_0().truncate().child(issue.summary.clone()))
        });
        let time = match self.purpose {
            Purpose::Stop {
                started_at,
                stopped_at,
            } => {
                let spent = clock((stopped_at - started_at).max(0) as u64);
                let text = format!(
                    "{spent}, from {} to {}",
                    local_time(started_at),
                    local_time(stopped_at)
                );
                Some(
                    div()
                        .id("picked-time")
                        .test_support()
                        .aria_label(text.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                )
            }
            Purpose::Running => None,
        };
        div()
            .id("issue-picker")
            .test_support()
            .flex()
            .flex_col()
            .gap_3()
            .children(time)
            .child(
                Input::new(&self.query)
                    .id("issue-picker-search")
                    .aria_label("Key or words of the summary"),
            )
            .child(
                div()
                    .id("issue-picker-results")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .children(rows),
            )
    }
}

/// Asks which issue gets the time of the timer that runs without one: the
/// time up to the stop at `stopped_at`, or with `None` the timer from its
/// start on. Leaving a stop without an issue asks in words whether the timer
/// runs on or the time is discarded.
pub fn choose_issue(
    timer: Entity<ActiveTimer>,
    stopped_at: Option<i64>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(started_at) = timer.read(cx).running().map(|running| running.started_at) else {
        return;
    };
    let purpose = match stopped_at {
        Some(stopped_at) => Purpose::Stop {
            started_at,
            stopped_at,
        },
        None => Purpose::Running,
    };
    let picker = cx.new(|cx| IssuePicker::new(timer.clone(), purpose, window, cx));
    let query = picker.read(cx).query.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let dialog = dialog
            .w(px(560.))
            .child(picker.clone())
            // Nothing is lost by leaving: the timer runs on, as it did
            .on_close({
                let timer = timer.clone();
                move |_, _, cx| timer.update(cx, |timer, cx| timer.keep_running(cx))
            });
        match purpose {
            Purpose::Running => dialog.title("Which issue is the timer for?"),
            Purpose::Stop { .. } => {
                let keep = timer.clone();
                let discard = timer.clone();
                dialog.title("Which issue was the time for?").footer(
                    div()
                        .w_full()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("keep-timer-running")
                                .small()
                                .label("Keep the timer running")
                                .on_click(move |_, window, cx| {
                                    keep.update(cx, |timer, cx| timer.keep_running(cx));
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("discard-timer")
                                .small()
                                .danger()
                                .label("Discard the time")
                                .on_click(move |_, window, cx| {
                                    discard.update(cx, |timer, cx| timer.discard(cx));
                                    window.close_dialog(cx);
                                }),
                        ),
                )
            }
        }
    });
    query.update(cx, |query, cx| query.focus(window, cx));
}

/// Asks what becomes of the time of the timer without an issue when the
/// user starts the timer on `key`: `key` gets it, or it is discarded.
pub fn ask_start_over(
    timer: Entity<ActiveTimer>,
    key: IssueKey,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(started_at) = timer.read(cx).running().map(|running| running.started_at) else {
        return;
    };
    let description = format!(
        "It has run for {} since {} with no issue chosen. Give that time to {key}, and the \
         timer goes on for it, or discard the time and start the timer on {key} anew.",
        clock((unix_now() - started_at).max(0) as u64),
        local_time(started_at),
    );
    window.open_dialog(cx, move |dialog, _, _| {
        let give = timer.clone();
        let anew = timer.clone();
        let (give_key, anew_key) = (key.clone(), key.clone());
        dialog
            .title("A timer without an issue is running")
            .child(
                div()
                    .id("start-over-question")
                    .test_support()
                    .aria_label(description.clone())
                    .text_sm()
                    .child(description.clone()),
            )
            .footer(
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-start-over")
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("start-over")
                            .small()
                            .danger()
                            .label("Discard and start anew")
                            .on_click(move |_, window, cx| {
                                anew.update(cx, |timer, cx| {
                                    timer.start_anew(Some(anew_key.clone()), cx)
                                });
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("give-time-to-issue")
                            .small()
                            .primary()
                            .label(format!("Give the time to {key}"))
                            .on_click(move |_, window, cx| {
                                give.update(cx, |timer, cx| {
                                    timer.choose_issue(give_key.clone(), cx)
                                });
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}
