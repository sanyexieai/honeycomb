use super::*;

#[test]
fn conversation_tracks_participants_without_duplicates() {
    let mut conversation = ChannelConversation::new(
        "conversation.0001",
        "session.0001",
        "channel.0001",
        "Planning Thread",
        100,
    );

    conversation.add_participant(ConversationParticipant::user("user.default", "default"));
    conversation.add_participant(ConversationParticipant::agent(
        "instance.0001",
        "planner",
        "planner",
        Some("responder.llm".to_owned()),
    ));
    conversation.add_participant(ConversationParticipant::agent(
        "instance.0001",
        "planner",
        "planner",
        Some("responder.llm".to_owned()),
    ));

    assert_eq!(conversation.participant_refs.len(), 2);
    assert_eq!(conversation.participants.len(), 2);
}

#[test]
fn conversation_can_transition_to_active_and_resolve_turns() {
    let mut conversation = ChannelConversation::new(
        "conversation.0001",
        "session.0001",
        "channel.0001",
        "Planning Thread",
        100,
    );

    conversation.activate(120);
    conversation.open_turn("message.0001", 130);
    conversation.resolve_turn(140);

    assert_eq!(conversation.status, ConversationStatus::Active);
    assert_eq!(conversation.turn_state, ConversationTurnState::Resolved);
    assert_eq!(
        conversation.last_message_id.as_deref(),
        Some("message.0001")
    );
    assert!(conversation.active_message_id.is_none());
    assert_eq!(conversation.last_activity_at_ms, 140);
}
