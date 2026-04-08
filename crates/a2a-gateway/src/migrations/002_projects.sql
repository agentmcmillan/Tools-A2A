-- Projects: every agent must belong to a project, every project references a repo.
--
-- visibility: 'lan-only' | 'group' | 'public'
--   Transitions are guarded: project must be idle (no active agent calls) to change.
--
-- folder_path: where the synced project files are placed on disk (user-chosen at onboard)
-- file_glob:   filter for which local files to include in the content-addressed snapshot
--
-- onboarding_step tracks interactive onboarding state:
--   NULL / 'done'         = fully onboarded
--   'awaiting_folder'     = waiting for user to confirm local folder placement
--   'awaiting_files'      = waiting for user to confirm file filter/glob
--   'building_context'    = dispatched to user's own agent for context analysis
--   'awaiting_user_input' = context ready, showing user the project overview + prompt
--
CREATE TABLE IF NOT EXISTS projects (
    id           TEXT PRIMARY KEY,             -- UUID
    name         TEXT NOT NULL UNIQUE,
    repo_url     TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    visibility   TEXT NOT NULL DEFAULT 'lan-only'
                     CHECK (visibility IN ('lan-only', 'group', 'public')),
    folder_path  TEXT NOT NULL DEFAULT '',     -- absolute path on disk, set during onboarding
    file_glob    TEXT NOT NULL DEFAULT '**/*', -- glob filter for file sync
    is_active    INTEGER NOT NULL DEFAULT 0,   -- 1 if any agent is currently working on it
    created_at   DOUBLE PRECISION NOT NULL,
    updated_at   DOUBLE PRECISION NOT NULL,

    -- onboarding
    onboarding_step TEXT,                      -- NULL = done, else see states above
    origin_gateway  TEXT NOT NULL DEFAULT '',  -- gateway name that owns this project

    -- peer project link
    peer_gateway    TEXT NOT NULL DEFAULT '',  -- empty if local-origin project
    peer_project_id TEXT NOT NULL DEFAULT ''   -- empty if local-origin project
);

CREATE INDEX IF NOT EXISTS projects_visibility ON projects(visibility);
CREATE INDEX IF NOT EXISTS projects_onboarding ON projects(onboarding_step) WHERE onboarding_step IS NOT NULL;

-- Agent ↔ Project membership
CREATE TABLE IF NOT EXISTS agent_projects (
    agent_name   TEXT NOT NULL,
    project_id   TEXT NOT NULL,
    joined_at    DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (agent_name, project_id)
);
