-- The list shows the priority of every issue, and reading it out of raw_json
-- would parse the whole issue for each row. The column was there from the
-- start and nothing wrote it, so the cached issues take it from raw_json once
UPDATE issues SET priority = json_extract(raw_json, '$.fields.priority.name');
