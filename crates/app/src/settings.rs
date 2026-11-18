//! Where logged time goes, Jira or Tempo, and the Tempo API token.

use crate::runtime::spawn;
use app_core::{
    api,
    models::{WorklogSettings, WorklogTarget},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

/// Emitted when the user leaves the screen.
pub struct SettingsClosed;

pub struct Settings {
    // `None` until the core has answered
    current: Option<WorklogSettings>,
    tempo_token: Entity<InputState>,
    error: Option<SharedString>,
    saving: Option<Task<()>>,
    _load: Task<()>,
}

impl EventEmitter<SettingsClosed> for Settings {}

impl Settings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tempo_token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Tempo API token")
        });
        cx.subscribe(&tempo_token, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save_token(cx);
            }
        })
        .detach();
        let mut this = Self {
            current: None,
            tempo_token,
            error: None,
            saving: None,
            _load: Task::ready(()),
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

    fn choose(&mut self, target: WorklogTarget, cx: &mut Context<Self>) {
        self.error = None;
        self.saving = Some(self.run(cx, move |state| async move {
            api::set_worklog_target(&state, target).await?;
            api::worklog_settings(&state).await
        }));
        cx.notify();
    }

    fn save_token(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let token = self.tempo_token.read(cx).value();
        self.error = None;
        self.saving = Some(self.run(cx, move |state| async move {
            api::save_tempo_token(&state, &token).await?;
            api::worklog_settings(&state).await
        }));
        cx.notify();
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.current.map(|current| current.target);
        let token_saved = self
            .current
            .is_some_and(|current| current.tempo_token_saved);
        let busy = self.saving.is_some();
        let choice = |id: &'static str, label: &'static str, this: WorklogTarget| {
            Button::new(id)
                .label(label)
                .selected(target == Some(this))
                .disabled(busy || self.current.is_none())
                .on_click(cx.listener(move |this_view, _, _, cx| this_view.choose(this, cx)))
        };
        let form = div()
            .w(px(360.))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xl().child("Time tracking"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Logged time goes to one place. Tempo copies its worklogs into Jira, \
                         so sending to both would log the time twice.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(choice("target-jira", "Jira", WorklogTarget::Jira))
                    .child(choice("target-tempo", "Tempo", WorklogTarget::Tempo)),
            )
            .child(
                div()
                    .id("worklog-target")
                    .test_support()
                    .aria_label(match target {
                        Some(WorklogTarget::Tempo) => "Tempo",
                        Some(WorklogTarget::Jira) => "Jira",
                        None => "",
                    })
                    .text_sm()
                    .child(match target {
                        Some(WorklogTarget::Tempo) => "Time is logged to Tempo",
                        Some(WorklogTarget::Jira) => "Time is logged to Jira",
                        None => "",
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("Tempo API token")
                    .child(
                        Input::new(&self.tempo_token)
                            .id("tempo-token")
                            .aria_label("Tempo API token"),
                    )
                    .child(
                        div()
                            .id("tempo-token-state")
                            .test_support()
                            .aria_label(if token_saved { "saved" } else { "not saved" })
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if token_saved {
                                "A token is saved in the Keychain"
                            } else {
                                "Create one in Tempo under Settings, API integration"
                            }),
                    )
                    .child(
                        div().flex().child(
                            Button::new("save-tempo-token")
                                .label("Save token")
                                .loading(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.save_token(cx))),
                        ),
                    ),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .id("settings-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error.clone())
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
            .child(
                div().flex().child(
                    Button::new("close-settings")
                        .primary()
                        .label("Done")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsClosed))),
                ),
            );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(form)
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
    fn tempo_cannot_be_chosen_until_its_token_is_saved(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[]);
        open(cx, &app);

        in_window(cx, app.window, |window, cx| {
            window.click("target-tempo", cx)
        });
        wait_until(cx, app.window, |window| {
            window.try_find("settings-error").is_some()
        });
        in_window(cx, app.window, |window, cx| {
            assert_eq!(
                window.find("settings-error").label(),
                Some("Save the Tempo API token first")
            );
            assert_eq!(window.find("worklog-target").label(), Some("Jira"));
            assert_eq!(window.find("tempo-token-state").label(), Some("not saved"));
            window.click("tempo-token", cx);
            window.input("tempo-secret", cx);
            window.click("save-tempo-token", cx);
        });
        wait_until(cx, app.window, |window| {
            window.find("tempo-token-state").label() == Some("saved")
        });

        in_window(cx, app.window, |window, cx| {
            window.click("target-tempo", cx)
        });
        wait_until(cx, app.window, |window| {
            window.find("worklog-target").label() == Some("Tempo")
        });
        in_window(cx, app.window, |window, cx| window.click("target-jira", cx));
        wait_until(cx, app.window, |window| {
            window.find("worklog-target").label() == Some("Jira")
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
