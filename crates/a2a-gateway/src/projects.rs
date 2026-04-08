/// Project registry — CRUD for the `projects` and `agent_projects` tables.
///
/// Visibility state machine:
///
///   lan-only ──► group ──► public
///      ◄──────────────────────────
///
/// Transitions are guarded: a project must be idle (is_active = 0) to change.
/// The caller is responsible for checking idleness before calling set_visibility.

use crate::db::Db;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::util::now_secs;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    LanOnly,
    Group,
    Public,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LanOnly => "lan-only",
            Self::Group   => "group",
            Self::Public  => "public",
        }
    }

    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "lan-only" => Ok(Self::LanOnly),
            "group"    => Ok(Self::Group),
            "public"   => Ok(Self::Public),
            other      => bail!("unknown visibility '{other}'"),
        }
    }

    /// Returns true if transition from self → next is a valid single step.
    ///
    /// Allowed transitions (must move exactly one level):
    ///   lan-only ↔ group ↔ public
    ///
    /// Skipping a level (lan-only → public) or staying the same is rejected.
    pub fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::LanOnly, Self::Group)
            | (Self::Group, Self::LanOnly)
            | (Self::Group, Self::Public)
            | (Self::Public, Self::Group)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id:              String,
    pub name:            String,
    pub repo_url:        String,
    pub description:     String,
    pub visibility:      Visibility,
    pub folder_path:     String,
    pub file_glob:       String,
    pub is_active:       bool,
    pub created_at:      f64,
    pub updated_at:      f64,
    pub onboarding_step: Option<String>,
    pub origin_gateway:  String,
    pub peer_gateway:    String,
    pub peer_project_id: String,
}

// ── ProjectStore ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ProjectStore {
    db: Db,
}

impl ProjectStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // ── Create ────────────────────────────────────────────────────────────

    pub async fn create(
        &self,
        name:           &str,
        repo_url:       &str,
        description:    &str,
        origin_gateway: &str,
        peer_gateway:   &str,
        peer_project_id: &str,
    ) -> Result<Project> {
        let id  = Uuid::new_v4().to_string();
        let now = now_secs();

        // Peer projects start at onboarding_step = 'awaiting_folder'
        // Local projects have no onboarding step
        let onboarding = if peer_gateway.is_empty() { None } else { Some("awaiting_folder") };

        sqlx::query(
            "INSERT INTO projects \
             (id, name, repo_url, description, visibility, folder_path, file_glob, \
              is_active, created_at, updated_at, onboarding_step, \
              origin_gateway, peer_gateway, peer_project_id) \
             VALUES ($1,$2,$3,$4,'lan-only','','**/*',0,$5,$6,$7,$8,$9,$10)"
        )
        .bind(&id)
        .bind(name)
        .bind(repo_url)
        .bind(description)
        .bind(now)
        .bind(now)
        .bind(onboarding)
        .bind(origin_gateway)
        .bind(peer_gateway)
        .bind(peer_project_id)
        .execute(&self.db)
        .await
        .context("insert project")?;

        self.get(&id).await?.context("project not found after insert")
    }

    // ── Read ──────────────────────────────────────────────────────────────

    pub async fn get(&self, id: &str) -> Result<Option<Project>> {
        let row = sqlx::query_as::<_, ProjectRow>(
            "SELECT * FROM projects WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .context("get project")?;
        Ok(row.map(Project::from))
    }

    pub async fn get_by_name(&self, name: &str) -> Result<Option<Project>> {
        let row = sqlx::query_as::<_, ProjectRow>(
            "SELECT * FROM projects WHERE name = $1"
        )
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .context("get project by name")?;
        Ok(row.map(Project::from))
    }

    pub async fn list(&self) -> Result<Vec<Project>> {
        let rows = sqlx::query_as::<_, ProjectRow>(
            "SELECT * FROM projects ORDER BY created_at DESC"
        )
        .fetch_all(&self.db)
        .await
        .context("list projects")?;
        Ok(rows.into_iter().map(Project::from).collect())
    }

    pub async fn list_by_visibility(&self, vis: &Visibility) -> Result<Vec<Project>> {
        let rows = sqlx::query_as::<_, ProjectRow>(
            "SELECT * FROM projects WHERE visibility = $1 ORDER BY created_at DESC"
        )
        .bind(vis.as_str())
        .fetch_all(&self.db)
        .await
        .context("list projects by visibility")?;
        Ok(rows.into_iter().map(Project::from).collect())
    }

    /// Returns projects that have a pending onboarding step.
    pub async fn list_pending_onboarding(&self) -> Result<Vec<Project>> {
        let rows = sqlx::query_as::<_, ProjectRow>(
            "SELECT * FROM projects WHERE onboarding_step IS NOT NULL ORDER BY created_at ASC"
        )
        .fetch_all(&self.db)
        .await
        .context("list pending onboarding")?;
        Ok(rows.into_iter().map(Project::from).collect())
    }

    // ── Update ────────────────────────────────────────────────────────────

    pub async fn set_visibility(&self, id: &str, next: Visibility) -> Result<()> {
        // Guard: project must be idle
        let project = self.get(id).await?.context("project not found")?;
        if project.is_active {
            bail!("cannot change visibility while project is active");
        }
        if !project.visibility.can_transition_to(&next) {
            bail!("project is already at visibility '{}'", next.as_str());
        }

        sqlx::query(
            "UPDATE projects SET visibility = $1, updated_at = $2 WHERE id = $3"
        )
        .bind(next.as_str())
        .bind(now_secs())
        .bind(id)
        .execute(&self.db)
        .await
        .context("set visibility")?;
        Ok(())
    }

    pub async fn set_folder_path(&self, id: &str, path: &str) -> Result<()> {
        // Canonicalize to prevent symlink traversal and directory escape
        let canonical = std::path::Path::new(path)
            .canonicalize()
            .with_context(|| format!("folder path does not exist or is inaccessible: {path}"))?;

        if !canonical.is_dir() {
            bail!("folder path is not a directory: {}", canonical.display());
        }

        sqlx::query(
            "UPDATE projects SET folder_path = $1, updated_at = $2 WHERE id = $3"
        )
        .bind(canonical.to_string_lossy().as_ref())
        .bind(now_secs())
        .bind(id)
        .execute(&self.db)
        .await
        .context("set folder path")?;
        Ok(())
    }

    pub async fn set_file_glob(&self, id: &str, glob: &str) -> Result<()> {
        sqlx::query(
            "UPDATE projects SET file_glob = $1, updated_at = $2 WHERE id = $3"
        )
        .bind(glob)
        .bind(now_secs())
        .bind(id)
        .execute(&self.db)
        .await
        .context("set file glob")?;
        Ok(())
    }

    pub async fn set_onboarding_step(&self, id: &str, step: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE projects SET onboarding_step = $1, updated_at = $2 WHERE id = $3"
        )
        .bind(step)
        .bind(now_secs())
        .bind(id)
        .execute(&self.db)
        .await
        .context("set onboarding step")?;
        Ok(())
    }

    pub async fn set_active(&self, id: &str, active: bool) -> Result<()> {
        sqlx::query(
            "UPDATE projects SET is_active = $1, updated_at = $2 WHERE id = $3"
        )
        .bind(active as i64)
        .bind(now_secs())
        .bind(id)
        .execute(&self.db)
        .await
        .context("set active")?;
        Ok(())
    }

    // ── Agent membership ──────────────────────────────────────────────────

    pub async fn add_agent(&self, agent_name: &str, project_id: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO agent_projects (agent_name, project_id, joined_at) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING"
        )
        .bind(agent_name)
        .bind(project_id)
        .bind(now_secs())
        .execute(&self.db)
        .await
        .context("add agent to project")?;
        Ok(())
    }

    pub async fn list_agents(&self, project_id: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT agent_name FROM agent_projects WHERE project_id = $1"
        )
        .bind(project_id)
        .fetch_all(&self.db)
        .await
        .context("list agents for project")?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// Returns the project a given agent is currently assigned to (if any).
    pub async fn agent_project(&self, agent_name: &str) -> Result<Option<Project>> {
        let row = sqlx::query_as::<_, ProjectRow>(
            "SELECT p.* FROM projects p \
             JOIN agent_projects ap ON ap.project_id = p.id \
             WHERE ap.agent_name = $1 \
             ORDER BY ap.joined_at DESC LIMIT 1"
        )
        .bind(agent_name)
        .fetch_optional(&self.db)
        .await
        .context("agent project lookup")?;
        Ok(row.map(Project::from))
    }
}

// ── SQLx row mapping ──────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct ProjectRow {
    id:              String,
    name:            String,
    repo_url:        String,
    description:     String,
    visibility:      String,
    folder_path:     String,
    file_glob:       String,
    is_active:       i64,
    created_at:      f64,
    updated_at:      f64,
    onboarding_step: Option<String>,
    origin_gateway:  String,
    peer_gateway:    String,
    peer_project_id: String,
}

impl From<ProjectRow> for Project {
    fn from(r: ProjectRow) -> Self {
        Self {
            id:              r.id,
            name:            r.name,
            repo_url:        r.repo_url,
            description:     r.description,
            visibility:      Visibility::from_str(&r.visibility).unwrap_or(Visibility::LanOnly),
            folder_path:     r.folder_path,
            file_glob:       r.file_glob,
            is_active:       r.is_active != 0,
            created_at:      r.created_at,
            updated_at:      r.updated_at,
            onboarding_step: r.onboarding_step,
            origin_gateway:  r.origin_gateway,
            peer_gateway:    r.peer_gateway,
            peer_project_id: r.peer_project_id,
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn store() -> ProjectStore {
        let db = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        ProjectStore::new(db)
    }

    #[tokio::test]
    async fn create_and_get() {
        let s = store().await;
        let p = s.create("alpha", "https://github.com/x/alpha", "desc", "site-a", "", "").await.unwrap();
        assert_eq!(p.name, "alpha");
        assert_eq!(p.visibility, Visibility::LanOnly);
        assert!(p.onboarding_step.is_none());

        let got = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(got.id, p.id);
    }

    #[tokio::test]
    async fn peer_project_starts_onboarding() {
        let s = store().await;
        let p = s.create("beta", "https://github.com/x/beta", "", "site-a", "site-b", "remote-id").await.unwrap();
        assert_eq!(p.onboarding_step.as_deref(), Some("awaiting_folder"));
        assert_eq!(p.peer_gateway, "site-b");
    }

    #[tokio::test]
    async fn visibility_transition() {
        let s = store().await;
        let p = s.create("gamma", "https://github.com/x/g", "", "site-a", "", "").await.unwrap();
        s.set_visibility(&p.id, Visibility::Group).await.unwrap();
        let got = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(got.visibility, Visibility::Group);
    }

    #[tokio::test]
    async fn visibility_blocked_while_active() {
        let s = store().await;
        let p = s.create("delta", "https://github.com/x/d", "", "site-a", "", "").await.unwrap();
        s.set_active(&p.id, true).await.unwrap();
        let err = s.set_visibility(&p.id, Visibility::Group).await.unwrap_err();
        assert!(err.to_string().contains("active"));
    }

    #[tokio::test]
    async fn agent_membership() {
        let s = store().await;
        let p = s.create("epsilon", "https://github.com/x/e", "", "site-a", "", "").await.unwrap();
        s.add_agent("orchestrator", &p.id).await.unwrap();
        s.add_agent("research", &p.id).await.unwrap();
        let agents = s.list_agents(&p.id).await.unwrap();
        assert_eq!(agents.len(), 2);
    }

    #[tokio::test]
    async fn onboarding_step_progression() {
        let s = store().await;
        let p = s.create("zeta", "https://github.com/x/z", "", "site-a", "site-b", "r1").await.unwrap();
        assert_eq!(p.onboarding_step.as_deref(), Some("awaiting_folder"));

        s.set_folder_path(&p.id, "/data/projects/zeta").await.unwrap();
        s.set_onboarding_step(&p.id, Some("awaiting_files")).await.unwrap();
        let got = s.get(&p.id).await.unwrap().unwrap();
        assert_eq!(got.onboarding_step.as_deref(), Some("awaiting_files"));
        assert_eq!(got.folder_path, "/data/projects/zeta");

        s.set_onboarding_step(&p.id, None).await.unwrap();
        let done = s.get(&p.id).await.unwrap().unwrap();
        assert!(done.onboarding_step.is_none());
    }
}
