//! What the UI integration tests share: the real views in a headless window on a
//! real core.

use crate::{Root, runtime::tests::init_core};
use gpui_kit::test::{TestQueryExt, TestWindowExt};
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, TestAppContext, Window, WindowBounds, WindowOptions,
    px, size,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{path, path_regex},
};

/// A Jira where no worklog changed: every sync cycle asks for them.
pub(crate) fn no_worklog_changes() -> Mock {
    Mock::given(wiremock::matchers::method("GET"))
        .and(path_regex("^/rest/api/3/worklog/(updated|deleted)$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "values": [], "until": 1_700_000_000_000_i64, "lastPage": true }),
        ))
}

pub(crate) fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

// What main() does: start the core on the data dir, then open the window
pub(crate) fn launch(
    cx: &mut TestAppContext,
    tokio: &tokio::runtime::Runtime,
    data_dir: &Path,
) -> AnyWindowHandle {
    cx.update(gpui_kit::init);
    cx.update(crate::theme::init);
    cx.update(crate::menu::init);
    let step = init_core(cx, tokio, data_dir);
    // The tokio side is outside GPUI's deterministic scheduler
    cx.executor().allow_parking();
    cx.update(|cx| {
        let bounds = Bounds::from_corners(Default::default(), size(px(800.), px(600.)).into());
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Root::new(step, window, cx))
        })
        .unwrap();
        window
    })
}

pub(crate) fn in_window<R>(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx)
    })
    .unwrap()
}

/// Waits until `reached` holds for the window, and panics after 5 s without it.
// The answer comes from a real HTTP request on the tokio runtime, so the
// wait is in real time, not on GPUI's test clock
#[track_caller]
pub(crate) fn wait_until(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    reached: impl FnMut(&mut Window) -> bool,
) {
    wait_at_most(Duration::from_secs(5), cx, window, reached);
}

// The panic is reported at the line of the test that waited
#[track_caller]
fn wait_at_most(
    timeout: Duration,
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    mut reached: impl FnMut(&mut Window) -> bool,
) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        if in_window(cx, window, |window, _| reached(window)) {
            return;
        }
        if started.elapsed() >= timeout {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let on_screen = in_window(cx, window, |window, _| {
        window
            .elements()
            .iter()
            .map(|element| match (element.path().last(), element.label()) {
                (Some(id), Some(label)) => format!("\n  {id} {label:?}"),
                (Some(id), None) => format!("\n  {id}"),
                (None, _) => String::new(),
            })
            .collect::<String>()
    });
    panic!("the window did not reach the expected state in {timeout:?}. On screen:{on_screen}");
}

/// A data dir of an app that is signed in to `server` and watches WEB, but
/// has not synced yet.
pub(crate) fn watching_web(server: &MockServer) -> tempfile::TempDir {
    let data_dir = tempfile::tempdir().unwrap();
    let config = json!({
        "site_url": server.uri(),
        "email": "ada@example.com",
        "account_id": "5b10a",
        "watched_projects": ["WEB"],
    });
    std::fs::write(data_dir.path().join("config.json"), config.to_string()).unwrap();
    std::fs::write(data_dir.path().join("api-token"), "secret-token").unwrap();
    data_dir
}

/// The app in a headless window with a mock Jira as its site.
pub(crate) struct TestApp {
    pub(crate) window: AnyWindowHandle,
    // Dropped in this order: the server shuts down while its runtime is alive,
    // and the database file outlives the core that has it open
    _jira: MockServer,
    _tokio: tokio::runtime::Runtime,
    _data_dir: tempfile::TempDir,
}

/// Launches the app on a fresh database against a mock Jira whose project WEB
/// has `issues`, each an element of a Jira search page. Returns once the
/// initial sync has stored them and the home screen is open.
pub(crate) fn launch_on_jira(cx: &mut TestAppContext, issues: &[Value]) -> TestApp {
    let tokio = tokio_runtime();
    let jira = tokio.block_on(async {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/search/approximate-count"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "count": issues.len() })),
            )
            .mount(&server)
            .await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "issues": issues })))
            .mount(&server)
            .await;
        no_worklog_changes().mount(&server).await;
        server
    });
    let data_dir = watching_web(&jira);
    let window = launch(cx, &tokio, data_dir.path());
    wait_until(cx, window, |window| window.try_find("home").is_some());
    TestApp {
        window,
        _jira: jira,
        _tokio: tokio,
        _data_dir: data_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_list::tests::{issue, rows};

    #[gpui_kit::test]
    fn issues_of_the_mock_jira_are_listed_and_found_by_search(cx: &mut TestAppContext) {
        let app = launch_on_jira(cx, &[issue(1, "Fix login"), issue(2, "Broken checkout")]);
        wait_until(cx, app.window, |window| {
            rows(window)
                == [
                    "WEB-2: Broken checkout, To Do, Unassigned",
                    "WEB-1: Fix login, To Do, Unassigned",
                ]
        });

        in_window(cx, app.window, |window, cx| {
            window.click("search", cx);
            window.input("login", cx);
        });

        wait_until(cx, app.window, |window| {
            rows(window) == ["WEB-1: Fix login, To Do, Unassigned"]
        });
    }

    #[gpui_kit::test]
    #[should_panic(
        expected = "the window did not reach the expected state in 50ms. On screen:\n  site-url\n  input-state \"Site URL\""
    )]
    fn a_wait_that_times_out_says_how_long_it_waited_and_what_is_on_screen(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());

        wait_at_most(Duration::from_millis(50), cx, window, |window| {
            window.try_find("home").is_some()
        });
    }
}
