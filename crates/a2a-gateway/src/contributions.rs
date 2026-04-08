/// Contribution proposals — PR-style gated approval for project file updates.
///
/// Approval rules (evaluated at accept time):
///
///   ┌──────────────────────────────────────────────────────────┐
///   │ gateway_count > 2 │  Agents vote; majority wins          │
///   │ gateway_count == 2│  Requires PR review by origin owner  │
///   └──────────────────────────────────────────────────────────┘
///
/// State machine:
///
///   open ──► voting      (when gateway_count > 2: auto-transition)
///        └─► pr_pending  (when gateway_count == 2: auto-transition)
///
///   voting   ──► accepted / rejected  (majority vote reached)
///   pr_pending ──► pr_accepted        (origin owner approves)
///              └─► rejected           (origin owner rejects)
///   pr_accepted ──► accepted          (auto on pr_accepted)

use crate::db::Db;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::util::now_secs;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Open,
    Voting,
    PrPending,
    PrAccepted,
    Accepted,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open       => "open",
            Self::Voting     => "voting",
            Self::PrPending  => "pr_pending",
            Self::PrAccepted => "pr_accepted",
            Self::Accepted   => "accepted",
            Self::Rejected   => "rejected",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "voting"      => Self::Voting,
            "pr_pending"  => Self::PrPending,
            "pr_accepted" => Self::PrAccepted,
            "accepted"    => Self::Accepted,
            "rejected"    => Self::Rejected,
            _             => Self::Open,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id:               String,
    pub project_id:       String,
    pub proposer_gateway: String,
    pub proposer_agent:   String,
    pub snapshot_id:      String,
    pub description:      String,
    pub status:           ProposalStatus,
    pub gateway_count:    i64,
    pub created_at:       f64,
    pub resolved_at:      Option<f64>,
    pub resolved_by:      Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub id:           String,
    pub proposal_id:  String,
    pub gateway_name: String,
    pub agent_name:   String,
    pub vote:         VoteKind,
    pub reason:       String,
    pub voted_at:     f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteKind { Approve, Reject }
impl VoteKind {
    pub fn as_str(&self) -> &'static str { match self { Self::Approve => "approve", Self::Reject => "reject" } }
    pub fn from_str(s: &str) -> Self { if s == "reject" { Self::Reject } else { Self::Approve } }
}

// ── ContributionStore ─────────────────────────────────────────────────────────

pub struct ContributionStore {
    db: Db,
}

impl ContributionStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // ── Proposals ─────────────────────────────────────────────────────────

    /// Open a new proposal. gateway_count is read from the caller (should be
    /// queried from GroupStore::gateway_count_for_project before calling).
    pub async fn open(
        &self,
        project_id:       &str,
        proposer_gateway: &str,
        proposer_agent:   &str,
        snapshot_id:      &str,
        description:      &str,
        gateway_count:    i64,
    ) -> Result<Proposal> {
        let id  = Uuid::new_v4().to_string();
        let now = now_secs();

        // Auto-assign initial status based on gateway count
        let status = if gateway_count > 2 {
            ProposalStatus::Voting
        } else {
            ProposalStatus::PrPending
        };

        sqlx::query(
            "INSERT INTO contribution_proposals \
             (id, project_id, proposer_gateway, proposer_agent, snapshot_id, description, \
              status, gateway_count, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)"
        )
        .bind(&id).bind(project_id).bind(proposer_gateway).bind(proposer_agent)
        .bind(snapshot_id).bind(description).bind(status.as_str())
        .bind(gateway_count).bind(now)
        .execute(&self.db)
        .await
        .context("open proposal")?;

        Ok(Proposal {
            id, project_id: project_id.to_owned(),
            proposer_gateway: proposer_gateway.to_owned(),
            proposer_agent: proposer_agent.to_owned(),
            snapshot_id: snapshot_id.to_owned(),
            description: description.to_owned(),
            status, gateway_count, created_at: now,
            resolved_at: None, resolved_by: None,
        })
    }

    pub async fn get(&self, id: &str) -> Result<Option<Proposal>> {
        let row = sqlx::query_as::<_, PropRow>(
            "SELECT * FROM contribution_proposals WHERE id = $1"
        )
        .bind(id).fetch_optional(&self.db).await.context("get proposal")?;
        Ok(row.map(Proposal::from))
    }

    pub async fn list_for_project(&self, project_id: &str) -> Result<Vec<Proposal>> {
        let rows = sqlx::query_as::<_, PropRow>(
            "SELECT * FROM contribution_proposals WHERE project_id = $1 ORDER BY created_at DESC"
        )
        .bind(project_id).fetch_all(&self.db).await.context("list proposals")?;
        Ok(rows.into_iter().map(Proposal::from).collect())
    }

    // ── Voting (gateway_count > 2) ────────────────────────────────────────

    pub async fn cast_vote(
        &self,
        proposal_id:  &str,
        gateway_name: &str,
        agent_name:   &str,
        vote:         VoteKind,
        reason:       &str,
    ) -> Result<VoteOutcome> {
        let proposal = self.get(proposal_id).await?.context("proposal not found")?;
        if proposal.status != ProposalStatus::Voting {
            bail!("proposal is not in voting state (status={})", proposal.status.as_str());
        }

        let id  = Uuid::new_v4().to_string();
        let now = now_secs();

        sqlx::query(
            "INSERT INTO contribution_votes \
             (id, proposal_id, gateway_name, agent_name, vote, reason, voted_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (proposal_id, gateway_name, agent_name) DO NOTHING"
        )
        .bind(&id).bind(proposal_id).bind(gateway_name).bind(agent_name)
        .bind(vote.as_str()).bind(reason).bind(now)
        .execute(&self.db)
        .await
        .context("insert vote")?;

        // Check if majority reached
        self.check_vote_outcome(proposal_id, &proposal).await
    }

    async fn check_vote_outcome(&self, proposal_id: &str, proposal: &Proposal) -> Result<VoteOutcome> {
        let total = proposal.gateway_count;

        let (approvals, rejections): (i64, i64) = {
            let row: (i64, i64) = sqlx::query_as(
                "SELECT \
                   SUM(CASE WHEN vote='approve' THEN 1 ELSE 0 END), \
                   SUM(CASE WHEN vote='reject'  THEN 1 ELSE 0 END) \
                 FROM contribution_votes WHERE proposal_id = $1"
            )
            .bind(proposal_id).fetch_one(&self.db).await.context("tally votes")?;
            row
        };

        let majority = (total / 2) + 1;
        if approvals >= majority {
            self.resolve(proposal_id, ProposalStatus::Accepted, "vote-majority").await?;
            Ok(VoteOutcome::Accepted)
        } else if rejections >= majority {
            self.resolve(proposal_id, ProposalStatus::Rejected, "vote-majority").await?;
            Ok(VoteOutcome::Rejected)
        } else {
            Ok(VoteOutcome::Pending { approvals, rejections, needed: majority })
        }
    }

    // ── PR flow (gateway_count == 2) ──────────────────────────────────────

    pub async fn accept_pr(&self, proposal_id: &str, reviewer_gateway: &str) -> Result<()> {
        let proposal = self.get(proposal_id).await?.context("proposal not found")?;
        if proposal.status != ProposalStatus::PrPending {
            bail!("proposal is not in pr_pending state");
        }
        self.set_status(proposal_id, ProposalStatus::PrAccepted).await?;
        self.resolve(proposal_id, ProposalStatus::Accepted, reviewer_gateway).await?;
        Ok(())
    }

    pub async fn reject_pr(&self, proposal_id: &str, reviewer_gateway: &str) -> Result<()> {
        let proposal = self.get(proposal_id).await?.context("proposal not found")?;
        if proposal.status != ProposalStatus::PrPending {
            bail!("proposal is not in pr_pending state");
        }
        self.resolve(proposal_id, ProposalStatus::Rejected, reviewer_gateway).await?;
        Ok(())
    }

    // ── Private ───────────────────────────────────────────────────────────

    async fn set_status(&self, id: &str, status: ProposalStatus) -> Result<()> {
        sqlx::query("UPDATE contribution_proposals SET status = $1 WHERE id = $2")
            .bind(status.as_str()).bind(id)
            .execute(&self.db).await.context("set proposal status")?;
        Ok(())
    }

    async fn resolve(&self, id: &str, status: ProposalStatus, resolved_by: &str) -> Result<()> {
        let now = now_secs();
        sqlx::query(
            "UPDATE contribution_proposals \
             SET status = $1, resolved_at = $2, resolved_by = $3 WHERE id = $4"
        )
        .bind(status.as_str()).bind(now).bind(resolved_by).bind(id)
        .execute(&self.db).await.context("resolve proposal")?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum VoteOutcome {
    Accepted,
    Rejected,
    Pending { approvals: i64, rejections: i64, needed: i64 },
}

// ── SQLx row mapping ──────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct PropRow {
    id: String, project_id: String,
    proposer_gateway: String, proposer_agent: String,
    snapshot_id: String, description: String, status: String,
    gateway_count: i64, created_at: f64,
    resolved_at: Option<f64>, resolved_by: Option<String>,
}
impl From<PropRow> for Proposal {
    fn from(r: PropRow) -> Self {
        Self {
            id: r.id, project_id: r.project_id,
            proposer_gateway: r.proposer_gateway, proposer_agent: r.proposer_agent,
            snapshot_id: r.snapshot_id, description: r.description,
            status: ProposalStatus::from_str(&r.status),
            gateway_count: r.gateway_count, created_at: r.created_at,
            resolved_at: r.resolved_at, resolved_by: r.resolved_by,
        }
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn store_with_project() -> (ContributionStore, String) {
        let db  = db::open(&std::env::var("DATABASE_URL").expect("DATABASE_URL required for tests")).await.unwrap();
        let cs  = ContributionStore::new(db.clone());
        let pid = Uuid::new_v4().to_string();
        let sid = Uuid::new_v4().to_string();
        let now = now_secs();

        // Insert a minimal project row
        sqlx::query(
            "INSERT INTO projects \
             (id,name,repo_url,description,visibility,folder_path,file_glob,\
              is_active,created_at,updated_at,origin_gateway,peer_gateway,peer_project_id) \
             VALUES ($1,$2,'https://x.com','','lan-only','','**/*',0,$3,$4,'gw','','')"
        )
        .bind(&pid).bind(&pid).bind(now).bind(now)
        .execute(&db).await.unwrap();

        // Insert a stub snapshot row
        sqlx::query(
            "INSERT INTO snapshots (id, project_id, gateway, transcript, created_at) \
             VALUES ($1,$2,$3,$4,$5)"
        )
        .bind(&sid).bind(&pid).bind("gw").bind("").bind(now)
        .execute(&db).await.unwrap();

        (cs, pid)
    }

    async fn open_proposal(cs: &ContributionStore, pid: &str, gw_count: i64) -> Proposal {
        let sid = {
            let row: (String,) = sqlx::query_as("SELECT id FROM snapshots WHERE project_id = $1")
                .bind(pid).fetch_one(&cs.db).await.unwrap();
            row.0
        };
        cs.open(pid, "site-b", "agent", &sid, "add feature X", gw_count).await.unwrap()
    }

    #[tokio::test]
    async fn pr_flow_two_gateways() {
        let (cs, pid) = store_with_project().await;
        let proposal  = open_proposal(&cs, &pid, 2).await;
        assert_eq!(proposal.status, ProposalStatus::PrPending);

        cs.accept_pr(&proposal.id, "site-a").await.unwrap();
        let done = cs.get(&proposal.id).await.unwrap().unwrap();
        assert_eq!(done.status, ProposalStatus::Accepted);
        assert_eq!(done.resolved_by.as_deref(), Some("site-a"));
    }

    #[tokio::test]
    async fn pr_reject() {
        let (cs, pid) = store_with_project().await;
        let proposal  = open_proposal(&cs, &pid, 2).await;
        cs.reject_pr(&proposal.id, "site-a").await.unwrap();
        let done = cs.get(&proposal.id).await.unwrap().unwrap();
        assert_eq!(done.status, ProposalStatus::Rejected);
    }

    #[tokio::test]
    async fn voting_majority_accepts() {
        let (cs, pid) = store_with_project().await;
        let proposal  = open_proposal(&cs, &pid, 3).await;
        assert_eq!(proposal.status, ProposalStatus::Voting);

        // 2 of 3 approve → accepted
        cs.cast_vote(&proposal.id, "site-a", "orch-a", VoteKind::Approve, "lgtm").await.unwrap();
        let outcome = cs.cast_vote(&proposal.id, "site-b", "orch-b", VoteKind::Approve, "good").await.unwrap();
        assert!(matches!(outcome, VoteOutcome::Accepted));
        let done = cs.get(&proposal.id).await.unwrap().unwrap();
        assert_eq!(done.status, ProposalStatus::Accepted);
    }

    #[tokio::test]
    async fn voting_majority_rejects() {
        let (cs, pid) = store_with_project().await;
        let proposal  = open_proposal(&cs, &pid, 3).await;
        cs.cast_vote(&proposal.id, "site-a", "a", VoteKind::Reject, "").await.unwrap();
        let out = cs.cast_vote(&proposal.id, "site-b", "b", VoteKind::Reject, "").await.unwrap();
        assert!(matches!(out, VoteOutcome::Rejected));
    }

    #[tokio::test]
    async fn voting_pending_until_majority() {
        let (cs, pid) = store_with_project().await;
        let proposal  = open_proposal(&cs, &pid, 4).await;
        let out = cs.cast_vote(&proposal.id, "site-a", "a", VoteKind::Approve, "").await.unwrap();
        assert!(matches!(out, VoteOutcome::Pending { .. }));
        let p = cs.get(&proposal.id).await.unwrap().unwrap();
        assert_eq!(p.status, ProposalStatus::Voting); // still open
    }

    #[tokio::test]
    async fn cannot_vote_on_non_voting_proposal() {
        let (cs, pid) = store_with_project().await;
        let proposal  = open_proposal(&cs, &pid, 2).await; // pr_pending, not voting
        let err = cs.cast_vote(&proposal.id, "site-a", "a", VoteKind::Approve, "").await.unwrap_err();
        assert!(err.to_string().contains("not in voting state"));
    }
}
