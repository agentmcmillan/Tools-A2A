-- Track the last cursor seen from each peer for each project.
-- Used for delta sync: "give me entries after cursor X from your project Y."
CREATE TABLE IF NOT EXISTS peer_cursor_state (
    peer_name    TEXT NOT NULL,
    project_id   TEXT NOT NULL,
    last_cursor  BIGINT NOT NULL DEFAULT 0,
    updated_at   DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (peer_name, project_id)
);
