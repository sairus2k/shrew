//! The application menu in the macOS menu bar.

use gpui_kit::*;

actions!(menu, [Hide, HideOthers, ShowAll, Quit]);

/// Installs the Shrew menu and the shortcuts of its items.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    // Bound before the menu is set: the menu reads the shortcut of each item
    // from the keymap. No key context, so they also work in a text input.
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.set_menus([Menu::new("Shrew").items([
        MenuItem::action("Hide Shrew", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit Shrew", Quit),
    ])]);
}

#[cfg(test)]
mod tests {
    use super::{Hide, HideOthers, Quit, ShowAll};
    use crate::harness::{in_window, launch, tokio_runtime};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{Action, OwnedMenuItem, TestAppContext};
    use std::{cell::Cell, rc::Rc};

    #[gpui_kit::test]
    fn the_menu_bar_has_a_shrew_menu_with_hide_and_quit(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        launch(cx, &tokio, data_dir.path());

        let menus = cx.update(|cx| cx.get_menus()).unwrap();

        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].name, "Shrew");
        let expected: [(&str, &dyn Action); 4] = [
            ("Hide Shrew", &Hide),
            ("Hide Others", &HideOthers),
            ("Show All", &ShowAll),
            ("Quit Shrew", &Quit),
        ];
        let items: Vec<_> = menus[0]
            .items
            .iter()
            .filter_map(|item| match item {
                OwnedMenuItem::Action { name, action, .. } => Some((name.as_str(), action)),
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), expected.len());
        for ((name, action), (expected_name, expected_action)) in items.into_iter().zip(expected) {
            assert_eq!(name, expected_name);
            assert!(action.partial_eq(expected_action), "{name}");
        }
    }

    #[gpui_kit::test]
    fn cmd_q_quits_while_a_text_input_has_focus(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());
        // The test platform ignores a quit, so the test listens for the action
        // instead. The listener registered last is the one that runs.
        let quit = Rc::new(Cell::new(false));
        let seen = quit.clone();
        cx.update(|cx| {
            cx.on_action(move |_: &Quit, _| seen.set(true));
        });
        in_window(cx, window, |window, cx| {
            window.click("site-url", cx);
            window.input("typed", cx);
        });

        cx.simulate_keystrokes(window, "cmd-q");

        assert!(quit.get());
    }
}
