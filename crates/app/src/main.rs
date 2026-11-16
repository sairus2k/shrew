use app_core::models::SetupStep;
use gpui_kit::*;
use initial_sync::{InitialSync, InitialSyncDone};
use projects::{ProjectPicker, ProjectsChosen};
use runtime::{Core, SyncBridge};
use sign_in::{SignIn, SignedIn};

mod initial_sync;
mod projects;
mod runtime;
mod sign_in;

// Names the data directory: ~/Library/Application Support/<APP_ID>
const APP_ID: &str = "dev.shrew.app";

enum Screen {
    SignIn(Entity<SignIn>),
    ChooseProjects(Entity<ProjectPicker>),
    InitialSync(Entity<InitialSync>),
    Home,
}

struct Root {
    screen: Screen,
}

impl Root {
    fn new(step: SetupStep, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            screen: Screen::Home,
        };
        this.show(step, window, cx);
        this
    }

    // Each setup screen reports when it is done, and the next one takes its place
    fn show(&mut self, step: SetupStep, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = match step {
            SetupStep::SignIn => {
                let sign_in = cx.new(|cx| SignIn::new(window, cx));
                cx.subscribe_in(&sign_in, window, |this, _, _: &SignedIn, window, cx| {
                    this.show(SetupStep::ChooseProjects, window, cx)
                })
                .detach();
                Screen::SignIn(sign_in)
            }
            SetupStep::ChooseProjects => {
                let picker = cx.new(ProjectPicker::new);
                cx.subscribe_in(
                    &picker,
                    window,
                    |this, _, _: &ProjectsChosen, window, cx| {
                        this.show(SetupStep::InitialSync, window, cx)
                    },
                )
                .detach();
                Screen::ChooseProjects(picker)
            }
            SetupStep::InitialSync => {
                let sync = cx.new(InitialSync::new);
                cx.subscribe_in(&sync, window, |this, _, _: &InitialSyncDone, window, cx| {
                    this.show(SetupStep::Ready, window, cx)
                })
                .detach();
                Screen::InitialSync(sync)
            }
            SetupStep::Ready => Screen::Home,
        };
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match &self.screen {
            Screen::SignIn(sign_in) => sign_in.clone().into_any_element(),
            Screen::ChooseProjects(picker) => picker.clone().into_any_element(),
            Screen::InitialSync(sync) => sync.clone().into_any_element(),
            Screen::Home => div()
                .id("home")
                .test_support()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child("Shrew")
                .into_any_element(),
        }
    }
}

fn main() -> anyhow::Result<()> {
    // sqlx and reqwest need a tokio runtime. GPUI drives the UI on its own executor,
    // so tokio runs beside it on two worker threads.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let handle = tokio.handle().clone();

    let data_dir = dirs::data_dir().expect("no data dir").join(APP_ID);
    let (state, events) = tokio.block_on(app_core::start(&data_dir))?;
    let step = tokio.block_on(app_core::api::setup_step(&state));

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);

        let sync = cx.new(|cx| SyncBridge::new(events, cx));
        cx.set_global(Core {
            state,
            tokio: handle,
            sync,
        });

        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Root::new(step, window, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
    Ok(())
}
