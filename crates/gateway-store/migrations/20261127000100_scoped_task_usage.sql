ALTER TABLE usage_events ADD COLUMN IF NOT EXISTS task_service_name text;

CREATE INDEX IF NOT EXISTS usage_events_scoped_task_idx
    ON usage_events (task_service_name, task_id, created_at DESC, id DESC)
    WHERE task_service_name IS NOT NULL AND task_id IS NOT NULL;
