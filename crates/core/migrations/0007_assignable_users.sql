-- One row per issue: the people Jira lets the issue be assigned to.
CREATE TABLE assignable_users (
    issue_key   TEXT PRIMARY KEY,
    users       TEXT NOT NULL,  -- JSON array: account_id, display_name
    fetched_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
