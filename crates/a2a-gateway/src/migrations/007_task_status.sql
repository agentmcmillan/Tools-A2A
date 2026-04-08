-- Task status: 10-state lifecycle machine for task tracking.
--
-- State machine (valid transitions):
--
--   created ──► queued ──► in_progress ──► completed
--                  │            │                │
--                  │            ├──► blocked ─────┤
--                  │            │                 │
--                  │            ├──► waiting ─────┤
--                  │            │                 │
--                  │            ├──► delegated    │
--                  │            │                 │
--                  └──► cancelled               failed
--                                                │
--                                           timed_out
--
-- task_status is independent of entry_type:
--   entry_type = what the log entry IS (focus, done, blocked, note)
--   task_status = what state the TASK is in (created, queued, in_progress, ...)
--
-- task_status is nullable: existing entries and pure "note" entries have no status.

ALTER TABLE task_log ADD COLUMN IF NOT EXISTS task_status TEXT
    CHECK (task_status IS NULL OR task_status IN (
        'created', 'queued', 'in_progress',
        'waiting_for_input', 'blocked',
        'completed', 'failed', 'cancelled',
        'timed_out', 'delegated'
    ));

-- Index for reconciler queries: find stuck/active tasks
CREATE INDEX IF NOT EXISTS task_log_status
    ON task_log(project_id, task_status)
    WHERE task_status IS NOT NULL;

-- Temporal validity: entries naturally expire without manual deletion.
-- valid_from defaults to created_at; valid_to = NULL means no expiry.
ALTER TABLE task_log ADD COLUMN IF NOT EXISTS valid_from DOUBLE PRECISION;
ALTER TABLE task_log ADD COLUMN IF NOT EXISTS valid_to   DOUBLE PRECISION;

-- Index for expiry queries
CREATE INDEX IF NOT EXISTS task_log_validity
    ON task_log(valid_to)
    WHERE valid_to IS NOT NULL;
