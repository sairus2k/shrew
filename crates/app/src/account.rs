//! The signed-in account and its sign-out: a row of the settings, and one line
//! under the form that asks for a missing token.

use crate::runtime::spawn;
use app_core::api;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::{
    ActiveTheme, Sizable, WindowExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants},
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
    _load: Task<()>,
}

impl EventEmitter<SignedOut> for Account {}

impl Account {
    pub fn new(in_settings: bool, cx: &mut Context<Self>) -> Self {
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
            _load,
        }
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
        div()
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
            .child(sign_out.outline().small())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        harness::{in_window, launch, tab_to, tokio_runtime, wait_until},
        issue_list::tests::{issue, rows, synced},
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
}
