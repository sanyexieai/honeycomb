use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationParticipantKind {
    User,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationParticipantMode {
    Active,
    ListenOnly,
    ManualOnly,
    NominateFirst,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationParticipantState {
    Idle,
    Waiting,
    Speaking,
    Muted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStatus {
    Draft,
    Active,
    Paused,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationTurnPolicy {
    SingleSpeakerNomination,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStopPolicy {
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationTurnState {
    Waiting,
    Open,
    Resolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationParticipant {
    pub participant_ref: String,
    pub kind: ConversationParticipantKind,
    pub display_name: String,
    pub role: String,
    pub responder_binding_ref: Option<String>,
    pub conversation_mode: ConversationParticipantMode,
    pub state: ConversationParticipantState,
}

impl ConversationParticipant {
    pub fn user(
        participant_ref: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        Self {
            participant_ref: participant_ref.into(),
            kind: ConversationParticipantKind::User,
            display_name: display_name.into(),
            role: "user".to_owned(),
            responder_binding_ref: None,
            conversation_mode: ConversationParticipantMode::Active,
            state: ConversationParticipantState::Idle,
        }
    }

    pub fn agent(
        participant_ref: impl Into<String>,
        display_name: impl Into<String>,
        role: impl Into<String>,
        responder_binding_ref: Option<String>,
    ) -> Self {
        Self {
            participant_ref: participant_ref.into(),
            kind: ConversationParticipantKind::Agent,
            display_name: display_name.into(),
            role: role.into(),
            responder_binding_ref,
            conversation_mode: ConversationParticipantMode::NominateFirst,
            state: ConversationParticipantState::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelConversation {
    pub id: String,
    pub session_id: String,
    pub channel_id: String,
    pub title: String,
    pub status: ConversationStatus,
    pub participant_refs: Vec<String>,
    pub participants: Vec<ConversationParticipant>,
    pub turn_policy: ConversationTurnPolicy,
    pub stop_policy: ConversationStopPolicy,
    pub turn_state: ConversationTurnState,
    pub active_message_id: Option<String>,
    pub last_message_id: Option<String>,
    pub started_at_ms: u64,
    pub last_activity_at_ms: u64,
}

impl ChannelConversation {
    pub fn new(
        id: impl Into<String>,
        session_id: impl Into<String>,
        channel_id: impl Into<String>,
        title: impl Into<String>,
        started_at_ms: u64,
    ) -> Self {
        Self {
            id: id.into(),
            session_id: session_id.into(),
            channel_id: channel_id.into(),
            title: title.into(),
            status: ConversationStatus::Draft,
            participant_refs: Vec::new(),
            participants: Vec::new(),
            turn_policy: ConversationTurnPolicy::SingleSpeakerNomination,
            stop_policy: ConversationStopPolicy::Manual,
            turn_state: ConversationTurnState::Waiting,
            active_message_id: None,
            last_message_id: None,
            started_at_ms,
            last_activity_at_ms: started_at_ms,
        }
    }

    pub fn activate(&mut self, activity_at_ms: u64) {
        self.status = ConversationStatus::Active;
        self.last_activity_at_ms = activity_at_ms;
    }

    pub fn add_participant(&mut self, participant: ConversationParticipant) {
        if !self
            .participant_refs
            .iter()
            .any(|existing| existing == &participant.participant_ref)
        {
            self.participant_refs
                .push(participant.participant_ref.clone());
            self.participants.push(participant);
        }
    }

    pub fn open_turn(
        &mut self,
        message_id: impl Into<String>,
        activity_at_ms: u64,
    ) {
        self.turn_state = ConversationTurnState::Open;
        let message_id = message_id.into();
        self.active_message_id = Some(message_id.clone());
        self.last_message_id = Some(message_id);
        self.last_activity_at_ms = activity_at_ms;
    }

    pub fn resolve_turn(&mut self, activity_at_ms: u64) {
        self.turn_state = ConversationTurnState::Resolved;
        self.active_message_id = None;
        self.last_activity_at_ms = activity_at_ms;
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(conversation.last_message_id.as_deref(), Some("message.0001"));
        assert!(conversation.active_message_id.is_none());
        assert_eq!(conversation.last_activity_at_ms, 140);
    }
}
