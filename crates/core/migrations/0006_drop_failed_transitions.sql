-- A refused status change is announced and rolled back, and no longer kept.
DELETE FROM sync_queue WHERE entity_type = 'transition' AND status = 'failed';
