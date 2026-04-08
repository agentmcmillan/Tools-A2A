-- Agents registry
CREATE TABLE IF NOT EXISTS agents (
    name         TEXT PRIMARY KEY,
    version      TEXT NOT NULL DEFAULT '0.1.0',
    endpoint     TEXT NOT NULL,
    capabilities TEXT NOT NULL DEFAULT '[]',   -- JSON array of strings
    soul_toml    TEXT NOT NULL DEFAULT '',
    last_seen    TEXT,                          -- RFC3339
    registered_at TEXT NOT NULL
);

-- Agent memories (append-only markdown)
CREATE TABLE IF NOT EXISTS memories (
    agent_name TEXT PRIMARY KEY REFERENCES agents(name) ON DELETE CASCADE,
    content_md TEXT NOT NULL DEFAULT ''
);

-- Agent todos (annotated, never deleted)
CREATE TABLE IF NOT EXISTS todos (
    id           BIGSERIAL PRIMARY KEY,
    agent_name   TEXT NOT NULL REFERENCES agents(name) ON DELETE CASCADE,
    task         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending', -- pending | in_progress | done
    notes        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    completed_at TEXT
);

-- User requests (append-only)
CREATE TABLE IF NOT EXISTS requests (
    id         BIGSERIAL PRIMARY KEY,
    content    TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Reconciled project task list
CREATE TABLE IF NOT EXISTS project_todos (
    id          BIGSERIAL PRIMARY KEY,
    task        TEXT NOT NULL UNIQUE,  -- deduplicated by normalised task text
    assigned_to TEXT,             -- agent name, NULL = unassigned
    status      TEXT NOT NULL DEFAULT 'pending',
    notes       TEXT NOT NULL DEFAULT '',
    source_todos TEXT NOT NULL DEFAULT '[]',  -- JSON array of todo IDs merged
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_todos_agent  ON todos(agent_name);
CREATE INDEX IF NOT EXISTS idx_todos_status ON todos(status);
CREATE INDEX IF NOT EXISTS idx_ptodos_status ON project_todos(status);
