CREATE TABLE worklogs (
    local_id        TEXT PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(8)))),
    remote_id       TEXT UNIQUE,        -- Jira worklog ID, NULL until synced
    issue_key       TEXT NOT NULL,
    time_spent_sec  INTEGER NOT NULL,
    started_at      TEXT NOT NULL,      -- UTC, fixed width: 2026-03-29T00:30:00.000Z
    comment         TEXT,
    sync_status     TEXT NOT NULL DEFAULT 'pending', -- 'synced'|'pending'|'failed'
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_worklogs_issue ON worklogs(issue_key, started_at DESC);
