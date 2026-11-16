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
    pub project_key: String,
    pub sync_status: SyncStatus,
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
    /// The page of the issue in Jira.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub author_name: String,
    /// As Jira sent it, in the time zone of the account: `2026-03-29T01:30:00.000+0100`.
    pub created: String,
    /// Markdown.
    pub body_md: String,
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

/// What the app still needs before it can show issues.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStep {
    SignIn,
    ChooseProjects,
    InitialSync,
    Ready,
}
