use anyhow::Result;
use hc_bootstrap::wall_clock_ms;
use hc_core::{
    ChannelRecord, MessageRecord, MessageRoute, NominationStatus, RuntimeNamespace,
    RuntimeSupervisor, SessionRecord,
};
use serde::{Deserialize, Serialize};

use crate::{
    AgentPlan, ChannelConversation, ConversationParticipant, MaterializedAgent, TaskPlan,
    TaskRequest, bootstrap_planning_task, materialize_plan,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePhase {
    Planning,
    Assignment,
    Execution,
    Consolidation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentWorkbench {
    pub task: TaskRequest,
    pub session: SessionRecord,
    pub phase: WorkspacePhase,
    pub task_plan: TaskPlan,
    pub plan: AgentPlan,
    pub agents: Vec<MaterializedAgent>,
    pub channel_conversations: Vec<ChannelConversation>,
}

impl AgentWorkbench {
    pub fn ensure_channel_conversation_for_channel(&mut self, channel: &ChannelRecord) -> String {
        if let Some(existing) = self
            .channel_conversations
            .iter()
            .find(|conversation| conversation.channel_id == channel.id)
        {
            return existing.id.clone();
        }

        self.create_channel_conversation(channel.id.clone(), format!("{} discussion", channel.name))
    }

    pub fn create_channel_conversation(
        &mut self,
        channel_id: impl Into<String>,
        title: impl Into<String>,
    ) -> String {
        let id = format!("conversation.{:04}", self.channel_conversations.len() + 1);
        let now = wall_clock_ms();
        let mut conversation =
            ChannelConversation::new(id.clone(), self.session.id.clone(), channel_id, title, now);
        conversation.activate(now);
        self.channel_conversations.push(conversation);
        id
    }

    pub fn add_user_to_conversation(
        &mut self,
        conversation_id: &str,
        user_ref: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<()> {
        let conversation = self
            .channel_conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))?;
        conversation.add_participant(ConversationParticipant::user(user_ref, display_name));
        Ok(())
    }

    pub fn add_agent_to_conversation(
        &mut self,
        conversation_id: &str,
        instance_id: &str,
    ) -> Result<()> {
        let conversation = self
            .channel_conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))?;
        let agent = self
            .agents
            .iter()
            .find(|agent| agent.binding.instance_id == instance_id)
            .ok_or_else(|| anyhow::anyhow!("agent not found for instance: {instance_id}"))?;
        conversation.add_participant(ConversationParticipant::agent(
            agent.binding.instance_id.clone(),
            agent.persona.name.clone(),
            agent.persona.role.clone(),
            agent.binding.responder_binding_ref.clone(),
        ));
        Ok(())
    }

    pub fn sync_conversation_participants_from_channel(
        &mut self,
        runtime: &RuntimeSupervisor,
        conversation_id: &str,
        channel_id: &str,
    ) -> Result<()> {
        let channel = runtime
            .channel(channel_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("channel not found: {channel_id}"))?;

        for instance_id in &channel.member_instance_ids {
            if self
                .agents
                .iter()
                .any(|agent| &agent.binding.instance_id == instance_id)
            {
                self.add_agent_to_conversation(conversation_id, instance_id)?;
            } else if let Some(instance) = runtime.instance(instance_id) {
                self.add_user_to_conversation(
                    conversation_id,
                    format!("user.{}", instance.name),
                    instance.name.clone(),
                )?;
            }
        }

        Ok(())
    }

    pub fn ensure_and_sync_channel_conversation(
        &mut self,
        runtime: &RuntimeSupervisor,
        channel_id: &str,
    ) -> Result<String> {
        let channel = runtime
            .channel(channel_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("channel not found: {channel_id}"))?;
        let conversation_id = self.ensure_channel_conversation_for_channel(&channel);
        self.sync_conversation_participants_from_channel(runtime, &conversation_id, channel_id)?;
        Ok(conversation_id)
    }

    pub fn open_conversation_turn(&mut self, conversation_id: &str) -> Result<()> {
        let conversation = self
            .channel_conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))?;
        let message_id = conversation
            .last_message_id
            .clone()
            .unwrap_or_else(|| format!("turn.{}", wall_clock_ms()));
        conversation.open_turn(message_id, wall_clock_ms());
        Ok(())
    }

    pub fn resolve_conversation_turn(&mut self, conversation_id: &str) -> Result<()> {
        let conversation = self
            .channel_conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))?;
        conversation.resolve_turn(wall_clock_ms());
        Ok(())
    }

    pub fn require_conversation(&self, conversation_id: &str) -> Result<&ChannelConversation> {
        self.channel_conversations
            .iter()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))
    }

    pub fn sync_conversation_turn_from_message(
        &mut self,
        runtime: &RuntimeSupervisor,
        message: &MessageRecord,
    ) -> Result<Option<String>> {
        let MessageRoute::Channel { channel_id } = &message.route else {
            return Ok(None);
        };

        let conversation_id = self.ensure_and_sync_channel_conversation(runtime, channel_id)?;
        let conversation = self
            .channel_conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))?;

        conversation.open_turn(message.id.clone(), wall_clock_ms());

        if let Ok(nomination) = runtime.nomination_for_message(&message.id) {
            match nomination.status {
                NominationStatus::Open => {
                    conversation.turn_state = crate::ConversationTurnState::Open;
                }
                NominationStatus::Granted | NominationStatus::Exhausted => {
                    conversation.resolve_turn(wall_clock_ms());
                }
            }
        }

        Ok(Some(conversation_id))
    }
}

pub fn bootstrap_task_workbench(
    runtime: &mut RuntimeSupervisor,
    task: TaskRequest,
) -> Result<AgentWorkbench> {
    let namespace = RuntimeNamespace::new(
        task.namespace.tenant_id.clone(),
        task.namespace.user_id.clone(),
    );
    let session = runtime
        .create_session_in_namespace(format!("task-{}", task.id.replace(' ', "-")), namespace);
    let plan = bootstrap_planning_task(&task);
    let mut task_plan = TaskPlan::awaiting_planner_input(&task);
    let agents = materialize_plan(runtime, &session.id, &plan)?;
    for agent in &agents {
        task_plan.register_agent_runtime_budget(agent.runtime_budget.clone());
    }

    Ok(AgentWorkbench {
        task,
        session,
        phase: WorkspacePhase::Planning,
        task_plan,
        plan,
        agents,
        channel_conversations: Vec::new(),
    })
}

#[cfg(test)]
#[path = "../tests/unit/workbench.rs"]
mod tests;
