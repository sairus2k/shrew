CREATE TABLE sync_queue (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_type  TEXT NOT NULL,  -- 'transition' | 'comment' | 'worklog'
    issue_key    TEXT NOT NULL,
    operation    TEXT NOT NULL,  -- 'create' | 'update' | 'delete'
    payload      TEXT NOT NULL,  -- JSON for the Jira API request body
    status       TEXT NOT NULL DEFAULT 'pending', -- 'pending'|'in_progress'|'synced'|'failed'
    retry_count  INTEGER NOT NULL DEFAULT 0,
    error_msg    TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    synced_at    TEXT
);

CREATE INDEX idx_queue_status ON sync_queue(status, created_at);
