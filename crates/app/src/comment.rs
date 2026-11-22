//! The input under the comments of an issue, for a new comment or an answer
//! to one of them.

use crate::keys;
use crate::runtime::spawn;
use app_core::{api, models::IssueKey};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    input::{InputEvent, Textarea, TextareaState},
    notification::Notification,
};
use gpui_kit::*;

/// The comment an answer is written to.
#[derive(Clone)]
pub struct ReplyTarget {
    /// `Comment::id`.
    pub comment_id: String,
    pub author_name: String,
}

pub struct CommentForm {
    key: IssueKey,
    // Kept until the answer is queued or the user drops it
    reply_to: Option<ReplyTarget>,
    input: Entity<TextareaState>,
    // The text stays in the input until the core has queued it, so a failed
    // write loses nothing; the flag keeps a second click from queueing it twice
    sending: bool,
    _typing: Subscription,
}

impl CommentForm {
    pub fn new(key: IssueKey, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10));
        let _typing = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            // Enter alone is a new line of the comment
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.submit(window, cx);
            }
            cx.notify()
        });
        Self {
            key,
            reply_to: None,
            input,
            sending: false,
            _typing,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// What is written from now on answers `target`.
    pub fn reply_to(&mut self, target: ReplyTarget, cx: &mut Context<Self>) {
        self.reply_to = Some(target);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if self.sending || text.trim().is_empty() {
            return;
        }
        self.sending = true;
        cx.notify();
        let key = self.key.clone();
        let reply_to = self.reply_to.clone();
        let write = spawn(cx, move |state| async move {
            match reply_to {
                Some(target) => {
                    api::reply_to_comment(&state, &key, &target.comment_id, &text).await
                }
                None => api::add_comment(&state, &key, &text).await,
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = write.await;
            this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match saved {
                    // The core announces the issue and the model reloads it, pending comment included
                    Ok(()) => {
                        this.reply_to = None;
                        this.input
                            .update(cx, |input, cx| input.set_value("", window, cx))
                    }
                    Err(error) => window.push_notification(
                        Notification::error(format!("Could not save the comment: {error}")),
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

impl Render for CommentForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.input.read(cx).value().trim().is_empty();
        let replying = self.reply_to.as_ref().map(|target| {
            let label = format!("Replying to {}", target.author_name);
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("replying-to")
                        .test_support()
                        .aria_label(label.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(
                    Button::new("cancel-reply")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reply_to = None;
                            cx.notify();
                        })),
                )
        });
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .children(replying)
            .child(
                div()
                    .id("comment-input")
                    .test_support()
                    .w_full()
                    .relative()
                    .child(Textarea::new(&self.input).aria_label("New comment"))
                    // Only over an empty input: the corner is where the first
                    // line of a comment ends
                    .children(
                        keys::hint(&keys::WriteComment, cx)
                            .filter(|_| empty)
                            .map(|hint| div().absolute().top_1p5().right_1p5().child(hint)),
                    ),
            )
            .child(
                Button::new("submit-comment")
                    .primary()
                    .small()
                    .label(if self.reply_to.is_some() {
                        "Reply"
                    } else {
                        "Add comment"
                    })
                    .disabled(empty || self.sending)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::{
        in_window, launch, no_worklog_changes, tab_to, tokio_runtime, wait_until,
    };
    use crate::issue_detail::tests::painted;
    use crate::issue_list::tests::{issue, synced_on};
    use crate::runtime::Core;
    use app_core::{AppState, api};
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, ScrollDelta, TestAppContext, Window, point, px};
    use serde_json::{Value, json};
    use std::{net::TcpListener, sync::Arc};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// What Jira sends for the comment of Ada Lovelace.
    fn jira_comment() -> Value {
        json!({
            "id": "10100",
            "author": { "accountId": "5b10a", "displayName": "Ada Lovelace" },
            "created": "2026-03-30T10:00:00.000+0100",
            "body": { "type": "doc", "version": 1, "content": [{
                "type": "paragraph",
                "content": [
                    { "type": "text", "text": "Seen on staging" },
                    { "type": "hardBreak" },
                    { "type": "text", "text": "and in production" },
                ],
            }] },
        })
    }

    /// WEB-1 as the next poll finds it once Jira has the comment.
    fn commented() -> Value {
        let mut commented = issue(1, "Fix login");
        commented["fields"]["updated"] = json!("2026-03-30T10:00:00.000+0100");
        commented["fields"]["comment"] = json!({ "total": 1, "comments": [jira_comment()] });
        commented
    }

    /// A mock Jira on `listener` whose searches find `issues`, and that
    /// accepts comments on WEB-1.
    async fn jira(listener: TcpListener, issues: Value) -> MockServer {
        let server = MockServer::builder().listener(listener).start().await;
        no_worklog_changes().mount(&server).await;
        crate::harness::no_sprint_field().mount(&server).await;
        no_replies().mount(&server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": issues })))
            .mount(&server)
            .await;
        server
    }

    /// Jira asked which comments of a search page are replies: none is.
    fn no_replies() -> Mock {
        Mock::given(method("POST"))
            .and(path("/rest/api/3/comment/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "values": [] })))
    }

    async fn accept_comments(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/comment"))
            .respond_with(ResponseTemplate::new(201).set_body_json(jira_comment()))
            .mount(server)
            .await;
    }

    fn local_port() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").unwrap()
    }

    fn core(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| cx.global::<Core>().state.clone())
    }

    fn open_web_1(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("issue-WEB-1").is_some()
        });
        in_window(cx, window, |window, cx| window.click("issue-WEB-1", cx));
        wait_until(cx, window, |window| {
            window.try_find("comment-input").is_some()
        });
        // The form is below the worklog form and the description, beyond the 600 px of the window
        in_window(cx, window, |window, cx| {
            window.scroll(
                "issue-detail",
                ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                cx,
            );
        });
    }

    fn write(window: &mut Window, cx: &mut gpui_kit::App, lines: &[&str]) {
        window.click("comment-input", cx);
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                window.press("enter", cx);
            }
            window.input(line, cx);
        }
        window.render_frame(cx);
    }

    fn can_submit(window: &Window) -> bool {
        window.find("submit-comment").disabled() != Some(true)
    }

    /// The headings of the comments on screen.
    fn headings(window: &Window) -> Vec<String> {
        (0..5)
            .map_while(|index| window.try_find(format!("comment-{index}")))
            .map(|comment| comment.label().unwrap().to_owned())
            .collect()
    }

    fn posts(tokio: &tokio::runtime::Runtime, server: &MockServer) -> Vec<Value> {
        let sent = tokio.block_on(server.received_requests()).unwrap();
        sent.iter()
            .filter(|request| request.url.path() == "/rest/api/3/issue/WEB-1/comment")
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    #[gpui_kit::test]
    fn an_empty_comment_cannot_be_submitted(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            assert!(!can_submit(window));

            write(window, cx, &["  ", ""]);
            assert!(!can_submit(window));

            // A click on the disabled button queues nothing
            window.click("submit-comment", cx);
            window.render_frame(cx);
            assert_eq!(headings(window), Vec::<String>::new());
            assert!(window.try_find("no-comments").is_some());

            write(window, cx, &["Seen on staging"]);
            assert!(can_submit(window));
        });
    }

    #[gpui_kit::test]
    fn a_comment_is_pending_until_jira_confirms_it_and_is_not_shown_twice(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        tokio.block_on(accept_comments(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            write(window, cx, &["Seen on staging", "and in production"]);
            window.click("submit-comment", cx);
        });

        wait_until(cx, window, |window| headings(window) == ["You, pending"]);
        in_window(cx, window, |window, _| {
            assert_eq!(
                painted(window, ("comment-body", 0usize)),
                "Seen on staging\nand in production"
            );
            // The input is ready for the next comment
            assert!(!can_submit(window));
        });

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| {
            headings(window) == ["Ada Lovelace, 2026-03-30 10:00"]
        });
        assert_eq!(
            posts(&tokio, &server),
            [json!({ "body": { "type": "doc", "version": 1, "content": [{
                "type": "paragraph",
                "content": [
                    { "type": "text", "text": "Seen on staging" },
                    { "type": "hardBreak" },
                    { "type": "text", "text": "and in production" },
                ],
            }] } })]
        );

        // The next poll finds the issue with the comment Jira stored
        tokio.block_on(server.reset());
        tokio.block_on(async {
            no_worklog_changes().mount(&server).await;
            crate::harness::no_sprint_field().mount(&server).await;
            no_replies().mount(&server).await;
            Mock::given(path("/rest/api/3/search/jql"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({ "issues": [commented()] })),
                )
                .mount(&server)
                .await;
        });
        tokio.block_on(state.sync_once()).unwrap();
        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some("WEB-1")
        });
        in_window(cx, window, |window, _| {
            assert_eq!(headings(window), ["Ada Lovelace, 2026-03-30 10:00"]);
            assert_eq!(window.find("detail-comments").label(), Some("Comments"));
        });
    }

    /// WEB-1 with a comment of Ada, an answer of Grace to it and a comment of Grace.
    fn discussed() -> Value {
        let mut answer = jira_comment();
        answer["id"] = json!("10101");
        answer["author"]["displayName"] = json!("Grace Hopper");
        answer["parentId"] = json!("10100");
        let mut other = jira_comment();
        other["id"] = json!("10102");
        other["author"]["displayName"] = json!("Grace Hopper");
        let mut discussed = issue(1, "Fix login");
        discussed["fields"]["comment"] =
            json!({ "total": 3, "comments": [jira_comment(), other, answer] });
        discussed
    }

    const ADA: &str = "Ada Lovelace, 2026-03-30 10:00";
    const GRACE: &str = "Grace Hopper, 2026-03-30 10:00";
    const GRACE_TO_ADA: &str = "Grace Hopper, 2026-03-30 10:00, reply to Ada Lovelace";

    #[gpui_kit::test]
    fn a_reply_is_shown_under_its_comment_and_sent_into_the_thread_of_that_one(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([discussed()])));
        // Jira answers with the reply as it stored it
        let mut stored = jira_comment();
        stored["id"] = json!("10103");
        stored["parentId"] = json!(10100);
        tokio.block_on(
            Mock::given(method("POST"))
                .and(path("/rest/api/3/issue/WEB-1/comment"))
                .respond_with(ResponseTemplate::new(201).set_body_json(stored))
                .mount(&server),
        );
        let data_dir = synced_on(&tokio, &server.uri(), &[discussed()]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            // A reply made in Jira follows the comment it answers
            assert_eq!(headings(window), [ADA, GRACE_TO_ADA, GRACE]);
            assert!(window.try_find("replying-to").is_none());
            assert_eq!(window.find("submit-comment").label(), Some("Add comment"));

            // An answer to a reply joins the thread of Ada's comment
            window.click("reply-comment-1", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("replying-to").label(),
                Some("Replying to Grace Hopper")
            );
            assert_eq!(window.find("submit-comment").label(), Some("Reply"));
            // The cursor is in the input
            window.input("Which build?", cx);
            window.render_frame(cx);
            window.click("submit-comment", cx);
        });

        let pending = "You, pending, reply to Ada Lovelace";
        wait_until(cx, window, |window| {
            headings(window) == [ADA, GRACE_TO_ADA, pending, GRACE]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(painted(window, ("comment-body", 2usize)), "Which build?");
            // What Jira does not have cannot be answered
            assert!(window.try_find("reply-comment-2").is_none());
            // The next comment is not a reply
            assert!(window.try_find("replying-to").is_none());
            assert_eq!(window.find("submit-comment").label(), Some("Add comment"));
        });

        tokio.block_on(core(cx).sync_once()).unwrap();

        let sent = "Ada Lovelace, 2026-03-30 10:00, reply to Ada Lovelace";
        wait_until(cx, window, |window| {
            headings(window) == [ADA, GRACE_TO_ADA, sent, GRACE]
        });
        let posts = posts(&tokio, &server);
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["parentId"], "10100");
        assert_eq!(
            posts[0]["body"]["content"][0]["content"][0]["text"],
            "Which build?"
        );
    }

    #[gpui_kit::test]
    fn a_reply_that_is_cancelled_is_sent_as_a_comment_of_its_own(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([discussed()])));
        tokio.block_on(accept_comments(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[discussed()]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            window.click("reply-comment-0", cx);
            window.render_frame(cx);
            window.input("Seen on staging", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("replying-to").label(),
                Some("Replying to Ada Lovelace")
            );
            window.click("cancel-reply", cx);
            window.render_frame(cx);
            assert!(window.try_find("replying-to").is_none());
            // The text stays
            assert!(can_submit(window));
            window.click("submit-comment", cx);
        });

        wait_until(cx, window, |window| {
            headings(window) == [ADA, GRACE_TO_ADA, GRACE, "You, pending"]
        });
        tokio.block_on(core(cx).sync_once()).unwrap();
        let posts = posts(&tokio, &server);
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].get("parentId"), None);
    }

    #[gpui_kit::test]
    fn a_reply_jira_refuses_stays_in_its_thread_and_can_be_retried(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([discussed()])));
        tokio.block_on(refuse_one_comment(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[discussed()]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        in_window(cx, window, |window, cx| {
            window.click("reply-comment-0", cx);
            window.render_frame(cx);
            window.input("Which build?", cx);
            window.render_frame(cx);
            window.click("submit-comment", cx);
        });
        let pending = "You, pending, reply to Ada Lovelace";
        wait_until(cx, window, |window| {
            headings(window) == [ADA, GRACE_TO_ADA, pending, GRACE]
        });
        tokio.block_on(core(cx).sync_once()).unwrap();
        let refused = "You, not sent, reply to Ada Lovelace";
        wait_until(cx, window, |window| {
            headings(window) == [ADA, GRACE_TO_ADA, refused, GRACE]
        });

        in_window(cx, window, |window, cx| window.click("retry-comment-2", cx));
        wait_until(cx, window, |window| {
            headings(window) == [ADA, GRACE_TO_ADA, pending, GRACE]
        });
        tokio.block_on(core(cx).sync_once()).unwrap();
        let posts = posts(&tokio, &server);
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[1]["parentId"], "10100");
    }

    #[gpui_kit::test]
    fn a_comment_written_offline_is_delivered_after_reconnect(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let port = local_port();
        let address = port.local_addr().unwrap();
        let server = tokio.block_on(jira(port, json!([issue(1, "Fix login")])));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        // Jira goes away: nothing listens on its port any more
        drop(server);
        in_window(cx, window, |window, cx| {
            write(window, cx, &["Written on the train"]);
            window.click("submit-comment", cx);
        });
        wait_until(cx, window, |window| headings(window) == ["You, pending"]);
        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap_err();
        wait_until(cx, window, |window| headings(window) == ["You, pending"]);

        // Jira is back at the same address
        let server = tokio.block_on(jira(
            TcpListener::bind(address).unwrap(),
            json!([issue(1, "Fix login")]),
        ));
        tokio.block_on(accept_comments(&server));
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| {
            headings(window) == ["Ada Lovelace, 2026-03-30 10:00"]
        });
        let sent = posts(&tokio, &server);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0]["body"]["content"][0]["content"][0]["text"],
            "Written on the train"
        );
    }

    /// Jira refuses the first comment on WEB-1 and takes every later one.
    async fn refuse_one_comment(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/comment"))
            .respond_with(ResponseTemplate::new(403).set_body_string("No permission"))
            .up_to_n_times(1)
            .mount(server)
            .await;
        accept_comments(server).await;
    }

    /// Writes "Seen on staging", sends it and waits until Jira has refused it.
    fn comment_that_jira_refuses(
        cx: &mut TestAppContext,
        tokio: &tokio::runtime::Runtime,
        window: AnyWindowHandle,
    ) {
        in_window(cx, window, |window, cx| {
            write(window, cx, &["Seen on staging"]);
            window.click("submit-comment", cx);
        });
        wait_until(cx, window, |window| headings(window) == ["You, pending"]);
        tokio.block_on(core(cx).sync_once()).unwrap();
        wait_until(cx, window, |window| headings(window) == ["You, not sent"]);
    }

    #[gpui_kit::test]
    fn a_comment_jira_refuses_stays_with_its_text_and_can_be_retried(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        tokio.block_on(refuse_one_comment(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);

        comment_that_jira_refuses(cx, &tokio, window);

        in_window(cx, window, |window, cx| {
            assert_eq!(painted(window, ("comment-body", 0usize)), "Seen on staging");
            assert_eq!(
                window.find("comment-reason-0").label(),
                Some("Not sent: Jira returned 403 Forbidden: No permission")
            );
            // The notification of the rejection is the only one
            assert_eq!(window.notifications(cx).len(), 1);
            window.click("retry-comment-0", cx);
        });
        wait_until(cx, window, |window| headings(window) == ["You, pending"]);
        tokio.block_on(core(cx).sync_once()).unwrap();

        wait_until(cx, window, |window| {
            headings(window) == ["Ada Lovelace, 2026-03-30 10:00"]
        });
        assert_eq!(posts(&tokio, &server).len(), 2);
    }

    #[gpui_kit::test]
    fn a_comment_on_an_issue_that_left_the_cache_keeps_its_text_and_reports_the_error(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);
        in_window(cx, window, |window, cx| {
            write(window, cx, &["Seen on staging"])
        });

        // The project of the issue is unwatched while its detail is open. A
        // second core on the same data does it, so that the open detail is not
        // told and still offers the form
        let (other, _events) = tokio
            .block_on(app_core::start_without_keychain(data_dir.path()))
            .unwrap();
        tokio
            .block_on(api::watch_projects(&other, &["API".to_owned()]))
            .unwrap();
        in_window(cx, window, |window, cx| {
            assert_eq!(window.notifications(cx).len(), 0);
            window.click("submit-comment", cx);
        });

        wait_until(cx, window, |window| {
            window.try_find("notification").is_some()
        });
        in_window(cx, window, |window, cx| {
            assert_eq!(window.notifications(cx).len(), 1);
            assert!(can_submit(window));
        });
        let state = core(cx);
        let queued = tokio.block_on(async { api::unsent_changes(&state).await.unwrap() });
        assert_eq!(queued, 0);
    }

    #[gpui_kit::test]
    fn a_comment_jira_refuses_can_be_discarded(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        tokio.block_on(refuse_one_comment(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);
        comment_that_jira_refuses(cx, &tokio, window);

        in_window(cx, window, |window, cx| {
            window.click("discard-comment-0", cx)
        });

        wait_until(cx, window, |window| {
            window.try_find("no-comments").is_some()
        });
        // Nothing is left to send
        tokio.block_on(core(cx).sync_once()).unwrap();
        assert_eq!(posts(&tokio, &server).len(), 1);
    }

    #[gpui_kit::test]
    fn a_comment_jira_refuses_is_discarded_with_tab_and_enter(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        tokio.block_on(refuse_one_comment(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);
        comment_that_jira_refuses(cx, &tokio, window);

        tab_to(cx, window, "discard-comment-0");
        in_window(cx, window, |window, cx| window.press("enter", cx));

        wait_until(cx, window, |window| {
            window.try_find("no-comments").is_some()
        });
    }

    #[gpui_kit::test]
    fn a_comment_jira_refuses_is_retried_with_tab_and_space(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port(), json!([issue(1, "Fix login")])));
        tokio.block_on(refuse_one_comment(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);
        comment_that_jira_refuses(cx, &tokio, window);

        tab_to(cx, window, "retry-comment-0");
        in_window(cx, window, |window, cx| window.press("space", cx));

        wait_until(cx, window, |window| headings(window) == ["You, pending"]);
    }

    #[gpui_kit::test]
    fn a_comment_under_a_long_thread_is_written_and_sent_from_the_keyboard(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let mut long = issue(1, "Fix login");
        let thread = vec![jira_comment(); 30];
        long["fields"]["comment"] = json!({ "total": 30, "comments": thread });
        let server = tokio.block_on(jira(local_port(), json!([long])));
        let data_dir = synced_on(&tokio, &server.uri(), &[long]);
        let window = launch(cx, &tokio, data_dir.path());
        open_web_1(cx, window);
        wait_until(cx, window, |window| {
            window.try_find("comment-input").is_some()
        });
        let in_view = |window: &Window| {
            let pane = window.find("issue-detail").bounds();
            let input = window.find("comment-input").bounds();
            pane.top() <= input.top() && input.bottom() <= pane.bottom()
        };
        // The description is laid out a moment after the pane opens
        in_window(cx, window, |window, cx| {
            let to_the_top = ScrollDelta::Pixels(point(px(0.), px(100_000.)));
            window.scroll("issue-detail", to_the_top, cx);
            window.render_frame(cx);
            assert!(!in_view(window));
        });

        in_window(cx, window, |window, cx| {
            window.press("c", cx);
            window.render_frame(cx);
            assert!(in_view(window));
            window.input("Seen on staging", cx);
            // Enter alone is a new line
            window.press("enter", cx);
            window.input("and in production", cx);
        });
        cx.run_until_parked();
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("comment-30").is_none());
            window.press("cmd-enter", cx);
        });

        wait_until(cx, window, |window| {
            window
                .try_find("comment-30")
                .is_some_and(|comment| comment.label() == Some("You, pending"))
        });
        in_window(cx, window, |window, _| {
            assert_eq!(
                painted(window, ("comment-body", 30usize)),
                "Seen on staging\nand in production"
            );
        });
    }
}
