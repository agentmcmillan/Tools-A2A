-- Contribution proposals: PR-style, gated by gateway count.
--
-- Approval rules (checked at accept time):
--   gateway_count > 2  →  agents vote (majority wins); tracked in contribution_votes
--   gateway_count == 2 →  requires PR review by origin owner (status must be 'pr_accepted')
--
-- status: 'open' | 'voting' | 'pr_pending' | 'pr_accepted' | 'accepted' | 'rejected'
--
CREATE TABLE IF NOT EXISTS contribution_proposals (
    id              TEXT PRIMARY KEY,   -- UUID
    project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    proposer_gateway TEXT NOT NULL,
    proposer_agent   TEXT NOT NULL DEFAULT '',
    -- Snapshots are content-addressed and immutable once written; they are never
    -- deleted in normal operation. RESTRICT here ensures proposals block snapshot
    -- deletion, which is the correct safety behaviour.
    snapshot_id     TEXT NOT NULL REFERENCES snapshots(id) ON DELETE RESTRICT,
    description     TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'open'
                        CHECK (status IN ('open', 'voting', 'pr_pending', 'pr_accepted', 'accepted', 'rejected')),
    gateway_count   INTEGER NOT NULL DEFAULT 0, -- captured at proposal time
    created_at      DOUBLE PRECISION NOT NULL,
    resolved_at     DOUBLE PRECISION,
    resolved_by     TEXT                        -- gateway that cast the deciding vote/review
);

CREATE INDEX IF NOT EXISTS contrib_project ON contribution_proposals(project_id, status);

-- Agent votes for proposals (only used when gateway_count > 2)
CREATE TABLE IF NOT EXISTS contribution_votes (
    id              TEXT PRIMARY KEY,
    proposal_id     TEXT NOT NULL REFERENCES contribution_proposals(id) ON DELETE CASCADE,
    gateway_name    TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    vote            TEXT NOT NULL CHECK (vote IN ('approve', 'reject')),
    reason          TEXT NOT NULL DEFAULT '',
    voted_at        DOUBLE PRECISION NOT NULL,
    UNIQUE (proposal_id, gateway_name, agent_name)
);

CREATE INDEX IF NOT EXISTS votes_proposal ON contribution_votes(proposal_id);
