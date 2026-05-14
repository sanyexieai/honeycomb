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
