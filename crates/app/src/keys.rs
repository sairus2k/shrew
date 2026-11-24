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
        FilterByAssignee,
        FilterByStatus,
        FilterBySprint,
        ChangeStatus,
        ChangeAssignee,
        WriteComment,
        LogTime,
        ToggleTimer,
        StartOrStopTimer,
        OpenTimerIssue,
        OpenInJira,
        SyncNow,
        EditUserSettings,
        ShowShortcuts,
        Back
    ]
);

/// Whether the controls show the key of their shortcut. The core keeps the
/// choice; this is its copy for the render, which cannot wait for the core.
pub struct Hints(pub bool);

impl Global for Hints {}

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
        // `a` and `s` change the open issue
        KeyBinding::new("shift-a", FilterByAssignee, Some(LIST)),
        KeyBinding::new("shift-s", FilterByStatus, Some(LIST)),
        // `r` syncs
        KeyBinding::new("shift-r", FilterBySprint, Some(LIST)),
        KeyBinding::new("s", ChangeStatus, Some(LIST)),
        KeyBinding::new("a", ChangeAssignee, Some(LIST)),
        KeyBinding::new("c", WriteComment, Some(LIST)),
        KeyBinding::new("w", LogTime, Some(LIST)),
        KeyBinding::new("t", ToggleTimer, Some(LIST)),
        KeyBinding::new("n", StartOrStopTimer, Some(LIST)),
        KeyBinding::new("shift-t", OpenTimerIssue, Some(LIST)),
        KeyBinding::new("b", OpenInJira, Some(LIST)),
        KeyBinding::new("r", SyncNow, Some(LIST)),
        KeyBinding::new("cmd-,", EditUserSettings, Some(LIST)),
        KeyBinding::new("?", ShowShortcuts, Some(LIST)),
        KeyBinding::new("escape", Back, Some(HOME)),
    ]);
}

/// What the help view lists, in its order. The keys are read from the keymap,
/// so the view cannot name a key that is not bound.
fn listed() -> [(Box<dyn Action>, &'static str); 20] {
    [
        (Box::new(SelectNext), "Next issue"),
        (Box::new(SelectPrevious), "Previous issue"),
        (Box::new(OpenSelected), "Open the selected issue"),
        (
            Box::new(Back),
            "Leave an input or a button, then close the issue",
        ),
        (Box::new(FocusSearch), "Search"),
        (Box::new(FilterByProject), "Filter by project"),
        (Box::new(FilterByAssignee), "Filter by assignee"),
        (Box::new(FilterByStatus), "Filter by status"),
        (Box::new(FilterBySprint), "Filter by sprint"),
        (Box::new(ChangeStatus), "Change the status"),
        (Box::new(ChangeAssignee), "Change the assignee"),
        (Box::new(WriteComment), "Write a comment"),
        (Box::new(LogTime), "Log time"),
        (Box::new(ToggleTimer), "Start or stop the timer"),
        (
            Box::new(StartOrStopTimer),
            "Start a timer without an issue, or stop the running one",
        ),
        (
            Box::new(OpenTimerIssue),
            "Open or choose the issue of the running timer",
        ),
        (Box::new(OpenInJira), "Open in Jira"),
        (Box::new(SyncNow), "Sync now"),
        (Box::new(EditUserSettings), "Settings"),
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

/// The key of `action`, to show in the control that does the same, or nothing
/// when the user turned the hints off. With several keys bound, the first.
pub fn hint(action: &dyn Action, cx: &App) -> Option<AnyElement> {
    if !cx.global::<Hints>().0 {
        return None;
    }
    let key = keys_of(action, cx).into_iter().next()?;
    Some(
        div()
            .id(SharedString::from(format!("hint:{}", action.name())))
            .test_support()
            .aria_label(key.display(OperatingSystem::Mac).to_string())
            .flex_none()
            .child(Kbd::new(key))
            .into_any_element(),
    )
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
    use super::{ChangeStatus, LIST};
    use crate::harness::{
        close_settings, in_window, launch, no_worklog_changes, tokio_runtime, wait_until,
    };
    use crate::issue_list::tests::{issue, rows, synced_on};
    use crate::runtime::Core;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::{TestQueryExt, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, KeyBinding, Role, TestAppContext, Window};
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
            crate::harness::no_sprint_field().mount(&server).await;
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
        // With an issue open the selection takes the pane along
        press(cx, &app, &["j"]);
        wait_until(cx, app.window, |window| {
            opened(window).as_deref() == Some("WEB-1")
        });
        press(cx, &app, &["k"]);
        wait_until(cx, app.window, |window| {
            opened(window).as_deref() == Some("WEB-2")
        });

        press(cx, &app, &["escape"]);
        in_window(cx, app.window, |window, _| assert_eq!(opened(window), None));
        // And without one it moves alone again
        press(cx, &app, &["j"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(selected(window).as_deref(), Some("WEB-1"));
            assert_eq!(opened(window), None);
        });
    }

    #[gpui_kit::test]
    fn a_key_at_the_end_of_the_list_keeps_the_open_issue_and_its_unsent_comment(
        cx: &mut TestAppContext,
    ) {
        let app = app(cx);
        open_top(cx, &app);
        in_window(cx, app.window, |window, cx| {
            window.click("comment-input", cx);
            window.input("Seen", cx);
        });

        // WEB-3 is the first row
        press(cx, &app, &["escape", "k"]);

        in_window(cx, app.window, |window, _| {
            assert_eq!(opened(window).as_deref(), Some("WEB-3"));
            assert_eq!(window.find_by_label("New comment").value(), Some("Seen"));
        });
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
            "j", "k", "down", "up", "/", "p", "shift-a", "shift-s", "shift-r", "s", "a", "c", "w",
            "t", "shift-t", "b", "r", "cmd-,", "?",
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
                assert_eq!(
                    window.find_by_label(input).value(),
                    Some("jk/pASRsacwtTbr?")
                );
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

        // A request made while a cycle runs would start one more after it
        let mut sync_requests = sync_requests;
        sync_requests.mark_unchanged();
        state.emit(app_core::SyncEvent::Started);
        cx.run_until_parked();
        press(cx, &app, &["r"]);
        assert!(!sync_requests.has_changed().unwrap());
    }

    #[gpui_kit::test]
    fn the_settings_open_with_a_key(cx: &mut TestAppContext) {
        let app = app(cx);

        press(cx, &app, &["cmd-,"]);
        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("worklog-target").is_some())
        });
        close_settings(cx, app.window);

        // The list has the keys again after the settings
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

        // The pane of WEB-2 has its form once the core has given the issue
        wait_until(cx, app.window, |window| {
            window.try_find_by_label("New comment").is_some()
        });
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

    /// The hints on screen: the action each stands for and the key it shows.
    fn hints(window: &Window) -> Vec<(String, String)> {
        let mut hints: Vec<_> = window
            .elements()
            .iter()
            .filter_map(|element| {
                let action = element.path().last()?.to_string();
                let action = action.strip_prefix("hint:keys::")?.to_owned();
                Some((action, element.label()?.to_owned()))
            })
            .collect();
        hints.sort();
        hints
    }

    #[gpui_kit::test]
    fn every_control_with_a_shortcut_shows_the_key_bound_to_it(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);
        // The running timer adds its issue to the status bar
        press(cx, &app, &["t"]);

        let shown = in_window(cx, app.window, |window, _| hints(window));

        let expected = [
            ("Back", "⎋"),
            ("ChangeAssignee", "A"),
            ("ChangeStatus", "S"),
            ("EditUserSettings", "⌘,"),
            ("FilterByAssignee", "⇧A"),
            ("FocusSearch", "/"),
            ("LogTime", "W"),
            ("OpenInJira", "B"),
            ("OpenTimerIssue", "⇧T"),
            ("StartOrStopTimer", "N"),
            ("ToggleTimer", "T"),
            ("WriteComment", "C"),
        ];
        let shown: Vec<_> = shown
            .iter()
            .map(|(action, key)| (action.as_str(), key.as_str()))
            .collect();
        assert_eq!(shown, expected);
    }

    #[gpui_kit::test]
    fn a_hint_shows_the_key_the_keymap_binds_now(cx: &mut TestAppContext) {
        let app = app(cx);
        open_top(cx, &app);

        cx.update(|cx| {
            cx.clear_key_bindings();
            cx.bind_keys([KeyBinding::new("x", ChangeStatus, Some(LIST))]);
        });

        // An action without a key has no hint
        in_window(cx, app.window, |window, _| {
            assert_eq!(hints(window), [("ChangeStatus".to_owned(), "X".to_owned())])
        });
    }

    #[gpui_kit::test]
    fn the_hints_are_turned_off_in_the_settings_and_the_shortcuts_keep_working(
        cx: &mut TestAppContext,
    ) {
        let app = app(cx);
        let state = cx.update(|cx| cx.global::<Core>().state.clone());

        press(cx, &app, &["cmd-,"]);
        in_window(cx, app.window, |window, cx| {
            assert_eq!(window.find("shortcut-hints").checked(), Some(true));
            window.click("shortcut-hints", cx);
        });
        wait_until(cx, app.window, |window| {
            window.find("shortcut-hints").checked() == Some(false)
        });
        // The home screen under the settings has lost them already
        in_window(cx, app.window, |window, _| {
            assert!(hints(window).is_empty())
        });
        close_settings(cx, app.window);

        // The core has it, and the next start reads it from there
        assert!(!app._tokio.block_on(app_core::api::shortcut_hints(&state)));
        open_top(cx, &app);
        press(cx, &app, &["t"]);
        in_window(cx, app.window, |window, _| {
            assert!(hints(window).is_empty());
            assert!(window.try_find("stop-timer").is_some());
        });
        press(cx, &app, &["s"]);
        in_window(cx, app.window, |window, _| {
            assert_eq!(menu(window), ["Start"])
        });
        press(cx, &app, &["escape", "?"]);
        in_window(cx, app.window, |window, cx| {
            assert!(window.has_active_dialog(cx))
        });
        press(cx, &app, &["escape"]);

        // And back on
        press(cx, &app, &["cmd-,"]);
        in_window(cx, app.window, |window, cx| {
            window.click("shortcut-hints", cx)
        });
        wait_until(cx, app.window, |window| {
            window.find("shortcut-hints").checked() == Some(true)
        });
        in_window(cx, app.window, |window, _| {
            assert!(!hints(window).is_empty())
        });
        close_settings(cx, app.window);
    }

    #[gpui_kit::test]
    fn an_app_started_with_the_hints_turned_off_shows_none(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let issues = [issue(1, "Fix login")];
        let data_dir = synced_on(&tokio, "https://127.0.0.1:1", &issues);
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir.path())
                .await
                .unwrap();
            app_core::api::set_shortcut_hints(&state, false)
                .await
                .unwrap();
        });

        let window = launch(cx, &tokio, data_dir.path());

        wait_until(cx, window, |window| rows(window).len() == 1);
        in_window(cx, window, |window, _| {
            assert!(window.try_find("edit-settings").is_some());
            assert!(hints(window).is_empty());
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
            "J, ↓: Next issue",
            "K, ↑: Previous issue",
            "enter: Open the selected issue",
            "⎋: Leave an input or a button, then close the issue",
            "/: Search",
            "P: Filter by project",
            "⇧A: Filter by assignee",
            "⇧S: Filter by status",
            "⇧R: Filter by sprint",
            "S: Change the status",
            "A: Change the assignee",
            "C: Write a comment",
            "W: Log time",
            "T: Start or stop the timer",
            "N: Start a timer without an issue, or stop the running one",
            "⇧T: Open or choose the issue of the running timer",
            "B: Open in Jira",
            "R: Sync now",
            "⌘,: Settings",
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
