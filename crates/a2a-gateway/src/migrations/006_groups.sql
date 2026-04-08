-- Groups: named sets of gateways that share projects at 'group' visibility.
--
-- Membership is keyed by gateway name.
-- An invite token is single-use, cryptographically random, and group-scoped.
--
CREATE TABLE IF NOT EXISTS groups (
    id          TEXT PRIMARY KEY,   -- UUID
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_by  TEXT NOT NULL,      -- gateway name
    created_at  DOUBLE PRECISION NOT NULL
);

CREATE TABLE IF NOT EXISTS group_members (
    group_id     TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    gateway_name TEXT NOT NULL,
    joined_at    DOUBLE PRECISION NOT NULL,
    invited_by   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (group_id, gateway_name)
);

CREATE INDEX IF NOT EXISTS members_gateway ON group_members(gateway_name);

-- One-time invite tokens
CREATE TABLE IF NOT EXISTS invites (
    token       TEXT PRIMARY KEY,   -- 32-byte random hex string
    group_id    TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by  TEXT NOT NULL,      -- gateway name
    created_at  DOUBLE PRECISION NOT NULL,
    expires_at  DOUBLE PRECISION NOT NULL,      -- unix timestamp; tokens expire after 48h
    used_at     DOUBLE PRECISION,               -- NULL = not yet used
    used_by     TEXT                -- gateway name that consumed it
);

CREATE INDEX IF NOT EXISTS invites_group   ON invites(group_id);
CREATE INDEX IF NOT EXISTS invites_expires ON invites(expires_at) WHERE used_at IS NULL;

-- Group ↔ Project mapping (a group-visibility project belongs to one group)
CREATE TABLE IF NOT EXISTS group_projects (
    group_id   TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    added_at   DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (group_id, project_id)
);
