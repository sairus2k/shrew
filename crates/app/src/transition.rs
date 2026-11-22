//! The status of an issue as a button whose menu holds the transitions out of it.

use crate::issue::IssueModel;
use crate::keys;
use crate::runtime::spawn;
use crate::theme::status_dot;
use app_core::{
    api,
    models::{RequiredField, Transition},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, WindowExt as _,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    select::{Select, SelectEvent, SelectItem, SelectState},
};
use gpui_kit::*;
use serde_json::{Value, json};

pub struct StatusButton {
    issue: Entity<IssueModel>,
    transitions: Vec<Transition>,
    // The status `transitions` were asked for
    loaded_for: Option<String>,
    // A transition is being written to the core
    saving: bool,
    // Open while the user fills in the fields of a transition
    form: Option<Entity<TransitionFieldsForm>>,
    // Of the button, for the shortcut that opens its menu
    focus: FocusHandle,
    _form_events: Option<Subscription>,
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
            saving: false,
            form: None,
            focus: cx.focus_handle(),
            _form_events: None,
            _load: Task::ready(()),
            _issue_changed,
        };
        this.load_transitions(cx);
        this
    }

    /// Opens the menu, unless the button is disabled for having no transitions.
    pub fn open_menu(&self, window: &mut Window, cx: &mut App) {
        if !self.transitions.is_empty() {
            crate::keys::open_menu(&self.focus, window, cx);
        }
    }

    fn load_transitions(&mut self, cx: &mut Context<Self>) {
        let issue = self.issue.read(cx);
        let status = issue.detail.as_ref().map(|detail| detail.status.clone());
        // The model notifies about every change of the issue. Asking only for
        // a new status keeps a status Jira has to be asked about at one request.
        if status.is_none() || status == self.loaded_for {
            return;
        }
        let key = issue.key.clone();
        // Until the new status answers, the button is disabled instead of
        // offering transitions of the status the issue just left
        self.transitions.clear();
        cx.notify();
        // The core asks its database for the status, which the write of the
        // transition has not reached yet: it would answer for the old one
        if self.saving {
            return;
        }
        self.loaded_for = status;
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

    fn ask_fields(&mut self, transition: Transition, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| TransitionFieldsForm::new(&transition, window, cx));
        self._form_events = Some(cx.subscribe_in(
            &form,
            window,
            move |this, _, event: &FormEvent, window, cx| {
                this.form = None;
                this._form_events = None;
                if let FormEvent::Submitted(fields) = event {
                    this.fire(transition.clone(), fields.clone(), window, cx);
                }
                cx.notify();
            },
        ));
        self.form = Some(form);
        cx.notify();
    }

    fn fire(
        &mut self,
        transition: Transition,
        fields: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving = true;
        // Optimistic: every view observing the model repaints in this frame
        self.issue
            .update(cx, |issue, cx| issue.apply_transition(&transition, cx));

        let key = self.issue.read(cx).key.clone();
        let write = spawn(cx, move |state| async move {
            api::transition_issue(&state, &key, &transition, &fields).await
        });
        cx.spawn_in(window, async move |this, cx| {
            // On success the core announces the issue and the model reloads it
            let saved = write.await.is_ok();
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                this.loaded_for = None;
                this.load_transitions(cx);
                if !saved {
                    // Nothing was committed, so the cache still holds the previous status
                    this.issue.update(cx, |issue, cx| issue.reload(cx));
                    window.push_notification(
                        Notification::error("Could not save the transition"),
                        cx,
                    );
                }
            })
            .ok();
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
        let dot = issue
            .detail
            .as_ref()
            .map(|detail| status_dot(detail.status_category, cx));
        let url = issue.detail.as_ref().map(|detail| detail.url.clone());

        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .child(
                Button::new("status-button")
                    .track_focus(&self.focus)
                    .small()
                    .outline()
                    .dropdown_caret(true)
                    .accessibility_label(status.clone().unwrap_or_default())
                    .children(dot)
                    .child(status.unwrap_or_default())
                    .children(keys::hint(&keys::ChangeStatus, cx))
                    .disabled(transitions.is_empty())
                    .dropdown_menu(move |mut menu, window, _| {
                        for transition in transitions.iter().cloned() {
                            if !transition.required_fields.iter().all(is_supported) {
                                menu = menu.item(
                                    PopupMenuItem::new(transition.name.clone()).disabled(true),
                                );
                                if let Some(url) = &url {
                                    menu = menu.item(PopupMenuItem::link("Open in Jira", url));
                                }
                                continue;
                            }
                            menu = menu.item(PopupMenuItem::new(transition.name.clone()).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    if transition.required_fields.is_empty() {
                                        this.fire(transition.clone(), json!({}), window, cx);
                                    } else {
                                        this.ask_fields(transition.clone(), window, cx);
                                    }
                                }),
                            ));
                        }
                        menu
                    }),
            )
            .children(self.form.clone())
    }
}

/// What the form has controls for: a value out of a list Jira gives, or text.
fn is_supported(field: &RequiredField) -> bool {
    field.allowed_values.is_some() || field.schema["type"] == "string"
}

enum FormEvent {
    Submitted(Value),
    Cancelled,
}

/// One of the values Jira allows for a field. Jira takes it back by its `id`.
#[derive(Clone)]
struct Choice {
    title: SharedString,
    value: Value,
}

impl Choice {
    fn new(allowed: &Value) -> Self {
        let id = allowed["id"].as_str();
        let title = ["name", "value"]
            .iter()
            .find_map(|key| allowed[key].as_str())
            .or(id)
            .map_or_else(|| allowed.to_string(), str::to_owned);
        Self {
            title: title.into(),
            value: id.map_or_else(|| allowed.clone(), |id| json!({ "id": id })),
        }
    }
}

impl SelectItem for Choice {
    type Value = Value;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Value {
        &self.value
    }
}

enum Control {
    Choice(Entity<SelectState<Vec<Choice>>>),
    Text(Entity<InputState>),
}

struct Field {
    id: String,
    name: String,
    control: Control,
}

impl Field {
    /// `None` until the user has picked or typed something.
    fn value(&self, cx: &App) -> Option<Value> {
        match &self.control {
            Control::Choice(select) => select.read(cx).selected_value().cloned(),
            Control::Text(input) => {
                let text = input.read(cx).value();
                let text = text.trim();
                (!text.is_empty()).then(|| json!(text))
            }
        }
    }
}

/// Asks for the fields a transition cannot be sent without.
struct TransitionFieldsForm {
    fields: Vec<Field>,
    _changes: Vec<Subscription>,
}

impl EventEmitter<FormEvent> for TransitionFieldsForm {}

impl TransitionFieldsForm {
    fn new(transition: &Transition, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut changes = Vec::new();
        let fields = transition
            .required_fields
            .iter()
            .map(|field| {
                let control = match &field.allowed_values {
                    Some(allowed) => {
                        let choices = allowed.iter().map(Choice::new).collect::<Vec<_>>();
                        let select = cx.new(|cx| SelectState::new(choices, None, window, cx));
                        changes.push(
                            cx.subscribe(&select, |_, _, _: &SelectEvent<Vec<Choice>>, cx| {
                                cx.notify()
                            }),
                        );
                        Control::Choice(select)
                    }
                    None => {
                        let input = cx.new(|cx| InputState::new(window, cx));
                        changes.push(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
                        Control::Text(input)
                    }
                };
                Field {
                    id: field.id.clone(),
                    name: field.name.clone(),
                    control,
                }
            })
            .collect();
        Self {
            fields,
            _changes: changes,
        }
    }

    /// The `fields` of the transition request, once every field has a value.
    fn collected(&self, cx: &App) -> Option<Value> {
        let mut collected = serde_json::Map::new();
        for field in &self.fields {
            collected.insert(field.id.clone(), field.value(cx)?);
        }
        Some(Value::Object(collected))
    }
}

impl Render for TransitionFieldsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collected = self.collected(cx);
        div()
            .id("transition-fields")
            .test_support()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .children(self.fields.iter().map(|field| {
                let id = format!("field-{}", field.id);
                let control = match &field.control {
                    Control::Choice(select) => Select::new(select)
                        .id(id)
                        .accessibility_label(field.name.clone())
                        .placeholder("Select")
                        .into_any_element(),
                    Control::Text(input) => Input::new(input)
                        .id(id)
                        .aria_label(field.name.clone())
                        .into_any_element(),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child(field.name.clone()))
                    .child(control)
            }))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("submit-fields")
                            .primary()
                            .small()
                            .label("Move")
                            .disabled(collected.is_none())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(fields) = &collected {
                                    cx.emit(FormEvent::Submitted(fields.clone()));
                                }
                            })),
                    )
                    .child(
                        Button::new("cancel-fields")
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Cancelled))),
                    ),
            )
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

    fn to(name: &str, category: &str) -> Value {
        json!({ "name": name, "statusCategory": { "key": category } })
    }

    /// What each status of `two_issues` offers.
    fn offers() -> [(&'static str, Value); 2] {
        [
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
        ]
    }

    /// A mock Jira on `listener` whose project WEB has `two_issues`. It
    /// answers the searches of a sync cycle and says what each status offers.
    async fn jira(listener: TcpListener) -> MockServer {
        jira_offering(listener, offers()).await
    }

    async fn jira_offering(
        listener: TcpListener,
        offers: [(&'static str, Value); 2],
    ) -> MockServer {
        jira_asked_about_menus(listener, offers, 1).await
    }

    /// Like `jira_offering`, but a menu may be asked for up to `at_most` times.
    async fn jira_asked_about_menus(
        listener: TcpListener,
        offers: [(&'static str, Value); 2],
        at_most: u64,
    ) -> MockServer {
        let server = MockServer::builder().listener(listener).start().await;
        no_worklog_changes().mount(&server).await;
        crate::harness::no_sprint_field().mount(&server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "issues": two_issues() })),
            )
            .mount(&server)
            .await;
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
                .expect(..=at_most)
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
        rows(window).iter().any(|row| row.contains("(pending)"))
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
        });
        wait_until(cx, window, |window| pending(window));
        // The menu follows the new status, and Jira is not asked about it
        wait_until(cx, window, |window| {
            let button = window.find("status-button");
            button.label() == Some("In Progress") && button.disabled() != Some(true)
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
    fn a_second_transition_can_follow_the_first_at_once(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port()));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        // The sync stores what the status of each issue offers. WEB-2 is never
        // opened, and it is in the status WEB-1 is about to get.
        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();
        open(cx, window, "WEB-1");
        // Nothing can be asked or delivered from here on
        drop(server);

        in_window(cx, window, |window, cx| choose(window, cx, "Start"));
        wait_until(cx, window, |window| {
            let button = window.find("status-button");
            button.label() == Some("In Progress") && button.disabled() != Some(true)
        });
        in_window(cx, window, |window, cx| {
            choose(window, cx, "Review");
            assert_eq!(status(window), "Status: In Review");
        });

        // The list shows what the core stored
        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, In Review (pending), Unassigned".to_owned())
        });
        // No cached issue is in In Review, so what it offers is not known
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("status-button").disabled(), Some(true));
        });
    }

    /// WEB-1 offers a plain "Start", "Close" behind a screen asking for a
    /// resolution and a root cause, and "Reassign" behind one asking for a user.
    fn screens() -> [(&'static str, Value); 2] {
        let resolutions = json!([
            { "id": "10000", "name": "Done", "self": "http://jira/resolution/10000" },
            { "id": "10001", "name": "Won't Do", "self": "http://jira/resolution/10001" },
        ]);
        let [(_, _), other] = offers();
        [
            (
                "WEB-1",
                json!([
                    { "id": "21", "name": "Start", "to": to("In Progress", "indeterminate") },
                    {
                        "id": "31",
                        "name": "Close",
                        "to": to("Done", "done"),
                        "fields": {
                            "resolution": {
                                "required": true,
                                "hasDefaultValue": false,
                                "name": "Resolution",
                                "schema": { "type": "resolution", "system": "resolution" },
                                "allowedValues": resolutions,
                            },
                            "customfield_10050": {
                                "required": true,
                                "hasDefaultValue": false,
                                "name": "Root cause",
                                "schema": { "type": "string" },
                            },
                        },
                    },
                    {
                        "id": "51",
                        "name": "Reassign",
                        "to": to("In Progress", "indeterminate"),
                        "fields": {
                            "assignee": {
                                "required": true,
                                "hasDefaultValue": false,
                                "name": "Assignee",
                                "schema": { "type": "user", "system": "assignee" },
                            },
                        },
                    },
                ]),
            ),
            other,
        ]
    }

    /// Chooses "Close" and waits for the form it opens.
    fn choose_close(cx: &mut TestAppContext, window: AnyWindowHandle) {
        in_window(cx, window, |window, cx| choose(window, cx, "Close"));
        wait_until(cx, window, |window| {
            window.try_find("transition-fields").is_some()
        });
    }

    /// Picks the first option of the select of the field `field`, which shows `option` after it.
    fn pick_first(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        field: &'static str,
        option: &str,
    ) {
        in_window(cx, window, |window, cx| {
            window.within(format!("field-{field}")).click("input", cx);
            window.press("down", cx);
            window.press("enter", cx);
        });
        // The select commits after the key has been dispatched
        wait_until(cx, window, |window| {
            window.find(format!("field-{field}")).value() == Some(option)
        });
    }

    fn type_into(window: &mut Window, cx: &mut App, field: &str, text: &str) {
        window.click(format!("field-{field}"), cx);
        window.input(text, cx);
        window.render_frame(cx);
    }

    fn can_submit(window: &Window) -> bool {
        window.find("submit-fields").disabled() != Some(true)
    }

    /// The bodies of the transition requests Jira got.
    fn posts(tokio: &tokio::runtime::Runtime, server: &MockServer) -> Vec<Value> {
        let sent = tokio.block_on(server.received_requests()).unwrap();
        sent.iter()
            .filter(|request| request.url.path() == "/rest/api/3/issue/WEB-1/transitions")
            .filter(|request| request.method.as_str() == "POST")
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }

    #[gpui_kit::test]
    fn a_transition_with_required_fields_is_sent_once_the_form_is_filled_in(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        // Jira keeps saying To Do after the transition, so the menu of Done is
        // asked for again at every change of the issue
        let server = tokio.block_on(jira_asked_about_menus(local_port(), screens(), u64::MAX));
        tokio.block_on(accept_one_transition(&server));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        choose_close(cx, window);

        in_window(cx, window, |window, cx| {
            // Nothing is enqueued before the form is submitted
            assert_eq!(status(window), "Status: To Do");
            assert!(!pending(window));
            // One control per field: a select for the values Jira allows, an input for text
            assert_eq!(window.find("field-resolution").role(), Some(Role::ComboBox));
            assert_ne!(
                window.find("field-customfield_10050").role(),
                Some(Role::ComboBox)
            );
            assert!(!can_submit(window));

            type_into(window, cx, "customfield_10050", "Cache never expired");
            assert!(!can_submit(window));
        });
        pick_first(cx, window, "resolution", "Done");
        in_window(cx, window, |window, cx| {
            assert!(can_submit(window));

            window.click("submit-fields", cx);
        });
        wait_until(cx, window, |window| {
            window.try_find("transition-fields").is_none()
        });
        in_window(cx, window, |window, _| {
            assert_eq!(status(window), "Status: Done");
            assert!(pending(window));
        });

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();
        wait_until(cx, window, |window| !pending(window));
        assert_eq!(
            posts(&tokio, &server),
            [json!({
                "transition": { "id": "31" },
                "fields": {
                    "resolution": { "id": "10000" },
                    "customfield_10050": "Cache never expired",
                },
            })]
        );
    }

    #[gpui_kit::test]
    fn cancelling_the_form_changes_nothing(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira_offering(local_port(), screens()));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        choose_close(cx, window);
        pick_first(cx, window, "resolution", "Done");
        in_window(cx, window, |window, cx| {
            window.click("cancel-fields", cx);
        });
        wait_until(cx, window, |window| {
            window.try_find("transition-fields").is_none()
        });
        in_window(cx, window, |window, _| {
            assert_eq!(status(window), "Status: To Do");
            assert!(!pending(window));
        });

        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        assert_eq!(posts(&tokio, &server), Vec::<Value>::new());
        // A form that is opened again starts empty
        choose_close(cx, window);
        in_window(cx, window, |window, _| assert!(!can_submit(window)));
    }

    #[gpui_kit::test]
    fn a_transition_with_an_unsupported_field_is_disabled_and_links_to_jira(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira_offering(local_port(), screens()));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");

        in_window(cx, window, |window, cx| {
            open_menu(window, cx);
            let items = window.find_all_by_role(Role::MenuItem);
            let names: Vec<_> = items.iter().map(|item| item.label().unwrap()).collect();
            assert_eq!(names, ["Start", "Close", "Reassign", "Open in Jira"]);

            // The disabled item does nothing
            let reassign = items[2].bounds().center();
            window.pointer_down(reassign, MouseButton::Left, cx);
            window.pointer_up(reassign, MouseButton::Left, cx);
            window.render_frame(cx);
            assert_eq!(status(window), "Status: To Do");
            assert!(!pending(window));
            assert!(window.try_find("transition-fields").is_none());

            let link = window.find_all_by_role(Role::MenuItem)[3].bounds().center();
            window.pointer_down(link, MouseButton::Left, cx);
            window.pointer_up(link, MouseButton::Left, cx);
        });

        assert_eq!(
            cx.opened_url().as_deref(),
            Some(&*format!("{}/browse/WEB-1", server.uri()))
        );
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
        // Jira still says To Do, so each change of the issue asks for its menu
        // again, and how many changes arrive depends on timing
        let server = tokio.block_on(jira_asked_about_menus(
            TcpListener::bind(address).unwrap(),
            offers(),
            u64::MAX,
        ));
        tokio.block_on(accept_one_transition(&server));
        // A pooled connection to the old server can still be in use, and the
        // request on it fails with "connection closed before message
        // completed". The next sync cycle opens a new one.
        if tokio.block_on(state.sync_once()).is_err() {
            tokio.block_on(state.sync_once()).unwrap();
        }

        wait_until(cx, window, |window| !pending(window));
        in_window(cx, window, |window, _| {
            assert_eq!(status(window), "Status: In Progress");
        });
        let sent = tokio.block_on(server.received_requests()).unwrap();
        let transition = sent
            .iter()
            .find(|request| {
                request.method.as_str() == "POST"
                    && request.url.path() == "/rest/api/3/issue/WEB-1/transitions"
            })
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&transition.body).unwrap(),
            json!({ "transition": { "id": "21" }, "fields": {} })
        );
    }

    /// Jira after it has said no to a transition of WEB-1: the issue is still
    /// in To Do, and the menu of To Do is asked for again after the rejection.
    async fn jira_rejects_the_transition(server: &MockServer) {
        server.reset().await;
        let [(_, to_do), _] = offers();
        let mut web_1 = two_issues()[0].clone();
        web_1["fields"]["status"] = to("To Do", "new");
        no_worklog_changes().mount(server).await;
        crate::harness::no_sprint_field().mount(server).await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "issues": two_issues() })),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(web_1))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "transitions": to_do })))
            // Whether the menu of the status the issue is moved to is asked for,
            // and whether To Do is asked for again or comes from the cache,
            // depends on how the question races the rejection. That the
            // rejection drops the cached menu is checked in the core.
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/WEB-1/transitions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Resolution is required"))
            .expect(1)
            .mount(server)
            .await;
    }

    #[gpui_kit::test]
    fn a_transition_jira_rejects_is_undone_and_reported(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(jira(local_port()));
        let data_dir = synced_on(&tokio, &server.uri(), &two_issues());
        let window = launch(cx, &tokio, data_dir.path());
        open(cx, window, "WEB-1");
        tokio.block_on(jira_rejects_the_transition(&server));
        in_window(cx, window, |window, cx| {
            choose(window, cx, "Start");
            assert_eq!(status(window), "Status: In Progress");
            assert_eq!(window.notifications(cx).len(), 0);
        });
        wait_until(cx, window, |window| pending(window));

        // A sync cycle sends it and Jira says no
        let state = core(cx);
        tokio.block_on(state.sync_once()).unwrap();

        wait_until(cx, window, |window| status(window) == "Status: To Do");
        in_window(cx, window, |window, cx| {
            assert!(!pending(window));
            assert_eq!(window.find("status-button").label(), Some("To Do"));
            assert_eq!(window.notifications(cx).len(), 1);
        });
        close(cx, window);
        wait_until(cx, window, |window| {
            rows(window).contains(&"WEB-1: Fix login, To Do, Unassigned".to_owned())
        });
    }
}
