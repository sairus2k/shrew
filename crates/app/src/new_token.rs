//! The banner that asks for a new API token after Jira rejected the saved one.

use crate::runtime::spawn;
use app_core::api;
use gpui_kit::component::{
    ActiveTheme,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

/// Emitted once Jira has accepted the new token and the core has saved it.
pub struct TokenRenewed;

pub struct NewToken {
    api_token: Entity<InputState>,
    error: Option<SharedString>,
    checking: Option<Task<()>>,
}

impl EventEmitter<TokenRenewed> for NewToken {}

impl NewToken {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("New API token")
        });
        cx.subscribe(&api_token, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(cx);
            }
        })
        .detach();
        Self {
            api_token,
            error: None,
            checking: None,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.checking.is_some() {
            return;
        }
        let api_token = self.api_token.read(cx).value();
        let check = spawn(cx, move |state| async move {
            api::renew_token(&state, &api_token).await
        });
        self.error = None;
        self.checking = Some(cx.spawn(async move |this, cx| {
            let result = check.await;
            this.update(cx, |this, cx| {
                this.checking = None;
                match result {
                    Ok(()) => cx.emit(TokenRenewed),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for NewToken {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("new-token")
            .test_support()
            .flex_none()
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                "Jira no longer accepts the API token, so sync is paused. \
                 Create a new token at id.atlassian.com/manage-profile/security/api-tokens",
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().w(px(360.)).child(
                            Input::new(&self.api_token)
                                .id("new-api-token")
                                .aria_label("New API token"),
                        ),
                    )
                    .child(
                        Button::new("save-token")
                            .primary()
                            .label("Save")
                            .loading(self.checking.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .id("new-token-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error.clone())
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        issue_list::tests::synced,
        runtime::Core,
        sign_in::tests::{in_window, jira, launch, tokio_runtime, wait_until},
    };
    use app_core::SyncEvent;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};
    use std::path::Path;
    use wiremock::{MockServer, ResponseTemplate};

    /// The app with a finished initial sync against `server`, right after Jira
    /// answered a sync with 401.
    fn rejected(
        cx: &mut TestAppContext,
        tokio: &tokio::runtime::Runtime,
        server: &MockServer,
    ) -> (tempfile::TempDir, AnyWindowHandle) {
        let data_dir = synced(tokio, &[]);
        let config_file = data_dir.path().join("config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_file).unwrap()).unwrap();
        config["site_url"] = server.uri().into();
        std::fs::write(&config_file, config.to_string()).unwrap();
        let window = launch(cx, tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("new-token").is_none());
        });

        cx.update(|cx| cx.global::<Core>().state.emit(SyncEvent::TokenRejected));
        wait_until(cx, window, |window| window.try_find("new-token").is_some());
        (data_dir, window)
    }

    fn save(cx: &mut TestAppContext, window: AnyWindowHandle, token: &str) {
        in_window(cx, window, |window, cx| {
            window.click("new-api-token", cx);
            window.input(token, cx);
            window.click("save-token", cx);
        });
    }

    fn saved_token(data_dir: &Path) -> String {
        std::fs::read_to_string(data_dir.join("api-token")).unwrap()
    }

    #[gpui_kit::test]
    fn a_rejected_token_is_asked_for_again_and_an_accepted_one_closes_the_banner(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = jira(
            &tokio,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accountId": "5b10a", "displayName": "Ada" })),
        );
        let (data_dir, window) = rejected(cx, &tokio, &server);

        save(cx, window, "new-token");

        wait_until(cx, window, |window| window.try_find("new-token").is_none());
        assert_eq!(saved_token(data_dir.path()), "new-token");
        in_window(cx, window, |window, _| {
            assert!(window.try_find("home").is_some());
        });
    }

    #[gpui_kit::test]
    fn a_new_token_jira_rejects_shows_the_reason_and_keeps_the_banner(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(401));
        let (data_dir, window) = rejected(cx, &tokio, &server);

        save(cx, window, "mistyped");

        wait_until(cx, window, |window| {
            window.try_find("new-token-error").is_some()
        });
        in_window(cx, window, |window, _| {
            assert_eq!(
                window.find("new-token-error").label(),
                Some("Jira rejected the API token")
            );
            assert!(window.try_find("new-api-token").is_some());
        });
        assert_eq!(saved_token(data_dir.path()), "secret-token");
    }
}
