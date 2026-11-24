-- A timer can run before its issue is known: the issue is chosen at the stop
-- or while it runs. SQLite cannot drop NOT NULL from a column, so the table
-- is built again with the row it holds
CREATE TABLE running_timer_new (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    issue_key   TEXT,             -- NULL until the user chooses the issue
    started_at  INTEGER NOT NULL  -- Unix seconds
);
INSERT INTO running_timer_new (id, issue_key, started_at)
    SELECT id, issue_key, started_at FROM running_timer;
DROP TABLE running_timer;
ALTER TABLE running_timer_new RENAME TO running_timer;
