use serde::{Deserialize, Serialize};
use std::fmt;

/// An issue key such as "PROJ-123".
#[derive(Clone, Debug, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct IssueKey(String);

impl From<&str> for IssueKey {
    fn from(key: &str) -> Self {
        Self(key.to_owned())
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// Jira's fourth category, "No Category" (key `undefined`), is stored as `Todo`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    Todo,
    InProgress,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
pub enum SyncStatus {
    Synced,
    Pending,
    Failed,
}

#[derive(Clone, Debug)]
pub struct IssueRow {
    pub id: String,
    pub key: IssueKey,
    pub summary: String,
    pub status: String,
    pub status_category: StatusCategory,
    pub assignee_name: Option<String>,
    /// Its name in the priority scheme of the site: Highest to Lowest by default.
    pub priority: Option<String>,
    pub project_key: String,
    pub sync_status: SyncStatus,
}

/// Which issues the list shows. A field left `None` does not narrow it, and
/// the fields that are set must all hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueFilter {
    pub project: Option<String>,
    pub assignee: Option<AssigneeFilter>,
    /// The name of a status. Projects that give a status the same name share it.
    pub status: Option<String>,
    pub sprint: Option<SprintFilter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SprintFilter {
    /// In no active or future sprint, and not done.
    Backlog,
    /// Jira's id of the sprint.
    Sprint(i64),
}

/// A sprint that is running or planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sprint {
    pub id: i64,
    pub name: String,
    /// Running. A sprint that is not is planned.
    pub active: bool,
    /// The projects that have an issue in it.
    pub project_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssigneeFilter {
    Unassigned,
    /// The account id of the assignee.
    User(String),
}

/// The values the cached issues have in the fields the list is filtered by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilterOptions {
    /// Everyone an issue is assigned to, by name.
    pub assignees: Vec<User>,
    /// To do first, done last, by name inside a category.
    pub statuses: Vec<String>,
    /// The active ones first. Empty for a site or for projects without sprints.
    pub sprints: Vec<Sprint>,
}

/// One issue with everything the detail view shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueDetail {
    pub key: IssueKey,
    pub summary: String,
    pub status: String,
    pub status_category: StatusCategory,
    /// `Pending` while a change made here has not reached Jira.
    pub sync_status: SyncStatus,
    /// Whether `status` is such a change. A pending issue whose status Jira
    /// has is one with another assignee.
    pub status_pending: bool,
    pub issue_type: Option<String>,
    pub assignee_name: Option<String>,
    pub reporter_name: Option<String>,
    pub priority: Option<String>,
    pub labels: Vec<String>,
    /// Markdown. Empty when the issue has no description.
    pub description_md: String,
    /// Oldest first.
    pub comments: Vec<Comment>,
    /// How many comments the issue has in Jira. Jira sends only part of a long
    /// thread with a search page, so this can be more than `comments.len()`.
    pub comment_total: usize,
    /// The one started last first.
    pub worklogs: Vec<Worklog>,
    /// The page of the issue in Jira.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// The id Jira gave it, which `api::reply_to_comment` takes. `None` for a
    /// comment Jira does not have.
    pub id: Option<String>,
    /// Set for a reply. It is listed after the comment it answers.
    pub reply_to: Option<ReplyTo>,
    pub author_name: String,
    /// As Jira sent it, in the time zone of the account: `2026-03-29T01:30:00.000+0100`.
    pub created: String,
    /// Markdown.
    pub body_md: String,
    /// Written here, and Jira has not received it yet. Such a comment has no
    /// `created` time.
    pub pending: bool,
    /// Set for a comment Jira refused. It stays until the user retries or discards it.
    pub rejected: Option<RejectedComment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyTo {
    /// Who wrote the comment answered. `None` when it is not among the
    /// comments of the issue: it was deleted, or Jira sent a part of the thread.
    pub author_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedComment {
    /// What `api::retry_comment` and `api::discard_comment` take.
    pub entry_id: i64,
    /// Why Jira refused it.
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worklog {
    /// The user's own name for a worklog logged here that Jira has not sent
    /// back yet.
    pub author_name: String,
    /// Unix seconds.
    pub started_at: i64,
    pub time_spent_secs: u64,
    pub comment: Option<String>,
    /// `Pending` until Jira has received it; `Failed` if Jira refused it.
    pub status: SyncStatus,
    /// Set for a worklog Jira or Tempo refused. It stays until the user retries
    /// or discards it.
    pub rejected: Option<RejectedWorklog>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedWorklog {
    /// What `api::retry_worklog` and `api::discard_worklog` take.
    pub entry_id: i64,
    /// Why it was refused.
    pub reason: String,
}

/// The timer that is running now. Only the start is stored: the elapsed time is
/// derived from it, so a restart or a sleep does not skew it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningTimer {
    pub issue_key: IssueKey,
    /// Unix seconds.
    pub started_at: i64,
}

impl RunningTimer {
    /// The system clock can be set back while the timer runs.
    pub fn elapsed_secs(&self, now: i64) -> u64 {
        (now - self.started_at).max(0) as u64
    }
}

/// One way out of a status: an entry of the status menu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_status: String,
    pub to_status_category: StatusCategory,
    /// The fields Jira refuses the transition without. Empty for most transitions.
    pub required_fields: Vec<RequiredField>,
}

/// A Jira user an issue can be assigned to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub account_id: String,
    pub display_name: String,
}

/// A field the user has to fill in before a transition can be sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequiredField {
    /// The key of the value in the request to Jira, such as `resolution`.
    pub id: String,
    pub name: String,
    /// Jira's description of the value, such as `{ "type": "resolution" }`.
    pub schema: serde_json::Value,
    /// The values Jira accepts, as it sent them. `None` for a field that
    /// takes any value of its type.
    pub allowed_values: Option<Vec<serde_json::Value>>,
}

/// The Jira account the app is signed in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub site_url: String,
    pub email: String,
    pub account_id: String,
}

/// A Jira project the account can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub key: String,
    pub name: String,
}

/// Where logged time is sent. Never both: Tempo mirrors its worklogs into Jira,
/// so posting to both would log the time twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorklogTarget {
    #[default]
    Jira,
    Tempo,
}

/// The look of the app the user chose: the one of the system, or always
/// light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorklogSettings {
    pub target: WorklogTarget,
    pub tempo_token_saved: bool,
}

/// What the app still needs before it can show issues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStep {
    SignIn,
    /// The account is known and its token is not: `api::renew_token` saves one.
    EnterToken,
    ChooseProjects,
    InitialSync,
    Ready,
}
