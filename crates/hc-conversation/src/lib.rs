use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationPolicy {
    #[serde(default)]
    pub can_initiate: bool,
    #[serde(default)]
    pub can_follow_up: bool,
    #[serde(default)]
    pub follow_up_style: Option<String>,
    #[serde(default)]
    pub proactive_triggers: Vec<String>,
    #[serde(default)]
    pub quiet_hours: Option<QuietHours>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuietHours {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationEventStatus {
    Pending,
    Claimed,
    Processed,
    Ignored,
    Failed,
}

impl Default for ConversationEventStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationEvent {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub status: ConversationEventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub payload: Map<String, Value>,
    pub created_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at_unix: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpStatus {
    Pending,
    Fired,
    Cancelled,
    Failed,
}

impl Default for FollowUpStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingFollowUp {
    pub id: String,
    pub agent_id: String,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default)]
    pub status: FollowUpStatus,
    pub due_at_unix: u64,
    #[serde(default)]
    pub payload: Map<String, Value>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTurnProposalStatus {
    Pending,
    Accepted,
    Sent,
    Dismissed,
    Failed,
}

impl Default for AgentTurnProposalStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTurnProposal {
    pub id: String,
    pub agent_id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default)]
    pub status: AgentTurnProposalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_followup_id: Option<String>,
    #[serde(default)]
    pub payload: Map<String, Value>,
    pub created_at_unix: u64,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConversationEventFrontmatter {
    id: String,
    r#type: String,
    tenant_id: String,
    user_id: String,
    kind: String,
    #[serde(default)]
    status: ConversationEventStatus,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    payload: Map<String, Value>,
    created_at_unix: u64,
    #[serde(default)]
    due_at_unix: Option<u64>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingFollowUpFrontmatter {
    id: String,
    r#type: String,
    tenant_id: String,
    user_id: String,
    agent_id: String,
    trigger: String,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    status: FollowUpStatus,
    due_at_unix: u64,
    #[serde(default)]
    payload: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentTurnProposalFrontmatter {
    id: String,
    r#type: String,
    tenant_id: String,
    user_id: String,
    agent_id: String,
    kind: String,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    status: AgentTurnProposalStatus,
    #[serde(default)]
    source_event_id: Option<String>,
    #[serde(default)]
    source_followup_id: Option<String>,
    #[serde(default)]
    payload: Map<String, Value>,
    created_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct ConversationRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl ConversationRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn event_relative_path_for(event_id: &str) -> PathBuf {
        PathBuf::from("conversation")
            .join("events")
            .join(format!("{}.md", slugify(event_id)))
    }

    pub fn followup_relative_path_for(followup_id: &str) -> PathBuf {
        PathBuf::from("conversation")
            .join("followups")
            .join(format!("{}.md", slugify(followup_id)))
    }

    pub fn proposal_relative_path_for(proposal_id: &str) -> PathBuf {
        PathBuf::from("conversation")
            .join("proposals")
            .join(format!("{}.md", slugify(proposal_id)))
    }

    pub fn write_event(&self, event: &ConversationEvent) -> Result<PathBuf> {
        validate_event(event)?;
        let relative_path = if event.relative_path.trim().is_empty() {
            Self::event_relative_path_for(&event.id)
        } else {
            PathBuf::from(&event.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &ConversationEventFrontmatter::from_event(event, &self.namespace),
            event.notes.trim(),
        )
    }

    pub fn read_event(&self, relative_path: impl AsRef<Path>) -> Result<ConversationEvent> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<ConversationEventFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut event = ConversationEvent::from_document(stored.frontmatter, stored.body)?;
        event.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(event)
    }

    pub fn list_events(&self) -> Result<Vec<ConversationEvent>> {
        let root = self.store.resolve_in_namespace(
            &self.namespace,
            PathBuf::from("conversation").join("events"),
        );
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        for relative in self.markdown_paths_under(&root)? {
            events.push(self.read_event(relative)?);
        }
        events.sort_by(|left, right| {
            left.created_at_unix
                .cmp(&right.created_at_unix)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(events)
    }

    pub fn pending_events(&self, now_unix: u64) -> Result<Vec<ConversationEvent>> {
        Ok(self
            .list_events()?
            .into_iter()
            .filter(|event| {
                event.status == ConversationEventStatus::Pending
                    && event.due_at_unix.is_none_or(|due| due <= now_unix)
            })
            .collect())
    }

    pub fn write_followup(&self, followup: &PendingFollowUp) -> Result<PathBuf> {
        validate_followup(followup)?;
        let relative_path = if followup.relative_path.trim().is_empty() {
            Self::followup_relative_path_for(&followup.id)
        } else {
            PathBuf::from(&followup.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &PendingFollowUpFrontmatter::from_followup(followup, &self.namespace),
            followup.notes.trim(),
        )
    }

    pub fn read_followup(&self, relative_path: impl AsRef<Path>) -> Result<PendingFollowUp> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<PendingFollowUpFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut followup = PendingFollowUp::from_document(stored.frontmatter, stored.body)?;
        followup.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(followup)
    }

    pub fn list_followups(&self) -> Result<Vec<PendingFollowUp>> {
        let root = self.store.resolve_in_namespace(
            &self.namespace,
            PathBuf::from("conversation").join("followups"),
        );
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut followups = Vec::new();
        for relative in self.markdown_paths_under(&root)? {
            followups.push(self.read_followup(relative)?);
        }
        followups.sort_by(|left, right| {
            left.due_at_unix
                .cmp(&right.due_at_unix)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(followups)
    }

    pub fn due_followups(&self, now_unix: u64) -> Result<Vec<PendingFollowUp>> {
        Ok(self
            .list_followups()?
            .into_iter()
            .filter(|followup| {
                followup.status == FollowUpStatus::Pending && followup.due_at_unix <= now_unix
            })
            .collect())
    }

    pub fn write_proposal(&self, proposal: &AgentTurnProposal) -> Result<PathBuf> {
        validate_proposal(proposal)?;
        let relative_path = if proposal.relative_path.trim().is_empty() {
            Self::proposal_relative_path_for(&proposal.id)
        } else {
            PathBuf::from(&proposal.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &AgentTurnProposalFrontmatter::from_proposal(proposal, &self.namespace),
            proposal.notes.trim(),
        )
    }

    pub fn read_proposal(&self, relative_path: impl AsRef<Path>) -> Result<AgentTurnProposal> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<AgentTurnProposalFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut proposal = AgentTurnProposal::from_document(stored.frontmatter, stored.body)?;
        proposal.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(proposal)
    }

    pub fn list_proposals(&self) -> Result<Vec<AgentTurnProposal>> {
        let root = self.store.resolve_in_namespace(
            &self.namespace,
            PathBuf::from("conversation").join("proposals"),
        );
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut proposals = Vec::new();
        for relative in self.markdown_paths_under(&root)? {
            proposals.push(self.read_proposal(relative)?);
        }
        proposals.sort_by(|left, right| {
            left.created_at_unix
                .cmp(&right.created_at_unix)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(proposals)
    }

    pub fn pending_proposals(&self) -> Result<Vec<AgentTurnProposal>> {
        Ok(self
            .list_proposals()?
            .into_iter()
            .filter(|proposal| proposal.status == AgentTurnProposalStatus::Pending)
            .collect())
    }

    pub fn get_proposal(&self, proposal_id: &str) -> Result<AgentTurnProposal> {
        self.read_proposal(Self::proposal_relative_path_for(proposal_id))
    }

    pub fn set_proposal_status(
        &self,
        proposal_id: &str,
        status: AgentTurnProposalStatus,
    ) -> Result<AgentTurnProposal> {
        let mut proposal = self.get_proposal(proposal_id)?;
        proposal.status = status;
        self.write_proposal(&proposal)?;
        Ok(proposal)
    }

    fn markdown_paths_under(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let namespace_root = self.store.resolve_in_namespace(&self.namespace, "");
        let mut paths = Vec::new();
        collect_markdown_files(root, &mut paths)?;
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                path.strip_prefix(&namespace_root)
                    .with_context(|| format!("path not under namespace: {}", path.display()))
                    .map(Path::to_path_buf)
            })
            .collect()
    }
}

impl ConversationEvent {
    pub fn new(kind: impl Into<String>) -> Self {
        let now = now_unix();
        let kind = kind.into();
        Self {
            id: format!("conversation-event.{now}"),
            kind,
            status: ConversationEventStatus::Pending,
            room_id: None,
            agent_id: None,
            payload: Map::new(),
            created_at_unix: now,
            due_at_unix: None,
            tags: vec!["conversation".to_owned()],
            notes: String::new(),
            relative_path: String::new(),
        }
    }

    fn from_document(frontmatter: ConversationEventFrontmatter, body: String) -> Result<Self> {
        if frontmatter.r#type != "conversation_event" {
            bail!(
                "unsupported conversation event type: {}",
                frontmatter.r#type
            );
        }
        Ok(Self {
            id: frontmatter.id,
            kind: frontmatter.kind,
            status: frontmatter.status,
            room_id: frontmatter.room_id,
            agent_id: frontmatter.agent_id,
            payload: frontmatter.payload,
            created_at_unix: frontmatter.created_at_unix,
            due_at_unix: frontmatter.due_at_unix,
            tags: frontmatter.tags,
            notes: body.trim().to_owned(),
            relative_path: String::new(),
        })
    }
}

impl PendingFollowUp {
    pub fn new(agent_id: impl Into<String>, trigger: impl Into<String>, due_at_unix: u64) -> Self {
        let now = now_unix();
        let agent_id = agent_id.into();
        let trigger = trigger.into();
        Self {
            id: format!("conversation-followup.{now}"),
            agent_id,
            trigger,
            room_id: None,
            status: FollowUpStatus::Pending,
            due_at_unix,
            payload: Map::new(),
            notes: String::new(),
            relative_path: String::new(),
        }
    }

    fn from_document(frontmatter: PendingFollowUpFrontmatter, body: String) -> Result<Self> {
        if frontmatter.r#type != "conversation_followup" {
            bail!(
                "unsupported conversation followup type: {}",
                frontmatter.r#type
            );
        }
        Ok(Self {
            id: frontmatter.id,
            agent_id: frontmatter.agent_id,
            trigger: frontmatter.trigger,
            room_id: frontmatter.room_id,
            status: frontmatter.status,
            due_at_unix: frontmatter.due_at_unix,
            payload: frontmatter.payload,
            notes: body.trim().to_owned(),
            relative_path: String::new(),
        })
    }
}

impl AgentTurnProposal {
    pub fn new(agent_id: impl Into<String>, kind: impl Into<String>) -> Self {
        let now = now_unix();
        let agent_id = agent_id.into();
        let kind = kind.into();
        Self {
            id: format!("conversation-proposal.{now}"),
            agent_id,
            kind,
            room_id: None,
            status: AgentTurnProposalStatus::Pending,
            source_event_id: None,
            source_followup_id: None,
            payload: Map::new(),
            created_at_unix: now,
            notes: String::new(),
            relative_path: String::new(),
        }
    }

    fn from_document(frontmatter: AgentTurnProposalFrontmatter, body: String) -> Result<Self> {
        if frontmatter.r#type != "agent_turn_proposal" {
            bail!(
                "unsupported agent turn proposal type: {}",
                frontmatter.r#type
            );
        }
        Ok(Self {
            id: frontmatter.id,
            agent_id: frontmatter.agent_id,
            kind: frontmatter.kind,
            room_id: frontmatter.room_id,
            status: frontmatter.status,
            source_event_id: frontmatter.source_event_id,
            source_followup_id: frontmatter.source_followup_id,
            payload: frontmatter.payload,
            created_at_unix: frontmatter.created_at_unix,
            notes: body.trim().to_owned(),
            relative_path: String::new(),
        })
    }
}

impl ConversationEventFrontmatter {
    fn from_event(event: &ConversationEvent, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: event.id.clone(),
            r#type: "conversation_event".to_owned(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            kind: event.kind.clone(),
            status: event.status.clone(),
            room_id: event.room_id.clone(),
            agent_id: event.agent_id.clone(),
            payload: event.payload.clone(),
            created_at_unix: event.created_at_unix,
            due_at_unix: event.due_at_unix,
            tags: event.tags.clone(),
        }
    }
}

impl PendingFollowUpFrontmatter {
    fn from_followup(followup: &PendingFollowUp, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: followup.id.clone(),
            r#type: "conversation_followup".to_owned(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            agent_id: followup.agent_id.clone(),
            trigger: followup.trigger.clone(),
            room_id: followup.room_id.clone(),
            status: followup.status.clone(),
            due_at_unix: followup.due_at_unix,
            payload: followup.payload.clone(),
        }
    }
}

impl AgentTurnProposalFrontmatter {
    fn from_proposal(proposal: &AgentTurnProposal, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: proposal.id.clone(),
            r#type: "agent_turn_proposal".to_owned(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            agent_id: proposal.agent_id.clone(),
            kind: proposal.kind.clone(),
            room_id: proposal.room_id.clone(),
            status: proposal.status.clone(),
            source_event_id: proposal.source_event_id.clone(),
            source_followup_id: proposal.source_followup_id.clone(),
            payload: proposal.payload.clone(),
            created_at_unix: proposal.created_at_unix,
        }
    }
}

pub fn now_unix() -> u64 {
    hc_bootstrap::unix_timestamp_secs()
}

fn validate_event(event: &ConversationEvent) -> Result<()> {
    if event.id.trim().is_empty() {
        bail!("conversation event id cannot be empty");
    }
    if event.kind.trim().is_empty() {
        bail!("conversation event kind cannot be empty");
    }
    Ok(())
}

fn validate_followup(followup: &PendingFollowUp) -> Result<()> {
    if followup.id.trim().is_empty() {
        bail!("conversation followup id cannot be empty");
    }
    if followup.agent_id.trim().is_empty() {
        bail!("conversation followup agent_id cannot be empty");
    }
    if followup.trigger.trim().is_empty() {
        bail!("conversation followup trigger cannot be empty");
    }
    Ok(())
}

fn validate_proposal(proposal: &AgentTurnProposal) -> Result<()> {
    if proposal.id.trim().is_empty() {
        bail!("agent turn proposal id cannot be empty");
    }
    if proposal.agent_id.trim().is_empty() {
        bail!("agent turn proposal agent_id cannot be empty");
    }
    if proposal.kind.trim().is_empty() {
        bail!("agent turn proposal kind cannot be empty");
    }
    Ok(())
}

fn collect_markdown_files(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, paths)?;
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    Ok(())
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '.' | '-' | '_') {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hc-conversation-{name}-{}", now_unix()))
    }

    #[test]
    fn event_round_trips_and_filters_pending() {
        let repo = ConversationRepository::new(temp_root("event"));
        let mut event = ConversationEvent::new("order.status_changed");
        event.id = "event.order.1".to_owned();
        event.due_at_unix = Some(10);
        event.agent_id = Some("agent.careos.food_delivery".to_owned());
        repo.write_event(&event).unwrap();

        assert!(repo.pending_events(9).unwrap().is_empty());
        let pending = repo.pending_events(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, "order.status_changed");
    }

    #[test]
    fn followup_round_trips_and_filters_due() {
        let repo = ConversationRepository::new(temp_root("followup"));
        let mut followup = PendingFollowUp::new("agent.demo", "tool_result", 20);
        followup.id = "followup.demo.1".to_owned();
        repo.write_followup(&followup).unwrap();

        assert!(repo.due_followups(19).unwrap().is_empty());
        let due = repo.due_followups(20).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].agent_id, "agent.demo");
    }

    #[test]
    fn proposal_round_trips_and_filters_pending() {
        let repo = ConversationRepository::new(temp_root("proposal"));
        let mut proposal = AgentTurnProposal::new("agent.demo", "proactive");
        proposal.id = "proposal.demo.1".to_owned();
        repo.write_proposal(&proposal).unwrap();

        let pending = repo.pending_proposals().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].agent_id, "agent.demo");
    }
}
