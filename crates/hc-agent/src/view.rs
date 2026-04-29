use serde::{Deserialize, Serialize};

use hc_core::{MessageRoute, NominationStatus, RuntimeSupervisor, SpeakingGrant};
use hc_trace::{
    ActivityItemView, DecisionTraceView, agent_code_from, behavior_mode_code_from, code_from,
    summarize_trace_body,
};

use crate::{AgentWorkbench, MaterializedAgent};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceViewModel {
    pub task_id: String,
    pub task_title: String,
    pub task_goal: String,
    pub task_token_budget: u32,
    pub task_time_budget_minutes: u32,
    pub planned_token_cost: u32,
    pub planned_time_minutes: u32,
    pub session_id: String,
    pub namespace_label: String,
    pub phase: String,
    pub plan_status: String,
    pub planning_notes: Vec<String>,
    pub work_item_lines: Vec<String>,
    pub assignment_lines: Vec<String>,
    pub work_item_count: usize,
    pub proposed_agent_count: usize,
    pub work_item_claim_count: usize,
    pub work_item_assignment_count: usize,
    pub channel_conversation_count: usize,
    pub channel_conversation_lines: Vec<String>,
    pub agent_cards: Vec<AgentCardView>,
    pub recent_activity: Vec<ActivityItemView>,
    pub decision_traces: Vec<DecisionTraceView>,
    pub asset_summary: AssetSummaryView,
    pub evolution_issue_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardView {
    pub instance_id: String,
    pub name: String,
    pub role: String,
    pub agent_code: String,
    pub status: String,
    pub behavior_mode_code: String,
    pub capability_names: Vec<String>,
    pub memory_scope_refs: Vec<String>,
    pub responder_label: Option<String>,
    pub pending_reply_count: usize,
    pub token_budget: u32,
    pub idle_token_budget: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AssetSummaryView {
    pub personas: usize,
    pub capabilities: usize,
    pub memory_records: usize,
}

pub fn build_workspace_view(
    runtime: &RuntimeSupervisor,
    workbench: &AgentWorkbench,
) -> WorkspaceViewModel {
    let agent_cards = workbench
        .agents
        .iter()
        .map(|agent| build_agent_card(runtime, agent))
        .collect::<Vec<_>>();

    let recent_activity = build_recent_activity(runtime, workbench);
    let phase = match workbench.phase {
        crate::WorkspacePhase::Planning => "planning".to_owned(),
        crate::WorkspacePhase::Assignment => "assignment".to_owned(),
        crate::WorkspacePhase::Execution => "execution".to_owned(),
        crate::WorkspacePhase::Consolidation => "consolidation".to_owned(),
    };

    WorkspaceViewModel {
        task_id: workbench.task.id.clone(),
        task_title: workbench.task.title.clone(),
        task_goal: workbench.task.goal.clone(),
        task_token_budget: workbench.task_plan.task_token_budget,
        task_time_budget_minutes: workbench.task_plan.task_time_budget_minutes,
        planned_token_cost: workbench.task_plan.planned_token_cost(),
        planned_time_minutes: workbench.task_plan.planned_time_minutes(),
        session_id: workbench.session.id.clone(),
        namespace_label: format!(
            "{}/{}",
            workbench.session.namespace.tenant_id, workbench.session.namespace.user_id
        ),
        phase,
        plan_status: match workbench.task_plan.status {
            crate::TaskPlanStatus::AwaitingPlannerInput => "awaiting_planner_input".to_owned(),
            crate::TaskPlanStatus::Drafted => "drafted".to_owned(),
            crate::TaskPlanStatus::Approved => "approved".to_owned(),
        },
        planning_notes: workbench.task_plan.planning_notes.clone(),
        work_item_lines: build_work_item_lines(workbench),
        assignment_lines: build_assignment_lines(workbench),
        work_item_count: workbench.task_plan.work_items.len(),
        proposed_agent_count: workbench.task_plan.agent_proposals.len(),
        work_item_claim_count: workbench.task_plan.work_item_claims.len(),
        work_item_assignment_count: workbench.task_plan.work_item_assignments.len(),
        channel_conversation_count: workbench.channel_conversations.len(),
        channel_conversation_lines: build_channel_conversation_lines(workbench),
        agent_cards,
        recent_activity,
        decision_traces: build_decision_traces(runtime, workbench),
        asset_summary: AssetSummaryView {
            personas: workbench.agents.len(),
            capabilities: workbench
                .agents
                .iter()
                .map(|agent| agent.capabilities.len())
                .sum(),
            memory_records: 0,
        },
        evolution_issue_count: workbench.task_plan.evolution_issues.len(),
    }
}

fn build_channel_conversation_lines(workbench: &AgentWorkbench) -> Vec<String> {
    if workbench.channel_conversations.is_empty() {
        return vec!["No active channel conversations.".to_owned()];
    }

    workbench
        .channel_conversations
        .iter()
        .map(|conversation| {
            format!(
                "{} | #{} | status={} | turn={} | participants={}",
                conversation.title,
                conversation.channel_id,
                match conversation.status {
                    crate::conversation::ConversationStatus::Draft => "draft",
                    crate::conversation::ConversationStatus::Active => "active",
                    crate::conversation::ConversationStatus::Paused => "paused",
                    crate::conversation::ConversationStatus::Closed => "closed",
                },
                match conversation.turn_state {
                    crate::conversation::ConversationTurnState::Waiting => "waiting",
                    crate::conversation::ConversationTurnState::Open => "open",
                    crate::conversation::ConversationTurnState::Resolved => "resolved",
                },
                conversation.participants.len()
            )
        })
        .collect()
}

fn build_agent_card(runtime: &RuntimeSupervisor, agent: &MaterializedAgent) -> AgentCardView {
    let runtime_instance = runtime.instance(&agent.binding.instance_id);
    let status = match runtime_instance {
        Some(instance) if !instance.job_ids.is_empty() => "executing",
        Some(_) => "idle",
        None => "detached",
    };

    AgentCardView {
        instance_id: agent.binding.instance_id.clone(),
        name: agent.persona.name.clone(),
        role: agent.persona.role.clone(),
        agent_code: build_agent_code(agent),
        status: status.to_owned(),
        behavior_mode_code: build_behavior_mode_code(agent),
        capability_names: agent
            .capabilities
            .iter()
            .map(|capability| capability.name.clone())
            .collect(),
        memory_scope_refs: agent.binding.memory_scope_refs.clone(),
        responder_label: agent
            .binding
            .responder
            .as_ref()
            .map(|responder| responder.label()),
        pending_reply_count: 0,
        token_budget: agent.runtime_budget.allocated_tokens,
        idle_token_budget: agent.runtime_budget.idle_tokens(),
    }
}

fn build_recent_activity(
    runtime: &RuntimeSupervisor,
    workbench: &AgentWorkbench,
) -> Vec<ActivityItemView> {
    let recent_messages = runtime
        .state()
        .messages
        .iter()
        .filter(|message| message.session_id == workbench.session.id)
        .rev()
        .take(6)
        .collect::<Vec<_>>();

    if recent_messages.is_empty() {
        return vec![
            ActivityItemView::new(
                "task",
                "bootstrap",
                "system",
                "Task Accepted",
                summarize_trace_body(&workbench.task.goal),
            ),
            ActivityItemView::new(
                "bootstrap",
                "bootstrap",
                "system",
                "Planning Agent Materialized",
                workbench
                    .agents
                    .iter()
                    .map(|agent| format!("{}[{}]", agent.persona.name, build_agent_code(agent)))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            ActivityItemView::new(
                "planning",
                "planning",
                "planner",
                "Plan Awaiting Input",
                "No work items, claims, or agent proposals exist yet.",
            ),
        ];
    }

    let mut items = vec![
        ActivityItemView::new(
            "task",
            "bootstrap",
            "system",
            "Task Accepted",
            summarize_trace_body(&workbench.task.goal),
        ),
        ActivityItemView::new(
            "bootstrap",
            "bootstrap",
            "system",
            "Planning Agent Materialized",
            workbench
                .agents
                .iter()
                .map(|agent| format!("{}[{}]", agent.persona.name, build_agent_code(agent)))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ActivityItemView::new(
            "planning",
            "planning",
            "planner",
            "Plan Awaiting Input",
            format!(
                "status={} | work_items={} | agent_proposals={} | claims={} | assignments={}",
                match workbench.task_plan.status {
                    crate::TaskPlanStatus::AwaitingPlannerInput => "awaiting_planner_input",
                    crate::TaskPlanStatus::Drafted => "drafted",
                    crate::TaskPlanStatus::Approved => "approved",
                },
                workbench.task_plan.work_items.len(),
                workbench.task_plan.agent_proposals.len(),
                workbench.task_plan.work_item_claims.len(),
                workbench.task_plan.work_item_assignments.len()
            ),
        ),
    ];

    items.extend(recent_messages.into_iter().map(|message| {
        let route_label = match &message.route {
            MessageRoute::Direct { .. } => "direct",
            MessageRoute::Broadcast => "broadcast",
            MessageRoute::Channel { .. } => "channel",
        };
        let nomination = runtime.nomination_for_message(&message.id).ok();
        let nomination_detail = nomination
            .map(|nomination| match nomination.status {
                NominationStatus::Open => {
                    format!("nomination round {} open", nomination.current_round)
                }
                NominationStatus::Granted => {
                    format!("speaking granted in round {}", nomination.current_round)
                }
                NominationStatus::Exhausted => {
                    format!(
                        "nomination exhausted after round {}",
                        nomination.current_round
                    )
                }
            })
            .unwrap_or_else(|| "no nomination".to_owned());

        ActivityItemView::new(
            "message",
            "message",
            message.from.clone(),
            format!("{} ({})", message.id, route_label),
            format!(
                "{} | {}",
                summarize_trace_body(&message.body),
                nomination_detail
            ),
        )
    }));

    items
}

fn build_decision_traces(
    runtime: &RuntimeSupervisor,
    workbench: &AgentWorkbench,
) -> Vec<DecisionTraceView> {
    let mut traces = Vec::new();

    traces.push(DecisionTraceView::new(
        code_from("TASK", &workbench.task.id),
        "bootstrap",
        workbench.task.title.clone(),
        "accepted",
        summarize_trace_body(&workbench.task.goal),
    ));

    for agent in &workbench.agents {
        traces.push(DecisionTraceView::new(
            code_from("AGENT", &agent.binding.instance_id),
            "bootstrap",
            agent.persona.name.clone(),
            "materialized",
            format!(
                "{} | mode {}",
                build_agent_code(agent),
                build_behavior_mode_code(agent)
            ),
        ));
    }

    for nomination in runtime
        .state()
        .nominations
        .iter()
        .filter(|nomination| nomination.session_id == workbench.session.id)
    {
        traces.push(DecisionTraceView::new(
            code_from("NOM", &nomination.message_id),
            "nomination",
            nomination.message_id.clone(),
            match nomination.status {
                NominationStatus::Open => format!("round-{}-open", nomination.current_round),
                NominationStatus::Granted => format!("round-{}-granted", nomination.current_round),
                NominationStatus::Exhausted => {
                    format!("round-{}-exhausted", nomination.current_round)
                }
            },
            format!("route {:?}", nomination.route),
        ));
    }

    for claim in runtime.state().claims.iter().filter(|claim| {
        runtime.state().messages.iter().any(|message| {
            message.id == claim.message_id && message.session_id == workbench.session.id
        })
    }) {
        traces.push(DecisionTraceView::new(
            code_from("CLM", &claim.instance_id),
            "claim",
            claim.instance_id.clone(),
            format!("score-{:.2}", claim.score),
            format!("message {} | round {}", claim.message_id, claim.round),
        ));
    }

    for grant in runtime
        .state()
        .speaking_grants
        .iter()
        .filter(|grant| message_in_session(runtime, &workbench.session.id, &grant.message_id))
    {
        traces.push(build_grant_trace(grant));
    }

    for work_item in &workbench.task_plan.work_items {
        traces.push(DecisionTraceView::new(
            code_from("WRK", &work_item.id),
            "planning",
            work_item.title.clone(),
            work_item.status.clone(),
            format!(
                "stage {} | {}",
                work_item.stage,
                summarize_trace_body(&work_item.goal)
            ),
        ));
    }

    for assignment in &workbench.task_plan.work_item_assignments {
        traces.push(DecisionTraceView::new(
            code_from("ASN", &assignment.id),
            "assignment",
            assignment.work_item_id.clone(),
            assignment.status.clone(),
            format!(
                "{} | {}",
                assignment.agent_name,
                summarize_trace_body(&assignment.rationale)
            ),
        ));
    }

    traces
}

fn build_work_item_lines(workbench: &AgentWorkbench) -> Vec<String> {
    if workbench.task_plan.work_items.is_empty() {
        return vec!["- none".to_owned()];
    }

    workbench
        .task_plan
        .work_items
        .iter()
        .map(|item| {
            format!(
                "- {} [{}] {} :: {}",
                item.id,
                item.status,
                item.title,
                summarize_trace_body(&item.goal)
            )
        })
        .collect()
}

fn build_assignment_lines(workbench: &AgentWorkbench) -> Vec<String> {
    if workbench.task_plan.work_item_assignments.is_empty() {
        return vec!["- none".to_owned()];
    }

    workbench
        .task_plan
        .work_item_assignments
        .iter()
        .map(|assignment| {
            format!(
                "- {} [{}] {} -> {} :: {}",
                assignment.id,
                assignment.status,
                assignment.work_item_id,
                assignment.agent_name,
                summarize_trace_body(&assignment.rationale)
            )
        })
        .collect()
}

fn build_grant_trace(grant: &SpeakingGrant) -> DecisionTraceView {
    DecisionTraceView::new(
        code_from("GRT", &grant.instance_id),
        "grant",
        grant.instance_id.clone(),
        format!("won-round-{}", grant.round),
        format!("message {}", grant.message_id),
    )
}

fn message_in_session(runtime: &RuntimeSupervisor, session_id: &str, message_id: &str) -> bool {
    runtime
        .state()
        .messages
        .iter()
        .any(|message| message.id == message_id && message.session_id == session_id)
}

fn build_agent_code(agent: &MaterializedAgent) -> String {
    agent_code_from(&agent.seed.role, &agent.binding.instance_id)
}

fn build_behavior_mode_code(agent: &MaterializedAgent) -> String {
    behavior_mode_code_from(
        agent
            .binding
            .responder
            .as_ref()
            .map(|responder| responder.kind()),
    )
}

#[cfg(test)]
#[path = "../tests/unit/view.rs"]
mod tests;
