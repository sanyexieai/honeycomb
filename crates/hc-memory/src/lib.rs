//! Memory records, queries, and lightweight writeback helpers.

use anyhow::Result;
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};
use hc_trace::TraceEvent;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
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
#[serde(rename_all = "snake_case")]
pub enum MemoryAssetStage {
    Captured,
    Extracted,
    Generalized,
    Procedural,
    Compiled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAssetForm {
    RawNote,
    Summary,
    Entity,
    Topic,
    Relation,
    Fact,
    Workflow,
    Policy,
    Prompt,
    Rewrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactConsumer {
    Recall,
    PromptComposer,
    ToolPlanner,
    ToolExecutor,
    Evaluator,
    Human,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactSignalKind {
    UserMessage,
    AssistantReply,
    ToolOutcome,
    ScriptOutput,
    PromptTemplate,
    Document,
    Evaluation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactSignal {
    pub kind: ArtifactSignalKind,
    pub content: String,
    pub room_hint: Option<String>,
    pub layer_hint: Option<MemoryLayer>,
    pub tags: Vec<String>,
    pub source_docs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactDraft {
    pub title: String,
    pub summary: String,
    pub room_id: String,
    pub layer: MemoryLayer,
    pub asset_kind: MemoryRoomAssetKind,
    pub memory_kind: MemoryKind,
    pub stage: MemoryAssetStage,
    pub form: MemoryAssetForm,
    pub visibility: MemoryVisibility,
    pub tags: Vec<String>,
    pub owners: Vec<MemoryOwnerRef>,
    pub consumers: Vec<ArtifactConsumer>,
    pub derived_from: Vec<String>,
    pub source_docs: Vec<String>,
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactEvolutionAction {
    Created,
    Derived,
    Evaluated,
    Promoted,
    Revised,
    Retired,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactEvolutionEvent {
    pub id: String,
    pub artifact_id: String,
    pub room_id: String,
    pub action: ArtifactEvolutionAction,
    pub reason: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub tags: Vec<String>,
    pub created_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MaterializationKind {
    Asset,
    Draft,
    EvolutionEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterializationRecord {
    pub room_id: String,
    pub kind: MaterializationKind,
    pub path: PathBuf,
    pub room_relative_path: String,
}

impl ArtifactEvolutionEvent {
    pub fn new(
        id: impl Into<String>,
        artifact_id: impl Into<String>,
        room_id: impl Into<String>,
        action: ArtifactEvolutionAction,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            artifact_id: artifact_id.into(),
            room_id: room_id.into(),
            action,
            reason: reason.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            tags: Vec::new(),
            created_at_ms: 0,
        }
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.inputs.push(input.into());
        self
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.outputs.push(output.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_created_at_ms(mut self, created_at_ms: u128) -> Self {
        self.created_at_ms = created_at_ms;
        self
    }
}

impl MaterializationRecord {
    pub fn new(
        room_id: impl Into<String>,
        kind: MaterializationKind,
        path: PathBuf,
        room_relative_path: impl Into<String>,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            kind,
            path,
            room_relative_path: room_relative_path.into(),
        }
    }
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

// 能力继承相关类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InheritanceType {
    /// 直接继承
    Direct,
    /// 从父级Room继承
    FromParent,
    /// 从同级Room继承
    FromSibling,
    /// 自动发现继承
    AutoDiscovered,
    /// 用户手动添加
    Manual,
}

impl Default for InheritanceType {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapabilityRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_room_id: Option<String>,
    #[serde(default)]
    pub inheritance_type: InheritanceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_config: Option<serde_json::Value>,
}

impl CapabilityRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source_room_id: None,
            inheritance_type: InheritanceType::default(),
            override_config: None,
        }
    }

    pub fn with_source_room(mut self, room_id: impl Into<String>) -> Self {
        self.source_room_id = Some(room_id.into());
        self
    }

    pub fn with_inheritance_type(mut self, inheritance_type: InheritanceType) -> Self {
        self.inheritance_type = inheritance_type;
        self
    }

    pub fn with_override_config(mut self, config: serde_json::Value) -> Self {
        self.override_config = Some(config);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_room_id: Option<String>,
    #[serde(default)]
    pub inheritance_type: InheritanceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_override: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_override: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ToolRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source_room_id: None,
            inheritance_type: InheritanceType::default(),
            command_override: None,
            args_override: None,
        }
    }

    pub fn with_source_room(mut self, room_id: impl Into<String>) -> Self {
        self.source_room_id = Some(room_id.into());
        self
    }

    pub fn with_inheritance_type(mut self, inheritance_type: InheritanceType) -> Self {
        self.inheritance_type = inheritance_type;
        self
    }

    pub fn with_command_override(mut self, command: Vec<String>) -> Self {
        self.command_override = Some(command);
        self
    }

    pub fn with_args_override(mut self, args: serde_json::Map<String, serde_json::Value>) -> Self {
        self.args_override = Some(args);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_room_id: Option<String>,
    #[serde(default)]
    pub inheritance_type: InheritanceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions_override: Option<String>,
}

impl SkillRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source_room_id: None,
            inheritance_type: InheritanceType::default(),
            instructions_override: None,
        }
    }

    pub fn with_source_room(mut self, room_id: impl Into<String>) -> Self {
        self.source_room_id = Some(room_id.into());
        self
    }

    pub fn with_inheritance_type(mut self, inheritance_type: InheritanceType) -> Self {
        self.inheritance_type = inheritance_type;
        self
    }

    pub fn with_instructions_override(mut self, instructions: impl Into<String>) -> Self {
        self.instructions_override = Some(instructions.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleRef {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_room_id: Option<String>,
    #[serde(default)]
    pub inheritance_type: InheritanceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_override: Option<serde_json::Value>, // 将来可以改为具体的 ScheduleSpec 类型
    #[serde(default = "default_schedule_enabled")]
    pub enabled_in_room: bool,
}

impl ScheduleRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source_room_id: None,
            inheritance_type: InheritanceType::default(),
            schedule_override: None,
            enabled_in_room: default_schedule_enabled(),
        }
    }

    pub fn with_source_room(mut self, room_id: impl Into<String>) -> Self {
        self.source_room_id = Some(room_id.into());
        self
    }

    pub fn with_inheritance_type(mut self, inheritance_type: InheritanceType) -> Self {
        self.inheritance_type = inheritance_type;
        self
    }

    pub fn with_schedule_override(mut self, schedule: serde_json::Value) -> Self {
        self.schedule_override = Some(schedule);
        self
    }

    pub fn enabled(mut self) -> Self {
        self.enabled_in_room = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled_in_room = false;
        self
    }
}

fn default_schedule_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RoomConfig {
    /// 是否自动继承父级 Room 的能力
    #[serde(default)]
    pub auto_inherit_parent: bool,
    /// 是否自动继承同层级相关 Room 的能力
    #[serde(default)]
    pub auto_inherit_siblings: bool,
    /// 能力过滤标签
    #[serde(default)]
    pub capability_filter_tags: Vec<String>,
    /// 工具过滤标签
    #[serde(default)]
    pub tool_filter_tags: Vec<String>,
    /// 技能过滤标签  
    #[serde(default)]
    pub skill_filter_tags: Vec<String>,
    /// 执行上下文配置
    #[serde(default)]
    pub execution_context: ExecutionContext,
}

impl RoomConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_auto_inherit_parent(mut self) -> Self {
        self.auto_inherit_parent = true;
        self
    }

    pub fn with_auto_inherit_siblings(mut self) -> Self {
        self.auto_inherit_siblings = true;
        self
    }

    pub fn with_capability_filter_tag(mut self, tag: impl Into<String>) -> Self {
        self.capability_filter_tags.push(tag.into());
        self
    }

    pub fn with_tool_filter_tag(mut self, tag: impl Into<String>) -> Self {
        self.tool_filter_tags.push(tag.into());
        self
    }

    pub fn with_skill_filter_tag(mut self, tag: impl Into<String>) -> Self {
        self.skill_filter_tags.push(tag.into());
        self
    }

    pub fn with_execution_context(mut self, context: ExecutionContext) -> Self {
        self.execution_context = context;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExecutionContext {
    /// 默认命名空间
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_namespace: Option<String>,
    /// 环境变量
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
    /// 工作目录
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.default_namespace = Some(namespace.into());
        self
    }

    pub fn with_environment_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    pub fn with_working_directory(mut self, dir: impl Into<String>) -> Self {
        self.working_directory = Some(dir.into());
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
    
    // 新增：能力继承
    #[serde(default)]
    pub inherited_capabilities: Vec<CapabilityRef>,
    #[serde(default)]
    pub inherited_tools: Vec<ToolRef>,
    #[serde(default)]
    pub inherited_skills: Vec<SkillRef>,
    #[serde(default)]
    pub inherited_schedules: Vec<ScheduleRef>,
    
    // 新增：Room 特定配置
    #[serde(default)]
    pub room_config: RoomConfig,
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
    pub stage: MemoryAssetStage,
    pub form: MemoryAssetForm,
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
            inherited_capabilities: Vec::new(),
            inherited_tools: Vec::new(),
            inherited_skills: Vec::new(),
            inherited_schedules: Vec::new(),
            room_config: RoomConfig::default(),
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

    // 能力继承管理方法
    pub fn with_inherited_capability(mut self, capability_ref: CapabilityRef) -> Self {
        self.inherited_capabilities.push(capability_ref);
        self
    }

    pub fn with_inherited_tool(mut self, tool_ref: ToolRef) -> Self {
        self.inherited_tools.push(tool_ref);
        self
    }

    pub fn with_inherited_skill(mut self, skill_ref: SkillRef) -> Self {
        self.inherited_skills.push(skill_ref);
        self
    }

    pub fn with_inherited_schedule(mut self, schedule_ref: ScheduleRef) -> Self {
        self.inherited_schedules.push(schedule_ref);
        self
    }

    pub fn with_room_config(mut self, config: RoomConfig) -> Self {
        self.room_config = config;
        self
    }

    /// 添加能力引用
    pub fn add_capability(&mut self, capability_ref: CapabilityRef) {
        if !self.inherited_capabilities.iter().any(|c| c.id == capability_ref.id) {
            self.inherited_capabilities.push(capability_ref);
        }
    }

    /// 添加工具引用
    pub fn add_tool(&mut self, tool_ref: ToolRef) {
        if !self.inherited_tools.iter().any(|t| t.id == tool_ref.id) {
            self.inherited_tools.push(tool_ref);
        }
    }

    /// 添加技能引用
    pub fn add_skill(&mut self, skill_ref: SkillRef) {
        if !self.inherited_skills.iter().any(|s| s.id == skill_ref.id) {
            self.inherited_skills.push(skill_ref);
        }
    }

    /// 添加调度引用
    pub fn add_schedule(&mut self, schedule_ref: ScheduleRef) {
        if !self.inherited_schedules.iter().any(|s| s.id == schedule_ref.id) {
            self.inherited_schedules.push(schedule_ref);
        }
    }

    /// 移除能力引用
    pub fn remove_capability(&mut self, capability_id: &str) {
        self.inherited_capabilities.retain(|c| c.id != capability_id);
    }

    /// 移除工具引用
    pub fn remove_tool(&mut self, tool_id: &str) {
        self.inherited_tools.retain(|t| t.id != tool_id);
    }

    /// 移除技能引用
    pub fn remove_skill(&mut self, skill_id: &str) {
        self.inherited_skills.retain(|s| s.id != skill_id);
    }

    /// 移除调度引用
    pub fn remove_schedule(&mut self, schedule_id: &str) {
        self.inherited_schedules.retain(|s| s.id != schedule_id);
    }

    /// 检查是否继承了指定的能力
    pub fn has_capability(&self, capability_id: &str) -> bool {
        self.inherited_capabilities.iter().any(|c| c.id == capability_id)
    }

    /// 检查是否继承了指定的工具
    pub fn has_tool(&self, tool_id: &str) -> bool {
        self.inherited_tools.iter().any(|t| t.id == tool_id)
    }

    /// 检查是否继承了指定的技能
    pub fn has_skill(&self, skill_id: &str) -> bool {
        self.inherited_skills.iter().any(|s| s.id == skill_id)
    }

    /// 检查是否继承了指定的调度
    pub fn has_schedule(&self, schedule_id: &str) -> bool {
        self.inherited_schedules.iter().any(|s| s.id == schedule_id)
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
            stage: default_memory_asset_stage_for_room_asset_kind(&kind),
            form: default_memory_asset_form_for_room_asset_kind(&kind),
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
        self.form = default_memory_asset_form_for_memory_kind(&self.kind, &self.memory_kind);
        self
    }

    pub fn with_stage(mut self, stage: MemoryAssetStage) -> Self {
        self.stage = stage;
        self
    }

    pub fn with_form(mut self, form: MemoryAssetForm) -> Self {
        self.form = form;
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

    pub fn from_draft(
        id: impl Into<String>,
        namespace: MemoryNamespace,
        draft: ArtifactDraft,
    ) -> Self {
        let mut asset = Self::new(
            id.into(),
            draft.room_id,
            draft.file_name.unwrap_or_else(|| "artifact.md".to_owned()),
            draft.layer,
            draft.asset_kind,
            draft.title,
            draft.summary,
        )
        .with_namespace(namespace)
        .with_visibility(draft.visibility)
        .with_memory_kind(draft.memory_kind)
        .with_stage(draft.stage)
        .with_form(draft.form);

        for tag in draft.tags {
            asset = asset.with_tag(tag);
        }
        for owner in draft.owners {
            asset = asset.with_owner(owner);
        }
        for source in draft.derived_from {
            asset = asset.with_derived_from(source);
        }
        for source_doc in draft.source_docs {
            asset = asset.with_source_doc(source_doc);
        }

        asset
    }

    pub fn is_visible_to(&self, namespace: &MemoryNamespace) -> bool {
        match self.visibility {
            MemoryVisibility::Private => self.namespace == *namespace,
            MemoryVisibility::TenantShared => self.namespace.tenant_id == namespace.tenant_id,
            MemoryVisibility::CrossTenantShared => true,
        }
    }
}

impl ArtifactSignal {
    pub fn new(kind: ArtifactSignalKind, content: impl Into<String>) -> Self {
        Self {
            kind,
            content: content.into(),
            room_hint: None,
            layer_hint: None,
            tags: Vec::new(),
            source_docs: Vec::new(),
        }
    }

    pub fn with_room_hint(mut self, room_hint: impl Into<String>) -> Self {
        self.room_hint = Some(room_hint.into());
        self
    }

    pub fn with_layer_hint(mut self, layer_hint: MemoryLayer) -> Self {
        self.layer_hint = Some(layer_hint);
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_source_doc(mut self, source_doc: impl Into<String>) -> Self {
        self.source_docs.push(source_doc.into());
        self
    }
}

impl ArtifactDraft {
    pub fn new(
        room_id: impl Into<String>,
        layer: MemoryLayer,
        asset_kind: MemoryRoomAssetKind,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        let asset_kind = asset_kind;
        Self {
            title: title.into(),
            summary: summary.into(),
            room_id: room_id.into(),
            layer,
            memory_kind: default_memory_kind_for_room_asset_kind(&asset_kind),
            stage: default_memory_asset_stage_for_room_asset_kind(&asset_kind),
            form: default_memory_asset_form_for_room_asset_kind(&asset_kind),
            asset_kind,
            visibility: default_memory_visibility(),
            tags: Vec::new(),
            owners: Vec::new(),
            consumers: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
            file_name: None,
        }
    }

    pub fn with_memory_kind(mut self, memory_kind: MemoryKind) -> Self {
        self.memory_kind = memory_kind;
        self.form = default_memory_asset_form_for_memory_kind(&self.asset_kind, &self.memory_kind);
        self
    }

    pub fn with_stage(mut self, stage: MemoryAssetStage) -> Self {
        self.stage = stage;
        self
    }

    pub fn with_form(mut self, form: MemoryAssetForm) -> Self {
        self.form = form;
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

    pub fn with_owner(mut self, owner: MemoryOwnerRef) -> Self {
        self.owners.push(owner);
        self
    }

    pub fn with_consumer(mut self, consumer: ArtifactConsumer) -> Self {
        self.consumers.push(consumer);
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

    pub fn with_file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
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
    // 新增：能力继承
    #[serde(default)]
    inherited_capabilities: Vec<CapabilityRef>,
    #[serde(default)]
    inherited_tools: Vec<ToolRef>,
    #[serde(default)]
    inherited_skills: Vec<SkillRef>,
    #[serde(default)]
    inherited_schedules: Vec<ScheduleRef>,
    #[serde(default)]
    room_config: RoomConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MemoryRoomAssetSidecar {
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
    stage: MemoryAssetStage,
    form: MemoryAssetForm,
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
        let path = self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &frontmatter,
            &body,
        )?;
        hc_trace::emit_trace(
            TraceEvent::info(
                "hc-memory",
                "memory_record",
                "write",
                "persisted memory record",
            )
            .with_status("saved")
            .with_field("record_id", record.id.clone())
            .with_field("scope", format!("{:?}", record.scope).to_ascii_lowercase())
            .with_field(
                "memory_kind",
                format!("{:?}", record.kind).to_ascii_lowercase(),
            )
            .with_field(
                "owner_kind",
                format!("{:?}", record.owner.kind).to_ascii_lowercase(),
            )
            .with_field("owner_id", record.owner.id.clone())
            .with_field("path", path.display().to_string()),
        );
        Ok(path)
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

    pub fn prompt_doc_relative_path(room: &MemoryRoom, file_name: impl AsRef<Path>) -> PathBuf {
        Self::room_root_relative_path(room)
            .join("prompt")
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
        let path = self.store.write_markdown_in_namespace(
            &self.namespace,
            relative_path,
            &frontmatter,
            &body,
        )?;
        hc_trace::emit_trace(
            TraceEvent::info("hc-memory", "memory_room", "write", "persisted memory room")
                .with_status("saved")
                .with_field("room_id", room.id.clone())
                .with_field("layer", format!("{:?}", room.layer).to_ascii_lowercase())
                .with_field("status_value", room.status.clone())
                .with_field("path", path.display().to_string()),
        );
        Ok(path)
    }

    pub fn asset_relative_path(room: &MemoryRoom, asset: &MemoryRoomAsset) -> PathBuf {
        if asset.stage == MemoryAssetStage::Compiled && asset.form == MemoryAssetForm::Prompt {
            return Self::prompt_doc_relative_path(room, &asset.file_name);
        }

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
        Ok(self.materialize_asset(room, asset)?.path)
    }

    pub fn materialize_asset(
        &self,
        room: &MemoryRoom,
        asset: &MemoryRoomAsset,
    ) -> Result<MaterializationRecord> {
        let relative_path = Self::asset_relative_path(room, asset);
        let body = render_room_asset_body(asset);
        let written_path =
            self.store
                .write_text_in_namespace(&self.namespace, &relative_path, &body)?;
        write_room_asset_sidecar(
            &self.store,
            &self.namespace,
            &relative_path,
            &MemoryRoomAssetSidecar::from_asset(asset, &self.namespace),
        )?;
        self.sync_room_objects_for_asset(room, &relative_path, asset)?;
        let record = MaterializationRecord::new(
            room.id.clone(),
            MaterializationKind::Asset,
            written_path,
            room_relative_path_for_asset(room, &relative_path),
        );
        hc_trace::emit_trace(
            TraceEvent::info(
                "hc-memory",
                "memory_asset",
                "materialize",
                "persisted room asset",
            )
            .with_status("saved")
            .with_field("room_id", room.id.clone())
            .with_field("asset_id", asset.id.clone())
            .with_field(
                "asset_kind",
                format!("{:?}", asset.kind).to_ascii_lowercase(),
            )
            .with_field(
                "memory_kind",
                format!("{:?}", asset.memory_kind).to_ascii_lowercase(),
            )
            .with_field(
                "stage_value",
                format!("{:?}", asset.stage).to_ascii_lowercase(),
            )
            .with_field("form", format!("{:?}", asset.form).to_ascii_lowercase())
            .with_field("path", record.path.display().to_string())
            .with_field("room_relative_path", record.room_relative_path.clone()),
        );
        Ok(record)
    }

    pub fn write_artifact_draft(
        &self,
        room: &MemoryRoom,
        asset_id: impl Into<String>,
        draft: ArtifactDraft,
    ) -> Result<PathBuf> {
        Ok(self.materialize_artifact_draft(room, asset_id, draft)?.path)
    }

    pub fn materialize_artifact_draft(
        &self,
        room: &MemoryRoom,
        asset_id: impl Into<String>,
        draft: ArtifactDraft,
    ) -> Result<MaterializationRecord> {
        let namespace = MemoryNamespace::new(
            self.namespace.tenant_id.clone(),
            self.namespace.user_id.clone(),
        );
        let asset = MemoryRoomAsset::from_draft(asset_id, namespace, draft);
        let mut materialized = self.materialize_asset(room, &asset)?;
        materialized.kind = MaterializationKind::Draft;
        Ok(materialized)
    }

    pub fn write_evolution_event(
        &self,
        room: &MemoryRoom,
        event: &ArtifactEvolutionEvent,
    ) -> Result<PathBuf> {
        Ok(self.materialize_evolution_event(room, event)?.path)
    }

    pub fn materialize_evolution_event(
        &self,
        room: &MemoryRoom,
        event: &ArtifactEvolutionEvent,
    ) -> Result<MaterializationRecord> {
        let relative_path = Self::timeline_relative_path(room);
        let resolved_path = self
            .store
            .resolve_in_namespace(&self.namespace, &relative_path);
        let existing = if resolved_path.exists() {
            fs::read_to_string(&resolved_path)?
        } else {
            String::new()
        };
        let entry = render_evolution_event_entry(event);
        let summary = if existing.trim().is_empty() {
            entry
        } else if existing.contains(&format!("event: {}", event.id)) {
            existing
        } else {
            format!("{}\n\n{}", existing.trim_end(), entry)
        };
        let mut asset = MemoryRoomAsset::new(
            format!("asset.{}.timeline", room.id),
            room.id.clone(),
            "timeline.md",
            room.layer.clone(),
            MemoryRoomAssetKind::Timeline,
            format!("{} Timeline", room.title),
            summary,
        )
        .with_namespace(MemoryNamespace::new(
            self.namespace.tenant_id.clone(),
            self.namespace.user_id.clone(),
        ))
        .with_visibility(room.visibility.clone())
        .with_memory_kind(MemoryKind::WorkflowMemory)
        .with_stage(MemoryAssetStage::Extracted)
        .with_form(MemoryAssetForm::Workflow)
        .with_tag("timeline")
        .with_tag("event")
        .with_derived_from(event.artifact_id.clone());
        for tag in &event.tags {
            asset = asset.with_tag(tag.clone());
        }
        let mut materialized = self.materialize_asset(room, &asset)?;
        materialized.kind = MaterializationKind::EvolutionEvent;
        hc_trace::emit_trace(
            TraceEvent::info(
                "hc-memory",
                "memory_timeline",
                "append_event",
                "appended room evolution event",
            )
            .with_status("saved")
            .with_field("room_id", room.id.clone())
            .with_field("event_id", event.id.clone())
            .with_field("artifact_id", event.artifact_id.clone())
            .with_field(
                "event_action",
                format!("{:?}", event.action).to_ascii_lowercase(),
            )
            .with_field("path", materialized.path.display().to_string()),
        );
        Ok(materialized)
    }

    pub fn read_room(&self, relative_path: impl AsRef<Path>) -> Result<MemoryRoom> {
        let stored: StoredMarkdown<MemoryRoomFrontmatter> = self
            .store
            .read_markdown_in_namespace(&self.namespace, relative_path)?;
        Ok(MemoryRoom::from_document(stored.frontmatter, stored.body))
    }

    pub fn get_room_by_id(&self, room_id: &str) -> Result<Option<MemoryRoom>> {
        // 尝试在不同层级中查找房间
        let layers = [
            MemoryLayer::Chat,
            MemoryLayer::Topic, 
            MemoryLayer::Task,
            MemoryLayer::Project,
            MemoryLayer::Global,
        ];
        
        for layer in layers {
            let room_path = PathBuf::from("memory")
                .join("rooms")
                .join(layer_dir_name(&layer))
                .join(room_id)
                .join("room.md");
                
            if let Ok(room) = self.read_room(&room_path) {
                if room.id == room_id {
                    return Ok(Some(room));
                }
            }
        }
        
        Ok(None)
    }

    pub fn list_rooms(&self) -> Result<Vec<MemoryRoom>> {
        let mut rooms = Vec::new();
        
        // 遍历所有层级
        let layers = [
            MemoryLayer::Chat,
            MemoryLayer::Topic, 
            MemoryLayer::Task,
            MemoryLayer::Project,
            MemoryLayer::Global,
        ];
        
        for layer in layers {
            let layer_path = PathBuf::from("memory")
                .join("rooms")
                .join(layer_dir_name(&layer));
                
            let full_layer_path = self.store.resolve_in_namespace(&self.namespace, &layer_path);
            
            // 检查层级目录是否存在
            if !full_layer_path.exists() {
                continue;
            }
            
            // 遍历层级目录中的所有房间
            if let Ok(entries) = std::fs::read_dir(&full_layer_path) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                        let room_dir = entry.path();
                        let room_file = room_dir.join("room.md");
                        
                        if room_file.exists() {
                            let relative_room_path = layer_path.join(entry.file_name()).join("room.md");
                            if let Ok(room) = self.read_room(&relative_room_path) {
                                rooms.push(room);
                            }
                        }
                    }
                }
            }
        }
        
        // 按层级和ID排序
        rooms.sort_by(|a, b| {
            a.layer.cmp(&b.layer).then(a.id.cmp(&b.id))
        });
        
        Ok(rooms)
    }

    pub fn list_rooms_by_layer(&self, layer: MemoryLayer) -> Result<Vec<MemoryRoom>> {
        let mut rooms = Vec::new();
        
        let layer_path = PathBuf::from("memory")
            .join("rooms")
            .join(layer_dir_name(&layer));
            
        let full_layer_path = self.store.resolve_in_namespace(&self.namespace, &layer_path);
        
        // 检查层级目录是否存在
        if !full_layer_path.exists() {
            return Ok(rooms);
        }
        
        // 遍历层级目录中的所有房间
        if let Ok(entries) = std::fs::read_dir(&full_layer_path) {
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    let room_dir = entry.path();
                    let room_file = room_dir.join("room.md");
                    
                    if room_file.exists() {
                        let relative_room_path = layer_path.join(entry.file_name()).join("room.md");
                        if let Ok(room) = self.read_room(&relative_room_path) {
                            rooms.push(room);
                        }
                    }
                }
            }
        }
        
        // 按ID排序
        rooms.sort_by(|a, b| a.id.cmp(&b.id));
        
        Ok(rooms)
    }

    pub fn read_asset(&self, relative_path: impl AsRef<Path>) -> Result<MemoryRoomAsset> {
        let relative_path = relative_path.as_ref();
        let path = self
            .store
            .resolve_in_namespace(&self.namespace, relative_path);
        let body = fs::read_to_string(&path)?;
        let sidecar = read_room_asset_sidecar(&self.store, &self.namespace, relative_path)?;
        Ok(MemoryRoomAsset::from_plain_document(
            relative_path,
            body,
            sidecar,
            &self.namespace,
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
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }

            let relative = path
                .strip_prefix(self.store.resolve_in_namespace(&self.namespace, ""))
                .expect("asset path should live under namespace root")
                .to_path_buf();
            match self.read_asset(relative) {
                Ok(asset) => assets.push(asset),
                Err(error) if is_not_found_error(&error) => continue,
                Err(error) => return Err(error),
            }
        }

        assets.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(assets)
    }

    fn sync_room_objects_for_asset(
        &self,
        room: &MemoryRoom,
        relative_path: &Path,
        asset: &MemoryRoomAsset,
    ) -> Result<()> {
        let room_relative_path = relative_path
            .strip_prefix(Self::room_root_relative_path(room))
            .unwrap_or(relative_path)
            .to_string_lossy()
            .replace('\\', "/");

        let room_doc_relative = Self::relative_path_for(room);
        let mut indexed_room = self
            .read_room(&room_doc_relative)
            .unwrap_or_else(|_| room.clone());

        merge_room_metadata(&mut indexed_room, room);

        let target_docs = if asset.kind == MemoryRoomAssetKind::Raw {
            &mut indexed_room.source_docs
        } else {
            &mut indexed_room.derived_docs
        };
        if !target_docs.iter().any(|path| path == &room_relative_path) {
            target_docs.push(room_relative_path);
        }

        self.write_room(&indexed_room)?;
        Ok(())
    }
}

fn room_relative_path_for_asset(room: &MemoryRoom, relative_path: &Path) -> String {
    relative_path
        .strip_prefix(MemoryRoomRepository::room_root_relative_path(room))
        .unwrap_or(relative_path)
        .to_string_lossy()
        .replace('\\', "/")
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
            inherited_capabilities: frontmatter.inherited_capabilities,
            inherited_tools: frontmatter.inherited_tools,
            inherited_skills: frontmatter.inherited_skills,
            inherited_schedules: frontmatter.inherited_schedules,
            room_config: frontmatter.room_config,
        }
    }
}

impl MemoryRoomAsset {
    fn from_plain_document(
        relative_path: &Path,
        body: String,
        sidecar: MemoryRoomAssetSidecar,
        _namespace: &WorkspaceNamespace,
    ) -> Self {
        let (_, _, _, _) = parse_room_asset_path(relative_path);
        Self {
            id: sidecar.id,
            room_id: sidecar.room_id,
            file_name: sidecar.file_name,
            namespace: MemoryNamespace::new(sidecar.tenant_id, sidecar.user_id),
            visibility: sidecar.visibility,
            layer: sidecar.layer,
            kind: sidecar.asset_kind,
            memory_kind: sidecar.memory_kind,
            stage: sidecar.stage,
            form: sidecar.form,
            title: sidecar.title,
            summary: extract_room_asset_summary_from_body(&body),
            tags: sidecar.tags,
            owners: sidecar.owners,
            derived_from: sidecar.derived_from,
            source_docs: sidecar.source_docs,
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
            inherited_capabilities: room.inherited_capabilities.clone(),
            inherited_tools: room.inherited_tools.clone(),
            inherited_skills: room.inherited_skills.clone(),
            inherited_schedules: room.inherited_schedules.clone(),
            room_config: room.room_config.clone(),
        }
    }
}

impl MemoryRoomAssetSidecar {
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
            stage: asset.stage.clone(),
            form: asset.form.clone(),
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

    body.push_str("\n## Manifest\n\n");
    body.push_str(&format!("- room: {}\n", room.id));
    body.push_str(&format!("- layer: {}\n", layer_dir_name(&room.layer)));
    body.push_str(&format!("- status: {}\n", room.status));

    if !room.related_entities.is_empty() {
        body.push_str("\n## Entities\n\n");
        for entity in &room.related_entities {
            body.push_str(&format!(
                "- {}: {}\n",
                memory_entity_kind_label(&entity.kind),
                entity.id
            ));
        }
    }

    if !room.relations.is_empty() {
        body.push_str("\n## Relations\n\n");
        for relation in &room.relations {
            let detail = relation
                .detail
                .as_ref()
                .map(|value| format!(" | {value}"))
                .unwrap_or_default();
            body.push_str(&format!(
                "- {} -> {}{}\n",
                memory_relation_kind_label(&relation.kind),
                relation.target,
                detail
            ));
        }
    }

    if !room.source_docs.is_empty() || !room.derived_docs.is_empty() {
        body.push_str("\n## Objects\n\n");
        for path in &room.source_docs {
            body.push_str(&format!("- source: {}\n", path));
        }
        for path in &room.derived_docs {
            body.push_str(&format!("- derived: {}\n", path));
        }
    }

    body
}

fn merge_room_metadata(target: &mut MemoryRoom, source: &MemoryRoom) {
    target.namespace = source.namespace.clone();
    target.visibility = source.visibility.clone();
    target.layer = source.layer.clone();
    target.title = source.title.clone();
    target.status = source.status.clone();
    target.summary = source.summary.clone();

    for tag in &source.tags {
        if !target.tags.iter().any(|existing| existing == tag) {
            target.tags.push(tag.clone());
        }
    }
    for entity in &source.related_entities {
        if !target
            .related_entities
            .iter()
            .any(|existing| existing == entity)
        {
            target.related_entities.push(entity.clone());
        }
    }
    for relation in &source.relations {
        if !target.relations.iter().any(|existing| existing == relation) {
            target.relations.push(relation.clone());
        }
    }
    for doc in &source.source_docs {
        if !target.source_docs.iter().any(|existing| existing == doc) {
            target.source_docs.push(doc.clone());
        }
    }
    for doc in &source.derived_docs {
        if !target.derived_docs.iter().any(|existing| existing == doc) {
            target.derived_docs.push(doc.clone());
        }
    }
}

fn render_room_asset_body(asset: &MemoryRoomAsset) -> String {
    let summary = asset.summary.trim();
    if summary.is_empty() {
        String::new()
    } else {
        format!("{summary}\n")
    }
}

fn render_evolution_event_entry(event: &ArtifactEvolutionEvent) -> String {
    let mut body = String::new();
    body.push_str(&format!("### {:?}\n\n", event.action));
    body.push_str(&format!("- event: {}\n", event.id));
    body.push_str(&format!("- artifact: {}\n", event.artifact_id));
    body.push_str(&format!("- action: {:?}\n", event.action));
    body.push_str(&format!("- reason: {}\n", event.reason));
    if event.created_at_ms > 0 {
        body.push_str(&format!("- created_at_ms: {}\n", event.created_at_ms));
    }
    if !event.tags.is_empty() {
        body.push_str(&format!("- tags: {}\n", event.tags.join(", ")));
    }
    if !event.inputs.is_empty() {
        body.push_str("- inputs:\n");
        for input in &event.inputs {
            body.push_str(&format!("  - {}\n", input));
        }
    }
    if !event.outputs.is_empty() {
        body.push_str("- outputs:\n");
        for output in &event.outputs {
            body.push_str(&format!("  - {}\n", output));
        }
    }
    body.trim_end().to_owned()
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
    body.trim().to_owned()
}

fn parse_room_asset_path(
    relative_path: &Path,
) -> (String, MemoryLayer, MemoryRoomAssetKind, String) {
    let segments = relative_path
        .iter()
        .filter_map(|segment| segment.to_str())
        .collect::<Vec<_>>();
    let layer = segments
        .get(2)
        .map(|value| parse_memory_layer(value))
        .unwrap_or(MemoryLayer::Chat);
    let room_id = segments.get(3).copied().unwrap_or_default().to_owned();
    let file_name = relative_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset.md")
        .to_owned();
    let kind = match segments.get(4).copied() {
        Some("raw") => MemoryRoomAssetKind::Raw,
        Some("compressed") => MemoryRoomAssetKind::Compressed,
        Some("literary") => MemoryRoomAssetKind::Literary,
        Some("prompt") => MemoryRoomAssetKind::Compressed,
        _ => match file_name.as_str() {
            "facts.md" => MemoryRoomAssetKind::Facts,
            "timeline.md" => MemoryRoomAssetKind::Timeline,
            "entities.md" => MemoryRoomAssetKind::Entities,
            "relations.md" => MemoryRoomAssetKind::Relations,
            _ => MemoryRoomAssetKind::Compressed,
        },
    };

    (room_id, layer, kind, file_name)
}

fn parse_memory_layer(value: &str) -> MemoryLayer {
    match value {
        "chat" => MemoryLayer::Chat,
        "topic" => MemoryLayer::Topic,
        "task" => MemoryLayer::Task,
        "project" => MemoryLayer::Project,
        "global" => MemoryLayer::Global,
        _ => MemoryLayer::Chat,
    }
}

fn memory_entity_kind_label(kind: &MemoryEntityKind) -> &'static str {
    match kind {
        MemoryEntityKind::User => "user",
        MemoryEntityKind::Agent => "agent",
        MemoryEntityKind::Persona => "persona",
        MemoryEntityKind::Session => "session",
        MemoryEntityKind::Instance => "instance",
        MemoryEntityKind::Task => "task",
        MemoryEntityKind::Topic => "topic",
        MemoryEntityKind::Project => "project",
        MemoryEntityKind::Crate => "crate",
        MemoryEntityKind::Document => "document",
        MemoryEntityKind::Other => "other",
    }
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == ErrorKind::NotFound)
    })
}

fn memory_relation_kind_label(kind: &MemoryRelationKind) -> &'static str {
    match kind {
        MemoryRelationKind::BelongsTo => "belongs_to",
        MemoryRelationKind::About => "about",
        MemoryRelationKind::References => "references",
        MemoryRelationKind::DerivedFrom => "derived_from",
        MemoryRelationKind::Summarizes => "summarizes",
        MemoryRelationKind::Aggregates => "aggregates",
    }
}

fn room_asset_sidecar_relative_path(relative_path: &Path) -> PathBuf {
    let file_name = relative_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("asset.md");
    let sidecar_name = format!("{}.meta.json", file_name.trim_end_matches(".md"));
    relative_path.with_file_name(sidecar_name)
}

fn write_room_asset_sidecar(
    store: &WorkspaceStore,
    namespace: &WorkspaceNamespace,
    relative_path: &Path,
    sidecar: &MemoryRoomAssetSidecar,
) -> Result<()> {
    let path =
        store.resolve_in_namespace(namespace, room_asset_sidecar_relative_path(relative_path));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(sidecar)?;
    fs::write(path, payload)?;
    Ok(())
}

fn read_room_asset_sidecar(
    store: &WorkspaceStore,
    namespace: &WorkspaceNamespace,
    relative_path: &Path,
) -> Result<MemoryRoomAssetSidecar> {
    let path =
        store.resolve_in_namespace(namespace, room_asset_sidecar_relative_path(relative_path));
    Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
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

fn default_memory_asset_stage_for_room_asset_kind(kind: &MemoryRoomAssetKind) -> MemoryAssetStage {
    match kind {
        MemoryRoomAssetKind::Raw => MemoryAssetStage::Captured,
        MemoryRoomAssetKind::Compressed => MemoryAssetStage::Generalized,
        MemoryRoomAssetKind::Literary => MemoryAssetStage::Compiled,
        MemoryRoomAssetKind::Facts
        | MemoryRoomAssetKind::Timeline
        | MemoryRoomAssetKind::Entities
        | MemoryRoomAssetKind::Relations => MemoryAssetStage::Extracted,
    }
}

fn default_memory_asset_form_for_room_asset_kind(kind: &MemoryRoomAssetKind) -> MemoryAssetForm {
    match kind {
        MemoryRoomAssetKind::Raw => MemoryAssetForm::RawNote,
        MemoryRoomAssetKind::Compressed => MemoryAssetForm::Summary,
        MemoryRoomAssetKind::Literary => MemoryAssetForm::Rewrite,
        MemoryRoomAssetKind::Facts => MemoryAssetForm::Fact,
        MemoryRoomAssetKind::Timeline => MemoryAssetForm::Workflow,
        MemoryRoomAssetKind::Entities => MemoryAssetForm::Entity,
        MemoryRoomAssetKind::Relations => MemoryAssetForm::Relation,
    }
}

fn default_memory_asset_form_for_memory_kind(
    kind: &MemoryRoomAssetKind,
    memory_kind: &MemoryKind,
) -> MemoryAssetForm {
    match memory_kind {
        MemoryKind::Preference => MemoryAssetForm::Policy,
        MemoryKind::WorkflowMemory => MemoryAssetForm::Workflow,
        MemoryKind::Knowledge => default_memory_asset_form_for_room_asset_kind(kind),
        MemoryKind::Summary | MemoryKind::Decision => {
            default_memory_asset_form_for_room_asset_kind(kind)
        }
    }
}

// Room 能力解析系统
#[derive(Debug, Clone)]
pub struct ResolvedRoomCapabilities {
    pub room_id: String,
    pub capabilities: Vec<ResolvedCapability>,
    pub tools: Vec<ResolvedTool>,
    pub skills: Vec<ResolvedSkill>,
    pub schedules: Vec<ResolvedSchedule>,
}

impl ResolvedRoomCapabilities {
    pub fn new(room_id: impl Into<String>) -> Self {
        Self {
            room_id: room_id.into(),
            capabilities: Vec::new(),
            tools: Vec::new(),
            skills: Vec::new(),
            schedules: Vec::new(),
        }
    }

    /// 获取所有工具 ID
    pub fn tool_ids(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.tool_ref.id.clone()).collect()
    }

    /// 获取所有技能 ID
    pub fn skill_ids(&self) -> Vec<String> {
        self.skills.iter().map(|s| s.skill_ref.id.clone()).collect()
    }

    /// 获取所有能力 ID
    pub fn capability_ids(&self) -> Vec<String> {
        self.capabilities.iter().map(|c| c.capability_ref.id.clone()).collect()
    }

    /// 获取所有启用的调度 ID
    pub fn enabled_schedule_ids(&self) -> Vec<String> {
        self.schedules
            .iter()
            .filter(|s| s.schedule_ref.enabled_in_room)
            .map(|s| s.schedule_ref.id.clone())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedCapability {
    pub capability_ref: CapabilityRef,
    pub source_data: Option<serde_json::Value>, // 这里将来可以是具体的 CapabilityProfile 类型
}

impl ResolvedCapability {
    pub fn new(capability_ref: CapabilityRef) -> Self {
        Self {
            capability_ref,
            source_data: None,
        }
    }

    pub fn with_source_data(mut self, data: serde_json::Value) -> Self {
        self.source_data = Some(data);
        self
    }

    pub fn auto_discovered(capability_id: impl Into<String>) -> Self {
        Self::new(
            CapabilityRef::new(capability_id)
                .with_inheritance_type(InheritanceType::AutoDiscovered),
        )
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTool {
    pub tool_ref: ToolRef,
    pub source_data: Option<serde_json::Value>, // 这里将来可以是具体的 ToolSpec 类型
}

impl ResolvedTool {
    pub fn new(tool_ref: ToolRef) -> Self {
        Self {
            tool_ref,
            source_data: None,
        }
    }

    pub fn with_source_data(mut self, data: serde_json::Value) -> Self {
        self.source_data = Some(data);
        self
    }

    pub fn auto_discovered(tool_id: impl Into<String>) -> Self {
        Self::new(
            ToolRef::new(tool_id)
                .with_inheritance_type(InheritanceType::AutoDiscovered),
        )
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub skill_ref: SkillRef,
    pub source_data: Option<serde_json::Value>, // 这里将来可以是具体的 SkillProfile 类型
}

impl ResolvedSkill {
    pub fn new(skill_ref: SkillRef) -> Self {
        Self {
            skill_ref,
            source_data: None,
        }
    }

    pub fn with_source_data(mut self, data: serde_json::Value) -> Self {
        self.source_data = Some(data);
        self
    }

    pub fn auto_discovered(skill_id: impl Into<String>) -> Self {
        Self::new(
            SkillRef::new(skill_id)
                .with_inheritance_type(InheritanceType::AutoDiscovered),
        )
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSchedule {
    pub schedule_ref: ScheduleRef,
    pub source_data: Option<serde_json::Value>, // 这里将来可以是具体的 ScheduledTask 类型
}

impl ResolvedSchedule {
    pub fn new(schedule_ref: ScheduleRef) -> Self {
        Self {
            schedule_ref,
            source_data: None,
        }
    }

    pub fn with_source_data(mut self, data: serde_json::Value) -> Self {
        self.source_data = Some(data);
        self
    }

    pub fn auto_discovered(schedule_id: impl Into<String>) -> Self {
        Self::new(
            ScheduleRef::new(schedule_id)
                .with_inheritance_type(InheritanceType::AutoDiscovered),
        )
    }
}

/// Room 能力解析器 - 这是一个基础版本，将来需要注入真实的仓库依赖
#[derive(Debug, Clone)]
pub struct RoomCapabilityResolver {
    pub namespace: MemoryNamespace,
}

const RUST_TAG_TOOLS: &[&str] = &["tool.cargo-check", "tool.cargo-test", "tool.cargo-build"];
const SEARCH_TAG_TOOLS: &[&str] = &["tool.rg", "tool.grep", "tool.find"];
const HONEYCOMB_PROJECT_TOOLS: &[&str] = &[
    "tool.cargo-check",
    "tool.cargo-test",
    "tool.rg",
    "tool.local-file.read",
    "tool.local-dir.list",
];
const REFACTOR_TASK_TOOLS: &[&str] = &["tool.rg", "tool.ast-grep", "tool.cargo-check"];
const MEMORY_CRATE_TOOLS: &[&str] = &["tool.cargo-test", "tool.cargo-doc"];

impl RoomCapabilityResolver {
    pub fn new(namespace: MemoryNamespace) -> Self {
        Self { namespace }
    }

    /// 解析 Room 的所有可用能力
    pub fn resolve_room_capabilities(&self, room: &MemoryRoom) -> Result<ResolvedRoomCapabilities> {
        let mut resolved = ResolvedRoomCapabilities::new(&room.id);

        // 1. 解析直接继承的能力
        for cap_ref in &room.inherited_capabilities {
            resolved.capabilities.push(ResolvedCapability::new(cap_ref.clone()));
        }

        // 2. 解析继承的工具
        for tool_ref in &room.inherited_tools {
            resolved.tools.push(ResolvedTool::new(tool_ref.clone()));
        }

        // 3. 解析继承的技能
        for skill_ref in &room.inherited_skills {
            resolved.skills.push(ResolvedSkill::new(skill_ref.clone()));
        }

        // 4. 解析继承的定时任务
        for schedule_ref in &room.inherited_schedules {
            if schedule_ref.enabled_in_room {
                resolved.schedules.push(ResolvedSchedule::new(schedule_ref.clone()));
            }
        }

        // 5. 自动发现相关能力
        if room.room_config.auto_inherit_parent || room.room_config.auto_inherit_siblings {
            self.auto_discover_capabilities(room, &mut resolved)?;
        }

        Ok(resolved)
    }

    /// 自动发现相关能力 - 基础版本
    fn auto_discover_capabilities(
        &self,
        room: &MemoryRoom,
        resolved: &mut ResolvedRoomCapabilities,
    ) -> Result<()> {
        // 基于标签匹配发现能力
        for tag in &room.tags {
            if tag == "rust" {
                self.add_auto_discovered_tools(resolved, RUST_TAG_TOOLS);
            }

            if tag == "search" {
                self.add_auto_discovered_tools(resolved, SEARCH_TAG_TOOLS);
            }
        }

        // 基于相关实体发现能力
        for entity in &room.related_entities {
            match entity.kind {
                MemoryEntityKind::Project => {
                    // 继承项目级能力
                    self.inherit_project_capabilities(&entity.id, resolved)?;
                }
                MemoryEntityKind::Task => {
                    // 继承任务相关能力
                    self.inherit_task_capabilities(&entity.id, resolved)?;
                }
                MemoryEntityKind::Crate => {
                    // 继承 crate 相关能力
                    self.inherit_crate_capabilities(&entity.id, resolved)?;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// 继承项目级能力
    fn inherit_project_capabilities(
        &self,
        project_id: &str,
        resolved: &mut ResolvedRoomCapabilities,
    ) -> Result<()> {
        if project_id.contains("honeycomb") {
            self.add_auto_discovered_tools(resolved, HONEYCOMB_PROJECT_TOOLS);
        }
        
        Ok(())
    }

    /// 继承任务相关能力
    fn inherit_task_capabilities(
        &self,
        task_id: &str,
        resolved: &mut ResolvedRoomCapabilities,
    ) -> Result<()> {
        if task_id.contains("refactor") {
            self.add_auto_discovered_tools(resolved, REFACTOR_TASK_TOOLS);
        }
        
        Ok(())
    }

    /// 继承 crate 相关能力
    fn inherit_crate_capabilities(
        &self,
        crate_id: &str,
        resolved: &mut ResolvedRoomCapabilities,
    ) -> Result<()> {
        if crate_id.contains("memory") {
            self.add_auto_discovered_tools(resolved, MEMORY_CRATE_TOOLS);
        }
        
        Ok(())
    }

    fn add_auto_discovered_tools(
        &self,
        resolved: &mut ResolvedRoomCapabilities,
        tool_ids: &[&str],
    ) {
        for tool_id in tool_ids {
            if !resolved.tools.iter().any(|tool| tool.tool_ref.id == *tool_id) {
                resolved.tools.push(ResolvedTool::auto_discovered(*tool_id));
            }
        }
    }

    /// 为 Room 添加能力引用
    pub fn add_capability_to_room(
        &self,
        room: &mut MemoryRoom,
        capability_ref: CapabilityRef,
    ) -> Result<()> {
        room.add_capability(capability_ref);
        Ok(())
    }

    /// 为 Room 添加工具引用
    pub fn add_tool_to_room(&self, room: &mut MemoryRoom, tool_ref: ToolRef) -> Result<()> {
        room.add_tool(tool_ref);
        Ok(())
    }

    /// 为 Room 添加技能引用
    pub fn add_skill_to_room(&self, room: &mut MemoryRoom, skill_ref: SkillRef) -> Result<()> {
        room.add_skill(skill_ref);
        Ok(())
    }

    /// 为 Room 添加调度引用
    pub fn add_schedule_to_room(
        &self,
        room: &mut MemoryRoom,
        schedule_ref: ScheduleRef,
    ) -> Result<()> {
        room.add_schedule(schedule_ref);
        Ok(())
    }
}
