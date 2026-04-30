use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hc_conversation::ConversationPolicy;
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    DomainService,
    TaskRole,
    Router,
    Guard,
    Other,
}

impl Default for AgentKind {
    fn default() -> Self {
        Self::DomainService
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub intent_hints: Vec<String>,
    #[serde(default)]
    pub routing_examples: Vec<String>,
    #[serde(default)]
    pub negative_routing_examples: Vec<String>,
    #[serde(default)]
    pub tool_refs: Vec<String>,
    #[serde(default)]
    pub memory_scope_refs: Vec<String>,
    #[serde(default)]
    pub prompt_refs: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_schema_ref: Option<String>,
    #[serde(default)]
    pub conversation_policy: ConversationPolicy,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub relative_path: String,
}

impl AgentProfile {
    pub fn summary(&self) -> AgentProfileSummary {
        AgentProfileSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: agent_kind_label(&self.kind).to_owned(),
            project_id: self.project_id.clone(),
            domain_id: self.domain_id.clone(),
            priority: self.priority,
            intent_hints: self.intent_hints.clone(),
            routing_examples: self.routing_examples.clone(),
            negative_routing_examples: self.negative_routing_examples.clone(),
            tool_refs: self.tool_refs.clone(),
            memory_scope_refs: self.memory_scope_refs.clone(),
            tags: self.tags.clone(),
            conversation_policy: self.conversation_policy.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub domain_id: Option<String>,
    pub priority: i32,
    pub intent_hints: Vec<String>,
    pub routing_examples: Vec<String>,
    pub negative_routing_examples: Vec<String>,
    pub tool_refs: Vec<String>,
    pub memory_scope_refs: Vec<String>,
    pub tags: Vec<String>,
    pub conversation_policy: ConversationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentProfileFrontmatter {
    id: String,
    r#type: String,
    title: String,
    #[serde(default)]
    kind: AgentKind,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    domain_id: Option<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    intent_hints: Vec<String>,
    #[serde(default)]
    routing_examples: Vec<String>,
    #[serde(default)]
    negative_routing_examples: Vec<String>,
    #[serde(default)]
    tool_refs: Vec<String>,
    #[serde(default)]
    memory_scope_refs: Vec<String>,
    #[serde(default)]
    prompt_refs: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    responder_ref: Option<String>,
    #[serde(default)]
    state_schema_ref: Option<String>,
    #[serde(default)]
    conversation_policy: ConversationPolicy,
}

#[derive(Debug, Clone)]
pub struct AgentRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl AgentRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(agent: &AgentProfile) -> PathBuf {
        PathBuf::from("agents").join(format!("{}.md", slugify_agent_path(&agent.id)))
    }

    pub fn read_profile(&self, relative_path: impl AsRef<Path>) -> Result<AgentProfile> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<AgentProfileFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut profile = AgentProfile::from_document(stored.frontmatter, stored.body)?;
        profile.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(profile)
    }

    pub fn list_profiles(&self) -> Result<Vec<AgentProfile>> {
        let root = self
            .store
            .resolve_in_namespace(&self.namespace, PathBuf::from("agents"));
        if !root.exists() {
            return Ok(Vec::new());
        }

        let namespace_root = self.store.resolve_in_namespace(&self.namespace, "");
        let mut paths = Vec::new();
        collect_markdown_files(&root, &mut paths)?;
        paths.sort();

        let mut profiles = Vec::new();
        for path in paths {
            let relative = path
                .strip_prefix(&namespace_root)
                .with_context(|| format!("agent path not under namespace: {}", path.display()))?;
            profiles.push(self.read_profile(relative)?);
        }
        profiles.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(profiles)
    }

    pub fn write_profile(&self, profile: &AgentProfile) -> Result<PathBuf> {
        let relative_path = if profile.relative_path.trim().is_empty() {
            Self::relative_path_for(profile)
        } else {
            PathBuf::from(&profile.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &AgentProfileFrontmatter::from_profile(profile),
            profile.instructions.trim(),
        )
    }
}

impl AgentProfile {
    fn from_document(frontmatter: AgentProfileFrontmatter, body: String) -> Result<Self> {
        if frontmatter.r#type != "agent_profile" {
            anyhow::bail!("unsupported agent profile type: {}", frontmatter.r#type);
        }
        Ok(Self {
            id: frontmatter.id,
            name: frontmatter.title,
            kind: frontmatter.kind,
            project_id: frontmatter.project_id,
            domain_id: frontmatter.domain_id,
            priority: frontmatter.priority,
            intent_hints: frontmatter.intent_hints,
            routing_examples: frontmatter.routing_examples,
            negative_routing_examples: frontmatter.negative_routing_examples,
            tool_refs: frontmatter.tool_refs,
            memory_scope_refs: frontmatter.memory_scope_refs,
            prompt_refs: frontmatter.prompt_refs,
            tags: frontmatter.tags,
            responder_ref: frontmatter.responder_ref,
            state_schema_ref: frontmatter.state_schema_ref,
            conversation_policy: frontmatter.conversation_policy,
            instructions: body.trim().to_owned(),
            relative_path: String::new(),
        })
    }
}

impl AgentProfileFrontmatter {
    fn from_profile(profile: &AgentProfile) -> Self {
        Self {
            id: profile.id.clone(),
            r#type: "agent_profile".to_owned(),
            title: profile.name.clone(),
            kind: profile.kind.clone(),
            project_id: profile.project_id.clone(),
            domain_id: profile.domain_id.clone(),
            priority: profile.priority,
            intent_hints: profile.intent_hints.clone(),
            routing_examples: profile.routing_examples.clone(),
            negative_routing_examples: profile.negative_routing_examples.clone(),
            tool_refs: profile.tool_refs.clone(),
            memory_scope_refs: profile.memory_scope_refs.clone(),
            prompt_refs: profile.prompt_refs.clone(),
            tags: profile.tags.clone(),
            responder_ref: profile.responder_ref.clone(),
            state_schema_ref: profile.state_schema_ref.clone(),
            conversation_policy: profile.conversation_policy.clone(),
        }
    }
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

fn agent_kind_label(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::DomainService => "domain_service",
        AgentKind::TaskRole => "task_role",
        AgentKind::Router => "router",
        AgentKind::Guard => "guard",
        AgentKind::Other => "other",
    }
}

fn slugify_agent_path(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '.' | '-' | '_' | '/') {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_owned()
}
