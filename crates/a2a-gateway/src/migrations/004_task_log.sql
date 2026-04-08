-- Task log: append-only, gateway-signed distributed standup log per project.
--
-- Each entry records what a gateway/agent is working on.
-- Entries are never deleted or modified; superseded by new entries.
-- cursor is a monotonically increasing integer per project — used for delta sync.
-- Next cursor value: SELECT COALESCE(MAX(cursor), 0) + 1 FROM task_log WHERE project_id = $1
--
-- entry_type: 'focus' | 'done' | 'blocked' | 'note'
--
CREATE TABLE IF NOT EXISTS task_log (
    id           TEXT    PRIMARY KEY,   -- UUID
    project_id   TEXT    NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    gateway_name TEXT    NOT NULL,      -- which gateway wrote this
    agent_name   TEXT    NOT NULL DEFAULT '',
    entry_type   TEXT    NOT NULL DEFAULT 'focus'
                             CHECK (entry_type IN ('focus', 'done', 'blocked', 'note')),
    content      TEXT    NOT NULL,      -- free-form standup text
    cursor       INTEGER NOT NULL,      -- monotonic per-project, set by gateway on write
    created_at   DOUBLE PRECISION NOT NULL,

    -- optional link to a specific todo (SET NULL on todo deletion — log entry survives)
    todo_id      BIGINT REFERENCES todos(id) ON DELETE SET NULL,

    -- gateway-issued signature (HMAC-SHA256 of "id|project_id|gateway_name|content|created_at")
    -- using the gateway's JWT secret; NULL for local-only entries
    signature    TEXT
);

CREATE INDEX IF NOT EXISTS task_log_project   ON task_log(project_id, cursor);
CREATE INDEX IF NOT EXISTS task_log_gateway   ON task_log(gateway_name, created_at DESC);
