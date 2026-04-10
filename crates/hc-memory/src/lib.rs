//! Memory records, queries, and lightweight writeback helpers.

use anyhow::Result;
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Global,
    Persona,
    Session,
    Instance,
    Project,
    Task,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Summary,
    Decision,
    Preference,
    Knowledge,
    WorkflowMemory,
}

pub type MemoryType = MemoryKind;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOwnerKind {
    Global,
    Persona,
    Session,
    Instance,
    Project,
    Task,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryNamespace {
    pub tenant_id: String,
    pub user_id: String,
}

impl MemoryNamespace {
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

impl Default for MemoryNamespace {
    fn default() -> Self {
        Self::local_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVisibility {
    Private,
    TenantShared,
    CrossTenantShared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOwnerRef {
    pub kind: MemoryOwnerKind,
    pub id: String,
}

impl MemoryOwnerRef {
    pub fn new(kind: MemoryOwnerKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub fn global() -> Self {
        Self::new(MemoryOwnerKind::Global, "global")
    }

    pub fn persona(id: impl Into<String>) -> Self {
        Self::new(MemoryOwnerKind::Persona, id)
    }

    pub fn session(id: impl Into<String>) -> Self {
        Self::new(MemoryOwnerKind::Session, id)
    }

    pub fn instance(id: impl Into<String>) -> Self {
        Self::new(MemoryOwnerKind::Instance, id)
    }

    pub fn project(id: impl Into<String>) -> Self {
        Self::new(MemoryOwnerKind::Project, id)
    }

    pub fn task(id: impl Into<String>) -> Self {
        Self::new(MemoryOwnerKind::Task, id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: String,
    #[serde(default)]
    pub namespace: MemoryNamespace,
    #[serde(default = "default_memory_visibility")]
    pub visibility: MemoryVisibility,
    pub scope: MemoryScope,
    pub owner: MemoryOwnerRef,
    pub kind: MemoryKind,
    pub title: String,
    pub summary: String,
    pub content_ref: Option<String>,
    pub tags: Vec<String>,
    pub derived_from: Vec<String>,
    pub confidence_milli: u16,
}

impl MemoryRecord {
    pub fn new(
        id: impl Into<String>,
        scope: MemoryScope,
        owner: MemoryOwnerRef,
        kind: MemoryKind,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            namespace: MemoryNamespace::local_default(),
            visibility: default_memory_visibility(),
            scope,
            owner,
            kind,
            title: title.into(),
            summary: summary.into(),
            content_ref: None,
            tags: Vec::new(),
            derived_from: Vec::new(),
            confidence_milli: 750,
        }
    }

    pub fn task_summary(
        task_id: impl Into<String>,
        instance_id: impl Into<String>,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        let task_id = task_id.into();
        let instance_id = instance_id.into();
        Self::new(
            format!("memory.task.{}.{}", task_id, instance_id),
            MemoryScope::Task,
            MemoryOwnerRef::task(task_id),
            MemoryKind::Summary,
            title,
            summary,
        )
        .with_derived_from(instance_id)
    }

    pub fn with_content_ref(mut self, content_ref: impl Into<String>) -> Self {
        self.content_ref = Some(content_ref.into());
        self
    }

    pub fn with_namespace(mut self, namespace: MemoryNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    pub fn with_visibility(mut self, visibility: MemoryVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_derived_from(mut self, source: impl Into<String>) -> Self {
        self.derived_from.push(source.into());
        self
    }

    pub fn with_confidence_milli(mut self, confidence_milli: u16) -> Self {
        self.confidence_milli = confidence_milli.min(1000);
        self
    }

    pub fn is_visible_to(&self, namespace: &MemoryNamespace) -> bool {
        match self.visibility {
            MemoryVisibility::Private => self.namespace == *namespace,
            MemoryVisibility::TenantShared => self.namespace.tenant_id == namespace.tenant_id,
            MemoryVisibility::CrossTenantShared => true,
        }
    }
}

fn default_memory_visibility() -> MemoryVisibility {
    MemoryVisibility::Private
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryQuery {
    pub namespace: Option<MemoryNamespace>,
    pub scope: Option<MemoryScope>,
    pub owner: Option<MemoryOwnerRef>,
    pub kind: Option<MemoryKind>,
    pub tag: Option<String>,
    pub text: Option<String>,
}

impl MemoryQuery {
    pub fn matches(&self, record: &MemoryRecord) -> bool {
        if self
            .namespace
            .as_ref()
            .is_some_and(|namespace| !record.is_visible_to(namespace))
        {
            return false;
        }

        if self.scope.as_ref().is_some_and(|scope| scope != &record.scope) {
            return false;
        }

        if self.owner.as_ref().is_some_and(|owner| owner != &record.owner) {
            return false;
        }

        if self.kind.as_ref().is_some_and(|kind| kind != &record.kind) {
            return false;
        }

        if self
            .tag
            .as_ref()
            .is_some_and(|tag| !record.tags.iter().any(|candidate| candidate == tag))
        {
            return false;
        }

        if let Some(text) = &self.text {
            let text = text.to_ascii_lowercase();
            let haystack = format!(
                "{} {} {}",
                record.title,
                record.summary,
                record.tags.join(" ")
            )
            .to_ascii_lowercase();

            if !haystack.contains(&text) {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryCatalog {
    records: Vec<MemoryRecord>,
}

impl MemoryCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, record: MemoryRecord) {
        self.records.push(record);
    }

    pub fn records(&self) -> &[MemoryRecord] {
        &self.records
    }

    pub fn find(&self, query: &MemoryQuery) -> Vec<&MemoryRecord> {
        self.records
            .iter()
            .filter(|record| query.matches(record))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    visibility: MemoryVisibility,
    scope: MemoryScope,
    owner_kind: MemoryOwnerKind,
    owner_ref: String,
    memory_kind: MemoryKind,
    tags: Vec<String>,
    derived_from: Vec<String>,
    confidence_milli: u16,
}

#[derive(Debug, Clone)]
pub struct MemoryRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl MemoryRepository {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_namespace(root, WorkspaceNamespace::local_default())
    }

    pub fn with_namespace(root: impl Into<PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            store: WorkspaceStore::new(root),
            namespace,
        }
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }

    pub fn namespace(&self) -> &WorkspaceNamespace {
        &self.namespace
    }

    pub fn relative_path_for(record: &MemoryRecord) -> PathBuf {
        PathBuf::from("memory")
            .join(scope_dir_name(&record.scope))
            .join(format!("{}.md", record.id))
    }

    pub fn write_record(&self, record: &MemoryRecord) -> Result<PathBuf> {
        let relative_path = Self::relative_path_for(record);
        let frontmatter = MemoryFrontmatter::from_record(record, &self.namespace);
        let body = render_memory_body(record);
        self.store
            .write_markdown_in_namespace(&self.namespace, relative_path, &frontmatter, &body)
    }

    pub fn read_record(&self, relative_path: impl AsRef<Path>) -> Result<MemoryRecord> {
        let stored: StoredMarkdown<MemoryFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(MemoryRecord::from_document(stored.frontmatter, stored.body))
    }
}

impl MemoryRecord {
    fn from_document(frontmatter: MemoryFrontmatter, body: String) -> Self {
        Self {
            id: frontmatter.id,
            namespace: MemoryNamespace::new(frontmatter.tenant_id, frontmatter.user_id),
            visibility: frontmatter.visibility,
            scope: frontmatter.scope,
            owner: MemoryOwnerRef::new(frontmatter.owner_kind, frontmatter.owner_ref),
            kind: frontmatter.memory_kind,
            title: frontmatter.title,
            summary: extract_summary_from_body(&body),
            content_ref: None,
            tags: frontmatter.tags,
            derived_from: frontmatter.derived_from,
            confidence_milli: frontmatter.confidence_milli,
        }
    }
}

impl From<&MemoryRecord> for MemoryFrontmatter {
    fn from(record: &MemoryRecord) -> Self {
        Self::from_record(record, &WorkspaceNamespace::local_default())
    }
}

impl MemoryFrontmatter {
    fn from_record(record: &MemoryRecord, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: record.id.clone(),
            r#type: "memory".to_owned(),
            title: record.title.clone(),
            tenant_id: if record.namespace == MemoryNamespace::local_default() {
                namespace.tenant_id.clone()
            } else {
                record.namespace.tenant_id.clone()
            },
            user_id: if record.namespace == MemoryNamespace::local_default() {
                namespace.user_id.clone()
            } else {
                record.namespace.user_id.clone()
            },
            visibility: record.visibility.clone(),
            scope: record.scope.clone(),
            owner_kind: record.owner.kind.clone(),
            owner_ref: record.owner.id.clone(),
            memory_kind: record.kind.clone(),
            tags: record.tags.clone(),
            derived_from: record.derived_from.clone(),
            confidence_milli: record.confidence_milli,
        }
    }
}

fn scope_dir_name(scope: &MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Global => "global",
        MemoryScope::Persona => "persona",
        MemoryScope::Session => "session",
        MemoryScope::Instance => "instance",
        MemoryScope::Project => "project",
        MemoryScope::Task => "task",
    }
}

fn render_memory_body(record: &MemoryRecord) -> String {
    let mut body = format!("# {}\n\n{}\n", record.title, record.summary);

    if let Some(content_ref) = &record.content_ref {
        body.push_str(&format!("\nContent Ref: `{}`\n", content_ref));
    }

    body
}

fn extract_summary_from_body(body: &str) -> String {
    body.lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| !line.trim_start().starts_with("Content Ref:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}
