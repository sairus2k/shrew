//! The status of an issue as a button whose menu holds the transitions out of it.

use crate::issue::IssueModel;
use crate::runtime::spawn;
use app_core::{api, models::Transition};
use gpui_kit::component::{
    Disableable, Sizable, WindowExt as _,
    button::Button,
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
};
use gpui_kit::*;

pub struct StatusButton {
    issue: Entity<IssueModel>,
    transitions: Vec<Transition>,
    // The status `transitions` were asked for
    loaded_for: Option<String>,
    _load: Task<()>,
    _issue_changed: Subscription,
}

impl StatusButton {
    pub fn new(issue: Entity<IssueModel>, cx: &mut Context<Self>) -> Self {
        // The transition cache is keyed by status, so the options follow the
        // status of the model. After an optimistic transition they change
        // without waiting for Jira.
        let _issue_changed = cx.observe(&issue, |this, _, cx| this.load_transitions(cx));
        let mut this = Self {
            issue,
            transitions: Vec::new(),
            loaded_for: None,
            _load: Task::ready(()),
            _issue_changed,
        };
        this.load_transitions(cx);
        this
    }

    fn load_transitions(&mut self, cx: &mut Context<Self>) {
        let issue = self.issue.read(cx);
        let status = issue.detail.as_ref().map(|detail| detail.status.clone());
        // The model notifies about every change of the issue. Asking only for
        // a new status keeps a status Jira has to be asked about at one request.
        if status.is_none() || status == self.loaded_for {
            return;
        }
        self.loaded_for = status;
        let key = issue.key.clone();
        let load = spawn(cx, move |state| async move {
            api::get_transitions(&state, &key).await
        });
        self._load = cx.spawn(async move |this, cx| {
            let loaded = load.await;
            this.update(cx, |this, cx| {
                if loaded.is_err() {
                    // Offline with nothing cached: the next change of the issue asks again
                    this.loaded_for = None;
                }
                // The transitions of the previous status must not stay in the menu
                this.transitions = loaded.unwrap_or_default();
                cx.notify();
            })
            .ok();
        });
    }

    fn fire(&mut self, transition: Transition, window: &mut Window, cx: &mut Context<Self>) {
        // Optimistic: every view observing the model repaints in this frame
        self.issue
            .update(cx, |issue, cx| issue.apply_transition(&transition, cx));

        let key = self.issue.read(cx).key.clone();
        let write = spawn(cx, move |state| async move {
            api::transition_issue(&state, &key, &transition, &serde_json::json!({})).await
        });
        cx.spawn_in(window, async move |this, cx| {
            // On success the core announces the issue and the model reloads it
            if write.await.is_err() {
                // Nothing was committed, so the cache still holds the previous status
                this.update_in(cx, |this, window, cx| {
                    this.issue.update(cx, |issue, cx| issue.reload(cx));
                    window.push_notification(
                        Notification::error("Could not save the transition"),
                        cx,
                    );
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for StatusButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let transitions = self.transitions.clone();
        let issue = self.issue.read(cx);
        let status = issue.detail.as_ref().map(|detail| detail.status.clone());

        Button::new("status-button")
            .small()
            .label(status.unwrap_or_default())
            .disabled(transitions.is_empty())
            .dropdown_menu(move |mut menu, window, _| {
                for transition in transitions.iter().cloned() {
                    // Jira refuses such a transition without the values of its fields
                    let needs_fields = !transition.required_fields.is_empty();
                    menu = menu.item(
                        PopupMenuItem::new(transition.name.clone())
                            .disabled(needs_fields)
                            .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                this.fire(transition.clone(), window, cx)
                            })),
                    );
                }
                menu
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::harness::{in_window, launch, tokio_runtime, wait_until};
    use crate::issue_list::tests::{issue, rows, synced_on};
    use crate::runtime::Core;
    use app_core::AppState;
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, App, MouseButton, Role, TestAppContext, Window};
    use serde_json::{Value, json};
    use std::{net::TcpListener, sync::Arc};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    /// WEB-1 "Fix login" in To Do and WEB-2 "Fix logout" in In Progress, with
    /// one workflow.
    fn two_issues() -> [Value; 2] {
        let mut started = issue(2, "Fix logout");
        started["fields"]["status"] =
            json!({ "name": "In Progress", "statusCategory": { "key": "indeterminate" } });
        [issue(1, "Fix login"), started]
    }

    /// A mock Jira on `listener` whose project WEB has `two_issues`. It
    /// answers the searches of a sync cycle and says what each status offers.
    async fn jira(listener: TcpListener) -> MockServer {
        let server = MockServer::builder().listener(listener).start().await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "issues": two_issues() })),
            )
            .mount(&server)
            .await;
        let to = |name: &str, category: &str| json!({ "name": name, "statusCategory": { "key": category } });
        let offers = [
            (
                "WEB-1",
                json!([
                    { "id": "21", "name": "Start", "to": to("In Progress", "indeterminate") },
                    { "id": "31", "name": "Close", "to": to("Done", "done") },
                ]),
            ),
            (
                "WEB-2",
                json!([
                    { "id": "41", "name": "Review", "to": to("In Review", "indeterminate") },
                    { "id": "11", "name": "Stop", "to": to("To Do", "new") },
                ]),
            ),
        ];
        for (key, transitions) in offers {
            let status = if key == "WEB-1" {
                "To Do"
            } else {
                "In Progress"
            };
            Mock::given(method("GET"))
                .and(path(format!("/rest/api/3/issue/{key}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({ "key": key, "fields": { "status": { "name": status } } }),
                ))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/rest/api/3/issue/{key}/transitions")))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({ "transitions": transitions })),
                )
                // One question per status: every later menu comes from the cache
                .expect(..=1)
                .mount(&server)
                .await;
        }
        server
    }

    /// Jira accepts the transitions of WEB-1, and must get exactly one.
    async fn accept_one_transition(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(server)
            .await;
    }

    fn local_port() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").unwrap()
    }

    fn core(cx: &mut TestAppContext) -> Arc<AppState> {
        cx.update(|cx| cx.global::<Core>().state.clone())
    }

    /// Opens `key` from the list and waits for its transitions.
    fn open(cx: &mut TestAppContext, window: AnyWindowHandle, key: &'static str) {
        let row = format!("issue-{key}");
        wait_until(cx, window, |window| window.try_find(row.clone()).is_some());
        in_window(cx, window, |window, cx| window.click(row.clone(), cx));
        wait_until(cx, window, |window| {
            window.find("issue-detail").label() == Some(key)
                && window
                    .try_find("status-button")
                    .is_some_and(|button| button.disabled() != Some(true))
        });
    }

    fn close(cx: &mut TestAppContext, window: AnyWindowHandle) {
        in_window(cx, window, |window, cx| window.click("close-detail", cx));
    }

    fn status(window: &Window) -> String {
        window.find("detail-status").label().unwrap().to_owned()
    }

    fn pending(window: &Window) -> bool {
        window.try_find("sync-badge").is_some()
    }

    /// Opens the status menu and returns the names of its items.
    fn open_menu(window: &mut Window, cx: &mut App) -> Vec<String> {
        window.click("status-button", cx);
        window.render_frame(cx);
        window
            .find_all_by_role(Role::MenuItem)
            .iter()
            .map(|item| item.label().unwrap().to_owned())
            .collect()
    }

    /// Opens the status menu and clicks the item `name`.
    fn choose(window: &mut Window, cx: &mut App, name: &str) {
        open_menu(window, cx);
        let items = window.find_all_by_role(Role::MenuItem);
        let item = items.iter().find(|item| item.label() == Some(name));
        let middle = item.expect("no such transition").bounds().center();
        window.pointer_down(middle, MouseButton::Left, cx);
        window.pointer_up(middle, MouseButton::Left, cx);
        window.render_frame(cx);
    }

    #[gpui_kit::test]
    fn the_status_menu_lists_the_transitions_of_the_current_status(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port()));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());

        open(cx, window, "WEB-1");
        in_window(cx, window, |window, cx| {
            assert_eq!(status(window), "Status: To Do");
            assert_eq!(open_menu(window, cx), ["Start", "Close"]);
            window.press("escape", cx);
        });

        close(cx, window);
        open(cx, window, "WEB-2");
        in_window(cx, window, |window, cx| {
            assert_eq!(status(window), "Status: In Progress");
            assert_eq!(open_menu(window, cx), ["Review", "Stop"]);
        });
    }

    #[gpui_kit::test]
    fn a_transition_shows_the_new_status_at_once_and_jira_confirms_it(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port()));
        tokio.block_on(accept_one_transition(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        // WEB-2 is in the status WEB-1 is about to get: opening it fills the
        // cache with what that status offers
        open(cx, window, "WEB-2");
        close(cx, window);
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| {
            choose(window, cx, "Start");
            // The frame of the click: the core has not stored anything yet
            assert_eq!(status(window), "Status: In Progress");
            assert!(pending(window));
        });
        // The menu follows the new status, and Jira is not asked about it
        wait_until(cx, window, |window| {
            window.find("status-button").label() == Some("In Progress")
        });
        let offered = in_window(cx, window, |window, cx| {
            let offered = open_menu(window, cx);
            window.press("escape", cx);
            offered
        });
        assert_eq!(offered, ["Review", "Stop"]);

        // The list shows the issue the same way
        close(cx, window);
        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, In Progress (pending), Unassigned".to_owned())
        });
        open(cx, window, "WEB-1");
        in_window(cx, window, |window, _| assert!(pending(window)));

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| !pending(window));
        in_window(cx, window, |window, _| {
            assert_eq!(status(window), "Status: In Progress");
        });
        close(cx, window);
        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, In Progress, Unassigned".to_owned())
        });
    }

    #[gpui_kit::test]
    fn a_transition_that_cannot_be_saved_is_undone_and_reported(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port()));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        let state = core(cx);
        tokio
            .block_on(state.execute(
                "CREATE TRIGGER outbox_is_broken BEFORE INSERT ON sync_queue
                 BEGIN SELECT RAISE(ABORT, 'the outbox is broken'); END",
            ))
            .unwrap();

        in_window(cx, window, |window, cx| {
            choose(window, cx, "Start");
            assert_eq!(status(window), "Status: In Progress");
            assert_eq!(window.notifications(cx).len(), 0);
        });

        wait_until(cx, window, |window| status(window) == "Status: To Do");
        in_window(cx, window, |window, cx| {
            assert!(!pending(window));
            assert_eq!(window.notifications(cx).len(), 1);
        });
        close(cx, window);
        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, To Do, Unassigned".to_owned())
        });
    }

    #[gpui_kit::test]
    fn a_transition_made_offline_is_delivered_after_reconnect(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let port = local_port();
        let address = port.local_addr().unwrap();
        let server = tokio.block_on(jira(port));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        // Jira goes away: nothing listens on its port any more
        drop(server);
        in_window(cx, window, |window, cx| {
            choose(window, cx, "Start");
            assert_eq!(status(window), "Status: In Progress");
        });
        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap_err();
        wait_until(cx, window, |window| {
            window.find("status-button").label() == Some("In Progress")
        });
        in_window(cx, window, |window, _| assert!(pending(window)));

        // Jira is back at the same address
        let server = tokio.block_on(jira(TcpListener::bind(address).unwrap()));
        tokio.block_on(accept_one_transition(&server));
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| !pending(window));
        in_window(cx, window, |window, _| {
            assert_eq!(status(window), "Status: In Progress");
        });
        let sent = tokio.block_on(server.received_requests()).unwrap();
        let transition = sent
            .iter()
            .find(|request| request.url.path() == "/rest/api/3/issue/WEB-1/transitions")
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&transition.body).unwrap(),
            json!({ "transition": { "id": "21" }, "fields": {} })
        );
    }
}
