//! The pane beside the list: one issue with its description and comments.

use crate::assignee::AssigneeButton;
use crate::comment::{CommentForm, ReplyTarget};
use crate::duration::{format_duration, local_minute};
use crate::issue::IssueModel;
use crate::keys;
use crate::runtime::{Core, spawn};
use crate::theme::{badge, pending_mark, tabular_digits};
use crate::timer::{ActiveTimer, TimerControl};
use crate::transition::StatusButton;
use crate::worklog::WorklogForm;
use app_core::{
    api,
    models::{Comment, IssueDetail, IssueKey, SyncStatus, Worklog},
};
use gpui_kit::base::{ElementExt as _, StyledExt as _};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    avatar::Avatar,
    button::{Button, ButtonVariants},
    text::{MarkdownExtensions, MarkdownNode, TextView, TextViewStyle, markdown_ast},
};
use gpui_kit::*;
use std::{cell::Cell, rc::Rc};

/// Emitted when the user closes the pane.
pub struct DetailClosed;

pub struct DetailPane {
    issue: Entity<IssueModel>,
    status: Entity<StatusButton>,
    assignee: Entity<AssigneeButton>,
    comment_form: Entity<CommentForm>,
    worklog_form: Entity<WorklogForm>,
    timer: Entity<TimerControl>,
    scroll: ScrollHandle,
    // Where the comment form was painted last, to scroll it into view
    comment_bounds: Rc<Cell<Bounds<Pixels>>>,
    _issue_changed: Subscription,
    // The marks of what Jira does not have yet differ during a sync cycle
    _sync_changed: Subscription,
}

impl EventEmitter<DetailClosed> for DetailPane {}

impl DetailPane {
    pub fn new(
        issue: Entity<IssueModel>,
        timer: Entity<ActiveTimer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _issue_changed = cx.observe(&issue, |_, _, cx| cx.notify());
        let sync = cx.global::<Core>().sync.clone();
        let _sync_changed = cx.observe(&sync, |_, _, cx| cx.notify());
        let key = issue.read(cx).key.clone();
        Self {
            status: cx.new(|cx| StatusButton::new(issue.clone(), cx)),
            assignee: cx.new(|cx| AssigneeButton::new(issue.clone(), cx)),
            comment_form: cx.new(|cx| CommentForm::new(key.clone(), window, cx)),
            worklog_form: cx.new(|cx| WorklogForm::new(key.clone(), window, cx)),
            timer: cx.new(|cx| TimerControl::new(key, timer, cx)),
            scroll: ScrollHandle::new(),
            comment_bounds: Rc::default(),
            issue,
            _issue_changed,
            _sync_changed,
        }
    }

    pub fn key(&self, cx: &App) -> IssueKey {
        self.issue.read(cx).key.clone()
    }

    pub fn change_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status
            .update(cx, |status, cx| status.open_menu(window, cx));
    }

    pub fn change_assignee(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.assignee
            .update(cx, |assignee, cx| assignee.open_menu(window, cx));
    }

    /// Puts the cursor into the comment input. Under a long description the
    /// input is out of view, and nothing scrolls to a focused element by itself.
    pub fn write_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_form
            .update(cx, |form, cx| form.focus(window, cx));
        let (pane, form) = (self.scroll.bounds(), self.comment_bounds.get());
        let below = (form.bottom() - pane.bottom()).max(px(0.));
        let above = (pane.top() - form.top()).max(px(0.));
        let mut offset = self.scroll.offset();
        // A form taller than the pane shows its top
        offset.y += if above > px(0.) { above } else { -below };
        self.scroll.set_offset(offset);
        cx.notify();
    }

    /// Makes the comment form write an answer to `target` and goes to it.
    fn reply_to(&mut self, target: ReplyTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.comment_form
            .update(cx, |form, cx| form.reply_to(target, cx));
        self.write_comment(window, cx);
    }

    pub fn log_time(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.worklog_form
            .update(cx, |form, cx| form.focus(window, cx));
        self.scroll.set_offset(Point::default());
        cx.notify();
    }

    pub fn open_in_jira(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(detail) = &self.issue.read(cx).detail {
            cx.open_url(&detail.url);
        }
    }
}

/// A line of the field list. Tests read it as "Name: value".
fn field(id: &'static str, name: &str, value: &str, cx: &App) -> impl IntoElement {
    let shown = div()
        .flex_1()
        .min_w_0()
        .text_color(cx.theme().foreground)
        .child(value.to_owned());
    field_line(id, name, value, cx).child(shown)
}

/// The line without its value: the caller adds the element that shows it.
fn field_line(
    id: &'static str,
    name: &str,
    value: &str,
    cx: &App,
) -> impl IntoElement + ParentElement {
    div()
        .id(id)
        .test_support()
        .aria_label(format!("{name}: {value}"))
        .min_h(px(24.))
        .min_w_0()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(72.))
                .flex_none()
                .text_color(cx.theme().muted_foreground)
                .child(name.to_owned()),
        )
}

/// "2026-03-29 01:30" from Jira's `2026-03-29T01:30:00.000+0100`. Jira sends the
/// time in the time zone of the account, so it is shown as it came.
fn minute(created: &str) -> String {
    created.get(..16).unwrap_or(created).replace('T', " ")
}

/// A description or a comment body. Code and tables that are wider than the
/// pane scroll sideways.
fn markdown(id: impl Into<ElementId>, text: &str) -> TextView {
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);
    let extensions = MarkdownExtensions::default()
        .block_parser(|node, _| match node {
            markdown_ast::Node::Code(code) => {
                Some(MarkdownNode::new("code", ()).markdown(fenced(code)))
            }
            _ => None,
        })
        .block_renderer("code", code_block)
        // The same parser on every render: the parsed document is kept
        .parser_revision(1);
    TextView::markdown(id, text.to_owned())
        .style(TextViewStyle::default().table(table))
        .markdown_extensions(extensions)
}

/// The block as Markdown of its own. Its source in the document cannot be used:
/// inside a list or a quote every line of it carries their indent or `> `.
fn fenced(code: &markdown_ast::Code) -> String {
    let backticks = code.value.split(|c| c != '`').map(str::len).max();
    let fence = "`".repeat(backticks.unwrap_or(0).max(2) + 1);
    let language = code.lang.as_deref().unwrap_or_default();
    format!("{fence}{language}\n{}\n{fence}", code.value)
}

// GPUI Kit 0.9.1 and 0.10.0 wrap long code lines and cannot scroll a code block
// sideways, so each block is a view of its own that is as wide as its longest
// line, inside a container that scrolls.
fn code_block(node: &MarkdownNode, _: &mut Window, _: &mut App) -> Stateful<Div> {
    let position = node.source_range().map_or(0, |range| range.start);
    let mut one_line_each = StyleRefinement::default();
    one_line_each.text.white_space = Some(WhiteSpace::Nowrap);
    let code = TextView::markdown(("code", position), node.as_markdown().to_owned())
        .style(TextViewStyle::default().code_block(one_line_each));
    div()
        .id(("code-scroll", position))
        .flex()
        .overflow_x_scroll()
        // Without it the wheel moves the code sideways
        .restrict_scroll_to_axis()
        .child(div().flex_none().child(code))
}

/// A comment or a worklog: the picture of its author, and beside it the
/// `lines`, the first of which is the heading.
fn entry(author: &str, lines: Div) -> impl IntoElement {
    div()
        .flex()
        .gap_2p5()
        .child(Avatar::new().name(author.to_owned()).small().flex_none())
        .child(lines.flex_1().min_w_0().flex().flex_col().gap_1())
}

/// The first line of an entry, with the author in it. Tests read `label`.
fn heading(id: String, label: String, author: &str) -> impl IntoElement + ParentElement {
    div()
        .id(SharedString::from(id))
        .test_support()
        .aria_label(label)
        .flex()
        .items_center()
        .gap_2()
        .child(div().font_semibold().child(author.to_owned()))
}

fn when(time: String, cx: &App) -> impl IntoElement {
    div()
        .text_size(px(12.))
        .font_features(tabular_digits())
        .text_color(cx.theme().muted_foreground)
        .child(time)
}

fn comment(
    index: usize,
    comment: &Comment,
    pane: WeakEntity<DetailPane>,
    cx: &App,
) -> impl IntoElement {
    let state = if comment.pending {
        Some("Pending")
    } else if comment.rejected.is_some() {
        Some("Not sent")
    } else {
        None
    };
    let time = minute(&comment.created);
    let mut label = match state {
        Some(state) => format!("{}, {}", comment.author_name, state.to_lowercase()),
        None => format!("{}, {time}", comment.author_name),
    };
    let answers = comment.reply_to.as_ref().map(|reply_to| {
        match &reply_to.author_name {
            Some(author) => format!("reply to {author}"),
            // The comment it answers is not among those shown
            None => "reply".to_owned(),
        }
    });
    if let Some(answers) = &answers {
        label = format!("{label}, {answers}");
    }
    let heading = heading(format!("comment-{index}"), label, &comment.author_name);
    let heading = match state {
        Some(_) if comment.pending => heading.child(pending_mark(cx)),
        Some(state) => heading.child(badge(state, cx)),
        None => heading.child(when(time, cx)),
    };
    let heading = heading.children(answers.map(|answers| when(answers, cx)));
    // Only a comment Jira has can be answered
    let reply = comment.id.clone().map(|comment_id| {
        let target = ReplyTarget {
            comment_id,
            author_name: comment.author_name.clone(),
        };
        div().flex().child(
            Button::new(SharedString::from(format!("reply-comment-{index}")))
                .ghost()
                .xsmall()
                .label("Reply")
                .on_click(move |_, window, cx| {
                    pane.update(cx, |pane, cx| pane.reply_to(target.clone(), window, cx))
                        .ok();
                }),
        )
    });
    // Jira has one level of replies, and so has the indent
    let in_thread = matches!(&comment.reply_to, Some(reply_to) if reply_to.author_name.is_some());
    let lines = div()
        .child(heading)
        .child(markdown(("comment-body", index), &comment.body_md))
        .children(reply)
        .children(comment.rejected.as_ref().map(|rejected| {
            refused(
                index,
                Refused::Comment(rejected.entry_id),
                &rejected.reason,
                cx,
            )
        }));
    let indent = if in_thread { px(32.) } else { px(0.) };
    div().pl(indent).child(entry(&comment.author_name, lines))
}

fn worklog(index: usize, worklog: &Worklog, cx: &App) -> impl IntoElement {
    let started = local_minute(worklog.started_at);
    let spent = format_duration(worklog.time_spent_secs);
    let state = match worklog.status {
        SyncStatus::Synced => None,
        SyncStatus::Pending => Some("Pending"),
        SyncStatus::Failed => Some("Not sent"),
    };
    let mut label = format!("{}, {started}, {spent}", worklog.author_name);
    if let Some(state) = state {
        label.push_str(&format!(", {}", state.to_lowercase()));
    }
    let heading = heading(format!("worklog-{index}"), label, &worklog.author_name)
        .child(when(started, cx))
        .children(state.map(|state| match worklog.status {
            SyncStatus::Pending => pending_mark(cx),
            _ => badge(state, cx).into_any_element(),
        }))
        .child(
            div()
                .ml_auto()
                .font_semibold()
                .font_features(tabular_digits())
                .child(spent),
        );
    let lines = div()
        .child(heading)
        .children(
            worklog
                .comment
                .as_ref()
                .map(|text| markdown(("worklog-comment", index), text)),
        )
        .children(worklog.rejected.as_ref().map(|rejected| {
            refused(
                index,
                Refused::Worklog(rejected.entry_id),
                &rejected.reason,
                cx,
            )
        }));
    entry(&worklog.author_name, lines)
}

/// A change that was refused, by its outbox entry.
#[derive(Clone, Copy)]
enum Refused {
    Comment(i64),
    Worklog(i64),
}

impl Refused {
    fn kind(self) -> &'static str {
        match self {
            Self::Comment(_) => "comment",
            Self::Worklog(_) => "worklog",
        }
    }

    async fn retry(self, state: &app_core::AppState) -> anyhow::Result<()> {
        match self {
            Self::Comment(entry_id) => api::retry_comment(state, entry_id).await,
            Self::Worklog(entry_id) => api::retry_worklog(state, entry_id).await,
        }
    }

    async fn discard(self, state: &app_core::AppState) -> anyhow::Result<()> {
        match self {
            Self::Comment(entry_id) => api::discard_comment(state, entry_id).await,
            Self::Worklog(entry_id) => api::discard_worklog(state, entry_id).await,
        }
    }
}

/// Why a comment or a worklog was refused, and what the user can do about it.
fn refused(index: usize, refused: Refused, reason: &str, cx: &App) -> impl IntoElement {
    let reason = format!("Not sent: {reason}");
    let kind = refused.kind();
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("{kind}-reason-{index}")))
                .test_support()
                .aria_label(reason.clone())
                .text_sm()
                .text_color(cx.theme().danger)
                .child(reason),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("retry-{kind}-{index}")))
                        .small()
                        .label("Retry")
                        .on_click(move |_, _, cx| {
                            spawn(cx, move |state| async move { refused.retry(&state).await })
                                .detach_and_log_err(cx)
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("discard-{kind}-{index}")))
                        .small()
                        .label("Discard")
                        .on_click(move |_, _, cx| {
                            spawn(
                                cx,
                                move |state| async move { refused.discard(&state).await },
                            )
                            .detach_and_log_err(cx)
                        }),
                ),
        )
}

/// The title of the comments or of the worklogs, with how many there are.
fn section_title(
    id: &'static str,
    label: String,
    title: &'static str,
    count: Option<String>,
    cx: &App,
) -> impl IntoElement {
    div()
        .flex()
        .items_baseline()
        .gap_2()
        .child(
            div()
                .id(id)
                .test_support()
                .aria_label(label)
                .text_size(px(14.))
                .font_semibold()
                .child(title),
        )
        .children(count.map(|count| {
            div()
                .text_size(px(12.))
                .text_color(cx.theme().muted_foreground)
                .child(count)
        }))
}

fn body(
    issue: &IssueDetail,
    status: Entity<StatusButton>,
    assignee_button: Entity<AssigneeButton>,
    comment_form: Entity<CommentForm>,
    comment_bounds: Rc<Cell<Bounds<Pixels>>>,
    worklog_form: Entity<WorklogForm>,
    cx: &Context<DetailPane>,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let or_none = |value: &Option<String>| value.clone().unwrap_or_else(|| "None".to_owned());
    let labels = if issue.labels.is_empty() {
        "None".to_owned()
    } else {
        issue.labels.join(", ")
    };
    let assignee = issue.assignee_name.as_deref().unwrap_or("Unassigned");
    let shown = issue.comments.len();
    let (comments_heading, comment_count) = if shown < issue.comment_total {
        // Jira sends only part of a long thread with a search page
        let part = format!("{shown} of {}", issue.comment_total);
        (format!("Comments ({part})"), Some(part))
    } else {
        (
            "Comments".to_owned(),
            (shown > 0).then(|| shown.to_string()),
        )
    };
    let worklog_count = (!issue.worklogs.is_empty()).then(|| issue.worklogs.len().to_string());

    let about = div()
        .px_5()
        .pb_4()
        .flex()
        .flex_col()
        .gap_3p5()
        .child(
            div()
                .id("detail-summary")
                .test_support()
                .aria_label(issue.summary.clone())
                .text_size(px(19.))
                .font_semibold()
                .child(issue.summary.clone()),
        )
        .child(
            div()
                .grid()
                .grid_cols(2)
                .gap_x_6()
                .gap_y_1p5()
                .child(field_line("detail-status", "Status", &issue.status, cx).child(status))
                .child(
                    field_line("detail-assignee", "Assignee", assignee, cx).child(assignee_button),
                )
                .child(field(
                    "detail-type",
                    "Type",
                    &or_none(&issue.issue_type),
                    cx,
                ))
                .child(field(
                    "detail-reporter",
                    "Reporter",
                    &or_none(&issue.reporter_name),
                    cx,
                ))
                .child(field(
                    "detail-priority",
                    "Priority",
                    &or_none(&issue.priority),
                    cx,
                ))
                .child(field("detail-labels", "Labels", &labels, cx)),
        )
        // Logging time is a main action, so its form stays above the description and
        // the comments, where a long issue does not push it out of view
        .child(worklog_form)
        .child(if issue.description_md.is_empty() {
            div()
                .id("no-description")
                .test_support()
                .text_color(muted)
                .child("No description")
                .into_any_element()
        } else {
            markdown("description", &issue.description_md).into_any_element()
        });

    let comments = div()
        .py_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(section_title(
            "detail-comments",
            comments_heading,
            "Comments",
            comment_count,
            cx,
        ))
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
                .map(|(index, each)| comment(index, each, cx.weak_entity(), cx)),
        )
        .child(
            div()
                .w_full()
                .on_prepaint(move |bounds, _, _| comment_bounds.set(bounds))
                .child(comment_form),
        );

    let worklogs = div()
        .py_3()
        .flex()
        .flex_col()
        .gap_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(section_title(
            "detail-worklogs",
            "Worklogs".to_owned(),
            "Worklogs",
            worklog_count,
            cx,
        ))
        .children(issue.worklogs.is_empty().then(|| {
            div()
                .id("no-worklogs")
                .test_support()
                .text_color(muted)
                .child("No worklogs")
        }))
        .children(
            issue
                .worklogs
                .iter()
                .enumerate()
                .map(|(index, each)| worklog(index, each, cx)),
        );

    div()
        .flex_1()
        .flex()
        .flex_col()
        .child(about)
        // What people wrote and logged lies on a surface of its own, so that
        // it does not read as more of the description
        .child(
            div()
                .flex_1()
                .px_5()
                .bg(cx.theme().sidebar)
                .border_t_1()
                .border_color(cx.theme().border)
                .child(comments)
                .child(worklogs),
        )
}

impl Render for DetailPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let close = Button::new("close-detail")
            .ghost()
            .small()
            .label("Close")
            .children(keys::hint(&keys::Back, cx))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(DetailClosed)));
        let issue = self.issue.read(cx);
        let open_in_jira = issue.detail.as_ref().map(|detail| {
            let url = detail.url.clone();
            Button::new("open-in-jira")
                .small()
                .label("Open in Jira")
                .children(keys::hint(&keys::OpenInJira, cx))
                .on_click(move |_, _, cx| cx.open_url(&url))
        });

        let top = div()
            .flex_none()
            .px_5()
            .py_3()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .font_features(tabular_digits())
                    .text_color(cx.theme().muted_foreground)
                    .child(issue.key.to_string()),
            )
            .child(self.timer.clone())
            .children(open_in_jira)
            .child(close);
        let body = issue.detail.as_ref().map(|detail| {
            body(
                detail,
                self.status.clone(),
                self.assignee.clone(),
                self.comment_form.clone(),
                self.comment_bounds.clone(),
                self.worklog_form.clone(),
                cx,
            )
        });

        div()
            .id("issue-detail")
            .test_support()
            .aria_label(issue.key.to_string())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            // Without it a sideways swipe over code or a table also moves the pane
            .restrict_scroll_to_axis()
            // At least as tall as the pane, so that the surface of the
            // comments reaches its bottom under a short issue
            .child(
                div()
                    .min_h_full()
                    .flex()
                    .flex_col()
                    .child(top)
                    .children(body),
            )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::duration::local_minute;
    use crate::harness::{
        in_window, launch, no_worklog_changes, tab_to, tokio_runtime, wait_until,
    };
    use crate::issue_list::tests::{issue, rows, sync_brings, synced, synced_on};
    use crate::runtime::Core;
    use app_core::{SyncEvent, api};
    use gpui_kit::base::test_support::texts;
    use gpui_kit::test::{TestEventExt, TestWindowExt};
    use gpui_kit::{
        AnyWindowHandle, App, Bounds, ElementId, InputEvent, MouseButton, Pixels, Point,
        ScrollDelta, ScrollEvent, TestAppContext, Window, point, px, size,
    };
    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

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
    pub(crate) fn painted(window: &Window, id: impl Into<ElementId>) -> String {
        let id = id.into();
        let texts: Vec<String> = texts(window)
            .iter()
            .filter(|text| text.path().contains(&id))
            .map(|text| text.text().to_owned())
            .collect();
        texts.join("\n")
    }

    fn bounds_of(window: &Window, text: &str) -> Bounds<Pixels> {
        let found = texts(window).into_iter().find(|each| each.text() == text);
        found
            .unwrap_or_else(|| panic!("no {text:?} on screen"))
            .bounds()
    }

    /// Turns the wheel or swipes over `position`.
    fn scroll(window: &mut Window, position: Point<Pixels>, delta: Point<Pixels>, cx: &mut App) {
        // A scroll goes to what the pointer is over
        window.pointer_move(position, None, cx);
        let event = ScrollEvent {
            position,
            delta: ScrollDelta::Pixels(delta),
            ..Default::default()
        };
        window.dispatch_event(event.to_platform_input(), cx);
        window.render_frame(cx);
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
                // The key hint of the search is a text of the list
                if line.right() <= pane.left() {
                    continue;
                }
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
    fn wide_code_and_a_wide_table_scroll_sideways_in_a_narrow_pane(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut bug = issue(1, "Fix login");
        let wide_code = format!("src/auth/{}  # the last column\nb", "nested/".repeat(30));
        let cell = |kind: &str, text: &str| json!({ "type": kind, "content": [paragraph(text)] });
        let row = |kind: &str, texts: [&str; 6]| {
            let cells: Vec<Value> = texts.iter().map(|text| cell(kind, text)).collect();
            json!({ "type": "tableRow", "content": cells })
        };
        bug["fields"]["description"] = doc(json!([
            { "type": "codeBlock", "content": [{ "type": "text", "text": wide_code }] },
            { "type": "codeBlock", "content": [{ "type": "text", "text": "a\nb" }] },
            { "type": "table", "content": [
                row("tableHeader", ["Id", "Component", "Environment", "Reproducibility", "Responsible", "Last"]),
                row("tableCell", ["1", "Authentication", "Preproduction", "Intermittently", "Administrators", "Yesterday"]),
            ] },
        ]));
        let data_dir = synced(&tokio, &[bug]);
        let window = launch(cx, &tokio, data_dir.path());
        cx.simulate_window_resize(window, size(px(700.), px(600.)));
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| {
            let pane = window.find("issue-detail").bounds();
            assert_eq!(pane.size.width, px(419.));

            let code = bounds_of(window, &wide_code);
            assert!(code.right() > pane.right());
            // Two source lines take as much height as two short ones
            assert_eq!(code.size.height, bounds_of(window, "a\nb").size.height);
            scroll(window, code.origin, point(px(-100_000.), px(0.)), cx);
            let scrolled = bounds_of(window, &wide_code);
            assert!(scrolled.right() <= pane.right(), "{scrolled:?} in {pane:?}");
            // The pane did not take the same gesture as a vertical one
            assert_eq!(scrolled.top(), code.top());

            // "Id" fits on one line at any width
            let one_line = bounds_of(window, "Id").size.height;
            for word in ["Reproducibility", "Authentication", "Administrators"] {
                assert_eq!(bounds_of(window, word).size.height, one_line, "{word}");
            }
            let last = bounds_of(window, "Yesterday");
            assert!(last.right() > pane.right());
            let first_column = bounds_of(window, "Id").origin;
            scroll(window, first_column, point(px(-100_000.), px(0.)), cx);
            let scrolled = bounds_of(window, "Yesterday");
            assert!(scrolled.right() <= pane.right(), "{scrolled:?} in {pane:?}");
            assert_eq!(scrolled.top(), last.top());

            // The wheel over a code block still scrolls the pane, not the code
            let code = bounds_of(window, "a\nb");
            scroll(window, code.origin, point(px(0.), px(-20.)), cx);
            let scrolled = bounds_of(window, "a\nb");
            assert_eq!(scrolled.origin, point(code.left(), code.top() - px(20.)));
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
    fn code_is_shown_as_written_and_can_be_copied(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut bug = issue(1, "Fix login");
        let code = |text: &str| {
            let content = json!([{ "type": "text", "text": text }]);
            json!({ "type": "codeBlock", "attrs": { "language": "rust" }, "content": content })
        };
        bug["fields"]["description"] = doc(json!([
            code("let x = 1;"),
            { "type": "bulletList", "content": [
                { "type": "listItem", "content": [paragraph("Open the page"), code("in_a_list();")] },
            ] },
            { "type": "blockquote", "content": [code("quoted();")] },
        ]));
        let data_dir = synced(&tokio, &[bug]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| {
            assert_eq!(
                painted(window, "description"),
                "let x = 1;\nOpen the page\nin_a_list();\nquoted();"
            );
            let line = bounds_of(window, "let x = 1;");
            let middle = line.center().y;
            let (start, end) = (point(line.left(), middle), point(line.right(), middle));
            window.pointer_down(start, MouseButton::Left, cx);
            window.pointer_move(end, Some(MouseButton::Left), cx);
            window.pointer_up(end, MouseButton::Left, cx);
            window.press("secondary-c", cx);
        });

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("let x = 1;"));
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
    fn the_detail_of_a_moved_issue_does_not_stay_under_the_old_key(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[login_bug(), issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        let mut moved = login_bug();
        moved["key"] = json!("API-1");
        moved["fields"]["updated"] = json!("2026-03-30T10:00:00.000+0100");
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        tokio.block_on(state.store_issues(&[moved])).unwrap();
        state.emit(SyncEvent::IssuesUpdated(vec![
            "API-1".into(),
            "WEB-1".into(),
        ]));

        wait_until(cx, window, |window| {
            window.try_find("detail-summary").is_none()
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

    #[gpui_kit::test]
    fn the_open_issue_stays_on_screen_until_the_next_one_is_read(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[login_bug(), issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| window.click("issue-WEB-2", cx));
        // The answer of the cache waits for the executor, which has not run
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("issue-detail").label(), Some("WEB-1"));
            assert_eq!(label(window, "detail-summary"), "Fix login");
        });

        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some("WEB-2")
        });
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "detail-summary"), "Fix logout");
        });
    }

    /// A mock Jira that finds WEB-1 and has no worklog changes.
    async fn jira_with_web_1() -> MockServer {
        let server = MockServer::start().await;
        no_worklog_changes().mount(&server).await;
        crate::harness::no_sprint_field().mount(&server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "issues": [issue(1, "Fix login")] })),
            )
            .mount(&server)
            .await;
        server
    }

    /// The lines of the worklog list on screen.
    fn worklog_lines(window: &Window) -> Vec<String> {
        (0..5)
            .map_while(|index| window.try_find(format!("worklog-{index}")))
            .map(|worklog| worklog.label().unwrap().to_owned())
            .collect()
    }

    #[gpui_kit::test]
    fn the_worklogs_of_an_issue_appear_when_a_sync_brings_them(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira_with_web_1());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "detail-worklogs"), "Worklogs");
            assert!(window.try_find("no-worklogs").is_some());
        });

        // 10:15 at UTC+2, 93 minutes
        tokio.block_on(async {
            server.reset().await;
            crate::harness::no_sprint_field().mount(&server).await;
            Mock::given(path("/rest/api/3/search/jql"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({ "issues": [issue(1, "Fix login")] })),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/rest/api/3/worklog/updated"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "values": [{ "worklogId": 7, "updatedTime": 5 }],
                    "until": 5,
                    "lastPage": true,
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/rest/api/3/worklog/deleted"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({ "values": [], "until": 5, "lastPage": true })),
                )
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/rest/api/3/worklog/list"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                    "id": "7",
                    "issueId": "1",
                    "author": { "accountId": "7c20b", "displayName": "Grace Hopper" },
                    "timeSpentSeconds": 5580,
                    "started": "2026-03-29T10:15:00.000+0200",
                    "comment": doc(json!([paragraph("Pairing")])),
                }])))
                .mount(&server)
                .await;
        });
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        tokio.block_on(state.sync_once()).unwrap();

        let started = local_minute(1_774_772_100);
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("Grace Hopper, {started}, 1h 33m")]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(painted(window, ("worklog-comment", 0usize)), "Pairing");
            assert!(window.try_find("no-worklogs").is_none());
        });
    }

    #[gpui_kit::test]
    fn a_worklog_is_pending_until_delivered_and_not_sent_when_jira_refuses_it(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira_with_web_1());
        tokio.block_on(
            Mock::given(method("POST"))
                .and(path("/rest/api/3/issue/WEB-1/worklog"))
                .respond_with(
                    ResponseTemplate::new(400)
                        .set_body_json(json!({ "errorMessages": ["No permission"] })),
                )
                .mount(&server),
        );
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        let state = cx.update(|cx| cx.global::<Core>().state.clone());

        tokio
            .block_on(api::log_time(
                &state,
                &"WEB-1".into(),
                900,
                1_774_771_200,
                Some("Review"),
            ))
            .unwrap();

        let earlier = local_minute(1_774_771_200);
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {earlier}, 15m, pending")]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(painted(window, ("worklog-comment", 0usize)), "Review");
        });

        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {earlier}, 15m, not sent")]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(
                window.find("worklog-reason-0").label(),
                Some("Not sent: Jira returned 400 Bad Request: No permission")
            );
        });

        // Jira refuses it again
        in_window(cx, window, |window, cx| {
            window.scroll(
                "issue-detail",
                ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                cx,
            );
            window.click("retry-worklog-0", cx);
        });
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {earlier}, 15m, pending")]
        });
        tokio.block_on(state.sync_once()).unwrap();
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {earlier}, 15m, not sent")]
        });

        // And again when the retry comes from the keyboard
        tab_to(cx, window, "retry-worklog-0");
        in_window(cx, window, |window, cx| window.press("enter", cx));
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {earlier}, 15m, pending")]
        });
        tokio.block_on(state.sync_once()).unwrap();
        wait_until(cx, window, |window| {
            worklog_lines(window) == [format!("You, {earlier}, 15m, not sent")]
        });

        in_window(cx, window, |window, cx| {
            window.scroll(
                "issue-detail",
                ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                cx,
            );
            window.click("discard-worklog-0", cx);
        });
        wait_until(cx, window, |window| {
            window.try_find("no-worklogs").is_some()
        });
        let unsent = tokio.block_on(api::unsent_changes(&state)).unwrap();
        assert_eq!(unsent, 0);
    }
}
