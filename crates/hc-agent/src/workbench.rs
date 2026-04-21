use anyhow::Result;
use hc_core::{
    ChannelRecord, MessageRecord, MessageRoute, NominationStatus, RuntimeNamespace,
    RuntimeSupervisor, SessionRecord,
};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
        let now = current_timestamp_ms();
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
            .unwrap_or_else(|| format!("turn.{}", current_timestamp_ms()));
        conversation.open_turn(message_id, current_timestamp_ms());
        Ok(())
    }

    pub fn resolve_conversation_turn(&mut self, conversation_id: &str) -> Result<()> {
        let conversation = self
            .channel_conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
            .ok_or_else(|| anyhow::anyhow!("conversation not found: {conversation_id}"))?;
        conversation.resolve_turn(current_timestamp_ms());
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

        conversation.open_turn(message.id.clone(), current_timestamp_ms());

        if let Ok(nomination) = runtime.nomination_for_message(&message.id) {
            match nomination.status {
                NominationStatus::Open => {
                    conversation.turn_state = crate::ConversationTurnState::Open;
                }
                NominationStatus::Granted | NominationStatus::Exhausted => {
                    conversation.resolve_turn(current_timestamp_ms());
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
mod tests {
    use super::*;
    use crate::TaskNamespace;

    #[test]
    fn bootstrap_task_workbench_creates_planning_workspace() {
        let mut runtime = RuntimeSupervisor::new();
        let task = TaskRequest::new(
            "task.ui.bootstrap",
            "UI Bootstrap",
            "Create a working multi-agent workspace",
        )
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

        let workbench =
            bootstrap_task_workbench(&mut runtime, task).expect("workbench should bootstrap");

        assert_eq!(workbench.session.namespace.tenant_id, "tenant-a");
        assert_eq!(workbench.session.namespace.user_id, "user-a");
        assert_eq!(workbench.phase, WorkspacePhase::Planning);
        assert_eq!(workbench.agents.len(), 1);
        assert!(workbench.channel_conversations.is_empty());
        assert_eq!(runtime.state().instances.len(), 1);
        assert_eq!(workbench.agents[0].persona.role, "planner");
        assert_eq!(
            workbench.task_plan.status,
            crate::TaskPlanStatus::AwaitingPlannerInput
        );
    }

    #[test]
    fn workbench_can_create_channel_conversation_and_add_participants() {
        let mut runtime = RuntimeSupervisor::new();
        let task = TaskRequest::new(
            "task.ui.bootstrap",
            "UI Bootstrap",
            "Create a working multi-agent workspace",
        )
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

        let mut workbench =
            bootstrap_task_workbench(&mut runtime, task).expect("workbench should bootstrap");

        let conversation_id = workbench.create_channel_conversation("channel.0001", "Planning");
        workbench
            .add_user_to_conversation(&conversation_id, "user.user-a", "user-a")
            .expect("user should be added");
        let planner_id = workbench.agents[0].binding.instance_id.clone();
        workbench
            .add_agent_to_conversation(&conversation_id, &planner_id)
            .expect("planner should be added");
        workbench
            .open_conversation_turn(&conversation_id)
            .expect("turn should open");
        workbench
            .resolve_conversation_turn(&conversation_id)
            .expect("turn should resolve");

        let conversation = workbench
            .require_conversation(&conversation_id)
            .expect("conversation should exist");
        assert_eq!(conversation.participants.len(), 2);
        assert_eq!(conversation.status, crate::ConversationStatus::Active);
        assert_eq!(
            conversation.turn_state,
            crate::ConversationTurnState::Resolved
        );
    }

    #[test]
    fn workbench_can_sync_conversation_from_runtime_channel_members() {
        let mut runtime = RuntimeSupervisor::new();
        let task = TaskRequest::new(
            "task.ui.bootstrap",
            "UI Bootstrap",
            "Create a working multi-agent workspace",
        )
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

        let mut workbench =
            bootstrap_task_workbench(&mut runtime, task).expect("workbench should bootstrap");

        let channel = runtime
            .create_channel(&workbench.session.id, "discussion")
            .expect("channel should be created");
        let planner_id = workbench.agents[0].binding.instance_id.clone();
        runtime
            .join_channel(&planner_id, &channel.id)
            .expect("planner should join");

        let user = runtime
            .create_instance(&workbench.session.id, "alice", None)
            .expect("user instance should be created");
        runtime
            .join_channel(&user.id, &channel.id)
            .expect("user should join");

        let conversation_id = workbench
            .ensure_and_sync_channel_conversation(&runtime, &channel.id)
            .expect("conversation should sync");
        let conversation = workbench
            .require_conversation(&conversation_id)
            .expect("conversation should exist");

        assert_eq!(conversation.channel_id, channel.id);
        assert_eq!(conversation.participants.len(), 2);
        assert!(
            conversation
                .participants
                .iter()
                .any(|participant| participant.display_name == "planner")
        );
        assert!(
            conversation
                .participants
                .iter()
                .any(|participant| participant.display_name == "alice")
        );
    }

    #[test]
    fn workbench_can_sync_channel_message_into_conversation_turn() {
        let mut runtime = RuntimeSupervisor::new();
        let task = TaskRequest::new(
            "task.ui.bootstrap",
            "UI Bootstrap",
            "Create a working multi-agent workspace",
        )
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

        let mut workbench =
            bootstrap_task_workbench(&mut runtime, task).expect("workbench should bootstrap");

        let channel = runtime
            .create_channel(&workbench.session.id, "discussion")
            .expect("channel should be created");
        let planner_id = workbench.agents[0].binding.instance_id.clone();
        runtime
            .join_channel(&planner_id, &channel.id)
            .expect("planner should join");

        let message = runtime
            .post_message(
                &workbench.session.id,
                &planner_id,
                MessageRoute::Channel {
                    channel_id: channel.id.clone(),
                },
                hc_core::MessageKind::Chat,
                "let us discuss the plan",
                None,
            )
            .expect("channel message should post");

        let conversation_id = workbench
            .sync_conversation_turn_from_message(&runtime, &message)
            .expect("conversation should sync from message")
            .expect("channel message should map to a conversation");
        let conversation = workbench
            .require_conversation(&conversation_id)
            .expect("conversation should exist");

        assert_eq!(conversation.channel_id, channel.id);
        assert_eq!(
            conversation.last_message_id.as_deref(),
            Some(message.id.as_str())
        );
        assert_eq!(conversation.turn_state, crate::ConversationTurnState::Open);
    }
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
