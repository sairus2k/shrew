-- Tempo's id of a worklog that was posted to Tempo from here. Tempo's answer
-- does not carry the id of the Jira worklog it mirrors the worklog into; the
-- mirror names this id in its `tempo` property, which is how the two are matched.
ALTER TABLE worklogs ADD COLUMN tempo_id INTEGER;

CREATE UNIQUE INDEX idx_worklogs_tempo ON worklogs(tempo_id) WHERE tempo_id IS NOT NULL;
