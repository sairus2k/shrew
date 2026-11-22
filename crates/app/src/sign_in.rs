//! The first screen of a new install: site URL, email and API token.

use crate::runtime::spawn;
use app_core::api;
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt as _,
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
            cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit(false, window, cx);
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

    /// `discard_unsent` is set only by the answer to `confirm`.
    fn submit(&mut self, discard_unsent: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking.is_some() {
            return;
        }
        let site_url = self.site_url.read(cx).value();
        let email = self.email.read(cx).value();
        let api_token = self.api_token.read(cx).value();
        let check = spawn(cx, move |state| async move {
            if discard_unsent {
                api::sign_in_discarding(&state, &site_url, &email, &api_token).await
            } else {
                api::sign_in(&state, &site_url, &email, &api_token).await
            }
        });
        self.error = None;
        self.checking = Some(cx.spawn_in(window, async move |this, cx| {
            let result = check.await;
            this.update_in(cx, |this, window, cx| {
                this.checking = None;
                match result {
                    Ok(_) => cx.emit(SignedIn),
                    Err(error) => match error.downcast_ref() {
                        Some(unsent) => this.confirm(unsent, window, cx),
                        None => this.error = Some(error.to_string().into()),
                    },
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The database holds changes of another account that Jira has not
    /// received. They are deleted only when the user says so.
    fn confirm(
        &mut self,
        unsent: &api::UnsentChanges,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let description = unsent_message(unsent);
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title("Delete unsent changes?")
                .description(description.clone())
                .ok_text("Sign in and delete")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.submit(true, window, cx))
                        .ok();
                    true
                })
        });
    }
}

fn unsent_message(unsent: &api::UnsentChanges) -> String {
    let (changes, have) = if unsent.count == 1 {
        ("1 change".to_owned(), "has")
    } else {
        (format!("{} changes", unsent.count), "have")
    };
    match &unsent.owner {
        Some(owner) => format!(
            "{changes} made as {} on {} {have} not been sent to Jira yet. \
             Sign in to that account to keep them: signing in to this one deletes them.",
            owner.email, owner.site_url
        ),
        None => format!(
            "{changes} made in this app {have} not been sent to Jira yet, and the app \
             cannot tell which account made them. Signing in deletes them."
        ),
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
                // A row, so the link is as wide as its text and not as the form
                .child(
                    div().flex().child(
                        Button::new("create-api-token")
                            .link()
                            .small()
                            .label(
                                "Create one at id.atlassian.com/manage-profile/security/api-tokens",
                            )
                            .on_click(|_, _, cx| {
                                cx.open_url(
                                    "https://id.atlassian.com/manage-profile/security/api-tokens",
                                )
                            }),
                    ),
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
                    .on_click(cx.listener(|this, _, window, cx| this.submit(false, window, cx))),
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
    use crate::{
        harness::{in_window, launch, tokio_runtime, wait_until},
        issue_list::tests::{issue, synced_on},
        runtime::Core,
    };
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    const API_TOKENS_URL: &str = "https://id.atlassian.com/manage-profile/security/api-tokens";

    pub(crate) fn jira(tokio: &tokio::runtime::Runtime, myself: ResponseTemplate) -> MockServer {
        tokio.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/rest/api/3/myself"))
                .respond_with(myself)
                .mount(&server)
                .await;
            crate::harness::no_sprint_field().mount(&server).await;
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
    fn the_token_hint_opens_the_atlassian_token_page(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());
        assert_eq!(cx.opened_url(), None);

        in_window(cx, window, |window, cx| {
            window.click("create-api-token", cx)
        });

        assert_eq!(cx.opened_url().as_deref(), Some(API_TOKENS_URL));
    }

    #[gpui_kit::test]
    fn the_token_hint_is_reached_and_activated_from_the_keyboard(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());

        in_window(cx, window, |window, cx| {
            window.click("api-token", cx);
            window.press("tab", cx);
            window.press("enter", cx);
        });

        assert_eq!(cx.opened_url().as_deref(), Some(API_TOKENS_URL));
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

    /// A data dir whose config was lost, with one change of ada@example.com
    /// on `site_url` that Jira has not received.
    fn config_lost_with_an_unsent_change(
        tokio: &tokio::runtime::Runtime,
        site_url: &str,
    ) -> tempfile::TempDir {
        let data_dir = synced_on(tokio, site_url, &[issue(1, "Fix login")]);
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir.path())
                .await
                .unwrap();
            let queue = "INSERT INTO sync_queue (entity_type, issue_key, operation, payload)
                         VALUES ('transition', 'WEB-1', 'create', '{}')";
            state.execute(queue).await.unwrap();
        });
        std::fs::write(data_dir.path().join("config.json"), "").unwrap();
        data_dir
    }

    fn unsent_changes(cx: &mut TestAppContext, tokio: &tokio::runtime::Runtime) -> i64 {
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        tokio
            .block_on(app_core::api::unsent_changes(&state))
            .unwrap()
    }

    #[test]
    fn the_question_counts_the_unsent_changes_and_names_the_account_that_keeps_them() {
        let mut unsent = app_core::api::UnsentChanges {
            count: 1,
            owner: Some(app_core::models::Account {
                site_url: "https://acme.atlassian.net".to_owned(),
                email: "ada@example.com".to_owned(),
                account_id: "5b10a".to_owned(),
            }),
        };
        assert_eq!(
            super::unsent_message(&unsent),
            "1 change made as ada@example.com on https://acme.atlassian.net has not been sent \
             to Jira yet. Sign in to that account to keep them: signing in to this one deletes \
             them."
        );

        unsent.count = 3;
        unsent.owner = None;
        assert_eq!(
            super::unsent_message(&unsent),
            "3 changes made in this app have not been sent to Jira yet, and the app cannot \
             tell which account made them. Signing in deletes them."
        );
    }

    #[gpui_kit::test]
    fn the_account_of_the_unsent_changes_signs_in_and_keeps_them(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(
            &tokio,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accountId": "5b10a", "displayName": "Ada" })),
        );
        let data_dir = config_lost_with_an_unsent_change(&tokio, &server.uri());
        let window = launch(cx, &tokio, data_dir.path());

        fill_in_and_submit(cx, window, &server.uri());

        wait_until(cx, window, |window| {
            window.try_find("choose-projects").is_some()
        });
        in_window(cx, window, |window, cx| {
            assert!(!window.has_active_dialog(cx));
        });
        assert_eq!(unsent_changes(cx, &tokio), 1);
    }

    #[gpui_kit::test]
    fn another_account_is_told_about_the_unsent_changes_and_deletes_them_only_on_ok(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let data_dir = config_lost_with_an_unsent_change(&tokio, "https://acme.atlassian.net");
        let server = jira(
            &tokio,
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "accountId": "7c20b", "displayName": "Grace" })),
        );
        let window = launch(cx, &tokio, data_dir.path());

        fill_in_and_submit(cx, window, &server.uri());

        wait_until(cx, window, |window| window.try_find("cancel").is_some());
        in_window(cx, window, |window, cx| window.click("cancel", cx));
        cx.run_until_parked();
        in_window(cx, window, |window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(window.try_find("sign-in").is_some());
            assert!(window.try_find("sign-in-error").is_none());
        });
        assert_eq!(unsent_changes(cx, &tokio), 1);

        in_window(cx, window, |window, cx| window.click("sign-in", cx));
        wait_until(cx, window, |window| window.try_find("ok").is_some());
        in_window(cx, window, |window, cx| window.click("ok", cx));

        wait_until(cx, window, |window| {
            window.try_find("choose-projects").is_some()
        });
        assert_eq!(unsent_changes(cx, &tokio), 0);
    }
}
