-- Display name of whoever logged the time. NULL for a worklog logged here that
-- Jira has not sent back yet: the user's own name is not stored locally.
ALTER TABLE worklogs ADD COLUMN author_name TEXT;
