CREATE TABLE IF NOT EXISTS memory_facts (
    id          BIGSERIAL PRIMARY KEY,
    agent_name  TEXT NOT NULL REFERENCES agents(name) ON DELETE CASCADE,
    project_id  TEXT,                          -- NULL = global fact
    topic       TEXT NOT NULL DEFAULT '',      -- free-form topic tag
    content     TEXT NOT NULL,                 -- verbatim content, never summarized
    created_at  DOUBLE PRECISION NOT NULL,
    valid_from  DOUBLE PRECISION NOT NULL,     -- defaults to created_at
    valid_to    DOUBLE PRECISION               -- NULL = no expiry
);

CREATE INDEX IF NOT EXISTS memory_facts_agent ON memory_facts(agent_name, created_at DESC);
CREATE INDEX IF NOT EXISTS memory_facts_project ON memory_facts(project_id, agent_name) WHERE project_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS memory_facts_expiry ON memory_facts(valid_to) WHERE valid_to IS NOT NULL;
