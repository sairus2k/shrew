//! The second screen of a new install: which projects to keep locally.

use crate::runtime::spawn;
use app_core::{api, models::Project};
use gpui_kit::component::{
    ActiveTheme, Disableable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
};
use gpui_kit::*;

/// Emitted once the core has saved the watched projects.
pub struct ProjectsChosen;

pub struct ProjectPicker {
    // `None` until Jira has answered
    projects: Option<Vec<Project>>,
    // Project keys
    watched: Vec<String>,
    error: Option<SharedString>,
    loading: Task<()>,
    saving: Option<Task<()>>,
}

impl EventEmitter<ProjectsChosen> for ProjectPicker {}

impl ProjectPicker {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
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
        let load = spawn(cx, |state| async move { api::projects(&state).await });
        self.error = None;
        self.loading = cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(projects) => this.projects = Some(projects),
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
            api::watch_projects(&state, &watched).await
        });
        self.error = None;
        self.saving = Some(cx.spawn(async move |this, cx| {
            let result = save.await;
            this.update(cx, |this, cx| {
                this.saving = None;
                match result {
                    Ok(()) => cx.emit(ProjectsChosen),
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
                    Button::new("watch-projects")
                        .primary()
                        .label("Start sync")
                        .disabled(self.watched.is_empty())
                        .loading(self.saving.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                ),
            None if error.is_some() => form.children(error).child(
                Button::new("retry-projects")
                    .label("Try again")
                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
            ),
            None => form.child("Loading projects…"),
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
    use crate::sign_in::tests::{
        fill_in_and_submit, in_window, jira, launch, tokio_runtime, wait_until,
    };
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt;
    use serde_json::json;
    use wiremock::{
        Mock, ResponseTemplate,
        matchers::{method, path},
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
        let config = std::fs::read_to_string(data_dir.path().join("config.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        assert_eq!(config["watched_projects"], json!(["WEB"]));
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
