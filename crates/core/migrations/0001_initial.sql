CREATE TABLE issues (
    id                  TEXT PRIMARY KEY,
    key                 TEXT UNIQUE NOT NULL,
    summary             TEXT NOT NULL,
    description_text    TEXT,           -- plain-text extracted from ADF, for FTS
    status              TEXT NOT NULL,
    status_category     TEXT NOT NULL,  -- 'todo' | 'in_progress' | 'done'
    assignee_id         TEXT,
    assignee_name       TEXT,
    reporter_id         TEXT,
    project_key         TEXT NOT NULL,
    issue_type          TEXT,
    priority            TEXT,
    labels              TEXT,           -- JSON array
    raw_json            TEXT NOT NULL,  -- full Jira API response
    sync_status         TEXT NOT NULL DEFAULT 'synced', -- 'synced'|'pending'|'failed'
    remote_updated_at   TEXT NOT NULL,  -- Jira's `updated` field (ISO 8601)
    synced_at           TEXT
);

CREATE INDEX idx_issues_project  ON issues(project_key);
CREATE INDEX idx_issues_status   ON issues(status_category);
CREATE INDEX idx_issues_project_status ON issues(project_key, status_category);
CREATE INDEX idx_issues_assignee ON issues(assignee_id);
CREATE INDEX idx_issues_updated  ON issues(remote_updated_at DESC);

CREATE TABLE users (
    id          TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    email       TEXT,
    avatar_url  TEXT
);

CREATE TABLE projects (
    key         TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    avatar_url  TEXT
);
