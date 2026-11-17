//! The only sync the user ever sees: the first download of the watched projects.

use crate::runtime::{Core, spawn};
use app_core::{SyncEvent, api};
use gpui_kit::component::{ActiveTheme, button::Button};
use gpui_kit::*;

/// Emitted once every issue of the watched projects is stored.
pub struct InitialSyncDone;

struct ProjectProgress {
    project_key: String,
    synced: u64,
    approximate_total: u64,
}

pub struct InitialSync {
    // In the order the projects are synced
    progress: Vec<ProjectProgress>,
    error: Option<SharedString>,
    // Whether the sync is for projects added after setup. Unknown until read.
    for_added_projects: Option<bool>,
    running: Task<()>,
    _sync: Subscription,
    _load: Task<()>,
}

impl EventEmitter<InitialSyncDone> for InitialSync {}

impl InitialSync {
    /// Starts the sync, or starts it again if an earlier run did not finish.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sync = cx.global::<Core>().sync.clone();
        let _sync = cx.subscribe(&sync, |this, _, event, cx| match event {
            SyncEvent::InitialSyncProgress {
                project_key,
                synced,
                approximate_total,
            } => {
                let new = ProjectProgress {
                    project_key: project_key.clone(),
                    synced: *synced,
                    approximate_total: *approximate_total,
                };
                match this
                    .progress
                    .iter_mut()
                    .find(|progress| progress.project_key == new.project_key)
                {
                    Some(progress) => *progress = new,
                    None => this.progress.push(new),
                }
                cx.notify();
            }
            SyncEvent::InitialSyncComplete => cx.emit(InitialSyncDone),
            _ => {}
        });
        // The first sync of an account is the only one that runs before `last_sync_at` is set
        let first_sync_done = spawn(cx, |state| async move {
            Ok(api::last_sync_at(&state).await.is_some())
        });
        let _load = cx.spawn(async move |this, cx| {
            let added = first_sync_done.await.unwrap_or(false);
            this.update(cx, |this, cx| {
                this.for_added_projects = Some(added);
                cx.notify();
            })
            .ok();
        });
        let mut this = Self {
            progress: Vec::new(),
            error: None,
            for_added_projects: None,
            running: Task::ready(()),
            _sync,
            _load,
        };
        this.run(cx);
        this
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let run = spawn(cx, |state| async move { api::initial_sync(&state).await });
        self.error = None;
        self.running = cx.spawn(async move |this, cx| {
            // Success arrives as `SyncEvent::InitialSyncComplete`
            if let Err(error) = run.await {
                this.update(cx, |this, cx| {
                    this.error = Some(format!("The sync stopped: {error}").into());
                    cx.notify();
                })
                .ok();
            }
        });
        cx.notify();
    }
}

/// 12340 → "12,340"
fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

impl Render for InitialSync {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.progress.iter().map(|progress| {
            let text = SharedString::from(format!(
                "{}: {} / ~{} issues",
                progress.project_key,
                grouped(progress.synced),
                grouped(progress.approximate_total),
            ));
            div()
                .id(SharedString::from(format!(
                    "sync-progress-{}",
                    progress.project_key
                )))
                .test_support()
                .aria_label(text.clone())
                .child(text)
        });
        let form = div()
            .w(px(360.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("initial-sync")
                    .test_support()
                    .text_xl()
                    .child("Syncing issues"),
            )
            .children(self.for_added_projects.map(|added| {
                let note = if added {
                    "Downloading the issues of the projects you added. Later syncs run in the background."
                } else {
                    "This happens once. Later syncs run in the background."
                };
                div()
                    .id("initial-sync-note")
                    .test_support()
                    .aria_label(note)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(note)
            }))
            .children(rows)
            .children(self.error.clone().map(|error| {
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .id("sync-error")
                            .role(Role::Alert)
                            .test_support()
                            .aria_label(error.clone())
                            .text_color(cx.theme().danger)
                            .child(error),
                    )
                    .child(
                        Button::new("retry-sync")
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                    )
            }));
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
    use super::grouped;
    use crate::harness::{in_window, launch, tokio_runtime, wait_until, watching_web};
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt;
    use serde_json::{Value, json};
    use std::path::Path;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, path},
    };

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(847), "847");
        assert_eq!(grouped(2340), "2,340");
        assert_eq!(grouped(1234567), "1,234,567");
    }

    fn issue(id: u32) -> Value {
        json!({
            "id": id.to_string(),
            "key": format!("WEB-{id}"),
            "fields": {
                "summary": format!("Issue number {id}"),
                "description": null,
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "assignee": null,
                "project": { "key": "WEB" },
                "issuetype": { "name": "Task" },
                "updated": "2026-03-29T01:30:00.000+0100",
            },
        })
    }

    /// WEB has 3 issues in two pages, and Jira estimates them as 2340.
    async fn mount_web(server: &MockServer, second_page_works: bool) {
        Mock::given(path("/rest/api/3/search/approximate-count"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "count": 2340 })))
            .mount(server)
            .await;
        // Mounted first, so it wins over the mock below for the second page
        Mock::given(path("/rest/api/3/search/jql"))
            .and(body_partial_json(json!({ "nextPageToken": "tok" })))
            .respond_with(if second_page_works {
                ResponseTemplate::new(200).set_body_json(json!({ "issues": [issue(3)] }))
            } else {
                ResponseTemplate::new(503)
            })
            .mount(server)
            .await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issues": [issue(1), issue(2)],
                "nextPageToken": "tok",
            })))
            .mount(server)
            .await;
    }

    fn is_home(cx: &mut TestAppContext, tokio: &tokio::runtime::Runtime, data_dir: &Path) -> bool {
        let window = launch(cx, tokio, data_dir);
        in_window(cx, window, |window, _| window.try_find("home").is_some())
    }

    #[gpui_kit::test]
    fn the_sync_shows_progress_and_then_opens_the_app(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(MockServer::start());
        tokio.block_on(mount_web(&server, true));
        let data_dir = watching_web(&server);

        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            window.try_find("initial-sync-note").is_some()
        });
        in_window(cx, window, |window, _| {
            assert!(window.try_find("initial-sync").is_some());
            assert_eq!(
                window.find("initial-sync-note").label(),
                Some("This happens once. Later syncs run in the background.")
            );
            assert!(window.try_find("home").is_none());
        });

        wait_until(cx, window, |window| window.try_find("home").is_some());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("initial-sync").is_none());
        });
        // The sync is not shown a second time
        assert!(is_home(cx, &tokio, data_dir.path()));
    }

    #[gpui_kit::test]
    fn an_interrupted_sync_keeps_its_progress_on_screen_and_a_restart_completes_it(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(MockServer::start());
        tokio.block_on(mount_web(&server, false));
        let data_dir = watching_web(&server);
        let window = launch(cx, &tokio, data_dir.path());

        wait_until(cx, window, |window| window.try_find("sync-error").is_some());
        in_window(cx, window, |window, _| {
            assert_eq!(
                window.find("sync-progress-WEB").label(),
                Some("WEB: 2 / ~2,340 issues")
            );
            assert!(window.try_find("home").is_none());
        });

        tokio.block_on(async {
            server.reset().await;
            mount_web(&server, true).await;
        });
        let window = launch(cx, &tokio, data_dir.path());

        wait_until(cx, window, |window| window.try_find("home").is_some());
        assert!(is_home(cx, &tokio, data_dir.path()));
    }

    #[gpui_kit::test]
    fn a_stopped_sync_can_be_started_again_without_a_restart(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let server = tokio.block_on(MockServer::start());
        tokio.block_on(mount_web(&server, false));
        let data_dir = watching_web(&server);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| window.try_find("sync-error").is_some());

        tokio.block_on(async {
            server.reset().await;
            mount_web(&server, true).await;
        });
        in_window(cx, window, |window, cx| window.click("retry-sync", cx));

        wait_until(cx, window, |window| window.try_find("home").is_some());
    }
}
