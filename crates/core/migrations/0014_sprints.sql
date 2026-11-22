-- The sprints that are running or planned, as the last sync found them on the
-- issues of the watched projects. A sprint that is closed is not kept
CREATE TABLE sprints (
    id     INTEGER PRIMARY KEY,  -- Jira's sprint id
    name   TEXT NOT NULL,
    state  TEXT NOT NULL         -- 'active' | 'future'
);

-- The active or future sprint the issue is in. Jira allows one at most
ALTER TABLE issues ADD COLUMN sprint_id INTEGER;

CREATE INDEX idx_issues_sprint ON issues(sprint_id);
