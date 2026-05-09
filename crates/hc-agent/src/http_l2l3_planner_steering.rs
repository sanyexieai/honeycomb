//! Optional HTTP L2/L3 **planner natural-language steering** (workbench parity hook).
//!
//! When enabled via **`HC_HTTP_CHAT_L2L3_PLANNER_STEER`**, `hc-service` may call
//! [`maybe_apply_http_l2l3_planner_steering`] **after** swarm observability / implicit task allocation
//! and **before** the L2/L3 task-plan excerpt is appended — so the same HTTP round can see planner
//! draft updates on disk.
//!
//! This does **not** replace full `AgentOrchestrator` / workbench UI flows (no channel nomination,
//! no assignment-phase agent materialization from HTTP). It only merges structured planner JSON
//! into the persisted [`crate::planning::TaskPlan`] using the same prompt template as the UI.

use std::path::Path;

use anyhow::{Context, Result, bail};
use hc_bootstrap::wall_clock_ms;
use hc_context::load_agent_planner_input_prompt;
use hc_core::{RuntimeNamespace, RuntimeSupervisor};
use hc_llm::{ChatMessage, GenerateRequest, MessageRole, ModelRef, ProviderRegistry};
use hc_responder::{ReplyRequest, ReplyResponse, ResponderBinding};
use hc_store::store::WorkspaceNamespace;
use serde::Deserialize;

use crate::TaskRequest;
use crate::bootstrap::{
    bootstrap_task_preset_from_env, bootstrap_task_with_preset, materialize_plan,
};
use crate::persistence::{
    load_task_coordination_bundle_for_journal_updates, persist_plan_note_artifact_v1,
    persist_task_artifacts_with_in_memory_prune,
};
use crate::planning::{TaskPlan, TaskPlanStatus};

#[derive(Debug, Clone, Deserialize)]
struct PlannerDraft {
    notes: Vec<String>,
    work_items: Vec<PlannerWorkItem>,
    agent_proposals: Vec<PlannerAgentProposal>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlannerWorkItem {
    stage: String,
    title: String,
    goal: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PlannerAgentProposal {
    role: String,
    reason: String,
}

fn env_flag_truthy(raw: Option<String>) -> bool {
    raw.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

#[must_use]
pub fn http_l2l3_planner_steering_enabled_from_env() -> bool {
    env_flag_truthy(std::env::var("HC_HTTP_CHAT_L2L3_PLANNER_STEER").ok())
}

fn render_planner_input_body(
    store_ns: &WorkspaceNamespace,
    task: &TaskRequest,
    plan: &TaskPlan,
    user_input: &str,
) -> Result<String> {
    let planning_notes = plan.planning_notes.join("\n");
    let work_items = plan
        .work_items
        .iter()
        .map(|item| format!("{} | {} | {}", item.stage, item.title, item.goal))
        .collect::<Vec<_>>()
        .join("\n");
    let agent_proposals = plan
        .agent_proposals
        .iter()
        .map(|proposal| format!("{} | {}", proposal.role, proposal.reason))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(load_agent_planner_input_prompt(store_ns)?
        .replace("{{task_title}}", &task.title)
        .replace("{{task_goal}}", &task.goal)
        .replace("{{plan_status}}", &format!("{:?}", plan.status))
        .replace("{{planning_notes}}", &planning_notes)
        .replace("{{work_items}}", &work_items)
        .replace("{{agent_proposals}}", &agent_proposals)
        .replace("{{user_input}}", user_input.trim()))
}

fn steering_generate_reply(
    registry: &ProviderRegistry,
    request: &ReplyRequest,
) -> Result<ReplyResponse> {
    match &request.responder {
        ResponderBinding::Llm(llm) => {
            let mut messages = Vec::new();
            if let Some(system_prompt) = &llm.system_prompt {
                messages.push(ChatMessage::new(MessageRole::System, system_prompt.clone()));
            }
            messages.push(ChatMessage::new(
                MessageRole::User,
                request.source_body.clone(),
            ));
            let response = registry
                .generate(&GenerateRequest::new(
                    ModelRef::new(llm.provider.clone(), llm.model.clone()),
                    messages,
                ))
                .map_err(|e| anyhow::anyhow!(e))?;
            Ok(ReplyResponse::new(response.message.content))
        }
        ResponderBinding::Human(_) => {
            bail!("planner responder is human; HTTP L2/L3 steering requires LLM or rule planner");
        }
        ResponderBinding::Rule(config) => {
            let profile = config.profile.as_deref().unwrap_or("default");
            Ok(ReplyResponse::new(format!(
                "[rule:{profile}] {}",
                request.source_body.trim()
            )))
        }
        ResponderBinding::Script(_) => {
            bail!("planner script responder is not supported for HTTP L2/L3 steering");
        }
    }
}

fn parse_planner_draft(body: &str) -> Result<PlannerDraft> {
    let trimmed = body.trim();
    serde_json::from_str::<PlannerDraft>(trimmed)
        .with_context(|| "planner returned invalid structured JSON draft")
}

fn apply_planner_draft_to_task_plan(plan: &mut TaskPlan, draft: &PlannerDraft) -> Result<()> {
    if draft.notes.is_empty() && draft.work_items.is_empty() && draft.agent_proposals.is_empty() {
        bail!("planner returned an empty draft");
    }
    for note in &draft.notes {
        plan.add_note(note.clone());
    }
    for item in &draft.work_items {
        plan.add_work_item(item.stage.clone(), item.title.clone(), item.goal.clone());
    }
    for proposal in &draft.agent_proposals {
        plan.add_agent_proposal(proposal.role.clone(), proposal.reason.clone());
    }
    if !draft.work_items.is_empty()
        && matches!(
            plan.status,
            TaskPlanStatus::AwaitingPlannerInput | TaskPlanStatus::Drafted
        )
    {
        plan.approve();
    }
    Ok(())
}

fn summarize_planner_draft_for_plan_note(draft: &PlannerDraft) -> (String, Option<String>) {
    let summary = if let Some(first) = draft.notes.first().map(|value| value.trim()) {
        if first.is_empty() {
            "Planner steering updated task plan".to_owned()
        } else {
            format!("Planner steering: {first}")
        }
    } else {
        format!(
            "Planner steering updated task plan ({} work item(s), {} proposal(s))",
            draft.work_items.len(),
            draft.agent_proposals.len()
        )
    };

    let details = Some(format!(
        "notes={}; work_items={}; agent_proposals={}",
        draft.notes.len(),
        draft.work_items.len(),
        draft.agent_proposals.len()
    ));
    (summary, details)
}

/// Loads coordination `task_plan.md`, runs ephemeral **materialized planner** LLM (same bootstrap preset
/// as env), merges JSON draft into the **disk-backed** plan, then persists.
pub fn maybe_apply_http_l2l3_planner_steering(
    workspace_root: &Path,
    store_ns: &WorkspaceNamespace,
    task_id: &str,
    user_text: &str,
) -> Result<()> {
    let tid = task_id.trim();
    if tid.is_empty() {
        return Ok(());
    }
    let trimmed_user = user_text.trim();
    if trimmed_user.is_empty() {
        return Ok(());
    }

    let Some((task, mut plan)) =
        load_task_coordination_bundle_for_journal_updates(workspace_root, store_ns, tid)?
    else {
        return Ok(());
    };

    let mut runtime = RuntimeSupervisor::new();
    let rt_ns = RuntimeNamespace::new(store_ns.tenant_id.clone(), store_ns.user_id.clone());
    let session = runtime.create_session_in_namespace(
        format!("http-planner-steer.{}.{}", tid, wall_clock_ms()),
        rt_ns,
    );
    let agent_plan = bootstrap_task_with_preset(&task, bootstrap_task_preset_from_env());
    let outcome = materialize_plan(&mut runtime, &session.id, &agent_plan)
        .context("materialize_plan for HTTP L2/L3 planner steering")?;
    let planner = outcome
        .agents
        .iter()
        .find(|a| a.persona.role == "planner")
        .context("no planner role in materialized bootstrap plan")?;
    let responder = planner
        .binding
        .responder
        .clone()
        .context("planner responder not bound")?;

    let source_body = render_planner_input_body(store_ns, &task, &plan, trimmed_user)?;
    let registry = hc_llm::default_registry_from_env();
    let request = ReplyRequest {
        source_message_id: format!("http-planner-steer.{}", wall_clock_ms()),
        source_session_id: session.id.clone(),
        source_from_instance_id: "user.http.chat".to_owned(),
        source_body,
        replying_instance_id: planner.binding.instance_id.clone(),
        replying_agent_name: planner.persona.name.clone(),
        replying_role: planner.persona.role.clone(),
        responder,
    };

    let response = steering_generate_reply(&registry, &request)?;
    let draft = parse_planner_draft(&response.body)?;
    apply_planner_draft_to_task_plan(&mut plan, &draft)?;
    persist_task_artifacts_with_in_memory_prune(workspace_root, &task, &mut plan)?;
    let (summary, details) = summarize_planner_draft_for_plan_note(&draft);
    if let Err(error) = persist_plan_note_artifact_v1(
        workspace_root,
        store_ns,
        &task,
        None,
        summary,
        details,
        format!("http_planner_steering:{}", planner.binding.instance_id),
    ) {
        tracing::warn!(
            ?error,
            task_id = %tid,
            "persist ADR-005 plan_note artifact (non-fatal)"
        );
    }

    tracing::info!(
        task_id = %tid,
        notes = draft.notes.len(),
        work_items = draft.work_items.len(),
        agent_proposals = draft.agent_proposals.len(),
        "HTTP L2/L3 planner steering persisted draft to task_plan"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskNamespace;

    #[test]
    fn env_flag_truthy_parsing() {
        assert!(!env_flag_truthy(None));
        assert!(env_flag_truthy(Some(" TRUE ".into())));
        assert!(env_flag_truthy(Some("on".into())));
        assert!(!env_flag_truthy(Some("0".into())));
        assert!(!env_flag_truthy(Some("".into())));
    }

    #[test]
    fn apply_draft_approves_when_work_items_present() {
        let task = TaskRequest::new("t1", "Title", "Goal")
            .with_namespace(TaskNamespace::new("local", "default"));
        let mut plan = TaskPlan::awaiting_planner_input(&task);
        let draft = PlannerDraft {
            notes: vec!["note a".to_owned()],
            work_items: vec![PlannerWorkItem {
                stage: "planning".to_owned(),
                title: "Do thing".to_owned(),
                goal: "g".to_owned(),
            }],
            agent_proposals: vec![],
        };
        apply_planner_draft_to_task_plan(&mut plan, &draft).unwrap();
        assert_eq!(plan.status, TaskPlanStatus::Approved);
        assert!(
            plan.planning_notes.iter().any(|n| n.contains("note a")),
            "draft note should append after awaiting_planner_input seed notes"
        );
        assert_eq!(plan.work_items.len(), 1);
    }

    #[test]
    fn apply_draft_errors_on_empty() {
        let task = TaskRequest::new("t1", "Title", "Goal")
            .with_namespace(TaskNamespace::new("local", "default"));
        let mut plan = TaskPlan::awaiting_planner_input(&task);
        let draft = PlannerDraft {
            notes: vec![],
            work_items: vec![],
            agent_proposals: vec![],
        };
        assert!(apply_planner_draft_to_task_plan(&mut plan, &draft).is_err());
    }

    #[test]
    fn summarize_planner_draft_prefers_first_note() {
        let draft = PlannerDraft {
            notes: vec!["tighten rollout guardrails".to_owned()],
            work_items: vec![],
            agent_proposals: vec![],
        };
        let (summary, details) = summarize_planner_draft_for_plan_note(&draft);
        assert!(summary.contains("tighten rollout guardrails"));
        assert_eq!(
            details.as_deref(),
            Some("notes=1; work_items=0; agent_proposals=0")
        );
    }
}
