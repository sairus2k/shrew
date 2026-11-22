-- How far the polls have read Jira. It describes the rows of this database,
-- so it is committed with them: a page and the position it moves are one
-- transaction. Always one row; `db::clear` puts it back to these defaults.
CREATE TABLE sync_state (
    id                           INTEGER PRIMARY KEY CHECK (id = 1),
    -- When the last sync started, in Unix milliseconds. NULL until the initial
    -- sync has finished
    last_sync_at                 INTEGER,
    -- When the last pass for deleted and moved issues started. 0 until the first
    last_reconcile_at            INTEGER NOT NULL DEFAULT 0,
    -- Where the next poll of changed worklogs starts: the `until` Jira gave
    -- with the last page. 0 asks for every worklog, once
    last_worklog_sync_at         INTEGER NOT NULL DEFAULT 0,
    -- Same for the poll of deleted worklogs, which Jira reports separately
    last_worklog_delete_sync_at  INTEGER NOT NULL DEFAULT 0
);

INSERT INTO sync_state (id) VALUES (1);

-- The projects whose issues the app keeps, in the order they were chosen
CREATE TABLE watched_projects (
    key       TEXT PRIMARY KEY,
    position  INTEGER NOT NULL,
    -- Added after the initial sync, and its issues are not all downloaded yet.
    -- A quit in the middle leaves it set for the next start
    pending   INTEGER NOT NULL DEFAULT 0
);
