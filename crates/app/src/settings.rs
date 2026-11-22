//! The settings, one page: the account, the watched projects, where logged
//! time goes, Jira or Tempo, with the Tempo API token, and whether the
//! controls show the keys of their shortcuts.

use crate::account::Account;
use crate::keys::Hints;
use crate::projects::ProjectPicker;
use crate::runtime::spawn;
use app_core::{
    api,
    models::{WorklogSettings, WorklogTarget},
};
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonGroup, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

/// Emitted when the user leaves the page.
pub struct SettingsClosed;

pub struct Settings {
    account: Entity<Account>,
    projects: Entity<ProjectPicker>,
    // `None` until the core has answered
    current: Option<WorklogSettings>,
    tempo_token: Entity<InputState>,
    // The field for a new Tempo token is shown
    replacing: bool,
    // Tempo was chosen with no token saved: the token is asked for first,
    // and saving it makes Tempo the target
    choosing_tempo: bool,
    // Takes the focus from the token field when the field goes away: GPUI
    // Kit keeps a focused input alive, and Tab would start from a field
    // that is not on the page
    focus: FocusHandle,
    error: Option<SharedString>,
    saving: Option<Task<()>>,
    _load: Task<()>,
    _hints: Task<()>,
}

impl EventEmitter<SettingsClosed> for Settings {}

impl Settings {
    pub fn new(
        account: Entity<Account>,
        projects: Entity<ProjectPicker>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tempo_token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Tempo API token")
        });
        cx.subscribe_in(
            &tempo_token,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save_token(window, cx);
                }
            },
        )
        .detach();
        let mut this = Self {
            account,
            projects,
            current: None,
            tempo_token,
            replacing: false,
            choosing_tempo: false,
            focus: cx.focus_handle(),
            error: None,
            saving: None,
            _load: Task::ready(()),
            _hints: Task::ready(()),
        };
        this._load = this.run(
            cx,
            |state| async move { api::worklog_settings(&state).await },
        );
        this
    }

    // Runs a call of the core, then shows what the core has now
    fn run<Fut>(
        &mut self,
        cx: &mut Context<Self>,
        call: impl FnOnce(std::sync::Arc<app_core::AppState>) -> Fut,
    ) -> Task<()>
    where
        Fut: Future<Output = anyhow::Result<WorklogSettings>> + Send + 'static,
    {
        let result = spawn(cx, call);
        cx.spawn(async move |this, cx| {
            let result = result.await;
            this.update(cx, |this, cx| {
                this.saving = None;
                match result {
                    Ok(current) => this.current = Some(current),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
    }

    fn choose(&mut self, target: WorklogTarget, window: &mut Window, cx: &mut Context<Self>) {
        let token_saved = self
            .current
            .is_some_and(|current| current.tempo_token_saved);
        if target == WorklogTarget::Tempo && !token_saved {
            self.choosing_tempo = true;
            self.replace_token(window, cx);
            return;
        }
        if self.replacing {
            self.hide_token_field(window, cx);
        }
        self.error = None;
        self.saving = Some(self.run(cx, move |state| async move {
            api::set_worklog_target(&state, target).await?;
            api::worklog_settings(&state).await
        }));
        cx.notify();
    }

    fn replace_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replacing = true;
        self.tempo_token
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn hide_token_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replacing = false;
        self.choosing_tempo = false;
        self.tempo_token
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn save_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let token = self.tempo_token.read(cx).value();
        self.error = None;
        let choosing_tempo = self.choosing_tempo;
        let saved = spawn(cx, move |state| async move {
            api::save_tempo_token(&state, &token).await?;
            if choosing_tempo {
                api::set_worklog_target(&state, WorklogTarget::Tempo).await?;
            }
            api::worklog_settings(&state).await
        });
        self.saving = Some(cx.spawn_in(window, async move |this, cx| {
            let result = saved.await;
            this.update_in(cx, |this, window, cx| {
                this.saving = None;
                match result {
                    Ok(current) => {
                        this.current = Some(current);
                        this.hide_token_field(window, cx);
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn show_hints(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.error = None;
        let saved = spawn(cx, move |state| async move {
            api::set_shortcut_hints(&state, shown).await
        });
        self._hints = cx.spawn(async move |this, cx| {
            let result = saved.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => cx.set_global(Hints(shown)),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn time_tracking(&self, cx: &mut Context<Self>) -> Div {
        let target = self.current.map(|current| current.target);
        let token_saved = self
            .current
            .is_some_and(|current| current.tempo_token_saved);
        let busy = self.saving.is_some();
        let muted = cx.theme().muted_foreground;
        let choice = |id: &'static str, label: &'static str, this: WorklogTarget| {
            let button = Button::new(id)
                .small()
                .label(label)
                .disabled(busy || self.current.is_none())
                .on_click(
                    cx.listener(move |this_view, _, window, cx| this_view.choose(this, window, cx)),
                );
            if target == Some(this) {
                button.primary()
            } else {
                button.outline()
            }
        };
        let targets = div()
            .id("worklog-target")
            .test_support()
            .aria_label(match target {
                Some(WorklogTarget::Tempo) => "Tempo",
                Some(WorklogTarget::Jira) => "Jira",
                None => "",
            })
            .flex()
            .child(
                ButtonGroup::new("worklog-targets")
                    .child(choice("target-jira", "Jira", WorklogTarget::Jira))
                    .child(choice("target-tempo", "Tempo", WorklogTarget::Tempo)),
            );
        let token_state = div()
            .id("tempo-token-state")
            .test_support()
            .aria_label(if token_saved { "saved" } else { "not saved" })
            .flex_1()
            .min_w_0()
            .text_xs()
            .text_color(muted)
            .child(if token_saved {
                "The Tempo API token is saved in the Keychain"
            } else {
                "No Tempo API token is saved"
            });
        let token = if self.replacing {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(token_state)
                .child(
                    Input::new(&self.tempo_token)
                        .id("tempo-token")
                        .aria_label("Tempo API token"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Create one in Tempo under Settings, API integration"),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("save-tempo-token")
                                .primary()
                                .small()
                                .label("Save token")
                                .loading(busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save_token(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("cancel-tempo-token")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.error = None;
                                    this.hide_token_field(window, cx)
                                })),
                        ),
                )
        } else {
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(token_state)
                .child(
                    Button::new("replace-tempo-token")
                        .outline()
                        .small()
                        // There is nothing to replace yet
                        .label(if token_saved {
                            "Replace token"
                        } else {
                            "Add token"
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.replace_token(window, cx)),
                        ),
                )
        };
        // The token is Tempo's alone, so it is not shown while time goes to Jira
        let token = (target == Some(WorklogTarget::Tempo) || self.replacing).then_some(token);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(targets)
            .child(div().text_xs().text_color(muted).child(
                "Tempo copies its worklogs into Jira, so sending to both would log the time twice.",
            ))
            .children(token)
            .children(self.error.clone().map(|error| {
                div()
                    .id("settings-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error.clone())
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }
}

/// A section of the page: its name and what it is on the left, its controls
/// on the right, a hairline under it.
fn section(name: &'static str, about: &'static str, controls: impl IntoElement, cx: &App) -> Div {
    div()
        .flex()
        .gap_6()
        .py_4()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .w(px(150.))
                .flex_none()
                .child(div().font_semibold().child(name))
                .child(
                    div()
                        .mt_0p5()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(about),
                ),
        )
        .child(div().flex_1().min_w_0().child(controls))
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex_none()
            .px_5()
            .py_3()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .text_size(px(16.))
                    .font_semibold()
                    .child("Settings"),
            )
            .child(
                Button::new("close-settings")
                    .primary()
                    .small()
                    .label("Done")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsClosed))),
            );
        let hints = Checkbox::new("shortcut-hints")
            .label("Show the key of a shortcut in its button or field")
            .checked(cx.global::<Hints>().0)
            .on_click(cx.listener(|this, shown: &bool, _, cx| this.show_hints(*shown, cx)));
        let sections = div()
            .w(px(620.))
            .max_w_full()
            .mx_auto()
            .px_5()
            .py_1()
            .child(section(
                "Account",
                "The Jira site you are signed in to",
                self.account.clone(),
                cx,
            ))
            .child(section(
                "Projects",
                "All their issues are downloaded, so that you can search them",
                self.projects.clone(),
                cx,
            ))
            .child(section(
                "Time tracking",
                "Logged time goes to one place",
                self.time_tracking(cx),
                cx,
            ))
            .child(
                section(
                    "Keyboard shortcuts",
                    "They are listed under ? in the status bar",
                    hints,
                    cx,
                )
                .border_b_0(),
            );
        div()
            .id("settings")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .id("settings-sections")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(sections),
            )
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        harness::{in_window, launch_on_jira, wait_until},
        runtime::Core,
    };
    use app_core::SyncEvent;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;

    fn open(cx: &mut TestAppContext, app: &crate::harness::TestApp) {
        wait_until(cx, app.window, |window| {
            window.try_find("edit-settings").is_some()
        });
        in_window(cx, app.window, |window, cx| {
            window.click("edit-settings", cx)
        });
        wait_until(cx, app.window, |window| {
            window.find("worklog-target").label() == Some("Jira")
        });
    }

    #[gpui_kit::test]
    fn choosing_tempo_asks_for_its_token_first_and_the_token_shows_only_for_tempo(
        cx: &mut TestAppContext,
    ) {
        let app = launch_on_jira(cx, &[]);
        open(cx, &app);
        in_window(cx, app.window, |window, cx| {
            // Time goes to Jira: nothing of the Tempo token is on the page
            assert!(window.try_find("tempo-token-state").is_none());
            assert!(window.try_find("replace-tempo-token").is_none());
            assert!(window.try_find("tempo-token").is_none());
            window.click("target-tempo", cx);
        });

        in_window(cx, app.window, |window, cx| {
            assert!(window.try_find("settings-error").is_none());
            assert_eq!(window.find("worklog-target").label(), Some("Jira"));
            assert_eq!(window.find("tempo-token-state").label(), Some("not saved"));
            window.click("tempo-token", cx);
            window.input("tempo-secret", cx);
            window.click("save-tempo-token", cx);
        });
        wait_until(cx, app.window, |window| {
            window.find("worklog-target").label() == Some("Tempo")
        });
        in_window(cx, app.window, |window, _| {
            assert_eq!(window.find("tempo-token-state").label(), Some("saved"));
            // The field is not there until it is asked for
            assert!(window.try_find("tempo-token").is_none());
            assert!(window.try_find("replace-tempo-token").is_some());
        });

        in_window(cx, app.window, |window, cx| window.click("target-jira", cx));
        wait_until(cx, app.window, |window| {
            window.find("worklog-target").label() == Some("Jira")
        });
        in_window(cx, app.window, |window, cx| {
            assert!(window.try_find("tempo-token-state").is_none());
            // The token is saved now, so Tempo is chosen at once
            window.click("target-tempo", cx);
        });
        wait_until(cx, app.window, |window| {
            window.find("worklog-target").label() == Some("Tempo")
        });
        in_window(cx, app.window, |window, cx| {
            window.click("replace-tempo-token", cx)
        });
        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("tempo-token").is_some());
        });
    }

    #[gpui_kit::test]
    fn cancelling_a_new_tempo_token_hides_its_field_and_saves_nothing(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[]);
        open(cx, &app);
        in_window(cx, app.window, |window, cx| {
            window.click("target-tempo", cx)
        });
        in_window(cx, app.window, |window, cx| {
            window.click("tempo-token", cx);
            window.input("tempo-secret", cx);
            window.click("cancel-tempo-token", cx);
        });

        in_window(cx, app.window, |window, _| {
            assert!(window.try_find("tempo-token").is_none());
            assert!(window.try_find("tempo-token-state").is_none());
            assert_eq!(window.find("worklog-target").label(), Some("Jira"));
        });
    }

    #[gpui_kit::test]
    fn a_rejected_tempo_token_is_reported_once_however_often_tempo_answers(
        cx: &mut TestAppContext,
    ) {
        let app = launch_on_jira(cx, &[]);
        wait_until(cx, app.window, |window| window.try_find("home").is_some());
        let rejected = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                cx.global::<Core>()
                    .state
                    .emit(SyncEvent::TempoTokenRejected)
            });
            cx.run_until_parked();
        };

        rejected(cx);
        rejected(cx);

        wait_until(cx, app.window, |window| {
            window.try_find("notification").is_some()
        });
        in_window(cx, app.window, |window, cx| {
            assert_eq!(window.notifications(cx).len(), 1);
        });
    }
}
