-- At most one row: the timer that is running now
CREATE TABLE running_timer (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    issue_key   TEXT NOT NULL,
    started_at  INTEGER NOT NULL  -- Unix seconds
);
