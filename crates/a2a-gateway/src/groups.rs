/// Group and invite management.
///
/// Groups are named sets of gateways that share 'group'-visibility projects.
/// Membership is managed via single-use crypto invite tokens (48h TTL).
///
/// Invite token: 32 random bytes, hex-encoded (64 chars).
/// A token is consumed atomically on first valid use — subsequent uses are rejected.

use crate::db::Db;
use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::util::now_secs;

const INVITE_TTL_SECS: f64 = 48.0 * 3600.0;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id:          String,
    pub name:        String,
    pub description: String,
    pub created_by:  String,
    pub created_at:  f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub group_id:     String,
    pub gateway_name: String,
    pub joined_at:    f64,
    pub invited_by:   String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub token:      String,
    pub group_id:   String,
    pub created_by: String,
    pub created_at: f64,
    pub expires_at: f64,
    pub used_at:    Option<f64>,
    pub used_by:    Option<String>,
}

impl Invite {
    pub fn is_valid(&self) -> bool {
        self.used_at.is_none() && now_secs() < self.expires_at
    }
}

// ── GroupStore ────────────────────────────────────────────────────────────────

pub struct GroupStore {
    db: Db,
}

impl GroupStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // ── Groups ────────────────────────────────────────────────────────────

    pub async fn create_group(&self, name: &str, description: &str, created_by: &str) -> Result<Group> {
        let id  = Uuid::new_v4().to_string();
        let now = now_secs();
        sqlx::query(
            "INSERT INTO groups (id, name, description, created_by, created_at) VALUES ($1,$2,$3,$4,$5)"
        )
        .bind(&id).bind(name).bind(description).bind(created_by).bind(now)
        .execute(&self.db)
        .await
        .context("create group")?;

        // Creator auto-joins
        self.add_member(&id, created_by, created_by).await?;

        Ok(Group { id, name: name.to_owned(), description: description.to_owned(),
                   created_by: created_by.to_owned(), created_at: now })
    }

    pub async fn get_group(&self, id: &str) -> Result<Option<Group>> {
        let row = sqlx::query_as::<_, GroupRow>("SELECT * FROM groups WHERE id = $1")
            .bind(id).fetch_optional(&self.db).await.context("get group")?;
        Ok(row.map(Group::from))
    }

    pub async fn get_group_by_name(&self, name: &str) -> Result<Option<Group>> {
        let row = sqlx::query_as::<_, GroupRow>("SELECT * FROM groups WHERE name = $1")
            .bind(name).fetch_optional(&self.db).await.context("get group by name")?;
        Ok(row.map(Group::from))
    }

    pub async fn list_groups(&self) -> Result<Vec<Group>> {
        let rows = sqlx::query_as::<_, GroupRow>("SELECT * FROM groups ORDER BY created_at DESC")
            .fetch_all(&self.db).await.context("list groups")?;
        Ok(rows.into_iter().map(Group::from).collect())
    }

    // ── Members ───────────────────────────────────────────────────────────

    pub async fn add_member(&self, group_id: &str, gateway_name: &str, invited_by: &str) -> Result<()> {
        let now = now_secs();
        sqlx::query(
            "INSERT INTO group_members (group_id, gateway_name, joined_at, invited_by) \
             VALUES ($1,$2,$3,$4)"
        )
        .bind(group_id).bind(gateway_name).bind(now).bind(invited_by)
        .execute(&self.db)
        .await
        .context("add group member")?;
        Ok(())
    }

    pub async fn list_members(&self, group_id: &str) -> Result<Vec<GroupMember>> {
        let rows = sqlx::query_as::<_, MemberRow>(
            "SELECT * FROM group_members WHERE group_id = $1 ORDER BY joined_at ASC"
        )
        .bind(group_id)
        .fetch_all(&self.db)
        .await
        .context("list members")?;
        Ok(rows.into_iter().map(GroupMember::from).collect())
    }

    pub async fn member_count(&self, group_id: &str) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT count(*) FROM group_members WHERE group_id = $1")
            .bind(group_id).fetch_one(&self.db).await.context("member count")?;
        Ok(row.0)
    }

    pub async fn is_member(&self, group_id: &str, gateway_name: &str) -> Result<bool> {
        let row: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM group_members WHERE group_id = $1 AND gateway_name = $2"
        )
        .bind(group_id).bind(gateway_name)
        .fetch_one(&self.db).await?;
        Ok(row.0 > 0)
    }

    /// Returns all groups a gateway belongs to.
    pub async fn groups_for_gateway(&self, gateway_name: &str) -> Result<Vec<Group>> {
        let rows = sqlx::query_as::<_, GroupRow>(
            "SELECT g.* FROM groups g \
             JOIN group_members m ON m.group_id = g.id \
             WHERE m.gateway_name = $1 \
             ORDER BY m.joined_at ASC"
        )
        .bind(gateway_name)
        .fetch_all(&self.db)
        .await
        .context("groups for gateway")?;
        Ok(rows.into_iter().map(Group::from).collect())
    }

    // ── Invites ───────────────────────────────────────────────────────────

    pub async fn create_invite(&self, group_id: &str, created_by: &str) -> Result<Invite> {
        let token      = random_token();
        let now        = now_secs();
        let expires_at = now + INVITE_TTL_SECS;

        sqlx::query(
            "INSERT INTO invites (token, group_id, created_by, created_at, expires_at) \
             VALUES ($1,$2,$3,$4,$5)"
        )
        .bind(&token).bind(group_id).bind(created_by).bind(now).bind(expires_at)
        .execute(&self.db)
        .await
        .context("create invite")?;

        Ok(Invite {
            token,
            group_id:   group_id.to_owned(),
            created_by: created_by.to_owned(),
            created_at: now,
            expires_at,
            used_at:    None,
            used_by:    None,
        })
    }

    /// Consume a token: validates, marks used, adds the joining gateway as a member.
    /// Returns the group the token grants access to.
    pub async fn consume_invite(&self, token: &str, joining_gateway: &str) -> Result<Group> {
        // Atomic consume: only succeeds if token is unused AND not expired.
        // Prevents TOCTOU race where two concurrent requests both pass the
        // validity check before either marks the token as used.
        let now = now_secs();
        let row: Option<(String, String)> = sqlx::query_as(
            "UPDATE invites SET used_at = $1, used_by = $2 \
             WHERE token = $3 AND used_at IS NULL AND expires_at > $4 \
             RETURNING group_id, created_by"
        )
        .bind(now)
        .bind(joining_gateway)
        .bind(token)
        .bind(now)
        .fetch_optional(&self.db)
        .await
        .context("consume invite")?;

        let (group_id, created_by) = row
            .ok_or_else(|| anyhow::anyhow!("invite token is expired or already used"))?;

        self.add_member(&group_id, joining_gateway, &created_by).await?;

        self.get_group(&group_id).await?
            .context("group not found after invite consume")
    }

    pub async fn get_invite(&self, token: &str) -> Result<Option<Invite>> {
        let row = sqlx::query_as::<_, InviteRow>("SELECT * FROM invites WHERE token = $1")
            .bind(token).fetch_optional(&self.db).await.context("get invite")?;
        Ok(row.map(Invite::from))
    }

    // ── Project ↔ Group ───────────────────────────────────────────────────

    pub async fn add_project(&self, group_id: &str, project_id: &str) -> Result<()> {
        let now = now_secs();
        sqlx::query(
            "INSERT INTO group_projects (group_id, project_id, added_at) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING"
        )
        .bind(group_id).bind(project_id).bind(now)
        .execute(&self.db).await.context("add project to group")?;
        Ok(())
    }

    pub async fn list_group_projects(&self, group_id: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT project_id FROM group_projects WHERE group_id = $1 ORDER BY added_at ASC"
        )
        .bind(group_id).fetch_all(&self.db).await.context("list group projects")?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// Gateways count for a given project (via its group membership)
    /// Used to determine contribution approval mode (vote vs PR).
    pub async fn gateway_count_for_project(&self, project_id: &str) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT count(DISTINCT m.gateway_name) \
             FROM group_members m \
             JOIN group_projects gp ON gp.group_id = m.group_id \
             WHERE gp.project_id = $1"
        )
        .bind(project_id).fetch_one(&self.db).await.context("gateway count for project")?;
        Ok(row.0)
    }
}

// ── SQLx row mappings ─────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct GroupRow { id: String, name: String, description: String, created_by: String, created_at: f64 }
impl From<GroupRow> for Group {
    fn from(r: GroupRow) -> Self {
        Self { id: r.id, name: r.name, description: r.description, created_by: r.created_by, created_at: r.created_at }
    }
}

#[derive(sqlx::FromRow)]
struct MemberRow { group_id: String, gateway_name: String, joined_at: f64, invited_by: String }
impl From<MemberRow> for GroupMember {
    fn from(r: MemberRow) -> Self {
        Self { group_id: r.group_id, gateway_name: r.gateway_name, joined_at: r.joined_at, invited_by: r.invited_by }
    }
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    token: String, group_id: String, created_by: String,
    created_at: f64, expires_at: f64, used_at: Option<f64>, used_by: Option<String>
}
impl From<InviteRow> for Invite {
    fn from(r: InviteRow) -> Self {
        Self {
            token: r.token, group_id: r.group_id, created_by: r.created_by,
            created_at: r.created_at, expires_at: r.expires_at,
            used_at: r.used_at, used_by: r.used_by,
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn store() -> GroupStore {
        GroupStore::new(db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap())
    }

    #[tokio::test]
    async fn create_group_creator_joins() {
        let s = store().await;
        let g = s.create_group("alpha", "test group", "site-a").await.unwrap();
        assert!(s.is_member(&g.id, "site-a").await.unwrap());
        assert_eq!(s.member_count(&g.id).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn invite_flow() {
        let s  = store().await;
        let g  = s.create_group("beta", "", "site-a").await.unwrap();
        let inv = s.create_invite(&g.id, "site-a").await.unwrap();
        assert!(inv.is_valid());

        let joined = s.consume_invite(&inv.token, "site-b").await.unwrap();
        assert_eq!(joined.id, g.id);
        assert!(s.is_member(&g.id, "site-b").await.unwrap());
        assert_eq!(s.member_count(&g.id).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn invite_single_use() {
        let s   = store().await;
        let g   = s.create_group("gamma", "", "site-a").await.unwrap();
        let inv = s.create_invite(&g.id, "site-a").await.unwrap();
        s.consume_invite(&inv.token, "site-b").await.unwrap();
        let err = s.consume_invite(&inv.token, "site-c").await.unwrap_err();
        assert!(err.to_string().contains("expired or already used"));
    }

    #[tokio::test]
    async fn groups_for_gateway() {
        let s  = store().await;
        let g1 = s.create_group("g1", "", "site-a").await.unwrap();
        let g2 = s.create_group("g2", "", "site-a").await.unwrap();
        s.add_member(&g2.id, "site-b", "site-a").await.unwrap();
        let gws = s.groups_for_gateway("site-b").await.unwrap();
        assert_eq!(gws.len(), 1);
        assert_eq!(gws[0].id, g2.id);
        let _ = g1; // used for setup
    }

    #[tokio::test]
    async fn gateway_count_for_project() {
        let s  = store().await;
        let g  = s.create_group("delta", "", "site-a").await.unwrap();
        s.add_member(&g.id, "site-b", "site-a").await.unwrap();
        s.add_member(&g.id, "site-c", "site-a").await.unwrap();

        // Need a project row for FK
        let db  = s.db.clone();
        let pid = uuid::Uuid::new_v4().to_string();
        let now = now_secs();
        sqlx::query(
            "INSERT INTO projects (id,name,repo_url,description,visibility,folder_path,file_glob,\
             is_active,created_at,updated_at,origin_gateway,peer_gateway,peer_project_id) \
             VALUES ($1,$2,'https://x.com','','lan-only','','**/*',0,$3,$4,'gw','','')"
        )
        .bind(&pid).bind(&pid).bind(now).bind(now)
        .execute(&db).await.unwrap();

        s.add_project(&g.id, &pid).await.unwrap();
        let count = s.gateway_count_for_project(&pid).await.unwrap();
        assert_eq!(count, 3); // site-a, site-b, site-c
    }
}
