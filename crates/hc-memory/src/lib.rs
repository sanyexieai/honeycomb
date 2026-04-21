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
pub enum MemoryLayer {
    Chat,
    Topic,
    Task,
    Project,
    Global,
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
#[serde(rename_all = "snake_case")]
pub enum MemoryEntityKind {
    User,
    Agent,
    Persona,
    Session,
    Instance,
    Task,
    Topic,
    Project,
    Crate,
    Document,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRelationKind {
    BelongsTo,
    About,
    References,
    DerivedFrom,
    Summarizes,
    Aggregates,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRoomAssetKind {
    Raw,
    Compressed,
    Literary,
    Facts,
    Timeline,
    Entities,
    Relations,
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
pub struct MemoryEntityRef {
    pub kind: MemoryEntityKind,
    pub id: String,
}

impl MemoryEntityRef {
    pub fn new(kind: MemoryEntityKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRelation {
    pub kind: MemoryRelationKind,
    pub target: String,
    pub detail: Option<String>,
}

impl MemoryRelation {
    pub fn new(kind: MemoryRelationKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: target.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRoom {
    pub id: String,
    #[serde(default)]
    pub namespace: MemoryNamespace,
    #[serde(default = "default_memory_visibility")]
    pub visibility: MemoryVisibility,
    pub layer: MemoryLayer,
    pub title: String,
    pub status: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub related_entities: Vec<MemoryEntityRef>,
    pub relations: Vec<MemoryRelation>,
    pub source_docs: Vec<String>,
    pub derived_docs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRoomAsset {
    pub id: String,
    pub room_id: String,
    pub file_name: String,
    #[serde(default)]
    pub namespace: MemoryNamespace,
    #[serde(default = "default_memory_visibility")]
    pub visibility: MemoryVisibility,
    pub layer: MemoryLayer,
    pub kind: MemoryRoomAssetKind,
    pub memory_kind: MemoryKind,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub owners: Vec<MemoryOwnerRef>,
    pub derived_from: Vec<String>,
    pub source_docs: Vec<String>,
}

impl MemoryRoom {
    pub fn new(
        id: impl Into<String>,
        layer: MemoryLayer,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            namespace: MemoryNamespace::local_default(),
            visibility: default_memory_visibility(),
            layer,
            title: title.into(),
            status: "active".to_owned(),
            summary: summary.into(),
            tags: Vec::new(),
            related_entities: Vec::new(),
            relations: Vec::new(),
            source_docs: Vec::new(),
            derived_docs: Vec::new(),
        }
    }

    pub fn with_namespace(mut self, namespace: MemoryNamespace) -> Self {
        self.namespace = namespace;
        self
    }

    pub fn with_visibility(mut self, visibility: MemoryVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_related_entity(mut self, entity: MemoryEntityRef) -> Self {
        self.related_entities.push(entity);
        self
    }

    pub fn with_relation(mut self, relation: MemoryRelation) -> Self {
        self.relations.push(relation);
        self
    }

    pub fn with_source_doc(mut self, source_doc: impl Into<String>) -> Self {
        self.source_docs.push(source_doc.into());
        self
    }

    pub fn with_derived_doc(mut self, derived_doc: impl Into<String>) -> Self {
        self.derived_docs.push(derived_doc.into());
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

impl MemoryRoomAsset {
    pub fn new(
        id: impl Into<String>,
        room_id: impl Into<String>,
        file_name: impl Into<String>,
        layer: MemoryLayer,
        kind: MemoryRoomAssetKind,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            room_id: room_id.into(),
            file_name: file_name.into(),
            namespace: MemoryNamespace::local_default(),
            visibility: default_memory_visibility(),
            layer,
            memory_kind: default_memory_kind_for_room_asset_kind(&kind),
            kind,
            title: title.into(),
            summary: summary.into(),
            tags: Vec::new(),
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        }
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

    pub fn with_memory_kind(mut self, memory_kind: MemoryKind) -> Self {
        self.memory_kind = memory_kind;
        self
    }

    pub fn with_owner(mut self, owner: MemoryOwnerRef) -> Self {
        self.owners.push(owner);
        self
    }

    pub fn with_derived_from(mut self, source: impl Into<String>) -> Self {
        self.derived_from.push(source.into());
        self
    }

    pub fn with_source_doc(mut self, source_doc: impl Into<String>) -> Self {
        self.source_docs.push(source_doc.into());
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

        if self
            .scope
            .as_ref()
            .is_some_and(|scope| scope != &record.scope)
        {
            return false;
        }

        if self
            .owner
            .as_ref()
            .is_some_and(|owner| owner != &record.owner)
        {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryRoomFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    visibility: MemoryVisibility,
    layer: MemoryLayer,
    status: String,
    tags: Vec<String>,
    related_entities: Vec<MemoryEntityRef>,
    relations: Vec<MemoryRelation>,
    source_docs: Vec<String>,
    derived_docs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryRoomAssetFrontmatter {
    id: String,
    r#type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    visibility: MemoryVisibility,
    room_id: String,
    layer: MemoryLayer,
    asset_kind: MemoryRoomAssetKind,
    memory_kind: MemoryKind,
    file_name: String,
    tags: Vec<String>,
    owners: Vec<MemoryOwnerRef>,
    derived_from: Vec<String>,
    source_docs: Vec<String>,
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

#[derive(Debug, Clone)]
pub struct MemoryRoomRepository {
    store: WorkspaceStore,
    namespace: WorkspaceNamespace,
}

impl MemoryRoomRepository {
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

    pub fn room_root_relative_path(room: &MemoryRoom) -> PathBuf {
        PathBuf::from("memory")
            .join("rooms")
            .join(layer_dir_name(&room.layer))
            .join(&room.id)
    }

    pub fn relative_path_for(room: &MemoryRoom) -> PathBuf {
        Self::room_root_relative_path(room).join("room.md")
    }

    pub fn raw_doc_relative_path(room: &MemoryRoom, file_name: impl AsRef<Path>) -> PathBuf {
        Self::room_root_relative_path(room)
            .join("raw")
            .join(file_name.as_ref())
    }

    pub fn compressed_doc_relative_path(room: &MemoryRoom, file_name: impl AsRef<Path>) -> PathBuf {
        Self::room_root_relative_path(room)
            .join("compressed")
            .join(file_name.as_ref())
    }

    pub fn literary_doc_relative_path(room: &MemoryRoom, file_name: impl AsRef<Path>) -> PathBuf {
        Self::room_root_relative_path(room)
            .join("literary")
            .join(file_name.as_ref())
    }

    pub fn facts_relative_path(room: &MemoryRoom) -> PathBuf {
        Self::room_root_relative_path(room).join("facts.md")
    }

    pub fn timeline_relative_path(room: &MemoryRoom) -> PathBuf {
        Self::room_root_relative_path(room).join("timeline.md")
    }

    pub fn entities_relative_path(room: &MemoryRoom) -> PathBuf {
        Self::room_root_relative_path(room).join("entities.md")
    }

    pub fn relations_relative_path(room: &MemoryRoom) -> PathBuf {
        Self::room_root_relative_path(room).join("relations.md")
    }

    pub fn write_room(&self, room: &MemoryRoom) -> Result<PathBuf> {
        let relative_path = Self::relative_path_for(room);
        let frontmatter = MemoryRoomFrontmatter::from_room(room, &self.namespace);
        let body = render_room_body(room);
        self.store
            .write_markdown_in_namespace(&self.namespace, relative_path, &frontmatter, &body)
    }

    pub fn asset_relative_path(room: &MemoryRoom, asset: &MemoryRoomAsset) -> PathBuf {
        match asset.kind {
            MemoryRoomAssetKind::Raw => Self::raw_doc_relative_path(room, &asset.file_name),
            MemoryRoomAssetKind::Compressed => {
                Self::compressed_doc_relative_path(room, &asset.file_name)
            }
            MemoryRoomAssetKind::Literary => {
                Self::literary_doc_relative_path(room, &asset.file_name)
            }
            MemoryRoomAssetKind::Facts => {
                Self::room_root_relative_path(room).join(&asset.file_name)
            }
            MemoryRoomAssetKind::Timeline => {
                Self::room_root_relative_path(room).join(&asset.file_name)
            }
            MemoryRoomAssetKind::Entities => {
                Self::room_root_relative_path(room).join(&asset.file_name)
            }
            MemoryRoomAssetKind::Relations => {
                Self::room_root_relative_path(room).join(&asset.file_name)
            }
        }
    }

    pub fn write_asset(&self, room: &MemoryRoom, asset: &MemoryRoomAsset) -> Result<PathBuf> {
        let relative_path = Self::asset_relative_path(room, asset);
        let frontmatter = MemoryRoomAssetFrontmatter::from_asset(asset, &self.namespace);
        let body = render_room_asset_body(asset);
        self.store
            .write_markdown_in_namespace(&self.namespace, relative_path, &frontmatter, &body)
    }

    pub fn read_room(&self, relative_path: impl AsRef<Path>) -> Result<MemoryRoom> {
        let stored: StoredMarkdown<MemoryRoomFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(MemoryRoom::from_document(stored.frontmatter, stored.body))
    }

    pub fn read_asset(&self, relative_path: impl AsRef<Path>) -> Result<MemoryRoomAsset> {
        let stored: StoredMarkdown<MemoryRoomAssetFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(MemoryRoomAsset::from_document(
            stored.frontmatter,
            stored.body,
        ))
    }

    pub fn read_compressed_assets(&self, room: &MemoryRoom) -> Result<Vec<MemoryRoomAsset>> {
        let compressed_dir = self.store.resolve_in_namespace(
            &self.namespace,
            Self::room_root_relative_path(room).join("compressed"),
        );
        if !compressed_dir.exists() {
            return Ok(Vec::new());
        }

        let mut assets = Vec::new();
        for entry in std::fs::read_dir(&compressed_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let relative = path
                .strip_prefix(self.store.resolve_in_namespace(&self.namespace, ""))
                .expect("asset path should live under namespace root")
                .to_path_buf();
            assets.push(self.read_asset(relative)?);
        }

        assets.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(assets)
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

impl MemoryRoom {
    fn from_document(frontmatter: MemoryRoomFrontmatter, body: String) -> Self {
        Self {
            id: frontmatter.id,
            namespace: MemoryNamespace::new(frontmatter.tenant_id, frontmatter.user_id),
            visibility: frontmatter.visibility,
            layer: frontmatter.layer,
            title: frontmatter.title,
            status: frontmatter.status,
            summary: extract_room_summary_from_body(&body),
            tags: frontmatter.tags,
            related_entities: frontmatter.related_entities,
            relations: frontmatter.relations,
            source_docs: frontmatter.source_docs,
            derived_docs: frontmatter.derived_docs,
        }
    }
}

impl MemoryRoomAsset {
    fn from_document(frontmatter: MemoryRoomAssetFrontmatter, body: String) -> Self {
        Self {
            id: frontmatter.id,
            room_id: frontmatter.room_id,
            file_name: frontmatter.file_name,
            namespace: MemoryNamespace::new(frontmatter.tenant_id, frontmatter.user_id),
            visibility: frontmatter.visibility,
            layer: frontmatter.layer,
            kind: frontmatter.asset_kind,
            memory_kind: frontmatter.memory_kind,
            title: frontmatter.title,
            summary: extract_room_asset_summary_from_body(&body),
            tags: frontmatter.tags,
            owners: frontmatter.owners,
            derived_from: frontmatter.derived_from,
            source_docs: frontmatter.source_docs,
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

impl MemoryRoomFrontmatter {
    fn from_room(room: &MemoryRoom, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: room.id.clone(),
            r#type: "memory_room".to_owned(),
            title: room.title.clone(),
            tenant_id: if room.namespace == MemoryNamespace::local_default() {
                namespace.tenant_id.clone()
            } else {
                room.namespace.tenant_id.clone()
            },
            user_id: if room.namespace == MemoryNamespace::local_default() {
                namespace.user_id.clone()
            } else {
                room.namespace.user_id.clone()
            },
            visibility: room.visibility.clone(),
            layer: room.layer.clone(),
            status: room.status.clone(),
            tags: room.tags.clone(),
            related_entities: room.related_entities.clone(),
            relations: room.relations.clone(),
            source_docs: room.source_docs.clone(),
            derived_docs: room.derived_docs.clone(),
        }
    }
}

impl MemoryRoomAssetFrontmatter {
    fn from_asset(asset: &MemoryRoomAsset, namespace: &WorkspaceNamespace) -> Self {
        Self {
            id: asset.id.clone(),
            r#type: "memory_room_asset".to_owned(),
            title: asset.title.clone(),
            tenant_id: if asset.namespace == MemoryNamespace::local_default() {
                namespace.tenant_id.clone()
            } else {
                asset.namespace.tenant_id.clone()
            },
            user_id: if asset.namespace == MemoryNamespace::local_default() {
                namespace.user_id.clone()
            } else {
                asset.namespace.user_id.clone()
            },
            visibility: asset.visibility.clone(),
            room_id: asset.room_id.clone(),
            layer: asset.layer.clone(),
            asset_kind: asset.kind.clone(),
            memory_kind: asset.memory_kind.clone(),
            file_name: asset.file_name.clone(),
            tags: asset.tags.clone(),
            owners: asset.owners.clone(),
            derived_from: asset.derived_from.clone(),
            source_docs: asset.source_docs.clone(),
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

fn layer_dir_name(layer: &MemoryLayer) -> &'static str {
    match layer {
        MemoryLayer::Chat => "chat",
        MemoryLayer::Topic => "topic",
        MemoryLayer::Task => "task",
        MemoryLayer::Project => "project",
        MemoryLayer::Global => "global",
    }
}

fn render_memory_body(record: &MemoryRecord) -> String {
    let mut body = format!("# {}\n\n{}\n", record.title, record.summary);

    if let Some(content_ref) = &record.content_ref {
        body.push_str(&format!("\nContent Ref: `{}`\n", content_ref));
    }

    body
}

fn render_room_body(room: &MemoryRoom) -> String {
    let mut body = format!("# {}\n\n{}\n", room.title, room.summary);

    body.push_str("\n## Related Entities\n\n");
    if room.related_entities.is_empty() {
        body.push_str("- none\n");
    } else {
        for entity in &room.related_entities {
            body.push_str(&format!("- {:?}: {}\n", entity.kind, entity.id));
        }
    }

    body.push_str("\n## Relations\n\n");
    if room.relations.is_empty() {
        body.push_str("- none\n");
    } else {
        for relation in &room.relations {
            let detail = relation
                .detail
                .as_ref()
                .map(|value| format!(" | {value}"))
                .unwrap_or_default();
            body.push_str(&format!(
                "- {:?}: {}{}\n",
                relation.kind, relation.target, detail
            ));
        }
    }

    body.push_str("\n## Source Docs\n\n");
    if room.source_docs.is_empty() {
        body.push_str("- none\n");
    } else {
        for path in &room.source_docs {
            body.push_str(&format!("- {}\n", path));
        }
    }

    body.push_str("\n## Derived Docs\n\n");
    if room.derived_docs.is_empty() {
        body.push_str("- none\n");
    } else {
        for path in &room.derived_docs {
            body.push_str(&format!("- {}\n", path));
        }
    }

    body
}

fn render_room_asset_body(asset: &MemoryRoomAsset) -> String {
    let mut body = format!("# {}\n\n{}\n", asset.title, asset.summary);

    body.push_str("\n## Owners\n\n");
    if asset.owners.is_empty() {
        body.push_str("- none\n");
    } else {
        for owner in &asset.owners {
            body.push_str(&format!("- {:?}: {}\n", owner.kind, owner.id));
        }
    }

    body.push_str("\n## Derived From\n\n");
    if asset.derived_from.is_empty() {
        body.push_str("- none\n");
    } else {
        for value in &asset.derived_from {
            body.push_str(&format!("- {}\n", value));
        }
    }

    body.push_str("\n## Source Docs\n\n");
    if asset.source_docs.is_empty() {
        body.push_str("- none\n");
    } else {
        for value in &asset.source_docs {
            body.push_str(&format!("- {}\n", value));
        }
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

fn extract_room_summary_from_body(body: &str) -> String {
    body.lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| !line.trim_start().starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn extract_room_asset_summary_from_body(body: &str) -> String {
    body.lines()
        .skip_while(|line| line.starts_with('#') || line.trim().is_empty())
        .take_while(|line| !line.trim_start().starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn default_memory_kind_for_room_asset_kind(kind: &MemoryRoomAssetKind) -> MemoryKind {
    match kind {
        MemoryRoomAssetKind::Compressed => MemoryKind::Summary,
        MemoryRoomAssetKind::Facts | MemoryRoomAssetKind::Entities => MemoryKind::Knowledge,
        MemoryRoomAssetKind::Timeline => MemoryKind::WorkflowMemory,
        MemoryRoomAssetKind::Relations => MemoryKind::Knowledge,
        MemoryRoomAssetKind::Raw => MemoryKind::Knowledge,
        MemoryRoomAssetKind::Literary => MemoryKind::Summary,
    }
}
