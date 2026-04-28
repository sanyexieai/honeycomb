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
mod tests {
    use super::{SkillCatalog, SkillProfile, SkillRepository};
    use hc_capability::ModelDependence;
    use hc_store::store::WorkspaceNamespace;
    use hc_toolchain::{ToolExecutionKind, ToolProvider};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn skill_repository_roundtrips_markdown_profile() {
        let root = unique_temp_dir("skill-repo");
        let namespace = WorkspaceNamespace::new("tenant-a", "alice");
        let repository = SkillRepository::with_namespace(&root, namespace.clone());
        let skill = SkillProfile::new("skill.search.workspace", "Workspace Search")
            .with_namespace(namespace)
            .with_description("Searches the current workspace with rg.")
            .with_instructions("Use rg first, then narrow by path if the result set is broad.")
            .with_tool_id("tool.skill.search.workspace")
            .with_execution_kind(ToolExecutionKind::Cli)
            .with_model_dependence(ModelDependence::Optional)
            .with_default_command(["rg", "-n"])
            .with_tool_ref("tool.rg")
            .with_tag("search");

        repository
            .write_profile(&skill)
            .expect("skill profile should be written");
        let loaded = repository
            .read_profile("skills/skill.search.workspace.md")
            .expect("skill profile should be read");

        assert_eq!(loaded.id, skill.id);
        assert_eq!(loaded.tool_id, skill.tool_id);
        assert_eq!(loaded.default_command, skill.default_command);
        assert_eq!(loaded.instructions, skill.instructions);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn skill_catalog_acts_as_tool_provider() {
        let mut catalog = SkillCatalog::new();
        catalog.insert(
            SkillProfile::new("skill.test.runner", "Test Runner")
                .with_description("Runs a narrow test target.")
                .with_instructions("Prefer cargo test <name> over broad test sweeps.")
                .with_execution_kind(ToolExecutionKind::Cli)
                .with_default_command(["cargo", "test"])
                .with_tag("testing"),
        );

        let tools = catalog.list_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].id, "tool.test.runner");
        assert!(tools[0].tags.iter().any(|tag| tag == "skill"));
    }

    #[test]
    fn repository_loads_catalog_from_namespace_skills() {
        let root = unique_temp_dir("skill-catalog");
        let repository = SkillRepository::new(&root);
        repository
            .write_profile(
                &SkillProfile::new("skill.docs.lookup", "Docs Lookup")
                    .with_description("Looks up project documentation.")
                    .with_instructions("Check docs before making code changes.")
                    .with_execution_kind(ToolExecutionKind::Builtin),
            )
            .expect("skill should be written");

        let catalog = repository.load_catalog().expect("catalog should load");
        assert!(catalog.get("skill.docs.lookup").is_some());
        assert_eq!(catalog.list_tools().len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should work")
            .as_nanos();
        std::env::temp_dir().join(format!("hc-{prefix}-{stamp}"))
    }
}
