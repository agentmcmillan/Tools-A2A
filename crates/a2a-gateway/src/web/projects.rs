/// Route handlers for project pages.

use super::{AppError, AppState};
use crate::contributions::VoteKind;
use crate::onboarding::OnboardingAnswer;
use axum::{
    extract::{Path, Query, State},
    response::Html,
    Form,
};
use minijinja::context;
use serde::Deserialize;

// ── Dashboard ─────────────────────────────────────────────────────────────────

pub async fn dashboard(State(s): State<AppState>) -> Result<Html<String>, AppError> {
    let projects  = s.projects.list().await?;
    let pending   = s.onboarding.all_pending().await;

    // Summarise each project
    let rows: Vec<_> = {
        let mut out = Vec::with_capacity(projects.len());
        for p in &projects {
            let agent_count  = s.projects.list_agents(&p.id).await.unwrap_or_default().len();
            let head_cursor  = s.task_log.head_cursor(&p.id).await.unwrap_or(0);
            out.push(minijinja::context! {
                id           => &p.id,
                name         => &p.name,
                repo_url     => &p.repo_url,
                visibility   => p.visibility.as_str(),
                is_active    => p.is_active,
                folder_path  => &p.folder_path,
                agent_count  => agent_count,
                log_entries  => head_cursor,
                onboarding   => p.onboarding_step.as_deref().unwrap_or("done"),
            });
        }
        out
    };

    let pending_rows: Vec<_> = pending.iter().map(|pr| minijinja::context! {
        project_id   => &pr.project_id,
        project_name => &pr.project_name,
        step         => &pr.step,
        message      => &pr.message,
    }).collect();

    s.render("dashboard.html", context! {
        gateway_name   => &s.gateway_name,
        projects       => rows,
        pending_prompts => pending_rows,
    })
}

// ── Project detail ────────────────────────────────────────────────────────────

pub async fn project_detail(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Html<String>, AppError> {
    let project = s.projects.get(&id).await?
        .ok_or_else(|| AppError::NotFound(format!("project {id} not found")))?;

    let agents        = s.projects.list_agents(&id).await?;
    let log_entries   = s.task_log.list(&id).await?;
    let contributions = s.contributions.list_for_project(&id).await?;
    let prompt        = s.onboarding.peek_prompt(&id).await;

    let log_rows: Vec<_> = log_entries.iter().map(|e| minijinja::context! {
        id           => &e.id,
        gateway_name => &e.gateway_name,
        agent_name   => &e.agent_name,
        entry_type   => e.entry_type.as_str(),
        content      => &e.content,
        cursor       => e.cursor,
    }).collect();

    let contrib_rows: Vec<_> = contributions.iter().map(|c| minijinja::context! {
        id               => &c.id,
        proposer_gateway => &c.proposer_gateway,
        status           => c.status.as_str(),
        description      => &c.description,
        gateway_count    => c.gateway_count,
    }).collect();

    let prompt_ctx = prompt.as_ref().map(|p| minijinja::context! {
        step          => &p.step,
        message       => &p.message,
        default_value => &p.default_value,
    });

    s.render("project.html", context! {
        gateway_name  => &s.gateway_name,
        project       => minijinja::context! {
            id           => &project.id,
            name         => &project.name,
            repo_url     => &project.repo_url,
            description  => &project.description,
            visibility   => project.visibility.as_str(),
            folder_path  => &project.folder_path,
            file_glob    => &project.file_glob,
            is_active    => project.is_active,
            onboarding   => project.onboarding_step.as_deref().unwrap_or("done"),
            peer_gateway => &project.peer_gateway,
        },
        agents        => agents,
        log_entries   => log_rows,
        contributions => contrib_rows,
        prompt        => prompt_ctx,
    })
}

// ── Onboarding ────────────────────────────────────────────────────────────────

/// htmx polling endpoint: returns the current onboarding prompt partial if one exists.
pub async fn onboard_prompt(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Html<String>, AppError> {
    let prompt = s.onboarding.peek_prompt(&id).await;
    match prompt {
        None => Ok(Html(String::new())), // empty → htmx replaces with nothing
        Some(p) => s.render("onboard_prompt.html", context! {
            project_id    => id,
            step          => p.step,
            message       => p.message,
            default_value => p.default_value,
        }),
    }
}

#[derive(Deserialize)]
pub struct OnboardForm {
    step:  String,
    value: String,
}

pub async fn onboard_answer(
    State(s):  State<AppState>,
    Path(id):  Path<String>,
    Form(form): Form<OnboardForm>,
) -> Result<Html<String>, AppError> {
    s.onboarding.answer(OnboardingAnswer {
        project_id: id.clone(),
        step:       form.step.clone(),
        value:      form.value,
    }).await?;

    // Return the updated prompt partial (or empty if onboarding complete)
    let prompt = s.onboarding.peek_prompt(&id).await;
    match prompt {
        None => Ok(Html("<p>Onboarding complete.</p>".into())),
        Some(p) => s.render("onboard_prompt.html", context! {
            project_id    => id,
            step          => p.step,
            message       => p.message,
            default_value => p.default_value,
        }),
    }
}

// ── Task log partial ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TaskCursor {
    after: Option<i64>,
}

pub async fn task_log_partial(
    State(s):     State<AppState>,
    Path(id):     Path<String>,
    Query(params): Query<TaskCursor>,
) -> Result<Html<String>, AppError> {
    let after   = params.after.unwrap_or(0);
    let entries = s.task_log.since(&id, after).await?;

    let rows: Vec<_> = entries.iter().map(|e| minijinja::context! {
        id           => &e.id,
        gateway_name => &e.gateway_name,
        agent_name   => &e.agent_name,
        entry_type   => e.entry_type.as_str(),
        content      => &e.content,
        cursor       => e.cursor,
    }).collect();

    s.render("task_log_partial.html", context! {
        entries      => rows,
        next_cursor  => entries.last().map(|e| e.cursor).unwrap_or(after),
    })
}

// ── Contributions ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ContributionForm {
    snapshot_id:  String,
    description:  String,
    agent_name:   String,
}

pub async fn open_contribution(
    State(s):   State<AppState>,
    Path(id):   Path<String>,
    Form(form): Form<ContributionForm>,
) -> Result<Html<String>, AppError> {
    let gw_count = s.groups.gateway_count_for_project(&id).await.unwrap_or(2);
    let proposal = s.contributions.open(
        &id,
        &s.gateway_name,
        &form.agent_name,
        &form.snapshot_id,
        &form.description,
        gw_count,
    ).await?;

    s.render("contribution.html", context! {
        gateway_name => &s.gateway_name,
        proposal     => minijinja::context! {
            id            => &proposal.id,
            project_id    => &proposal.project_id,
            status        => proposal.status.as_str(),
            description   => &proposal.description,
            gateway_count => proposal.gateway_count,
        },
        votes        => Vec::<minijinja::Value>::new(),
    })
}

pub async fn contribution_detail(
    State(s):   State<AppState>,
    Path((_pid, prop_id)): Path<(String, String)>,
) -> Result<Html<String>, AppError> {
    let proposal = s.contributions.get(&prop_id).await?
        .ok_or_else(|| AppError::NotFound(format!("proposal {prop_id} not found")))?;

    s.render("contribution.html", context! {
        gateway_name => &s.gateway_name,
        proposal     => minijinja::context! {
            id            => &proposal.id,
            project_id    => &proposal.project_id,
            status        => proposal.status.as_str(),
            description   => &proposal.description,
            gateway_count => proposal.gateway_count,
        },
        votes => Vec::<minijinja::Value>::new(),
    })
}

#[derive(Deserialize)]
pub struct VoteForm {
    vote:      String,  // "approve" or "reject"
    agent_name: String,
    reason:    String,
}

pub async fn cast_vote(
    State(s):   State<AppState>,
    Path((_pid, prop_id)): Path<(String, String)>,
    Form(form): Form<VoteForm>,
) -> Result<Html<String>, AppError> {
    let kind = if form.vote == "reject" { VoteKind::Reject } else { VoteKind::Approve };
    let outcome = s.contributions.cast_vote(
        &prop_id, &s.gateway_name, &form.agent_name, kind, &form.reason
    ).await?;

    let msg = match outcome {
        crate::contributions::VoteOutcome::Accepted  => "Proposal accepted by majority vote.",
        crate::contributions::VoteOutcome::Rejected  => "Proposal rejected by majority vote.",
        crate::contributions::VoteOutcome::Pending { .. } => "Vote recorded. Waiting for majority.",
    };
    Ok(Html(format!("<p>{msg}</p>")))
}

#[derive(Deserialize)]
pub struct ReviewForm {
    action: String, // "accept" or "reject"
}

pub async fn review_pr(
    State(s):   State<AppState>,
    Path((_pid, prop_id)): Path<(String, String)>,
    Form(form): Form<ReviewForm>,
) -> Result<Html<String>, AppError> {
    if form.action == "accept" {
        s.contributions.accept_pr(&prop_id, &s.gateway_name).await?;
        Ok(Html("<p>PR accepted. Contribution merged.</p>".into()))
    } else {
        s.contributions.reject_pr(&prop_id, &s.gateway_name).await?;
        Ok(Html("<p>PR rejected.</p>".into()))
    }
}
