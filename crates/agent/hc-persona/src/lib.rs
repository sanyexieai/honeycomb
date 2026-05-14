//! Persona definitions for human and agent participants.

use anyhow::Result;
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaNamespace {
    pub tenant_id: String,
    pub user_id: String,
}

impl PersonaNamespace {
    pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        }
    }

    pub fn local_default() -> Self {
        Self::new("local", "default")
    }
}

impl Default for PersonaNamespace {
    fn default() -> Self {
        Self::local_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersonaVisibility {
    Private,
    TenantShared,
    CrossTenantShared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersonaKind {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersonaLifecycle {
    Seed,
    Incubating,
    Stable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CollaborationRules {
    pub auto_claim: bool,
    pub default_reply_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonaProfile {
    pub id: String,
    #[serde(default)]
    pub namespace: PersonaNamespace,
    #[serde(default = "default_persona_visibility")]
    pub visibility: PersonaVisibility,
    pub kind: PersonaKind,
    pub lifecycle: PersonaLifecycle,
    pub name: String,
    pub role: String,
    pub description: String,
    pub style: String,
    pub goals: Vec<String>,
    pub collaboration_rules: CollaborationRules,
    pub capability_refs: Vec<String>,
    pub default_memory_scope_refs: Vec<String>,
}

impl PersonaProfile {
    pub fn new(
        id: impl Into<String>,
        namespace: PersonaNamespace,
        kind: PersonaKind,
        lifecycle: PersonaLifecycle,
        name: impl Into<String>,
        role: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            namespace,
            visibility: default_persona_visibility(),
            kind,
            lifecycle,
            name: name.into(),
            role: role.into(),
            description: String::new(),
            style: String::new(),
            goals: Vec::new(),
            collaboration_rules: CollaborationRules::default(),
            capability_refs: Vec::new(),
            default_memory_scope_refs: Vec::new(),
        }
    }
}

fn default_persona_visibility() -> PersonaVisibility {
    PersonaVisibility::Private
}

pub fn seed_persona_for_role(
    namespace: PersonaNamespace,
    task_id: &str,
    proposed_name: &str,
    role: &str,
    goal: &str,
) -> PersonaProfile {
    let mut profile = PersonaProfile::new(
        format!("persona.seed.{task_id}.{role}"),
        namespace,
        PersonaKind::Agent,
        PersonaLifecycle::Seed,
        proposed_name,
        role,
    );

    profile.description = format!("Seed persona for role `{role}` in task `{task_id}`.");
    profile.style = match role {
        "planner" => "structured and anticipatory".to_owned(),
        "reviewer" => "critical and careful".to_owned(),
        "worker" => "practical and execution-focused".to_owned(),
        _ => "collaborative and adaptive".to_owned(),
    };
    profile.goals.push(goal.to_owned());
    profile.collaboration_rules.auto_claim = true;
    profile.collaboration_rules.default_reply_mode = Some("nominate_first".to_owned());
    profile.capability_refs = vec![format!("capability.seed.{role}")];
    profile.default_memory_scope_refs = vec![format!("memory_scope.task.{task_id}")];

    profile
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersonaFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    visibility: PersonaVisibility,
    kind: PersonaKind,
    lifecycle: PersonaLifecycle,
    role: String,
    style: String,
    capability_refs: Vec<String>,
    default_memory_scope_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PersonaRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl PersonaRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(profile: &PersonaProfile) -> PathBuf {
        PathBuf::from("personas").join(format!("{}.md", profile.id))
    }

    pub fn write_profile(&self, profile: &PersonaProfile) -> Result<PathBuf> {
        let frontmatter = PersonaFrontmatter::from_profile(profile, &self.namespace);
        let body = render_persona_body(profile);
        self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::relative_path_for(profile),
            &frontmatter,
            &body,
        )
    }

    pub fn read_profile(&self, relative_path: impl AsRef<Path>) -> Result<PersonaProfile> {
        let stored: StoredMarkdown<PersonaFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(PersonaProfile::from_document(
            stored.frontmatter,
            stored.body,
        ))
    }
}

impl PersonaProfile {
    fn from_document(frontmatter: PersonaFrontmatter, body: String) -> Self {
        Self {
            id: frontmatter.id,
            namespace: PersonaNamespace::new(frontmatter.tenant_id, frontmatter.user_id),
            visibility: frontmatter.visibility,
            kind: frontmatter.kind,
            lifecycle: frontmatter.lifecycle,
            name: frontmatter.title,
            role: frontmatter.role,
            description: extract_persona_description(&body),
            style: frontmatter.style,
            goals: extract_persona_goals(&body),
            collaboration_rules: CollaborationRules::default(),
            capability_refs: frontmatter.capability_refs,
            default_memory_scope_refs: frontmatter.default_memory_scope_refs,
        }
    }
}

impl PersonaFrontmatter {
    fn from_profile(profile: &PersonaProfile, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: profile.id.clone(),
            r#type: "persona".to_owned(),
            title: profile.name.clone(),
            tenant_id: if profile.namespace == PersonaNamespace::local_default() {
                namespace.tenant_id.clone()
            } else {
                profile.namespace.tenant_id.clone()
            },
            user_id: if profile.namespace == PersonaNamespace::local_default() {
                namespace.user_id.clone()
            } else {
                profile.namespace.user_id.clone()
            },
            visibility: profile.visibility.clone(),
            kind: profile.kind.clone(),
            lifecycle: profile.lifecycle.clone(),
            role: profile.role.clone(),
            style: profile.style.clone(),
            capability_refs: profile.capability_refs.clone(),
            default_memory_scope_refs: profile.default_memory_scope_refs.clone(),
        }
    }
}

fn render_persona_body(profile: &PersonaProfile) -> String {
    let mut body = format!("# {}\n\n{}\n", profile.name, profile.description);

    if !profile.goals.is_empty() {
        body.push_str("\n## Goals\n\n");
        for goal in &profile.goals {
            body.push_str(&format!("- {}\n", goal));
        }
    }

    body
}

fn extract_persona_description(body: &str) -> String {
    body.lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| !line.trim_start().starts_with("## Goals"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn extract_persona_goals(body: &str) -> Vec<String> {
    let mut in_goals = false;
    let mut goals = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "## Goals" {
            in_goals = true;
            continue;
        }
        if !in_goals {
            continue;
        }
        if trimmed.starts_with("## ") {
            break;
        }
        if let Some(goal) = trimmed.strip_prefix("- ") {
            goals.push(goal.to_owned());
        }
    }

    goals
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
