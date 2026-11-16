//! The pane beside the list: one issue with its description and comments.

use crate::issue::IssueModel;
use app_core::models::{Comment, IssueDetail, StatusCategory};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    text::TextView,
};
use gpui_kit::*;

/// Emitted when the user closes the pane.
pub struct DetailClosed;

pub struct DetailPane {
    issue: Entity<IssueModel>,
    _issue_changed: Subscription,
}

impl EventEmitter<DetailClosed> for DetailPane {}

impl DetailPane {
    pub fn new(issue: Entity<IssueModel>, cx: &mut Context<Self>) -> Self {
        let _issue_changed = cx.observe(&issue, |_, _, cx| cx.notify());
        Self {
            issue,
            _issue_changed,
        }
    }
}

/// A line of the field list. Tests read it as "Name: value".
fn field(id: &'static str, name: &str, value: &str, color: Hsla, cx: &App) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .aria_label(format!("{name}: {value}"))
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(80.))
                .flex_none()
                .text_color(cx.theme().muted_foreground)
                .child(name.to_owned()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(color)
                .child(value.to_owned()),
        )
}

/// "2026-03-29 01:30" from Jira's `2026-03-29T01:30:00.000+0100`. Jira sends the
/// time in the time zone of the account, so it is shown as it came.
fn minute(created: &str) -> String {
    created.get(..16).unwrap_or(created).replace('T', " ")
}

fn comment(index: usize, comment: &Comment, cx: &App) -> impl IntoElement {
    let heading = format!("{}, {}", comment.author_name, minute(&comment.created));
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("comment-{index}")))
                .test_support()
                .aria_label(heading.clone())
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(heading),
        )
        .child(TextView::markdown(
            ("comment-body", index),
            comment.body_md.clone(),
        ))
}

fn body(issue: &IssueDetail, cx: &App) -> impl IntoElement {
    let foreground = cx.theme().foreground;
    let muted = cx.theme().muted_foreground;
    let status_color = match issue.status_category {
        StatusCategory::Done => muted,
        StatusCategory::Todo | StatusCategory::InProgress => foreground,
    };
    let or_none = |value: &Option<String>| value.clone().unwrap_or_else(|| "None".to_owned());
    let labels = if issue.labels.is_empty() {
        "None".to_owned()
    } else {
        issue.labels.join(", ")
    };
    let assignee = issue.assignee_name.as_deref().unwrap_or("Unassigned");
    let comments_heading = if issue.comments.len() < issue.comment_total {
        // Jira sends only part of a long thread with a search page
        format!(
            "Comments ({} of {})",
            issue.comments.len(),
            issue.comment_total
        )
    } else {
        "Comments".to_owned()
    };

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .id("detail-summary")
                .test_support()
                .aria_label(issue.summary.clone())
                .text_xl()
                .child(issue.summary.clone()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .text_sm()
                .child(field(
                    "detail-status",
                    "Status",
                    &issue.status,
                    status_color,
                    cx,
                ))
                .child(field(
                    "detail-assignee",
                    "Assignee",
                    assignee,
                    foreground,
                    cx,
                ))
                .child(field(
                    "detail-reporter",
                    "Reporter",
                    &or_none(&issue.reporter_name),
                    foreground,
                    cx,
                ))
                .child(field(
                    "detail-type",
                    "Type",
                    &or_none(&issue.issue_type),
                    foreground,
                    cx,
                ))
                .child(field(
                    "detail-priority",
                    "Priority",
                    &or_none(&issue.priority),
                    foreground,
                    cx,
                ))
                .child(field("detail-labels", "Labels", &labels, foreground, cx)),
        )
        .child(if issue.description_md.is_empty() {
            div()
                .id("no-description")
                .test_support()
                .text_color(muted)
                .child("No description")
                .into_any_element()
        } else {
            TextView::markdown("description", issue.description_md.clone()).into_any_element()
        })
        .child(
            div()
                .id("detail-comments")
                .test_support()
                .aria_label(comments_heading.clone())
                .text_lg()
                .child(comments_heading),
        )
        .children(issue.comments.is_empty().then(|| {
            div()
                .id("no-comments")
                .test_support()
                .text_color(muted)
                .child("No comments")
        }))
        .children(
            issue
                .comments
                .iter()
                .enumerate()
                .map(|(index, each)| comment(index, each, cx)),
        )
}

impl Render for DetailPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let close = Button::new("close-detail")
            .ghost()
            .small()
            .label("Close")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailClosed)));
        let issue = self.issue.read(cx);
        let open_in_jira = issue.detail.as_ref().map(|detail| {
            let url = detail.url.clone();
            Button::new("open-in-jira")
                .small()
                .label("Open in Jira")
                .on_click(move |_, _, cx| cx.open_url(&url))
        });

        div()
            .id("issue-detail")
            .test_support()
            .aria_label(issue.key.to_string())
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pb_3()
                    .child(
                        div()
                            .flex_1()
                            .text_color(cx.theme().muted_foreground)
                            .child(issue.key.to_string()),
                    )
                    .children(open_in_jira)
                    .child(close),
            )
            .children(issue.detail.as_ref().map(|detail| body(detail, cx)))
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::{in_window, launch, tokio_runtime, wait_until};
    use crate::issue_list::tests::{issue, rows, sync_brings, synced};
    use gpui_kit::base::test_support::texts;
    use gpui_kit::test::{TestEventExt, TestWindowExt};
    use gpui_kit::{
        AnyWindowHandle, ElementId, MouseButton, ScrollDelta, TestAppContext, Window, point, px,
    };
    use serde_json::{Value, json};

    fn doc(content: Value) -> Value {
        json!({ "type": "doc", "version": 1, "content": content })
    }

    fn paragraph(text: &str) -> Value {
        json!({ "type": "paragraph", "content": [{ "type": "text", "text": text }] })
    }

    fn comment(author: &str, created: &str, text: &str) -> Value {
        json!({
            "author": { "accountId": author, "displayName": author },
            "created": created,
            "body": doc(json!([paragraph(text)])),
        })
    }

    /// WEB-1 with every field the detail shows.
    fn login_bug() -> Value {
        let mut bug = issue(1, "Fix login");
        let fields = &mut bug["fields"];
        fields["description"] = doc(json!([
            {
                "type": "paragraph",
                "content": [
                    { "type": "text", "text": "Login " },
                    { "type": "text", "text": "fails", "marks": [{ "type": "strong" }] },
                    { "type": "text", "text": " on Safari" },
                ],
            },
            {
                "type": "bulletList",
                "content": [
                    { "type": "listItem", "content": [paragraph("Open the page")] },
                    { "type": "listItem", "content": [paragraph("Press Sign in")] },
                ],
            },
        ]));
        fields["status"] =
            json!({ "name": "In Review", "statusCategory": { "key": "indeterminate" } });
        fields["assignee"] = json!({ "accountId": "5b10a", "displayName": "Ada Lovelace" });
        fields["reporter"] = json!({ "accountId": "7c20b", "displayName": "Grace Hopper" });
        fields["issuetype"] = json!({ "name": "Bug" });
        fields["priority"] = json!({ "name": "High" });
        fields["labels"] = json!(["auth", "regression"]);
        fields["comment"] = json!({
            "total": 2,
            "comments": [
                comment("Grace Hopper", "2026-03-28T09:15:00.000+0100", "Seen on staging"),
                comment("Ada Lovelace", "2026-03-29T01:30:00.000+0100", "Cannot reproduce"),
            ],
        });
        bug
    }

    fn open(cx: &mut TestAppContext, window: AnyWindowHandle, key: &'static str) {
        let row = format!("issue-{key}");
        wait_until(cx, window, |window| window.try_find(row.clone()).is_some());
        in_window(cx, window, |window, cx| window.click(row.clone(), cx));
        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some(key)
                && window.try_find("detail-summary").is_some()
        });
    }

    fn label(window: &Window, id: &'static str) -> String {
        window.find(id).label().unwrap().to_owned()
    }

    /// The text that the Markdown view `id` painted, one line per block.
    fn painted(window: &Window, id: impl Into<ElementId>) -> String {
        let id = id.into();
        let texts: Vec<String> = texts(window)
            .iter()
            .filter(|text| text.path().contains(&id))
            .map(|text| text.text().to_owned())
            .collect();
        texts.join("\n")
    }

    #[gpui_kit::test]
    fn selecting_an_issue_in_the_list_opens_its_detail(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[login_bug(), issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());

        open(cx, window, "WEB-1");

        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "detail-summary"), "Fix login");
            assert_eq!(label(window, "detail-status"), "Status: In Review");
            assert_eq!(label(window, "detail-assignee"), "Assignee: Ada Lovelace");
            assert_eq!(label(window, "detail-reporter"), "Reporter: Grace Hopper");
            assert_eq!(label(window, "detail-type"), "Type: Bug");
            assert_eq!(label(window, "detail-priority"), "Priority: High");
            assert_eq!(label(window, "detail-labels"), "Labels: auth, regression");
            assert_eq!(
                painted(window, "description"),
                "Login fails on Safari\nOpen the page\nPress Sign in"
            );
            assert_eq!(label(window, "detail-comments"), "Comments");
            assert_eq!(label(window, "comment-0"), "Grace Hopper, 2026-03-28 09:15");
            assert_eq!(painted(window, ("comment-body", 0usize)), "Seen on staging");
            assert_eq!(label(window, "comment-1"), "Ada Lovelace, 2026-03-29 01:30");
            assert_eq!(
                painted(window, ("comment-body", 1usize)),
                "Cannot reproduce"
            );
            // The list stays beside the issue
            assert_eq!(rows(window).len(), 2);
        });
    }

    #[gpui_kit::test]
    fn a_long_issue_wraps_inside_the_pane_and_scrolls(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut bug = login_bug();
        let long_line = "A sentence that is repeated until it needs several lines. ".repeat(8);
        let paragraphs: Vec<Value> = (0..40).map(|_| paragraph(long_line.trim())).collect();
        bug["fields"]["description"] = doc(json!(paragraphs));
        let data_dir = synced(&tokio, &[bug]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| {
            let pane = window.find("issue-detail").bounds();
            // The list takes 320 px and the border between the two takes one
            assert_eq!((pane.left(), pane.right()), (px(321.), px(800.)));
            for text in texts(window) {
                let line = text.bounds();
                assert!(
                    line.left() >= pane.left() && line.right() <= pane.right(),
                    "{line:?} is outside {pane:?}"
                );
            }
            assert!(!window.find("comment-0").visible());

            window.scroll(
                "issue-detail",
                ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                cx,
            );
            assert!(window.find("comment-1").visible());
        });
    }

    #[gpui_kit::test]
    fn the_description_can_be_selected_and_copied(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut bug = issue(1, "Fix login");
        bug["fields"]["description"] = doc(json!([paragraph("Login fails on Safari")]));
        let data_dir = synced(&tokio, &[bug]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| {
            let line = texts(window)
                .into_iter()
                .find(|text| text.text() == "Login fails on Safari")
                .unwrap()
                .bounds();
            let middle = line.center().y;
            let (start, end) = (point(line.left(), middle), point(line.right(), middle));
            window.pointer_down(start, MouseButton::Left, cx);
            window.pointer_move(end, Some(MouseButton::Left), cx);
            window.pointer_up(end, MouseButton::Left, cx);
            window.press("secondary-c", cx);
        });

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("Login fails on Safari"));
    }

    #[gpui_kit::test]
    fn the_detail_follows_the_cache(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[login_bug(), issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        let mut resolved = login_bug();
        let fields = &mut resolved["fields"];
        fields["status"] = json!({ "name": "Done", "statusCategory": { "key": "done" } });
        fields["description"] = doc(json!([paragraph("Fixed in 2.1")]));
        let thanks = comment("Grace Hopper", "2026-03-30T10:00:00.000+0100", "Thanks");
        fields["comment"]["comments"]
            .as_array_mut()
            .unwrap()
            .push(thanks);
        fields["comment"]["total"] = json!(3);
        // The cache only takes an issue that Jira changed after the stored one
        fields["updated"] = json!("2026-03-30T10:00:00.000+0100");
        sync_brings(cx, &tokio, resolved);

        wait_until(cx, window, |window| {
            label(window, "detail-status") == "Status: Done"
        });
        in_window(cx, window, |window, _| {
            assert_eq!(painted(window, "description"), "Fixed in 2.1");
            assert_eq!(label(window, "comment-2"), "Grace Hopper, 2026-03-30 10:00");
            assert_eq!(painted(window, ("comment-body", 2usize)), "Thanks");
        });
    }

    #[gpui_kit::test]
    fn the_issue_can_be_opened_in_jira(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[login_bug()]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        assert_eq!(cx.opened_url(), None);

        in_window(cx, window, |window, cx| window.click("open-in-jira", cx));

        // `synced` signs in to http://localhost
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("http://localhost/browse/WEB-1")
        );
    }

    #[gpui_kit::test]
    fn an_issue_with_no_description_and_no_comments_is_shown(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        // Jira sends a null description and an empty comment list
        let mut bare = issue(1, "Fix login");
        bare["fields"]["comment"] = json!({ "total": 0, "comments": [] });
        // An issue cached before the app asked Jira for the other fields has none of them
        let data_dir = synced(&tokio, &[bare, issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());

        for key in ["WEB-1", "WEB-2"] {
            open(cx, window, key);
            in_window(cx, window, |window, _| {
                assert_eq!(label(window, "detail-status"), "Status: To Do");
                assert_eq!(label(window, "detail-assignee"), "Assignee: Unassigned");
                assert_eq!(label(window, "detail-reporter"), "Reporter: None");
                assert_eq!(label(window, "detail-priority"), "Priority: None");
                assert_eq!(label(window, "detail-labels"), "Labels: None");
                assert!(window.try_find("no-description").is_some());
                assert!(window.try_find("no-comments").is_some());
                assert!(window.try_find("comment-0").is_none());
                assert_eq!(painted(window, "description"), "");
            });
        }
    }

    #[gpui_kit::test]
    fn a_long_thread_says_how_many_comments_are_shown(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut bug = login_bug();
        bug["fields"]["comment"]["total"] = json!(40);
        let data_dir = synced(&tokio, &[bug]);
        let window = launch(cx, &tokio, data_dir.path());

        open(cx, window, "WEB-1");

        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "detail-comments"), "Comments (2 of 40)");
        });
    }

    #[gpui_kit::test]
    fn another_issue_replaces_the_open_one_and_closing_brings_the_list_back(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[login_bug(), issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        let beside_an_issue = in_window(cx, window, |window, _| {
            window.find("issue-WEB-1").bounds().size.width
        });

        open(cx, window, "WEB-2");
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "detail-summary"), "Fix logout");
            assert_eq!(painted(window, "description"), "");
            assert!(window.try_find("comment-0").is_none());
        });

        in_window(cx, window, |window, cx| window.click("close-detail", cx));
        in_window(cx, window, |window, _| {
            assert!(window.try_find("issue-detail").is_none());
            assert_eq!(rows(window).len(), 2);
            assert_eq!(beside_an_issue, px(320.));
            assert_eq!(window.find("issue-WEB-1").bounds().size.width, px(800.));
        });
    }
}
