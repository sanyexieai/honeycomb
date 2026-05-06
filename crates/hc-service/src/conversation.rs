use anyhow::Result;
use hc_agent::AgentRepository;
use hc_conversation::{
    AgentTurnProposal, AgentTurnProposalStatus, ConversationEvent, ConversationEventStatus,
    ConversationRepository, FollowUpStatus, PendingFollowUp, now_unix,
};
use hc_protocol::{ApiMemoryQuery, ApiNamespace, ChatRequest, ChatResponse};
use hc_store::store::WorkspaceNamespace;
use serde::Serialize;

use crate::{ServiceConfig, chat::handle_chat_request};

#[derive(Debug, Clone, Serialize)]
pub struct ConversationInboxSnapshot {
    pub now_unix: u64,
    pub pending_events: Vec<ConversationEvent>,
    pub due_followups: Vec<PendingFollowUp>,
    pub pending_proposals: Vec<AgentTurnProposal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationProcessReport {
    pub now_unix: u64,
    pub processed_events: usize,
    pub fired_followups: usize,
    pub proposals: Vec<AgentTurnProposal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTurnDraft {
    pub proposal: AgentTurnProposal,
    pub response: ChatResponse,
}

pub fn publish_conversation_event(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    event: ConversationEvent,
) -> Result<ConversationEvent> {
    let repository = repository(config, namespace);
    repository.write_event(&event)?;
    Ok(event)
}

pub fn create_pending_followup(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    followup: PendingFollowUp,
) -> Result<PendingFollowUp> {
    let repository = repository(config, namespace);
    repository.write_followup(&followup)?;
    Ok(followup)
}

pub fn conversation_inbox_snapshot(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    now: Option<u64>,
) -> Result<ConversationInboxSnapshot> {
    let now = now.unwrap_or_else(now_unix);
    let repository = repository(config, namespace);
    Ok(ConversationInboxSnapshot {
        now_unix: now,
        pending_events: repository.pending_events(now)?,
        due_followups: repository.due_followups(now)?,
        pending_proposals: repository.pending_proposals()?,
    })
}

pub fn process_conversation_inbox(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    now: Option<u64>,
) -> Result<ConversationProcessReport> {
    let now = now.unwrap_or_else(now_unix);
    let workspace_namespace =
        WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
    let repository = ConversationRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let agent_repository =
        AgentRepository::with_namespace(config.workspace_root.clone(), workspace_namespace);
    let agents = agent_repository.list_profiles()?;

    let mut proposals = Vec::new();
    let mut processed_events = 0usize;
    let mut fired_followups = 0usize;

    for mut event in repository.pending_events(now)? {
        let Some(agent_id) = event.agent_id.clone() else {
            event.status = ConversationEventStatus::Ignored;
            repository.write_event(&event)?;
            processed_events += 1;
            continue;
        };
        let Some(agent) = agents.iter().find(|agent| agent.id == agent_id) else {
            event.status = ConversationEventStatus::Failed;
            repository.write_event(&event)?;
            processed_events += 1;
            continue;
        };
        if !agent.conversation_policy.can_initiate
            || !trigger_allowed(&agent.conversation_policy.proactive_triggers, &event.kind)
        {
            event.status = ConversationEventStatus::Ignored;
            repository.write_event(&event)?;
            processed_events += 1;
            continue;
        }
        let mut proposal = AgentTurnProposal::new(agent.id.clone(), "proactive_event");
        proposal.id = format!(
            "conversation-proposal.event.{}.{}",
            now,
            proposals.len() + 1
        );
        proposal.room_id = event.room_id.clone();
        proposal.source_event_id = Some(event.id.clone());
        proposal.payload = event.payload.clone();
        proposal.notes = format!(
            "Agent {} may initiate a message for event kind {}.",
            agent.id, event.kind
        );
        repository.write_proposal(&proposal)?;
        event.status = ConversationEventStatus::Processed;
        repository.write_event(&event)?;
        proposals.push(proposal);
        processed_events += 1;
    }

    for mut followup in repository.due_followups(now)? {
        let Some(agent) = agents.iter().find(|agent| agent.id == followup.agent_id) else {
            followup.status = FollowUpStatus::Cancelled;
            repository.write_followup(&followup)?;
            fired_followups += 1;
            continue;
        };
        if !agent.conversation_policy.can_follow_up {
            followup.status = FollowUpStatus::Cancelled;
            repository.write_followup(&followup)?;
            fired_followups += 1;
            continue;
        }
        let mut proposal = AgentTurnProposal::new(agent.id.clone(), "follow_up");
        proposal.id = format!(
            "conversation-proposal.followup.{}.{}",
            now,
            proposals.len() + 1
        );
        proposal.room_id = followup.room_id.clone();
        proposal.source_followup_id = Some(followup.id.clone());
        proposal.payload = followup.payload.clone();
        proposal.notes = format!(
            "Agent {} may follow up for trigger {}.",
            agent.id, followup.trigger
        );
        repository.write_proposal(&proposal)?;
        followup.status = FollowUpStatus::Fired;
        repository.write_followup(&followup)?;
        proposals.push(proposal);
        fired_followups += 1;
    }

    Ok(ConversationProcessReport {
        now_unix: now,
        processed_events,
        fired_followups,
        proposals,
    })
}

pub fn draft_agent_turn_proposal(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    proposal_id: &str,
) -> Result<AgentTurnDraft> {
    let repository = repository(config, namespace.clone());
    let mut proposal = repository.get_proposal(proposal_id)?;
    let prompt = proposal_prompt(&proposal);
    let response = handle_chat_request(
        config,
        ChatRequest {
            tenant_id: Some(namespace.tenant_id.clone()),
            user_id: Some(namespace.user_id.clone()),
            session_id: proposal.room_id.clone(),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some(prompt),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: Some(
                "You are drafting a user-facing proactive or follow-up message. Keep it concise, helpful, and natural. Do not expose internal event ids, tool ids, MCP server names, raw JSON, or implementation details."
                    .to_owned(),
            ),
            agent_id: Some(proposal.agent_id.clone()),
            domain_id: None,
            active_agent_id: Some(proposal.agent_id.clone()),
            active_task_id: None,
            memory: ApiMemoryQuery {
                namespace,
                scope: None,
                kind: None,
                tag: None,
                text: Some(proposal.notes.clone()),
                limit: Some(8),
            },
            temperature: Some(0.2),
            max_output_tokens: Some(300),
        },
    )?;
    proposal.status = AgentTurnProposalStatus::Accepted;
    proposal.payload.insert(
        "draft_message".to_owned(),
        serde_json::Value::String(response.message.content.clone()),
    );
    repository.write_proposal(&proposal)?;
    Ok(AgentTurnDraft { proposal, response })
}

pub fn mark_agent_turn_proposal_sent(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    proposal_id: &str,
) -> Result<AgentTurnProposal> {
    repository(config, namespace).set_proposal_status(proposal_id, AgentTurnProposalStatus::Sent)
}

pub fn dismiss_agent_turn_proposal(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    proposal_id: &str,
) -> Result<AgentTurnProposal> {
    repository(config, namespace)
        .set_proposal_status(proposal_id, AgentTurnProposalStatus::Dismissed)
}

fn repository(config: &ServiceConfig, namespace: ApiNamespace) -> ConversationRepository {
    ConversationRepository::with_namespace(
        config.workspace_root.clone(),
        WorkspaceNamespace::new(namespace.tenant_id, namespace.user_id),
    )
}

fn proposal_prompt(proposal: &AgentTurnProposal) -> String {
    let payload =
        serde_json::to_string_pretty(&proposal.payload).unwrap_or_else(|_| "{}".to_owned());
    let source = proposal
        .source_event_id
        .as_deref()
        .or(proposal.source_followup_id.as_deref())
        .unwrap_or("");
    format!(
        "Draft a short user-facing message for this agent turn proposal.\nkind: {}\nsource: {}\nnotes: {}\npayload:\n{}",
        proposal.kind, source, proposal.notes, payload
    )
}

fn trigger_allowed(triggers: &[String], kind: &str) -> bool {
    triggers.is_empty()
        || triggers
            .iter()
            .any(|trigger| trigger == kind || trigger == "*" || kind.starts_with(trigger))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hc_agent::{AgentKind, AgentProfile, AgentRepository};
    use hc_conversation::ConversationPolicy;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hc-service-conversation-{name}-{}", now_unix()))
    }

    #[test]
    fn processing_event_creates_proposal_when_agent_policy_allows_it() {
        let root = temp_root("event-proposal");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let agent_repository = AgentRepository::with_namespace(&root, workspace_namespace.clone());
        let mut agent = AgentProfile {
            id: "agent.demo.proactive".to_owned(),
            name: "Demo Proactive".to_owned(),
            kind: AgentKind::DomainService,
            project_id: None,
            domain_id: None,
            priority: 0,
            intent_hints: Vec::new(),
            routing_examples: Vec::new(),
            negative_routing_examples: Vec::new(),
            tool_refs: Vec::new(),
            memory_scope_refs: Vec::new(),
            prompt_refs: Vec::new(),
            tags: Vec::new(),
            responder_ref: None,
            state_schema_ref: None,
            conversation_policy: ConversationPolicy {
                can_initiate: true,
                can_follow_up: false,
                follow_up_style: None,
                proactive_triggers: vec!["demo.event".to_owned()],
                quiet_hours: None,
            },
            instructions: String::new(),
            relative_path: String::new(),
        };
        agent.relative_path = "agents/demo/proactive.md".to_owned();
        agent_repository.write_profile(&agent).unwrap();

        let conversation_repository =
            ConversationRepository::with_namespace(&root, workspace_namespace);
        let mut event = ConversationEvent::new("demo.event");
        event.id = "event.demo.1".to_owned();
        event.agent_id = Some(agent.id.clone());
        conversation_repository.write_event(&event).unwrap();

        let report = process_conversation_inbox(&config, namespace, Some(10)).unwrap();

        assert_eq!(report.processed_events, 1);
        assert_eq!(report.proposals.len(), 1);
        assert_eq!(report.proposals[0].agent_id, agent.id);
        assert_eq!(
            conversation_repository.list_events().unwrap()[0].status,
            ConversationEventStatus::Processed
        );
    }

    #[test]
    fn proposal_can_be_marked_sent_or_dismissed() {
        let root = temp_root("proposal-status");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let repository = ConversationRepository::with_namespace(&root, workspace_namespace);

        let mut sent_proposal = AgentTurnProposal::new("agent.demo", "follow_up");
        sent_proposal.id = "proposal.sent.demo".to_owned();
        repository.write_proposal(&sent_proposal).unwrap();
        let sent = mark_agent_turn_proposal_sent(&config, namespace.clone(), "proposal.sent.demo")
            .unwrap();
        assert_eq!(sent.status, AgentTurnProposalStatus::Sent);

        let mut dismissed_proposal = AgentTurnProposal::new("agent.demo", "follow_up");
        dismissed_proposal.id = "proposal.dismissed.demo".to_owned();
        repository.write_proposal(&dismissed_proposal).unwrap();
        let dismissed =
            dismiss_agent_turn_proposal(&config, namespace, "proposal.dismissed.demo").unwrap();
        assert_eq!(dismissed.status, AgentTurnProposalStatus::Dismissed);
    }
}
