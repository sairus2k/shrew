//! The text the user typed into the comment input and the time form of an
//! issue and did not send. The detail pane is built anew for every issue it
//! shows, so the forms save here as the user types and read from here when
//! they are built. It lives as long as the signed-in session: nothing is
//! written to disk, and signing out drops it.

use crate::comment::ReplyTarget;
use app_core::models::IssueKey;
use gpui_kit::*;
use std::collections::HashMap;

#[derive(Clone, Default)]
pub struct Draft {
    pub comment: String,
    pub reply_to: Option<ReplyTarget>,
    pub duration: String,
    pub started: String,
    pub worklog_comment: String,
}

impl Draft {
    fn is_empty(&self) -> bool {
        self.comment.is_empty()
            && self.reply_to.is_none()
            && self.duration.is_empty()
            && self.started.is_empty()
            && self.worklog_comment.is_empty()
    }
}

#[derive(Clone, Default)]
pub struct Drafts(HashMap<IssueKey, Draft>);

impl Global for Drafts {}

impl Drafts {
    /// What was kept for `key`, empty when nothing was.
    pub fn get(cx: &mut App, key: &IssueKey) -> Draft {
        cx.default_global::<Self>()
            .0
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn update(cx: &mut App, key: &IssueKey, change: impl FnOnce(&mut Draft)) {
        let drafts = &mut cx.default_global::<Self>().0;
        let draft = drafts.entry(key.clone()).or_default();
        change(draft);
        if draft.is_empty() {
            drafts.remove(key);
        }
    }

    pub fn clear(cx: &mut App) {
        cx.set_global(Self::default());
    }
}

#[cfg(test)]
mod tests {
    use super::Drafts;
    use crate::harness::{close_settings, in_window, launch, tokio_runtime, wait_until};
    use crate::issue_list::tests::{issue, synced};
    use gpui_kit::test::{TestQueryExt as _, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, App, TestAppContext, Window};

    fn open(cx: &mut TestAppContext, window: AnyWindowHandle, key: &'static str) {
        let row = format!("issue-{key}");
        wait_until(cx, window, |window| window.try_find(row.clone()).is_some());
        in_window(cx, window, |window, cx| window.click(row.clone(), cx));
        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some(key)
                && window.try_find("comment-input").is_some()
        });
    }

    fn fill(window: &mut Window, cx: &mut App, id: &'static str, text: &str) {
        window.click(id, cx);
        window.press("secondary-a", cx);
        window.press("backspace", cx);
        window.input(text, cx);
        window.render_frame(cx);
    }

    /// The comment input and the time form as the user sees them.
    fn typed(window: &Window) -> [String; 4] {
        let value = |id: &'static str| window.find(id).value().unwrap_or_default().to_owned();
        [
            window
                .find_by_label("New comment")
                .value()
                .unwrap_or_default()
                .to_owned(),
            value("worklog-duration"),
            value("worklog-started"),
            value("worklog-comment-input"),
        ]
    }

    fn write_everything(cx: &mut TestAppContext, window: AnyWindowHandle) {
        in_window(cx, window, |window, cx| {
            fill(window, cx, "comment-input", "Half a thought");
            fill(window, cx, "worklog-duration", "1h 30m");
            fill(window, cx, "worklog-started", "2026-03-29 10:15");
            fill(window, cx, "worklog-comment-input", "Planning");
        });
    }

    fn kept_text() -> [String; 4] {
        ["Half a thought", "1h 30m", "2026-03-29 10:15", "Planning"].map(String::from)
    }

    // The runtime and the data dir must outlive the test body
    fn two_issues(
        cx: &mut TestAppContext,
    ) -> (tokio::runtime::Runtime, tempfile::TempDir, AnyWindowHandle) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[issue(1, "Fix login"), issue(2, "Fix logout")]);
        let window = launch(cx, &tokio, data_dir.path());
        (tokio, data_dir, window)
    }

    #[gpui_kit::test]
    fn the_text_is_back_after_another_issue_was_opened(cx: &mut TestAppContext) {
        let (_tokio, _dir, window) = two_issues(cx);
        open(cx, window, "WEB-1");
        write_everything(cx, window);

        open(cx, window, "WEB-2");
        in_window(cx, window, |window, _| {
            assert_eq!(typed(window)[0], "");
            assert_eq!(typed(window)[1], "");
        });

        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| {
            assert_eq!(typed(window), kept_text());
        });
    }

    #[gpui_kit::test]
    fn the_text_is_back_after_the_pane_was_closed(cx: &mut TestAppContext) {
        let (_tokio, _dir, window) = two_issues(cx);
        open(cx, window, "WEB-1");
        write_everything(cx, window);

        in_window(cx, window, |window, cx| window.click("close-detail", cx));
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| {
            assert_eq!(typed(window), kept_text());
        });
    }

    #[gpui_kit::test]
    fn the_text_is_back_after_the_settings(cx: &mut TestAppContext) {
        let (_tokio, _dir, window) = two_issues(cx);
        open(cx, window, "WEB-1");
        write_everything(cx, window);

        in_window(cx, window, |window, cx| window.click("edit-settings", cx));
        wait_until(cx, window, |window| window.try_find("settings").is_some());
        close_settings(cx, window);
        in_window(cx, window, |window, _| {
            assert_eq!(typed(window), kept_text());
        });
    }

    #[gpui_kit::test]
    fn sending_clears_what_was_kept_for_that_issue_only(cx: &mut TestAppContext) {
        let (_tokio, _dir, window) = two_issues(cx);
        open(cx, window, "WEB-1");
        write_everything(cx, window);
        open(cx, window, "WEB-2");
        in_window(cx, window, |window, cx| {
            fill(window, cx, "comment-input", "For the other issue");
        });

        open(cx, window, "WEB-1");
        in_window(cx, window, |window, cx| {
            window.click("submit-comment", cx);
            window.click("submit-worklog", cx);
        });
        wait_until(cx, window, |window| {
            typed(window)[0].is_empty() && typed(window)[1].is_empty()
        });

        open(cx, window, "WEB-2");
        in_window(cx, window, |window, _| {
            assert_eq!(typed(window)[0], "For the other issue");
        });
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| {
            assert_eq!(typed(window)[0], "");
            assert_eq!(typed(window)[1], "");
            assert_eq!(typed(window)[3], "");
        });
    }

    #[gpui_kit::test]
    fn signing_out_discards_everything_that_was_kept(cx: &mut TestAppContext) {
        let (_tokio, _dir, window) = two_issues(cx);
        open(cx, window, "WEB-1");
        write_everything(cx, window);
        open(cx, window, "WEB-2");
        let key = "WEB-1".into();
        assert!(!cx.update(|cx| Drafts::get(cx, &key)).comment.is_empty());

        in_window(cx, window, |window, cx| window.click("edit-settings", cx));
        wait_until(cx, window, |window| window.try_find("sign-out").is_some());
        in_window(cx, window, |window, cx| window.click("sign-out", cx));
        wait_until(cx, window, |window| window.try_find("site-url").is_some());

        let draft = cx.update(|cx| Drafts::get(cx, &key));
        assert!(draft.comment.is_empty() && draft.duration.is_empty());
    }

    #[gpui_kit::test]
    fn a_start_the_user_left_alone_is_not_kept(cx: &mut TestAppContext) {
        let (_tokio, _dir, window) = two_issues(cx);
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, cx| {
            fill(window, cx, "worklog-duration", "1h");
        });

        let draft = cx.update(|cx| Drafts::get(cx, &"WEB-1".into()));
        assert_eq!(draft.duration, "1h");
        assert_eq!(draft.started, "");
    }
}
