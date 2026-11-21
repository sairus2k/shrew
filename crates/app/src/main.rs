use account::{Account, SignedOut};
use app_core::{
    SyncEvent,
    models::{IssueKey, SetupStep},
};
use gpui_kit::component::{
    ActiveTheme, Sizable, Theme, WindowExt as _,
    button::{Button, ButtonVariants},
    notification::Notification,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use initial_sync::{InitialSync, InitialSyncDone};
use issue::IssueModel;
use issue_detail::{DetailClosed, DetailPane};
use issue_list::{IssueList, IssueOpened};
use keys::{
    Back, ChangeAssignee, ChangeStatus, EditProjects, EditTimeTracking, LogTime, OpenInJira,
    OpenTimerIssue, ShowShortcuts, SyncNow, ToggleTimer, WriteComment,
};
use new_token::{NewToken, TokenRenewed};
use projects::{ProjectPicker, ProjectsCancelled, ProjectsChosen};
use runtime::{Core, SyncBridge};
use settings::{Settings, SettingsClosed};
use sign_in::{SignIn, SignedIn};
use sync_status::SyncStatus;
use timer::{ActiveTimer, TimerIssueClicked, TimerStatus, TimerWriteFailed};

mod account;
mod assignee;
mod comment;
mod duration;
#[cfg(test)]
mod harness;
mod initial_sync;
mod issue;
mod issue_detail;
mod issue_list;
mod keys;
mod menu;
mod new_token;
mod projects;
mod runtime;
mod settings;
mod sign_in;
mod sync_status;
mod theme;
mod timer;
mod transition;
mod worklog;

// Names the data directory: ~/Library/Application Support/<APP_ID>
const APP_ID: &str = "dev.shrew.app";

enum Screen {
    SignIn(Entity<SignIn>),
    ChooseProjects(Entity<ProjectPicker>),
    InitialSync(Entity<InitialSync>),
    Settings(Entity<Settings>),
    Home {
        list: Entity<IssueList>,
        // The issue the user opened from the list
        detail: Option<Entity<DetailPane>>,
        status: Entity<SyncStatus>,
        account: Entity<Account>,
        timer: Entity<ActiveTimer>,
        timer_status: Entity<TimerStatus>,
        // Shown from the moment Jira rejects the API token until it accepts a new one
        new_token: Option<Entity<NewToken>>,
    },
}

struct Root {
    screen: Screen,
    // Has the focus on a screen without the issue list while no control of
    // it does: Tab does not start from nowhere
    focus: FocusHandle,
    // The sync error the user was last told about. While Jira is down every
    // cycle fails the same way, and one notification is enough.
    notified_error: Option<String>,
    // Tempo keeps answering 401 in every cycle until the token is replaced,
    // and the user needs to be told once
    tempo_rejection_shown: bool,
}

impl Root {
    fn new(step: SetupStep, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sync = cx.global::<Core>().sync.clone();
        cx.subscribe_in(&sync, window, Self::on_sync_event).detach();
        cx.observe_window_appearance(window, |_, window, cx| {
            Theme::sync_system_appearance(Some(window), cx)
        })
        .detach();
        cx.global::<Core>()
            .state
            .set_window_focused(window.is_window_active());
        cx.observe_window_activation(window, |_, window, cx| {
            cx.global::<Core>()
                .state
                .set_window_focused(window.is_window_active())
        })
        .detach();
        let this = Self {
            screen: Self::screen(step, window, cx),
            focus: cx.focus_handle(),
            notified_error: None,
            tempo_rejection_shown: false,
        };
        this.rest_focus(window, cx);
        this
    }

    /// Gives the focus to where the keys are expected when no control has
    /// it: the issue list, which the shortcuts are bound to, or the screen.
    /// Called when the view that may have had the focus goes away. The focus
    /// does not become empty by itself then: GPUI Kit keeps the last focused
    /// input alive.
    fn rest_focus(&self, window: &mut Window, cx: &mut App) {
        match &self.screen {
            Screen::Home { list, .. } => list.focus_handle(cx).focus(window, cx),
            _ => self.focus.focus(window, cx),
        }
    }

    fn on_sync_event(
        &mut self,
        _: &Entity<SyncBridge>,
        event: &SyncEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SyncEvent::Completed => self.notified_error = None,
            SyncEvent::Error(error) if self.notified_error.as_ref() != Some(error) => {
                self.notified_error = Some(error.clone());
                window.push_notification(Notification::error(error.clone()), cx);
            }
            SyncEvent::ChangeRejected {
                key,
                action,
                reason,
            } => {
                let message = rejection_message(key, action, reason);
                window.push_notification(Notification::error(message), cx);
            }
            SyncEvent::TokenRejected => self.ask_for_token(window, cx),
            SyncEvent::TempoTokenRejected if !self.tempo_rejection_shown => {
                self.tempo_rejection_shown = true;
                window.push_notification(Notification::error(TEMPO_REJECTED), cx);
            }
            _ => {}
        }
    }

    fn ask_for_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Home { new_token, .. } = &mut self.screen else {
            return;
        };
        if new_token.is_some() {
            return;
        }
        let banner = cx.new(|cx| NewToken::new(window, cx));
        cx.subscribe_in(&banner, window, |this, _, _: &TokenRenewed, window, cx| {
            if let Screen::Home { new_token, .. } = &mut this.screen {
                *new_token = None;
            }
            this.rest_focus(window, cx);
            cx.notify();
        })
        .detach();
        *new_token = Some(banner);
        cx.notify();
    }

    fn show(&mut self, step: SetupStep, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = Self::screen(step, window, cx);
        self.rest_focus(window, cx);
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
            SetupStep::ChooseProjects => Screen::ChooseProjects(Self::picker(false, window, cx)),
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
                cx.subscribe_in(&list, window, Self::open_issue).detach();
                let account = cx.new(Account::new);
                cx.subscribe_in(&account, window, |this, _, _: &SignedOut, window, cx| {
                    // The next account starts with its own sync errors
                    this.notified_error = None;
                    this.show(SetupStep::SignIn, window, cx)
                })
                .detach();
                let timer = cx.new(ActiveTimer::new);
                cx.subscribe_in(
                    &timer,
                    window,
                    |_, _, failed: &TimerWriteFailed, window, cx| {
                        window.push_notification(Notification::error(failed.0.clone()), cx)
                    },
                )
                .detach();
                let timer_status = cx.new(|cx| TimerStatus::new(timer.clone(), cx));
                let opening = list.clone();
                cx.subscribe(
                    &timer_status,
                    move |_, _, clicked: &TimerIssueClicked, cx| {
                        opening.update(cx, |list, cx| list.open(clicked.0.clone(), cx));
                    },
                )
                .detach();
                Screen::Home {
                    list,
                    detail: None,
                    status: cx.new(SyncStatus::new),
                    account,
                    timer,
                    timer_status,
                    new_token: None,
                }
            }
        }
    }

    fn picker(editing: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<ProjectPicker> {
        let picker = cx.new(|cx| ProjectPicker::new(editing, cx));
        cx.subscribe_in(
            &picker,
            window,
            |this, _, chosen: &ProjectsChosen, window, cx| this.show(chosen.0, window, cx),
        )
        .detach();
        cx.subscribe_in(
            &picker,
            window,
            |this, _, _: &ProjectsCancelled, window, cx| this.show(SetupStep::Ready, window, cx),
        )
        .detach();
        picker
    }

    fn edit_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.screen, Screen::Home { .. }) {
            return;
        }
        let settings = cx.new(|cx| Settings::new(window, cx));
        cx.subscribe_in(
            &settings,
            window,
            |this, _, _: &SettingsClosed, window, cx| {
                this.tempo_rejection_shown = false;
                this.show(SetupStep::Ready, window, cx)
            },
        )
        .detach();
        self.screen = Screen::Settings(settings);
        self.rest_focus(window, cx);
        cx.notify();
    }

    fn edit_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.screen, Screen::Home { .. }) {
            self.screen = Screen::ChooseProjects(Self::picker(true, window, cx));
            self.rest_focus(window, cx);
            cx.notify();
        }
    }

    /// Escape leads back to the list from wherever the focus is, and from the
    /// list it closes the issue.
    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        let Screen::Home { list, detail, .. } = &mut self.screen else {
            return;
        };
        let focus = list.focus_handle(cx);
        if !focus.is_focused(window) {
            focus.focus(window, cx);
        } else if detail.take().is_some() {
            list.update(cx, |list, cx| list.closed(cx));
            cx.notify();
        }
    }

    /// Runs `act` on the open issue. Without one its shortcuts do nothing.
    fn in_detail(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        act: fn(&mut DetailPane, &mut Window, &mut Context<DetailPane>),
    ) {
        if let Screen::Home {
            detail: Some(detail),
            ..
        } = &self.screen
        {
            detail.update(cx, |detail, cx| act(detail, window, cx));
        }
    }

    fn toggle_timer(&mut self, _: &ToggleTimer, _: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Home {
            detail: Some(detail),
            timer,
            ..
        } = &self.screen
        {
            let key = detail.read(cx).key(cx);
            timer.update(cx, |timer, cx| timer.toggle(key, cx));
        }
    }

    fn open_timer_issue(&mut self, _: &OpenTimerIssue, _: &mut Window, cx: &mut Context<Self>) {
        let Screen::Home { list, timer, .. } = &self.screen else {
            return;
        };
        if let Some(key) = timer.read(cx).issue().cloned() {
            list.update(cx, |list, cx| list.open(key, cx));
        }
    }

    fn open_issue(
        &mut self,
        _: &Entity<IssueList>,
        opened: &IssueOpened,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Screen::Home {
            list,
            detail,
            timer,
            ..
        } = &mut self.screen
        else {
            return;
        };
        let issue = cx.new(|cx| IssueModel::new(opened.0.clone(), cx));
        let timer = timer.clone();
        let pane = cx.new(|cx| DetailPane::new(issue, timer, window, cx));
        let list = list.clone();
        cx.subscribe_in(
            &pane,
            window,
            move |this, _, _: &DetailClosed, window, cx| {
                if let Screen::Home { detail, .. } = &mut this.screen {
                    *detail = None;
                }
                list.update(cx, |list, cx| list.closed(cx));
                this.rest_focus(window, cx);
                cx.notify();
            },
        )
        .detach();
        *detail = Some(pane);
        // The issue shown until now may have had the focus in its comment
        self.rest_focus(window, cx);
        cx.notify();
    }
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = match &self.screen {
            Screen::SignIn(sign_in) => sign_in.clone().into_any_element(),
            Screen::ChooseProjects(picker) => picker.clone().into_any_element(),
            Screen::InitialSync(sync) => sync.clone().into_any_element(),
            Screen::Settings(settings) => settings.clone().into_any_element(),
            Screen::Home {
                list,
                detail,
                status,
                account,
                timer_status,
                new_token,
                ..
            } => div()
                .id("home")
                .test_support()
                .key_context(keys::HOME)
                .on_action(cx.listener(Self::back))
                .on_action(cx.listener(|this, _: &ChangeStatus, window, cx| {
                    this.in_detail(window, cx, DetailPane::change_status)
                }))
                .on_action(cx.listener(|this, _: &ChangeAssignee, window, cx| {
                    this.in_detail(window, cx, DetailPane::change_assignee)
                }))
                .on_action(cx.listener(|this, _: &WriteComment, window, cx| {
                    this.in_detail(window, cx, DetailPane::write_comment)
                }))
                .on_action(cx.listener(|this, _: &LogTime, window, cx| {
                    this.in_detail(window, cx, DetailPane::log_time)
                }))
                .on_action(cx.listener(|this, _: &OpenInJira, window, cx| {
                    this.in_detail(window, cx, DetailPane::open_in_jira)
                }))
                .on_action(cx.listener(Self::toggle_timer))
                .on_action(cx.listener(Self::open_timer_issue))
                .on_action(|_: &SyncNow, _, cx| app_core::api::sync_now(&cx.global::<Core>().state))
                .on_action(cx.listener(|this, _: &EditTimeTracking, window, cx| {
                    this.edit_settings(window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &EditProjects, window, cx| {
                        this.edit_projects(window, cx)
                    }),
                )
                .on_action(|_: &ShowShortcuts, window, cx| keys::show(window, cx))
                .size_full()
                .flex()
                .flex_col()
                .children(new_token.clone())
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .child(
                            div()
                                .h_full()
                                .min_w_0()
                                .map(|list| match detail {
                                    Some(_) => {
                                        list.w(relative(0.4)).flex_none().bg(cx.theme().sidebar)
                                    }
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
                        })),
                )
                .child(
                    div()
                        .h(px(28.))
                        .flex_none()
                        .px_4()
                        .flex()
                        .items_center()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .bg(cx.theme().status_bar)
                        .border_t_1()
                        .border_color(cx.theme().status_bar_border)
                        .child(div().flex_1().min_w_0().child(status.clone()))
                        .child(div().mr_2().child(timer_status.clone()))
                        .child(
                            Button::new("edit-settings")
                                .ghost()
                                .xsmall()
                                .label("Time tracking")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.edit_settings(window, cx)
                                })),
                        )
                        .child(
                            Button::new("edit-projects")
                                .ghost()
                                .xsmall()
                                .label("Projects")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.edit_projects(window, cx)
                                })),
                        )
                        .child(account.clone())
                        .child(
                            Button::new("show-shortcuts")
                                .ghost()
                                .xsmall()
                                .label("?")
                                .accessibility_label("Keyboard shortcuts")
                                .on_click(|_, window, cx| keys::show(window, cx)),
                        ),
                )
                .into_any_element(),
        };
        div().size_full().track_focus(&self.focus).child(screen)
    }
}

const TEMPO_REJECTED: &str = "Tempo rejected the API token, so logged time waits in the queue. \
     Save a new token under Time tracking, or switch the target to Jira";

fn rejection_message(key: &IssueKey, action: &str, reason: &str) -> String {
    format!("{key}: {action} rejected: {reason}")
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
        theme::init(cx);
        menu::init(cx);
        keys::init(cx);

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

#[cfg(test)]
mod tests {
    use super::{Core, rejection_message};
    use crate::harness::{launch, tokio_runtime};
    use gpui_kit::{AppContext, TestAppContext, VisualTestContext};

    #[gpui_kit::test]
    fn the_core_is_told_when_the_window_loses_and_gains_focus(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = tempfile::tempdir().unwrap();
        let window = launch(cx, &tokio, data_dir.path());
        let focused =
            |cx: &mut TestAppContext| cx.read(|cx| cx.global::<Core>().state.window_focused());
        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        assert!(focused(cx));

        VisualTestContext::from_window(window, cx).deactivate_window();
        assert!(!focused(cx));

        cx.update_window(window, |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        assert!(focused(cx));
    }

    #[test]
    fn a_rejection_names_the_issue_the_action_and_jiras_reason() {
        assert_eq!(
            rejection_message(&"WEB-1".into(), "status change", "Resolution is required"),
            "WEB-1: status change rejected: Resolution is required"
        );
    }
}
