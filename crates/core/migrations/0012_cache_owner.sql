-- At most one row: the account whose issues, unsent changes and timer this
-- database holds. The config file can be lost while the database is not, and
-- a sign-in has to know whether the data is its own
CREATE TABLE cache_owner (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    site_url    TEXT NOT NULL,
    account_id  TEXT NOT NULL,
    email       TEXT NOT NULL
);
