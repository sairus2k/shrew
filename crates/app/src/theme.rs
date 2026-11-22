//! The look of the app: its own light and dark palette over the GPUI Kit
//! defaults, and the pieces of style several views share.

use crate::runtime::Core;
use app_core::models::StatusCategory;
use gpui_kit::component::{ActiveTheme, Sizable, Theme, ThemeSet, spinner::Spinner};
use gpui_kit::*;
use std::{rc::Rc, sync::Arc};

/// Installs the palette for both modes and takes the mode of the system.
pub fn init(cx: &mut App) {
    let set: ThemeSet =
        serde_json::from_str(include_str!("theme.json")).expect("theme.json is not a theme set");
    Theme::update(cx, |theme| {
        for config in set.themes {
            if config.mode.is_dark() {
                theme.dark_theme = Rc::new(config);
            } else {
                theme.light_theme = Rc::new(config);
            }
        }
    });
    Theme::sync_system_appearance(None, cx);
}

/// Digits of one width, so that a number that changes does not move what is
/// beside it.
pub fn tabular_digits() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)]))
}

/// The category of a status as a colored dot: the status names differ per
/// project, the three categories do not.
pub fn status_dot(category: StatusCategory, cx: &App) -> Div {
    let color = match category {
        StatusCategory::Todo => cx.theme().muted_foreground.opacity(0.6),
        StatusCategory::InProgress => cx.theme().blue,
        StatusCategory::Done => cx.theme().green,
    };
    div().size(px(8.)).flex_none().rounded_full().bg(color)
}

/// The mark of a change Jira does not have yet: an arrow while the change
/// waits in the outbox, a spinner while a sync cycle is sending the outbox.
// A change made offline can wait for hours, and a spinner that turns all
// that time would say that something is going on
pub fn pending_mark(cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    if cx.global::<Core>().sync.read(cx).sending {
        Spinner::new().xsmall().color(muted).into_any_element()
    } else {
        div().text_color(muted).child("↑").into_any_element()
    }
}

/// A state of an entry that is not its content: "Not sent".
pub fn badge(text: &'static str, cx: &App) -> Div {
    div()
        .px_1p5()
        .rounded_full()
        .border_1()
        .border_color(cx.theme().input)
        .text_size(px(11.))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}
