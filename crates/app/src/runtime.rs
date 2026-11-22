//! The bridge to the core. Core futures must never be awaited on a GPUI executor
//! directly: they need the tokio context. `spawn` is the only way from the UI into
//! the core.

use app_core::{AppState, SyncEvent};
use gpui_kit::*;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;

pub struct Core {
    pub state: Arc<AppState>,
    pub tokio: tokio::runtime::Handle,
    pub sync: Entity<SyncBridge>,
}

impl Global for Core {}

pub fn spawn<T, Fut>(cx: &App, f: impl FnOnce(Arc<AppState>) -> Fut) -> Task<anyhow::Result<T>>
where
    Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let core = cx.global::<Core>();
    let join = core.tokio.spawn(f(core.state.clone()));
    // A tokio JoinHandle can be awaited from any executor
    cx.background_spawn(async move { join.await? })
}

/// Re-emits core events inside GPUI. Views subscribe to it and reload from SQLite.
pub struct SyncBridge {
    _pump: Task<()>,
}

impl EventEmitter<SyncEvent> for SyncBridge {}

impl SyncBridge {
    pub fn new(mut events: UnboundedReceiver<SyncEvent>, cx: &mut Context<Self>) -> Self {
        let _pump = cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this.update(cx, |_, cx| cx.emit(event)).is_err() {
                    break;
                }
            }
        });
        Self { _pump }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Core, SyncBridge, spawn};
    use app_core::{SyncEvent, models::SetupStep};
    use gpui_kit::{AppContext, Context, Render, TestAppContext, Window, div};
    use std::path::Path;

    /// Starts a core on `data_dir` and installs it as the global, the way main()
    /// does. Returns the setup step that core is at.
    pub(crate) fn init_core(
        cx: &mut TestAppContext,
        tokio: &tokio::runtime::Runtime,
        data_dir: &Path,
    ) -> SetupStep {
        let (state, events) = tokio
            .block_on(app_core::start_without_keychain(data_dir))
            .unwrap();
        let step = tokio.block_on(app_core::api::setup_step(&state)).unwrap();
        let handle = tokio.handle().clone();
        cx.update(|cx| {
            let sync = cx.new(|cx| SyncBridge::new(events, cx));
            cx.set_global(Core {
                state,
                tokio: handle,
                sync,
            });
        });
        step
    }

    // The returned runtime and data dir must outlive the test body
    fn init_fresh_core(cx: &mut TestAppContext) -> (tokio::runtime::Runtime, tempfile::TempDir) {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        // Each test gets its own database: tests run in parallel, and two of them
        // migrating the same file would race
        let data_dir = tempfile::tempdir().unwrap();
        init_core(cx, &tokio, data_dir.path());
        (tokio, data_dir)
    }

    struct Probe {
        events: Vec<SyncEvent>,
    }

    impl Probe {
        fn new(cx: &mut Context<Self>) -> Self {
            let sync = cx.global::<Core>().sync.clone();
            cx.subscribe(&sync, |this: &mut Self, _, event, _| {
                this.events.push(event.clone())
            })
            .detach();
            Self { events: Vec::new() }
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui_kit::IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    async fn spawn_runs_a_future_on_tokio_and_returns_its_result(cx: &mut TestAppContext) {
        let _core = init_fresh_core(cx);
        // The tokio side is outside GPUI's deterministic scheduler
        cx.executor().allow_parking();

        let task = cx.update(|cx| {
            spawn(cx, |_state| async {
                // Panics unless polled inside a tokio runtime, as sqlx and reqwest do
                Ok(tokio::spawn(async { 7 }).await?)
            })
        });

        assert_eq!(task.await.unwrap(), 7);
    }

    #[gpui_kit::test]
    async fn a_core_event_reaches_a_view_subscribed_to_the_bridge(cx: &mut TestAppContext) {
        let _core = init_fresh_core(cx);
        let probe = cx.new(Probe::new);

        cx.update(|cx| cx.global::<Core>().state.emit(SyncEvent::Started));
        cx.run_until_parked();

        probe.read_with(cx, |probe, _| {
            assert!(matches!(probe.events[..], [SyncEvent::Started]));
        });
    }
}
