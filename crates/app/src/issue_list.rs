//! The main screen: the cached issues and the search over them.

use crate::runtime::{Core, spawn};
use app_core::{
    SyncEvent, api,
    models::{IssueRow, StatusCategory},
};
use gpui_kit::component::{
    ActiveTheme,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;
use std::ops::Range;

pub struct IssueList {
    query: Entity<InputState>,
    issues: Vec<IssueRow>,
    loading: Task<()>,
}

impl IssueList {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search issues"));
        cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.reload(cx);
            }
        })
        .detach();
        let sync = cx.global::<Core>().sync.clone();
        cx.subscribe(&sync, |this, _, event: &SyncEvent, cx| {
            if let SyncEvent::IssuesUpdated(_) = event {
                this.reload(cx);
            }
        })
        .detach();

        let mut this = Self {
            query,
            issues: Vec::new(),
            loading: Task::ready(()),
        };
        this.reload(cx);
        this
    }

    // No debounce: an FTS5 query takes a few milliseconds and there is no IPC hop
    fn reload(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_owned();
        let load = spawn(cx, move |state| async move {
            if query.chars().count() < 2 {
                api::list_issues(&state).await
            } else {
                api::search_issues(&state, &query, None).await
            }
        });
        self.show(load, cx);
    }

    fn show(&mut self, load: Task<anyhow::Result<Vec<IssueRow>>>, cx: &mut Context<Self>) {
        // Assigning the task drops the previous one, so a slow older query never
        // replaces newer rows. The old rows stay on screen until the new ones arrive.
        self.loading = cx.spawn(async move |this, cx| {
            if let Ok(issues) = load.await {
                this.update(cx, |this, cx| {
                    this.issues = issues;
                    cx.notify();
                })
                .ok();
            }
        });
    }

    fn rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.issues[range]
            .iter()
            .map(|issue| row(issue, cx).into_any_element())
            .collect()
    }
}

fn row(issue: &IssueRow, cx: &App) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let assignee = issue.assignee_name.as_deref().unwrap_or("Unassigned");
    let label = format!(
        "{}: {}, {}, {assignee}",
        issue.key, issue.summary, issue.status
    );
    div()
        .id(SharedString::from(format!("issue-{}", issue.key)))
        .role(Role::ListItem)
        .test_support()
        .aria_label(label)
        // uniform_list gives every row the height of the first one
        .h(px(32.))
        .w_full()
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .truncate()
                .text_color(muted)
                .child(issue.key.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(issue.summary.clone()),
        )
        .child(
            div()
                .w(px(120.))
                .flex_none()
                .truncate()
                .text_color(match issue.status_category {
                    StatusCategory::Done => muted,
                    StatusCategory::Todo | StatusCategory::InProgress => cx.theme().foreground,
                })
                .child(issue.status.clone()),
        )
        .child(
            div()
                .w(px(140.))
                .flex_none()
                .truncate()
                .text_color(muted)
                .child(assignee.to_owned()),
        )
}

impl Render for IssueList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div().p_3().child(
                    Input::new(&self.query)
                        .id("search")
                        .aria_label("Search issues"),
                ),
            )
            .child(
                // Only the rows in view are built, so the cost of a frame does
                // not grow with the number of issues
                uniform_list(
                    "issues",
                    self.issues.len(),
                    cx.processor(|this, range, _, cx| this.rows(range, cx)),
                )
                .flex_1()
                .min_h_0(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::IssueList;
    use crate::runtime::{Core, tests::init_core};
    use crate::sign_in::tests::{in_window, launch, tokio_runtime, wait_until};
    use app_core::{
        SyncEvent,
        models::{IssueRow, StatusCategory, SyncStatus},
    };
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, AppContext, Role, TestAppContext, Window, WindowOptions, px};
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};
    use tokio::{runtime::Runtime, sync::oneshot};

    /// An unassigned WEB issue in To Do. A higher `id` was changed later.
    fn issue(id: u32, summary: &str) -> Value {
        json!({
            "id": id.to_string(),
            "key": format!("WEB-{id}"),
            "fields": {
                "summary": summary,
                "description": null,
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "assignee": null,
                "project": { "key": "WEB" },
                "issuetype": { "name": "Task" },
                "updated": format!(
                    "2026-03-29T{:02}:{:02}:{:02}.000+0000",
                    id / 3600,
                    id / 60 % 60,
                    id % 60
                ),
            },
        })
    }

    /// The data dir of an app that has finished its initial sync and has
    /// `issues` in its cache.
    fn synced(tokio: &Runtime, issues: &[Value]) -> tempfile::TempDir {
        let data_dir = tempfile::tempdir().unwrap();
        let config = json!({
            "site_url": "http://localhost",
            "email": "ada@example.com",
            "account_id": "5b10a",
            "watched_projects": ["WEB"],
            "last_sync_at": 1,
        });
        std::fs::write(data_dir.path().join("config.json"), config.to_string()).unwrap();
        std::fs::write(data_dir.path().join("api-token"), "secret-token").unwrap();
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir.path())
                .await
                .unwrap();
            state.store_issues(issues).await.unwrap();
        });
        data_dir
    }

    /// What the sync engine does when Jira reports a new or changed issue.
    fn sync_brings(cx: &mut TestAppContext, tokio: &Runtime, issue: Value) {
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        let key = issue["key"].as_str().unwrap().into();
        tokio.block_on(state.store_issues(&[issue])).unwrap();
        state.emit(SyncEvent::IssuesUpdated(vec![key]));
    }

    /// The rows on screen, top to bottom.
    fn rows(window: &Window) -> Vec<String> {
        window
            .find_all_by_role(Role::ListItem)
            .iter()
            .map(|row| row.label().unwrap().to_owned())
            .collect()
    }

    fn wait_for_rows(cx: &mut TestAppContext, window: AnyWindowHandle, expected: &[&str]) {
        wait_until(cx, window, |window| rows(window) == expected);
    }

    const LOGIN: &str = "WEB-1: Fix login, To Do, Unassigned";
    const LOGOUT: &str = "WEB-2: Fix logout, To Do, Unassigned";
    const CHECKOUT: &str = "WEB-3: Broken checkout, To Do, Unassigned";

    fn three_issues() -> [Value; 3] {
        [
            issue(1, "Fix login"),
            issue(2, "Fix logout"),
            issue(3, "Broken checkout"),
        ]
    }

    #[gpui_kit::test]
    fn the_window_lists_the_stored_issues(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut assigned = issue(2, "Broken checkout");
        assigned["fields"]["status"] =
            json!({ "name": "In Progress", "statusCategory": { "key": "indeterminate" } });
        assigned["fields"]["assignee"] =
            json!({ "accountId": "5b10a", "displayName": "Ada Lovelace" });
        let data_dir = synced(&tokio, &[issue(1, "Fix login"), assigned]);

        let window = launch(cx, &tokio, data_dir.path());

        wait_for_rows(
            cx,
            window,
            &[
                "WEB-2: Broken checkout, In Progress, Ada Lovelace",
                "WEB-1: Fix login, To Do, Unassigned",
            ],
        );
        // Each row is as wide as the window, so the columns line up
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("issue-WEB-1").bounds().size.width, px(800.));
        });
    }

    #[gpui_kit::test]
    fn each_keystroke_from_the_second_on_searches(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("lo", cx);
            // The answer cannot be here yet: the old rows stay until it is
            assert_eq!(rows(window), [CHECKOUT, LOGOUT, LOGIN]);
        });
        wait_until(cx, window, |window| {
            let mut found = rows(window);
            found.sort();
            found == [LOGIN, LOGOUT]
        });

        in_window(cx, window, |window, cx| {
            window.input("gi", cx);
            assert_eq!(rows(window).len(), 2);
        });
        wait_for_rows(cx, window, &[LOGIN]);

        in_window(cx, window, |window, cx| window.input("x", cx));
        wait_for_rows(cx, window, &[]);
    }

    #[gpui_kit::test]
    fn a_query_of_one_character_shows_every_issue(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("che", cx);
        });
        wait_for_rows(cx, window, &[CHECKOUT]);

        in_window(cx, window, |window, cx| {
            window.press("backspace", cx);
            window.press("backspace", cx);
        });

        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);
    }

    #[gpui_kit::test]
    fn the_list_follows_the_sync_engine(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        sync_brings(cx, &tokio, issue(4, "Login page is slow"));
        wait_for_rows(
            cx,
            window,
            &[
                "WEB-4: Login page is slow, To Do, Unassigned",
                CHECKOUT,
                LOGOUT,
                LOGIN,
            ],
        );

        // With a query in the input it is the query that is repeated
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("checkout", cx);
        });
        wait_for_rows(cx, window, &[CHECKOUT]);
        sync_brings(cx, &tokio, issue(5, "Checkout button"));
        wait_until(cx, window, |window| rows(window).len() == 2);
        sync_brings(cx, &tokio, issue(6, "Unrelated"));
        let mut renamed = issue(3, "Broken payment");
        // The cache only takes an issue that Jira changed after the stored one
        renamed["fields"]["updated"] = json!("2026-03-30T00:00:00.000+0000");
        sync_brings(cx, &tokio, renamed);
        wait_for_rows(cx, window, &["WEB-5: Checkout button, To Do, Unassigned"]);
    }

    fn twelve_thousand_issues() -> Vec<Value> {
        (1..=12_000)
            .map(|id| issue(id, &format!("Task {id}")))
            .collect()
    }

    fn found(key: &str) -> anyhow::Result<Vec<IssueRow>> {
        Ok(vec![IssueRow {
            id: key.to_owned(),
            key: key.into(),
            summary: "Found".to_owned(),
            status: "To Do".to_owned(),
            status_category: StatusCategory::Todo,
            assignee_name: None,
            project_key: "WEB".to_owned(),
            sync_status: SyncStatus::Synced,
        }])
    }

    #[gpui_kit::test]
    fn a_slower_earlier_query_does_not_replace_a_later_one(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[]);
        cx.update(gpui_kit::init);
        init_core(cx, &tokio, data_dir.path());
        let (window, list) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| IssueList::new(window, cx))
            })
            .unwrap()
        });

        // Two queries whose answers the test releases in the opposite order
        let (answer_earlier, earlier) = oneshot::channel::<()>();
        let (answer_later, later) = oneshot::channel::<()>();
        list.update(cx, |list, cx| {
            let earlier = cx.background_spawn(async move {
                earlier.await?;
                found("WEB-1")
            });
            list.show(earlier, cx);
            let later = cx.background_spawn(async move {
                later.await?;
                found("WEB-2")
            });
            list.show(later, cx);
        });
        answer_later.send(()).unwrap();
        cx.run_until_parked();
        // Fails once the view has dropped the earlier query, which is the point
        answer_earlier.send(()).ok();
        cx.run_until_parked();

        in_window(cx, window, |window, _| {
            assert_eq!(rows(window), ["WEB-2: Found, To Do, Unassigned"]);
        });
    }

    #[gpui_kit::test]
    fn the_list_stays_responsive_with_12_000_issues(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &twelve_thousand_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| !rows(window).is_empty());

        // Only the rows that fit the 600 px window are built
        let frame = Instant::now();
        let shown = in_window(cx, window, |window, _| rows(window));
        let frame = frame.elapsed();
        assert_eq!(shown[0], "WEB-12000: Task 12000, To Do, Unassigned");
        assert!(shown.len() < 30, "{} rows were built", shown.len());
        assert!(frame < Duration::from_millis(100), "a frame took {frame:?}");

        let search = Instant::now();
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("11999", cx);
        });
        wait_for_rows(cx, window, &["WEB-11999: Task 11999, To Do, Unassigned"]);
        let search = search.elapsed();
        assert!(
            search < Duration::from_millis(500),
            "five keystrokes took {search:?}"
        );
    }
}
