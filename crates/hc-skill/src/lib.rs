//! Skill profiles and repository support for toolchain-compatible skills.

use anyhow::Result;
use hc_capability::ModelDependence;
use hc_store::store::{MarkdownQuery, StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use hc_toolchain::{ToolComposition, ToolExecutionKind, ToolProvider, ToolSpec, ToolStability};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillProfile {
    pub id: String,
    pub namespace: WorkspaceNamespace,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tool_id: Option<String>,
    pub execution_kind: ToolExecutionKind,
    pub model_dependence: ModelDependence,
    pub default_command: Vec<String>,
    pub tool_refs: Vec<String>,
    pub tags: Vec<String>,
}

impl SkillProfile {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            namespace: WorkspaceNamespace::local_default(),
            name: name.into(),
            description: String::new(),
            instructions: String::new(),
            tool_id: None,
            execution_kind: ToolExecutionKind::Builtin,
            model_dependence: ModelDependence::Optional,
            default_command: Vec::new(),
            tool_refs: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn with_namespace(mut self, namespace: WorkspaceNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    pub fn with_tool_id(mut self, tool_id: impl Into<String>) -> Self {
        self.tool_id = Some(tool_id.into());
        self
    }

    pub fn with_execution_kind(mut self, execution_kind: ToolExecutionKind) -> Self {
        self.execution_kind = execution_kind;
        self
    }

    pub fn with_model_dependence(mut self, model_dependence: ModelDependence) -> Self {
        self.model_dependence = model_dependence;
        self
    }

    pub fn with_default_command<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.default_command = command.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_tool_ref(mut self, tool_ref: impl Into<String>) -> Self {
        self.tool_refs.push(tool_ref.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn resolved_tool_id(&self) -> String {
        if let Some(rest) = self.id.strip_prefix("skill.") {
            format!("tool.{rest}")
        } else {
            format!("tool.skill.{}", self.id)
        }
    }

    pub fn delegated_tool_id(&self) -> Option<&str> {
        self.tool_id
            .as_deref()
            .or_else(|| self.tool_refs.first().map(String::as_str))
    }

    pub fn to_tool_spec(&self) -> ToolSpec {
        let mut tags = self.tags.clone();
        if !tags.iter().any(|tag| tag == "skill") {
            tags.push("skill".to_owned());
        }
        ToolSpec {
            id: self.resolved_tool_id(),
            name: self.name.clone(),
            description: if self.instructions.trim().is_empty() {
                self.description.clone()
            } else if self.description.trim().is_empty() {
                self.instructions
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            } else {
                format!(
                    "{} | {}",
                    self.description,
                    self.instructions.lines().next().unwrap_or_default()
                )
            },
            execution_kind: self.execution_kind.clone(),
            composition: ToolComposition::Composite,
            stability: ToolStability::Managed,
            model_dependence: self.model_dependence.clone(),
            default_command: self.default_command.clone(),
            tags,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SkillFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    tool_id: Option<String>,
    execution_kind: ToolExecutionKind,
    model_dependence: ModelDependence,
    default_command: Vec<String>,
    tool_refs: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    skills: Vec<SkillProfile>,
}

impl SkillCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, skill: SkillProfile) {
        if let Some(existing) = self
            .skills
            .iter_mut()
            .find(|candidate| candidate.id == skill.id)
        {
            *existing = skill;
        } else {
            self.skills.push(skill);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &SkillProfile> {
        self.skills.iter()
    }

    pub fn get(&self, skill_id: &str) -> Option<&SkillProfile> {
        self.skills.iter().find(|skill| skill.id == skill_id)
    }
}

impl ToolProvider for SkillCatalog {
    fn list_tools(&self) -> Vec<ToolSpec> {
        self.skills.iter().map(SkillProfile::to_tool_spec).collect()
    }
}

#[derive(Debug, Clone)]
pub struct SkillRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl SkillRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(profile: &SkillProfile) -> PathBuf {
        PathBuf::from("skills").join(format!("{}.md", profile.id))
    }

    pub fn write_profile(&self, profile: &SkillProfile) -> Result<PathBuf> {
        let frontmatter = SkillFrontmatter::from_profile(profile, &self.namespace);
        let body = render_skill_body(profile);
        let path = self.store.write_markdown_in_namespace(
            &self.namespace,
            Self::relative_path_for(profile),
            &frontmatter,
            &body,
        )?;
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        Ok(path)
    }

    pub fn read_profile(&self, relative_path: impl AsRef<Path>) -> Result<SkillProfile> {
        let stored: StoredMarkdown<SkillFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(SkillProfile::from_document(stored.frontmatter, stored.body))
    }

    pub fn list_profiles(&self) -> Result<Vec<SkillProfile>> {
        let _ = self
            .store
            .rebuild_markdown_index_in_namespace(&self.namespace);
        let query = MarkdownQuery::default()
            .with_path_prefix("skills/")
            .with_limit(500);
        let entries = self
            .store
            .query_markdown_index_in_namespace(&self.namespace, &query)?;
        let mut profiles = Vec::new();
        for entry in entries {
            profiles.push(self.read_profile(entry.relative_path)?);
        }
        profiles.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(profiles)
    }

    pub fn load_catalog(&self) -> Result<SkillCatalog> {
        let mut catalog = SkillCatalog::new();
        for profile in self.list_profiles()? {
            catalog.insert(profile);
        }
        Ok(catalog)
    }
}

impl SkillProfile {
    fn from_document(frontmatter: SkillFrontmatter, body: String) -> Self {
        let (description, instructions) = split_skill_body(&body);
        Self {
            id: frontmatter.id,
            namespace: WorkspaceNamespace::new(frontmatter.tenant_id, frontmatter.user_id),
            name: frontmatter.title,
            description,
            instructions,
            tool_id: frontmatter.tool_id,
            execution_kind: frontmatter.execution_kind,
            model_dependence: frontmatter.model_dependence,
            default_command: frontmatter.default_command,
            tool_refs: frontmatter.tool_refs,
            tags: frontmatter.tags,
        }
    }
}

impl SkillFrontmatter {
    fn from_profile(profile: &SkillProfile, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: profile.id.clone(),
            r#type: "skill".to_owned(),
            title: profile.name.clone(),
            tenant_id: if profile.namespace == WorkspaceNamespace::local_default() {
                namespace.tenant_id.clone()
            } else {
                profile.namespace.tenant_id.clone()
            },
            user_id: if profile.namespace == WorkspaceNamespace::local_default() {
                namespace.user_id.clone()
            } else {
                profile.namespace.user_id.clone()
            },
            tool_id: profile.tool_id.clone(),
            execution_kind: profile.execution_kind.clone(),
            model_dependence: profile.model_dependence.clone(),
            default_command: profile.default_command.clone(),
            tool_refs: profile.tool_refs.clone(),
            tags: profile.tags.clone(),
        }
    }
}

fn render_skill_body(profile: &SkillProfile) -> String {
    let mut body = format!("# {}\n\n{}\n", profile.name, profile.description);
    body.push_str("\n## Instructions\n\n");
    body.push_str(profile.instructions.trim());
    body.push('\n');
    body
}

fn split_skill_body(body: &str) -> (String, String) {
    let content = body.trim();
    let without_title = if let Some(rest) = content.strip_prefix("# ") {
        rest.split_once('\n')
            .map(|(_, remaining)| remaining.trim())
            .unwrap_or("")
    } else {
        content
    };

    if let Some((description, instructions)) = without_title.split_once("\n## Instructions\n") {
        (
            description.trim().to_owned(),
            instructions.trim().to_owned(),
        )
    } else {
        (without_title.trim().to_owned(), String::new())
    }
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
