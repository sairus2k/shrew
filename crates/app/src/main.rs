use app_core::models::SetupStep;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use initial_sync::{InitialSync, InitialSyncDone};
use issue::IssueModel;
use issue_detail::{DetailClosed, DetailPane};
use issue_list::{IssueList, IssueOpened};
use projects::{ProjectPicker, ProjectsChosen};
use runtime::{Core, SyncBridge};
use sign_in::{SignIn, SignedIn};

mod initial_sync;
mod issue;
mod issue_detail;
mod issue_list;
mod projects;
mod runtime;
mod sign_in;

// Names the data directory: ~/Library/Application Support/<APP_ID>
const APP_ID: &str = "dev.shrew.app";

enum Screen {
    SignIn(Entity<SignIn>),
    ChooseProjects(Entity<ProjectPicker>),
    InitialSync(Entity<InitialSync>),
    Home {
        list: Entity<IssueList>,
        // The issue the user opened from the list
        detail: Option<Entity<DetailPane>>,
    },
}

struct Root {
    screen: Screen,
}

impl Root {
    fn new(step: SetupStep, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            screen: Self::screen(step, window, cx),
        }
    }

    fn show(&mut self, step: SetupStep, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = Self::screen(step, window, cx);
        cx.notify();
    }

    // Each setup screen reports when it is done, and the next one takes its place
    fn screen(step: SetupStep, window: &mut Window, cx: &mut Context<Self>) -> Screen {
        match step {
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
            SetupStep::Ready => {
                let list = cx.new(|cx| IssueList::new(window, cx));
                cx.subscribe(&list, Self::open_issue).detach();
                Screen::Home { list, detail: None }
            }
        }
    }

    fn open_issue(&mut self, _: Entity<IssueList>, opened: &IssueOpened, cx: &mut Context<Self>) {
        let Screen::Home { list, detail } = &mut self.screen else {
            return;
        };
        let issue = cx.new(|cx| IssueModel::new(opened.0.clone(), cx));
        let pane = cx.new(|cx| DetailPane::new(issue, cx));
        let list = list.clone();
        cx.subscribe(&pane, move |this, _, _: &DetailClosed, cx| {
            if let Screen::Home { detail, .. } = &mut this.screen {
                *detail = None;
            }
            list.update(cx, |list, cx| list.closed(cx));
            cx.notify();
        })
        .detach();
        *detail = Some(pane);
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.screen {
            Screen::SignIn(sign_in) => sign_in.clone().into_any_element(),
            Screen::ChooseProjects(picker) => picker.clone().into_any_element(),
            Screen::InitialSync(sync) => sync.clone().into_any_element(),
            Screen::Home { list, detail } => div()
                .id("home")
                .test_support()
                .size_full()
                .flex()
                .child(
                    div()
                        .h_full()
                        .min_w_0()
                        .map(|list| match detail {
                            Some(_) => list.w(relative(0.4)).flex_none(),
                            None => list.flex_1(),
                        })
                        .child(list.clone()),
                )
                .children(detail.clone().map(|detail| {
                    div()
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(detail)
                }))
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
