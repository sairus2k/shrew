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
#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "snake_case")]
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

/// The Jira account the app is signed in to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub site_url: String,
    pub email: String,
    pub account_id: String,
}
