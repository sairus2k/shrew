//! The signed-in account and its sign-out: a row of the settings, with the
//! replacement of the API token under it, and one line under the form that
//! asks for a missing token.

use crate::runtime::spawn;
use app_core::api;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    notification::Notification,
};
use gpui_kit::*;

/// Emitted once the core has deleted the credentials and the cache.
pub struct SignedOut;

pub struct Account {
    // A row of the settings, with the site, not the one line
    in_settings: bool,
    email: Option<SharedString>,
    // What Jira calls the user. The email stands in until the core has it
    name: Option<SharedString>,
    site: Option<SharedString>,
    signing_out: Option<Task<()>>,
    api_token: Entity<InputState>,
    // The field for a new API token is shown
    replacing: bool,
    // Jira accepted a new token since the page was opened
    replaced: bool,
    token_error: Option<SharedString>,
    saving_token: Option<Task<()>>,
    // Takes the focus from the token field when the field goes away: GPUI
    // Kit keeps a focused input alive, and Tab would start from a field
    // that is not on the page
    focus: FocusHandle,
    _load: Task<()>,
}

impl EventEmitter<SignedOut> for Account {}

impl Account {
    pub fn new(in_settings: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("New API token")
        });
        cx.subscribe_in(
            &api_token,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save_token(window, cx);
                }
            },
        )
        .detach();
        let account = spawn(cx, |state| async move {
            Ok((api::account(&state).await, api::display_name(&state).await))
        });
        let _load = cx.spawn(async move |this, cx| {
            let (account, name) = account.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.name = name.map(SharedString::from);
                if let Some(account) = account {
                    this.email = Some(account.email.into());
                    let site = account.site_url.trim_start_matches("https://");
                    this.site = Some(site.to_owned().into());
                }
                cx.notify();
            })
            .ok();
        });
        Self {
            in_settings,
            email: None,
            name: None,
            site: None,
            signing_out: None,
            api_token,
            replacing: false,
            replaced: false,
            token_error: None,
            saving_token: None,
            focus: cx.focus_handle(),
            _load,
        }
    }

    fn replace_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replacing = true;
        self.api_token
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn hide_token_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.replacing = false;
        self.token_error = None;
        self.api_token
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn save_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving_token.is_some() {
            return;
        }
        let token = self.api_token.read(cx).value();
        let saved = spawn(cx, move |state| async move {
            api::renew_token(&state, &token).await
        });
        self.token_error = None;
        self.saving_token = Some(cx.spawn_in(window, async move |this, cx| {
            let result = saved.await;
            this.update_in(cx, |this, window, cx| {
                this.saving_token = None;
                match result {
                    Ok(()) => {
                        this.replaced = true;
                        this.hide_token_field(window, cx);
                    }
                    Err(error) => this.token_error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn token(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let state = div()
            .id("api-token-state")
            .test_support()
            .aria_label(if self.replaced { "replaced" } else { "saved" })
            .flex_1()
            .min_w_0()
            .text_xs()
            .text_color(muted)
            .child(if self.replaced {
                "The new API token is saved in the Keychain"
            } else {
                "The API token is saved in the Keychain"
            });
        if !self.replacing {
            return div().flex().items_center().gap_2p5().child(state).child(
                Button::new("replace-token")
                    .outline()
                    .small()
                    .label("Replace token")
                    .on_click(cx.listener(|this, _, window, cx| this.replace_token(window, cx))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(state)
            .child(
                Input::new(&self.api_token)
                    .id("new-jira-token")
                    .aria_label("New API token"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("Create one at id.atlassian.com/manage-profile/security/api-tokens"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("save-jira-token")
                            .primary()
                            .small()
                            .label("Save token")
                            .loading(self.saving_token.is_some())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_token(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("cancel-jira-token")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.hide_token_field(window, cx)
                                }),
                            ),
                    ),
            )
            .children(self.token_error.clone().map(|error| {
                div()
                    .id("jira-token-error")
                    .role(Role::Alert)
                    .test_support()
                    .aria_label(error.clone())
                    .text_color(cx.theme().danger)
                    .child(error)
            }))
    }

    /// Signs out at once, unless that would discard changes Jira has not
    /// received: then the user confirms first.
    fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_out.is_some() {
            return;
        }
        let unsent = spawn(cx, |state| async move { api::unsent_changes(&state).await });
        self.signing_out = Some(cx.spawn_in(window, async move |this, cx| {
            // An outbox that cannot be read must not keep the user signed in
            let unsent = unsent.await.unwrap_or(0);
            this.update_in(cx, |this, window, cx| {
                this.signing_out = None;
                if unsent == 0 {
                    this.sign_out(window, cx);
                } else {
                    this.confirm(unsent, window, cx);
                }
            })
            .ok();
        }));
    }

    fn confirm(&mut self, unsent: i64, window: &mut Window, cx: &mut Context<Self>) {
        let changes = if unsent == 1 {
            "1 change has".to_owned()
        } else {
            format!("{unsent} changes have")
        };
        let description =
            format!("{changes} not been sent to Jira yet and will be lost if you sign out now.");
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title("Sign out with unsent changes?")
                .description(description.clone())
                .ok_text("Sign out")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.sign_out(window, cx)).ok();
                    true
                })
        });
    }

    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.signing_out.is_some() {
            return;
        }
        let signed_out = spawn(cx, |state| async move { api::sign_out(&state).await });
        self.signing_out = Some(cx.spawn_in(window, async move |this, cx| {
            let result = signed_out.await;
            this.update_in(cx, |this, window, cx| {
                this.signing_out = None;
                match result {
                    Ok(()) => cx.emit(SignedOut),
                    Err(error) => {
                        let message = format!("Could not sign out: {error:#}");
                        window.push_notification(Notification::error(message), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for Account {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let email = self.email.clone().unwrap_or_default();
        let account = div().id("account").test_support().aria_label(email.clone());
        let sign_out = Button::new("sign-out")
            .label("Sign out")
            .loading(self.signing_out.is_some())
            .on_click(cx.listener(|this, _, window, cx| this.ask(window, cx)));
        if !self.in_settings {
            return div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .child(account.child(email))
                .child(sign_out.ghost().xsmall());
        }
        let name = self.name.clone().unwrap_or_else(|| email.clone());
        let site = self.site.clone().unwrap_or_default();
        let about = match &self.name {
            Some(_) => format!("{email} · {site}").into(),
            None => site,
        };
        let row = div()
            .flex()
            .items_center()
            .gap_2p5()
            .children(
                self.email
                    .is_some()
                    .then(|| Avatar::new().name(name.clone()).small()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id("account-name")
                            .test_support()
                            .aria_label(name.clone())
                            .font_semibold()
                            .child(name),
                    )
                    .child(
                        account
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(about),
                    ),
            )
            .child(sign_out.outline().small());
        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_2()
            .child(row)
            .child(self.token(cx))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        harness::{in_window, launch, tab_to, tokio_runtime, wait_until},
        issue_list::tests::{issue, rows, synced, synced_on},
        runtime::Core,
        sign_in::tests::{fill_in_and_submit, jira},
    };
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};
    use serde_json::json;
    use std::path::Path;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    fn wait_for(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
        wait_until(cx, window, |window| window.try_find(id).is_some());
    }

    /// The account is a section of the settings.
    fn open_settings(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_for(cx, window, "edit-settings");
        in_window(cx, window, |window, cx| window.click("edit-settings", cx));
        wait_for(cx, window, "sign-out");
    }

    fn credentials_are_saved(data_dir: &Path) -> bool {
        let saved = |file| data_dir.join(file).exists();
        assert_eq!(saved("config.json"), saved("api-token"));
        saved("config.json")
    }

    /// A Jira that knows the account `account_id`, whose project WEB has one
    /// issue with the summary `summary`.
    fn jira_with_an_issue(
        tokio: &tokio::runtime::Runtime,
        account_id: &str,
        summary: &str,
    ) -> MockServer {
        let myself = json!({ "accountId": account_id, "displayName": "Grace" });
        let server = jira(tokio, ResponseTemplate::new(200).set_body_json(myself));
        let answers = [
            (
                "/rest/api/3/project/search",
                json!({ "isLast": true, "values": [{ "id": "10000", "key": "WEB", "name": "Website" }] }),
            ),
            (
                "/rest/api/3/search/approximate-count",
                json!({ "count": 1 }),
            ),
            (
                "/rest/api/3/search/jql",
                json!({ "issues": [issue(7, summary)] }),
            ),
        ];
        for (endpoint, answer) in answers {
            let answer = ResponseTemplate::new(200).set_body_json(answer);
            tokio.block_on(
                Mock::given(path(endpoint))
                    .respond_with(answer)
                    .mount(&server),
            );
        }
        server
    }

    #[gpui_kit::test]
    fn signing_out_shows_the_sign_in_form_and_a_restart_shows_it_again(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            // The status bar no longer names the account
            assert!(window.try_find("sign-out").is_none());
        });
        open_settings(cx, window);
        wait_until(cx, window, |window| {
            window.find("account").label() == Some("ada@example.com")
        });

        in_window(cx, window, |window, cx| window.click("sign-out", cx));

        wait_for(cx, window, "site-url");
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("home").is_none());
            // Nothing was unsent, so there was nothing to confirm
            assert!(!window.has_active_dialog(cx));
        });
        assert!(!credentials_are_saved(data_dir.path()));

        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("site-url").is_some());
            assert!(window.try_find("home").is_none());
        });
    }

    #[gpui_kit::test]
    fn the_account_is_shown_under_the_name_jira_has_for_it(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_settings(cx, window);
        // A config written before the name was kept
        wait_until(cx, window, |window| {
            window.find("account-name").label() == Some("ada@example.com")
        });

        let file = data_dir.path().join("config.json");
        let mut config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        config["display_name"] = json!("Ada Lovelace");
        std::fs::write(&file, config.to_string()).unwrap();
        let window = launch(cx, &tokio, data_dir.path());
        open_settings(cx, window);

        wait_until(cx, window, |window| {
            window.find("account-name").label() == Some("Ada Lovelace")
        });
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("account").label(), Some("ada@example.com"));
        });
    }

    #[gpui_kit::test]
    fn tab_and_enter_sign_out(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_settings(cx, window);

        tab_to(cx, window, "sign-out");
        in_window(cx, window, |window, cx| window.press("enter", cx));

        wait_for(cx, window, "site-url");
        assert!(!credentials_are_saved(data_dir.path()));
    }

    #[gpui_kit::test]
    fn another_account_sees_no_issue_of_the_previous_one(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_settings(cx, window);
        in_window(cx, window, |window, cx| window.click("sign-out", cx));
        wait_for(cx, window, "site-url");

        // The same window, with no restart in between
        let server = jira_with_an_issue(&tokio, "7c20b", "Broken checkout");
        fill_in_and_submit(cx, window, &server.uri());
        wait_for(cx, window, "project-WEB");
        in_window(cx, window, |window, cx| {
            window.click("project-WEB", cx);
            window.click("watch-projects", cx);
        });

        wait_for(cx, window, "home");
        let only_hers = ["WEB-7: Broken checkout, To Do, Unassigned"];
        wait_until(cx, window, |window| rows(window) == only_hers);
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("login", cx);
        });
        wait_until(cx, window, |window| rows(window).is_empty());
    }

    #[gpui_kit::test]
    fn signing_out_with_unsent_changes_asks_first(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open_settings(cx, window);
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        let queue = "INSERT INTO sync_queue (entity_type, issue_key, operation, payload)
                     VALUES ('transition', 'WEB-1', 'create', '{}')";
        tokio.block_on(state.execute(queue)).unwrap();

        in_window(cx, window, |window, cx| window.click("sign-out", cx));
        wait_until(cx, window, |window| window.try_find("cancel").is_some());
        in_window(cx, window, |window, cx| window.click("cancel", cx));

        cx.run_until_parked();
        in_window(cx, window, |window, cx| {
            assert!(!window.has_active_dialog(cx));
            assert!(window.try_find("settings").is_some());
        });
        assert!(credentials_are_saved(data_dir.path()));

        in_window(cx, window, |window, cx| window.click("sign-out", cx));
        wait_until(cx, window, |window| window.try_find("ok").is_some());
        in_window(cx, window, |window, cx| window.click("ok", cx));

        wait_for(cx, window, "site-url");
        assert!(!credentials_are_saved(data_dir.path()));
    }

    /// The settings of the app signed in as the account 5b10a, on a Jira that
    /// answers `myself` when a token is checked.
    fn settings_on_jira(
        cx: &mut TestAppContext,
        tokio: &tokio::runtime::Runtime,
        myself: ResponseTemplate,
    ) -> (MockServer, tempfile::TempDir, AnyWindowHandle) {
        let server = jira(tokio, myself);
        let data_dir = synced_on(tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, tokio, data_dir.path());
        open_settings(cx, window);
        (server, data_dir, window)
    }

    fn replace_token(cx: &mut TestAppContext, window: AnyWindowHandle, token: &str) {
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("new-jira-token").is_none());
            window.click("replace-token", cx);
        });
        in_window(cx, window, |window, cx| {
            window.click("new-jira-token", cx);
            window.input(token, cx);
            window.click("save-jira-token", cx);
        });
    }

    fn saved_token(data_dir: &Path) -> String {
        std::fs::read_to_string(data_dir.join("api-token")).unwrap()
    }

    fn a_refused_token_shows_the_reason_and_stays_unsaved(
        cx: &mut TestAppContext,
        myself: ResponseTemplate,
        reason: &str,
    ) {
        let tokio = tokio_runtime();
        let (_server, data_dir, window) = settings_on_jira(cx, &tokio, myself);

        replace_token(cx, window, "refused");

        wait_for(cx, window, "jira-token-error");
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("jira-token-error").label(), Some(reason));
            assert!(window.try_find("new-jira-token").is_some());
            assert_eq!(window.find("api-token-state").label(), Some("saved"));
        });
        assert_eq!(saved_token(data_dir.path()), "secret-token");
    }

    #[gpui_kit::test]
    fn a_new_token_jira_accepts_is_saved_and_the_account_stays_signed_in(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let myself = json!({ "accountId": "5b10a", "displayName": "Ada" });
        let myself = ResponseTemplate::new(200).set_body_json(myself);
        let (_server, data_dir, window) = settings_on_jira(cx, &tokio, myself);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("api-token-state").label(), Some("saved"));
        });

        replace_token(cx, window, "new-token");

        wait_until(cx, window, |window| {
            window.find("api-token-state").label() == Some("replaced")
        });
        in_window(cx, window, |window, _| {
            assert!(window.try_find("new-jira-token").is_none());
            assert!(window.try_find("replace-token").is_some());
            assert!(window.try_find("settings").is_some());
        });
        assert_eq!(saved_token(data_dir.path()), "new-token");
    }

    #[gpui_kit::test]
    fn a_new_token_jira_rejects_is_not_saved(cx: &mut TestAppContext) {
        a_refused_token_shows_the_reason_and_stays_unsaved(
            cx,
            ResponseTemplate::new(401),
            "Jira rejected the API token",
        );
    }

    #[gpui_kit::test]
    fn a_new_token_of_another_account_is_not_saved(cx: &mut TestAppContext) {
        let grace = json!({ "accountId": "7c20b", "displayName": "Grace" });
        a_refused_token_shows_the_reason_and_stays_unsaved(
            cx,
            ResponseTemplate::new(200).set_body_json(grace),
            "This API token belongs to another Jira account",
        );
    }

    #[gpui_kit::test]
    fn cancelling_a_new_token_hides_its_field_and_saves_nothing(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let (_server, data_dir, window) = settings_on_jira(cx, &tokio, ResponseTemplate::new(401));
        in_window(cx, window, |window, cx| window.click("replace-token", cx));
        in_window(cx, window, |window, cx| {
            window.click("new-jira-token", cx);
            window.input("mistyped", cx);
            window.click("cancel-jira-token", cx);
        });

        in_window(cx, window, |window, _| {
            assert!(window.try_find("new-jira-token").is_none());
            assert_eq!(window.find("api-token-state").label(), Some("saved"));
        });
        assert_eq!(saved_token(data_dir.path()), "secret-token");
    }
}
