//! Which projects to keep locally: the second screen of a new install, and
//! later a screen the user opens from the main window.

use crate::runtime::spawn;
use app_core::{
    api,
    models::{Project, SetupStep},
};
use gpui_kit::component::{
    ActiveTheme, Disableable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
};
use gpui_kit::*;

/// Emitted once the core has saved the watched projects. Carries the setup
/// step that follows: the download of new projects, or the app itself.
pub struct ProjectsChosen(pub SetupStep);

/// Emitted when the user leaves the screen without saving.
pub struct ProjectsCancelled;

pub struct ProjectPicker {
    // Opened from the main window, not as a step of the setup
    editing: bool,
    // `None` until Jira has answered
    projects: Option<Vec<Project>>,
    // Project keys
    watched: Vec<String>,
    error: Option<SharedString>,
    loading: Task<()>,
    saving: Option<Task<()>>,
}

impl EventEmitter<ProjectsChosen> for ProjectPicker {}
impl EventEmitter<ProjectsCancelled> for ProjectPicker {}

impl ProjectPicker {
    pub fn new(editing: bool, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            editing,
            projects: None,
            watched: Vec::new(),
            error: None,
            loading: Task::ready(()),
            saving: None,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let load = spawn(cx, |state| async move {
            // Nothing is watched during the setup
            Ok((
                api::projects(&state).await?,
                api::watched_projects(&state).await?,
            ))
        });
        self.error = None;
        self.loading = cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((projects, watched)) => {
                        this.projects = Some(projects);
                        this.watched = watched;
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not load the projects: {error}").into())
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn toggle(&mut self, key: &str, watch: bool, cx: &mut Context<Self>) {
        self.watched.retain(|watched| watched != key);
        if watch {
            self.watched.push(key.to_owned());
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let watched = self.watched.clone();
        let save = spawn(cx, move |state| async move {
            api::watch_projects(&state, &watched).await?;
            api::setup_step(&state).await
        });
        self.error = None;
        self.saving = Some(cx.spawn(async move |this, cx| {
            let result = save.await;
            this.update(cx, |this, cx| {
                this.saving = None;
                match result {
                    Ok(step) => cx.emit(ProjectsChosen(step)),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.error.clone().map(|error| {
            div()
                .id("projects-error")
                .role(Role::Alert)
                .test_support()
                .aria_label(error.clone())
                .text_color(cx.theme().danger)
                .child(error)
        });
        let cancel = self.editing.then(|| {
            Button::new("cancel-projects")
                .label("Cancel")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ProjectsCancelled)))
        });
        let form = div()
            .w(px(360.))
            .max_h_full()
            .py_6()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("choose-projects")
                    .test_support()
                    .text_xl()
                    .child("Choose the projects to watch"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("All their issues are downloaded, so that you can search them."),
            );
        let form = match &self.projects {
            Some(projects) => form
                .child(
                    div()
                        .id("projects")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(projects.iter().map(|project| {
                            let key = project.key.clone();
                            Checkbox::new(SharedString::from(format!("project-{key}")))
                                .label(format!("{key} — {}", project.name))
                                .checked(self.watched.contains(&key))
                                .on_click(cx.listener(move |this, watch: &bool, _, cx| {
                                    this.toggle(&key, *watch, cx)
                                }))
                        })),
                )
                .children(error)
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("watch-projects")
                                .primary()
                                .label(if self.editing { "Save" } else { "Start sync" })
                                .disabled(self.watched.is_empty())
                                .loading(self.saving.is_some())
                                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                        )
                        .children(cancel),
                ),
            None if error.is_some() => form.children(error).child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("retry-projects")
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                    )
                    .children(cancel),
            ),
            None => form.child("Loading projects…").children(cancel),
        };
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
        harness::{in_window, launch, tokio_runtime, wait_until},
        issue_list::tests::{api_issue, choose_project, issue, rows, synced_on, watch},
        runtime::Core,
        sign_in::tests::{fill_in_and_submit, jira},
    };
    use app_core::{api, models::SetupStep};
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AnyWindowHandle, TestAppContext};
    use serde_json::json;
    use std::time::Duration;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, method, path, path_regex},
    };

    #[gpui_kit::test]
    fn the_chosen_projects_are_saved_and_the_sync_starts(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(
            &tokio,
            ResponseTemplate::new(200)
                .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" })),
        );
        tokio.block_on(
            Mock::given(method("GET"))
                .and(path("/rest/api/3/project/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "isLast": true,
                    "values": [
                        { "id": "10001", "key": "API", "name": "Backend" },
                        { "id": "10000", "key": "WEB", "name": "Website" },
                    ],
                })))
                .mount(&server),
        );
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());
        fill_in_and_submit(cx, window, &server.uri());

        wait_until(cx, window, |window| {
            window.try_find("project-WEB").is_some()
        });
        in_window(cx, window, |window, cx| {
            assert_eq!(window.find("project-API").label(), Some("API — Backend"));
            assert_eq!(window.find("project-WEB").label(), Some("WEB — Website"));
            assert_eq!(window.find("project-WEB").checked(), Some(false));

            // With no project chosen there is nothing to sync
            window.click("watch-projects", cx);
        });
        cx.run_until_parked();
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("initial-sync").is_none());

            window.click("project-WEB", cx);
        });
        in_window(cx, window, |window, cx| {
            assert_eq!(window.find("project-WEB").checked(), Some(true));
            assert_eq!(window.find("project-API").checked(), Some(false));

            window.click("watch-projects", cx);
        });

        wait_until(cx, window, |window| {
            window.try_find("initial-sync").is_some()
        });
        in_window(cx, window, |window, _| {
            assert!(window.try_find("choose-projects").is_none());
        });

        // The choice was saved: a restart does not ask again
        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("initial-sync").is_some());
            assert!(window.try_find("choose-projects").is_none());
        });
        assert_eq!(watched(cx, &tokio), ["WEB"]);
    }

    fn mount_projects(tokio: &tokio::runtime::Runtime, server: &MockServer) {
        tokio.block_on(
            Mock::given(method("GET"))
                .and(path("/rest/api/3/project/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "isLast": true,
                    "values": [
                        { "id": "10001", "key": "API", "name": "Backend" },
                        { "id": "10000", "key": "WEB", "name": "Website" },
                    ],
                })))
                .mount(server),
        );
    }

    /// A signed-in app whose finished sync stored WEB-1 and, if it watches API, API-101.
    fn synced_watching(
        tokio: &tokio::runtime::Runtime,
        server: &MockServer,
        watched: &[&str],
    ) -> tempfile::TempDir {
        let mut issues = vec![issue(1, "Fix login")];
        if watched.contains(&"API") {
            issues.push(api_issue(101, "Login endpoint"));
        }
        let data_dir = synced_on(tokio, &server.uri(), &issues);
        watch(tokio, data_dir.path(), watched);
        data_dir
    }

    /// The watched projects the core has saved.
    fn watched(cx: &mut TestAppContext, tokio: &tokio::runtime::Runtime) -> Vec<String> {
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        tokio.block_on(api::watched_projects(&state)).unwrap()
    }

    fn open_projects(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("edit-projects").is_some()
        });
        in_window(cx, window, |window, cx| window.click("edit-projects", cx));
        wait_until(cx, window, |window| {
            window.try_find("project-WEB").is_some()
        });
    }

    const WEB_LOGIN: &str = "WEB-1: Fix login, To Do, Unassigned";
    const API_LOGIN: &str = "API-101: Login endpoint, To Do, Unassigned";

    /// The issues of API that Jira has, one page, with no worklogs.
    fn mount_api(tokio: &tokio::runtime::Runtime, server: &MockServer, response: ResponseTemplate) {
        tokio.block_on(async {
            Mock::given(path("/rest/api/3/search/approximate-count"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 1 })))
                .mount(server)
                .await;
            Mock::given(path("/rest/api/3/search/jql"))
                .and(body_partial_json(
                    json!({ "jql": "project = \"API\" ORDER BY updated DESC" }),
                ))
                .respond_with(response)
                .mount(server)
                .await;
            // An issue that enters the cache is asked for its existing worklogs
            Mock::given(path_regex(r"^/rest/api/3/issue/API-\d+/worklog$"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({ "startAt": 0, "total": 0, "worklogs": [] })),
                )
                .mount(server)
                .await;
        });
    }

    #[gpui_kit::test]
    fn the_projects_can_be_opened_from_the_main_window_and_left_without_saving(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(200));
        mount_projects(&tokio, &server);
        let data_dir = synced_watching(&tokio, &server, &["WEB", "API"]);
        let window = launch(cx, &tokio, data_dir.path());

        open_projects(cx, window);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("project-API").checked(), Some(true));
            assert_eq!(window.find("project-WEB").checked(), Some(true));
        });
        in_window(cx, window, |window, cx| window.click("project-API", cx));
        in_window(cx, window, |window, cx| {
            assert_eq!(window.find("project-API").checked(), Some(false));
            window.click("cancel-projects", cx);
        });

        wait_until(cx, window, |window| {
            rows(window).len() == 2 && window.try_find("choose-projects").is_none()
        });
        assert_eq!(watched(cx, &tokio), ["WEB", "API"]);
        // The cache is untouched, and so is the choice after a restart
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| rows(window).len() == 2);
    }

    #[gpui_kit::test]
    fn a_selection_with_no_project_cannot_be_saved(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(200));
        mount_projects(&tokio, &server);
        let data_dir = synced_watching(&tokio, &server, &["WEB"]);
        let window = launch(cx, &tokio, data_dir.path());
        open_projects(cx, window);

        in_window(cx, window, |window, cx| {
            window.click("project-WEB", cx);
            assert_eq!(window.find("project-WEB").checked(), Some(false));
            window.click("watch-projects", cx);
        });
        cx.run_until_parked();

        in_window(cx, window, |window, _| {
            assert!(window.try_find("choose-projects").is_some());
            assert!(window.try_find("home").is_none());
        });
        assert_eq!(watched(cx, &tokio), ["WEB"]);
    }

    #[gpui_kit::test]
    fn an_added_project_is_downloaded_with_progress_and_the_cached_ones_are_not(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(200));
        mount_projects(&tokio, &server);
        mount_api(
            &tokio,
            &server,
            // Slow enough for the progress to be seen
            ResponseTemplate::new(200)
                .set_body_json(json!({ "issues": [api_issue(101, "Login endpoint")] }))
                .set_delay(Duration::from_millis(300)),
        );
        let data_dir = synced_watching(&tokio, &server, &["WEB"]);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| rows(window) == [WEB_LOGIN]);
        in_window(cx, window, |window, _| {
            assert!(window.try_find("project-filter").is_none());
        });
        open_projects(cx, window);

        in_window(cx, window, |window, cx| {
            assert_eq!(window.find("project-API").checked(), Some(false));
            window.click("project-API", cx);
            window.click("watch-projects", cx);
        });

        wait_until(cx, window, |window| {
            window.try_find("sync-progress-API").is_some()
        });
        in_window(cx, window, |window, _| {
            assert_eq!(
                window.find("sync-progress-API").label(),
                Some("API: 0 / ~1 issues")
            );
            assert_eq!(
                window.find("initial-sync-note").label(),
                Some(
                    "Downloading the issues of the projects you added. Later syncs run in the background."
                )
            );
        });
        wait_until(cx, window, |window| window.try_find("home").is_some());
        wait_until(cx, window, |window| rows(window) == [API_LOGIN, WEB_LOGIN]);
        in_window(cx, window, |window, _| {
            assert!(window.try_find("project-filter").is_some());
        });
        // Only API was asked for
        let asked = tokio.block_on(server.received_requests()).unwrap();
        assert!(
            !asked
                .iter()
                .any(|request| String::from_utf8_lossy(&request.body).contains("project = \"WEB\""))
        );
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        let step = tokio.block_on(api::setup_step(&state)).unwrap();
        assert_eq!(step, SetupStep::Ready);
    }

    #[gpui_kit::test]
    fn a_download_of_an_added_project_interrupted_by_a_quit_completes_on_the_next_start(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(200));
        mount_projects(&tokio, &server);
        mount_api(&tokio, &server, ResponseTemplate::new(503));
        let data_dir = synced_watching(&tokio, &server, &["WEB"]);
        let window = launch(cx, &tokio, data_dir.path());
        open_projects(cx, window);
        in_window(cx, window, |window, cx| {
            window.click("project-API", cx);
            window.click("watch-projects", cx);
        });
        wait_until(cx, window, |window| window.try_find("sync-error").is_some());

        tokio.block_on(server.reset());
        tokio.block_on(crate::harness::no_sprint_field().mount(&server));
        mount_api(
            &tokio,
            &server,
            ResponseTemplate::new(200)
                .set_body_json(json!({ "issues": [api_issue(101, "Login endpoint")] })),
        );
        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("initial-sync").is_some());
        });

        wait_until(cx, window, |window| rows(window) == [API_LOGIN, WEB_LOGIN]);
    }

    #[gpui_kit::test]
    fn a_removed_project_leaves_the_list_the_search_and_the_filter(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(&tokio, ResponseTemplate::new(200));
        mount_projects(&tokio, &server);
        let data_dir = synced_watching(&tokio, &server, &["WEB", "API"]);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| rows(window) == [API_LOGIN, WEB_LOGIN]);
        // The project that is about to be removed is the one the list is narrowed to
        in_window(cx, window, |window, cx| choose_project(window, cx, "API"));
        wait_until(cx, window, |window| rows(window) == [API_LOGIN]);
        open_projects(cx, window);

        in_window(cx, window, |window, cx| {
            window.click("project-API", cx);
            window.click("watch-projects", cx);
        });

        wait_until(cx, window, |window| rows(window) == [WEB_LOGIN]);
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("project-filter").is_none());
            window.click("search", cx);
            window.input("login", cx);
        });
        wait_until(cx, window, |window| rows(window) == [WEB_LOGIN]);
        assert_eq!(watched(cx, &tokio), ["WEB"]);
    }

    #[gpui_kit::test]
    fn projects_that_cannot_be_loaded_can_be_asked_for_again(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = jira(
            &tokio,
            ResponseTemplate::new(200)
                .set_body_json(json!({ "accountId": "5b10a", "displayName": "Ada" })),
        );
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());
        // The mock server has no project list yet: it answers 404
        fill_in_and_submit(cx, window, &server.uri());
        wait_until(cx, window, |window| {
            window.try_find("projects-error").is_some()
        });

        tokio.block_on(
            Mock::given(method("GET"))
                .and(path("/rest/api/3/project/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "isLast": true,
                    "values": [{ "id": "10000", "key": "WEB", "name": "Website" }],
                })))
                .mount(&server),
        );
        in_window(cx, window, |window, cx| window.click("retry-projects", cx));

        wait_until(cx, window, |window| {
            window.try_find("project-WEB").is_some()
        });
        in_window(cx, window, |window, _| {
            assert!(window.try_find("projects-error").is_none());
        });
    }
}
