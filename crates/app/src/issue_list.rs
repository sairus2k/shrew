//! The main screen: the cached issues and the search over them.

use crate::runtime::{Core, spawn};
use app_core::{
    SyncEvent, api,
    models::{IssueKey, IssueRow, StatusCategory, SyncStatus},
};
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::Button,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::ops::Range;

/// Emitted when the user picks an issue to read.
pub struct IssueOpened(pub IssueKey);

pub struct IssueList {
    query: Entity<InputState>,
    // The watched projects; the filter is offered when there are several
    projects: Vec<String>,
    // `None` shows every watched project
    project: Option<String>,
    issues: Vec<IssueRow>,
    // The issue shown beside the list
    opened: Option<IssueKey>,
    loading: Task<()>,
}

impl EventEmitter<IssueOpened> for IssueList {}

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
            projects: Vec::new(),
            project: None,
            issues: Vec::new(),
            opened: None,
            loading: Task::ready(()),
        };
        this.reload(cx);
        this.load_projects(cx);
        this
    }

    fn load_projects(&mut self, cx: &mut Context<Self>) {
        let load = spawn(cx, |state| async move {
            Ok(api::watched_projects(&state).await)
        });
        cx.spawn(async move |this, cx| {
            let projects = load.await;
            this.update(cx, |this, cx| {
                this.projects = projects.unwrap_or_default();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_project(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.project = project;
        self.reload(cx);
        cx.notify();
    }

    pub fn open(&mut self, key: IssueKey, cx: &mut Context<Self>) {
        self.opened = Some(key.clone());
        cx.emit(IssueOpened(key));
        cx.notify();
    }

    /// The issue beside the list was closed.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        self.opened = None;
        cx.notify();
    }

    // No debounce: an FTS5 query takes a few milliseconds and there is no IPC hop
    fn reload(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_owned();
        let project = self.project.clone();
        let load = spawn(cx, move |state| async move {
            if query.chars().count() < 2 {
                api::list_issues(&state, project.as_deref()).await
            } else {
                api::search_issues(&state, &query, project.as_deref()).await
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
            .map(|issue| row(issue, self.opened.as_ref(), cx).into_any_element())
            .collect()
    }
}

fn row(
    issue: &IssueRow,
    opened: Option<&IssueKey>,
    cx: &mut Context<IssueList>,
) -> impl IntoElement + use<> {
    let muted = cx.theme().muted_foreground;
    let key = issue.key.clone();
    let assignee = issue.assignee_name.as_deref().unwrap_or("Unassigned");
    // The status is the user's own change and Jira does not have it yet
    let pending = issue.sync_status == SyncStatus::Pending;
    let label = format!(
        "{}: {}, {}{}, {assignee}",
        issue.key,
        issue.summary,
        issue.status,
        if pending { " (pending)" } else { "" }
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
        .when(opened == Some(&issue.key), |row| {
            row.bg(cx.theme().list_active)
        })
        .on_click(cx.listener(move |this, _, _, cx| this.open(key.clone(), cx)))
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
        // Beside an open issue the list is too narrow for these two, and the
        // issue itself shows them
        .when(opened.is_none(), |row| {
            row.child(
                div()
                    .w(px(120.))
                    .flex_none()
                    .truncate()
                    .text_color(match issue.status_category {
                        StatusCategory::Done => muted,
                        StatusCategory::Todo | StatusCategory::InProgress => cx.theme().foreground,
                    })
                    .child(if pending {
                        format!("↑ {}", issue.status)
                    } else {
                        issue.status.clone()
                    }),
            )
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .truncate()
                    .text_color(muted)
                    .child(assignee.to_owned()),
            )
        })
}

impl Render for IssueList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let projects = self.projects.clone();
        let choosable = projects.len() > 1;
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .p_3()
                    .flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.query)
                                .id("search")
                                .aria_label("Search issues"),
                        ),
                    )
                    .when(choosable, |bar| {
                        bar.child(
                            Button::new("project-filter")
                                .small()
                                .label(self.project.clone().unwrap_or("All projects".to_owned()))
                                .dropdown_menu(move |mut menu, window, _| {
                                    let choices = std::iter::once(None)
                                        .chain(projects.iter().cloned().map(Some));
                                    for choice in choices {
                                        let label =
                                            choice.clone().unwrap_or("All projects".to_owned());
                                        menu = menu.item(PopupMenuItem::new(label).on_click(
                                            window.listener_for(&view, move |this, _, _, cx| {
                                                this.choose_project(choice.clone(), cx)
                                            }),
                                        ));
                                    }
                                    menu
                                }),
                        )
                    }),
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
pub(crate) mod tests {
    use super::IssueList;
    use crate::harness::{in_window, launch, tokio_runtime, wait_until};
    use crate::runtime::{Core, tests::init_core};
    use app_core::{
        SyncEvent,
        models::{IssueRow, StatusCategory, SyncStatus},
    };
    use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
    use gpui_kit::{
        AnyWindowHandle, AppContext, MouseButton, Role, TestAppContext, Window, WindowOptions, px,
    };
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};
    use tokio::{runtime::Runtime, sync::oneshot};

    /// An unassigned WEB issue in To Do. A higher `id` was changed later.
    pub(crate) fn issue(id: u32, summary: &str) -> Value {
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
    pub(crate) fn synced(tokio: &Runtime, issues: &[Value]) -> tempfile::TempDir {
        synced_on(tokio, "http://localhost", issues)
    }

    /// `synced` for an app that is signed in to `site_url`.
    pub(crate) fn synced_on(
        tokio: &Runtime,
        site_url: &str,
        issues: &[Value],
    ) -> tempfile::TempDir {
        let data_dir = tempfile::tempdir().unwrap();
        let config = json!({
            "site_url": site_url,
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
    pub(crate) fn sync_brings(cx: &mut TestAppContext, tokio: &Runtime, issue: Value) {
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        let key = issue["key"].as_str().unwrap().into();
        tokio.block_on(state.store_issues(&[issue])).unwrap();
        state.emit(SyncEvent::IssuesUpdated(vec![key]));
    }

    /// `synced` for an app that watches WEB and API. API-1 and API-2 hold the
    /// ids 101 and 102 so that they are newer than the WEB issues.
    fn synced_two_projects(tokio: &Runtime, issues: &[Value]) -> tempfile::TempDir {
        let data_dir = synced(tokio, issues);
        let path = data_dir.path().join("config.json");
        let mut config: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        config["watched_projects"] = json!(["WEB", "API"]);
        std::fs::write(path, config.to_string()).unwrap();
        data_dir
    }

    pub(crate) fn api_issue(id: u32, summary: &str) -> Value {
        let mut issue = issue(id, summary);
        issue["key"] = json!(format!("API-{id}"));
        issue["fields"]["project"] = json!({ "key": "API" });
        issue
    }

    /// The rows on screen, top to bottom.
    pub(crate) fn rows(window: &Window) -> Vec<String> {
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

    const API_LOGIN: &str = "API-101: Login endpoint, To Do, Unassigned";
    const API_TOKEN: &str = "API-102: Token refresh, To Do, Unassigned";

    fn two_projects() -> Vec<Value> {
        let mut issues = three_issues().to_vec();
        issues.extend([
            api_issue(101, "Login endpoint"),
            api_issue(102, "Token refresh"),
        ]);
        issues
    }

    /// Opens the project menu and clicks the item `name`.
    pub(crate) fn choose_project(window: &mut Window, cx: &mut gpui_kit::App, name: &str) {
        window.click("project-filter", cx);
        window.render_frame(cx);
        let items = window.find_all_by_role(Role::MenuItem);
        let item = items.iter().find(|item| item.label() == Some(name));
        let middle = item.expect("no such project").bounds().center();
        window.pointer_down(middle, MouseButton::Left, cx);
        window.pointer_up(middle, MouseButton::Left, cx);
        window.render_frame(cx);
    }

    #[gpui_kit::test]
    fn a_chosen_project_narrows_the_list_and_all_projects_widen_it(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("project-filter").label(), Some("All projects"));
        });

        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("project-filter").label(), Some("WEB"));
        });

        in_window(cx, window, |window, cx| choose_project(window, cx, "API"));
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN]);

        in_window(cx, window, |window, cx| {
            choose_project(window, cx, "All projects")
        });
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
    }

    #[gpui_kit::test]
    fn the_search_finds_only_the_issues_of_the_chosen_project(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);

        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("login", cx);
        });
        wait_until(cx, window, |window| rows(window).len() == 2);
        let mut all = in_window(cx, window, |window, _| rows(window));
        all.sort();
        assert_eq!(all, [API_LOGIN, LOGIN]);

        in_window(cx, window, |window, cx| choose_project(window, cx, "API"));
        wait_for_rows(cx, window, &[API_LOGIN]);

        // Another query keeps the project
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.input("tok", cx);
        });
        wait_for_rows(cx, window, &[API_TOKEN]);
        in_window(cx, window, |window, cx| {
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
        });
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN]);
    }

    #[gpui_kit::test]
    fn the_chosen_project_stays_applied_when_a_sync_brings_issues(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        sync_brings(cx, &tokio, api_issue(103, "Rate limits"));
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
    }

    #[gpui_kit::test]
    fn with_one_watched_project_there_is_nothing_to_choose(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        in_window(cx, window, |window, _| {
            assert!(window.try_find("project-filter").is_none());
        });
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
        // The core answers from the tokio runtime, outside GPUI's scheduler
        cx.executor().allow_parking();
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
