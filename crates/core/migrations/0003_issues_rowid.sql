-- issues_fts finds its rows by issues.rowid. With a TEXT primary key that rowid
-- is implicit, and SQLite is free to renumber implicit rowids on VACUUM, which
-- would leave the index pointing at other issues. An INTEGER PRIMARY KEY is the
-- rowid itself and is never renumbered. SQLite cannot change a primary key in
-- place, so the table is rebuilt.
CREATE TABLE issues_new (
    rowid               INTEGER PRIMARY KEY,
    id                  TEXT NOT NULL UNIQUE,  -- Jira's issue id
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
    remote_updated_at   TEXT NOT NULL,  -- Jira's `updated` in UTC: 2026-03-29T00:30:00.000Z
    synced_at           TEXT
);

INSERT INTO issues_new
    (rowid, id, key, summary, description_text, status, status_category,
     assignee_id, assignee_name, reporter_id, project_key, issue_type, priority,
     labels, raw_json, sync_status, remote_updated_at, synced_at)
SELECT rowid, id, key, summary, description_text, status, status_category,
       assignee_id, assignee_name, reporter_id, project_key, issue_type, priority,
       labels, raw_json, sync_status, remote_updated_at, synced_at
FROM issues;

-- Takes the indexes and the FTS triggers with it
DROP TABLE issues;
ALTER TABLE issues_new RENAME TO issues;

CREATE INDEX idx_issues_project  ON issues(project_key);
CREATE INDEX idx_issues_status   ON issues(status_category);
CREATE INDEX idx_issues_project_status ON issues(project_key, status_category);
CREATE INDEX idx_issues_assignee ON issues(assignee_id);
CREATE INDEX idx_issues_updated  ON issues(remote_updated_at DESC);

CREATE TRIGGER issues_fts_ai AFTER INSERT ON issues BEGIN
    INSERT INTO issues_fts(rowid, key, summary, description_text)
    VALUES (new.rowid, new.key, new.summary, new.description_text);
END;

CREATE TRIGGER issues_fts_ad AFTER DELETE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts, rowid, key, summary, description_text)
    VALUES ('delete', old.rowid, old.key, old.summary, old.description_text);
END;

CREATE TRIGGER issues_fts_au AFTER UPDATE ON issues BEGIN
    INSERT INTO issues_fts(issues_fts, rowid, key, summary, description_text)
    VALUES ('delete', old.rowid, old.key, old.summary, old.description_text);
    INSERT INTO issues_fts(rowid, key, summary, description_text)
    VALUES (new.rowid, new.key, new.summary, new.description_text);
END;

-- The copy kept the rowids, but a database that was vacuumed before this
-- migration may already have an index that points at the wrong rows
INSERT INTO issues_fts(issues_fts) VALUES ('rebuild');
