use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DomainKind {
    Service,
    ProjectArea,
    Safety,
    Other,
}

impl Default for DomainKind {
    fn default() -> Self {
        Self::Service
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: DomainKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub intent_hints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent_id: Option<String>,
    #[serde(default)]
    pub tool_refs: Vec<String>,
    #[serde(default)]
    pub memory_scope_refs: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub relative_path: String,
}

impl DomainProfile {
    pub fn summary(&self) -> DomainProfileSummary {
        DomainProfileSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: domain_kind_label(&self.kind).to_owned(),
            project_id: self.project_id.clone(),
            priority: self.priority,
            intent_hints: self.intent_hints.clone(),
            default_agent_id: self.default_agent_id.clone(),
            tool_refs: self.tool_refs.clone(),
            memory_scope_refs: self.memory_scope_refs.clone(),
            tags: self.tags.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainProfileSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub priority: i32,
    pub intent_hints: Vec<String>,
    pub default_agent_id: Option<String>,
    pub tool_refs: Vec<String>,
    pub memory_scope_refs: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DomainProfileFrontmatter {
    id: String,
    r#type: String,
    title: String,
    #[serde(default)]
    kind: DomainKind,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    intent_hints: Vec<String>,
    #[serde(default)]
    default_agent_id: Option<String>,
    #[serde(default)]
    tool_refs: Vec<String>,
    #[serde(default)]
    memory_scope_refs: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DomainRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl DomainRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn relative_path_for(domain: &DomainProfile) -> PathBuf {
        PathBuf::from("domains").join(format!("{}.md", slugify_domain_path(&domain.id)))
    }

    pub fn read_profile(&self, relative_path: impl AsRef<Path>) -> Result<DomainProfile> {
        let relative_path = relative_path.as_ref();
        let stored: StoredMarkdown<DomainProfileFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        let mut profile = DomainProfile::from_document(stored.frontmatter, stored.body)?;
        profile.relative_path = relative_path.to_string_lossy().replace('\\', "/");
        Ok(profile)
    }

    pub fn list_profiles(&self) -> Result<Vec<DomainProfile>> {
        let root = self
            .store
            .resolve_in_namespace(&self.namespace, PathBuf::from("domains"));
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
                .with_context(|| format!("domain path not under namespace: {}", path.display()))?;
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

    pub fn write_profile(&self, profile: &DomainProfile) -> Result<PathBuf> {
        let relative_path = if profile.relative_path.trim().is_empty() {
            Self::relative_path_for(profile)
        } else {
            PathBuf::from(&profile.relative_path)
        };
        self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &DomainProfileFrontmatter::from_profile(profile),
            profile.description.trim(),
        )
    }
}

impl DomainProfile {
    fn from_document(frontmatter: DomainProfileFrontmatter, body: String) -> Result<Self> {
        if frontmatter.r#type != "domain_profile" {
            anyhow::bail!("unsupported domain profile type: {}", frontmatter.r#type);
        }
        Ok(Self {
            id: frontmatter.id,
            name: frontmatter.title,
            kind: frontmatter.kind,
            project_id: frontmatter.project_id,
            priority: frontmatter.priority,
            intent_hints: frontmatter.intent_hints,
            default_agent_id: frontmatter.default_agent_id,
            tool_refs: frontmatter.tool_refs,
            memory_scope_refs: frontmatter.memory_scope_refs,
            tags: frontmatter.tags,
            description: body.trim().to_owned(),
            relative_path: String::new(),
        })
    }
}

impl DomainProfileFrontmatter {
    fn from_profile(profile: &DomainProfile) -> Self {
        Self {
            id: profile.id.clone(),
            r#type: "domain_profile".to_owned(),
            title: profile.name.clone(),
            kind: profile.kind.clone(),
            project_id: profile.project_id.clone(),
            priority: profile.priority,
            intent_hints: profile.intent_hints.clone(),
            default_agent_id: profile.default_agent_id.clone(),
            tool_refs: profile.tool_refs.clone(),
            memory_scope_refs: profile.memory_scope_refs.clone(),
            tags: profile.tags.clone(),
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

fn domain_kind_label(kind: &DomainKind) -> &'static str {
    match kind {
        DomainKind::Service => "service",
        DomainKind::ProjectArea => "project_area",
        DomainKind::Safety => "safety",
        DomainKind::Other => "other",
    }
}

fn slugify_domain_path(value: &str) -> String {
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
