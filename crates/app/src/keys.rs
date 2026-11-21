//! The keyboard shortcuts: the keymap, and the help view that lists it.

use gpui_kit::base::actions::Confirm;
use gpui_kit::component::{ActiveTheme, WindowExt as _, kbd::Kbd};
use gpui_kit::*;

actions!(
    keys,
    [
        SelectNext,
        SelectPrevious,
        OpenSelected,
        FocusSearch,
        FilterByProject,
        ChangeStatus,
        ChangeAssignee,
        WriteComment,
        LogTime,
        ToggleTimer,
        OpenTimerIssue,
        OpenInJira,
        SyncNow,
        EditTimeTracking,
        EditProjects,
        ShowShortcuts,
        Back
    ]
);

/// The key context of the issue list, which has the focus while the user
/// types nowhere. Every shortcut of one key is bound in it, so that typing in
/// an input, an open menu or a dialog never fires one.
pub const LIST: &str = "IssueList";
/// The key context of the whole main screen.
pub const HOME: &str = "Home";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", SelectNext, Some(LIST)),
        KeyBinding::new("down", SelectNext, Some(LIST)),
        KeyBinding::new("k", SelectPrevious, Some(LIST)),
        KeyBinding::new("up", SelectPrevious, Some(LIST)),
        KeyBinding::new("enter", OpenSelected, Some(LIST)),
        KeyBinding::new("/", FocusSearch, Some(LIST)),
        KeyBinding::new("p", FilterByProject, Some(LIST)),
        KeyBinding::new("s", ChangeStatus, Some(LIST)),
        KeyBinding::new("a", ChangeAssignee, Some(LIST)),
        KeyBinding::new("c", WriteComment, Some(LIST)),
        KeyBinding::new("w", LogTime, Some(LIST)),
        KeyBinding::new("t", ToggleTimer, Some(LIST)),
        KeyBinding::new("shift-t", OpenTimerIssue, Some(LIST)),
        KeyBinding::new("b", OpenInJira, Some(LIST)),
        KeyBinding::new("r", SyncNow, Some(LIST)),
        KeyBinding::new("cmd-,", EditTimeTracking, Some(LIST)),
        KeyBinding::new("shift-p", EditProjects, Some(LIST)),
        KeyBinding::new("?", ShowShortcuts, Some(LIST)),
        KeyBinding::new("escape", Back, Some(HOME)),
    ]);
}

/// What the help view lists, in its order. The keys are read from the keymap,
/// so the view cannot name a key that is not bound.
fn listed() -> [(Box<dyn Action>, &'static str); 17] {
    [
        (Box::new(SelectNext), "Select the next issue"),
        (Box::new(SelectPrevious), "Select the previous issue"),
        (Box::new(OpenSelected), "Open the selected issue"),
        (
            Box::new(Back),
            "Leave an input or a button, then close the issue",
        ),
        (Box::new(FocusSearch), "Search"),
        (Box::new(FilterByProject), "Filter by project"),
        (Box::new(ChangeStatus), "Change the status"),
        (Box::new(ChangeAssignee), "Change the assignee"),
        (Box::new(WriteComment), "Write a comment"),
        (Box::new(LogTime), "Log time"),
        (Box::new(ToggleTimer), "Start or stop the timer"),
        (
            Box::new(OpenTimerIssue),
            "Open the issue of the running timer",
        ),
        (Box::new(OpenInJira), "Open in Jira"),
        (Box::new(SyncNow), "Sync now"),
        (Box::new(EditTimeTracking), "Time tracking settings"),
        (Box::new(EditProjects), "Choose the projects"),
        (Box::new(ShowShortcuts), "Show this list"),
    ]
}

/// What the keymap cannot say: the keys the inputs and the buttons take themselves.
const ELSEWHERE: [(&str, &str); 5] = [
    ("Tab, ⇧Tab", "Move through the buttons and the fields"),
    ("Enter, Space", "Press the focused button"),
    ("Enter in the search", "Open the selected issue"),
    ("Enter in a time field", "Log the time"),
    ("⌘Enter in a comment", "Send the comment"),
];

fn keys_of(action: &dyn Action, cx: &App) -> Vec<Keystroke> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap
        .bindings_for_action(action)
        .filter_map(|binding| Some(binding.keystrokes().first()?.inner().clone()))
        .collect()
}

fn line(
    index: usize,
    keys: impl IntoElement,
    label: String,
    does: &'static str,
) -> impl IntoElement {
    div()
        .id(("shortcut", index))
        .test_support()
        .aria_label(label)
        .flex()
        .items_center()
        .gap_3()
        .child(div().w(px(170.)).flex_none().child(keys))
        .child(does)
}

/// Opens the list of the shortcuts.
pub fn show(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let bound = listed().into_iter().map(|(action, does)| {
            let keys = keys_of(action.as_ref(), cx);
            let names: Vec<_> = keys
                .iter()
                .map(|key| key.display(OperatingSystem::Mac).to_string())
                .collect();
            let keys = div()
                .flex()
                .gap_1()
                .children(keys.into_iter().map(Kbd::new));
            (keys.into_any_element(), names.join(", "), does)
        });
        let elsewhere = ELSEWHERE.into_iter().map(|(keys, does)| {
            let muted = div().text_color(cx.theme().muted_foreground).child(keys);
            (muted.into_any_element(), keys.to_owned(), does)
        });
        dialog.title("Keyboard shortcuts").child(
            div()
                .id("shortcuts")
                .max_h(px(420.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1p5()
                .text_sm()
                .children(bound.chain(elsewhere).enumerate().map(
                    |(index, (keys, names, does))| {
                        line(index, keys, format!("{names}: {does}"), does)
                    },
                )),
        )
    });
}

/// Opens the menu of a dropdown button, given the focus handle the button
/// tracks, as Enter on the button does. The button does not take the focus,
/// so closing the menu gives it back to where it was.
pub fn open_menu(button: &FocusHandle, window: &mut Window, cx: &mut App) {
    let button = button.clone();
    // After the action that asked for it has been dispatched
    window.defer(cx, move |window, cx| {
        button.dispatch_action(&Confirm { secondary: false }, window, cx)
    });
}

#[cfg(test)]
mod tests {
    use crate::harness::{
        in_window, launch, no_worklog_changes, tab_to, tokio_runtime, wait_until,
    };
    use crate::issue_list::tests::{issue, rows, synced_on};
    use crate::runtime::Core;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, Role, TestAppContext, Window};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, path_regex},
    };

    /// The app on WEB-1 to WEB-3, all in To Do and unassigned, with a mock Jira
    /// that offers one transition and two people for each of them.
    struct App {
        window: AnyWindowHandle,
        _tokio: tokio::runtime::Runtime,
        _jira: MockServer,
        _data_dir: tempfile::TempDir,
    }

    fn app(cx: &mut TestAppContext) -> App {
        let tokio = tokio_runtime();
        let issues = [
            issue(1, "Fix login"),
            issue(2, "Fix logout"),
            issue(3, "Broken checkout"),
        ];
        let jira = tokio.block_on(async {
            let server = MockServer::start().await;
            no_worklog_changes().mount(&server).await;
            let answers = [
                (
                    r"^/rest/api/3/issue/WEB-\d$",
                    json!({ "fields": { "status": { "name": "To Do" } } }),
                ),
                (
                    r"^/rest/api/3/issue/WEB-\d/transitions$",
                    json!({ "transitions": [{
                        "id": "21",
                        "name": "Start",
                        "to": { "name": "In Progress", "statusCategory": { "key": "indeterminate" } },
                    }] }),
                ),
                (
                    "^/rest/api/3/user/assignable/search$",
                    json!([
                        { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                        { "accountId": "7c20b", "displayName": "Grace Hopper" },
                    ]),
                ),
            ];
            for (endpoint, answer) in answers {
                Mock::given(method("GET"))
                    .and(path_regex(endpoint))
                    .respond_with(ResponseTemplate::new(200).set_body_json(answer))
                    .mount(&server)
                    .await;
            }
            Mock::given(path("/rest/api/3/search/jql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": issues })))
                .mount(&server)
                .await;
            server
        });
        let data_dir = synced_on(&tokio, &jira.uri(), &issues);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| rows(window).len() == 3);
        App {
            window,
            _tokio: tokio,
            _jira: jira,
            _data_dir: data_dir,
        }
    }

    fn press(cx: &mut TestAppContext, app: &App, keys: &[&str]) {
        for key in keys {
            in_window(cx, app.window, |window, cx| window.press(key, cx));
            cx.run_until_parked();
        }
    }

    /// The key of the row the keyboard is on.
    fn selected(window: &Window) -> Option<String> {
        let rows = window.find_all_by_role(Role::ListItem);
        let row = rows.iter().find(|row| row.selected() == Some(true))?;
        Some(row.label()?.split(':').next()?.to_owned())
    }

    fn opened(window: &Window) -> Option<String> {
        Some(window.try_find("issue-detail")?.label()?.to_owned())
    }

    /// Opens the top issue with the keyboard and waits for its menus.
    fn open_top(cx: &mut TestAppContext, app: &App) {
        press(cx, app, &["j", "enter"]);
        wait_until(cx, app.window, |window| {
            ["status-button", "assignee-button"].iter().all(|button| {
                window
                    .try_find(*button)
                    .is_some_and(|button| button.disabled() != Some(true))
            })
        });
    }

    fn menu(window: &Window) -> Vec<String> {
        window
            .find_all_by_role(Role::MenuItem)
            .iter()
            .map(|item| item.label().unwrap().to_owned())
            .collect()
    }

    fn label(window: &Window, id: &'static str) -> String {
        window.find(id).label().unwrap().to_owned()
    }

    #[gpui_kit::test]
    fn the_list_is_walked_and_an_issue_is_opened_and_closed(cx: &mut TestAppContext) {
        let app = app(cx);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window), None)
        });

        press(cx, &app, &["j"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-3"))
        });
        press(cx, &app, &["j", "down"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-1"))
        });
        // The list ends here
        press(cx, &app, &["j", "k"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-2"))
        });
        press(cx, &app, &["up", "up"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-3"));
            assert_eq!(opened(window), None);
        });

        press(cx, &app, &["j", "enter"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-2"))
        });
        // The list keeps the keys while the issue is open
        press(cx, &app, &["j", "enter"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-1"))
        });

        press(cx, &app, &["escape"]);
        in_window(cx, app.window, |window, _| assert_eq!(opened(window), None));
    }

    #[gpui_kit::test]
    fn the_search_is_reached_with_a_key_and_enter_opens_what_it_found(cx: &mut TestAppContext) {
        let app = app(cx);

        press(cx, &app, &["/"]);
        in_window(cx, app.window, |window, cx| {
            assert_eq!(window.find("search").focused(), Some(true));
            window.input("checkout", cx);
        });
        wait_until(cx, app.window, |window| rows(window).len() == 1);
        press(cx, &app, &["enter"]);

        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-3"));
            assert_eq!(window.find("search").focused(), Some(false));
        });
        // Enter gave the keys back to the list
        press(cx, &app, &["escape"]);
        in_window(cx, app.window, |window, _| assert_eq!(opened(window), None));
    }

    #[gpui_kit::test]
    fn no_shortcut_fires_while_the_user_types(cx: &mut TestAppContext) {
        let app = app(cx);
        let sync_requests = cx.update(|cx| cx.global::<Core>().state.sync_requests());
        open_top(cx, &app);
        let every_key = [
            "j", "k", "down", "up", "/", "p", "s", "a", "c", "w", "t", "shift-t", "b", "r",
            "cmd-,", "shift-p", "?",
        ];
        for (clicked, input) in [
            ("worklog-duration", "Time spent"),
            ("comment-input", "New comment"),
            ("search", "Search issues"),
        ] {
            in_window(cx, app.window, |window, cx| window.click(clicked, cx));
            press(cx, &app, &every_key);
            in_window(cx, app.window, |window, cx| {
                // The keys that are characters were typed
                assert_eq!(window.find_by_label(input).value(), Some("jk/psacwtTbrP?"));
                assert_eq!(opened(window).as_deref(), Some("WEB-3"));
                assert!(menu(window).is_empty());
                assert!(!window.has_active_dialog(cx));
                assert!(window.try_find("stop-timer").is_none());
                assert!(window.try_find("home").is_some());
            });
            assert_eq!(cx.opened_url(), None);
        }
        assert!(!sync_requests.has_changed().unwrap());

        // Escape leaves the input, and the keys are shortcuts again
        press(cx, &app, &["escape"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-3"))
        });
        press(cx, &app, &["t"]);
        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("stop-timer").is_some())
        });
    }

    #[gpui_kit::test]
    fn the_status_and_the_assignee_are_changed_from_their_menus(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);

        press(cx, &app, &["s"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(menu(window), ["Start"])
        });
        press(cx, &app, &["down", "enter"]);
        wait_until(cx, app.window, |window| {
            label(window, "detail-status") == "Status: In Progress"
        });

        // The menu gave the keys back to the list when it closed
        press(cx, &app, &["a"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(menu(window), ["Assign to me", "Unassigned", "Grace Hopper"])
        });
        press(cx, &app, &["down", "enter"]);
        wait_until(cx, app.window, |window| {
            label(window, "detail-assignee") == "Assignee: Ada Lovelace"
        });

        // Escape closes a menu and not the issue under it
        press(cx, &app, &["a", "escape"]);
        in_window(cx, app.window, |window, _| {
            assert!(menu(window).is_empty());
            assert_eq!(opened(window).as_deref(), Some("WEB-3"));
        });
    }

    #[gpui_kit::test]
    fn time_is_logged_from_the_keyboard(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);

        press(cx, &app, &["w"]);
        in_window(cx, app.window, |window, cx| window.input("1h 30m", cx));
        press(cx, &app, &["enter"]);

        wait_until(cx, app.window, |window| {
            window
                .try_find("worklog-0")
                .is_some_and(|worklog| worklog.label().unwrap().ends_with("1h 30m, pending"))
        });
    }

    #[gpui_kit::test]
    fn the_timer_is_started_found_and_stopped_from_the_keyboard(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);

        press(cx, &app, &["t", "escape"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window), None);
            assert_eq!(label(window, "running-timer-issue"), "WEB-3");
        });

        press(cx, &app, &["shift-t"]);
        wait_until(cx, app.window, |window| {
            window.try_find("stop-timer").is_some()
        });
        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-3"))
        });

        press(cx, &app, &["t"]);
        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("start-timer").is_some());
            assert!(window.try_find("running-timer-issue").is_none());
        });
    }

    #[gpui_kit::test]
    fn the_issue_is_opened_in_jira_and_a_sync_is_asked_for(cx: &mut TestAppContext) {
        let app = app(cx);
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        let sync_requests = state.sync_requests();
        // Without an open issue there is nothing to open
        press(cx, &app, &["b"]);
        assert_eq!(cx.opened_url(), None);
        open_top(cx, &app);

        press(cx, &app, &["b"]);
        let url = cx.opened_url().unwrap();
        assert!(url.ends_with("/browse/WEB-3"), "{url}");

        assert!(!sync_requests.has_changed().unwrap());
        press(cx, &app, &["r"]);
        assert!(sync_requests.has_changed().unwrap());
    }

    #[gpui_kit::test]
    fn the_settings_and_the_projects_open_with_a_key(cx: &mut TestAppContext) {
        let app = app(cx);

        press(cx, &app, &["cmd-,"]);
        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("worklog-target").is_some())
        });
        tab_to(cx, app.window, "close-settings");
        press(cx, &app, &["enter"]);

        wait_until(cx, app.window, |window| window.try_find("home").is_some());
        press(cx, &app, &["shift-p"]);
        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("choose-projects").is_some())
        });
        tab_to(cx, app.window, "cancel-projects");
        press(cx, &app, &["space"]);

        // The list has the keys again after either screen
        wait_until(cx, app.window, |window| window.try_find("home").is_some());
        press(cx, &app, &["j"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-3"))
        });
    }

    #[gpui_kit::test]
    fn the_list_takes_the_keys_back_when_the_focused_input_goes_away(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);
        in_window(cx, app.window, |window, cx| {
            window.click("comment-input", cx);
            window.input("Seen", cx);
            window.click("close-detail", cx);
        });

        press(cx, &app, &["j", "enter"]);

        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-2"))
        });
    }

    #[gpui_kit::test]
    fn a_click_on_an_issue_gives_the_keys_to_the_list(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);
        in_window(cx, app.window, |window, cx| {
            window.click("comment-input", cx);
            window.click("issue-WEB-1", cx);
        });

        press(cx, &app, &["k"]);

        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-2"));
            assert_eq!(window.find_by_label("New comment").value(), Some(""));
        });
    }

    #[gpui_kit::test]
    fn tab_reaches_the_first_field_of_a_screen_nothing_was_clicked_on(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());

        in_window(cx, window, |window, cx| window.press("tab", cx));

        in_window(cx, window, |window, _| {
            assert_eq!(window.find("site-url").focused(), Some(true))
        });
    }

    fn shortcuts(window: &Window) -> Vec<String> {
        (0usize..)
            .map_while(|index| window.try_find(("shortcut", index)))
            .map(|line| line.label().unwrap().to_owned())
            .collect()
    }

    #[gpui_kit::test]
    fn the_help_lists_every_shortcut_with_the_key_that_is_bound_to_it(cx: &mut TestAppContext) {
        let app = app(cx);

        press(cx, &app, &["?"]);

        let listed = [
            "J, ↓: Select the next issue",
            "K, ↑: Select the previous issue",
            "enter: Open the selected issue",
            "⎋: Leave an input or a button, then close the issue",
            "/: Search",
            "P: Filter by project",
            "S: Change the status",
            "A: Change the assignee",
            "C: Write a comment",
            "W: Log time",
            "T: Start or stop the timer",
            "⇧T: Open the issue of the running timer",
            "B: Open in Jira",
            "R: Sync now",
            "⌘,: Time tracking settings",
            "⇧P: Choose the projects",
            "?: Show this list",
            "Tab, ⇧Tab: Move through the buttons and the fields",
            "Enter, Space: Press the focused button",
            "Enter in the search: Open the selected issue",
            "Enter in a time field: Log the time",
            "⌘Enter in a comment: Send the comment",
        ];
        in_window(cx, app.window, |window, cx| {
            assert!(window.has_active_dialog(cx));
            assert_eq!(shortcuts(window), listed);
        });
        press(cx, &app, &["escape"]);
        in_window(cx, app.window, |window, cx| {
            assert!(!window.has_active_dialog(cx));
            // The button in the status bar opens the same list
            window.click("show-shortcuts", cx);
            window.render_frame(cx);
            assert_eq!(shortcuts(window), listed);
        });
    }
}
