//! The form that logs time on an issue after the fact.

use crate::duration::{local_minute, parse_duration, parse_local_minute};
use crate::keys;
use crate::runtime::spawn;
use app_core::{api, models::IssueKey};
use chrono::Local;
use gpui_kit::component::{
    ActiveTheme, Disableable, WindowExt as _,
    button::Button,
    input::{Input, InputEvent, InputState},
    notification::Notification,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub struct WorklogForm {
    key: IssueKey,
    duration: Entity<InputState>,
    started: Entity<InputState>,
    comment: Entity<InputState>,
    // What the user got wrong, until the next submit
    error: Option<&'static str>,
    // The input stays filled until the core has queued the entry, so a failed
    // write loses nothing; the flag keeps a second click from queueing it twice
    sending: bool,
    _enter: [Subscription; 3],
}

impl WorklogForm {
    pub fn new(key: IssueKey, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let duration = cx.new(|cx| InputState::new(window, cx).placeholder("1h 30m"));
        let started = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(local_minute(Local::now().timestamp()), window, cx);
            input
        });
        let comment = cx.new(|cx| InputState::new(window, cx).placeholder("Comment (optional)"));
        let _enter = [&duration, &started, &comment].map(|input| {
            cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(window, cx);
                }
            })
        });
        Self {
            key,
            duration,
            started,
            comment,
            error: None,
            sending: false,
            _enter,
        }
    }

    /// Puts the cursor into the time spent.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.duration
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let entry = parse_duration(&self.duration.read(cx).value())
            .and_then(|seconds| Ok((seconds, parse_local_minute(&self.started.read(cx).value())?)));
        let (seconds, started_at) = match entry {
            Ok(entry) => entry,
            Err(message) => {
                self.error = Some(message);
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.sending = true;
        cx.notify();
        let key = self.key.clone();
        let comment = self.comment.read(cx).value().to_string();
        let write = spawn(cx, move |state| async move {
            api::log_time(&state, &key, seconds, started_at, Some(&comment)).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = write.await;
            this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match saved {
                    // The core announces the issue and the model reloads it, pending worklog included
                    Ok(()) => {
                        for input in [&this.duration, &this.comment] {
                            input.update(cx, |input, cx| input.set_value("", window, cx));
                        }
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("Could not save the worklog: {error}")),
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for WorklogForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div().w(px(90.)).flex_none().child(
                            Input::new(&self.duration)
                                .id("worklog-duration")
                                .aria_label("Time spent")
                                .when_some(keys::hint(&keys::LogTime, cx), Input::suffix),
                        ),
                    )
                    .child(
                        div().w(px(150.)).flex_none().child(
                            Input::new(&self.started)
                                .id("worklog-started")
                                .aria_label("Started"),
                        ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.comment)
                                .id("worklog-comment-input")
                                .aria_label("Worklog comment"),
                        ),
                    )
                    .child(
                        Button::new("submit-worklog")
                            .label("Log time")
                            .disabled(self.sending)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
            .children(self.error.map(|error| {
                div()
                    .id("worklog-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error)
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }
}

#[cfg(test)]
mod tests {
    use crate::duration::local_minute;
    use crate::harness::{in_window, launch, no_worklog_changes, tokio_runtime, wait_until};
    use crate::issue_detail::tests::painted;
    use crate::issue_list::tests::{issue, synced_on};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{App, TestAppContext, Window};
    use serde_json::json;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    async fn jira() -> MockServer {
        let server = MockServer::start().await;
        no_worklog_changes().mount(&server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "issues": [issue(1, "Fix login")] })),
            )
            .mount(&server)
            .await;
        server
    }

    fn open_web_1(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("issue-WEB-1").is_some()
        });
        in_window(cx, window, |window, cx| window.click("issue-WEB-1", cx));
        wait_until(cx, window, |window| {
            window.try_find("worklog-duration").is_some()
        });
    }

    fn fill(window: &mut Window, cx: &mut App, id: &'static str, text: &str) {
        window.click(id, cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.input(text, cx);
        window.render_frame(cx);
    }

    fn worklog_lines(window: &Window) -> Vec<String> {
        (0..5)
            .map_while(|index| window.try_find(format!("worklog-{index}")))
            .map(|worklog| worklog.label().unwrap().to_owned())
            .collect()
    }

    fn error(window: &Window) -> Option<String> {
        window
            .try_find("worklog-error")
            .map(|error| error.label().unwrap().to_owned())
    }

    #[gpui_kit::test]
    fn the_form_is_in_view_on_an_issue_with_a_long_description_and_many_comments(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let mut long = issue(1, "Fix login");
        let paragraph = json!({
            "type": "paragraph",
            "content": [{ "type": "text", "text": "A sentence that is repeated until it needs several lines. ".repeat(8) }],
        });
        long["fields"]["description"] =
            json!({ "type": "doc", "version": 1, "content": vec![paragraph; 40] });
        let comments: Vec<_> = (0..20)
            .map(|index| {
                json!({
                    "id": index.to_string(),
                    "author": { "accountId": "7c20b", "displayName": "Grace Hopper" },
                    "body": { "type": "doc", "version": 1, "content": [{
                        "type": "paragraph",
                        "content": [{ "type": "text", "text": "Seen on staging" }],
                    }] },
                    "created": "2026-03-28T09:15:00.000+0100",
                    "updated": "2026-03-28T09:15:00.000+0100",
                })
            })
            .collect();
        long["fields"]["comment"] = json!({ "total": 20, "comments": comments });
        let data_dir = synced_on(&tokio, &server.uri(), &[long]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            assert!(!window.find("comment-0").visible());
            for id in ["worklog-duration", "worklog-started", "submit-worklog"] {
                assert!(window.find(id).visible(), "{id} is out of view");
            }
            fill(window, cx, "worklog-duration", "soon");
            window.click("submit-worklog", cx);
            window.render_frame(cx);
            assert!(error(window).is_some());
        });
    }

    #[gpui_kit::test]
    fn a_zero_or_invalid_time_is_refused_with_a_message_and_logs_nothing(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            window.click("submit-worklog", cx);
            window.render_frame(cx);
            assert_eq!(
                error(window).as_deref(),
                Some("Enter the time like 1h 30m, 45m or 2h")
            );

            fill(window, cx, "worklog-duration", "0m");
            window.click("submit-worklog", cx);
            window.render_frame(cx);
            assert_eq!(
                error(window).as_deref(),
                Some("The time must be more than zero")
            );

            fill(window, cx, "worklog-duration", "soon");
            window.click("submit-worklog", cx);
            window.render_frame(cx);
            assert_eq!(
                error(window).as_deref(),
                Some("Enter the time like 1h 30m, 45m or 2h")
            );

            fill(window, cx, "worklog-duration", "1h 30m");
            fill(window, cx, "worklog-started", "yesterday");
            window.click("submit-worklog", cx);
            window.render_frame(cx);
            assert_eq!(
                error(window).as_deref(),
                Some("Enter the start like 2026-03-29 10:15")
            );

            assert!(window.try_find("no-worklogs").is_some());
            assert_eq!(worklog_lines(window), Vec::<String>::new());
        });
    }

    #[gpui_kit::test]
    fn a_submitted_entry_is_in_the_list_at_once_as_pending(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            fill(window, cx, "worklog-duration", "1h 30m");
            fill(window, cx, "worklog-started", "2026-03-29 10:15");
            fill(window, cx, "worklog-comment-input", "Planning meeting");
            window.click("submit-worklog", cx);
        });

        let started =
            local_minute(crate::duration::parse_local_minute("2026-03-29 10:15").unwrap());
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {started}, 1h 30m, pending")]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(
                painted(window, ("worklog-comment", 0usize)),
                "Planning meeting"
            );
            assert_eq!(error(window), None);
            // The form is ready for the next entry
            assert_eq!(window.find("worklog-duration").value(), Some(""));
            assert_eq!(window.find("worklog-comment-input").value(), Some(""));
        });
    }
}
