//! The main screen: the cached issues and the search over them.

use crate::keys::{
    self, FilterByAssignee, FilterByProject, FilterBySprint, FilterByStatus, FocusSearch,
    OpenSelected, SelectNext, SelectPrevious,
};
use crate::runtime::{Core, spawn};
use crate::theme::{status_dot, tabular_digits};
use app_core::{
    SyncEvent, api,
    models::{
        AssigneeFilter, FilterOptions, IssueFilter, IssueKey, IssueRow, SprintFilter,
        StatusCategory, StatusFilter, SyncStatus,
    },
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::ops::Range;

/// Emitted when the user picks an issue to read.
pub struct IssueOpened(pub IssueKey);

/// Whose issues the list shows, under the name the filter has for them.
#[derive(Clone)]
struct Assignee {
    label: String,
    filter: AssigneeFilter,
}

// The cache can hold an account under a name it had before
impl PartialEq for Assignee {
    fn eq(&self, other: &Self) -> bool {
        self.filter == other.filter
    }
}

/// The sprint whose issues the list shows, under the name the filter has for it.
#[derive(Clone)]
struct InSprint {
    label: String,
    filter: SprintFilter,
}

// A sprint that has started is the same sprint under another label
impl PartialEq for InSprint {
    fn eq(&self, other: &Self) -> bool {
        self.filter == other.filter
    }
}

/// What an item of a filter menu does. `None` takes the filter off.
#[derive(Clone)]
enum Pick {
    Project(Option<String>),
    Assignee(Option<Assignee>),
    /// Shows only the issues in the status.
    Status(Option<String>),
    /// Puts the status into the excluded ones, or takes it out.
    Exclude(String),
    Sprint(Option<InSprint>),
    /// Takes every filter off. The search text is not one of them.
    Clear,
}

#[derive(Clone)]
struct Choice {
    label: String,
    pick: Pick,
    chosen: bool,
}

/// One filter as its button and its menu show it.
struct Filter {
    id: &'static str,
    clear_id: &'static str,
    name: &'static str,
    /// What the button says while nothing is chosen.
    unset: &'static str,
    value: Option<String>,
    /// The first one takes the filter off.
    choices: Vec<Choice>,
    /// The values that can be left out, each a toggle. Empty for a filter
    /// that cannot leave values out.
    exclude: Vec<Choice>,
    /// Of the button, for the shortcut that opens its menu.
    focus: FocusHandle,
    /// The action of that shortcut.
    key: Box<dyn Action>,
}

/// The menu of a filter: the choice that takes it off, then `values`.
fn choices<T: Clone + PartialEq>(
    any: &str,
    mut values: Vec<T>,
    chosen: Option<&T>,
    label: impl Fn(&T) -> String,
    pick: impl Fn(Option<T>) -> Pick,
) -> Vec<Choice> {
    // A sync can take the last issue with the chosen value out of the cache.
    // The value stays in the menu, so that it is seen to be what is chosen
    if let Some(chosen) = chosen.filter(|chosen| !values.contains(chosen)) {
        values.push(chosen.clone());
    }
    let any = Choice {
        label: any.to_owned(),
        pick: pick(None),
        chosen: chosen.is_none(),
    };
    let values = values.into_iter().map(|value| Choice {
        label: label(&value),
        chosen: chosen == Some(&value),
        pick: pick(Some(value)),
    });
    std::iter::once(any).chain(values).collect()
}

fn menu_items(
    mut menu: PopupMenu,
    choices: &[Choice],
    exclude: &[Choice],
    view: &Entity<IssueList>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    // A scrollable menu draws a scrollbar, so only one that has more items
    // than the window has room for under the row of the search is scrollable
    let room = window.viewport_size().height - px(96.);
    let item = rems(1.75).to_pixels(window.rem_size());
    let items = choices.len() + if exclude.is_empty() { 0 } else { 2 };
    if item * (items + 1) as f32 > room {
        menu = menu.scrollable(true).max_h(room);
    }
    for choice in choices {
        let pick = choice.pick.clone();
        menu = menu.item(
            PopupMenuItem::new(choice.label.clone())
                .checked(choice.chosen)
                .on_click(window.listener_for(view, move |this, _, window, cx| {
                    this.pick(pick.clone(), cx);
                    // A menu gives the focus back only when it had it, so a pick
                    // in a submenu leaves it on the closed submenu, where the
                    // keys of the list do nothing
                    this.focus.focus(window, cx);
                })),
        );
    }
    if exclude.is_empty() {
        return menu;
    }
    // A menu closes on a click, so a submenu of toggles costs a trip to the
    // menu for every value, where one item per value in this menu would
    // double its length
    let (exclude, view) = (exclude.to_vec(), view.clone());
    menu.separator()
        .submenu("Exclude", window, cx, move |menu, window, cx| {
            menu_items(menu, &exclude, &[], &view, window, cx)
        })
}

/// The frame of a filter that is set: the accent, as on the open issue's row.
fn tinted(cx: &App) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(cx.theme().primary)
        .bg(cx.theme().list_active)
}

pub struct IssueList {
    query: Entity<InputState>,
    // The watched projects; the filter is offered when there are several
    projects: Vec<String>,
    // What the cached issues can be filtered by
    options: FilterOptions,
    // The account id of the signed-in user
    me: Option<String>,
    // The filters; `None` does not narrow the list
    project: Option<String>,
    assignee: Option<Assignee>,
    status: Option<StatusFilter>,
    sprint: Option<InSprint>,
    issues: Vec<IssueRow>,
    // The issue shown beside the list
    opened: Option<IssueKey>,
    // The issue the keyboard is on
    selected: Option<IssueKey>,
    // Rests here while the user types nowhere, which is when the shortcuts apply
    focus: FocusHandle,
    // Of the project filter, and of the button all the filters collapse into
    filter_focus: FocusHandle,
    assignee_focus: FocusHandle,
    status_focus: FocusHandle,
    sprint_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    loading: Task<()>,
}

impl EventEmitter<IssueOpened> for IssueList {}

impl Focusable for IssueList {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl IssueList {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search issues"));
        cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.reload(cx),
                InputEvent::PressEnter { .. } => {
                    this.open_selected(cx);
                    this.focus.focus(window, cx);
                }
                _ => {}
            },
        )
        .detach();
        let sync = cx.global::<Core>().sync.clone();
        cx.subscribe(&sync, |this, _, event: &SyncEvent, cx| {
            if let SyncEvent::IssuesUpdated(_) = event {
                this.reload(cx);
                this.load_options(cx);
            }
        })
        .detach();

        let mut this = Self {
            query,
            projects: Vec::new(),
            options: FilterOptions::default(),
            me: None,
            project: None,
            assignee: None,
            status: None,
            sprint: None,
            issues: Vec::new(),
            opened: None,
            selected: None,
            focus: cx.focus_handle(),
            filter_focus: cx.focus_handle(),
            assignee_focus: cx.focus_handle(),
            status_focus: cx.focus_handle(),
            sprint_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            loading: Task::ready(()),
        };
        this.reload(cx);
        this.load_projects(cx);
        this.load_options(cx);
        this
    }

    fn load_options(&mut self, cx: &mut Context<Self>) {
        let load = spawn(cx, |state| async move {
            let options = api::filter_options(&state).await?;
            let me = api::account(&state).await.map(|account| account.account_id);
            anyhow::Ok((options, me))
        });
        cx.spawn(async move |this, cx| {
            if let Ok((options, me)) = load.await {
                this.update(cx, |this, cx| {
                    this.options = options;
                    this.me = me;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn load_projects(&mut self, cx: &mut Context<Self>) {
        let load = spawn(
            cx,
            |state| async move { api::watched_projects(&state).await },
        );
        cx.spawn(async move |this, cx| {
            let projects = load.await;
            this.update(cx, |this, cx| {
                this.projects = projects.unwrap_or_default();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        match pick {
            Pick::Project(project) => self.project = project,
            Pick::Assignee(assignee) => self.assignee = assignee,
            Pick::Status(status) => self.status = status.map(StatusFilter::Is),
            Pick::Exclude(status) => {
                let mut excluded = match self.status.take() {
                    Some(StatusFilter::Not(excluded)) => excluded,
                    _ => Vec::new(),
                };
                match excluded.iter().position(|name| *name == status) {
                    Some(at) => drop(excluded.remove(at)),
                    None => excluded.push(status),
                }
                self.status = (!excluded.is_empty()).then_some(StatusFilter::Not(excluded));
            }
            Pick::Sprint(sprint) => self.sprint = sprint,
            Pick::Clear => {
                self.project = None;
                self.assignee = None;
                self.status = None;
                self.sprint = None;
            }
        }
        self.reload(cx);
        cx.notify();
    }

    /// The filters the user can set, in the order of their buttons.
    fn filters(&self) -> Vec<Filter> {
        let mut filters = Vec::new();
        if self.projects.len() > 1 {
            filters.push(Filter {
                id: "project-filter",
                clear_id: "project-filter-clear",
                name: "Project",
                unset: "All projects",
                value: self.project.clone(),
                choices: choices(
                    "All projects",
                    self.projects.clone(),
                    self.project.as_ref(),
                    String::clone,
                    Pick::Project,
                ),
                exclude: Vec::new(),
                focus: self.filter_focus.clone(),
                key: Box::new(FilterByProject),
            });
        }

        // The signed-in user comes first and not again among the others
        let me = self.me.iter().map(|me| Assignee {
            label: "Me".to_owned(),
            filter: AssigneeFilter::User(me.clone()),
        });
        let nobody = Assignee {
            label: "Unassigned".to_owned(),
            filter: AssigneeFilter::Unassigned,
        };
        let others = self.options.assignees.iter();
        let others = others
            .filter(|user| Some(&user.account_id) != self.me.as_ref())
            .map(|user| Assignee {
                label: user.display_name.clone(),
                filter: AssigneeFilter::User(user.account_id.clone()),
            });
        filters.push(Filter {
            id: "assignee-filter",
            clear_id: "assignee-filter-clear",
            name: "Assignee",
            unset: "Assignee",
            value: self.assignee.as_ref().map(|chosen| chosen.label.clone()),
            choices: choices(
                "Any assignee",
                me.chain([nobody]).chain(others).collect(),
                self.assignee.as_ref(),
                |assignee| assignee.label.clone(),
                Pick::Assignee,
            ),
            exclude: Vec::new(),
            focus: self.assignee_focus.clone(),
            key: Box::new(FilterByAssignee),
        });
        filters.push(self.status_filter());
        filters.extend(self.sprint_filter());
        filters
    }

    /// The status filter: one status to show, or the statuses to leave out.
    fn status_filter(&self) -> Filter {
        let statuses = &self.options.statuses;
        let (shown, excluded) = match &self.status {
            None => (None, &[][..]),
            Some(StatusFilter::Is(status)) => (Some(status), &[][..]),
            Some(StatusFilter::Not(excluded)) => (None, &excluded[..]),
        };
        let mut choices = choices(
            "Any status",
            statuses.clone(),
            shown,
            String::clone,
            Pick::Status,
        );
        choices[0].chosen = self.status.is_none();
        // As in `choices`: an excluded status no cached issue has any more
        // stays in the menu, so that it can be taken off
        let left_cache = excluded.iter().filter(|name| !statuses.contains(name));
        let exclude = statuses.iter().chain(left_cache).map(|name| Choice {
            label: name.clone(),
            pick: Pick::Exclude(name.clone()),
            chosen: excluded.contains(name),
        });
        // Two names fit beside the search; the menu shows the rest
        let value = self.status.as_ref().map(|status| match status {
            StatusFilter::Is(status) => status.clone(),
            StatusFilter::Not(excluded) if excluded.len() > 2 => {
                format!("not {} +{}", excluded[..2].join(", "), excluded.len() - 2)
            }
            StatusFilter::Not(excluded) => format!("not {}", excluded.join(", ")),
        });
        Filter {
            id: "status-filter",
            clear_id: "status-filter-clear",
            name: "Status",
            unset: "Status",
            value,
            choices,
            exclude: exclude.collect(),
            focus: self.status_focus.clone(),
            key: Box::new(FilterByStatus),
        }
    }

    /// The sprint filter: the running sprints, the planned ones and the
    /// backlog. `None` when the issues of the chosen project, or of all of
    /// them, are in no sprint: such a site or project does not work in sprints.
    fn sprint_filter(&self) -> Option<Filter> {
        let project = self.project.as_ref();
        let sprints = self.options.sprints.iter();
        let mut values: Vec<InSprint> = sprints
            .filter(|sprint| project.is_none_or(|project| sprint.project_keys.contains(project)))
            .map(|sprint| InSprint {
                label: match sprint.active {
                    true => sprint.name.clone(),
                    false => format!("{} (future)", sprint.name),
                },
                filter: SprintFilter::Sprint(sprint.id),
            })
            .collect();
        if values.is_empty() && self.sprint.is_none() {
            return None;
        }
        values.push(InSprint {
            label: "Backlog".to_owned(),
            filter: SprintFilter::Backlog,
        });
        Some(Filter {
            id: "sprint-filter",
            clear_id: "sprint-filter-clear",
            name: "Sprint",
            unset: "Sprint",
            value: self.sprint.as_ref().map(|chosen| chosen.label.clone()),
            choices: choices(
                "Any sprint",
                values,
                self.sprint.as_ref(),
                |sprint| sprint.label.clone(),
                Pick::Sprint,
            ),
            exclude: Vec::new(),
            focus: self.sprint_focus.clone(),
            key: Box::new(FilterBySprint),
        })
    }

    /// The text the list is searched for. One character finds too much to be a search.
    fn searched(&self, cx: &App) -> Option<String> {
        let query = self.query.read(cx).value().trim().to_owned();
        (query.chars().count() >= 2).then_some(query)
    }

    /// Moves the selection `by` rows. With nothing selected it starts at the
    /// top. Beside an open issue the pane follows the selection; with none
    /// open the keys only move the mark, so that the list can be read at its
    /// full width.
    fn select(&mut self, by: isize, cx: &mut Context<Self>) {
        let selected = self.selected.as_ref();
        let at = selected.and_then(|key| self.issues.iter().position(|issue| &issue.key == key));
        let last = self.issues.len().saturating_sub(1);
        let to = at.map_or(0, |at| at.saturating_add_signed(by).min(last));
        let Some(issue) = self.issues.get(to) else {
            return;
        };
        let key = issue.key.clone();
        self.scroll.scroll_to_item(to, ScrollStrategy::Nearest);
        // Opening builds the pane anew, which would empty the inputs of the
        // issue that is already open when a key hits the end of the list
        if self.opened.as_ref().is_some_and(|opened| *opened != key) {
            self.open(key, cx);
        } else {
            self.selected = Some(key);
            cx.notify();
        }
    }

    /// Opens the selected issue, or the first one when none on screen is selected.
    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected.as_ref();
        let issue = self
            .issues
            .iter()
            .find(|issue| Some(&issue.key) == selected)
            .or(self.issues.first());
        if let Some(issue) = issue {
            self.open(issue.key.clone(), cx);
        }
    }

    pub fn open(&mut self, key: IssueKey, cx: &mut Context<Self>) {
        self.selected = Some(key.clone());
        self.opened = Some(key.clone());
        cx.emit(IssueOpened(key));
        cx.notify();
    }

    /// The issue beside the list was closed.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        self.opened = None;
        cx.notify();
    }

    // No debounce: an FTS5 query takes a few milliseconds and there is no IPC hop
    fn reload(&mut self, cx: &mut Context<Self>) {
        let query = self.searched(cx);
        let filter = IssueFilter {
            project: self.project.clone(),
            assignee: self.assignee.as_ref().map(|chosen| chosen.filter.clone()),
            status: self.status.clone(),
            sprint: self.sprint.as_ref().map(|chosen| chosen.filter.clone()),
        };
        let load = spawn(cx, move |state| async move {
            match query {
                None => api::list_issues(&state, &filter).await,
                Some(query) => api::search_issues(&state, &query, &filter).await,
            }
        });
        self.show(load, cx);
    }

    fn show(&mut self, load: Task<anyhow::Result<Vec<IssueRow>>>, cx: &mut Context<Self>) {
        // Assigning the task drops the previous one, so a slow older query never
        // replaces newer rows. The old rows stay on screen until the new ones arrive.
        self.loading = cx.spawn(async move |this, cx| {
            if let Ok(issues) = load.await {
                this.update(cx, |this, cx| {
                    this.issues = issues;
                    cx.notify();
                })
                .ok();
            }
        });
    }

    fn rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.issues[range]
            .iter()
            .map(|issue| {
                row(issue, self.opened.as_ref(), self.selected.as_ref(), cx).into_any_element()
            })
            .collect()
    }
}

fn row(
    issue: &IssueRow,
    opened: Option<&IssueKey>,
    selected: Option<&IssueKey>,
    cx: &mut Context<IssueList>,
) -> impl IntoElement + use<> {
    let selected = selected == Some(&issue.key);
    let muted = cx.theme().muted_foreground;
    let hover = cx.theme().list_hover;
    let done = issue.status_category == StatusCategory::Done;
    let key = issue.key.clone();
    let assignee = issue.assignee_name.as_deref().unwrap_or("Unassigned");
    // The status is the user's own change and Jira does not have it yet
    let pending = issue.sync_status == SyncStatus::Pending;
    let mut label = format!(
        "{}: {}, {}{}, {assignee}",
        issue.key,
        issue.summary,
        issue.status,
        if pending { " (pending)" } else { "" }
    );
    if let Some(priority) = &issue.priority {
        label.push_str(&format!(", {priority} priority"));
    }
    div()
        .id(SharedString::from(format!("issue-{}", issue.key)))
        .role(Role::ListItem)
        .test_support()
        .aria_label(label)
        .aria_selected(selected)
        // uniform_list gives every row the height of the first one
        .h(px(32.))
        .w_full()
        .px_4()
        // Every row has the border, so that the selected one does not shift
        .border_l_2()
        .border_color(if selected {
            cx.theme().ring
        } else {
            transparent_black()
        })
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .map(|row| match opened == Some(&issue.key) {
            true => row.bg(cx.theme().list_active),
            false => row.hover(|row| row.bg(hover)),
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            // The keys follow the mouse to the list
            this.focus.focus(window, cx);
            this.open(key.clone(), cx)
        }))
        .child(status_dot(issue.status_category, cx))
        .child(
            div()
                .w(px(KEY_WIDTH))
                .flex_none()
                .truncate()
                .font_features(tabular_digits())
                .text_color(muted)
                .child(issue.key.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(done, |summary| summary.text_color(muted))
                .child(issue.summary.clone()),
        )
        // Beside an open issue the list is too narrow for these three, and the
        // issue itself shows them
        .when(opened.is_none(), |row| {
            row.child(
                div()
                    .w(px(PRIORITY_WIDTH))
                    .flex_none()
                    .when_some(issue.priority.as_deref(), |cell, name| {
                        cell.child(priority(&issue.key, name, done, cx))
                    }),
            )
            .child(
                div()
                    .w(px(STATUS_WIDTH))
                    .flex_none()
                    .truncate()
                    .text_color(if done { muted } else { cx.theme().foreground })
                    .child(if pending {
                        format!("↑ {}", issue.status)
                    } else {
                        issue.status.clone()
                    }),
            )
            .child(
                div()
                    .w(px(ASSIGNEE_WIDTH))
                    .flex_none()
                    .truncate()
                    .text_color(muted)
                    .child(assignee.to_owned()),
            )
        })
}

const KEY_WIDTH: f32 = 96.;
const PRIORITY_WIDTH: f32 = 96.;
const STATUS_WIDTH: f32 = 120.;
const ASSIGNEE_WIDTH: f32 = 140.;
const MARK_SIZE: f32 = 14.;

/// The priority of an issue: a mark and the name. The five priorities Jira
/// gives a site have marks of their own, so that they are told apart before
/// they are read. A priority of another scheme has no mark, as its rank among
/// the others is not known.
fn priority(key: &IssueKey, name: &str, done: bool, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let mark = match name {
        "Highest" => Some((IconName::ChevronsUp, theme.red)),
        "High" => Some((IconName::ChevronUp, theme.red)),
        "Medium" => Some((IconName::Equal, theme.yellow)),
        "Low" => Some((IconName::ChevronDown, theme.blue)),
        "Lowest" => Some((IconName::ChevronsDown, theme.blue)),
        _ => None,
    };
    div()
        .id(SharedString::from(format!("priority-{key}")))
        .test_support()
        .aria_label(name.to_owned())
        .flex()
        .items_center()
        .gap_1()
        .text_color(theme.muted_foreground)
        // The slot stays without a mark, so that every name starts in one place
        .child(
            div()
                .size(px(MARK_SIZE))
                .flex_none()
                .children(mark.map(|(icon, color)| {
                    // A done issue no longer asks for attention
                    let color = if done { theme.muted_foreground } else { color };
                    Icon::new(icon).with_size(px(MARK_SIZE)).text_color(color)
                })),
        )
        .child(div().min_w_0().truncate().child(name.to_owned()))
}

/// The names of the columns, over a list wide enough to show them all.
fn column_names(cx: &App) -> impl IntoElement {
    div()
        .h(px(24.))
        .flex_none()
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .text_size(px(11.))
        .text_color(cx.theme().muted_foreground)
        // The dot of the status stands before the key
        .child(div().w(px(8.)).flex_none())
        .child(div().w(px(KEY_WIDTH)).flex_none().child("KEY"))
        .child(div().flex_1().min_w_0().child("SUMMARY"))
        .child(
            div()
                .id("priority-column")
                .test_support()
                .w(px(PRIORITY_WIDTH))
                .flex_none()
                .child("PRIORITY"),
        )
        .child(div().w(px(STATUS_WIDTH)).flex_none().child("STATUS"))
        .child(div().w(px(ASSIGNEE_WIDTH)).flex_none().child("ASSIGNEE"))
}

impl IssueList {
    /// A filter in the row of the search. Set, it shows its value and a cross
    /// that takes it off.
    fn filter_button(&self, filter: Filter, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let Filter {
            id,
            clear_id,
            name,
            unset,
            value,
            choices,
            exclude,
            focus,
            key,
        } = filter;
        let label = match &value {
            Some(value) => format!("{name}: {value}"),
            None => unset.to_owned(),
        };
        let button = Button::new(id)
            .small()
            .label(label)
            .track_focus(&focus)
            .children(keys::hint(key.as_ref(), cx));
        let clear = choices[0].pick.clone();
        let menu = move |menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
            menu_items(menu, &choices, &exclude, &view, window, cx)
        };
        if value.is_none() {
            return button
                .outline()
                .dropdown_caret(true)
                .dropdown_menu(menu)
                .into_any_element();
        }
        tinted(cx)
            .child(button.ghost().dropdown_menu(menu))
            .child(
                Button::new(clear_id)
                    .ghost()
                    .xsmall()
                    .label("×")
                    .accessibility_label(format!("Clear the filter {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(clear.clone(), cx))),
            )
            .into_any_element()
    }

    /// One button for a list too narrow for the buttons of the filters. Its
    /// menu has a submenu for each filter.
    fn filters_button(&self, filters: Vec<Filter>, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let set = filters
            .iter()
            .filter(|filter| filter.value.is_some())
            .count();
        let label = match set {
            0 => "Filters".to_owned(),
            set => format!("Filters · {set}"),
        };
        let button = Button::new("filters")
            .track_focus(&self.filter_focus)
            .small()
            .label(label)
            .dropdown_caret(true)
            // The key of every filter opens this menu; one of them fits
            .children(keys::hint(filters[0].key.as_ref(), cx));
        let menu = move |mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
            // One menu with the values of every filter would not fit the window
            for filter in &filters {
                let label = match &filter.value {
                    Some(value) => format!("{}: {value}", filter.name),
                    None => filter.name.to_owned(),
                };
                let (choices, view) = (filter.choices.clone(), view.clone());
                let exclude = filter.exclude.clone();
                menu = menu.submenu(label, window, cx, move |menu, window, cx| {
                    menu_items(menu, &choices, &exclude, &view, window, cx)
                });
            }
            menu.separator().item(
                PopupMenuItem::new("Clear filters")
                    .disabled(set == 0)
                    .on_click(
                        window.listener_for(&view, |this, _, _, cx| this.pick(Pick::Clear, cx)),
                    ),
            )
        };
        if set == 0 {
            return button.outline().dropdown_menu(menu).into_any_element();
        }
        tinted(cx)
            .child(button.ghost().dropdown_menu(menu))
            .into_any_element()
    }

    /// How many issues the list holds, and the way back to all of them.
    fn count(&self, filtered: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let found = self.issues.len();
        // A search returns the best matches only, so there can be more
        let more = self.searched(cx).is_some() && found == api::SEARCH_LIMIT;
        let text = format!(
            "{found}{} {}",
            if more { "+" } else { "" },
            if found == 1 { "issue" } else { "issues" }
        );
        div()
            .flex_none()
            .h(px(24.))
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .id("issue-count")
                    .test_support()
                    .aria_label(text.clone())
                    .font_features(tabular_digits())
                    .child(text),
            )
            .when(filtered, |line| {
                line.child(
                    Button::new("clear-filters")
                        .link()
                        .xsmall()
                        .label("Clear filters")
                        .on_click(cx.listener(|this, _, _, cx| this.pick(Pick::Clear, cx))),
                )
            })
    }
}

impl Render for IssueList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filters = self.filters();
        let filtered = filters.iter().any(|filter| filter.value.is_some());
        // Beside an open issue the list is too narrow for the buttons of the
        // filters, for the count and for the names of the columns
        let narrow = self.opened.is_some();
        // `p` opens the menu the project is chosen in: its own, or the one of
        // all the filters
        let choosable = self.projects.len() > 1;
        let in_sprints = filters.iter().any(|filter| filter.id == "sprint-filter");
        let bar = div().p_3().flex().items_center().gap_2().child(
            div().flex_1().min_w_0().child(
                Input::new(&self.query)
                    .id("search")
                    .aria_label("Search issues")
                    .when_some(keys::hint(&FocusSearch, cx), Input::suffix),
            ),
        );
        let bar = if narrow {
            bar.child(self.filters_button(filters, cx))
        } else {
            let buttons: Vec<_> = filters
                .into_iter()
                .map(|filter| self.filter_button(filter, cx))
                .collect();
            bar.children(buttons)
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .children((!narrow).then(|| self.count(filtered, cx)))
            .children((!narrow).then(|| column_names(cx)))
            // Without this an empty list under a filter looks like an empty cache
            .children((filtered && self.issues.is_empty()).then(|| {
                div()
                    .id("no-matches")
                    .test_support()
                    .aria_label("No issues match the filters")
                    .flex_none()
                    .p_4()
                    .text_color(cx.theme().muted_foreground)
                    .child("No issues match the filters")
            }))
            .child(
                // Only the rows in view are built, so the cost of a frame does
                // not grow with the number of issues
                uniform_list(
                    "issues",
                    self.issues.len(),
                    cx.processor(|this, range, _, cx| this.rows(range, cx)),
                )
                .track_scroll(&self.scroll)
                .track_focus(&self.focus)
                .key_context(keys::LIST)
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select(1, cx)))
                .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.select(-1, cx)))
                .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(cx)))
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                    this.query.update(cx, |query, cx| query.focus(window, cx))
                }))
                .on_action(cx.listener(move |this, _: &FilterByProject, window, cx| {
                    if choosable {
                        keys::open_menu(&this.filter_focus, window, cx)
                    }
                }))
                .on_action(cx.listener(move |this, _: &FilterByAssignee, window, cx| {
                    let button = if narrow {
                        &this.filter_focus
                    } else {
                        &this.assignee_focus
                    };
                    keys::open_menu(button, window, cx)
                }))
                .on_action(cx.listener(move |this, _: &FilterByStatus, window, cx| {
                    let button = if narrow {
                        &this.filter_focus
                    } else {
                        &this.status_focus
                    };
                    keys::open_menu(button, window, cx)
                }))
                .on_action(cx.listener(move |this, _: &FilterBySprint, window, cx| {
                    let button = if narrow {
                        &this.filter_focus
                    } else {
                        &this.sprint_focus
                    };
                    if in_sprints {
                        keys::open_menu(button, window, cx)
                    }
                }))
                .flex_1()
                .min_h_0(),
            )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::IssueList;
    use crate::harness::{in_window, launch, tokio_runtime, wait_until};
    use crate::runtime::{Core, tests::init_core};
    use app_core::{
        SyncEvent,
        models::{IssueRow, StatusCategory, SyncStatus},
    };
    use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
    use gpui_kit::{
        AnyWindowHandle, AppContext, Entity, MouseButton, Role, TestAppContext, Window,
        WindowOptions, px,
    };
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};
    use tokio::{runtime::Runtime, sync::oneshot};

    /// An unassigned WEB issue in To Do. A higher `id` was changed later.
    pub(crate) fn issue(id: u32, summary: &str) -> Value {
        json!({
            "id": id.to_string(),
            "key": format!("WEB-{id}"),
            "fields": {
                "summary": summary,
                "description": null,
                "status": { "name": "To Do", "statusCategory": { "key": "new" } },
                "assignee": null,
                "project": { "key": "WEB" },
                "issuetype": { "name": "Task" },
                "updated": format!(
                    "2026-03-29T{:02}:{:02}:{:02}.000+0000",
                    id / 3600,
                    id / 60 % 60,
                    id % 60
                ),
            },
        })
    }

    /// The data dir of an app that has finished its initial sync and has
    /// `issues` in its cache.
    pub(crate) fn synced(tokio: &Runtime, issues: &[Value]) -> tempfile::TempDir {
        synced_on(tokio, "http://localhost", issues)
    }

    /// `synced` for an app that is signed in to `site_url`.
    pub(crate) fn synced_on(
        tokio: &Runtime,
        site_url: &str,
        issues: &[Value],
    ) -> tempfile::TempDir {
        let data_dir = tempfile::tempdir().unwrap();
        let config = json!({
            "site_url": site_url,
            "email": "ada@example.com",
            "account_id": "5b10a",
        });
        std::fs::write(data_dir.path().join("config.json"), config.to_string()).unwrap();
        std::fs::write(data_dir.path().join("api-token"), "secret-token").unwrap();
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir.path())
                .await
                .unwrap();
            state.store_issues(issues).await.unwrap();
        });
        watch(tokio, data_dir.path(), &["WEB"]);
        data_dir
    }

    /// Makes the app of `data_dir` watch `projects`, their initial sync finished.
    pub(crate) fn watch(tokio: &Runtime, data_dir: &std::path::Path, projects: &[&str]) {
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir).await.unwrap();
            state.set_synced(projects).await.unwrap();
        });
    }

    /// What the sync engine does when Jira reports a new or changed issue.
    pub(crate) fn sync_brings(cx: &mut TestAppContext, tokio: &Runtime, issue: Value) {
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        let key = issue["key"].as_str().unwrap().into();
        tokio.block_on(state.store_issues(&[issue])).unwrap();
        state.emit(SyncEvent::IssuesUpdated(vec![key]));
    }

    /// `synced` for an app that watches WEB and API. API-1 and API-2 hold the
    /// ids 101 and 102 so that they are newer than the WEB issues.
    fn synced_two_projects(tokio: &Runtime, issues: &[Value]) -> tempfile::TempDir {
        let data_dir = synced(tokio, issues);
        watch(tokio, data_dir.path(), &["WEB", "API"]);
        data_dir
    }

    pub(crate) fn api_issue(id: u32, summary: &str) -> Value {
        let mut issue = issue(id, summary);
        issue["key"] = json!(format!("API-{id}"));
        issue["fields"]["project"] = json!({ "key": "API" });
        issue
    }

    /// The rows on screen, top to bottom.
    pub(crate) fn rows(window: &Window) -> Vec<String> {
        window
            .find_all_by_role(Role::ListItem)
            .iter()
            .map(|row| row.label().unwrap().to_owned())
            .collect()
    }

    fn wait_for_rows(cx: &mut TestAppContext, window: AnyWindowHandle, expected: &[&str]) {
        wait_until(cx, window, |window| rows(window) == expected);
    }

    const LOGIN: &str = "WEB-1: Fix login, To Do, Unassigned";
    const LOGOUT: &str = "WEB-2: Fix logout, To Do, Unassigned";
    const CHECKOUT: &str = "WEB-3: Broken checkout, To Do, Unassigned";

    fn three_issues() -> [Value; 3] {
        [
            issue(1, "Fix login"),
            issue(2, "Fix logout"),
            issue(3, "Broken checkout"),
        ]
    }

    #[gpui_kit::test]
    fn the_window_lists_the_stored_issues(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut assigned = issue(2, "Broken checkout");
        assigned["fields"]["status"] =
            json!({ "name": "In Progress", "statusCategory": { "key": "indeterminate" } });
        assigned["fields"]["assignee"] =
            json!({ "accountId": "5b10a", "displayName": "Ada Lovelace" });
        let data_dir = synced(&tokio, &[issue(1, "Fix login"), assigned]);

        let window = launch(cx, &tokio, data_dir.path());

        wait_for_rows(
            cx,
            window,
            &[
                "WEB-2: Broken checkout, In Progress, Ada Lovelace",
                "WEB-1: Fix login, To Do, Unassigned",
            ],
        );
        // Each row is as wide as the window, so the columns line up
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("issue-WEB-1").bounds().size.width, px(800.));
        });
    }

    #[gpui_kit::test]
    fn each_row_shows_the_priority_of_its_issue_under_the_column_name(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut high = issue(1, "Fix login");
        high["fields"]["priority"] = json!({ "id": "2", "name": "High" });
        let mut long = issue(3, &"Checkout breaks on a card that expired ".repeat(8));
        long["fields"]["priority"] = json!({ "id": "10000", "name": "Blocker" });
        let data_dir = synced(&tokio, &[high, issue(2, "Fix logout"), long]);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| rows(window).len() == 3);

        in_window(cx, window, |window, _| {
            assert_eq!(rows(window)[1], LOGOUT);
            assert_eq!(rows(window)[2], format!("{LOGIN}, High priority"));
            assert_eq!(window.find("priority-WEB-1").label(), Some("High"));
            assert_eq!(window.find("priority-WEB-3").label(), Some("Blocker"));
            assert!(window.try_find("priority-WEB-2").is_none());
            // A long summary is cut off before the column
            let column = window.find("priority-column").bounds().origin.x;
            for cell in ["priority-WEB-1", "priority-WEB-3"] {
                let left = window.find(cell).bounds().origin.x;
                assert_eq!(left, column, "{cell} is out of line");
            }
        });

        let mut lowered = issue(1, "Fix login");
        lowered["fields"]["priority"] = json!({ "id": "4", "name": "Low" });
        lowered["fields"]["updated"] = json!("2026-03-30T00:00:00.000+0000");
        sync_brings(cx, &tokio, lowered);
        wait_until(cx, window, |window| {
            rows(window).first() == Some(&format!("{LOGIN}, Low priority"))
        });

        // The open issue shows the priority, and the list beside it has no room
        in_window(cx, window, |window, cx| window.click("issue-WEB-1", cx));
        in_window(cx, window, |window, _| {
            assert!(window.try_find("priority-column").is_none());
            assert!(window.try_find("priority-WEB-1").is_none());
        });
    }

    #[gpui_kit::test]
    fn each_keystroke_from_the_second_on_searches(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("lo", cx);
            // The answer cannot be here yet: the old rows stay until it is
            assert_eq!(rows(window), [CHECKOUT, LOGOUT, LOGIN]);
        });
        wait_until(cx, window, |window| {
            let mut found = rows(window);
            found.sort();
            found == [LOGIN, LOGOUT]
        });

        in_window(cx, window, |window, cx| {
            window.input("gi", cx);
            assert_eq!(rows(window).len(), 2);
        });
        wait_for_rows(cx, window, &[LOGIN]);

        in_window(cx, window, |window, cx| window.input("x", cx));
        wait_for_rows(cx, window, &[]);
    }

    #[gpui_kit::test]
    fn a_query_of_one_character_shows_every_issue(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("che", cx);
        });
        wait_for_rows(cx, window, &[CHECKOUT]);

        in_window(cx, window, |window, cx| {
            window.press("backspace", cx);
            window.press("backspace", cx);
        });

        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);
    }

    #[gpui_kit::test]
    fn the_list_follows_the_sync_engine(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        sync_brings(cx, &tokio, issue(4, "Login page is slow"));
        wait_for_rows(
            cx,
            window,
            &[
                "WEB-4: Login page is slow, To Do, Unassigned",
                CHECKOUT,
                LOGOUT,
                LOGIN,
            ],
        );

        // With a query in the input it is the query that is repeated
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("checkout", cx);
        });
        wait_for_rows(cx, window, &[CHECKOUT]);
        sync_brings(cx, &tokio, issue(5, "Checkout button"));
        wait_until(cx, window, |window| rows(window).len() == 2);
        sync_brings(cx, &tokio, issue(6, "Unrelated"));
        let mut renamed = issue(3, "Broken payment");
        // The cache only takes an issue that Jira changed after the stored one
        renamed["fields"]["updated"] = json!("2026-03-30T00:00:00.000+0000");
        sync_brings(cx, &tokio, renamed);
        wait_for_rows(cx, window, &["WEB-5: Checkout button, To Do, Unassigned"]);
    }

    const API_LOGIN: &str = "API-101: Login endpoint, To Do, Unassigned";
    const API_TOKEN: &str = "API-102: Token refresh, To Do, Unassigned";

    fn two_projects() -> Vec<Value> {
        let mut issues = three_issues().to_vec();
        issues.extend([
            api_issue(101, "Login endpoint"),
            api_issue(102, "Token refresh"),
        ]);
        issues
    }

    /// Waits for the project menu: the projects are read apart from the
    /// issues, so the rows can be on screen a moment before the button.
    #[track_caller]
    pub(crate) fn wait_for_project_filter(cx: &mut TestAppContext, window: AnyWindowHandle) {
        wait_until(cx, window, |window| {
            window.try_find("project-filter").is_some()
        });
    }

    /// Waits until `list` has read the watched projects and they are `expected`.
    #[track_caller]
    fn wait_for_projects(cx: &mut TestAppContext, list: &Entity<IssueList>, expected: &[&str]) {
        let started = Instant::now();
        while list.read_with(cx, |list, _| list.projects != expected) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the projects were not read"
            );
            std::thread::sleep(Duration::from_millis(10));
            cx.run_until_parked();
        }
    }

    /// The same for the list of the home screen of `window`: with one project
    /// there is no button, so nothing on screen tells that they are read.
    #[track_caller]
    pub(crate) fn wait_for_watched(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        expected: &[&str],
    ) {
        let list = cx
            .update_window(window, |root, _, cx| {
                let root = root.downcast::<gpui_kit::base::Root>().unwrap();
                let home = root.read(cx).view().clone().downcast::<crate::Root>();
                match &home.unwrap().read(cx).screen {
                    crate::Screen::Home { list, .. } => list.clone(),
                    _ => panic!("the home screen is not open"),
                }
            })
            .unwrap();
        wait_for_projects(cx, &list, expected);
    }

    /// Opens the project menu and clicks the item `name`.
    pub(crate) fn choose_project(window: &mut Window, cx: &mut gpui_kit::App, name: &str) {
        choose(window, cx, "project-filter", name)
    }

    /// Opens the menu of `button` and clicks the item `name`.
    fn choose(window: &mut Window, cx: &mut gpui_kit::App, button: &'static str, name: &str) {
        window.click(button, cx);
        window.render_frame(cx);
        let items = window.find_all_by_role(Role::MenuItem);
        let item = items.iter().find(|item| item.label() == Some(name));
        let labels: Vec<_> = items.iter().map(|item| item.label()).collect();
        let item = item.unwrap_or_else(|| panic!("no item {name} among {labels:?}"));
        let middle = item.bounds().center();
        window.pointer_down(middle, MouseButton::Left, cx);
        window.pointer_up(middle, MouseButton::Left, cx);
        window.render_frame(cx);
    }

    /// The items of the menu of `button`. Leaves the menu closed.
    fn menu_of(window: &mut Window, cx: &mut gpui_kit::App, button: &'static str) -> Vec<String> {
        window.click(button, cx);
        window.render_frame(cx);
        let items = window.find_all_by_role(Role::MenuItem);
        // A separator is an item without a label
        let labels = items.iter().filter_map(|item| item.label());
        let labels = labels.map(str::to_owned).collect();
        window.press("escape", cx);
        window.render_frame(cx);
        labels
    }

    /// Waits until the menu of `button` holds `expected`: the core fills the
    /// menus of the filters after the window is open, and again after a sync.
    #[track_caller]
    fn wait_for_menu(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        button: &'static str,
        expected: &[&str],
    ) {
        let started = Instant::now();
        loop {
            cx.run_until_parked();
            let menu = in_window(cx, window, |window, cx| menu_of(window, cx, button));
            if menu == expected {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the menu of {button} holds {menu:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Opens the menu all the filters collapse into, enters the submenu
    /// `filter` and clicks its item `name`.
    fn choose_collapsed(window: &mut Window, cx: &mut gpui_kit::App, filter: &str, name: &str) {
        choose_in(window, cx, "filters", &[filter, name])
    }

    /// Opens the menu of `button` and clicks the items of `path` in turn,
    /// each in the submenu the one before opened.
    fn choose_in(window: &mut Window, cx: &mut gpui_kit::App, button: &'static str, path: &[&str]) {
        window.click(button, cx);
        window.render_frame(cx);
        for &label in path {
            let items = window.find_all_by_role(Role::MenuItem);
            let labels: Vec<_> = items.iter().map(|item| item.label()).collect();
            // A submenu can repeat a label of its parent, and comes after it
            let item = items.iter().rev().find(|item| item.label() == Some(label));
            let item = item.unwrap_or_else(|| panic!("no item {label} among {labels:?}"));
            let middle = item.bounds().center();
            window.pointer_down(middle, MouseButton::Left, cx);
            window.pointer_up(middle, MouseButton::Left, cx);
            window.render_frame(cx);
        }
    }

    fn label(window: &Window, id: &'static str) -> String {
        window.find(id).label().unwrap().to_owned()
    }

    const ADA: (&str, &str) = ("5b10a", "Ada Lovelace");
    const GRACE: (&str, &str) = ("7c20b", "Grace Hopper");

    fn in_status(mut issue: Value, name: &str, category: &str) -> Value {
        issue["fields"]["status"] = json!({ "name": name, "statusCategory": { "key": category } });
        issue
    }

    fn assigned_to(mut issue: Value, (id, name): (&str, &str)) -> Value {
        issue["fields"]["assignee"] = json!({ "accountId": id, "displayName": name });
        issue
    }

    const MINE: &str = "WEB-1: Fix login, In Progress, Ada Lovelace";
    const GRACES: &str = "WEB-2: Fix logout, To Do, Grace Hopper";
    const NOBODYS: &str = "WEB-3: Broken checkout, In Progress, Unassigned";
    const API_MINE: &str = "API-101: Login endpoint, In Progress, Ada Lovelace";

    /// Two projects whose issues differ in assignee and status. The app is
    /// signed in as Ada.
    fn team_work() -> Vec<Value> {
        let in_progress = |issue| in_status(issue, "In Progress", "indeterminate");
        vec![
            assigned_to(in_progress(issue(1, "Fix login")), ADA),
            assigned_to(issue(2, "Fix logout"), GRACE),
            in_progress(issue(3, "Broken checkout")),
            assigned_to(in_progress(api_issue(101, "Login endpoint")), ADA),
        ]
    }

    #[gpui_kit::test]
    fn the_assignee_filter_offers_me_nobody_and_everyone_in_the_cache(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_menu(
            cx,
            window,
            "assignee-filter",
            &["Any assignee", "Me", "Unassigned", "Grace Hopper"],
        );
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "assignee-filter"), "Assignee");
            assert_eq!(label(window, "issue-count"), "4 issues");
            assert!(window.try_find("assignee-filter-clear").is_none());
            assert!(window.try_find("clear-filters").is_none());
        });

        in_window(cx, window, |window, cx| {
            choose(window, cx, "assignee-filter", "Me")
        });
        wait_for_rows(cx, window, &[API_MINE, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "assignee-filter"), "Assignee: Me");
            assert_eq!(label(window, "issue-count"), "2 issues");
        });

        in_window(cx, window, |window, cx| {
            choose(window, cx, "assignee-filter", "Unassigned")
        });
        wait_for_rows(cx, window, &[NOBODYS]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "issue-count"), "1 issue");
        });

        in_window(cx, window, |window, cx| {
            choose(window, cx, "assignee-filter", "Grace Hopper")
        });
        wait_for_rows(cx, window, &[GRACES]);

        in_window(cx, window, |window, cx| {
            window.click("assignee-filter-clear", cx)
        });
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "assignee-filter"), "Assignee");
        });
    }

    #[gpui_kit::test]
    fn the_status_filter_offers_the_statuses_in_the_cache(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "In Progress", "Exclude"],
        );

        in_window(cx, window, |window, cx| {
            choose(window, cx, "status-filter", "In Progress")
        });
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status: In Progress");
        });

        in_window(cx, window, |window, cx| {
            choose(window, cx, "status-filter", "Any status")
        });
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status");
        });
    }

    #[gpui_kit::test]
    fn the_filters_combine_with_the_project_and_the_search(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_project_filter(cx, window);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "In Progress", "Exclude"],
        );

        in_window(cx, window, |window, cx| {
            choose(window, cx, "status-filter", "In Progress");
            choose(window, cx, "assignee-filter", "Me");
        });
        wait_for_rows(cx, window, &[API_MINE, MINE]);
        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_for_rows(cx, window, &[MINE]);

        // The search looks among the issues the filters let through
        in_window(cx, window, |window, cx| {
            choose_project(window, cx, "All projects");
            choose(window, cx, "status-filter", "Any status");
            window.click("search", cx);
            window.input("fix", cx);
        });
        wait_for_rows(cx, window, &[MINE]);

        // One action takes off every filter, and leaves the search
        in_window(cx, window, |window, cx| {
            choose_project(window, cx, "WEB");
            window.click("clear-filters", cx);
        });
        wait_until(cx, window, |window| {
            let mut found = rows(window);
            found.sort();
            found == [MINE, GRACES]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "project-filter"), "All projects");
            assert_eq!(label(window, "assignee-filter"), "Assignee");
            assert_eq!(label(window, "status-filter"), "Status");
            assert!(window.try_find("clear-filters").is_none());
        });
    }

    const SHIPPED: &str = "WEB-4: Ship the docs, Done, Unassigned";
    const DROPPED: &str = "WEB-5: Log in by fax, Won't Do, Unassigned";

    /// `team_work` with WEB-4 in Done and WEB-5 in Won't Do, both finished.
    fn finished_work() -> Vec<Value> {
        let mut issues = team_work();
        issues.push(in_status(issue(4, "Ship the docs"), "Done", "done"));
        issues.push(in_status(issue(5, "Log in by fax"), "Won't Do", "done"));
        issues
    }

    /// Opens the menu of the status filter and clicks `name` in its submenu Exclude.
    fn exclude(window: &mut Window, cx: &mut gpui_kit::App, name: &str) {
        choose_in(window, cx, "status-filter", &["Exclude", name])
    }

    /// The statuses checked in the submenu Exclude of the status filter.
    /// Leaves the menu closed.
    fn excluded(window: &mut Window, cx: &mut gpui_kit::App) -> Vec<String> {
        choose_in(window, cx, "status-filter", &["Exclude"]);
        let items = window.find_all_by_role(Role::MenuItem);
        // The submenu comes after its parent, whose chosen item is checked too
        let submenu = items
            .iter()
            .skip_while(|item| item.label() != Some("Exclude"));
        let checked = submenu.skip(1).filter(|item| item.checked() == Some(true));
        let labels = checked
            .filter_map(|item| item.label().map(str::to_owned))
            .collect();
        window.press("escape", cx);
        window.render_frame(cx);
        labels
    }

    #[gpui_kit::test]
    fn statuses_are_excluded_one_by_one_and_taken_off_at_once(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &finished_work());
        let window = launch(cx, &tokio, data_dir.path());
        let all = [API_MINE, DROPPED, SHIPPED, NOBODYS, GRACES, MINE];
        wait_for_rows(cx, window, &all);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &[
                "Any status",
                "To Do",
                "In Progress",
                "Done",
                "Won't Do",
                "Exclude",
            ],
        );

        in_window(cx, window, |window, cx| exclude(window, cx, "Done"));
        wait_for_rows(cx, window, &[API_MINE, DROPPED, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status: not Done");
            assert_eq!(label(window, "issue-count"), "5 issues");
        });

        in_window(cx, window, |window, cx| exclude(window, cx, "Won't Do"));
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "status-filter"), "Status: not Done, Won't Do");
            assert_eq!(label(window, "issue-count"), "4 issues");
            assert_eq!(excluded(window, cx), ["Done", "Won't Do"]);
        });
        // Showing one status and leaving out others are one filter, so no
        // status to show is checked, and Any status is not either
        in_window(cx, window, |window, cx| {
            window.click("status-filter", cx);
            window.render_frame(cx);
            let items = window.find_all_by_role(Role::MenuItem);
            assert!(items.iter().all(|item| item.checked() != Some(true)));
            window.press("escape", cx);
        });

        // Past two names the button counts the rest, and the menu names them
        in_window(cx, window, |window, cx| exclude(window, cx, "To Do"));
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, MINE]);
        in_window(cx, window, |window, cx| {
            assert_eq!(
                label(window, "status-filter"),
                "Status: not Done, Won't Do +1"
            );
            assert_eq!(excluded(window, cx), ["To Do", "Done", "Won't Do"]);
        });

        // A checked status is taken out of the exclusion again
        in_window(cx, window, |window, cx| exclude(window, cx, "Won't Do"));
        wait_for_rows(cx, window, &[API_MINE, DROPPED, NOBODYS, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status: not Done, To Do");
        });

        in_window(cx, window, |window, cx| {
            window.click("status-filter-clear", cx)
        });
        wait_for_rows(cx, window, &all);
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "status-filter"), "Status");
            assert!(excluded(window, cx).is_empty());
        });

        // The last status taken out of the exclusion takes the filter off
        in_window(cx, window, |window, cx| exclude(window, cx, "Done"));
        wait_for_rows(cx, window, &[API_MINE, DROPPED, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, cx| exclude(window, cx, "Done"));
        wait_for_rows(cx, window, &all);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status");
            assert!(window.try_find("clear-filters").is_none());
        });
    }

    #[gpui_kit::test]
    fn one_status_to_show_replaces_the_excluded_ones_and_the_reverse(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &finished_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(
            cx,
            window,
            &[API_MINE, DROPPED, SHIPPED, NOBODYS, GRACES, MINE],
        );
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &[
                "Any status",
                "To Do",
                "In Progress",
                "Done",
                "Won't Do",
                "Exclude",
            ],
        );

        in_window(cx, window, |window, cx| exclude(window, cx, "Done"));
        in_window(cx, window, |window, cx| {
            choose(window, cx, "status-filter", "Won't Do")
        });
        wait_for_rows(cx, window, &[DROPPED]);
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "status-filter"), "Status: Won't Do");
            assert!(excluded(window, cx).is_empty());
        });
        in_window(cx, window, |window, cx| exclude(window, cx, "To Do"));
        wait_for_rows(cx, window, &[API_MINE, DROPPED, SHIPPED, NOBODYS, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status: not To Do");
        });
    }

    #[gpui_kit::test]
    fn the_exclusion_combines_with_the_other_filters_and_the_search(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let mut issues = finished_work();
        issues.push(assigned_to(
            in_status(issue(6, "Fix signup"), "Done", "done"),
            ADA,
        ));
        let data_dir = synced_two_projects(&tokio, &issues);
        let window = launch(cx, &tokio, data_dir.path());
        const SIGNUP: &str = "WEB-6: Fix signup, Done, Ada Lovelace";
        wait_until(cx, window, |window| rows(window).len() == 7);
        wait_for_project_filter(cx, window);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &[
                "Any status",
                "To Do",
                "In Progress",
                "Done",
                "Won't Do",
                "Exclude",
            ],
        );

        in_window(cx, window, |window, cx| {
            choose(window, cx, "assignee-filter", "Me");
        });
        wait_for_rows(cx, window, &[API_MINE, SIGNUP, MINE]);
        in_window(cx, window, |window, cx| exclude(window, cx, "Done"));
        wait_for_rows(cx, window, &[API_MINE, MINE]);
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "issue-count"), "2 issues");
            choose_project(window, cx, "WEB");
        });
        wait_for_rows(cx, window, &[MINE]);

        // The search looks only among the issues the exclusion lets through
        in_window(cx, window, |window, cx| {
            choose(window, cx, "assignee-filter", "Any assignee");
            window.click("search", cx);
            window.input("fix", cx);
        });
        wait_until(cx, window, |window| {
            let mut found = rows(window);
            found.sort();
            found == [MINE, GRACES]
        });
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "issue-count"), "2 issues");
        });

        // Beside an open issue the exclusion is a submenu of the status
        in_window(cx, window, |window, cx| window.click("issue-WEB-1", cx));
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "filters"), "Filters · 2");
            choose_in(
                window,
                cx,
                "filters",
                &["Status: not Done", "Exclude", "To Do"],
            );
        });
        wait_for_rows(cx, window, &[MINE]);
        in_window(cx, window, |window, cx| {
            assert_eq!(
                menu_of(window, cx, "filters"),
                [
                    "Project: WEB",
                    "Assignee",
                    "Status: not Done, To Do",
                    "Clear filters"
                ]
            );
        });
        in_window(cx, window, |window, cx| {
            choose(window, cx, "filters", "Clear filters")
        });
        wait_until(cx, window, |window| {
            let mut found = rows(window);
            found.sort();
            found == [MINE, GRACES, SIGNUP]
        });
    }

    #[gpui_kit::test]
    fn an_excluded_status_that_left_the_cache_hides_nothing_and_can_be_taken_off(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "In Progress", "Exclude"],
        );
        in_window(cx, window, |window, cx| exclude(window, cx, "To Do"));
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, MINE]);

        // The only issue in To Do is done now
        let mut done = in_status(assigned_to(issue(2, "Fix logout"), GRACE), "Done", "done");
        done["fields"]["updated"] = json!("2026-03-30T00:00:00.000+0000");
        sync_brings(cx, &tokio, done);

        const GRACES_DONE: &str = "WEB-2: Fix logout, Done, Grace Hopper";
        wait_for_rows(cx, window, &[GRACES_DONE, API_MINE, NOBODYS, MINE]);
        // To Do is no longer one to show, but stays checked among the excluded
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "In Progress", "Done", "Exclude"],
        );
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "status-filter"), "Status: not To Do");
            assert_eq!(excluded(window, cx), ["To Do"]);
        });
        in_window(cx, window, |window, cx| exclude(window, cx, "To Do"));
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "status-filter"), "Status");
            choose_in(window, cx, "status-filter", &["Exclude"]);
            let items = window.find_all_by_role(Role::MenuItem);
            let labels: Vec<_> = items.iter().filter_map(|item| item.label()).collect();
            assert!(!labels.contains(&"To Do"), "{labels:?}");
        });
    }

    const DOCS: &str = "WEB-3: Write docs, To Do, Unassigned";
    const ENDPOINT: &str = "API-101: Add endpoint, To Do, Unassigned";

    /// WEB-1 in the running Sprint 7, WEB-2 in the planned Sprint 8, WEB-3
    /// and API-101 in none.
    const TWO_SPRINTS: &str = "
        INSERT INTO sprints (id, name, state)
            VALUES (8, 'Sprint 8', 'future'), (7, 'Sprint 7', 'active');
        UPDATE issues SET sprint_id = 7 WHERE key = 'WEB-1';
        UPDATE issues SET sprint_id = 8 WHERE key = 'WEB-2';";

    /// The data dir of an app that watches WEB and API, with the sprints
    /// `sprints` puts in its cache.
    fn synced_with_sprints(tokio: &Runtime, sprints: &str) -> tempfile::TempDir {
        let issues = [
            issue(1, "Fix login"),
            issue(2, "Fix logout"),
            issue(3, "Write docs"),
            api_issue(101, "Add endpoint"),
        ];
        let data_dir = synced_two_projects(tokio, &issues);
        tokio.block_on(async {
            let (state, _events) = app_core::start_without_keychain(data_dir.path())
                .await
                .unwrap();
            state.execute(sprints).await.unwrap();
        });
        data_dir
    }

    #[gpui_kit::test]
    fn the_sprint_filter_offers_the_running_sprint_the_planned_one_and_the_backlog(
        cx: &mut TestAppContext,
    ) {
        let tokio = tokio_runtime();
        let data_dir = synced_with_sprints(&tokio, TWO_SPRINTS);
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[ENDPOINT, DOCS, LOGOUT, LOGIN]);
        wait_until(cx, window, |window| {
            window.try_find("sprint-filter").is_some()
        });
        wait_for_menu(
            cx,
            window,
            "sprint-filter",
            &["Any sprint", "Sprint 7", "Sprint 8 (future)", "Backlog"],
        );

        in_window(cx, window, |window, cx| {
            choose(window, cx, "sprint-filter", "Sprint 7");
        });
        wait_for_rows(cx, window, &[LOGIN]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "sprint-filter"), "Sprint: Sprint 7");
        });

        in_window(cx, window, |window, cx| {
            choose(window, cx, "sprint-filter", "Sprint 8 (future)");
        });
        wait_for_rows(cx, window, &[LOGOUT]);

        in_window(cx, window, |window, cx| {
            choose(window, cx, "sprint-filter", "Backlog");
        });
        wait_for_rows(cx, window, &[ENDPOINT, DOCS]);

        // The cross takes the filter off
        in_window(cx, window, |window, cx| {
            window.click("sprint-filter-clear", cx)
        });
        wait_for_rows(cx, window, &[ENDPOINT, DOCS, LOGOUT, LOGIN]);
    }

    #[gpui_kit::test]
    fn the_sprint_combines_with_the_other_filters_and_the_search(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_with_sprints(
            &tokio,
            "INSERT INTO sprints (id, name, state) VALUES (7, 'Sprint 7', 'active');
             UPDATE issues SET sprint_id = 7 WHERE key IN ('WEB-1', 'WEB-2', 'API-101');
             UPDATE issues SET status = 'In Progress', status_category = 'in_progress',
                               assignee_id = '5b10a', assignee_name = 'Ada Lovelace'
                 WHERE key IN ('WEB-2', 'WEB-3', 'API-101');",
        );
        let window = launch(cx, &tokio, data_dir.path());
        const MY_LOGOUT: &str = "WEB-2: Fix logout, In Progress, Ada Lovelace";
        const MY_ENDPOINT: &str = "API-101: Add endpoint, In Progress, Ada Lovelace";
        const MY_DOCS: &str = "WEB-3: Write docs, In Progress, Ada Lovelace";
        wait_for_rows(cx, window, &[MY_ENDPOINT, MY_DOCS, MY_LOGOUT, LOGIN]);
        wait_for_project_filter(cx, window);
        wait_until(cx, window, |window| {
            window.try_find("sprint-filter").is_some()
        });

        in_window(cx, window, |window, cx| {
            choose(window, cx, "sprint-filter", "Sprint 7");
        });
        wait_for_rows(cx, window, &[MY_ENDPOINT, MY_LOGOUT, LOGIN]);
        in_window(cx, window, |window, cx| {
            choose(window, cx, "assignee-filter", "Me");
            choose(window, cx, "status-filter", "In Progress");
        });
        wait_for_rows(cx, window, &[MY_ENDPOINT, MY_LOGOUT]);
        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_for_rows(cx, window, &[MY_LOGOUT]);

        // The search looks among the issues the filters let through
        in_window(cx, window, |window, cx| {
            choose_project(window, cx, "All projects");
            choose(window, cx, "assignee-filter", "Any assignee");
            choose(window, cx, "status-filter", "Any status");
            window.click("search", cx);
            window.input("fix", cx);
        });
        wait_until(cx, window, |window| {
            let mut found = rows(window);
            found.sort();
            found == [LOGIN, MY_LOGOUT]
        });

        // Clear filters takes the sprint off with the others
        in_window(cx, window, |window, cx| window.click("clear-filters", cx));
        wait_until(cx, window, |window| {
            label(window, "sprint-filter") == "Sprint"
        });
    }

    #[gpui_kit::test]
    fn the_sprint_filter_is_offered_only_where_there_are_sprints(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_with_sprints(&tokio, "");
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[ENDPOINT, DOCS, LOGOUT, LOGIN]);
        wait_for_project_filter(cx, window);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "Exclude"],
        );

        // A site without sprints: no button, and the key opens nothing
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("sprint-filter").is_none());
            window.press("shift-r", cx);
            window.render_frame(cx);
            assert!(window.find_all_by_role(Role::MenuItem).is_empty());
        });

        // A sync finds the sprints of WEB
        let state = cx.update(|cx| cx.global::<Core>().state.clone());
        tokio.block_on(state.execute(TWO_SPRINTS)).unwrap();
        state.emit(SyncEvent::IssuesUpdated(vec!["WEB-1".into()]));
        wait_until(cx, window, |window| {
            window.try_find("sprint-filter").is_some()
        });

        // API has no issue in a sprint
        in_window(cx, window, |window, cx| choose_project(window, cx, "API"));
        wait_for_rows(cx, window, &[ENDPOINT]);
        in_window(cx, window, |window, _| {
            assert!(window.try_find("sprint-filter").is_none());
        });
        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_until(cx, window, |window| {
            window.try_find("sprint-filter").is_some()
        });
    }

    #[gpui_kit::test]
    fn the_sprint_is_chosen_from_the_keyboard_and_beside_an_open_issue(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_with_sprints(&tokio, TWO_SPRINTS);
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[ENDPOINT, DOCS, LOGOUT, LOGIN]);
        wait_until(cx, window, |window| {
            window.try_find("sprint-filter").is_some()
        });

        // The menu holds Any sprint, Sprint 7, Sprint 8 and Backlog
        for key in ["shift-r", "down", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[LOGIN]);

        // Beside an open issue the sprint is a submenu of the one button
        in_window(cx, window, |window, cx| window.press("enter", cx));
        wait_until(cx, window, |window| window.try_find("filters").is_some());
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "filters"), "Filters · 1");
            choose_collapsed(window, cx, "Sprint: Sprint 7", "Backlog");
        });
        wait_until(cx, window, |window| rows(window).len() == 2);
    }

    #[gpui_kit::test]
    fn a_filter_whose_value_left_the_cache_says_why_the_list_is_empty(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "In Progress", "Exclude"],
        );
        in_window(cx, window, |window, cx| {
            choose(window, cx, "status-filter", "To Do")
        });
        wait_for_rows(cx, window, &[GRACES]);
        in_window(cx, window, |window, _| {
            assert!(window.try_find("no-matches").is_none());
        });

        // The only issue in To Do is done now
        let mut done = in_status(assigned_to(issue(2, "Fix logout"), GRACE), "Done", "done");
        done["fields"]["updated"] = json!("2026-03-30T00:00:00.000+0000");
        sync_brings(cx, &tokio, done);

        wait_for_rows(cx, window, &[]);
        // The chosen status is still in its menu, after the ones the cache has
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "In Progress", "Done", "To Do", "Exclude"],
        );
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "no-matches"), "No issues match the filters");
            assert_eq!(label(window, "status-filter"), "Status: To Do");
            assert_eq!(label(window, "issue-count"), "0 issues");
            window.click("clear-filters", cx);
        });
        wait_until(cx, window, |window| rows(window).len() == 4);
        in_window(cx, window, |window, _| {
            assert!(window.try_find("no-matches").is_none());
        });
    }

    #[gpui_kit::test]
    fn the_count_of_a_search_that_was_cut_short_says_there_are_more(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let issues: Vec<_> = (1..=60)
            .map(|id| issue(id, &format!("Task {id}")))
            .collect();
        let data_dir = synced(&tokio, &issues);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| {
            label(window, "issue-count") == "60 issues"
        });

        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("task", cx);
        });

        wait_until(cx, window, |window| {
            label(window, "issue-count") == "50+ issues"
        });
    }

    #[gpui_kit::test]
    fn beside_an_open_issue_the_filters_are_the_submenus_of_one_button(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "In Progress", "Exclude"],
        );
        in_window(cx, window, |window, cx| {
            assert!(window.try_find("filters").is_none());
            choose(window, cx, "status-filter", "In Progress");
        });
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, MINE]);

        in_window(cx, window, |window, cx| window.click("issue-WEB-3", cx));

        // The filter set in the wide list is counted, and its submenu names it
        in_window(cx, window, |window, cx| {
            for gone in [
                "project-filter",
                "assignee-filter",
                "status-filter",
                "issue-count",
            ] {
                assert!(window.try_find(gone).is_none(), "{gone} is on screen");
            }
            assert_eq!(label(window, "filters"), "Filters · 1");
            assert_eq!(
                menu_of(window, cx, "filters"),
                [
                    "Project",
                    "Assignee",
                    "Status: In Progress",
                    "Clear filters"
                ]
            );
        });
        in_window(cx, window, |window, cx| {
            choose_collapsed(window, cx, "Assignee", "Me")
        });
        wait_for_rows(cx, window, &[API_MINE, MINE]);
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "filters"), "Filters · 2");
            choose_collapsed(window, cx, "Status: In Progress", "Any status");
        });
        // The menu that has just closed opens again in the next frame
        in_window(cx, window, |window, cx| {
            choose_collapsed(window, cx, "Project", "WEB")
        });
        wait_for_rows(cx, window, &[MINE]);
        in_window(cx, window, |window, cx| {
            assert_eq!(label(window, "filters"), "Filters · 2");
            choose(window, cx, "filters", "Clear filters");
        });
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "filters"), "Filters");
        });
    }

    #[gpui_kit::test]
    fn a_chosen_project_narrows_the_list_and_all_projects_widen_it(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
        wait_for_project_filter(cx, window);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("project-filter").label(), Some("All projects"));
        });

        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("project-filter").label(), Some("Project: WEB"));
        });

        in_window(cx, window, |window, cx| choose_project(window, cx, "API"));
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN]);

        in_window(cx, window, |window, cx| {
            choose_project(window, cx, "All projects")
        });
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
    }

    #[gpui_kit::test]
    fn the_project_is_chosen_from_the_keyboard(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
        wait_for_project_filter(cx, window);

        // The menu holds All projects, WEB and API
        for key in ["p", "down", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }

        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);
        in_window(cx, window, |window, _| {
            assert_eq!(window.find("project-filter").label(), Some("Project: WEB"));
        });
    }

    #[gpui_kit::test]
    fn the_assignee_and_the_status_are_chosen_from_the_keyboard(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &["Any status", "To Do", "In Progress", "Exclude"],
        );

        // The menu holds Any assignee, Me, Unassigned and Grace Hopper
        for key in ["shift-a", "down", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[API_MINE, MINE]);

        // The menu holds Any status, To Do and In Progress
        for key in ["shift-s", "down", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "assignee-filter"), "Assignee: Me");
            assert_eq!(label(window, "status-filter"), "Status: To Do");
        });

        // Beside an open issue the same keys open the menu of all the filters
        in_window(cx, window, |window, cx| {
            window.click("clear-filters", cx);
        });
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, cx| window.press("enter", cx));
        for key in ["shift-s", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        in_window(cx, window, |window, _| {
            assert!(!window.find_all_by_role(Role::MenuItem).is_empty());
        });
    }

    #[gpui_kit::test]
    fn statuses_are_excluded_from_the_keyboard(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &finished_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(
            cx,
            window,
            &[API_MINE, DROPPED, SHIPPED, NOBODYS, GRACES, MINE],
        );
        wait_for_menu(
            cx,
            window,
            "status-filter",
            &[
                "Any status",
                "To Do",
                "In Progress",
                "Done",
                "Won't Do",
                "Exclude",
            ],
        );

        // Exclude is the last item. Enter opens its submenu on its first
        // item, on whichever side there is room for it; the submenu holds To
        // Do, In Progress, Done and Won't Do
        for key in ["shift-s", "up", "enter", "down", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[API_MINE, DROPPED, NOBODYS, GRACES, MINE]);
        for key in ["shift-s", "up", "enter", "down", "down", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status: not Done, Won't Do");
        });

        // Any status, first in the menu, takes the exclusion off
        for key in ["shift-s", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(
            cx,
            window,
            &[API_MINE, DROPPED, SHIPPED, NOBODYS, GRACES, MINE],
        );
        in_window(cx, window, |window, _| {
            assert_eq!(label(window, "status-filter"), "Status");
        });
    }

    #[gpui_kit::test]
    fn the_keys_of_the_list_work_after_a_pick_in_a_submenu(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &team_work());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_MINE, NOBODYS, GRACES, MINE]);
        wait_for_project_filter(cx, window);
        in_window(cx, window, |window, cx| window.press("enter", cx));
        wait_until(cx, window, |window| window.try_find("filters").is_some());

        // Beside an open issue the menu holds Project, Assignee and Status,
        // each a submenu that Enter opens on its first item: Me after Any
        // assignee, WEB after All projects
        for key in ["shift-s", "down", "down", "enter", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[API_MINE, MINE]);
        for key in ["shift-s", "down", "enter", "down", "enter"] {
            in_window(cx, window, |window, cx| window.press(key, cx));
        }
        wait_for_rows(cx, window, &[MINE]);
    }

    #[gpui_kit::test]
    fn the_selection_stays_in_view_while_it_moves_down_a_long_list(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let issues: Vec<_> = (1..=60)
            .map(|id| issue(id, &format!("Task {id}")))
            .collect();
        let data_dir = synced(&tokio, &issues);
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| !rows(window).is_empty());
        in_window(cx, window, |window, _| {
            assert!(window.try_find("issue-WEB-21").is_none());
        });

        // WEB-60 is the first row
        for _ in 0..40 {
            in_window(cx, window, |window, cx| window.press("j", cx));
        }

        in_window(cx, window, |window, _| {
            let row = window.find("issue-WEB-21");
            assert_eq!(row.selected(), Some(true));
            let list = window.find("home").bounds();
            assert!(row.bounds().top() >= list.top() && row.bounds().bottom() <= list.bottom());
        });
    }

    #[gpui_kit::test]
    fn the_search_finds_only_the_issues_of_the_chosen_project(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
        wait_for_project_filter(cx, window);

        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("login", cx);
        });
        wait_until(cx, window, |window| rows(window).len() == 2);
        let mut all = in_window(cx, window, |window, _| rows(window));
        all.sort();
        assert_eq!(all, [API_LOGIN, LOGIN]);

        in_window(cx, window, |window, cx| choose_project(window, cx, "API"));
        wait_for_rows(cx, window, &[API_LOGIN]);

        // Another query keeps the project
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.input("tok", cx);
        });
        wait_for_rows(cx, window, &[API_TOKEN]);
        in_window(cx, window, |window, cx| {
            window.press("backspace", cx);
            window.press("backspace", cx);
            window.press("backspace", cx);
        });
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN]);
    }

    #[gpui_kit::test]
    fn the_chosen_project_stays_applied_when_a_sync_brings_issues(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced_two_projects(&tokio, &two_projects());
        let window = launch(cx, &tokio, data_dir.path());
        wait_for_rows(cx, window, &[API_TOKEN, API_LOGIN, CHECKOUT, LOGOUT, LOGIN]);
        wait_for_project_filter(cx, window);
        in_window(cx, window, |window, cx| choose_project(window, cx, "WEB"));
        wait_for_rows(cx, window, &[CHECKOUT, LOGOUT, LOGIN]);

        sync_brings(cx, &tokio, api_issue(103, "Rate limits"));
        sync_brings(cx, &tokio, issue(4, "Login page is slow"));

        wait_for_rows(
            cx,
            window,
            &[
                "WEB-4: Login page is slow, To Do, Unassigned",
                CHECKOUT,
                LOGOUT,
                LOGIN,
            ],
        );
    }

    #[gpui_kit::test]
    fn with_one_watched_project_there_is_nothing_to_choose(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &three_issues());
        cx.update(gpui_kit::init);
        init_core(cx, &tokio, data_dir.path());
        // The core answers from the tokio runtime, outside GPUI's scheduler
        cx.executor().allow_parking();
        let (window, list) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| IssueList::new(window, cx))
            })
            .unwrap()
        });
        // Nothing on screen tells that the projects are read, and before
        // that the button is missing whatever their number
        wait_for_projects(cx, &list, &["WEB"]);

        in_window(cx, window, |window, _| {
            assert!(window.try_find("project-filter").is_none());
        });
    }

    fn twelve_thousand_issues() -> Vec<Value> {
        (1..=12_000)
            .map(|id| issue(id, &format!("Task {id}")))
            .collect()
    }

    fn found(key: &str) -> anyhow::Result<Vec<IssueRow>> {
        Ok(vec![IssueRow {
            id: key.to_owned(),
            key: key.into(),
            summary: "Found".to_owned(),
            status: "To Do".to_owned(),
            status_category: StatusCategory::Todo,
            assignee_name: None,
            priority: None,
            project_key: "WEB".to_owned(),
            sync_status: SyncStatus::Synced,
        }])
    }

    #[gpui_kit::test]
    fn a_slower_earlier_query_does_not_replace_a_later_one(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &[]);
        cx.update(gpui_kit::init);
        init_core(cx, &tokio, data_dir.path());
        // The core answers from the tokio runtime, outside GPUI's scheduler
        cx.executor().allow_parking();
        let (window, list) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| IssueList::new(window, cx))
            })
            .unwrap()
        });

        // Two queries whose answers the test releases in the opposite order
        let (answer_earlier, earlier) = oneshot::channel::<()>();
        let (answer_later, later) = oneshot::channel::<()>();
        list.update(cx, |list, cx| {
            let earlier = cx.background_spawn(async move {
                earlier.await?;
                found("WEB-1")
            });
            list.show(earlier, cx);
            let later = cx.background_spawn(async move {
                later.await?;
                found("WEB-2")
            });
            list.show(later, cx);
        });
        answer_later.send(()).unwrap();
        cx.run_until_parked();
        // Fails once the view has dropped the earlier query, which is the point
        answer_earlier.send(()).ok();
        cx.run_until_parked();

        in_window(cx, window, |window, _| {
            assert_eq!(rows(window), ["WEB-2: Found, To Do, Unassigned"]);
        });
    }

    #[gpui_kit::test]
    fn the_list_stays_responsive_with_12_000_issues(cx: &mut TestAppContext) {
        let tokio = tokio_runtime();
        let data_dir = synced(&tokio, &twelve_thousand_issues());
        let window = launch(cx, &tokio, data_dir.path());
        wait_until(cx, window, |window| !rows(window).is_empty());

        // Only the rows that fit the 600 px window are built
        let frame = Instant::now();
        let shown = in_window(cx, window, |window, _| rows(window));
        let frame = frame.elapsed();
        assert_eq!(shown[0], "WEB-12000: Task 12000, To Do, Unassigned");
        assert!(shown.len() < 30, "{} rows were built", shown.len());
        assert!(frame < Duration::from_millis(100), "a frame took {frame:?}");

        let search = Instant::now();
        in_window(cx, window, |window, cx| {
            window.click("search", cx);
            window.input("11999", cx);
        });
        wait_for_rows(cx, window, &["WEB-11999: Task 11999, To Do, Unassigned"]);
        let search = search.elapsed();
        assert!(
            search < Duration::from_millis(500),
            "five keystrokes took {search:?}"
        );
    }
}
