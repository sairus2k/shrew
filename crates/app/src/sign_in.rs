//! The first screen of a new install: site URL, email and API token.

use crate::runtime::spawn;
use app_core::api;
use gpui_kit::component::{
    ActiveTheme,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

/// Emitted once Jira has accepted the credentials and the core has saved them.
pub struct SignedIn;

pub struct SignIn {
    site_url: Entity<InputState>,
    email: Entity<InputState>,
    api_token: Entity<InputState>,
    error: Option<SharedString>,
    checking: Option<Task<()>>,
}

impl EventEmitter<SignedIn> for SignIn {}

impl SignIn {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let site_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://your-site.atlassian.net"));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com"));
        let api_token = cx.new(|cx| InputState::new(window, cx).masked(true));
        for input in [&site_url, &email, &api_token] {
            cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(cx);
                }
            })
            .detach();
        }
        Self {
            site_url,
            email,
            api_token,
            error: None,
            checking: None,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.checking.is_some() {
            return;
        }
        let site_url = self.site_url.read(cx).value();
        let email = self.email.read(cx).value();
        let api_token = self.api_token.read(cx).value();
        let check = spawn(cx, move |state| async move {
            api::sign_in(&state, &site_url, &email, &api_token).await
        });
        self.error = None;
        self.checking = Some(cx.spawn(async move |this, cx| {
            let result = check.await;
            this.update(cx, |this, cx| {
                this.checking = None;
                match result {
                    Ok(_) => cx.emit(SignedIn),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

fn field(label: &'static str, input: Input) -> Div {
    div().flex().flex_col().gap_1().child(label).child(input)
}

impl Render for SignIn {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = div()
            .w(px(360.))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xl().child("Sign in to Jira"))
            .child(field(
                "Site URL",
                Input::new(&self.site_url)
                    .id("site-url")
                    .aria_label("Site URL"),
            ))
            .child(field(
                "Email",
                Input::new(&self.email).id("email").aria_label("Email"),
            ))
            .child(
                field(
                    "API token",
                    Input::new(&self.api_token)
                        .id("api-token")
                        .aria_label("API token"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Create one at id.atlassian.com/manage-profile/security/api-tokens"),
                ),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .id("sign-in-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error.clone())
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
            .child(
                Button::new("sign-in")
                    .primary()
                    .label("Sign in")
                    .loading(self.checking.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
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
pub(crate) mod tests {
    use crate::harness::{in_window, launch, tokio_runtime, wait_until};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    pub(crate) fn jira(tokio: &tokio::runtime::Runtime, myself: ResponseTemplate) -> MockServer {
        tokio.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/rest/api/3/myself"))
                .respond_with(myself)
                .mount(&server)
                .await;
            server
        })
    }

    pub(crate) fn fill_in_and_submit(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        site_url: &str,
    ) {
        in_window(cx, window, |window, cx| {
            window.click("site-url", cx);
            window.input(site_url, cx);
            window.click("email", cx);
            window.input("ada@example.com", cx);
            window.click("api-token", cx);
            window.input("secret-token", cx);
            window.click("sign-in", cx);
        });
    }

    #[gpui_kit::test]
    fn without_credentials_the_app_asks_for_them(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();

        let window = launch(cx, &tokio, data_dir.path());

        in_window(cx, window, |window, _| {
            assert_eq!(window.find("site-url").label(), Some("Site URL"));
            assert_eq!(window.find("email").label(), Some("Email"));
            assert_eq!(window.find("api-token").label(), Some("API token"));
            assert!(window.try_find("choose-projects").is_none());
        });
    }

    #[gpui_kit::test]
    fn accepted_credentials_open_the_app_and_a_restart_does_not_ask_again(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(
            &tokio,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accountId": "5b10a", "displayName": "Ada" })),
        );
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());

        fill_in_and_submit(cx, window, &server.uri());

        wait_until(cx, window, |window| {
            window.try_find("choose-projects").is_some()
        });
        in_window(cx, window, |window, _| {
            assert!(window.try_find("sign-in").is_none());
        });

        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("choose-projects").is_some());
            assert!(window.try_find("sign-in").is_none());
        });
    }

    #[gpui_kit::test]
    fn rejected_credentials_show_the_reason_and_keep_the_form(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(401));
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());

        fill_in_and_submit(cx, window, &server.uri());

        wait_until(cx, window, |window| {
            window.try_find("sign-in-error").is_some()
        });
        in_window(cx, window, |window, _| {
            assert_eq!(
                window.find("sign-in-error").label(),
                Some("Jira rejected the email or the API token")
            );
            assert!(window.try_find("choose-projects").is_none());
            assert_eq!(
                window.find("email").value(),
                Some("ada@example.com"),
                "the form keeps what was typed"
            );
        });

        // The same data dir still has no credentials after a restart
        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("sign-in").is_some());
        });
    }
}
