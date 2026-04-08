/// Phase 2 integration tests — Groups + Contributions
///
/// Tests the GroupStore and ContributionStore data layers.

mod helpers;

use a2a_gateway::{
    db::{self, Db},
    groups::GroupStore,
    contributions::{ContributionStore, VoteKind, VoteOutcome},
    projects::ProjectStore,
};

async fn stores() -> (GroupStore, ContributionStore, ProjectStore, Db) {
    let db = db::open(":memory:").await.unwrap();
    (
        GroupStore::new(db.clone()),
        ContributionStore::new(db.clone()),
        ProjectStore::new(db.clone()),
        db,
    )
}

/// Create a minimal project row and a stub snapshot row, return (project_id, snapshot_id).
async fn setup_project_and_snapshot(db: &Db) -> (String, String) {
    let projects  = ProjectStore::new(db.clone());
    let pid = projects.create("test-proj", "https://x.com", "", "site-a", "", "").await.unwrap().id;

    let sid = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    sqlx::query(
        "INSERT INTO snapshots (id, project_id, gateway, transcript, created_at) VALUES (?,?,?,?,?)"
    )
    .bind(&sid).bind(&pid).bind("site-a").bind("").bind(now)
    .execute(db).await.unwrap();

    (pid, sid)
}

// ── Groups ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_group_creator_auto_joins() {
    let (groups, _, _, _) = stores().await;

    let g       = groups.create_group("team-alpha", "", "site-a").await.unwrap();
    let members = groups.list_members(&g.id).await.unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].gateway_name, "site-a");
}

#[tokio::test]
async fn list_groups_empty_initially() {
    let (groups, _, _, _) = stores().await;
    let list = groups.list_groups().await.unwrap();
    assert!(list.is_empty());
}

#[tokio::test]
async fn list_groups_after_create() {
    let (groups, _, _, _) = stores().await;

    groups.create_group("alpha", "", "site-a").await.unwrap();
    groups.create_group("bravo", "", "site-b").await.unwrap();

    let list = groups.list_groups().await.unwrap();
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn invite_flow_join_and_membership() {
    let (groups, _, _, _) = stores().await;

    let g   = groups.create_group("collab", "", "site-a").await.unwrap();
    let inv = groups.create_invite(&g.id, "site-a").await.unwrap();
    groups.consume_invite(&inv.token, "site-b").await.unwrap();

    let members = groups.list_members(&g.id).await.unwrap();
    assert_eq!(members.len(), 2);
    let names: Vec<&str> = members.iter().map(|m| m.gateway_name.as_str()).collect();
    assert!(names.contains(&"site-a"));
    assert!(names.contains(&"site-b"));
}

#[tokio::test]
async fn invite_single_use_enforcement() {
    let (groups, _, _, _) = stores().await;

    let g   = groups.create_group("collab", "", "site-a").await.unwrap();
    let inv = groups.create_invite(&g.id, "site-a").await.unwrap();

    groups.consume_invite(&inv.token, "site-b").await.unwrap();
    let result = groups.consume_invite(&inv.token, "site-c").await;
    assert!(result.is_err(), "invite token should be single-use");
}

#[tokio::test]
async fn invalid_invite_token_rejected() {
    let (groups, _, _, _) = stores().await;
    let result = groups.consume_invite("not-a-real-token", "site-x").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn groups_for_gateway() {
    let (groups, _, _, _) = stores().await;

    let _g1 = groups.create_group("g1", "", "site-a").await.unwrap();
    let g2  = groups.create_group("g2", "", "site-b").await.unwrap();
    let inv = groups.create_invite(&g2.id, "site-b").await.unwrap();
    groups.consume_invite(&inv.token, "site-a").await.unwrap();

    // site-a created g1 (auto-joined) and joined g2 via invite
    let site_a_groups = groups.groups_for_gateway("site-a").await.unwrap();
    assert_eq!(site_a_groups.len(), 2);
}

#[tokio::test]
async fn gateway_count_for_project() {
    let (groups, _, projects, db) = stores().await;

    let pid = projects.create("myproj", "https://x.com", "", "site-a", "", "").await.unwrap().id;

    let g  = groups.create_group("team", "", "site-a").await.unwrap();
    let t1 = groups.create_invite(&g.id, "site-a").await.unwrap();
    groups.consume_invite(&t1.token, "site-b").await.unwrap();
    groups.add_project(&g.id, &pid).await.unwrap();

    let count = groups.gateway_count_for_project(&pid).await.unwrap();
    assert_eq!(count, 2);

    let _ = db; // hold DB alive
}

// ── Contributions ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn two_gateway_project_opens_pr_pending() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Add auth module", 2).await.unwrap();
    assert_eq!(p.status.as_str(), "pr_pending");
}

#[tokio::test]
async fn three_gateway_project_opens_voting() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Add rate limiting", 3).await.unwrap();
    assert_eq!(p.status.as_str(), "voting");
}

#[tokio::test]
async fn voting_majority_accepts() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Feature X", 3).await.unwrap();

    let o1 = contributions.cast_vote(&p.id, "site-b", "agent-b", VoteKind::Approve, "looks good").await.unwrap();
    assert!(matches!(o1, VoteOutcome::Pending { .. }), "1 approval not majority of 3");

    let o2 = contributions.cast_vote(&p.id, "site-c", "agent-c", VoteKind::Approve, "approve").await.unwrap();
    assert!(matches!(o2, VoteOutcome::Accepted));

    let updated = contributions.get(&p.id).await.unwrap().unwrap();
    assert_eq!(updated.status.as_str(), "accepted");
}

#[tokio::test]
async fn voting_majority_rejects() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Feature Y", 3).await.unwrap();

    contributions.cast_vote(&p.id, "site-b", "agent-b", VoteKind::Reject, "nope").await.unwrap();
    let o = contributions.cast_vote(&p.id, "site-c", "agent-c", VoteKind::Reject, "also no").await.unwrap();
    assert!(matches!(o, VoteOutcome::Rejected));

    let updated = contributions.get(&p.id).await.unwrap().unwrap();
    assert_eq!(updated.status.as_str(), "rejected");
}

#[tokio::test]
async fn pr_flow_accept() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Add auth", 2).await.unwrap();
    contributions.accept_pr(&p.id, "site-b").await.unwrap();

    let updated = contributions.get(&p.id).await.unwrap().unwrap();
    assert_eq!(updated.status.as_str(), "accepted");
}

#[tokio::test]
async fn pr_flow_reject() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Add auth", 2).await.unwrap();
    contributions.reject_pr(&p.id, "site-b").await.unwrap();

    let updated = contributions.get(&p.id).await.unwrap().unwrap();
    assert_eq!(updated.status.as_str(), "rejected");
}

#[tokio::test]
async fn cannot_vote_on_pr_pending_proposal() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    let p = contributions.open(&pid, "site-a", "agent-x", &sid, "Feature", 2).await.unwrap();
    let result = contributions.cast_vote(&p.id, "site-b", "agent-y", VoteKind::Approve, "yes").await;
    assert!(result.is_err(), "cannot vote on pr_pending proposal");
}

#[tokio::test]
async fn proposals_listed_for_project() {
    let (_, contributions, _, db) = stores().await;
    let (pid, sid) = setup_project_and_snapshot(&db).await;

    contributions.open(&pid, "site-a", "agent", &sid, "Proposal A", 2).await.unwrap();
    contributions.open(&pid, "site-a", "agent", &sid, "Proposal B", 3).await.unwrap();

    // Different project — should not appear in list for pid
    let (pid2, sid2) = setup_project_and_snapshot(&db).await;
    contributions.open(&pid2, "site-a", "agent", &sid2, "Other", 2).await.unwrap();

    let list = contributions.list_for_project(&pid).await.unwrap();
    assert_eq!(list.len(), 2);
}
