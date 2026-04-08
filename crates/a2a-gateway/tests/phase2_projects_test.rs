/// Phase 2 integration tests — Projects + Visibility

mod helpers;

use a2a_gateway::{
    db,
    projects::{ProjectStore, Visibility},
};

async fn store() -> (ProjectStore, a2a_gateway::db::Db) {
    let db = db::open(":memory:").await.unwrap();
    (ProjectStore::new(db.clone()), db)
}

fn new_proj<'a>(store: &'a ProjectStore, name: &'a str) -> impl std::future::Future<Output = String> + 'a {
    async move {
        store.create(name, "https://x.com", "desc", "site-a", "", "").await.unwrap().id
    }
}

#[tokio::test]
async fn create_and_get_project() {
    let (store, _) = store().await;
    let id = new_proj(&store, "my-project").await;
    let p  = store.get(&id).await.unwrap().expect("project should exist");

    assert_eq!(p.name, "my-project");
    assert_eq!(p.repo_url, "https://x.com");
    assert_eq!(p.visibility, Visibility::LanOnly);
    assert!(!p.is_active);
    assert!(p.onboarding_step.is_none(), "local project has no onboarding step");
}

#[tokio::test]
async fn peer_project_starts_onboarding() {
    let (store, _) = store().await;
    let id = store.create("peer-proj", "https://x.com", "", "site-a", "site-b", "r1")
        .await.unwrap().id;
    let p = store.get(&id).await.unwrap().unwrap();
    assert_eq!(p.onboarding_step.as_deref(), Some("awaiting_folder"));
}

#[tokio::test]
async fn list_returns_all_projects() {
    let (store, _) = store().await;
    new_proj(&store, "alpha").await;
    new_proj(&store, "bravo").await;
    let list = store.list().await.unwrap();
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn get_by_name() {
    let (store, _) = store().await;
    new_proj(&store, "named-proj").await;
    let p = store.get_by_name("named-proj").await.unwrap().expect("find by name");
    assert_eq!(p.name, "named-proj");
}

#[tokio::test]
async fn visibility_lanonly_to_group() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;
    store.set_visibility(&id, Visibility::Group).await.unwrap();
    let p = store.get(&id).await.unwrap().unwrap();
    assert_eq!(p.visibility, Visibility::Group);
}

#[tokio::test]
async fn visibility_group_to_public() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;
    store.set_visibility(&id, Visibility::Group).await.unwrap();
    store.set_visibility(&id, Visibility::Public).await.unwrap();
    let p = store.get(&id).await.unwrap().unwrap();
    assert_eq!(p.visibility, Visibility::Public);
}

#[tokio::test]
async fn cannot_skip_visibility_step() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;
    // LanOnly → Public is not a valid single hop
    let res = store.set_visibility(&id, Visibility::Public).await;
    assert!(res.is_err(), "should reject invalid visibility transition");
}

#[tokio::test]
async fn cannot_skip_visibility_step() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;
    // LanOnly → Public is not a valid single hop
    let res = store.set_visibility(&id, Visibility::Public).await;
    assert!(res.is_err(), "should reject invalid visibility transition");
}

#[tokio::test]
async fn cannot_change_visibility_while_active() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;
    store.set_visibility(&id, Visibility::Group).await.unwrap();
    store.set_active(&id, true).await.unwrap();
    let res = store.set_visibility(&id, Visibility::Public).await;
    assert!(res.is_err(), "active projects cannot change visibility");
}

#[tokio::test]
async fn onboarding_step_progression() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;

    for step in &[Some("awaiting_folder"), Some("awaiting_files"), Some("building_context"), Some("awaiting_user_input"), None] {
        store.set_onboarding_step(&id, *step).await.unwrap();
        let p = store.get(&id).await.unwrap().unwrap();
        assert_eq!(p.onboarding_step.as_deref(), *step);
    }
}

#[tokio::test]
async fn agent_membership() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;

    store.add_agent("orchestrator", &id).await.unwrap();
    store.add_agent("research", &id).await.unwrap();

    let agents = store.list_agents(&id).await.unwrap();
    assert_eq!(agents.len(), 2);
    assert!(agents.contains(&"orchestrator".to_string()));
    assert!(agents.contains(&"research".to_string()));
}

#[tokio::test]
async fn add_agent_idempotent() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;

    store.add_agent("orchestrator", &id).await.unwrap();
    store.add_agent("orchestrator", &id).await.unwrap(); // INSERT OR IGNORE

    let agents = store.list_agents(&id).await.unwrap();
    assert_eq!(agents.len(), 1);
}

#[tokio::test]
async fn set_folder_and_glob() {
    let (store, _) = store().await;
    let id = new_proj(&store, "p").await;

    store.set_folder_path(&id, "/tmp/myproject").await.unwrap();
    store.set_file_glob(&id, "**/*.rs").await.unwrap();

    let p = store.get(&id).await.unwrap().unwrap();
    assert_eq!(p.folder_path, "/tmp/myproject");
    assert_eq!(p.file_glob, "**/*.rs");
}
