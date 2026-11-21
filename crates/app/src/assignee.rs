//! The assignee of an issue as a button whose menu holds the people it can be assigned to.

use crate::issue::IssueModel;
use crate::runtime::spawn;
use app_core::{api, models::User};
use gpui_kit::component::{
    Disableable, Sizable, WindowExt as _,
    avatar::Avatar,
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
};
use gpui_kit::*;

pub struct AssigneeButton {
    issue: Entity<IssueModel>,
    // Empty until the core has answered
    people: Vec<User>,
    // The account id of the signed-in user
    me: Option<String>,
    loading: bool,
    // Of the button, for the shortcut that opens its menu
    focus: FocusHandle,
    _load: Task<()>,
    _issue_changed: Subscription,
}

impl AssigneeButton {
    pub fn new(issue: Entity<IssueModel>, cx: &mut Context<Self>) -> Self {
        // Offline with nothing cached, the next change of the issue asks again
        let _issue_changed = cx.observe(&issue, |this, _, cx| this.load_people(cx));
        let mut this = Self {
            issue,
            people: Vec::new(),
            me: None,
            loading: false,
            focus: cx.focus_handle(),
            _load: Task::ready(()),
            _issue_changed,
        };
        this.load_people(cx);
        this
    }

    /// Opens the menu, unless the button is disabled for having nobody to offer.
    pub fn open_menu(&self, window: &mut Window, cx: &mut App) {
        if !self.people.is_empty() {
            crate::keys::open_menu(&self.focus, window, cx);
        }
    }

    // The list does not depend on the assignee, so choosing one needs no new
    // question to the core, and none can come before the write is committed
    fn load_people(&mut self, cx: &mut Context<Self>) {
        if self.loading || !self.people.is_empty() {
            return;
        }
        self.loading = true;
        let key = self.issue.read(cx).key.clone();
        let load = spawn(cx, move |state| async move {
            let people = api::assignable_users(&state, &key).await?;
            let me = api::account(&state).await.map(|account| account.account_id);
            anyhow::Ok((people, me))
        });
        self._load = cx.spawn(async move |this, cx| {
            let loaded = load.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                if let Ok((people, me)) = loaded {
                    this.people = people;
                    this.me = me;
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn assign(&mut self, assignee: Option<User>, window: &mut Window, cx: &mut Context<Self>) {
        // Optimistic: every view observing the model repaints in this frame
        self.issue
            .update(cx, |issue, cx| issue.apply_assignee(assignee.as_ref(), cx));

        let key = self.issue.read(cx).key.clone();
        let write = spawn(cx, move |state| async move {
            api::assign_issue(&state, &key, assignee.as_ref()).await
        });
        cx.spawn_in(window, async move |this, cx| {
            // On success the core announces the issue and the model reloads it
            if write.await.is_err() {
                this.update_in(cx, |this, window, cx| {
                    // Nothing was committed, so the cache still holds the previous assignee
                    this.issue.update(cx, |issue, cx| issue.reload(cx));
                    window
                        .push_notification(Notification::error("Could not save the assignee"), cx);
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for AssigneeButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let people = self.people.clone();
        let me = self
            .people
            .iter()
            .find(|user| Some(&user.account_id) == self.me.as_ref())
            .cloned();
        let assignee = self
            .issue
            .read(cx)
            .detail
            .as_ref()
            .and_then(|detail| detail.assignee_name.clone())
            .unwrap_or_else(|| "Unassigned".to_owned());

        let assigned = self.issue.read(cx).detail.as_ref();
        let avatar = assigned
            .and_then(|detail| detail.assignee_name.clone())
            .map(|name| Avatar::new().name(name).xsmall());

        Button::new("assignee-button")
            .track_focus(&self.focus)
            .small()
            .outline()
            .dropdown_caret(true)
            .accessibility_label(assignee.clone())
            .children(avatar)
            .child(assignee)
            .disabled(people.is_empty())
            .dropdown_menu(move |mut menu, window, _| {
                menu = menu.scrollable(true).max_h(px(320.));
                // Jira lists the signed-in user among the others; offered
                // only when Jira allows it, and not twice
                let mut choices = Vec::new();
                if let Some(me) = &me {
                    choices.push(("Assign to me".to_owned(), Some(me.clone())));
                }
                choices.push(("Unassigned".to_owned(), None));
                choices.extend(
                    people
                        .iter()
                        .filter(|user| Some(*user) != me.as_ref())
                        .map(|user| (user.display_name.clone(), Some(user.clone()))),
                );
                for (label, assignee) in choices {
                    menu = menu.item(PopupMenuItem::new(label).on_click(
                        window.listener_for(&view, move |this, _, window, cx| {
                            this.assign(assignee.clone(), window, cx)
                        }),
                    ));
                }
                menu
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::{in_window, launch, no_worklog_changes, tokio_runtime, wait_until};
    use crate::issue_list::tests::{issue, rows, synced_on};
    use crate::runtime::Core;
    use app_core::AppState;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, App, MouseButton, Role, TestAppContext, Window};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    /// A mock Jira whose project WEB has WEB-1, unassigned. The signed-in user
    /// (`5b10a`, Ada) and Grace can be assigned to it.
    async fn jira() -> MockServer {
        let server = MockServer::start().await;
        no_worklog_changes().mount(&server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "issues": [issue(1, "Fix login")] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/user/assignable/search"))
            .and(query_param("issueKey", "WEB-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "accountId": "5b10a", "displayName": "Ada Lovelace" },
                { "accountId": "7c20b", "displayName": "Grace Hopper" },
            ])))
            .mount(&server)
            .await;
        server
    }

    fn core(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| cx.global::<Core>().state.clone())
    }

    /// Opens WEB-1 from the list and waits for the people it can be assigned to.
    fn open(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("issue-WEB-1").is_some()
        });
        in_window(cx, window, |window, cx| window.click("issue-WEB-1", cx));
        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some("WEB-1")
                && window
                    .try_find("assignee-button")
                    .is_some_and(|button| button.disabled() != Some(true))
        });
    }

    fn assignee(window: &Window) -> String {
        window.find("detail-assignee").label().unwrap().to_owned()
    }

    fn pending(window: &Window) -> bool {
        window.try_find("sync-badge").is_some()
    }

    /// Opens the assignee menu and returns the names of its items.
    fn open_menu(window: &mut Window, cx: &mut App) -> Vec<String> {
        window.click("assignee-button", cx);
        window.render_frame(cx);
        window
            .find_all_by_role(Role::MenuItem)
            .iter()
            .map(|item| item.label().unwrap().to_owned())
            .collect()
    }

    /// Opens the assignee menu and clicks the item `name`.
    fn choose(window: &mut Window, cx: &mut App, name: &str) {
        open_menu(window, cx);
        let items = window.find_all_by_role(Role::MenuItem);
        let item = items.iter().find(|item| item.label() == Some(name));
        let middle = item.expect("no such person").bounds().center();
        window.pointer_down(middle, MouseButton::Left, cx);
        window.pointer_up(middle, MouseButton::Left, cx);
        window.render_frame(cx);
    }

    /// The bodies of the assignee requests Jira got.
    fn puts(tokio: &tokio::runtime::Runtime, server: &MockServer) -> Vec<Value> {
        let sent = tokio.block_on(server.received_requests()).unwrap();
        sent.iter()
            .filter(|request| request.url.path() == "/rest/api/3/issue/WEB-1/assignee")
            .filter(|request| request.method.as_str() == "PUT")
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    async fn accept_assignees(server: &MockServer) {
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/WEB-1/assignee"))
            .respond_with(ResponseTemplate::new(204))
            .mount(server)
            .await;
    }

    #[gpui_kit::test]
    fn the_assignee_menu_offers_the_people_jira_allows_and_oneself_first(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());

        open(cx, window);

        in_window(cx, window, |window, cx| {
            assert_eq!(assignee(window), "Assignee: Unassigned");
            // The signed-in user is offered once, as "Assign to me"
            assert_eq!(
                open_menu(window, cx),
                ["Assign to me", "Unassigned", "Grace Hopper"]
            );
        });
    }

    #[gpui_kit::test]
    fn a_picked_assignee_shows_at_once_and_jira_gets_it_from_the_outbox(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        tokio.block_on(accept_assignees(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window);

        in_window(cx, window, |window, cx| {
            choose(window, cx, "Grace Hopper");
            // The frame of the click: the core has not stored anything yet
            assert_eq!(assignee(window), "Assignee: Grace Hopper");
            assert!(pending(window));
        });
        wait_until(cx, window, |window| {
            window.find("assignee-button").label() == Some("Grace Hopper")
        });

        // The list shows the issue the same way
        in_window(cx, window, |window, cx| window.click("close-detail", cx));
        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, To Do (pending), Grace Hopper".to_owned())
        });
        assert!(puts(&tokio, &server).is_empty());

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, To Do, Grace Hopper".to_owned())
        });
        // The poll that followed brought the issue as Jira has it, which is older
        // than the change: the assignee stays
        assert_eq!(puts(&tokio, &server), [json!({ "accountId": "7c20b" })]);
    }

    #[gpui_kit::test]
    fn assigning_to_oneself_and_to_nobody(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        tokio.block_on(accept_assignees(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window);

        in_window(cx, window, |window, cx| {
            choose(window, cx, "Assign to me");
            assert_eq!(assignee(window), "Assignee: Ada Lovelace");
        });
        wait_until(cx, window, |window| {
            window.find("assignee-button").label() == Some("Ada Lovelace")
        });
        in_window(cx, window, |window, cx| {
            choose(window, cx, "Unassigned");
            assert_eq!(assignee(window), "Assignee: Unassigned");
        });
        wait_until(cx, window, |window| {
            window.find("assignee-button").label() == Some("Unassigned")
        });

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| !pending(window));
        assert_eq!(
            puts(&tokio, &server),
            [
                json!({ "accountId": "5b10a" }),
                json!({ "accountId": null })
            ]
        );
    }

    #[gpui_kit::test]
    fn an_assignee_jira_rejects_is_undone_and_reported(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window);
        tokio.block_on(async {
            Mock::given(method("PUT"))
                .and(path("/rest/api/3/issue/WEB-1/assignee"))
                .respond_with(ResponseTemplate::new(400).set_body_string("user not found"))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/rest/api/3/issue/WEB-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(issue(1, "Fix login")))
                .mount(&server)
                .await;
        });
        in_window(cx, window, |window, cx| {
            choose(window, cx, "Grace Hopper");
            assert_eq!(assignee(window), "Assignee: Grace Hopper");
            assert_eq!(window.notifications(cx).len(), 0);
        });

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| {
            assignee(window) == "Assignee: Unassigned"
        });
        in_window(cx, window, |window, cx| {
            assert!(!pending(window));
            assert_eq!(window.notifications(cx).len(), 1);
        });
    }

    #[gpui_kit::test]
    fn an_assignee_that_cannot_be_saved_is_undone_and_reported(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window);
        let state = core(cx);
        tokio
            .block_on(state.execute(
                "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON sync_queue
                 BEGIN SELECT RAISE(ABORT, 'the outbox is broken'); END",
            ))
            .unwrap();

        in_window(cx, window, |window, cx| {
            choose(window, cx, "Grace Hopper");
            assert_eq!(assignee(window), "Assignee: Grace Hopper");
        });

        wait_until(cx, window, |window| {
            assignee(window) == "Assignee: Unassigned"
        });
        in_window(cx, window, |window, cx| {
            assert!(!pending(window));
            assert_eq!(window.notifications(cx).len(), 1);
        });
    }

    #[gpui_kit::test]
    fn the_menu_of_an_issue_opened_before_works_without_jira(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira());
        let data_dir = synced_on(&tokio, &server.uri(), &[issue(1, "Fix login")]);
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window);
        in_window(cx, window, |window, cx| window.click("close-detail", cx));

        // Jira goes away: nothing listens on its port any more
        drop(server);
        open(cx, window);

        in_window(cx, window, |window, cx| {
            assert_eq!(
                open_menu(window, cx),
                ["Assign to me", "Unassigned", "Grace Hopper"]
            );
        });
    }
}
