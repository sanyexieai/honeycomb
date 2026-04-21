use anyhow::Result;
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponderKind {
    Llm,
    Human,
    Rule,
    Script,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmResponderConfig {
    pub provider: String,
    pub model: String,
    pub system_prompt: Option<String>,
}

impl LlmResponderConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            system_prompt: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanResponderConfig {
    pub user_ref: Option<String>,
    pub queue_id: Option<String>,
}

impl HumanResponderConfig {
    pub fn new(user_ref: Option<String>, queue_id: Option<String>) -> Self {
        Self { user_ref, queue_id }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleResponderConfig {
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptResponderConfig {
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponderBinding {
    Llm(LlmResponderConfig),
    Human(HumanResponderConfig),
    Rule(RuleResponderConfig),
    Script(ScriptResponderConfig),
}

impl ResponderBinding {
    pub fn kind(&self) -> ResponderKind {
        match self {
            Self::Llm(_) => ResponderKind::Llm,
            Self::Human(_) => ResponderKind::Human,
            Self::Rule(_) => ResponderKind::Rule,
            Self::Script(_) => ResponderKind::Script,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Llm(config) => format!("{}/{}", config.provider, config.model),
            Self::Human(config) => format!(
                "human:{}",
                config
                    .user_ref
                    .clone()
                    .unwrap_or_else(|| "local".to_owned())
            ),
            Self::Rule(config) => format!(
                "rule:{}",
                config
                    .profile
                    .clone()
                    .unwrap_or_else(|| "default".to_owned())
            ),
            Self::Script(config) => format!("script:{}", config.command),
        }
    }

    pub fn as_llm(&self) -> Option<&LlmResponderConfig> {
        match self {
            Self::Llm(config) => Some(config),
            _ => None,
        }
    }

    pub fn as_human(&self) -> Option<&HumanResponderConfig> {
        match self {
            Self::Human(config) => Some(config),
            _ => None,
        }
    }

    pub fn is_human(&self) -> bool {
        matches!(self, Self::Human(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyRequest {
    pub source_message_id: String,
    pub source_session_id: String,
    pub source_from_instance_id: String,
    pub source_body: String,
    pub replying_instance_id: String,
    pub replying_agent_name: String,
    pub replying_role: String,
    pub responder: ResponderBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyResponse {
    pub body: String,
}

impl ReplyResponse {
    pub fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }
}

pub trait ResponderBackend {
    fn generate_reply(&self, request: &ReplyRequest) -> Result<ReplyResponse>;
}

pub fn require_llm(binding: &ResponderBinding) -> Result<&LlmResponderConfig> {
    binding
        .as_llm()
        .ok_or_else(|| anyhow::anyhow!("responder is not llm-backed"))
}

pub fn require_human(binding: &ResponderBinding) -> Result<&HumanResponderConfig> {
    binding
        .as_human()
        .ok_or_else(|| anyhow::anyhow!("responder is not human-backed"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HumanInboxStatus {
    Pending,
    Answered,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanInboxItem {
    pub id: String,
    pub responder_user_ref: String,
    pub queue_id: String,
    pub replying_instance_id: String,
    pub replying_agent_name: String,
    pub replying_role: String,
    pub source_message_id: String,
    pub source_session_id: String,
    pub source_from_instance_id: String,
    pub source_body: String,
    pub status: HumanInboxStatus,
    pub response_body: Option<String>,
    pub created_at_ms: u64,
    pub answered_at_ms: Option<u64>,
}

impl HumanInboxItem {
    pub fn from_reply_request(
        request: &ReplyRequest,
        responder_user_ref: impl Into<String>,
        queue_id: impl Into<String>,
        created_at_ms: u64,
    ) -> Self {
        Self {
            id: format!(
                "human-inbox.{}.{}",
                request.source_message_id, request.replying_instance_id
            ),
            responder_user_ref: responder_user_ref.into(),
            queue_id: queue_id.into(),
            replying_instance_id: request.replying_instance_id.clone(),
            replying_agent_name: request.replying_agent_name.clone(),
            replying_role: request.replying_role.clone(),
            source_message_id: request.source_message_id.clone(),
            source_session_id: request.source_session_id.clone(),
            source_from_instance_id: request.source_from_instance_id.clone(),
            source_body: request.source_body.clone(),
            status: HumanInboxStatus::Pending,
            response_body: None,
            created_at_ms,
            answered_at_ms: None,
        }
    }

    pub fn answer(mut self, body: impl Into<String>, answered_at_ms: u64) -> Self {
        self.status = HumanInboxStatus::Answered;
        self.response_body = Some(body.into());
        self.answered_at_ms = Some(answered_at_ms);
        self
    }

    pub fn complete(mut self) -> Self {
        self.status = HumanInboxStatus::Completed;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct HumanInboxFrontmatter {
    id: String,
    r#type: String,
    responder_user_ref: String,
    queue_id: String,
    replying_instance_id: String,
    replying_agent_name: String,
    replying_role: String,
    source_message_id: String,
    source_session_id: String,
    source_from_instance_id: String,
    status: HumanInboxStatus,
    created_at_ms: u64,
    answered_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct HumanInboxRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl HumanInboxRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn namespace(&self) -> &WorkspaceNamespace {
        &self.namespace
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn write_pending(&self, item: &HumanInboxItem) -> Result<PathBuf> {
        self.write_in_status_dir("inbox/pending", item)
    }

    pub fn mark_answered(
        &self,
        item_id: &str,
        response_body: impl Into<String>,
        answered_at_ms: u64,
    ) -> Result<PathBuf> {
        let item = self
            .read_pending(item_id)?
            .answer(response_body, answered_at_ms);
        self.remove_if_exists(Self::relative_path("inbox/pending", item_id))?;
        self.write_in_status_dir("inbox/answered", &item)
    }

    pub fn mark_completed(&self, item_id: &str) -> Result<PathBuf> {
        let item = self.read_answered(item_id)?.complete();
        self.remove_if_exists(Self::relative_path("inbox/answered", item_id))?;
        self.write_in_status_dir("inbox/completed", &item)
    }

    pub fn complete_pending(
        &self,
        item_id: &str,
        response_body: impl Into<String>,
        answered_at_ms: u64,
    ) -> Result<PathBuf> {
        let item = self
            .read_pending(item_id)?
            .answer(response_body, answered_at_ms)
            .complete();
        self.remove_if_exists(Self::relative_path("inbox/pending", item_id))?;
        self.write_in_status_dir("inbox/completed", &item)
    }

    pub fn list_pending(&self) -> Result<Vec<HumanInboxItem>> {
        self.read_all_from("inbox/pending")
    }

    pub fn list_answered(&self) -> Result<Vec<HumanInboxItem>> {
        self.read_all_from("inbox/answered")
    }

    pub fn read_pending(&self, item_id: &str) -> Result<HumanInboxItem> {
        self.read_from("inbox/pending", item_id)
    }

    pub fn read_answered(&self, item_id: &str) -> Result<HumanInboxItem> {
        self.read_from("inbox/answered", item_id)
    }

    fn write_in_status_dir(&self, dir: &str, item: &HumanInboxItem) -> Result<PathBuf> {
        let frontmatter = HumanInboxFrontmatter::from_item(item);
        let body = render_human_inbox_body(item);
        self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::relative_path(dir, &item.id),
            &frontmatter,
            &body,
        )
    }

    fn read_from(&self, dir: &str, item_id: &str) -> Result<HumanInboxItem> {
        let stored: StoredMarkdown<HumanInboxFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, Self::relative_path(dir, item_id))?;
        Ok(HumanInboxItem::from_document(
            stored.frontmatter,
            stored.body,
        ))
    }

    fn read_all_from(&self, dir: &str) -> Result<Vec<HumanInboxItem>> {
        let root = self
            .store
            .resolve_in_namespace(&self.namespace, PathBuf::from(dir));
        if !root.exists() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let relative = path
                .strip_prefix(self.store.resolve_in_namespace(&self.namespace, ""))
                .map(PathBuf::from)
                .map_err(|error| anyhow::anyhow!("failed to relativize path: {error}"))?;
            let stored: StoredMarkdown<HumanInboxFrontmatter> = self
                .store
                .read_markdown_in_namespace(&self.namespace, &relative)?;
            items.push(HumanInboxItem::from_document(
                stored.frontmatter,
                stored.body,
            ));
        }
        items.sort_by(|left, right| left.created_at_ms.cmp(&right.created_at_ms));
        Ok(items)
    }

    fn remove_if_exists(&self, relative_path: PathBuf) -> Result<()> {
        let absolute = self
            .store
            .resolve_in_namespace(&self.namespace, &relative_path);
        if absolute.exists() {
            fs::remove_file(&absolute)?;
        }
        Ok(())
    }

    fn relative_path(dir: &str, item_id: &str) -> PathBuf {
        PathBuf::from(dir).join(format!("{item_id}.md"))
    }
}

impl HumanInboxItem {
    fn from_document(frontmatter: HumanInboxFrontmatter, body: String) -> Self {
        Self {
            id: frontmatter.id,
            responder_user_ref: frontmatter.responder_user_ref,
            queue_id: frontmatter.queue_id,
            replying_instance_id: frontmatter.replying_instance_id,
            replying_agent_name: frontmatter.replying_agent_name,
            replying_role: frontmatter.replying_role,
            source_message_id: frontmatter.source_message_id,
            source_session_id: frontmatter.source_session_id,
            source_from_instance_id: frontmatter.source_from_instance_id,
            source_body: extract_source_body(&body),
            status: frontmatter.status,
            response_body: extract_response_body(&body),
            created_at_ms: frontmatter.created_at_ms,
            answered_at_ms: frontmatter.answered_at_ms,
        }
    }
}

impl HumanInboxFrontmatter {
    fn from_item(item: &HumanInboxItem) -> Self {
        Self {
            id: item.id.clone(),
            r#type: "human_inbox".to_owned(),
            responder_user_ref: item.responder_user_ref.clone(),
            queue_id: item.queue_id.clone(),
            replying_instance_id: item.replying_instance_id.clone(),
            replying_agent_name: item.replying_agent_name.clone(),
            replying_role: item.replying_role.clone(),
            source_message_id: item.source_message_id.clone(),
            source_session_id: item.source_session_id.clone(),
            source_from_instance_id: item.source_from_instance_id.clone(),
            status: item.status.clone(),
            created_at_ms: item.created_at_ms,
            answered_at_ms: item.answered_at_ms,
        }
    }
}

fn render_human_inbox_body(item: &HumanInboxItem) -> String {
    let mut body = format!(
        "# Reply As {}\n\n{}\n",
        item.replying_agent_name, item.source_body
    );
    if let Some(response) = &item.response_body {
        body.push_str("\n---\n\n");
        body.push_str(response);
        body.push('\n');
    }
    body
}

fn extract_source_body(body: &str) -> String {
    body.lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| line.trim() != "---")
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn extract_response_body(body: &str) -> Option<String> {
    let (_, response) = body.split_once("\n---\n\n")?;
    let response = response.trim();
    if response.is_empty() {
        None
    } else {
        Some(response.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_item_round_trip_answer_flow() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = HumanInboxRepository::with_namespace(
            temp.path(),
            WorkspaceNamespace::new("tenant-a", "user-b"),
        );
        let request = ReplyRequest {
            source_message_id: "message.1".to_owned(),
            source_session_id: "session.1".to_owned(),
            source_from_instance_id: "instance.alice".to_owned(),
            source_body: "please review this".to_owned(),
            replying_instance_id: "instance.reviewer".to_owned(),
            replying_agent_name: "reviewer".to_owned(),
            replying_role: "reviewer".to_owned(),
            responder: ResponderBinding::Human(HumanResponderConfig::new(
                Some("user-b".to_owned()),
                Some("queue.default".to_owned()),
            )),
        };

        let item = HumanInboxItem::from_reply_request(&request, "user-b", "queue.default", 100);
        repo.write_pending(&item).expect("write pending");
        assert_eq!(repo.list_pending().expect("list pending").len(), 1);

        repo.mark_answered(&item.id, "looks good", 200)
            .expect("mark answered");
        let answered = repo.read_answered(&item.id).expect("read answered");
        assert_eq!(answered.response_body.as_deref(), Some("looks good"));

        repo.mark_completed(&item.id).expect("mark completed");
        assert!(repo.list_answered().expect("list answered").is_empty());
    }
}
