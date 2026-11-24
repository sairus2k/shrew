//! The item in the macOS menu bar: an icon whose menu has the running timer
//! with its issue and its start, a stop for it or the start of a timer
//! without an issue, and the way back to the window.

use crate::{menu::Quit, runtime::spawn, timer::ActiveTimer};
use app_core::api;
use gpui_kit::*;
use gpui_tray::{Icon, Tray};

actions!(tray, [StartTimer, StopTimer, ShowWindow]);

/// What the menu says about the timer.
#[derive(Clone, Debug, Default, PartialEq)]
enum TimerLine {
    /// No account is signed in, so no timer can run.
    #[default]
    Absent,
    Stopped,
    /// The line of the running timer.
    Running(String),
}

#[derive(Default)]
struct MenuBarItem {
    // Absent in tests: only main() puts an item into the menu bar
    native: Option<Tray>,
    // A strong handle: the timer goes on in the menu bar while the window
    // shows another screen or is closed
    timer: Option<Entity<ActiveTimer>>,
    // The line of the timer the menu was last built with
    line: TimerLine,
    _timer_changed: Option<Subscription>,
}

impl Global for MenuBarItem {}

/// Installs what the items of the menu do.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &StartTimer, cx| {
        if let Some(timer) = timer(cx) {
            timer.update(cx, |timer, cx| {
                if timer.running().is_none() {
                    timer.start_anew(None, cx)
                }
            });
        }
    });
    cx.on_action(|_: &StopTimer, cx| {
        if let Some(timer) = timer(cx) {
            timer.update(cx, |timer, cx| timer.stop(cx));
            // A timer without an issue asks for one in the window
            if timer.read(cx).stopping().is_some() {
                show_window(cx);
            }
        }
    });
    cx.on_action(|_: &ShowWindow, cx| show_window(cx));
}

/// Puts the item into the menu bar.
pub fn install(cx: &mut App) -> gpui_tray::Result<()> {
    // A template image, which macOS colors for the menu bar it is in
    #[cfg(target_os = "macos")]
    let icon = Icon::from_gpui(
        &Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../../../assets/icons/menuBarIconTemplate@2x.png").to_vec(),
        ),
        cx,
    )
    .scale_factor(2.0)
    .template(true)
    .build()?;
    // No other system colors an icon, and the black template cannot be seen
    // on a dark taskbar
    #[cfg(not(target_os = "macos"))]
    let icon = Icon::from_gpui(
        &Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../../../assets/icons/app-icon.svg").to_vec(),
        ),
        cx,
    )
    .build()?;
    let native = Tray::builder()
        .icon(icon)
        .tooltip("Shrew")
        .menu(|cx| items(cx.default_global::<MenuBarItem>().line.clone()))
        .build(cx)?;
    cx.default_global::<MenuBarItem>().native = Some(native);
    Ok(())
}

/// The timer the menu bar shows: a window opened anew takes it over.
pub fn timer(cx: &mut App) -> Option<Entity<ActiveTimer>> {
    cx.default_global::<MenuBarItem>().timer.clone()
}

/// Shows `timer` in the menu bar from now on, in place of the one before.
pub fn follow(timer: Entity<ActiveTimer>, cx: &mut App) {
    let changed = cx.observe(&timer, |_, cx| refresh(cx));
    let item = cx.default_global::<MenuBarItem>();
    item.timer = Some(timer);
    item._timer_changed = Some(changed);
    refresh(cx);
}

/// Stops showing a timer: the account it belongs to signed out.
pub fn forget(cx: &mut App) {
    let item = cx.default_global::<MenuBarItem>();
    item.timer = None;
    item._timer_changed = None;
    refresh(cx);
}

fn refresh(cx: &mut App) {
    let line = match timer(cx) {
        None => TimerLine::Absent,
        Some(timer) => timer
            .read(cx)
            .caption()
            .map_or(TimerLine::Stopped, TimerLine::Running),
    };
    let item = cx.default_global::<MenuBarItem>();
    if item.line == line {
        return;
    }
    item.line = line;
    // Only on a start and a stop, which is why the line names the start and
    // not the time elapsed: gpui-tray drops a click on a menu that was rebuilt
    // while it was open, so a menu rebuilt every second cannot be clicked.
    if let Some(native) = item.native.clone() {
        native.refresh_menu(cx).ok();
    }
}

// The time is not beside the icon: the menu bar gives a title digits of
// different widths, so a clock there moves its neighbours every second, and
// with the issue key it is wider than the space a crowded menu bar has.
fn items(timer: TimerLine) -> Vec<MenuItem> {
    let mut items = Vec::new();
    match timer {
        TimerLine::Absent => {}
        TimerLine::Stopped => {
            items.push(MenuItem::action("Start Timer", StartTimer));
            items.push(MenuItem::separator());
        }
        TimerLine::Running(caption) => {
            items.push(MenuItem::action(caption, ShowWindow).disabled(true));
            items.push(MenuItem::action("Stop Timer", StopTimer));
            items.push(MenuItem::separator());
        }
    }
    items.extend([
        MenuItem::action("Open Shrew", ShowWindow),
        MenuItem::separator(),
        MenuItem::action("Quit Shrew", Quit),
    ]);
    items
}

/// Brings the window forward, and opens a new one when the user closed it:
/// the app goes on without a window.
fn show_window(cx: &mut App) {
    cx.activate(true);
    if let Some(window) = cx.windows().first().copied() {
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        return;
    }
    let step = spawn(cx, |state| async move { api::setup_step(&state).await });
    cx.spawn(async move |cx| {
        if let Ok(step) = step.await {
            cx.update(|cx| {
                // A second click may have opened it while the core answered
                if cx.windows().is_empty() {
                    crate::open_main_window(step, cx);
                }
            });
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{MenuBarItem, ShowWindow, StartTimer, StopTimer, TimerLine, items};
    use crate::harness::{in_window, launch_on_jira, wait_until};
    use crate::issue_list::tests::issue;
    use crate::menu::Quit;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{
        Action, AnyWindowHandle, AppContext, MenuItem, TestAppContext, VisualTestContext,
    };

    fn names(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                MenuItem::Action { name, .. } => name.to_string(),
                _ => "-".to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_menu_offers_a_start_while_no_timer_runs_and_a_stop_while_one_does() {
        // Signed out
        assert_eq!(
            names(&items(TimerLine::Absent)),
            ["Open Shrew", "-", "Quit Shrew"]
        );
        let stopped = items(TimerLine::Stopped);
        assert_eq!(
            names(&stopped),
            ["Start Timer", "-", "Open Shrew", "-", "Quit Shrew"]
        );
        assert!(
            matches!(&stopped[0], MenuItem::Action { action, .. } if action.partial_eq(&StartTimer))
        );

        let running = items(TimerLine::Running("WEB-1 since 14:05".to_owned()));

        assert_eq!(
            names(&running),
            [
                "WEB-1 since 14:05",
                "Stop Timer",
                "-",
                "Open Shrew",
                "-",
                "Quit Shrew"
            ]
        );
        // The line of the timer is not a command
        assert!(running[0].is_disabled());
        assert!(!running[1].is_disabled());
        let expected: [&dyn Action; 4] = [&ShowWindow, &StopTimer, &ShowWindow, &Quit];
        let actions = running.iter().filter_map(|item| match item {
            MenuItem::Action { action, .. } => Some(action),
            _ => None,
        });
        for (action, expected) in actions.zip(expected) {
            assert!(action.partial_eq(expected));
        }
    }

    fn start_timer_on_web_1(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("issue-WEB-1").is_some()
        });
        in_window(cx, window, |window, cx| window.click("issue-WEB-1", cx));
        wait_until(cx, window, |window| {
            window.try_find("start-timer").is_some()
        });
        in_window(cx, window, |window, cx| window.click("start-timer", cx));
    }

    // The line of the timer in the menu of the item
    fn shown(cx: &mut TestAppContext) -> Option<String> {
        cx.update(|cx| match &cx.default_global::<MenuBarItem>().line {
            TimerLine::Running(caption) => Some(caption.clone()),
            _ => None,
        })
    }

    #[gpui_kit::test]
    fn the_menu_shows_the_running_timer_and_its_stop_stops_it(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login")]);
        assert_eq!(shown(cx), None);

        start_timer_on_web_1(cx, app.window);
        cx.run_until_parked();

        assert!(shown(cx).unwrap().starts_with("WEB-1 since "));

        cx.update(|cx| cx.dispatch_action(&StopTimer));
        cx.run_until_parked();

        assert_eq!(shown(cx), None);
        wait_until(cx, app.window, |window| {
            window.try_find("start-timer").is_some()
        });
    }

    #[gpui_kit::test]
    fn the_timer_stays_in_the_menu_bar_after_the_window_is_closed(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login")]);
        start_timer_on_web_1(cx, app.window);
        cx.run_until_parked();

        cx.update_window(app.window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();

        assert!(shown(cx).unwrap().starts_with("WEB-1 since "));
    }

    #[gpui_kit::test]
    fn open_shrew_opens_a_window_after_the_user_closed_it(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login")]);

        cx.update(|cx| cx.dispatch_action(&ShowWindow));
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 1);

        // The test platform goes on naming a removed window as the active one
        VisualTestContext::from_window(app.window, cx).deactivate_window();
        cx.update_window(app.window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), 0);

        cx.update(|cx| cx.dispatch_action(&ShowWindow));
        let started = std::time::Instant::now();
        while cx.update(|cx| cx.windows().is_empty()) {
            assert!(started.elapsed().as_secs() < 5, "no window was opened");
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let window = cx.update(|cx| cx.windows()[0]);
        wait_until(cx, window, |window| window.try_find("home").is_some());
    }

    fn start_without_issue_from_the_menu(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("start-timer-without-issue").is_some()
        });
        cx.update(|cx| cx.dispatch_action(&StartTimer));
        cx.run_until_parked();
    }

    // A timer of a minute and more has time to give to an issue; the menu
    // has no clock to wait on
    fn backdate_the_running_timer(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let timer = cx.default_global::<MenuBarItem>().timer.clone().unwrap();
            timer.update(cx, |timer, _| timer.backdate(600));
        });
    }

    #[gpui_kit::test]
    fn the_menu_starts_a_timer_without_an_issue(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login")]);

        start_without_issue_from_the_menu(cx, app.window);

        assert!(shown(cx).unwrap().starts_with("No issue chosen since "));
        wait_until(cx, app.window, |window| {
            window
                .try_find("running-timer-issue")
                .is_some_and(|issue| issue.label() == Some("No issue chosen"))
        });
    }

    #[gpui_kit::test]
    fn a_stop_from_the_menu_asks_for_the_issue_in_the_window(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login")]);
        start_without_issue_from_the_menu(cx, app.window);
        backdate_the_running_timer(cx);

        cx.update(|cx| cx.dispatch_action(&StopTimer));

        wait_until(cx, app.window, |window| {
            window.try_find("pick-WEB-1").is_some()
        });
        // Still running until the issue is chosen
        assert!(shown(cx).is_some());
    }

    #[gpui_kit::test]
    fn a_stop_from_the_menu_with_the_window_closed_opens_it_with_the_question(
        cx: &mut TestAppContext,
    ) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login")]);
        start_without_issue_from_the_menu(cx, app.window);
        backdate_the_running_timer(cx);
        VisualTestContext::from_window(app.window, cx).deactivate_window();
        cx.update_window(app.window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();

        cx.update(|cx| cx.dispatch_action(&StopTimer));

        let started = std::time::Instant::now();
        while cx.update(|cx| cx.windows().is_empty()) {
            assert!(started.elapsed().as_secs() < 5, "no window was opened");
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let window = cx.update(|cx| cx.windows()[0]);
        wait_until(cx, window, |window| window.try_find("pick-WEB-1").is_some());
        in_window(cx, window, |window, cx| window.click("pick-WEB-1", cx));
        cx.run_until_parked();
        wait_until(cx, window, |window| {
            window.try_find("start-timer-without-issue").is_some()
        });
        assert_eq!(shown(cx), None);
    }
}
