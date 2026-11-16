-- Content table FTS (references issues, stays in sync via triggers)
CREATE VIRTUAL TABLE issues_fts USING fts5(
    key,
    summary,
    description_text,
    content=issues,
    content_rowid=rowid,
    tokenize='porter unicode61'  -- porter stemming for better search
);

CREATE TRIGGER issues_fts_ai AFTER INSERT ON issues BEGIN
    INSERT INTO issues_fts(rowid, key, summary, description_text)
    VALUES (new.rowid, new.key, new.summary, new.description_text);
END;

-- A content table index is not updated in place: the old entry is removed
-- with the special 'delete' command, which needs the old column values
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
