-- One row per workflow position: the transitions available from a given status.
CREATE TABLE transition_cache (
    project_key  TEXT NOT NULL,
    issue_type   TEXT NOT NULL,
    from_status  TEXT NOT NULL,
    transitions  TEXT NOT NULL,  -- JSON array: id, name, to_status, to_status_category, required_fields
    fetched_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (project_key, issue_type, from_status)
);
