//! Lightweight memory + llm composition without depending on hc-agent.

pub mod runtime;

use anyhow::Result;
use hc_bootstrap::{tenant_id_from_env, user_id_from_env, wall_clock_ms};
use hc_capability::CapabilityProfile;
use hc_llm::{
    ChatMessage, GenerateRequest, GenerateResponse, LlmError, MessageRole, ProviderRegistry,
    StreamChunk,
};
use hc_memory::{
    ArtifactDraft, ArtifactEvolutionAction, ArtifactEvolutionEvent, MemoryAssetForm,
    MemoryAssetStage, MemoryCatalog, MemoryLayer, MemoryOwnerKind, MemoryOwnerRef, MemoryQuery,
    MemoryRecord, MemoryRepository, MemoryRoomAsset, MemoryRoomAssetKind, MemoryRoomRepository,
};
pub use hc_memory::{MemoryKind, MemoryNamespace, MemoryRoom, MemoryScope};
use hc_persona::PersonaProfile;
use hc_store::store::{
    MarkdownIndex, MarkdownIndexEntry, MarkdownQuery, WorkspaceNamespace, WorkspaceStore,
    query_markdown_index, query_markdown_index_with_search_index,
};
pub use hc_toolchain::{
    EvaluationSignal, ToolCatalog, ToolComposition, ToolExecutionKind, ToolExecutionOutcome,
    ToolExecutionPlan, ToolProvider, ToolRepository, ToolSpec, ToolStability, default_tool_catalog,
    default_tool_command as toolchain_default_tool_command, seed_tool_cargo_test, seed_tool_rg,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievedMemory {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub scope: MemoryScope,
    pub kind: MemoryKind,
    pub layer: Option<MemoryLayer>,
    pub room_id: Option<String>,
    pub source_kind: String,
    pub confidence_milli: u16,
    pub tags: Vec<String>,
    pub derived_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetTarget {
    Tool,
    Workflow,
    Agent,
    Capability,
    Project,
    Task,
    Topic,
    Global,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AssetConsumer {
    Llm,
    Executor,
    Planner,
    Human,
    Evaluator,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    Draft,
    Active,
    Deprecated,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetView {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub kind: MemoryKind,
    pub stage: MemoryAssetStage,
    pub form: MemoryAssetForm,
    pub target: AssetTarget,
    pub target_ref: Option<String>,
    pub consumers: Vec<AssetConsumer>,
    pub status: AssetStatus,
    pub visibility: hc_memory::MemoryVisibility,
    pub tags: Vec<String>,
    pub owners: Vec<MemoryOwnerRef>,
    pub derived_from: Vec<String>,
    pub source_docs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolExecutionEvaluation {
    pub tool_id: String,
    pub matched_asset_ids: Vec<String>,
    pub signals: Vec<EvaluationSignal>,
    pub supporting_events: usize,
    pub generalize_candidate_ids: Vec<String>,
    pub promote_candidate_ids: Vec<String>,
    pub revise_candidate_ids: Vec<String>,
    pub retire_candidate_ids: Vec<String>,
    pub events: Vec<AssetEvolutionEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCapabilityExportAsset {
    pub id: String,
    pub role: String,
    pub title: String,
    pub file: String,
    pub kind: MemoryKind,
    pub stage: MemoryAssetStage,
    pub form: MemoryAssetForm,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCapabilityExportManifest {
    pub schema_version: u16,
    pub package_id: String,
    pub tool: ToolSpec,
    pub command: Vec<String>,
    pub assets: Vec<ToolCapabilityExportAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCapabilityExportPackage {
    pub manifest: ToolCapabilityExportManifest,
    pub plan: ToolExecutionPlan,
}

#[derive(Debug, Clone, Default)]
pub struct DefaultToolExecutionBinder;

pub trait ToolExecutionBinder {
    fn bind(&self, goal: &str, tool: &ToolSpec, assets: &[AssetView]) -> Result<ToolExecutionPlan>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetEvolutionEvent {
    pub id: String,
    pub asset_id: String,
    pub action: EvolutionAction,
    pub reason: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub confidence_milli: u16,
    pub created_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionAction {
    Captured,
    Extracted,
    Generalized,
    Compiled,
    Bound,
    Evaluated,
    Promoted,
    Revised,
    Deprecated,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneralizationPolicy {
    pub min_confidence_milli: u16,
    pub min_supporting_events: usize,
    pub require_repeated_pattern: bool,
    pub allow_human_confirmation_override: bool,
}

impl Default for GeneralizationPolicy {
    fn default() -> Self {
        Self {
            min_confidence_milli: 700,
            min_supporting_events: 2,
            require_repeated_pattern: true,
            allow_human_confirmation_override: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromotionRule {
    pub from_stage: MemoryAssetStage,
    pub to_stage: MemoryAssetStage,
    pub min_confidence_milli: u16,
    pub required_tags: Vec<String>,
    pub required_consumers: Vec<AssetConsumer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetirementRule {
    pub max_failed_evaluations: usize,
    pub retire_on_explicit_human_rejection: bool,
    pub allow_replacement_by_newer_asset: bool,
}

impl Default for RetirementRule {
    fn default() -> Self {
        Self {
            max_failed_evaluations: 3,
            retire_on_explicit_human_rejection: true,
            allow_replacement_by_newer_asset: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomCandidate {
    pub room_id: String,
    pub layer: MemoryLayer,
    pub status: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub score_milli: u16,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptAssetKind {
    SystemPolicy,
    BehaviorTemplate,
    StyleGuide,
    OutputContract,
    PromptMemory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptPolicyKind {
    HardRuntime,
    CompiledMemory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptPolicy {
    pub kind: PromptPolicyKind,
    pub stage: MemoryAssetStage,
    pub form: MemoryAssetForm,
    pub title: String,
    pub content: String,
}

impl PromptPolicy {
    pub fn new(title: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            kind: PromptPolicyKind::HardRuntime,
            stage: MemoryAssetStage::Compiled,
            form: MemoryAssetForm::Policy,
            title: title.into(),
            content: content.into(),
        }
    }

    pub fn compiled_memory(title: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            kind: PromptPolicyKind::CompiledMemory,
            stage: MemoryAssetStage::Compiled,
            form: MemoryAssetForm::Policy,
            title: title.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptAsset {
    pub id: String,
    pub kind: PromptAssetKind,
    pub stage: MemoryAssetStage,
    pub form: MemoryAssetForm,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
}

impl PromptAsset {
    pub fn new(
        id: impl Into<String>,
        kind: PromptAssetKind,
        title: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            stage: prompt_asset_stage_for_kind(&kind),
            form: prompt_asset_form_for_kind(&kind),
            kind,
            title: title.into(),
            content: content.into(),
            tags: Vec::new(),
        }
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfCapability {
    pub name: String,
    pub description: String,
}

impl SelfCapability {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfConstraint {
    pub name: String,
    pub description: String,
}

impl SelfConstraint {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SelfModel {
    pub id: String,
    pub name: String,
    pub role: String,
    pub description: String,
    pub style: Option<String>,
    pub goals: Vec<String>,
    pub capabilities: Vec<SelfCapability>,
    pub constraints: Vec<SelfConstraint>,
}

impl SelfModel {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        role: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            role: role.into(),
            description: description.into(),
            style: None,
            goals: Vec::new(),
            capabilities: Vec::new(),
            constraints: Vec::new(),
        }
    }

    pub fn with_style(mut self, style: impl Into<String>) -> Self {
        self.style = Some(style.into());
        self
    }

    pub fn with_goal(mut self, goal: impl Into<String>) -> Self {
        self.goals.push(goal.into());
        self
    }

    pub fn with_capability(mut self, capability: SelfCapability) -> Self {
        self.capabilities.push(capability);
        self
    }

    pub fn with_constraint(mut self, constraint: SelfConstraint) -> Self {
        self.constraints.push(constraint);
        self
    }
}

pub fn self_model_from_persona_and_capabilities(
    persona: &PersonaProfile,
    capabilities: &[CapabilityProfile],
) -> SelfModel {
    let mut self_model = SelfModel::new(
        persona.id.clone(),
        persona.name.clone(),
        persona.role.clone(),
        persona.description.clone(),
    );

    if !persona.style.trim().is_empty() {
        self_model = self_model.with_style(persona.style.clone());
    }

    for goal in &persona.goals {
        self_model = self_model.with_goal(goal.clone());
    }

    for capability in capabilities {
        let domains = if capability.domains.is_empty() {
            String::new()
        } else {
            format!(" | domains={}", capability.domains.join(","))
        };
        self_model = self_model.with_capability(SelfCapability::new(
            capability.name.clone(),
            format!("{}{}", capability.description, domains)
                .trim()
                .to_owned(),
        ));

        for constraint in &capability.constraints {
            self_model = self_model.with_constraint(SelfConstraint::new(
                capability.name.clone(),
                constraint.clone(),
            ));
        }
    }

    self_model
}

impl From<&MemoryRecord> for RetrievedMemory {
    fn from(record: &MemoryRecord) -> Self {
        Self {
            id: record.id.clone(),
            title: record.title.clone(),
            summary: record.summary.clone(),
            scope: record.scope.clone(),
            kind: record.kind.clone(),
            layer: None,
            room_id: None,
            source_kind: "memory_record".to_owned(),
            confidence_milli: record.confidence_milli,
            tags: record.tags.clone(),
            derived_from: record.derived_from.clone(),
        }
    }
}

impl From<&MemoryRoomAsset> for RetrievedMemory {
    fn from(asset: &MemoryRoomAsset) -> Self {
        Self {
            id: asset.id.clone(),
            title: asset.title.clone(),
            summary: asset.summary.clone(),
            scope: memory_scope_for_layer(&asset.layer),
            kind: asset.memory_kind.clone(),
            layer: Some(asset.layer.clone()),
            room_id: Some(asset.room_id.clone()),
            source_kind: format!("room_{:?}", asset.kind).to_ascii_lowercase(),
            confidence_milli: confidence_for_room_asset_kind(&asset.kind),
            tags: asset.tags.clone(),
            derived_from: asset.derived_from.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextMemoryQuery {
    pub memory_query: MemoryQuery,
    pub limit: Option<usize>,
    pub room_anchor_ids: Vec<String>,
}

impl ContextMemoryQuery {
    pub fn for_namespace(mut self, namespace: MemoryNamespace) -> Self {
        self.memory_query.namespace = Some(namespace);
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.memory_query.text = Some(text.into());
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.memory_query.tag = Some(tag.into());
        self
    }

    pub fn with_scope(mut self, scope: MemoryScope) -> Self {
        self.memory_query.scope = Some(scope);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_room_anchor(mut self, room_id: impl Into<String>) -> Self {
        let room_id = room_id.into();
        if !room_id.trim().is_empty() && !self.room_anchor_ids.iter().any(|id| id == &room_id) {
            self.room_anchor_ids.push(room_id);
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMemoryOptions {
    pub enabled: bool,
    pub namespace: MemoryNamespace,
    pub limit: usize,
    pub scope: Option<MemoryScope>,
    pub kind: Option<MemoryKind>,
    pub tag: Option<String>,
    pub room_anchor_ids: Vec<String>,
}

impl ChatMemoryOptions {
    pub fn new(namespace: MemoryNamespace) -> Self {
        Self {
            enabled: true,
            namespace,
            limit: 5,
            scope: None,
            kind: None,
            tag: None,
            room_anchor_ids: Vec::new(),
        }
    }

    pub fn from_env() -> Self {
        Self {
            enabled: default_chat_memory_enabled_from_env(),
            namespace: runtime_memory_namespace_from_env(),
            ..Self::new(MemoryNamespace::default())
        }
    }

    pub fn build_query(&self, text: impl Into<String>) -> ContextMemoryQuery {
        build_chat_memory_query(
            self.namespace.clone(),
            text,
            self.limit,
            self.scope.clone(),
            self.kind.clone(),
            self.tag.clone(),
            self.room_anchor_ids.clone(),
        )
    }
}

pub fn runtime_memory_namespace_from_env() -> MemoryNamespace {
    MemoryNamespace::new(tenant_id_from_env(), user_id_from_env())
}

pub fn default_chat_memory_enabled_from_env() -> bool {
    env::var("HC_CONTEXT_CHAT_MEMORY")
        .or_else(|_| env::var("HC_CONTEXT_MEMORY"))
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

pub fn build_chat_memory_query(
    namespace: MemoryNamespace,
    text: impl Into<String>,
    limit: usize,
    scope: Option<MemoryScope>,
    kind: Option<MemoryKind>,
    tag: Option<String>,
    room_anchor_ids: Vec<String>,
) -> ContextMemoryQuery {
    let mut query = ContextMemoryQuery::default()
        .for_namespace(namespace)
        .with_text(text)
        .with_limit(limit.max(1));
    if let Some(scope) = scope {
        query = query.with_scope(scope);
    }
    if let Some(kind) = kind {
        query.memory_query.kind = Some(kind);
    }
    if let Some(tag) = tag
        && !tag.trim().is_empty()
    {
        query = query.with_tag(tag);
    }
    for room_id in room_anchor_ids {
        query = query.with_room_anchor(room_id);
    }
    query
}

pub fn render_recalled_memory_context(memories: &[RetrievedMemory]) -> Option<String> {
    if memories.is_empty() {
        return None;
    }
    let items = memories
        .iter()
        .take(8)
        .map(render_recalled_memory_context_item)
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "Recalled memory for this turn:\n{items}\nUse these as durable/background context when relevant. If a recalled item conflicts with the latest user message, prefer the latest user message."
    ))
}

pub fn render_recalled_memory_context_item(memory: &RetrievedMemory) -> String {
    let tags = if memory.tags.is_empty() {
        String::new()
    } else {
        format!(" tags={}", memory.tags.join(","))
    };
    format!(
        "- [{}:{} confidence={:.2}{}] {}: {}",
        memory_scope_label(&memory.scope),
        memory_kind_label(&memory.kind),
        f32::from(memory.confidence_milli) / 1000.0,
        tags,
        memory.title.trim(),
        memory.summary.trim()
    )
}

pub fn memory_scope_label(scope: &MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Global => "global",
        MemoryScope::Persona => "persona",
        MemoryScope::Session => "session",
        MemoryScope::Instance => "instance",
        MemoryScope::Project => "project",
        MemoryScope::Task => "task",
    }
}

pub fn memory_kind_label(kind: &MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Summary => "summary",
        MemoryKind::Decision => "decision",
        MemoryKind::Preference => "preference",
        MemoryKind::Knowledge => "knowledge",
        MemoryKind::WorkflowMemory => "workflow_memory",
    }
}

pub fn parse_memory_scope(value: &str) -> Result<MemoryScope> {
    match value.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(MemoryScope::Global),
        "persona" => Ok(MemoryScope::Persona),
        "session" => Ok(MemoryScope::Session),
        "instance" => Ok(MemoryScope::Instance),
        "project" => Ok(MemoryScope::Project),
        "task" => Ok(MemoryScope::Task),
        other => anyhow::bail!("unsupported scope: {other}"),
    }
}

pub fn parse_memory_kind(value: &str) -> Result<MemoryKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "summary" => Ok(MemoryKind::Summary),
        "decision" => Ok(MemoryKind::Decision),
        "preference" => Ok(MemoryKind::Preference),
        "knowledge" => Ok(MemoryKind::Knowledge),
        "workflow_memory" | "workflow-memory" => Ok(MemoryKind::WorkflowMemory),
        other => anyhow::bail!("unsupported memory kind: {other}"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextRequest {
    pub generation: GenerateRequest,
    pub memory_query: ContextMemoryQuery,
    pub system_prompt: Option<String>,
    pub self_model: Option<SelfModel>,
    pub prompt_policies: Vec<PromptPolicy>,
    pub prompt_assets: Vec<PromptAsset>,
}

impl ContextRequest {
    pub fn new(generation: GenerateRequest) -> Self {
        Self {
            generation,
            memory_query: ContextMemoryQuery::default(),
            system_prompt: None,
            self_model: None,
            prompt_policies: Vec::new(),
            prompt_assets: Vec::new(),
        }
    }

    pub fn with_memory_query(mut self, memory_query: ContextMemoryQuery) -> Self {
        self.memory_query = memory_query;
        self
    }

    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    pub fn with_self_model(mut self, self_model: SelfModel) -> Self {
        self.self_model = Some(self_model);
        self
    }

    pub fn with_prompt_policy(mut self, prompt_policy: PromptPolicy) -> Self {
        self.prompt_policies.push(prompt_policy);
        self
    }

    pub fn with_prompt_asset(mut self, prompt_asset: PromptAsset) -> Self {
        self.prompt_assets.push(prompt_asset);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextResponse {
    pub response: GenerateResponse,
    pub recalled_memories: Vec<RetrievedMemory>,
    pub synthesized_prompt_assets: Vec<PromptAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOrganizationInput {
    pub namespace: MemoryNamespace,
    pub content: String,
    pub title_hint: Option<String>,
    pub room_id_hint: Option<String>,
    pub room_layer_hint: Option<MemoryLayer>,
    pub owner: Option<MemoryOwnerRef>,
    pub visibility: hc_memory::MemoryVisibility,
    pub tags: Vec<String>,
}

impl MemoryOrganizationInput {
    pub fn new(namespace: MemoryNamespace, content: impl Into<String>) -> Self {
        Self {
            namespace,
            content: content.into(),
            title_hint: None,
            room_id_hint: None,
            room_layer_hint: None,
            owner: None,
            visibility: hc_memory::MemoryVisibility::Private,
            tags: Vec::new(),
        }
    }

    pub fn with_title_hint(mut self, title_hint: impl Into<String>) -> Self {
        self.title_hint = Some(title_hint.into());
        self
    }

    pub fn with_room_hint(mut self, room_id: impl Into<String>, room_layer: MemoryLayer) -> Self {
        self.room_id_hint = Some(room_id.into());
        self.room_layer_hint = Some(room_layer);
        self
    }

    pub fn with_owner(mut self, owner: MemoryOwnerRef) -> Self {
        self.owner = Some(owner);
        self
    }

    pub fn with_visibility(mut self, visibility: hc_memory::MemoryVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRoomRoute {
    pub room_id: String,
    pub room_layer: MemoryLayer,
    pub title: String,
    pub owners: Vec<MemoryOwnerRef>,
    pub visibility: hc_memory::MemoryVisibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryPromotionSuggestion {
    pub target_layer: MemoryLayer,
    pub target_room_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOrganizationDecision {
    pub route: MemoryRoomRoute,
    pub memory_kind: MemoryKind,
    pub tags: Vec<String>,
    pub promotions: Vec<MemoryPromotionSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomMemoryWriteRequest {
    pub room_id: String,
    pub room_layer: MemoryLayer,
    pub title: String,
    pub summary: String,
    pub memory_kind: MemoryKind,
    pub visibility: hc_memory::MemoryVisibility,
    pub owners: Vec<MemoryOwnerRef>,
    pub tags: Vec<String>,
    pub derived_from: Vec<String>,
    pub source_docs: Vec<String>,
    pub file_name: Option<String>,
    pub asset_id: Option<String>,
}

impl RoomMemoryWriteRequest {
    pub fn new(
        room_id: impl Into<String>,
        room_layer: MemoryLayer,
        title: impl Into<String>,
        summary: impl Into<String>,
        memory_kind: MemoryKind,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            room_layer,
            title: title.into(),
            summary: summary.into(),
            memory_kind,
            visibility: hc_memory::MemoryVisibility::Private,
            owners: Vec::new(),
            tags: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
            file_name: None,
            asset_id: None,
        }
    }

    pub fn with_visibility(mut self, visibility: hc_memory::MemoryVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_owner(mut self, owner: MemoryOwnerRef) -> Self {
        self.owners.push(owner);
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn with_derived_from(mut self, derived_from: impl Into<String>) -> Self {
        self.derived_from.push(derived_from.into());
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

    pub fn with_asset_id(mut self, asset_id: impl Into<String>) -> Self {
        self.asset_id = Some(asset_id.into());
        self
    }
}

pub trait MemoryRetriever {
    fn retrieve(&self, query: &ContextMemoryQuery) -> Result<Vec<RetrievedMemory>>;
}

pub trait MemoryRoomRouter {
    fn route_room(&self, input: &MemoryOrganizationInput) -> Result<MemoryRoomRoute>;
}

pub trait MemoryKindResolver {
    fn resolve_kind(&self, input: &MemoryOrganizationInput) -> Result<MemoryKind>;
}

pub trait MemoryTagSuggester {
    fn suggest_tags(&self, input: &MemoryOrganizationInput) -> Result<Vec<String>>;
}

pub trait MemoryPromotionAdvisor {
    fn suggest_promotions(
        &self,
        input: &MemoryOrganizationInput,
        route: &MemoryRoomRoute,
        memory_kind: MemoryKind,
    ) -> Result<Vec<MemoryPromotionSuggestion>>;
}

pub trait PromptAssetSynthesizer {
    fn synthesize(&self, memories: &[RetrievedMemory]) -> Result<Vec<PromptAsset>>;
}

pub trait ContextComposer {
    fn compose_messages(
        &self,
        system_prompt: Option<&str>,
        self_model: Option<&SelfModel>,
        prompt_policies: &[PromptPolicy],
        prompt_assets: &[PromptAsset],
        memories: &[RetrievedMemory],
        user_messages: &[ChatMessage],
    ) -> Vec<ChatMessage>;
}

#[derive(Debug, Clone, Default)]
pub struct RuleBasedMemoryRoomRouter;

#[derive(Debug, Clone, Default)]
pub struct RuleBasedMemoryKindResolver;

#[derive(Debug, Clone, Default)]
pub struct KeywordMemoryTagSuggester;

#[derive(Debug, Clone, Default)]
pub struct NoopMemoryPromotionAdvisor;

#[derive(Debug, Clone, Default)]
pub struct RuleBasedMemoryPromotionAdvisor;

#[derive(Debug, Clone, Default)]
pub struct DefaultPromptAssetSynthesizer;

#[derive(Clone)]
pub struct LlmPromptAssetSynthesizer<'a, F> {
    registry: &'a ProviderRegistry,
    model: hc_llm::ModelRef,
    workspace_namespace: WorkspaceNamespace,
    fallback: F,
    fallback_on_error: bool,
}

#[derive(Clone)]
pub struct LlmMemoryTagSuggester<'a, F> {
    registry: &'a ProviderRegistry,
    model: hc_llm::ModelRef,
    workspace_namespace: WorkspaceNamespace,
    fallback: F,
    fallback_on_error: bool,
}

#[derive(Clone)]
pub struct LlmMemoryOrganizer<'a, F> {
    registry: &'a ProviderRegistry,
    model: hc_llm::ModelRef,
    workspace_namespace: WorkspaceNamespace,
    fallback: F,
    fallback_on_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManagedPromptMetadata {
    id: String,
    r#type: String,
    title: String,
    kind: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedPromptKind {
    MemoryOrganizer,
    PromptAssetSynthesizer,
    SemanticTagSuggester,
    GlobalPreferenceSummary,
    AssistantWenyanRewrite,
    ToolChatAssistant,
    ToolRouter,
    ToolNaturalLanguageBuilder,
    AgentResponderSystem,
    AgentPlannerInput,
    AgentWorkItemExecution,
    ContextMemorySystem,
    ContextMemoryUsagePolicy,
    ContextLightweightChat,
    JsonSystemGuard,
}

#[derive(Debug, Clone)]
pub struct CompositeMemoryOrganizer<R, K, T, P> {
    router: R,
    kind_resolver: K,
    tag_suggester: T,
    promotion_advisor: P,
}

impl<R, K, T, P> CompositeMemoryOrganizer<R, K, T, P> {
    pub fn new(router: R, kind_resolver: K, tag_suggester: T, promotion_advisor: P) -> Self {
        Self {
            router,
            kind_resolver,
            tag_suggester,
            promotion_advisor,
        }
    }
}

impl<'a, F> LlmPromptAssetSynthesizer<'a, F> {
    pub fn new(
        registry: &'a ProviderRegistry,
        model: hc_llm::ModelRef,
        workspace_namespace: WorkspaceNamespace,
        fallback: F,
    ) -> Self {
        Self {
            registry,
            model,
            workspace_namespace,
            fallback,
            fallback_on_error: true,
        }
    }

    pub fn strict(
        registry: &'a ProviderRegistry,
        model: hc_llm::ModelRef,
        workspace_namespace: WorkspaceNamespace,
        fallback: F,
    ) -> Self {
        Self {
            registry,
            model,
            workspace_namespace,
            fallback,
            fallback_on_error: false,
        }
    }
}

impl<'a, F> LlmMemoryTagSuggester<'a, F> {
    pub fn new(
        registry: &'a ProviderRegistry,
        model: hc_llm::ModelRef,
        workspace_namespace: WorkspaceNamespace,
        fallback: F,
    ) -> Self {
        Self {
            registry,
            model,
            workspace_namespace,
            fallback,
            fallback_on_error: true,
        }
    }

    pub fn strict(
        registry: &'a ProviderRegistry,
        model: hc_llm::ModelRef,
        workspace_namespace: WorkspaceNamespace,
        fallback: F,
    ) -> Self {
        Self {
            registry,
            model,
            workspace_namespace,
            fallback,
            fallback_on_error: false,
        }
    }
}

impl<'a, F> LlmMemoryOrganizer<'a, F> {
    pub fn new(
        registry: &'a ProviderRegistry,
        model: hc_llm::ModelRef,
        workspace_namespace: WorkspaceNamespace,
        fallback: F,
    ) -> Self {
        Self {
            registry,
            model,
            workspace_namespace,
            fallback,
            fallback_on_error: true,
        }
    }

    pub fn strict(
        registry: &'a ProviderRegistry,
        model: hc_llm::ModelRef,
        workspace_namespace: WorkspaceNamespace,
        fallback: F,
    ) -> Self {
        Self {
            registry,
            model,
            workspace_namespace,
            fallback,
            fallback_on_error: false,
        }
    }
}

pub trait MemoryOrganizer {
    fn organize(&self, input: &MemoryOrganizationInput) -> Result<MemoryOrganizationDecision>;
}

impl<R, K, T, P> MemoryOrganizer for CompositeMemoryOrganizer<R, K, T, P>
where
    R: MemoryRoomRouter,
    K: MemoryKindResolver,
    T: MemoryTagSuggester,
    P: MemoryPromotionAdvisor,
{
    fn organize(&self, input: &MemoryOrganizationInput) -> Result<MemoryOrganizationDecision> {
        let route = self.router.route_room(input)?;
        let memory_kind = self.kind_resolver.resolve_kind(input)?;
        let mut tags = input.tags.clone();
        for tag in self.tag_suggester.suggest_tags(input)? {
            if !tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&tag))
            {
                tags.push(tag);
            }
        }
        let promotions =
            self.promotion_advisor
                .suggest_promotions(input, &route, memory_kind.clone())?;

        Ok(MemoryOrganizationDecision {
            route,
            memory_kind,
            tags,
            promotions,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceMemoryRetriever {
    root: std::path::PathBuf,
    namespace: WorkspaceNamespace,
}

impl WorkspaceMemoryRetriever {
    pub fn new(root: impl Into<std::path::PathBuf>, namespace: WorkspaceNamespace) -> Self {
        Self {
            root: root.into(),
            namespace,
        }
    }

    pub fn discover_room_candidates(
        &self,
        query: &ContextMemoryQuery,
    ) -> Result<Vec<RoomCandidate>> {
        discover_room_candidates(&self.root, &self.namespace, query)
    }
}

impl MemoryRetriever for WorkspaceMemoryRetriever {
    fn retrieve(&self, query: &ContextMemoryQuery) -> Result<Vec<RetrievedMemory>> {
        let started_at = Instant::now();
        let store = WorkspaceStore::new(self.root.clone());
        let markdown_index = store.read_or_rebuild_markdown_index_in_namespace(&self.namespace)?;
        let cache_key =
            MemoryRecallCacheKey::new(&self.root, &self.namespace, query, &markdown_index);
        if let Some(memories) = memory_recall_cache_get(&cache_key) {
            emit_memory_recall_trace(
                "cache_hit",
                query,
                &MemoryRecallTraceStats {
                    elapsed_ms: started_at.elapsed().as_millis(),
                    index_documents: markdown_index.documents.len(),
                    returned_count: memories.len(),
                    cache_hit: true,
                    ..Default::default()
                },
            );
            return Ok(memories);
        }
        let mut trace_stats = MemoryRecallTraceStats {
            index_documents: markdown_index.documents.len(),
            ..Default::default()
        };
        let repository =
            MemoryRepository::with_namespace(self.root.clone(), self.namespace.clone());
        let room_repository =
            MemoryRoomRepository::with_namespace(self.root.clone(), self.namespace.clone());
        let room_candidates = discover_room_candidates_from_index(
            &store,
            &room_repository,
            &self.namespace,
            &markdown_index,
            query,
        )?;
        trace_stats.room_candidate_count = room_candidates.len();
        let task_room_allowlist = task_scoped_room_allowlist_for_retrieval(query, &room_candidates);
        let room_candidate_boosts = room_candidates
            .iter()
            .map(|candidate| (candidate.room_id.clone(), candidate.score_milli))
            .collect::<std::collections::BTreeMap<_, _>>();

        let mut record_markdown_query = MarkdownQuery::default().with_path_prefix("memory");
        if let Some(tag) = &query.memory_query.tag {
            record_markdown_query = record_markdown_query.with_tag(tag.clone());
        }
        if let Some(text) = &query.memory_query.text {
            record_markdown_query = record_markdown_query.with_text(text.clone());
        }

        let entries = query_markdown_index_with_search_index(
            &store,
            &self.namespace,
            &markdown_index,
            &record_markdown_query,
        )?;
        trace_stats.record_entry_candidates = entries.len();
        let mut catalog = MemoryCatalog::new();
        for entry in entries {
            if !entry.relative_path.starts_with("memory/") || entry.doc_type != "memory" {
                continue;
            }
            let record = match repository.read_record(&entry.relative_path) {
                Ok(record) => record,
                Err(error) if is_not_found_error(&error) => {
                    trace_stats.record_entry_missing += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(allow) = task_room_allowlist.as_ref() {
                if record.scope == MemoryScope::Task && record.owner.kind == MemoryOwnerKind::Task {
                    if !allow.contains(&record.owner.id) {
                        continue;
                    }
                }
            }
            trace_stats.record_reads += 1;
            catalog.insert(record);
        }

        let mut matches = catalog
            .find(&query.memory_query)
            .into_iter()
            .map(RetrievedMemory::from)
            .collect::<Vec<_>>();

        let mut room_markdown_query = MarkdownQuery::default()
            .with_path_prefix("memory/rooms")
            .with_doc_type("memory_room_asset");
        if let Some(tag) = &query.memory_query.tag {
            room_markdown_query = room_markdown_query.with_tag(tag.clone());
        }
        if let Some(text) = &query.memory_query.text {
            room_markdown_query = room_markdown_query.with_text(text.clone());
        }

        let room_entries = query_markdown_index_with_search_index(
            &store,
            &self.namespace,
            &markdown_index,
            &room_markdown_query,
        )?;
        trace_stats.room_entry_candidates = room_entries.len();
        let mut seen_match_ids = matches
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for entry in room_entries {
            if !room_asset_entry_may_match_query(query, &entry) {
                trace_stats.room_entry_prefiltered += 1;
                continue;
            }
            if let Some(allow) = task_room_allowlist.as_ref() {
                match entry.room_id.as_deref() {
                    Some(room_id) if allow.contains(room_id) => {}
                    _ => {
                        trace_stats.room_entry_prefiltered += 1;
                        continue;
                    }
                }
            }
            let asset = match room_repository.read_asset(&entry.relative_path) {
                Ok(asset) => asset,
                Err(error) if is_not_found_error(&error) => {
                    trace_stats.room_entry_missing += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            trace_stats.room_asset_reads += 1;
            let mut retrieved = RetrievedMemory::from(&asset);
            if let Some(boost) = room_candidate_boosts.get(&asset.room_id) {
                retrieved.confidence_milli = retrieved
                    .confidence_milli
                    .saturating_add(*boost / 4)
                    .min(1000);
            }
            if room_asset_matches_query(query, &asset, &retrieved) {
                seen_match_ids.insert(retrieved.id.clone());
                matches.push(retrieved);
            }
        }

        if should_auto_include_global_preferences(query) {
            for entry in global_preference_room_asset_entries(&markdown_index) {
                if seen_match_ids.contains(&entry.id) {
                    continue;
                }
                let asset = match room_repository.read_asset(&entry.relative_path) {
                    Ok(asset) => asset,
                    Err(error) if is_not_found_error(&error) => {
                        trace_stats.room_entry_missing += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let retrieved = RetrievedMemory::from(&asset);
                seen_match_ids.insert(retrieved.id.clone());
                matches.push(retrieved);
                trace_stats.global_preference_reads += 1;
            }
        }

        for candidate in room_candidates.iter().filter(|candidate| {
            candidate.score_milli >= 700
                && candidate
                    .reasons
                    .iter()
                    .any(|reason| reason == "anchor-room" || reason == "anchor-related")
        }) {
            let Some((room, _)) = read_room_by_id(
                &store,
                &room_repository,
                &self.namespace,
                &markdown_index,
                &candidate.room_id,
            )?
            else {
                continue;
            };
            let assets = room_repository.read_compressed_assets(&room)?;
            trace_stats.anchor_room_asset_reads += assets.len();
            for asset in assets.into_iter().rev().take(2) {
                if seen_match_ids.contains(&asset.id) {
                    continue;
                }
                let mut retrieved = RetrievedMemory::from(&asset);
                retrieved.confidence_milli = retrieved
                    .confidence_milli
                    .saturating_add(candidate.score_milli / 3)
                    .min(1000);
                seen_match_ids.insert(retrieved.id.clone());
                matches.push(retrieved);
            }
        }

        matches.sort_by(|left, right| {
            right
                .confidence_milli
                .cmp(&left.confidence_milli)
                .then_with(|| left.id.cmp(&right.id))
        });
        matches.dedup_by(|left, right| left.id == right.id);
        matches = apply_room_kind_budgets(matches);
        if let Some(limit) = query.limit {
            matches.truncate(limit);
        }
        trace_stats.elapsed_ms = started_at.elapsed().as_millis();
        trace_stats.returned_count = matches.len();
        emit_memory_recall_trace("cache_miss", query, &trace_stats);
        memory_recall_cache_insert(cache_key, matches.clone());
        Ok(matches)
    }
}

#[derive(Debug, Clone, Default)]
struct MemoryRecallTraceStats {
    elapsed_ms: u128,
    index_documents: usize,
    room_candidate_count: usize,
    record_entry_candidates: usize,
    record_entry_missing: usize,
    record_reads: usize,
    room_entry_candidates: usize,
    room_entry_prefiltered: usize,
    room_entry_missing: usize,
    room_asset_reads: usize,
    global_preference_reads: usize,
    anchor_room_asset_reads: usize,
    returned_count: usize,
    cache_hit: bool,
}

fn emit_memory_recall_trace(
    status: &'static str,
    query: &ContextMemoryQuery,
    stats: &MemoryRecallTraceStats,
) {
    hc_trace::emit_trace(
        hc_trace::TraceEvent::debug(
            "hc-context",
            "memory_recall",
            "retrieve",
            "retrieved workspace memory",
        )
        .with_status(status)
        .with_field("elapsed_ms", stats.elapsed_ms.to_string())
        .with_field("index_documents", stats.index_documents.to_string())
        .with_field(
            "room_candidate_count",
            stats.room_candidate_count.to_string(),
        )
        .with_field(
            "record_entry_candidates",
            stats.record_entry_candidates.to_string(),
        )
        .with_field(
            "record_entry_missing",
            stats.record_entry_missing.to_string(),
        )
        .with_field("record_reads", stats.record_reads.to_string())
        .with_field(
            "room_entry_candidates",
            stats.room_entry_candidates.to_string(),
        )
        .with_field(
            "room_entry_prefiltered",
            stats.room_entry_prefiltered.to_string(),
        )
        .with_field("room_entry_missing", stats.room_entry_missing.to_string())
        .with_field("room_asset_reads", stats.room_asset_reads.to_string())
        .with_field(
            "global_preference_reads",
            stats.global_preference_reads.to_string(),
        )
        .with_field(
            "anchor_room_asset_reads",
            stats.anchor_room_asset_reads.to_string(),
        )
        .with_field("returned_count", stats.returned_count.to_string())
        .with_field("cache_hit", stats.cache_hit.to_string())
        .with_field("has_text", query.memory_query.text.is_some().to_string())
        .with_field("has_tag", query.memory_query.tag.is_some().to_string())
        .with_field("has_scope", query.memory_query.scope.is_some().to_string())
        .with_field("has_owner", query.memory_query.owner.is_some().to_string())
        .with_field("has_kind", query.memory_query.kind.is_some().to_string())
        .with_field("limit", query.limit.unwrap_or_default().to_string())
        .with_field("room_anchor_count", query.room_anchor_ids.len().to_string()),
    );
}

fn global_preference_room_asset_entries(markdown_index: &MarkdownIndex) -> Vec<MarkdownIndexEntry> {
    let mut entries = markdown_index
        .documents
        .iter()
        .filter(|entry| {
            entry.relative_path.starts_with("memory/rooms/")
                && entry.doc_type == "memory_room_asset"
                && entry.layer.as_deref() == Some("global")
                && entry.memory_kind.as_deref() == Some("preference")
        })
        .cloned()
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then_with(|| left.id.cmp(&right.id))
    });
    entries
}

fn should_auto_include_global_preferences(query: &ContextMemoryQuery) -> bool {
    let scope_allows = query
        .memory_query
        .scope
        .as_ref()
        .is_none_or(|scope| matches!(scope, MemoryScope::Global));
    let kind_allows = query
        .memory_query
        .kind
        .as_ref()
        .is_none_or(|kind| matches!(kind, MemoryKind::Preference));
    let tag_allows = query.memory_query.tag.as_ref().is_none_or(|tag| {
        matches!(
            tag.as_str(),
            "global" | "preference" | "identity" | "assistant-name"
        )
    });
    scope_allows && kind_allows && tag_allows
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == ErrorKind::NotFound)
    })
}

const MEMORY_RECALL_CACHE_TTL: Duration = Duration::from_secs(2);
const MEMORY_RECALL_CACHE_MAX_ENTRIES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MemoryRecallCacheKey {
    root: String,
    tenant_id: String,
    user_id: String,
    index_generated_at_ms: u64,
    scope: Option<String>,
    owner: Option<(String, String)>,
    kind: Option<String>,
    tag: Option<String>,
    text: Option<String>,
    limit: Option<usize>,
    room_anchor_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct MemoryRecallCacheEntry {
    stored_at: Instant,
    memories: Vec<RetrievedMemory>,
}

impl MemoryRecallCacheKey {
    fn new(
        root: &Path,
        namespace: &WorkspaceNamespace,
        query: &ContextMemoryQuery,
        markdown_index: &MarkdownIndex,
    ) -> Self {
        let mut room_anchor_ids = query.room_anchor_ids.clone();
        room_anchor_ids.sort();
        Self {
            root: root.to_string_lossy().to_string(),
            tenant_id: namespace.tenant_id.clone(),
            user_id: namespace.user_id.clone(),
            index_generated_at_ms: markdown_index.generated_at_ms,
            scope: query
                .memory_query
                .scope
                .as_ref()
                .map(|scope| format!("{scope:?}")),
            owner: query
                .memory_query
                .owner
                .as_ref()
                .map(|owner| (format!("{:?}", owner.kind), owner.id.clone())),
            kind: query
                .memory_query
                .kind
                .as_ref()
                .map(|kind| memory_kind_label(kind).to_owned()),
            tag: query.memory_query.tag.clone(),
            text: query.memory_query.text.clone(),
            limit: query.limit,
            room_anchor_ids,
        }
    }
}

fn memory_recall_cache() -> &'static Mutex<BTreeMap<MemoryRecallCacheKey, MemoryRecallCacheEntry>> {
    static CACHE: OnceLock<Mutex<BTreeMap<MemoryRecallCacheKey, MemoryRecallCacheEntry>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn memory_recall_cache_get(key: &MemoryRecallCacheKey) -> Option<Vec<RetrievedMemory>> {
    let mut cache = memory_recall_cache().lock().ok()?;
    let Some(entry) = cache.get(key) else {
        return None;
    };
    if entry.stored_at.elapsed() <= MEMORY_RECALL_CACHE_TTL {
        return Some(entry.memories.clone());
    }
    cache.remove(key);
    None
}

fn memory_recall_cache_insert(key: MemoryRecallCacheKey, memories: Vec<RetrievedMemory>) {
    let Ok(mut cache) = memory_recall_cache().lock() else {
        return;
    };
    prune_memory_recall_cache(&mut cache);
    while cache.len() >= MEMORY_RECALL_CACHE_MAX_ENTRIES {
        let Some(oldest_key) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.stored_at)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        cache.remove(&oldest_key);
    }
    cache.insert(
        key,
        MemoryRecallCacheEntry {
            stored_at: Instant::now(),
            memories,
        },
    );
}

fn prune_memory_recall_cache(cache: &mut BTreeMap<MemoryRecallCacheKey, MemoryRecallCacheEntry>) {
    let expired = cache
        .iter()
        .filter(|(_, entry)| entry.stored_at.elapsed() > MEMORY_RECALL_CACHE_TTL)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in expired {
        cache.remove(&key);
    }
}

fn apply_room_kind_budgets(matches: Vec<RetrievedMemory>) -> Vec<RetrievedMemory> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut selected = Vec::new();
    let mut overflow = Vec::new();

    for memory in matches {
        let kind = retrieved_memory_room_kind(&memory);
        let budget = room_kind_budget(kind);
        let count = counts.entry(kind).or_default();
        if *count < budget {
            *count += 1;
            selected.push(memory);
        } else {
            overflow.push(memory);
        }
    }

    selected.extend(overflow.into_iter().filter(|memory| {
        let kind = retrieved_memory_room_kind(memory);
        kind == "other"
    }));
    selected
}

fn retrieved_memory_room_kind(memory: &RetrievedMemory) -> &'static str {
    if let Some(room_id) = &memory.room_id {
        if room_id.starts_with("room.agent.") || memory.tags.iter().any(|tag| tag == "agent") {
            "agent"
        } else if room_id.starts_with("room.tool.") || memory.tags.iter().any(|tag| tag == "tool") {
            "tool"
        } else if room_id.starts_with("room.project.")
            || memory.tags.iter().any(|tag| tag == "project")
        {
            "project"
        } else if room_id.starts_with("room.task.") || memory.tags.iter().any(|tag| tag == "task") {
            "task"
        } else if room_id.starts_with("room.topic.") || memory.tags.iter().any(|tag| tag == "topic")
        {
            "topic"
        } else if room_id.starts_with("room.chat.") || memory.tags.iter().any(|tag| tag == "chat") {
            "chat"
        } else if room_id.starts_with("room.global.")
            || memory.tags.iter().any(|tag| tag == "global")
        {
            "global"
        } else {
            "other"
        }
    } else if matches!(memory.layer, Some(MemoryLayer::Global))
        || memory.scope == MemoryScope::Global
    {
        "global"
    } else {
        "other"
    }
}

fn room_kind_budget(kind: &str) -> usize {
    match kind {
        "chat" => 4,
        "task" => 3,
        "topic" => 2,
        "tool" => 2,
        "project" => 2,
        "agent" => 1,
        "global" => 1,
        _ => 2,
    }
}

/// When retrieving with [`MemoryScope::Task`] and non-empty [`ContextMemoryQuery::room_anchor_ids`],
/// restrict task-bound flat records and indexed room assets to the anchor room ids plus rooms
/// discovered with `anchor-room` / `anchor-related` reasons (avoids parallel task rooms mixing on
/// incidental text matches; aligns with swarm chat default when a task room anchor is known).
#[must_use]
fn task_scoped_room_allowlist_for_retrieval(
    query: &ContextMemoryQuery,
    room_candidates: &[RoomCandidate],
) -> Option<BTreeSet<String>> {
    if !matches!(query.memory_query.scope, Some(MemoryScope::Task)) {
        return None;
    }
    if query.room_anchor_ids.is_empty() {
        return None;
    }

    let mut allow = BTreeSet::new();
    for anchor in &query.room_anchor_ids {
        let trimmed = anchor.trim();
        if trimmed.is_empty() {
            continue;
        }
        allow.insert(trimmed.to_owned());
    }

    for candidate in room_candidates {
        if !candidate
            .reasons
            .iter()
            .any(|reason| reason == "anchor-room" || reason == "anchor-related")
        {
            continue;
        }
        allow.insert(candidate.room_id.clone());
    }

    Some(allow)
}

pub fn discover_room_candidates(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    query: &ContextMemoryQuery,
) -> Result<Vec<RoomCandidate>> {
    let root = root.as_ref().to_path_buf();
    let store = WorkspaceStore::new(root.clone());
    let room_repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let markdown_index = store.read_or_rebuild_markdown_index_in_namespace(namespace)?;

    discover_room_candidates_from_index(&store, &room_repository, namespace, &markdown_index, query)
}

fn discover_room_candidates_from_index(
    store: &WorkspaceStore,
    room_repository: &MemoryRoomRepository,
    namespace: &WorkspaceNamespace,
    markdown_index: &MarkdownIndex,
    query: &ContextMemoryQuery,
) -> Result<Vec<RoomCandidate>> {
    let mut markdown_query = MarkdownQuery::default()
        .with_path_prefix("memory/rooms")
        .with_doc_type("memory_room");

    if let Some(text) = &query.memory_query.text {
        markdown_query = markdown_query.with_text(text.clone());
    }
    if let Some(tag) = &query.memory_query.tag {
        markdown_query = markdown_query.with_tag(tag.clone());
    }

    let entries =
        query_markdown_index_with_search_index(store, namespace, markdown_index, &markdown_query)?;
    let mut candidates = Vec::new();
    let mut seen_room_ids = std::collections::BTreeSet::new();
    let mut related_room_ids = BTreeMap::<String, (u16, Vec<String>)>::new();

    for entry in entries {
        let room = room_repository.read_room(&entry.relative_path)?;
        let modified_at = modified_time_for_relative_path(&store, namespace, &entry.relative_path);
        collect_related_room_ids(&room, 120, "related-room", &mut related_room_ids);
        seen_room_ids.insert(room.id.clone());
        candidates.push(build_room_candidate(
            &room,
            query,
            modified_at,
            0,
            Vec::new(),
        ));
    }

    for room_id in &query.room_anchor_ids {
        if seen_room_ids.contains(room_id) {
            continue;
        }
        let Some((room, modified_at)) =
            read_room_by_id(store, room_repository, namespace, markdown_index, room_id)?
        else {
            continue;
        };
        collect_related_room_ids(&room, 220, "anchor-related", &mut related_room_ids);
        seen_room_ids.insert(room.id.clone());
        candidates.push(build_room_candidate(
            &room,
            query,
            modified_at,
            280,
            vec!["anchor-room".to_owned()],
        ));
    }

    for (related_room_id, (extra_score, reasons)) in related_room_ids {
        if seen_room_ids.contains(&related_room_id) {
            continue;
        }
        let Some((room, modified_at)) = read_room_by_id(
            store,
            room_repository,
            namespace,
            markdown_index,
            &related_room_id,
        )?
        else {
            continue;
        };
        candidates.push(build_room_candidate(
            &room,
            query,
            modified_at,
            extra_score,
            reasons,
        ));
    }

    candidates.sort_by(|left, right| {
        right
            .score_milli
            .cmp(&left.score_milli)
            .then_with(|| left.room_id.cmp(&right.room_id))
    });
    candidates.dedup_by(|left, right| left.room_id == right.room_id);
    Ok(candidates)
}

fn collect_related_room_ids(
    room: &hc_memory::MemoryRoom,
    score: u16,
    reason: &str,
    related_room_ids: &mut BTreeMap<String, (u16, Vec<String>)>,
) {
    let cluster_bonus = explicit_activity_score(room) / 2;
    for relation in &room.relations {
        if !relation.target.starts_with("room.") {
            continue;
        }
        let entry = related_room_ids
            .entry(relation.target.clone())
            .or_insert_with(|| (0, Vec::new()));
        entry.0 = entry.0.max(score.saturating_add(cluster_bonus).min(320));
        if !entry.1.iter().any(|existing| existing == reason) {
            entry.1.push(reason.to_owned());
        }
        if cluster_bonus > 0 && !entry.1.iter().any(|existing| existing == "active-cluster") {
            entry.1.push("active-cluster".to_owned());
        }
    }
}

fn read_room_by_id(
    store: &WorkspaceStore,
    repository: &MemoryRoomRepository,
    namespace: &WorkspaceNamespace,
    markdown_index: &MarkdownIndex,
    room_id: &str,
) -> Result<Option<(hc_memory::MemoryRoom, SystemTime)>> {
    let query = MarkdownQuery::default()
        .with_path_prefix("memory/rooms")
        .with_doc_type("memory_room")
        .with_id(room_id.to_owned())
        .with_limit(1);
    let Some(entry) = query_markdown_index(markdown_index, &query)
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let room = repository.read_room(&entry.relative_path)?;
    let modified_at = modified_time_for_relative_path(store, namespace, &entry.relative_path);
    Ok(Some((room, modified_at)))
}

fn modified_time_for_relative_path(
    store: &WorkspaceStore,
    namespace: &WorkspaceNamespace,
    relative_path: impl AsRef<Path>,
) -> SystemTime {
    store
        .resolve_in_namespace(namespace, relative_path)
        .metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn base_room_score(room: &hc_memory::MemoryRoom) -> u16 {
    match room.layer {
        MemoryLayer::Chat => 620,
        MemoryLayer::Task => 700,
        MemoryLayer::Topic => 680,
        MemoryLayer::Project => 640,
        MemoryLayer::Global => 560,
    }
}

fn build_room_candidate(
    room: &hc_memory::MemoryRoom,
    query: &ContextMemoryQuery,
    modified_at: SystemTime,
    extra_score: u16,
    mut reasons: Vec<String>,
) -> RoomCandidate {
    let mut score = base_room_score(room).saturating_add(extra_score);
    reasons.push(format!("layer={:?}", room.layer).to_ascii_lowercase());

    if room.status == "active" {
        score += 180;
        reasons.push("active-room".to_owned());
    }

    let activity_bonus = explicit_activity_score(room);
    if activity_bonus > 0 {
        score = score.saturating_add(activity_bonus);
        reasons.push(format!("recent-hit+{activity_bonus}"));
    }

    let recency_bonus = recency_score(modified_at);
    if recency_bonus > 0 {
        score = score.saturating_add(recency_bonus);
        reasons.push(format!("recent+{recency_bonus}"));
    }

    if let Some(scope) = &query.memory_query.scope
        && *scope == memory_scope_for_layer(&room.layer)
    {
        score += 140;
        reasons.push("scope-match".to_owned());
    }

    if let Some(tag) = &query.memory_query.tag
        && room.tags.iter().any(|candidate| candidate == tag)
    {
        score += 160;
        reasons.push(format!("tag={tag}"));
    }

    if let Some(text) = &query.memory_query.text {
        let lowered = text.to_ascii_lowercase();
        let haystack =
            format!("{} {} {}", room.title, room.summary, room.tags.join(" ")).to_ascii_lowercase();
        if haystack.contains(&lowered) {
            score += 260;
            reasons.push("text-match".to_owned());
        }

        if let Some(kind) = room_kind_hint(room) {
            if room_kind_matches_query(kind, &lowered) {
                score += 220;
                reasons.push(format!("room-kind={kind}"));
            }
        }
    }

    RoomCandidate {
        room_id: room.id.clone(),
        layer: room.layer.clone(),
        status: room.status.clone(),
        title: room.title.clone(),
        summary: room.summary.clone(),
        tags: room.tags.clone(),
        score_milli: score.min(1000),
        reasons,
    }
}

fn recency_score(modified_at: SystemTime) -> u16 {
    let Ok(elapsed) = SystemTime::now().duration_since(modified_at) else {
        return 0;
    };

    let hours = elapsed.as_secs() / 3600;
    if hours <= 6 {
        220
    } else if hours <= 24 {
        160
    } else if hours <= 24 * 3 {
        100
    } else if hours <= 24 * 7 {
        50
    } else {
        0
    }
}

fn explicit_activity_score(room: &hc_memory::MemoryRoom) -> u16 {
    let Some(timestamp) = room
        .derived_docs
        .iter()
        .rev()
        .find_map(|item| item.strip_prefix("last-active:"))
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return 0;
    };

    let activity_time = UNIX_EPOCH + std::time::Duration::from_secs(timestamp);
    recency_score(activity_time).saturating_add(80).min(300)
}

fn room_kind_hint(room: &hc_memory::MemoryRoom) -> Option<&'static str> {
    for tag in &room.tags {
        match tag.as_str() {
            "agent" => return Some("agent"),
            "tool" => return Some("tool"),
            "project" => return Some("project"),
            "task" => return Some("task"),
            "topic" => return Some("topic"),
            _ => {}
        }
    }

    if room.id.starts_with("room.agent.") {
        Some("agent")
    } else if room.id.starts_with("room.tool.") {
        Some("tool")
    } else if room.id.starts_with("room.project.") {
        Some("project")
    } else if room.id.starts_with("room.task.") {
        Some("task")
    } else if room.id.starts_with("room.topic.") {
        Some("topic")
    } else {
        None
    }
}

fn room_kind_matches_query(kind: &str, lowered_query: &str) -> bool {
    let keywords: &[&str] = match kind {
        "agent" => &[
            "agent",
            "persona",
            "reviewer",
            "planner",
            "coder",
            "助手",
            "智能体",
            "人格",
            "角色",
        ],
        "tool" => &[
            "tool", "api", "git", "cargo", "minimax", "openai", "工具", "命令", "接口", "sdk",
        ],
        "project" => &[
            "project",
            "architecture",
            "convention",
            "workspace",
            "repo",
            "项目",
            "架构",
            "约定",
            "仓库",
        ],
        "task" => &[
            "implement",
            "fix",
            "debug",
            "refactor",
            "test",
            "review",
            "实现",
            "修复",
            "调试",
            "重构",
            "测试",
            "任务",
        ],
        "topic" => &[
            "topic",
            "concept",
            "knowledge",
            "reference",
            "话题",
            "主题",
            "知识",
        ],
        _ => &[],
    };

    keywords
        .iter()
        .any(|keyword| lowered_query.contains(keyword))
}

impl MemoryRoomRouter for RuleBasedMemoryRoomRouter {
    fn route_room(&self, input: &MemoryOrganizationInput) -> Result<MemoryRoomRoute> {
        let (room_id, room_layer) = if let (Some(room_id), Some(room_layer)) =
            (&input.room_id_hint, &input.room_layer_hint)
        {
            (room_id.clone(), room_layer.clone())
        } else if let Some(owner) = &input.owner {
            (
                default_room_id_for_owner(owner),
                default_layer_for_owner_kind(&owner.kind),
            )
        } else {
            (
                format!(
                    "room.global.{}.{}",
                    slugify_for_memory(&input.namespace.tenant_id),
                    slugify_for_memory(&input.namespace.user_id)
                ),
                MemoryLayer::Global,
            )
        };

        Ok(MemoryRoomRoute {
            title: input
                .title_hint
                .clone()
                .unwrap_or_else(|| summarize_title_from_content(&input.content)),
            room_id,
            room_layer,
            owners: input.owner.clone().into_iter().collect(),
            visibility: input.visibility.clone(),
        })
    }
}

impl MemoryKindResolver for RuleBasedMemoryKindResolver {
    fn resolve_kind(&self, input: &MemoryOrganizationInput) -> Result<MemoryKind> {
        let content = input.content.to_ascii_lowercase();
        let kind = if contains_any(&content, &["decide", "decision", "assigned", "assignment"]) {
            MemoryKind::Decision
        } else if contains_any(&content, &["prefer", "preference", "style", "habit"]) {
            MemoryKind::Preference
        } else if contains_any(&content, &["workflow", "process", "steps", "procedure"]) {
            MemoryKind::WorkflowMemory
        } else if contains_any(
            &content,
            &["fact", "knowledge", "reference", "architecture"],
        ) {
            MemoryKind::Knowledge
        } else {
            MemoryKind::Summary
        };
        Ok(kind)
    }
}

impl MemoryTagSuggester for KeywordMemoryTagSuggester {
    fn suggest_tags(&self, input: &MemoryOrganizationInput) -> Result<Vec<String>> {
        let content = input.content.to_ascii_lowercase();
        let mut tags = BTreeSet::new();
        for (keyword, tag) in [
            ("runtime", "runtime"),
            ("assignment", "assignment"),
            ("planning", "planning"),
            ("memory", "memory"),
            ("stream", "streaming"),
            ("review", "review"),
            ("trace", "trace"),
        ] {
            if content.contains(keyword) {
                tags.insert(tag.to_owned());
            }
        }

        for tag in infer_semantic_tags(input) {
            tags.insert(tag);
        }

        Ok(tags.into_iter().collect())
    }
}

fn infer_semantic_tags(input: &MemoryOrganizationInput) -> Vec<String> {
    let mut tags = BTreeSet::new();
    let title = input.title_hint.as_deref().unwrap_or_default();
    let combined = format!("{title}\n{}", input.content);
    let lowered = combined.to_ascii_lowercase();

    if let Some(owner) = &input.owner {
        match owner.kind {
            MemoryOwnerKind::Task => {
                tags.insert("task".to_owned());
                let slug = slugify_for_memory(&owner.id);
                if !slug.is_empty() {
                    tags.insert(slug);
                }
            }
            MemoryOwnerKind::Project => {
                tags.insert("project".to_owned());
                let slug = slugify_for_memory(&owner.id);
                if !slug.is_empty() {
                    tags.insert(slug);
                }
            }
            _ => {}
        }
    }

    if let Some(agent_slug) = infer_agent_slug(&lowered, title) {
        tags.insert("agent".to_owned());
        tags.insert(agent_slug);
    }

    if let Some(tool_slug) = infer_tool_slug(&lowered, title) {
        tags.insert("tool".to_owned());
        tags.insert(tool_slug);
    }

    if let Some(project_slug) = infer_project_slug(&lowered, title, input.owner.as_ref()) {
        tags.insert("project".to_owned());
        tags.insert(project_slug);
    }

    if let Some(task_slug) = infer_task_slug(&lowered, title, input.owner.as_ref()) {
        tags.insert("task".to_owned());
        tags.insert(task_slug);
    }

    if let Some(topic_slug) = infer_topic_slug(&lowered, title) {
        tags.insert("topic".to_owned());
        tags.insert(topic_slug);
    }

    tags.into_iter().collect()
}

fn infer_agent_slug(lowered: &str, title: &str) -> Option<String> {
    for keyword in ["reviewer", "planner", "coder", "assistant"] {
        if lowered.contains(keyword) {
            return Some(keyword.to_owned());
        }
    }

    if !contains_any(
        lowered,
        &["agent", "persona", "助手", "智能体", "角色", "人格"],
    ) {
        return None;
    }

    semantic_slug_from_title(title, &["agent", "persona", "assistant", "habit", "style"])
}

fn infer_tool_slug(lowered: &str, title: &str) -> Option<String> {
    for (keyword, slug) in [
        ("ripgrep", "rg"),
        (" rg ", "rg"),
        ("cargo", "cargo"),
        ("git", "git"),
        ("minimax", "minimax"),
        ("openai", "openai"),
        ("bash", "bash"),
    ] {
        if lowered.contains(keyword) {
            return Some(slug.to_owned());
        }
    }

    if !contains_any(lowered, &["tool", "工具", "命令", "api", "sdk"]) {
        return None;
    }

    semantic_slug_from_title(title, &["tool", "workflow", "guide", "style"])
}

fn infer_project_slug(
    lowered: &str,
    title: &str,
    owner: Option<&MemoryOwnerRef>,
) -> Option<String> {
    if let Some(owner) = owner
        && owner.kind == MemoryOwnerKind::Project
    {
        let slug = slugify_for_memory(&owner.id);
        if !slug.is_empty() {
            return Some(slug);
        }
    }

    if !contains_any(
        lowered,
        &["project", "repo", "repository", "workspace", "项目", "仓库"],
    ) {
        return None;
    }

    semantic_slug_from_title(
        title,
        &["project", "repo", "style", "guide", "architecture"],
    )
}

fn infer_task_slug(lowered: &str, title: &str, owner: Option<&MemoryOwnerRef>) -> Option<String> {
    if let Some(owner) = owner
        && owner.kind == MemoryOwnerKind::Task
    {
        let slug = slugify_for_memory(&owner.id);
        if !slug.is_empty() {
            return Some(slug);
        }
    }

    if !contains_any(
        lowered,
        &[
            "task",
            "implement",
            "fix",
            "debug",
            "refactor",
            "review",
            "任务",
            "实现",
            "修复",
            "重构",
        ],
    ) {
        return None;
    }

    semantic_slug_from_title(
        title,
        &["task", "guide", "rule", "workflow", "habit", "decision"],
    )
}

fn infer_topic_slug(lowered: &str, title: &str) -> Option<String> {
    if !contains_any(
        lowered,
        &[
            "topic",
            "concept",
            "knowledge",
            "reference",
            "theme",
            "主题",
            "话题",
            "知识",
        ],
    ) {
        return None;
    }

    semantic_slug_from_title(
        title,
        &[
            "topic",
            "concept",
            "knowledge",
            "reference",
            "guide",
            "style",
        ],
    )
}

fn semantic_slug_from_title(title: &str, stopwords: &[&str]) -> Option<String> {
    let mut tokens = Vec::new();

    for raw in title.split(|character: char| !character.is_ascii_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let lowered = raw.to_ascii_lowercase();
        if stopwords.iter().any(|stopword| *stopword == lowered) {
            continue;
        }
        tokens.push(lowered);
        if tokens.len() >= 2 {
            break;
        }
    }

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join("."))
    }
}

impl MemoryPromotionAdvisor for NoopMemoryPromotionAdvisor {
    fn suggest_promotions(
        &self,
        _input: &MemoryOrganizationInput,
        _route: &MemoryRoomRoute,
        _memory_kind: MemoryKind,
    ) -> Result<Vec<MemoryPromotionSuggestion>> {
        Ok(Vec::new())
    }
}

impl MemoryPromotionAdvisor for RuleBasedMemoryPromotionAdvisor {
    fn suggest_promotions(
        &self,
        input: &MemoryOrganizationInput,
        _route: &MemoryRoomRoute,
        _memory_kind: MemoryKind,
    ) -> Result<Vec<MemoryPromotionSuggestion>> {
        let content = input.content.trim();
        let lowered = content.to_ascii_lowercase();
        let global_room_id = format!(
            "room.global.{}.{}",
            slugify_for_memory(&input.namespace.tenant_id),
            slugify_for_memory(&input.namespace.user_id)
        );

        let mut promotions = Vec::new();
        if detect_assistant_name_preference(content).is_some() {
            promotions.push(MemoryPromotionSuggestion {
                target_layer: MemoryLayer::Global,
                target_room_id: Some(global_room_id.clone()),
                reason: "assistant naming preference should persist across chats".to_owned(),
            });
        }
        if contains_any(
            &lowered,
            &[
                "??????",
                "?????",
                "???",
                "respond in chinese",
                "answer in chinese",
            ],
        ) {
            promotions.push(MemoryPromotionSuggestion {
                target_layer: MemoryLayer::Global,
                target_room_id: Some(global_room_id.clone()),
                reason: "language preference should persist across chats".to_owned(),
            });
        }
        if contains_any(
            &lowered,
            &["????", "????", "????", "be concise", "shorter answers"],
        ) {
            promotions.push(MemoryPromotionSuggestion {
                target_layer: MemoryLayer::Global,
                target_room_id: Some(global_room_id),
                reason: "response style preference should persist across chats".to_owned(),
            });
        }

        Ok(promotions)
    }
}

impl PromptAssetSynthesizer for DefaultPromptAssetSynthesizer {
    fn synthesize(&self, memories: &[RetrievedMemory]) -> Result<Vec<PromptAsset>> {
        let compiled_assets = memories
            .iter()
            .filter_map(prompt_asset_from_compiled_memory)
            .collect::<Vec<_>>();
        if !compiled_assets.is_empty() {
            return Ok(compiled_assets);
        }

        let mut assets = Vec::new();
        for memory in memories {
            let is_global_preference = memory.kind == MemoryKind::Preference
                && matches!(memory.layer, Some(MemoryLayer::Global))
                || (memory.kind == MemoryKind::Preference && memory.scope == MemoryScope::Global);
            if !is_global_preference {
                continue;
            }

            let kind = infer_prompt_asset_kind_from_preference(memory);
            let title = match kind {
                PromptAssetKind::StyleGuide => format!("Style Preference | {}", memory.title),
                PromptAssetKind::BehaviorTemplate => {
                    format!("Behavior Preference | {}", memory.title)
                }
                _ => format!("Global Preference | {}", memory.title),
            };
            assets.push(prompt_asset_from_memory(memory, kind, title));
        }
        Ok(assets)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LlmPromptAssetOutput {
    #[serde(default)]
    assets: Vec<LlmPromptAssetItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LlmPromptAssetItem {
    #[serde(default)]
    source_memory_id: Option<String>,
    #[serde(default = "default_prompt_asset_kind")]
    kind: PromptAssetKind,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LlmMemoryOrganizationOutput {
    #[serde(default)]
    room_layer: Option<MemoryLayer>,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    memory_kind: Option<MemoryKind>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    promotions: Vec<MemoryPromotionSuggestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LlmGlobalPreferenceSummaryOutput {
    #[serde(default)]
    summary: String,
    #[serde(default = "default_preference_memory_kind")]
    memory_kind: MemoryKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LlmSemanticTagOutput {
    #[serde(default)]
    tags: Vec<String>,
}

impl<'a, F> PromptAssetSynthesizer for LlmPromptAssetSynthesizer<'a, F>
where
    F: PromptAssetSynthesizer,
{
    fn synthesize(&self, memories: &[RetrievedMemory]) -> Result<Vec<PromptAsset>> {
        if memories.is_empty() {
            return Ok(Vec::new());
        }

        match self.try_synthesize(memories) {
            Ok(assets) if !assets.is_empty() => Ok(assets),
            Ok(_) if self.fallback_on_error => self.fallback.synthesize(memories),
            Ok(assets) => Ok(assets),
            Err(error) if self.fallback_on_error => {
                self.fallback.synthesize(memories).or(Err(error))
            }
            Err(error) => Err(error),
        }
    }
}

impl<'a, F> MemoryTagSuggester for LlmMemoryTagSuggester<'a, F>
where
    F: MemoryTagSuggester,
{
    fn suggest_tags(&self, input: &MemoryOrganizationInput) -> Result<Vec<String>> {
        match self.try_suggest_tags(input) {
            Ok(tags) if !tags.is_empty() => Ok(tags),
            Ok(_) if self.fallback_on_error => self.fallback.suggest_tags(input),
            Ok(tags) => Ok(tags),
            Err(error) if self.fallback_on_error => {
                self.fallback.suggest_tags(input).or(Err(error))
            }
            Err(error) => Err(error),
        }
    }
}

impl<'a, F> LlmPromptAssetSynthesizer<'a, F>
where
    F: PromptAssetSynthesizer,
{
    fn try_synthesize(&self, memories: &[RetrievedMemory]) -> Result<Vec<PromptAsset>> {
        let instructions = load_managed_prompt_body(
            default_workspace_root(),
            &self.workspace_namespace,
            ManagedPromptKind::PromptAssetSynthesizer,
        )?;
        let system_prompt = load_managed_prompt_body(
            default_workspace_root(),
            &self.workspace_namespace,
            ManagedPromptKind::JsonSystemGuard,
        )?;
        let response = self
            .registry
            .generate(&GenerateRequest {
                model: self.model.clone(),
                messages: vec![
                    ChatMessage::new(MessageRole::System, system_prompt),
                    ChatMessage::new(
                        MessageRole::User,
                        format!(
                            "{}\n\nMemories JSON:\n{}",
                            instructions,
                            serde_json::to_string_pretty(memories)?
                        ),
                    ),
                ],
                temperature: Some(0.1),
                max_output_tokens: Some(800),
                metadata: Default::default(),
            })
            .map_err(anyhow::Error::from)?;
        let parsed: LlmPromptAssetOutput = parse_json_payload(&response.message.content)?;
        Ok(parsed
            .assets
            .into_iter()
            .enumerate()
            .filter(|(_, asset)| !asset.content.trim().is_empty())
            .map(|(index, asset)| prompt_asset_from_llm_item(memories, index, asset))
            .collect())
    }
}

impl<'a, F> LlmMemoryTagSuggester<'a, F>
where
    F: MemoryTagSuggester,
{
    fn try_suggest_tags(&self, input: &MemoryOrganizationInput) -> Result<Vec<String>> {
        let instructions = load_managed_prompt_body(
            default_workspace_root(),
            &self.workspace_namespace,
            ManagedPromptKind::SemanticTagSuggester,
        )?;
        let system_prompt = load_managed_prompt_body(
            default_workspace_root(),
            &self.workspace_namespace,
            ManagedPromptKind::JsonSystemGuard,
        )?;
        let response = self
            .registry
            .generate(&GenerateRequest {
                model: self.model.clone(),
                messages: vec![
                    ChatMessage::new(MessageRole::System, system_prompt),
                    ChatMessage::new(
                        MessageRole::User,
                        format!(
                            "{}\n\nInput JSON:\n{}",
                            instructions,
                            serde_json::to_string_pretty(input)?
                        ),
                    ),
                ],
                temperature: Some(0.1),
                max_output_tokens: Some(300),
                metadata: Default::default(),
            })
            .map_err(anyhow::Error::from)?;
        let parsed: LlmSemanticTagOutput = parse_json_payload(&response.message.content)?;
        let mut tags = BTreeSet::new();
        for tag in parsed.tags {
            let slug = slugify_for_memory(&tag);
            if !slug.is_empty() {
                tags.insert(slug);
            }
        }
        Ok(tags.into_iter().collect())
    }
}

impl<'a, F> MemoryOrganizer for LlmMemoryOrganizer<'a, F>
where
    F: MemoryOrganizer,
{
    fn organize(&self, input: &MemoryOrganizationInput) -> Result<MemoryOrganizationDecision> {
        match self.try_organize(input) {
            Ok(decision) => Ok(decision),
            Err(error) if self.fallback_on_error => self.fallback.organize(input).or(Err(error)),
            Err(error) => Err(error),
        }
    }
}

impl<'a, F> LlmMemoryOrganizer<'a, F>
where
    F: MemoryOrganizer,
{
    fn try_organize(&self, input: &MemoryOrganizationInput) -> Result<MemoryOrganizationDecision> {
        let instructions = load_managed_prompt_body(
            default_workspace_root(),
            &self.workspace_namespace,
            ManagedPromptKind::MemoryOrganizer,
        )?;
        let system_prompt = load_managed_prompt_body(
            default_workspace_root(),
            &self.workspace_namespace,
            ManagedPromptKind::JsonSystemGuard,
        )?;
        let response = self
            .registry
            .generate(&GenerateRequest {
                model: self.model.clone(),
                messages: vec![
                    ChatMessage::new(MessageRole::System, system_prompt),
                    ChatMessage::new(
                        MessageRole::User,
                        format!(
                            "{}\n\nInput JSON:\n{}",
                            instructions,
                            serde_json::to_string_pretty(input)?
                        ),
                    ),
                ],
                temperature: Some(0.1),
                max_output_tokens: Some(900),
                metadata: Default::default(),
            })
            .map_err(anyhow::Error::from)?;
        let parsed: LlmMemoryOrganizationOutput = parse_json_payload(&response.message.content)?;
        let fallback = if self.fallback_on_error {
            self.fallback.organize(input)?
        } else {
            base_memory_decision_from_input(input)
        };
        Ok(memory_decision_from_llm_output(input, fallback, parsed))
    }
}

#[derive(Debug, Clone, Default)]
pub struct DefaultContextComposer;

impl ContextComposer for DefaultContextComposer {
    fn compose_messages(
        &self,
        system_prompt: Option<&str>,
        self_model: Option<&SelfModel>,
        prompt_policies: &[PromptPolicy],
        prompt_assets: &[PromptAsset],
        memories: &[RetrievedMemory],
        user_messages: &[ChatMessage],
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        let mut system_sections = Vec::new();
        if let Some(system_prompt) = system_prompt
            && !system_prompt.trim().is_empty()
        {
            system_sections.push(system_prompt.trim().to_owned());
        }

        if let Some(self_model) = self_model {
            system_sections.push(render_self_model_section(self_model));
        }

        if !prompt_policies.is_empty() {
            let rendered = prompt_policies
                .iter()
                .map(|policy| format!("[{}]\n{}", policy.title, policy.content))
                .collect::<Vec<_>>()
                .join("\n\n");
            system_sections.push(format!("Prompt policies:\n{rendered}"));
        }

        if !prompt_assets.is_empty() {
            let rendered = prompt_assets
                .iter()
                .map(|asset| {
                    format!(
                        "- {} | kind={:?} | {}",
                        asset.title, asset.kind, asset.content
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            system_sections.push(format!("Prompt assets:\n{rendered}"));
        }

        if !memories.is_empty() {
            let recalled = memories
                .iter()
                .map(|memory| {
                    format!(
                        "- {} | kind={:?} | scope={:?} | source={} | confidence={} | {}",
                        memory.title,
                        memory.kind,
                        memory.scope,
                        memory.source_kind,
                        memory.confidence_milli,
                        memory.summary
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            system_sections.push(format!("Relevant recalled memory:\n{}", recalled));
        }

        if !system_sections.is_empty() {
            messages.push(ChatMessage::new(
                MessageRole::System,
                system_sections.join("\n\n"),
            ));
        }

        messages.extend(
            user_messages
                .iter()
                .filter(|message| message.role != MessageRole::System)
                .cloned(),
        );

        messages
    }
}

pub fn generate_with_context(
    registry: &ProviderRegistry,
    retriever: &impl MemoryRetriever,
    composer: &impl ContextComposer,
    request: &ContextRequest,
) -> Result<ContextResponse> {
    let workspace_namespace = request
        .memory_query
        .memory_query
        .namespace
        .as_ref()
        .map(workspace_namespace_from_memory_namespace)
        .unwrap_or_else(WorkspaceNamespace::local_default);
    let synthesizer = LlmPromptAssetSynthesizer::new(
        registry,
        request.generation.model.clone(),
        workspace_namespace,
        DefaultPromptAssetSynthesizer,
    );
    generate_with_context_using_synthesizer(registry, retriever, composer, &synthesizer, request)
}

pub fn generate_with_context_using_synthesizer(
    registry: &ProviderRegistry,
    retriever: &impl MemoryRetriever,
    composer: &impl ContextComposer,
    synthesizer: &(impl PromptAssetSynthesizer + ?Sized),
    request: &ContextRequest,
) -> Result<ContextResponse> {
    let memories = retriever.retrieve(&request.memory_query)?;
    let recalled_assets = asset_views_from_retrieved_memories(&memories);
    let compiled_prompt_assets = compiled_prompt_assets_from_asset_views(&recalled_assets);
    let synthesized_prompt_assets = synthesizer.synthesize(&memories)?;
    let prompt_assets = merged_prompt_assets(
        &merged_prompt_assets(&request.prompt_assets, &compiled_prompt_assets),
        &synthesized_prompt_assets,
    );
    let messages = composer.compose_messages(
        request.system_prompt.as_deref(),
        request.self_model.as_ref(),
        &request.prompt_policies,
        &prompt_assets,
        &memories,
        &request.generation.messages,
    );
    let mut generation = request.generation.clone();
    generation.messages = messages;

    let response = registry
        .generate(&generation)
        .map_err(anyhow::Error::from)?;

    Ok(ContextResponse {
        response,
        recalled_memories: memories,
        synthesized_prompt_assets,
    })
}

pub fn generate_with_context_stream(
    registry: &ProviderRegistry,
    retriever: &impl MemoryRetriever,
    composer: &impl ContextComposer,
    request: &ContextRequest,
    on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
) -> Result<ContextResponse> {
    let workspace_namespace = request
        .memory_query
        .memory_query
        .namespace
        .as_ref()
        .map(workspace_namespace_from_memory_namespace)
        .unwrap_or_else(WorkspaceNamespace::local_default);
    let synthesizer = LlmPromptAssetSynthesizer::new(
        registry,
        request.generation.model.clone(),
        workspace_namespace,
        DefaultPromptAssetSynthesizer,
    );
    generate_with_context_stream_using_synthesizer(
        registry,
        retriever,
        composer,
        &synthesizer,
        request,
        on_chunk,
    )
}

pub fn generate_with_context_stream_using_synthesizer(
    registry: &ProviderRegistry,
    retriever: &impl MemoryRetriever,
    composer: &impl ContextComposer,
    synthesizer: &(impl PromptAssetSynthesizer + ?Sized),
    request: &ContextRequest,
    on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
) -> Result<ContextResponse> {
    let memories = retriever.retrieve(&request.memory_query)?;
    let recalled_assets = asset_views_from_retrieved_memories(&memories);
    let compiled_prompt_assets = compiled_prompt_assets_from_asset_views(&recalled_assets);
    let synthesized_prompt_assets = synthesizer.synthesize(&memories)?;
    let prompt_assets = merged_prompt_assets(
        &merged_prompt_assets(&request.prompt_assets, &compiled_prompt_assets),
        &synthesized_prompt_assets,
    );
    let messages = composer.compose_messages(
        request.system_prompt.as_deref(),
        request.self_model.as_ref(),
        &request.prompt_policies,
        &prompt_assets,
        &memories,
        &request.generation.messages,
    );
    let mut generation = request.generation.clone();
    generation.messages = messages;

    let response = registry
        .generate_stream(&generation, on_chunk)
        .map_err(anyhow::Error::from)?;

    Ok(ContextResponse {
        response,
        recalled_memories: memories,
        synthesized_prompt_assets,
    })
}

pub fn workspace_namespace_from_memory_namespace(
    namespace: &MemoryNamespace,
) -> WorkspaceNamespace {
    WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

pub fn default_workspace_root() -> &'static Path {
    static WORKSPACE_ROOT: OnceLock<PathBuf> = OnceLock::new();
    WORKSPACE_ROOT
        .get_or_init(hc_bootstrap::workspace_root)
        .as_path()
}

pub fn load_assistant_wenyan_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::AssistantWenyanRewrite,
    )
}

pub fn load_tool_chat_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::ToolChatAssistant,
    )
}

pub fn load_tool_router_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::ToolRouter,
    )
}

pub fn load_tool_natural_language_builder_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::ToolNaturalLanguageBuilder,
    )
}

pub fn load_agent_responder_system_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::AgentResponderSystem,
    )
}

pub fn load_agent_planner_input_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::AgentPlannerInput,
    )
}

pub fn load_agent_work_item_execution_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::AgentWorkItemExecution,
    )
}

pub fn load_context_memory_system_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::ContextMemorySystem,
    )
}

pub fn load_context_memory_usage_policy_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::ContextMemoryUsagePolicy,
    )
}

pub fn load_context_lightweight_chat_prompt(namespace: &WorkspaceNamespace) -> Result<String> {
    load_managed_prompt_body(
        default_workspace_root(),
        namespace,
        ManagedPromptKind::ContextLightweightChat,
    )
}

pub fn persist_room_memory(
    root: impl Into<PathBuf>,
    namespace: WorkspaceNamespace,
    request: &RoomMemoryWriteRequest,
) -> Result<PathBuf> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let room = hc_memory::MemoryRoom::new(
        request.room_id.clone(),
        request.room_layer.clone(),
        request.title.clone(),
        request.summary.clone(),
    );

    let asset_id = request
        .asset_id
        .clone()
        .unwrap_or_else(|| default_room_asset_id(request));
    let file_name = request
        .file_name
        .clone()
        .unwrap_or_else(|| default_room_asset_file_name(request));
    let mut draft = ArtifactDraft::new(
        request.room_id.clone(),
        request.room_layer.clone(),
        MemoryRoomAssetKind::Compressed,
        request.title.clone(),
        request.summary.clone(),
    )
    .with_visibility(request.visibility.clone())
    .with_memory_kind(request.memory_kind.clone())
    .with_file_name(file_name);

    for owner in &request.owners {
        draft = draft.with_owner(owner.clone());
    }
    for tag in &request.tags {
        draft = draft.with_tag(tag.clone());
    }
    for source in &request.derived_from {
        draft = draft.with_derived_from(source.clone());
    }
    for source_doc in &request.source_docs {
        draft = draft.with_source_doc(source_doc.clone());
    }

    repository.write_artifact_draft(&room, asset_id, draft)
}

pub fn persist_synthesized_prompt_assets(
    root: impl Into<PathBuf>,
    namespace: WorkspaceNamespace,
    response: &ContextResponse,
) -> Result<Vec<PathBuf>> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root.clone(), namespace.clone());
    let mut paths = Vec::new();

    for prompt_asset in &response.synthesized_prompt_assets {
        let Some(source_memory) = response
            .recalled_memories
            .iter()
            .find(|memory| memory.id == prompt_asset.id)
        else {
            continue;
        };
        let Some((room_id, room_layer)) = prompt_asset_target_room(
            source_memory,
            &MemoryNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()),
        ) else {
            continue;
        };

        let slug = slugify_for_memory(&prompt_asset.title);
        let asset_id = format!("asset.{room_id}.prompt.{slug}");
        let mut draft = ArtifactDraft::new(
            room_id.clone(),
            room_layer.clone(),
            MemoryRoomAssetKind::Compressed,
            prompt_asset.title.clone(),
            prompt_asset.content.clone(),
        )
        .with_visibility(hc_memory::MemoryVisibility::Private)
        .with_memory_kind(source_memory.kind.clone())
        .with_stage(prompt_asset.stage.clone())
        .with_form(prompt_asset.form.clone())
        .with_tag("prompt")
        .with_tag(format!("{:?}", prompt_asset.kind).to_ascii_lowercase())
        .with_file_name(format!("prompt.{slug}.md"));

        if let Some(source_room_id) = &source_memory.room_id {
            draft = draft.with_owner(MemoryOwnerRef::new(
                owner_kind_for_layer(&room_layer),
                source_room_id.clone(),
            ));
        }
        draft = draft.with_derived_from(source_memory.id.clone());

        for tag in &prompt_asset.tags {
            draft = draft.with_tag(tag.clone());
        }

        let room = MemoryRoom::new(
            room_id.clone(),
            room_layer,
            format!("Prompt Memory | {}", prompt_asset.title),
            prompt_asset.content.clone(),
        );
        let materialized = repository.materialize_artifact_draft(&room, asset_id.clone(), draft)?;
        persist_room_evolution_event(
            &repository,
            &room,
            ArtifactEvolutionEvent::new(
                format!("event.{asset_id}.compiled.{}", current_unix_timestamp_ms()),
                asset_id,
                room_id.clone(),
                ArtifactEvolutionAction::Promoted,
                "compiled recalled memory into prompt asset",
            ),
            vec![
                "prompt".to_owned(),
                format!("{:?}", prompt_asset.kind).to_ascii_lowercase(),
            ],
            vec![source_memory.id.clone()],
            vec![materialized.room_relative_path.clone()],
        )?;
        paths.push(materialized.path);
    }

    Ok(paths)
}

pub fn room_memory_write_request_from_response(
    room_id: impl Into<String>,
    room_layer: MemoryLayer,
    title: impl Into<String>,
    memory_kind: MemoryKind,
    response: &ContextResponse,
) -> RoomMemoryWriteRequest {
    let mut request = RoomMemoryWriteRequest::new(
        room_id,
        room_layer,
        title,
        response.response.message.content.trim().to_owned(),
        memory_kind,
    );
    for memory in &response.recalled_memories {
        request = request.with_derived_from(memory.id.clone());
    }
    request
}

pub fn room_memory_write_request_from_organization(
    decision: &MemoryOrganizationDecision,
    summary: impl Into<String>,
) -> RoomMemoryWriteRequest {
    let mut request = RoomMemoryWriteRequest::new(
        decision.route.room_id.clone(),
        decision.route.room_layer.clone(),
        decision.route.title.clone(),
        summary,
        decision.memory_kind.clone(),
    )
    .with_visibility(decision.route.visibility.clone());

    for owner in &decision.route.owners {
        request = request.with_owner(owner.clone());
    }
    for tag in &decision.tags {
        request = request.with_tag(tag.clone());
    }

    request
}

pub fn prompt_asset_from_memory(
    memory: &RetrievedMemory,
    kind: PromptAssetKind,
    title: impl Into<String>,
) -> PromptAsset {
    let mut asset = PromptAsset::new(memory.id.clone(), kind, title, memory.summary.clone());
    for tag in &memory.tags {
        asset = asset.with_tag(tag.clone());
    }
    asset
}

pub fn prompt_asset_from_compiled_memory(memory: &RetrievedMemory) -> Option<PromptAsset> {
    if !memory.tags.iter().any(|tag| tag == "prompt") {
        return None;
    }

    let kind = infer_prompt_asset_kind_from_compiled_memory(memory)?;
    let mut asset = PromptAsset::new(
        memory.id.clone(),
        kind,
        memory.title.clone(),
        memory.summary.clone(),
    );
    for tag in &memory.tags {
        asset = asset.with_tag(tag.clone());
    }
    Some(asset)
}

pub fn prompt_asset_from_asset_view(asset: &AssetView) -> Option<PromptAsset> {
    if asset.status == AssetStatus::Retired
        || !asset
            .consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Llm)
        || asset.form != MemoryAssetForm::Prompt
    {
        return None;
    }

    let kind = infer_prompt_asset_kind_from_asset_view(asset)?;
    let mut prompt_asset = PromptAsset::new(
        asset.id.clone(),
        kind,
        asset.title.clone(),
        asset.content.clone(),
    );
    for tag in &asset.tags {
        prompt_asset = prompt_asset.with_tag(tag.clone());
    }
    Some(prompt_asset)
}

pub fn compiled_prompt_assets_from_asset_views(assets: &[AssetView]) -> Vec<PromptAsset> {
    assets
        .iter()
        .filter_map(prompt_asset_from_asset_view)
        .collect()
}

pub fn asset_view_from_memory_record(record: &MemoryRecord) -> AssetView {
    let form = default_asset_form_for_memory_kind(&record.kind);
    AssetView {
        id: record.id.clone(),
        title: record.title.clone(),
        summary: record.summary.clone(),
        content: record.summary.clone(),
        kind: record.kind.clone(),
        stage: MemoryAssetStage::Extracted,
        form: form.clone(),
        target: infer_asset_target_from_memory_record(record),
        target_ref: infer_asset_target_ref_from_memory_record(record),
        consumers: infer_asset_consumers(form, &record.tags),
        status: infer_asset_status(&record.tags),
        visibility: record.visibility.clone(),
        tags: record.tags.clone(),
        owners: vec![record.owner.clone()],
        derived_from: record.derived_from.clone(),
        source_docs: Vec::new(),
    }
}

pub fn asset_view_from_room_asset(asset: &MemoryRoomAsset) -> AssetView {
    let form = if asset.tags.iter().any(|tag| tag == "validation") {
        MemoryAssetForm::Policy
    } else {
        asset.form.clone()
    };
    AssetView {
        id: asset.id.clone(),
        title: asset.title.clone(),
        summary: asset.summary.clone(),
        content: asset.summary.clone(),
        kind: asset.memory_kind.clone(),
        stage: asset.stage.clone(),
        form: form.clone(),
        target: infer_asset_target_from_room_asset(asset),
        target_ref: infer_asset_target_ref_from_room_asset(asset),
        consumers: infer_asset_consumers(form, &asset.tags),
        status: infer_asset_status(&asset.tags),
        visibility: asset.visibility.clone(),
        tags: asset.tags.clone(),
        owners: asset.owners.clone(),
        derived_from: asset.derived_from.clone(),
        source_docs: asset.source_docs.clone(),
    }
}

pub fn asset_view_from_retrieved_memory(memory: &RetrievedMemory) -> AssetView {
    let form = if memory.tags.iter().any(|tag| tag == "prompt") {
        MemoryAssetForm::Prompt
    } else if memory.tags.iter().any(|tag| tag == "validation") {
        MemoryAssetForm::Policy
    } else {
        default_asset_form_for_memory_kind(&memory.kind)
    };
    AssetView {
        id: memory.id.clone(),
        title: memory.title.clone(),
        summary: memory.summary.clone(),
        content: memory.summary.clone(),
        kind: memory.kind.clone(),
        stage: inferred_stage_from_retrieved_memory(memory),
        form: form.clone(),
        target: infer_asset_target_from_retrieved_memory(memory),
        target_ref: memory.room_id.clone(),
        consumers: infer_asset_consumers(form, &memory.tags),
        status: infer_asset_status(&memory.tags),
        visibility: hc_memory::MemoryVisibility::Private,
        tags: memory.tags.clone(),
        owners: memory
            .room_id
            .as_ref()
            .zip(memory.layer.as_ref())
            .map(|(room_id, layer)| {
                vec![MemoryOwnerRef::new(
                    owner_kind_for_layer(layer),
                    room_id.clone(),
                )]
            })
            .unwrap_or_default(),
        derived_from: memory.derived_from.clone(),
        source_docs: Vec::new(),
    }
}

pub fn asset_views_from_retrieved_memories(memories: &[RetrievedMemory]) -> Vec<AssetView> {
    memories
        .iter()
        .map(asset_view_from_retrieved_memory)
        .collect()
}

pub fn tool_memory_query(
    namespace: MemoryNamespace,
    tool: &ToolSpec,
    _goal: impl Into<String>,
) -> ContextMemoryQuery {
    let tool_slug = tool.id.trim_start_matches("tool.");
    ContextMemoryQuery::default()
        .for_namespace(namespace)
        .with_room_anchor(tool_room_id(tool))
        .with_limit(12)
        .with_tag(tool_slug)
}

pub fn load_tool_assets(
    retriever: &impl MemoryRetriever,
    namespace: MemoryNamespace,
    tool: &ToolSpec,
) -> Result<Vec<AssetView>> {
    let query = tool_memory_query(namespace, tool, tool.name.clone());
    let mut memories = retriever.retrieve(&query)?;
    let room_memories = read_tool_room_memories(default_workspace_root(), &query, tool)?;
    merge_retrieved_memories(&mut memories, room_memories);
    let mut assets = asset_views_from_retrieved_memories(&memories);
    let superseded = superseded_tool_asset_ids(&assets);
    assets.retain(|asset| {
        !superseded.contains(&asset.id)
            || asset.tags.iter().any(|tag| {
                matches!(
                    tag.as_str(),
                    "compiled" | "promotion" | "revision" | "retired"
                )
            })
    });
    assets.sort_by_key(tool_asset_priority);
    assets.reverse();
    Ok(assets)
}

pub fn build_tool_execution_plan_from_assets(
    binder: &impl ToolExecutionBinder,
    goal: impl Into<String>,
    tool: &ToolSpec,
    assets: &[AssetView],
) -> Result<ToolExecutionPlan> {
    binder.bind(&goal.into(), tool, assets)
}

pub fn build_tool_execution_plan(
    retriever: &impl MemoryRetriever,
    binder: &impl ToolExecutionBinder,
    namespace: MemoryNamespace,
    goal: impl Into<String>,
    tool: &ToolSpec,
) -> Result<ToolExecutionPlan> {
    let goal = goal.into();
    let assets = load_tool_assets(retriever, namespace, tool)?;
    build_tool_execution_plan_from_assets(binder, goal, tool, &assets)
}

pub fn export_tool_capability_package(
    output_dir: impl AsRef<Path>,
    tool: &ToolSpec,
    assets: &[AssetView],
) -> Result<ToolCapabilityExportPackage> {
    let clean_assets = clean_tool_export_assets(tool, assets);
    let plan = build_tool_execution_plan_from_assets(
        &DefaultToolExecutionBinder,
        format!("use {}", tool.name),
        tool,
        &clean_assets,
    )?;
    let output_dir = output_dir.as_ref();
    let portable_dir = output_dir.join("portable");
    let runnable_dir = output_dir.join("runnable");
    let assets_dir = portable_dir.join("assets");
    fs::create_dir_all(&assets_dir)?;
    fs::create_dir_all(&runnable_dir)?;

    let manifest_assets = clean_assets
        .iter()
        .map(|asset| {
            let role = tool_export_asset_role(asset);
            let file_name = format!("{}.{}.md", role, slugify_for_memory(&asset.title));
            let file = format!("assets/{file_name}");
            let clean_tags = clean_export_tags(&asset.tags);
            fs::write(
                assets_dir.join(&file_name),
                render_export_asset_markdown(asset, &role, &clean_tags),
            )?;
            Ok(ToolCapabilityExportAsset {
                id: asset.id.clone(),
                role,
                title: asset.title.clone(),
                file,
                kind: asset.kind.clone(),
                stage: asset.stage.clone(),
                form: asset.form.clone(),
                tags: clean_tags,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let package_id = format!("capability.{}", tool.id.trim_start_matches("tool."));
    let manifest = ToolCapabilityExportManifest {
        schema_version: 1,
        package_id: package_id.clone(),
        tool: tool.clone(),
        command: plan.suggested_command.clone(),
        assets: manifest_assets,
    };
    let package = ToolCapabilityExportPackage { manifest, plan };
    fs::write(
        portable_dir.join("manifest.json"),
        serde_json::to_string_pretty(&package.manifest)?,
    )?;
    fs::write(
        portable_dir.join("plan.json"),
        serde_json::to_string_pretty(&package.plan)?,
    )?;
    fs::write(
        portable_dir.join("README.md"),
        render_portable_capability_readme(&package),
    )?;
    fs::write(
        runnable_dir.join("tool.json"),
        serde_json::to_string_pretty(tool)?,
    )?;
    fs::write(
        runnable_dir.join("plan.json"),
        serde_json::to_string_pretty(&package.plan)?,
    )?;
    fs::write(
        runnable_dir.join("README.md"),
        render_runnable_capability_readme(&package),
    )?;
    let run_script = runnable_dir.join("run.sh");
    fs::write(&run_script, render_run_script(&package.plan))?;
    make_executable(&run_script)?;
    fs::write(
        output_dir.join("package.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": 1,
            "package_id": package_id,
            "layers": {
                "portable": {
                    "path": "portable",
                    "manifest": "portable/manifest.json"
                },
                "runnable": {
                    "path": "runnable",
                    "entrypoint": "runnable/run.sh"
                }
            }
        }))?,
    )?;
    fs::write(
        output_dir.join("README.md"),
        render_layered_capability_readme(&package),
    )?;
    Ok(package)
}

pub fn evaluate_tool_execution(
    tool: &ToolSpec,
    plan: &ToolExecutionPlan,
    outcome: &ToolExecutionOutcome,
    assets: &[AssetView],
    generalization_policy: &GeneralizationPolicy,
    promotion_rule: &PromotionRule,
    retirement_rule: &RetirementRule,
) -> ToolExecutionEvaluation {
    let matched_assets = assets
        .iter()
        .filter(|asset| asset_matches_tool(asset, tool))
        .filter(|asset| {
            asset.consumers.iter().any(|consumer| {
                matches!(
                    consumer,
                    AssetConsumer::Executor | AssetConsumer::Evaluator | AssetConsumer::Planner
                )
            })
        })
        .collect::<Vec<_>>();
    let signals = infer_tool_execution_signals(plan, outcome);
    let supporting_events = matched_assets.len();
    let human_confirmed = signals
        .iter()
        .any(|signal| matches!(signal, EvaluationSignal::HumanConfirmed));
    let revision_triggered = should_revise(&signals);

    let mut generalize_candidate_ids = Vec::new();
    let mut promote_candidate_ids = Vec::new();
    let mut revise_candidate_ids = Vec::new();
    let mut retire_candidate_ids = Vec::new();
    let mut events = Vec::new();
    let signal_labels = signals
        .iter()
        .map(|signal| format!("{signal:?}"))
        .collect::<Vec<_>>();
    let signal_reason = if signal_labels.is_empty() {
        "no evaluation signals".to_owned()
    } else {
        format!("signals: {}", signal_labels.join(", "))
    };

    for asset in &matched_assets {
        if should_generalize(
            asset,
            supporting_events,
            human_confirmed,
            generalization_policy,
        ) {
            generalize_candidate_ids.push(asset.id.clone());
        }
        let failed_evaluations =
            failed_tool_evaluation_count(&asset.id, assets) + usize::from(revision_triggered);
        if should_retire(failed_evaluations, &signals, retirement_rule) {
            retire_candidate_ids.push(asset.id.clone());
        } else {
            if can_promote(asset, promotion_rule) && !revision_triggered {
                promote_candidate_ids.push(asset.id.clone());
            }
            if revision_triggered {
                revise_candidate_ids.push(asset.id.clone());
            }
        }

        events.push(AssetEvolutionEvent {
            id: format!(
                "event.{}.evaluated.{}",
                asset.id,
                current_unix_timestamp_ms()
            ),
            asset_id: asset.id.clone(),
            action: EvolutionAction::Evaluated,
            reason: signal_reason.clone(),
            inputs: plan.suggested_command.clone(),
            outputs: outcome.observations.clone(),
            confidence_milli: asset_confidence(asset),
            created_at_ms: current_unix_timestamp_ms(),
        });
    }

    for asset_id in &promote_candidate_ids {
        events.push(AssetEvolutionEvent {
            id: format!("event.{asset_id}.promoted.{}", current_unix_timestamp_ms()),
            asset_id: asset_id.clone(),
            action: EvolutionAction::Promoted,
            reason: format!(
                "eligible for promotion from {:?} to {:?}",
                promotion_rule.from_stage, promotion_rule.to_stage
            ),
            inputs: signal_labels.clone(),
            outputs: vec![format!("promote_to:{:?}", promotion_rule.to_stage)],
            confidence_milli: promotion_rule.min_confidence_milli,
            created_at_ms: current_unix_timestamp_ms(),
        });
    }

    for asset_id in &revise_candidate_ids {
        events.push(AssetEvolutionEvent {
            id: format!("event.{asset_id}.revised.{}", current_unix_timestamp_ms()),
            asset_id: asset_id.clone(),
            action: EvolutionAction::Revised,
            reason: "revision rule triggered".to_owned(),
            inputs: signal_labels.clone(),
            outputs: vec!["status:revision-draft".to_owned()],
            confidence_milli: 0,
            created_at_ms: current_unix_timestamp_ms(),
        });
    }

    for asset_id in &retire_candidate_ids {
        events.push(AssetEvolutionEvent {
            id: format!("event.{asset_id}.retired.{}", current_unix_timestamp_ms()),
            asset_id: asset_id.clone(),
            action: EvolutionAction::Retired,
            reason: "retirement rule triggered".to_owned(),
            inputs: signal_labels.clone(),
            outputs: vec!["status:retired".to_owned()],
            confidence_milli: 0,
            created_at_ms: current_unix_timestamp_ms(),
        });
    }

    ToolExecutionEvaluation {
        tool_id: tool.id.clone(),
        matched_asset_ids: matched_assets
            .iter()
            .map(|asset| asset.id.clone())
            .collect(),
        signals,
        supporting_events,
        generalize_candidate_ids,
        promote_candidate_ids,
        revise_candidate_ids,
        retire_candidate_ids,
        events,
    }
}

pub fn room_memory_write_request_from_tool_outcome(
    tool: &ToolSpec,
    outcome: &ToolExecutionOutcome,
) -> RoomMemoryWriteRequest {
    let tool_slug = tool.id.trim_start_matches("tool.");
    let mut summary = outcome.summary.clone();
    if let Some(parent_tool_id) = &outcome.parent_tool_id {
        summary.push_str("\n\nParent tool:\n- ");
        summary.push_str(parent_tool_id);
    }
    if !outcome.invoked_tool_ids.is_empty() {
        summary.push_str("\n\nInvoked tools:\n");
        for tool_id in &outcome.invoked_tool_ids {
            summary.push_str("- ");
            summary.push_str(tool_id);
            summary.push('\n');
        }
        summary = summary.trim_end().to_owned();
    }
    if !outcome.command.is_empty() {
        summary.push_str("\n\nCommand:\n- ");
        summary.push_str(&outcome.command.join(" "));
    }
    if !outcome.observations.is_empty() {
        summary.push_str("\n\nObservations:\n");
        for observation in &outcome.observations {
            summary.push_str("- ");
            summary.push_str(observation);
            summary.push('\n');
        }
        summary = summary.trim_end().to_owned();
    }

    RoomMemoryWriteRequest::new(
        tool_room_id(tool),
        MemoryLayer::Project,
        format!("{} Execution Outcome", tool.name),
        summary,
        MemoryKind::Summary,
    )
    .with_owner(MemoryOwnerRef::project(tool_room_id(tool)))
    .with_tag("tool")
    .with_tag(tool_slug)
    .with_tag("execution")
    .with_tag(if outcome.success {
        "success"
    } else {
        "failure"
    })
}

pub fn room_memory_write_requests_from_tool_evaluation(
    tool: &ToolSpec,
    evaluation: &ToolExecutionEvaluation,
) -> Vec<RoomMemoryWriteRequest> {
    let tool_slug = tool.id.trim_start_matches("tool.");
    let room_id = tool_room_id(tool);
    let signal_labels = evaluation
        .signals
        .iter()
        .map(|signal| format!("{signal:?}"))
        .collect::<Vec<_>>();
    let summary_asset_id = format!(
        "asset.room.{}.evaluation-summary.{}",
        room_id,
        current_unix_timestamp_ms()
    );
    let mut requests = vec![
        RoomMemoryWriteRequest::new(
            room_id.clone(),
            MemoryLayer::Project,
            format!("{} Evaluation Summary", tool.name),
            format!(
                "Signals: {}\n\nMatched assets: {}\nPromote candidates: {}\nRetire candidates: {}",
                if signal_labels.is_empty() {
                    "none".to_owned()
                } else {
                    signal_labels.join(", ")
                },
                if evaluation.matched_asset_ids.is_empty() {
                    "none".to_owned()
                } else {
                    evaluation.matched_asset_ids.join(", ")
                },
                if evaluation.promote_candidate_ids.is_empty() {
                    "none".to_owned()
                } else {
                    evaluation.promote_candidate_ids.join(", ")
                },
                if evaluation.retire_candidate_ids.is_empty() {
                    "none".to_owned()
                } else {
                    evaluation.retire_candidate_ids.join(", ")
                },
            ),
            MemoryKind::Summary,
        )
        .with_owner(MemoryOwnerRef::project(room_id.clone()))
        .with_tag("tool")
        .with_tag(tool_slug)
        .with_tag("evaluation")
        .with_file_name(format!("evaluation-summary.{}.md", summary_asset_id))
        .with_asset_id(summary_asset_id),
    ];

    for event in &evaluation.events {
        let mut request = RoomMemoryWriteRequest::new(
            room_id.clone(),
            MemoryLayer::Project,
            format!("{} {:?}", tool.name, event.action),
            format!(
                "Asset: {}\nReason: {}\nInputs: {}\nOutputs: {}",
                event.asset_id,
                event.reason,
                if event.inputs.is_empty() {
                    "none".to_owned()
                } else {
                    event.inputs.join(", ")
                },
                if event.outputs.is_empty() {
                    "none".to_owned()
                } else {
                    event.outputs.join(", ")
                },
            ),
            MemoryKind::Summary,
        )
        .with_owner(MemoryOwnerRef::project(room_id.clone()))
        .with_tag("tool")
        .with_tag(tool_slug)
        .with_tag("evaluation-event")
        .with_tag(format!("{:?}", event.action).to_ascii_lowercase())
        .with_derived_from(event.asset_id.clone())
        .with_file_name(format!(
            "evaluation-event.{}.{}.md",
            format!("{:?}", event.action).to_ascii_lowercase(),
            event.id
        ))
        .with_asset_id(format!("asset.room.{}.{}", room_id, event.id));
        for signal in &evaluation.signals {
            request = request.with_tag(signal_tag(signal));
        }
        requests.push(request);
    }

    requests
}

pub fn persist_tool_evolution_events(
    root: impl Into<PathBuf>,
    namespace: WorkspaceNamespace,
    tool: &ToolSpec,
    evaluation: &ToolExecutionEvaluation,
) -> Result<Vec<PathBuf>> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let room_id = tool_room_id(tool);
    let room = MemoryRoom::new(
        room_id.clone(),
        MemoryLayer::Project,
        format!("{} Tool Room", tool.name),
        tool.description.clone(),
    );
    let mut paths = Vec::new();

    for event in &evaluation.events {
        let event_tags = vec![
            "tool".to_owned(),
            tool.id.trim_start_matches("tool.").to_owned(),
        ];
        let memory_event = ArtifactEvolutionEvent::new(
            event.id.clone(),
            event.asset_id.clone(),
            room_id.clone(),
            artifact_evolution_action_for_tool_event(&event.action),
            event.reason.clone(),
        )
        .with_created_at_ms(event.created_at_ms);
        paths.push(persist_room_evolution_event(
            &repository,
            &room,
            memory_event,
            event_tags,
            event.inputs.clone(),
            event.outputs.clone(),
        )?);
    }

    Ok(paths)
}

fn artifact_evolution_action_for_tool_event(action: &EvolutionAction) -> ArtifactEvolutionAction {
    match action {
        EvolutionAction::Captured => ArtifactEvolutionAction::Created,
        EvolutionAction::Extracted => ArtifactEvolutionAction::Derived,
        EvolutionAction::Generalized => ArtifactEvolutionAction::Derived,
        EvolutionAction::Compiled => ArtifactEvolutionAction::Promoted,
        EvolutionAction::Bound => ArtifactEvolutionAction::Derived,
        EvolutionAction::Evaluated => ArtifactEvolutionAction::Evaluated,
        EvolutionAction::Promoted => ArtifactEvolutionAction::Promoted,
        EvolutionAction::Revised => ArtifactEvolutionAction::Revised,
        EvolutionAction::Deprecated => ArtifactEvolutionAction::Superseded,
        EvolutionAction::Retired => ArtifactEvolutionAction::Retired,
    }
}

fn persist_room_evolution_event(
    repository: &MemoryRoomRepository,
    room: &MemoryRoom,
    event: ArtifactEvolutionEvent,
    tags: Vec<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
) -> Result<PathBuf> {
    let event = tags
        .into_iter()
        .fold(event, |event, tag| event.with_tag(tag));
    let event = inputs
        .into_iter()
        .fold(event, |event, input| event.with_input(input));
    let created_at_ms = if event.created_at_ms == 0 {
        current_unix_timestamp_ms()
    } else {
        event.created_at_ms
    };
    let event = outputs
        .into_iter()
        .fold(event, |event, output| event.with_output(output))
        .with_created_at_ms(created_at_ms);
    Ok(repository.materialize_evolution_event(room, &event)?.path)
}

pub fn persist_compiled_tool_assets(
    root: impl Into<PathBuf>,
    namespace: WorkspaceNamespace,
    tool: &ToolSpec,
    assets: &[AssetView],
    evaluation: &ToolExecutionEvaluation,
) -> Result<Vec<PathBuf>> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let room_id = tool_room_id(tool);
    let room = MemoryRoom::new(
        room_id.clone(),
        MemoryLayer::Project,
        format!("{} Compiled Tool Assets", tool.name),
        format!("Compiled guidance derived from {}.", tool.name),
    );
    let mut paths = Vec::new();

    for asset in assets.iter().filter(|asset| {
        evaluation
            .promote_candidate_ids
            .iter()
            .any(|candidate| candidate == &asset.id)
    }) {
        let slug = slugify_for_memory(&asset.title);
        let compiled_form = compiled_tool_asset_form(asset);
        let compiled_title = asset.title.clone();
        let compiled_content = compiled_tool_asset_content(asset);
        let compiled_asset_id = format!("asset.{room_id}.compiled.{slug}");
        let mut draft = ArtifactDraft::new(
            room_id.clone(),
            MemoryLayer::Project,
            MemoryRoomAssetKind::Compressed,
            compiled_title,
            compiled_content,
        )
        .with_visibility(hc_memory::MemoryVisibility::Private)
        .with_memory_kind(asset.kind.clone())
        .with_stage(MemoryAssetStage::Compiled)
        .with_form(compiled_form)
        .with_owner(MemoryOwnerRef::project(room_id.clone()))
        .with_tag("tool")
        .with_tag(tool.id.trim_start_matches("tool."))
        .with_tag("compiled")
        .with_tag("promotion")
        .with_derived_from(asset.id.clone())
        .with_file_name(format!("compiled.{slug}.md"));

        for tag in &asset.tags {
            draft = draft.with_tag(tag.clone());
        }

        paths.push(
            repository
                .materialize_artifact_draft(&room, compiled_asset_id, draft)?
                .path,
        );
    }

    Ok(paths)
}

pub fn persist_revised_tool_assets(
    root: impl Into<PathBuf>,
    namespace: WorkspaceNamespace,
    tool: &ToolSpec,
    assets: &[AssetView],
    evaluation: &ToolExecutionEvaluation,
    outcome: &ToolExecutionOutcome,
) -> Result<Vec<PathBuf>> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let room_id = tool_room_id(tool);
    let room = MemoryRoom::new(
        room_id.clone(),
        MemoryLayer::Project,
        format!("{} Revised Tool Assets", tool.name),
        format!("Revision drafts derived from {}.", tool.name),
    );
    let mut paths = Vec::new();
    let revision_stamp = current_unix_timestamp_ms();

    for asset in assets.iter().filter(|asset| {
        evaluation
            .revise_candidate_ids
            .iter()
            .any(|candidate| candidate == &asset.id)
    }) {
        let slug = slugify_for_memory(&asset.title);
        let revised_asset_id = format!("asset.{room_id}.revision.{slug}.{revision_stamp}");
        let mut draft = ArtifactDraft::new(
            room_id.clone(),
            MemoryLayer::Project,
            MemoryRoomAssetKind::Compressed,
            format!("{} Revision {}", asset.title, revision_stamp),
            revised_tool_asset_content(asset, outcome),
        )
        .with_visibility(hc_memory::MemoryVisibility::Private)
        .with_memory_kind(asset.kind.clone())
        .with_stage(asset.stage.clone())
        .with_form(asset.form.clone())
        .with_owner(MemoryOwnerRef::project(room_id.clone()))
        .with_tag("tool")
        .with_tag(tool.id.trim_start_matches("tool."))
        .with_tag("revision")
        .with_tag("draft")
        .with_derived_from(asset.id.clone())
        .with_file_name(format!("revision.{slug}.{revision_stamp}.md"));

        for tag in &asset.tags {
            draft = draft.with_tag(tag.clone());
        }

        paths.push(
            repository
                .materialize_artifact_draft(&room, revised_asset_id, draft)?
                .path,
        );
    }

    Ok(paths)
}

pub fn persist_retired_tool_assets(
    root: impl Into<PathBuf>,
    namespace: WorkspaceNamespace,
    tool: &ToolSpec,
    assets: &[AssetView],
    evaluation: &ToolExecutionEvaluation,
) -> Result<Vec<PathBuf>> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let room_id = tool_room_id(tool);
    let room = MemoryRoom::new(
        room_id.clone(),
        MemoryLayer::Project,
        format!("{} Retired Tool Assets", tool.name),
        format!("Retirement markers for {}.", tool.name),
    );
    let mut paths = Vec::new();
    let retirement_stamp = current_unix_timestamp_ms();

    for asset in assets.iter().filter(|asset| {
        evaluation
            .retire_candidate_ids
            .iter()
            .any(|candidate| candidate == &asset.id)
    }) {
        let slug = slugify_for_memory(&asset.title);
        let retired_asset_id = format!("asset.{room_id}.retired.{slug}.{retirement_stamp}");
        let mut draft = ArtifactDraft::new(
            room_id.clone(),
            MemoryLayer::Project,
            MemoryRoomAssetKind::Compressed,
            format!("{} Retired {}", asset.title, retirement_stamp),
            retired_tool_asset_content(asset, &evaluation.signals),
        )
        .with_visibility(hc_memory::MemoryVisibility::Private)
        .with_memory_kind(asset.kind.clone())
        .with_stage(asset.stage.clone())
        .with_form(asset.form.clone())
        .with_owner(MemoryOwnerRef::project(room_id.clone()))
        .with_tag("tool")
        .with_tag(tool.id.trim_start_matches("tool."))
        .with_tag("retired")
        .with_tag("retirement")
        .with_derived_from(asset.id.clone())
        .with_file_name(format!("retired.{slug}.{retirement_stamp}.md"));

        for tag in &asset.tags {
            draft = draft.with_tag(tag.clone());
        }

        paths.push(
            repository
                .materialize_artifact_draft(&room, retired_asset_id, draft)?
                .path,
        );
    }

    Ok(paths)
}

pub fn summarize_global_preference(
    input: &MemoryOrganizationInput,
) -> Option<(String, MemoryKind)> {
    if let Some(name) = detect_assistant_name_preference(&input.content) {
        return Some((
            format!("User prefers the assistant to be called {}.", name),
            MemoryKind::Preference,
        ));
    }

    let lowered = input.content.to_ascii_lowercase();
    if contains_any(
        &lowered,
        &[
            "??????",
            "?????",
            "???",
            "respond in chinese",
            "answer in chinese",
        ],
    ) {
        return Some((
            "User prefers responses in Chinese.".to_owned(),
            MemoryKind::Preference,
        ));
    }

    if contains_any(
        &lowered,
        &["????", "????", "????", "be concise", "shorter answers"],
    ) {
        return Some((
            "User prefers concise responses.".to_owned(),
            MemoryKind::Preference,
        ));
    }

    None
}

pub fn summarize_global_preference_with_llm(
    registry: &ProviderRegistry,
    model: &hc_llm::ModelRef,
    input: &MemoryOrganizationInput,
) -> Result<Option<(String, MemoryKind)>> {
    let instructions = load_managed_prompt_body(
        default_workspace_root(),
        &workspace_namespace_from_memory_namespace(&input.namespace),
        ManagedPromptKind::GlobalPreferenceSummary,
    )?;
    let system_prompt = load_managed_prompt_body(
        default_workspace_root(),
        &workspace_namespace_from_memory_namespace(&input.namespace),
        ManagedPromptKind::JsonSystemGuard,
    )?;
    let response = registry
        .generate(&GenerateRequest {
            model: model.clone(),
            messages: vec![
                ChatMessage::new(MessageRole::System, system_prompt),
                ChatMessage::new(
                    MessageRole::User,
                    format!(
                        "{}\n\nInput JSON:\n{}",
                        instructions,
                        serde_json::to_string_pretty(input)?
                    ),
                ),
            ],
            temperature: Some(0.1),
            max_output_tokens: Some(300),
            metadata: Default::default(),
        })
        .map_err(anyhow::Error::from)?;
    let parsed: LlmGlobalPreferenceSummaryOutput = parse_json_payload(&response.message.content)?;
    let summary = parsed.summary.trim();
    if summary.is_empty() {
        return Ok(None);
    }
    Ok(Some((summary.to_owned(), parsed.memory_kind)))
}

pub fn default_global_room_id(memory_namespace: &MemoryNamespace) -> String {
    format!(
        "room.global.{}.{}",
        slugify_for_memory(&memory_namespace.tenant_id),
        slugify_for_memory(&memory_namespace.user_id)
    )
}

pub fn persist_global_preference_from_chat_input(
    root: impl Into<PathBuf>,
    workspace_namespace: WorkspaceNamespace,
    memory_namespace: MemoryNamespace,
    chat_room_id: Option<String>,
    content: impl Into<String>,
    registry: &ProviderRegistry,
    model: &hc_llm::ModelRef,
) -> Result<Vec<PathBuf>> {
    let mut input =
        MemoryOrganizationInput::new(memory_namespace.clone(), content.into()).with_tag("chat");
    if let Some(chat_room_id) = chat_room_id.as_deref() {
        input = input
            .with_room_hint(chat_room_id.to_owned(), MemoryLayer::Chat)
            .with_owner(MemoryOwnerRef::session(chat_room_id.to_owned()));
    }

    let Some((summary, memory_kind)) = summarize_global_preference(&input).or_else(|| {
        summarize_global_preference_with_llm(registry, model, &input)
            .ok()
            .flatten()
    }) else {
        return Ok(Vec::new());
    };
    if memory_kind != MemoryKind::Preference {
        return Ok(Vec::new());
    }

    let root = root.into();
    let room_id = default_global_room_id(&memory_namespace);
    let file_slug = slugify_for_memory(&summary);
    if file_slug.is_empty() {
        return Ok(Vec::new());
    }
    let is_assistant_name_preference = is_assistant_name_preference_summary(&summary);
    let mut write_request = RoomMemoryWriteRequest::new(
        room_id.clone(),
        MemoryLayer::Global,
        "Global Preference",
        summary,
        memory_kind,
    )
    .with_visibility(hc_memory::MemoryVisibility::Private)
    .with_owner(MemoryOwnerRef::global())
    .with_tag("global")
    .with_tag("preference")
    .with_file_name(format!("pref.{}.md", file_slug))
    .with_asset_id(format!("asset.{}.{}", room_id, file_slug));
    if is_assistant_name_preference {
        write_request = write_request
            .with_tag("identity")
            .with_tag("assistant-name");
    }

    if let Some(chat_room_id) = chat_room_id {
        write_request = write_request.with_derived_from(chat_room_id);
    }

    Ok(vec![persist_room_memory(
        root,
        workspace_namespace,
        &write_request,
    )?])
}

fn is_assistant_name_preference_summary(summary: &str) -> bool {
    let lowered = summary.to_ascii_lowercase();
    contains_any(
        &lowered,
        &[
            "assistant to be called",
            "assistant's name",
            "call the assistant",
        ],
    )
}

#[derive(Debug, Clone)]
pub struct ChatCaptureOptions {
    pub enabled: bool,
    pub namespace: MemoryNamespace,
    pub room_id: Option<String>,
}

impl ChatCaptureOptions {
    pub fn from_env() -> Self {
        Self {
            enabled: default_chat_memory_enabled_from_env(),
            namespace: runtime_memory_namespace_from_env(),
            room_id: None,
        }
    }
}

pub fn default_chat_room_id(namespace: &MemoryNamespace) -> String {
    format!(
        "room.chat.{}.{}.{}",
        slugify_for_memory(&namespace.tenant_id),
        slugify_for_memory(&namespace.user_id),
        current_unix_timestamp_ms()
    )
}

pub fn create_chat_room(namespace: &MemoryNamespace, room_id: impl Into<String>) -> MemoryRoom {
    let room_id = room_id.into();
    MemoryRoom::new(
        room_id,
        MemoryLayer::Chat,
        "Chat Session",
        "Captured chat turns for context recall.",
    )
    .with_namespace(namespace.clone())
    .with_visibility(hc_memory::MemoryVisibility::Private)
    .with_tag("chat")
    .with_tag("session")
}

pub fn ensure_chat_room(
    root: impl Into<PathBuf>,
    workspace_namespace: WorkspaceNamespace,
    room: &MemoryRoom,
) -> Result<PathBuf> {
    let repository = MemoryRoomRepository::with_namespace(root, workspace_namespace);
    repository.write_room(room)
}

pub fn prepare_chat_capture_room(
    root: impl Into<PathBuf>,
    workspace_namespace: WorkspaceNamespace,
    options: &ChatCaptureOptions,
) -> Result<Option<MemoryRoom>> {
    if !options.enabled {
        return Ok(None);
    }
    let room_id = options
        .room_id
        .clone()
        .unwrap_or_else(|| default_chat_room_id(&options.namespace));
    let room = create_chat_room(&options.namespace, room_id);
    ensure_chat_room(root, workspace_namespace, &room)?;
    Ok(Some(room))
}

pub fn persist_chat_turn_user_message(
    root: impl Into<PathBuf>,
    workspace_namespace: WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: impl Into<String>,
) -> Result<PathBuf> {
    persist_chat_turn_asset(
        root,
        workspace_namespace,
        room,
        turn_index,
        "user",
        "User Message",
        content.into(),
    )
}

pub fn persist_chat_turn_assistant_reply(
    root: impl Into<PathBuf>,
    workspace_namespace: WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: impl Into<String>,
) -> Result<PathBuf> {
    persist_chat_turn_asset(
        root,
        workspace_namespace,
        room,
        turn_index,
        "assistant",
        "Assistant Reply",
        content.into(),
    )
}

fn persist_chat_turn_asset(
    root: impl Into<PathBuf>,
    workspace_namespace: WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    role: &str,
    title_prefix: &str,
    content: String,
) -> Result<PathBuf> {
    let root = root.into();
    let repository = MemoryRoomRepository::with_namespace(root, workspace_namespace);
    let asset_id = format!("asset.{}.turn.{}.{}", room.id, turn_index, role);
    let draft = ArtifactDraft::new(
        room.id.clone(),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Raw,
        format!("{title_prefix} {turn_index}"),
        content,
    )
    .with_visibility(hc_memory::MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Summary)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_tag("chat")
    .with_tag(role)
    .with_file_name(format!("{turn_index:04}.{role}.md"));

    let materialized = repository.materialize_artifact_draft(room, asset_id.clone(), draft)?;
    let event = ArtifactEvolutionEvent::new(
        format!("event.{asset_id}.created.{}", current_unix_timestamp_ms()),
        asset_id,
        room.id.clone(),
        ArtifactEvolutionAction::Created,
        format!("persisted {role} turn into chat room"),
    )
    .with_tag("chat")
    .with_tag(role)
    .with_input(format!("turn:{turn_index}"))
    .with_output(materialized.room_relative_path.clone())
    .with_created_at_ms(current_unix_timestamp_ms());
    repository.write_evolution_event(room, &event)?;
    Ok(materialized.path)
}

fn memory_scope_for_layer(layer: &MemoryLayer) -> MemoryScope {
    match layer {
        MemoryLayer::Chat => MemoryScope::Session,
        MemoryLayer::Topic => MemoryScope::Project,
        MemoryLayer::Task => MemoryScope::Task,
        MemoryLayer::Project => MemoryScope::Project,
        MemoryLayer::Global => MemoryScope::Global,
    }
}

fn confidence_for_room_asset_kind(kind: &MemoryRoomAssetKind) -> u16 {
    match kind {
        MemoryRoomAssetKind::Compressed => 980,
        MemoryRoomAssetKind::Facts => 940,
        MemoryRoomAssetKind::Timeline => 900,
        MemoryRoomAssetKind::Entities | MemoryRoomAssetKind::Relations => 860,
        MemoryRoomAssetKind::Raw => 780,
        MemoryRoomAssetKind::Literary => 640,
    }
}

fn inferred_stage_from_retrieved_memory(memory: &RetrievedMemory) -> MemoryAssetStage {
    if memory
        .tags
        .iter()
        .any(|tag| tag == "prompt" || tag == "compiled")
    {
        MemoryAssetStage::Compiled
    } else if memory.source_kind == "room_compressed" {
        MemoryAssetStage::Generalized
    } else {
        MemoryAssetStage::Extracted
    }
}

fn infer_asset_target_from_room_asset(asset: &MemoryRoomAsset) -> AssetTarget {
    infer_asset_target_from_room_id_and_tags(Some(&asset.room_id), &asset.tags)
}

fn infer_asset_target_from_memory_record(record: &MemoryRecord) -> AssetTarget {
    infer_asset_target_from_owner_scope_and_tags(
        Some(&record.owner),
        Some(&record.scope),
        &record.tags,
    )
}

fn infer_asset_target_ref_from_memory_record(record: &MemoryRecord) -> Option<String> {
    let owner_id = &record.owner.id;
    if owner_id.trim().is_empty() {
        None
    } else {
        Some(owner_id.clone())
    }
}

fn infer_asset_target_from_retrieved_memory(memory: &RetrievedMemory) -> AssetTarget {
    if let Some(room_id) = &memory.room_id {
        infer_asset_target_from_room_id_and_tags(Some(room_id), &memory.tags)
    } else if matches!(memory.scope, MemoryScope::Global)
        || memory.tags.iter().any(|tag| tag == "global")
    {
        AssetTarget::Global
    } else {
        infer_asset_target_from_room_id_and_tags(None, &memory.tags)
    }
}

fn infer_asset_target_ref_from_room_asset(asset: &MemoryRoomAsset) -> Option<String> {
    let target = infer_asset_target_from_room_asset(asset);
    if matches!(target, AssetTarget::Other) {
        None
    } else {
        Some(asset.room_id.clone())
    }
}

fn infer_asset_target_from_room_id_and_tags(room_id: Option<&str>, tags: &[String]) -> AssetTarget {
    if room_id.is_some_and(|room_id| room_id.starts_with("room.tool."))
        || tags.iter().any(|tag| tag == "tool")
    {
        AssetTarget::Tool
    } else if room_id.is_some_and(|room_id| room_id.starts_with("room.agent."))
        || tags.iter().any(|tag| tag == "agent")
    {
        AssetTarget::Agent
    } else if room_id.is_some_and(|room_id| room_id.starts_with("room.project."))
        || tags.iter().any(|tag| tag == "project")
    {
        AssetTarget::Project
    } else if room_id.is_some_and(|room_id| room_id.starts_with("room.task."))
        || tags.iter().any(|tag| tag == "task")
    {
        AssetTarget::Task
    } else if room_id.is_some_and(|room_id| room_id.starts_with("room.topic."))
        || tags.iter().any(|tag| tag == "topic")
    {
        AssetTarget::Topic
    } else if room_id.is_some_and(|room_id| room_id.starts_with("room.global."))
        || tags.iter().any(|tag| tag == "global")
    {
        AssetTarget::Global
    } else {
        AssetTarget::Other
    }
}

fn infer_asset_target_from_owner_scope_and_tags(
    owner: Option<&MemoryOwnerRef>,
    scope: Option<&MemoryScope>,
    tags: &[String],
) -> AssetTarget {
    if owner.is_some_and(|owner| owner.id.starts_with("tool."))
        || tags.iter().any(|tag| tag == "tool")
    {
        AssetTarget::Tool
    } else if owner
        .is_some_and(|owner| owner.id.starts_with("agent.") || owner.id.starts_with("persona."))
        || tags.iter().any(|tag| tag == "agent")
    {
        AssetTarget::Agent
    } else if owner.is_some_and(|owner| owner.id.starts_with("project."))
        || tags.iter().any(|tag| tag == "project")
    {
        AssetTarget::Project
    } else if owner.is_some_and(|owner| owner.id.starts_with("task."))
        || tags.iter().any(|tag| tag == "task")
    {
        AssetTarget::Task
    } else if owner.is_some_and(|owner| owner.id.starts_with("topic."))
        || tags.iter().any(|tag| tag == "topic")
    {
        AssetTarget::Topic
    } else if scope.is_some_and(|scope| matches!(scope, MemoryScope::Global))
        || tags.iter().any(|tag| tag == "global")
    {
        AssetTarget::Global
    } else {
        AssetTarget::Other
    }
}

fn infer_asset_consumers(form: MemoryAssetForm, tags: &[String]) -> Vec<AssetConsumer> {
    let mut consumers = BTreeSet::new();

    match form {
        MemoryAssetForm::Prompt => {
            consumers.insert(AssetConsumer::Llm);
        }
        MemoryAssetForm::Workflow => {
            consumers.insert(AssetConsumer::Planner);
            consumers.insert(AssetConsumer::Human);
        }
        MemoryAssetForm::Policy => {
            consumers.insert(AssetConsumer::Planner);
            consumers.insert(AssetConsumer::Executor);
        }
        MemoryAssetForm::Summary => {
            consumers.insert(AssetConsumer::Human);
            consumers.insert(AssetConsumer::Planner);
        }
        MemoryAssetForm::Rewrite => {
            consumers.insert(AssetConsumer::Llm);
            consumers.insert(AssetConsumer::Human);
        }
        _ => {
            consumers.insert(AssetConsumer::Human);
        }
    }

    if tags.iter().any(|tag| tag == "validation") {
        consumers.insert(AssetConsumer::Executor);
        consumers.insert(AssetConsumer::Evaluator);
    }
    if tags.iter().any(|tag| tag == "recipe" || tag == "recovery") {
        consumers.insert(AssetConsumer::Executor);
    }

    consumers.into_iter().collect()
}

fn infer_asset_status(tags: &[String]) -> AssetStatus {
    if tags.iter().any(|tag| tag == "retired") {
        AssetStatus::Retired
    } else if tags.iter().any(|tag| tag == "deprecated") {
        AssetStatus::Deprecated
    } else if tags.iter().any(|tag| tag == "draft") {
        AssetStatus::Draft
    } else {
        AssetStatus::Active
    }
}

fn default_asset_form_for_memory_kind(kind: &MemoryKind) -> MemoryAssetForm {
    match kind {
        MemoryKind::Preference => MemoryAssetForm::Policy,
        MemoryKind::WorkflowMemory => MemoryAssetForm::Workflow,
        MemoryKind::Summary => MemoryAssetForm::Summary,
        MemoryKind::Knowledge => MemoryAssetForm::Fact,
        MemoryKind::Decision => MemoryAssetForm::Policy,
    }
}

fn infer_prompt_asset_kind_from_asset_view(asset: &AssetView) -> Option<PromptAssetKind> {
    if asset.tags.iter().any(|tag| tag == "styleguide") {
        Some(PromptAssetKind::StyleGuide)
    } else if asset.tags.iter().any(|tag| tag == "behaviortemplate") {
        Some(PromptAssetKind::BehaviorTemplate)
    } else if asset.tags.iter().any(|tag| tag == "outputcontract") {
        Some(PromptAssetKind::OutputContract)
    } else if asset.tags.iter().any(|tag| tag == "systempolicy") {
        Some(PromptAssetKind::SystemPolicy)
    } else if asset
        .tags
        .iter()
        .any(|tag| tag == "promptmemory" || tag == "prompt")
    {
        Some(PromptAssetKind::PromptMemory)
    } else {
        None
    }
}

fn default_tool_command(tool: &ToolSpec, goal: &str) -> Vec<String> {
    toolchain_default_tool_command(tool, goal)
}

pub fn tool_room_id(tool: &ToolSpec) -> String {
    format!("room.tool.{}", tool.id.trim_start_matches("tool."))
}

fn read_tool_room_memories(
    root: impl AsRef<Path>,
    query: &ContextMemoryQuery,
    tool: &ToolSpec,
) -> Result<Vec<RetrievedMemory>> {
    let Some(namespace) = query.memory_query.namespace.as_ref() else {
        return Ok(Vec::new());
    };
    let workspace_namespace = workspace_namespace_from_memory_namespace(namespace);
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), workspace_namespace);
    let room = MemoryRoom::new(
        tool_room_id(tool),
        MemoryLayer::Project,
        tool.name.clone(),
        tool.description.clone(),
    );
    let assets = repository.read_compressed_assets(&room).unwrap_or_default();
    Ok(assets
        .into_iter()
        .map(|asset| RetrievedMemory::from(&asset))
        .collect())
}

fn merge_retrieved_memories(existing: &mut Vec<RetrievedMemory>, extras: Vec<RetrievedMemory>) {
    let mut seen = existing
        .iter()
        .map(|memory| memory.id.clone())
        .collect::<BTreeSet<_>>();
    for memory in extras {
        if seen.insert(memory.id.clone()) {
            existing.push(memory);
        }
    }
}

fn tool_slug_from_asset(asset: &AssetView) -> Option<String> {
    if let Some(target_ref) = &asset.target_ref
        && let Some(rest) = target_ref.strip_prefix("room.tool.")
    {
        return Some(rest.to_owned());
    }

    asset
        .tags
        .iter()
        .find(|tag| {
            !matches!(
                tag.as_str(),
                "tool" | "recipe" | "validation" | "recovery" | "prompt"
            )
        })
        .cloned()
}

fn asset_matches_tool(asset: &AssetView, tool: &ToolSpec) -> bool {
    if asset.target != AssetTarget::Tool
        || matches!(asset.status, AssetStatus::Retired | AssetStatus::Draft)
        || !asset
            .consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Executor)
    {
        return false;
    }

    let tool_slug = tool.id.trim_start_matches("tool.");
    tool_slug_from_asset(asset).is_some_and(|slug| slug == tool_slug)
}

fn clean_tool_export_assets(tool: &ToolSpec, assets: &[AssetView]) -> Vec<AssetView> {
    let mut selected = assets
        .iter()
        .filter(|asset| asset_matches_tool(asset, tool))
        .filter(|asset| !is_process_asset(asset))
        .cloned()
        .collect::<Vec<_>>();

    selected.sort_by_key(tool_asset_priority);
    selected.reverse();
    selected
}

fn is_process_asset(asset: &AssetView) -> bool {
    asset.tags.iter().any(|tag| {
        matches!(
            tag.as_str(),
            "evaluation"
                | "evaluation-event"
                | "revision"
                | "draft"
                | "retired"
                | "retirement"
                | "timeline"
                | "event"
                | "deprecated"
        )
    })
}

fn tool_export_asset_role(asset: &AssetView) -> String {
    if asset.tags.iter().any(|tag| tag == "recipe") {
        "recipe".to_owned()
    } else if asset.tags.iter().any(|tag| tag == "validation") {
        "validation".to_owned()
    } else if asset.tags.iter().any(|tag| tag == "recovery") {
        "recovery".to_owned()
    } else if asset.form == MemoryAssetForm::Prompt {
        "prompt".to_owned()
    } else {
        "support".to_owned()
    }
}

fn clean_export_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .filter(|tag| {
            !matches!(
                tag.as_str(),
                "promotion"
                    | "evaluation"
                    | "evaluation-event"
                    | "revision"
                    | "draft"
                    | "retired"
                    | "retirement"
                    | "timeline"
                    | "event"
                    | "deprecated"
            )
        })
        .cloned()
        .collect()
}

fn render_export_asset_markdown(asset: &AssetView, role: &str, tags: &[String]) -> String {
    let tags = if tags.is_empty() {
        "none".to_owned()
    } else {
        tags.join(", ")
    };
    format!(
        "# {}\n\nRole: {}\nKind: {:?}\nStage: {:?}\nForm: {:?}\nTags: {}\n\n{}\n",
        asset.title,
        role,
        asset.kind,
        asset.stage,
        asset.form,
        tags,
        asset.content.trim()
    )
}

fn render_layered_capability_readme(package: &ToolCapabilityExportPackage) -> String {
    format!(
        "# {}\n\n{}\n\n## Layers\n\n- portable: importable capability manifest and clean assets.\n- runnable: current executable plan and `run.sh` entrypoint.\n",
        package.manifest.tool.name, package.manifest.tool.description
    )
}

fn render_portable_capability_readme(package: &ToolCapabilityExportPackage) -> String {
    let mut readme = format!(
        "# {} Portable Capability\n\n{}\n\n## Assets\n\n",
        package.manifest.tool.name, package.manifest.tool.description,
    );
    if package.manifest.assets.is_empty() {
        readme.push_str("No clean executable assets were available at export time.\n");
    } else {
        for asset in &package.manifest.assets {
            readme.push_str(&format!(
                "- {}: [{}]({})\n",
                asset.role, asset.title, asset.file
            ));
        }
    }
    readme
}

fn render_runnable_capability_readme(package: &ToolCapabilityExportPackage) -> String {
    format!(
        "# {} Runnable Capability\n\n```sh\n./run.sh\n```\n\nDefault command:\n\n```sh\n{}\n```\n",
        package.manifest.tool.name,
        package.manifest.command.join(" ")
    )
}

fn render_run_script(plan: &ToolExecutionPlan) -> String {
    let command = plan
        .suggested_command
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!("#!/usr/bin/env sh\nset -eu\nexec {command} \"$@\"\n")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

impl ToolExecutionBinder for DefaultToolExecutionBinder {
    fn bind(&self, goal: &str, tool: &ToolSpec, assets: &[AssetView]) -> Result<ToolExecutionPlan> {
        let mut compiled_guidance = Vec::new();
        let mut guidance = Vec::new();
        let mut compiled_validation_steps = Vec::new();
        let mut validation_steps = Vec::new();
        let mut compiled_recovery_steps = Vec::new();
        let mut recovery_steps = Vec::new();

        for asset in assets
            .iter()
            .filter(|asset| asset_matches_tool(asset, tool))
        {
            if asset.tags.iter().any(|tag| tag == "recipe") {
                if asset.stage == MemoryAssetStage::Compiled
                    || asset.tags.iter().any(|tag| tag == "compiled")
                {
                    push_unique(&mut compiled_guidance, asset.content.clone());
                } else {
                    push_unique(&mut guidance, asset.content.clone());
                }
            } else if asset.tags.iter().any(|tag| tag == "validation") {
                if asset.stage == MemoryAssetStage::Compiled
                    || asset.tags.iter().any(|tag| tag == "compiled")
                {
                    push_unique(&mut compiled_validation_steps, asset.content.clone());
                } else {
                    push_unique(&mut validation_steps, asset.content.clone());
                }
            } else if asset.tags.iter().any(|tag| tag == "recovery") {
                if asset.stage == MemoryAssetStage::Compiled
                    || asset.tags.iter().any(|tag| tag == "compiled")
                {
                    push_unique(&mut compiled_recovery_steps, asset.content.clone());
                } else {
                    push_unique(&mut recovery_steps, asset.content.clone());
                }
            }
        }

        Ok(ToolExecutionPlan {
            tool_id: tool.id.clone(),
            suggested_command: default_tool_command(tool, goal),
            guidance: if compiled_guidance.is_empty() {
                guidance
            } else {
                compiled_guidance
            },
            validation_steps: if compiled_validation_steps.is_empty() {
                validation_steps
            } else {
                compiled_validation_steps
            },
            recovery_steps: if compiled_recovery_steps.is_empty() {
                recovery_steps
            } else {
                compiled_recovery_steps
            },
        })
    }
}

pub fn should_generalize(
    asset: &AssetView,
    supporting_events: usize,
    human_confirmed: bool,
    policy: &GeneralizationPolicy,
) -> bool {
    if human_confirmed && policy.allow_human_confirmation_override {
        return asset.status != AssetStatus::Retired;
    }

    if asset.status == AssetStatus::Retired || asset.status == AssetStatus::Draft {
        return false;
    }

    asset_confidence(asset) >= policy.min_confidence_milli
        && supporting_events >= policy.min_supporting_events
        && (!policy.require_repeated_pattern || supporting_events > 1)
}

pub fn can_promote(asset: &AssetView, rule: &PromotionRule) -> bool {
    asset.stage == rule.from_stage
        && asset_confidence(asset) >= rule.min_confidence_milli
        && rule
            .required_tags
            .iter()
            .all(|tag| asset.tags.iter().any(|candidate| candidate == tag))
        && rule.required_consumers.iter().all(|consumer| {
            asset
                .consumers
                .iter()
                .any(|candidate| candidate == consumer)
        })
}

pub fn should_retire(
    failed_evaluations: usize,
    signals: &[EvaluationSignal],
    rule: &RetirementRule,
) -> bool {
    (rule.retire_on_explicit_human_rejection
        && signals
            .iter()
            .any(|signal| matches!(signal, EvaluationSignal::HumanRejected)))
        || failed_evaluations >= rule.max_failed_evaluations
        || (rule.allow_replacement_by_newer_asset
            && signals
                .iter()
                .any(|signal| matches!(signal, EvaluationSignal::SupersededByNewerAsset)))
}

pub fn should_revise(signals: &[EvaluationSignal]) -> bool {
    signals.iter().any(|signal| {
        matches!(
            signal,
            EvaluationSignal::ExecutionFailed | EvaluationSignal::ValidationFailed
        )
    })
}

pub fn infer_tool_execution_signals(
    plan: &ToolExecutionPlan,
    outcome: &ToolExecutionOutcome,
) -> Vec<EvaluationSignal> {
    let mut signals = Vec::new();
    if outcome.success {
        signals.push(EvaluationSignal::ExecutionSucceeded);
    } else {
        signals.push(EvaluationSignal::ExecutionFailed);
    }

    if !plan.validation_steps.is_empty() {
        let combined = format!("{}\n{}", outcome.summary, outcome.observations.join("\n"));
        let lowered = combined.to_ascii_lowercase();
        let validation_failed = !outcome.success
            || lowered.contains("0 tests")
            || lowered.contains("no rg matches")
            || lowered.contains("no file candidates")
            || lowered.contains("no matches found");
        signals.push(if validation_failed {
            EvaluationSignal::ValidationFailed
        } else {
            EvaluationSignal::ValidationPassed
        });
    }

    if outcome.success && outcome.observations.len() >= 2 {
        signals.push(EvaluationSignal::RepeatedReuse);
    }

    signals
}

fn asset_confidence(asset: &AssetView) -> u16 {
    if asset.tags.iter().any(|tag| tag == "high-confidence") {
        950
    } else if asset.status == AssetStatus::Draft {
        500
    } else {
        800
    }
}

fn current_unix_timestamp_ms() -> u128 {
    wall_clock_ms() as u128
}

fn compiled_tool_asset_form(asset: &AssetView) -> MemoryAssetForm {
    if asset.tags.iter().any(|tag| tag == "recipe") {
        MemoryAssetForm::Workflow
    } else if asset.tags.iter().any(|tag| tag == "validation") {
        MemoryAssetForm::Policy
    } else if asset.tags.iter().any(|tag| tag == "recovery") {
        MemoryAssetForm::Policy
    } else {
        MemoryAssetForm::Policy
    }
}

fn compiled_tool_asset_content(asset: &AssetView) -> String {
    asset.content.trim().to_owned()
}

fn revised_tool_asset_content(asset: &AssetView, outcome: &ToolExecutionOutcome) -> String {
    format!(
        "Revision draft for asset `{}`.\n\nFailure summary: {}\n\nOriginal guidance:\n{}",
        asset.id,
        outcome.summary,
        asset.content.trim()
    )
}

fn retired_tool_asset_content(asset: &AssetView, signals: &[EvaluationSignal]) -> String {
    let signal_summary = if signals.is_empty() {
        "none".to_owned()
    } else {
        signals
            .iter()
            .map(|signal| format!("{signal:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Retirement marker for asset `{}`.\n\nSignals: {}\n\nPrevious guidance:\n{}",
        asset.id,
        signal_summary,
        asset.content.trim()
    )
}

fn tool_asset_priority(asset: &AssetView) -> u8 {
    let mut score = 0u8;
    if asset.stage == MemoryAssetStage::Compiled || asset.tags.iter().any(|tag| tag == "compiled") {
        score += 4;
    }
    if asset.tags.iter().any(|tag| tag == "promotion") {
        score += 2;
    }
    if asset.tags.iter().any(|tag| tag == "recipe") {
        score += 1;
    }
    score
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !values.iter().any(|value| value == &candidate) {
        values.push(candidate);
    }
}

fn superseded_tool_asset_ids(assets: &[AssetView]) -> BTreeSet<String> {
    let mut superseded = BTreeSet::new();
    for asset in assets {
        if asset.tags.iter().any(|tag| {
            matches!(
                tag.as_str(),
                "compiled" | "promotion" | "retired" | "deprecated"
            )
        }) {
            for source in &asset.derived_from {
                superseded.insert(source.clone());
            }
        }
    }
    superseded
}

fn failed_tool_evaluation_count(asset_id: &str, assets: &[AssetView]) -> usize {
    assets
        .iter()
        .filter(|asset| asset.tags.iter().any(|tag| tag == "evaluation-event"))
        .filter(|asset| asset.derived_from.iter().any(|source| source == asset_id))
        .filter(|asset| {
            asset.tags.iter().any(|tag| {
                matches!(
                    tag.as_str(),
                    "execution_failed"
                        | "validation_failed"
                        | "executionfailed"
                        | "validationfailed"
                )
            })
        })
        .count()
}

fn signal_tag(signal: &EvaluationSignal) -> &'static str {
    match signal {
        EvaluationSignal::HumanConfirmed => "human_confirmed",
        EvaluationSignal::HumanRejected => "human_rejected",
        EvaluationSignal::ExecutionSucceeded => "execution_succeeded",
        EvaluationSignal::ExecutionFailed => "execution_failed",
        EvaluationSignal::ValidationPassed => "validation_passed",
        EvaluationSignal::ValidationFailed => "validation_failed",
        EvaluationSignal::RepeatedReuse => "repeated_reuse",
        EvaluationSignal::SupersededByNewerAsset => "superseded_by_newer_asset",
    }
}

fn pseudo_record_for_room_asset(asset: &MemoryRoomAsset) -> MemoryRecord {
    let mut record = MemoryRecord::new(
        asset.id.clone(),
        memory_scope_for_layer(&asset.layer),
        asset.owners.first().cloned().unwrap_or_else(|| {
            MemoryOwnerRef::new(owner_kind_for_layer(&asset.layer), asset.room_id.clone())
        }),
        asset.memory_kind.clone(),
        asset.title.clone(),
        asset.summary.clone(),
    )
    .with_namespace(asset.namespace.clone())
    .with_visibility(asset.visibility.clone())
    .with_confidence_milli(confidence_for_room_asset_kind(&asset.kind));

    for tag in &asset.tags {
        record = record.with_tag(tag.clone());
    }
    for source in &asset.derived_from {
        record = record.with_derived_from(source.clone());
    }

    record
}

fn prompt_asset_target_room(
    source_memory: &RetrievedMemory,
    namespace: &MemoryNamespace,
) -> Option<(String, MemoryLayer)> {
    if let (Some(room_id), Some(layer)) = (&source_memory.room_id, &source_memory.layer)
        && *layer != MemoryLayer::Chat
    {
        return Some((room_id.clone(), layer.clone()));
    }

    if source_memory.tags.iter().any(|tag| tag == "agent") {
        return Some((
            format!("room.agent.{}", prompt_room_slug(source_memory)),
            MemoryLayer::Global,
        ));
    }

    if source_memory.tags.iter().any(|tag| tag == "tool") {
        return Some((
            format!("room.tool.{}", prompt_room_slug(source_memory)),
            MemoryLayer::Project,
        ));
    }

    if source_memory.tags.iter().any(|tag| tag == "topic") {
        return Some((
            format!("room.topic.{}", prompt_room_slug(source_memory)),
            MemoryLayer::Topic,
        ));
    }

    if source_memory.tags.iter().any(|tag| tag == "project") {
        return Some((
            format!("room.project.{}", prompt_room_slug(source_memory)),
            MemoryLayer::Project,
        ));
    }

    if source_memory.tags.iter().any(|tag| tag == "task") {
        return Some((
            format!("room.task.{}", prompt_room_slug(source_memory)),
            MemoryLayer::Task,
        ));
    }

    match source_memory.scope {
        MemoryScope::Global => Some((
            format!(
                "room.global.{}.{}",
                slugify_for_memory(&namespace.tenant_id),
                slugify_for_memory(&namespace.user_id)
            ),
            MemoryLayer::Global,
        )),
        _ => None,
    }
}

fn prompt_room_slug(source_memory: &RetrievedMemory) -> String {
    for tag in &source_memory.tags {
        if matches!(
            tag.as_str(),
            "prompt"
                | "global"
                | "preference"
                | "styleguide"
                | "behaviortemplate"
                | "promptmemory"
                | "topic"
                | "agent"
                | "tool"
                | "project"
                | "task"
        ) {
            continue;
        }
        let slug = slugify_for_memory(tag);
        if !slug.is_empty() {
            return slug;
        }
    }

    slugify_for_memory(&source_memory.title)
}

fn owner_kind_for_layer(layer: &MemoryLayer) -> MemoryOwnerKind {
    match layer {
        MemoryLayer::Chat => MemoryOwnerKind::Session,
        MemoryLayer::Topic => MemoryOwnerKind::Project,
        MemoryLayer::Task => MemoryOwnerKind::Task,
        MemoryLayer::Project => MemoryOwnerKind::Project,
        MemoryLayer::Global => MemoryOwnerKind::Global,
    }
}

fn room_asset_matches_query(
    query: &ContextMemoryQuery,
    asset: &MemoryRoomAsset,
    _retrieved: &RetrievedMemory,
) -> bool {
    let pseudo_record = pseudo_record_for_room_asset(asset);
    if !query.memory_query.matches(&pseudo_record) {
        return false;
    }

    if let Some(owner) = &query.memory_query.owner
        && !asset.owners.iter().any(|candidate| candidate == owner)
    {
        return false;
    }

    true
}

fn room_asset_entry_may_match_query(
    query: &ContextMemoryQuery,
    entry: &MarkdownIndexEntry,
) -> bool {
    if let Some(scope) = &query.memory_query.scope
        && let Some(layer) = entry.layer.as_deref().and_then(memory_layer_from_label)
        && *scope != memory_scope_for_layer(&layer)
    {
        return false;
    }

    if let Some(kind) = &query.memory_query.kind
        && let Some(memory_kind) = entry.memory_kind.as_deref()
        && memory_kind != memory_kind_label(kind)
    {
        return false;
    }

    if let Some(owner) = &query.memory_query.owner
        && !entry.owners.is_empty()
        && !entry.owners.iter().any(|candidate| candidate == &owner.id)
    {
        return false;
    }

    true
}

fn memory_layer_from_label(label: &str) -> Option<MemoryLayer> {
    match label {
        "chat" => Some(MemoryLayer::Chat),
        "topic" => Some(MemoryLayer::Topic),
        "task" => Some(MemoryLayer::Task),
        "project" => Some(MemoryLayer::Project),
        "global" => Some(MemoryLayer::Global),
        _ => None,
    }
}

fn default_room_asset_file_name(request: &RoomMemoryWriteRequest) -> String {
    let millis = current_unix_timestamp_ms();
    format!(
        "min.{}.{}.md",
        millis,
        format!("{:?}", request.memory_kind).to_ascii_lowercase()
    )
}

fn default_room_asset_id(request: &RoomMemoryWriteRequest) -> String {
    let millis = current_unix_timestamp_ms();
    format!(
        "asset.{}.{}.{}",
        request.room_id,
        format!("{:?}", request.memory_kind).to_ascii_lowercase(),
        millis
    )
}

fn render_self_model_section(self_model: &SelfModel) -> String {
    let mut lines = vec![
        "Self context:".to_owned(),
        format!("- id: {}", self_model.id),
        format!("- name: {}", self_model.name),
        format!("- role: {}", self_model.role),
        format!("- description: {}", self_model.description),
    ];

    if let Some(style) = &self_model.style {
        lines.push(format!("- style: {style}"));
    }

    if !self_model.goals.is_empty() {
        lines.push(format!("- goals: {}", self_model.goals.join(" | ")));
    }

    if !self_model.capabilities.is_empty() {
        let rendered = self_model
            .capabilities
            .iter()
            .map(|capability| format!("{} ({})", capability.name, capability.description))
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(format!("- capabilities: {rendered}"));
    }

    if !self_model.constraints.is_empty() {
        let rendered = self_model
            .constraints
            .iter()
            .map(|constraint| format!("{} ({})", constraint.name, constraint.description))
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(format!("- constraints: {rendered}"));
    }

    lines.join("\n")
}

fn default_layer_for_owner_kind(kind: &MemoryOwnerKind) -> MemoryLayer {
    match kind {
        MemoryOwnerKind::Global => MemoryLayer::Global,
        MemoryOwnerKind::Persona => MemoryLayer::Global,
        MemoryOwnerKind::Session | MemoryOwnerKind::Instance => MemoryLayer::Chat,
        MemoryOwnerKind::Project => MemoryLayer::Project,
        MemoryOwnerKind::Task => MemoryLayer::Task,
    }
}

fn default_room_id_for_owner(owner: &MemoryOwnerRef) -> String {
    let prefix = match owner.kind {
        MemoryOwnerKind::Global => "room.global",
        MemoryOwnerKind::Persona => "room.global.persona",
        MemoryOwnerKind::Session => "room.chat.session",
        MemoryOwnerKind::Instance => "room.chat.instance",
        MemoryOwnerKind::Project => "room.project",
        MemoryOwnerKind::Task => "room.task",
    };
    format!("{prefix}.{}", slugify_for_memory(&owner.id))
}

fn summarize_title_from_content(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return "Memory Note".to_owned();
    }
    trimmed.chars().take(64).collect()
}

fn slugify_for_memory(value: &str) -> String {
    let mut slug = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('.') {
            slug.push('.');
        }
    }
    slug.trim_matches('.').to_owned()
}

fn contains_any(content: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| content.contains(candidate))
}

fn detect_assistant_name_preference(content: &str) -> Option<String> {
    for marker in [
        "\u{4f60}\u{4ee5}\u{540e}\u{53eb}",
        "\u{4ee5}\u{540e}\u{53eb}\u{4f60}",
        "\u{4ee5}\u{540e}\u{4f60}\u{53eb}",
        "call you ",
    ] {
        if let Some(rest) = content.split_once(marker).map(|(_, rest)| rest.trim()) {
            let candidate = rest
                .trim_matches(|character: char| {
                    character.is_ascii_punctuation() || character.is_whitespace()
                })
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|character: char| {
                    character.is_ascii_punctuation() || character.is_whitespace()
                });
            if !candidate.is_empty() {
                return Some(candidate.to_owned());
            }
        }
    }
    None
}

fn infer_prompt_asset_kind_from_preference(memory: &RetrievedMemory) -> PromptAssetKind {
    let lowered = memory.summary.to_ascii_lowercase();
    if contains_any(
        &lowered,
        &[
            "concise", "style", "language", "中文", "markdown", "shorter",
        ],
    ) {
        PromptAssetKind::StyleGuide
    } else if contains_any(
        &lowered,
        &["called", "name", "叫", "称呼", "call the assistant"],
    ) {
        PromptAssetKind::BehaviorTemplate
    } else {
        PromptAssetKind::PromptMemory
    }
}

fn infer_prompt_asset_kind_from_compiled_memory(
    memory: &RetrievedMemory,
) -> Option<PromptAssetKind> {
    if memory.tags.iter().any(|tag| tag == "styleguide") {
        Some(PromptAssetKind::StyleGuide)
    } else if memory.tags.iter().any(|tag| tag == "behaviortemplate") {
        Some(PromptAssetKind::BehaviorTemplate)
    } else if memory.tags.iter().any(|tag| tag == "outputcontract") {
        Some(PromptAssetKind::OutputContract)
    } else if memory.tags.iter().any(|tag| tag == "systempolicy") {
        Some(PromptAssetKind::SystemPolicy)
    } else if memory.tags.iter().any(|tag| tag == "promptmemory") {
        Some(PromptAssetKind::PromptMemory)
    } else {
        None
    }
}

fn load_managed_prompt_body(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    kind: ManagedPromptKind,
) -> Result<String> {
    let store = WorkspaceStore::new(root.as_ref().to_path_buf());
    let relative_path = managed_prompt_relative_path(kind);
    ensure_managed_prompt(root.as_ref(), namespace, kind)?;
    let prompt_path = store.resolve_in_namespace(namespace, &relative_path);
    fs::read_to_string(&prompt_path)
        .map(|content| content.trim().to_owned())
        .map_err(anyhow::Error::from)
}

fn ensure_managed_prompt(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    kind: ManagedPromptKind,
) -> Result<()> {
    let store = WorkspaceStore::new(root.as_ref().to_path_buf());
    let relative_path = managed_prompt_relative_path(kind);
    let prompt_path = store.resolve_in_namespace(namespace, &relative_path);
    let default_content = managed_prompt_default_body(kind);
    if !prompt_path.exists() {
        store.write_text_in_namespace(namespace, &relative_path, default_content)?;
    }

    let sidecar_path =
        store.resolve_in_namespace(namespace, managed_prompt_sidecar_relative_path(kind));
    if !sidecar_path.exists() {
        if let Some(parent) = sidecar_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &sidecar_path,
            serde_json::to_string_pretty(&managed_prompt_metadata(kind))?,
        )?;
    }

    let prompt_body = fs::read_to_string(&prompt_path)?;
    sync_managed_prompt_room_asset(root, namespace, kind, prompt_body.trim())?;
    Ok(())
}

fn sync_managed_prompt_room_asset(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    kind: ManagedPromptKind,
    content: &str,
) -> Result<PathBuf> {
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let room = managed_prompt_room(namespace);
    repository.write_room(&room)?;

    let metadata = managed_prompt_metadata(kind);
    let file_name = managed_prompt_relative_path(kind)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("prompt.md")
        .to_owned();
    let current_relative = MemoryRoomRepository::prompt_doc_relative_path(&room, &file_name);

    let previous_asset = repository.read_asset(&current_relative).ok();
    let mut derived_from = Vec::new();
    let mut source_docs = vec![managed_prompt_relative_path(kind).display().to_string()];

    if let Some(previous) = previous_asset
        && previous.summary.trim() != content.trim()
    {
        let revision_asset =
            archive_managed_prompt_revision(&repository, &room, &metadata, kind, &previous)?;
        derived_from.push(revision_asset.id.clone());
        let revision_relative =
            MemoryRoomRepository::prompt_doc_relative_path(&room, &revision_asset.file_name);
        source_docs.push(revision_relative.display().to_string());
        persist_room_evolution_event(
            &repository,
            &room,
            ArtifactEvolutionEvent::new(
                format!(
                    "event.{}.revised.{}",
                    metadata.id,
                    current_unix_timestamp_ms()
                ),
                revision_asset.id.clone(),
                room.id.clone(),
                ArtifactEvolutionAction::Revised,
                "archived previous managed prompt revision before syncing current body",
            ),
            vec![
                "managed_prompt".to_owned(),
                "revision".to_owned(),
                metadata.kind.clone(),
            ],
            vec![metadata.id.clone()],
            vec![revision_asset.file_name.clone()],
        )?;
    }

    let mut asset = MemoryRoomAsset::new(
        metadata.id.clone(),
        room.id.clone(),
        file_name,
        room.layer.clone(),
        MemoryRoomAssetKind::Compressed,
        metadata.title.clone(),
        content.trim().to_owned(),
    )
    .with_namespace(MemoryNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    ))
    .with_visibility(hc_memory::MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Preference)
    .with_stage(MemoryAssetStage::Compiled)
    .with_form(MemoryAssetForm::Prompt)
    .with_tag("managed_prompt")
    .with_tag("current")
    .with_tag(metadata.kind.clone());

    for tag in metadata.tags {
        asset = asset.with_tag(tag);
    }
    for source in derived_from {
        asset = asset.with_derived_from(source);
    }
    for source_doc in source_docs {
        asset = asset.with_source_doc(source_doc);
    }

    let materialized = repository.materialize_asset(&room, &asset)?;
    persist_room_evolution_event(
        &repository,
        &room,
        ArtifactEvolutionEvent::new(
            format!(
                "event.{}.compiled.{}",
                metadata.id,
                current_unix_timestamp_ms()
            ),
            metadata.id.clone(),
            room.id.clone(),
            ArtifactEvolutionAction::Promoted,
            "synced managed prompt body into prompt library room",
        ),
        vec![
            "managed_prompt".to_owned(),
            "current".to_owned(),
            metadata.kind.clone(),
        ],
        vec![managed_prompt_relative_path(kind).display().to_string()],
        vec![materialized.room_relative_path.clone()],
    )?;
    Ok(materialized.path)
}

fn archive_managed_prompt_revision(
    repository: &MemoryRoomRepository,
    room: &MemoryRoom,
    metadata: &ManagedPromptMetadata,
    kind: ManagedPromptKind,
    previous: &MemoryRoomAsset,
) -> Result<MemoryRoomAsset> {
    let revision_stamp = current_unix_timestamp_ms();
    let managed_path = managed_prompt_relative_path(kind);
    let base_name = managed_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("prompt");
    let revision_file_name = format!("rev.{revision_stamp}.{base_name}.md");
    let revision_id = format!("{}.rev.{revision_stamp}", metadata.id);

    let mut revision_asset = MemoryRoomAsset::new(
        revision_id,
        room.id.clone(),
        revision_file_name,
        room.layer.clone(),
        MemoryRoomAssetKind::Compressed,
        format!("{} Revision {}", metadata.title, revision_stamp),
        previous.summary.clone(),
    )
    .with_namespace(previous.namespace.clone())
    .with_visibility(previous.visibility.clone())
    .with_memory_kind(previous.memory_kind.clone())
    .with_stage(MemoryAssetStage::Compiled)
    .with_form(MemoryAssetForm::Prompt)
    .with_tag("managed_prompt")
    .with_tag("revision")
    .with_tag(metadata.kind.clone());

    for tag in &metadata.tags {
        revision_asset = revision_asset.with_tag(tag.clone());
    }
    for source in &previous.derived_from {
        revision_asset = revision_asset.with_derived_from(source.clone());
    }
    for source_doc in &previous.source_docs {
        revision_asset = revision_asset.with_source_doc(source_doc.clone());
    }

    let _materialized = repository.materialize_asset(room, &revision_asset)?;
    Ok(revision_asset)
}

fn managed_prompt_room(namespace: &WorkspaceNamespace) -> MemoryRoom {
    MemoryRoom::new(
        "room.project.prompt-library",
        MemoryLayer::Project,
        "Managed Prompt Library",
        format!(
            "Managed prompt templates for {}.{}.",
            namespace.tenant_id, namespace.user_id
        ),
    )
    .with_namespace(MemoryNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    ))
    .with_visibility(hc_memory::MemoryVisibility::Private)
    .with_tag("prompt-library")
    .with_tag("managed-prompt")
    .with_tag("project")
}

fn managed_prompt_relative_path(kind: ManagedPromptKind) -> PathBuf {
    let (group, file_name) = match kind {
        ManagedPromptKind::MemoryOrganizer => ("organizer", "memory-organizer.md"),
        ManagedPromptKind::PromptAssetSynthesizer => ("synthesis", "prompt-asset-synthesizer.md"),
        ManagedPromptKind::SemanticTagSuggester => ("extract", "semantic-tags.md"),
        ManagedPromptKind::GlobalPreferenceSummary => ("summarize", "global-preference-summary.md"),
        ManagedPromptKind::AssistantWenyanRewrite => ("rewrite", "assistant-wenyan.md"),
        ManagedPromptKind::ToolChatAssistant => ("tool", "tool-chat-assistant.md"),
        ManagedPromptKind::ToolRouter => ("tool", "tool-router.md"),
        ManagedPromptKind::ToolNaturalLanguageBuilder => {
            ("tool", "natural-language-tool-builder.md")
        }
        ManagedPromptKind::AgentResponderSystem => ("agent", "responder-system.md"),
        ManagedPromptKind::AgentPlannerInput => ("agent", "planner-input.md"),
        ManagedPromptKind::AgentWorkItemExecution => ("agent", "work-item-execution.md"),
        ManagedPromptKind::ContextMemorySystem => ("context", "memory-system.md"),
        ManagedPromptKind::ContextMemoryUsagePolicy => ("context", "memory-usage-policy.md"),
        ManagedPromptKind::ContextLightweightChat => ("context", "lightweight-chat.md"),
        ManagedPromptKind::JsonSystemGuard => ("system", "json-guard.md"),
    };
    PathBuf::from("prompts").join(group).join(file_name)
}

fn managed_prompt_sidecar_relative_path(kind: ManagedPromptKind) -> PathBuf {
    let path = managed_prompt_relative_path(kind);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("prompt.md");
    path.with_file_name(format!("{}.meta.json", file_name.trim_end_matches(".md")))
}

fn managed_prompt_metadata(kind: ManagedPromptKind) -> ManagedPromptMetadata {
    match kind {
        ManagedPromptKind::MemoryOrganizer => ManagedPromptMetadata {
            id: "prompt.organizer.memory".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Memory Organizer".to_owned(),
            kind: "organizer".to_owned(),
            tags: vec![
                "memory".to_owned(),
                "organizer".to_owned(),
                "routing".to_owned(),
            ],
        },
        ManagedPromptKind::PromptAssetSynthesizer => ManagedPromptMetadata {
            id: "prompt.synthesis.prompt-assets".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Prompt Asset Synthesizer".to_owned(),
            kind: "synthesis".to_owned(),
            tags: vec![
                "prompt".to_owned(),
                "synthesis".to_owned(),
                "behavior".to_owned(),
            ],
        },
        ManagedPromptKind::SemanticTagSuggester => ManagedPromptMetadata {
            id: "prompt.extract.semantic-tags".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Semantic Tag Suggester".to_owned(),
            kind: "extract".to_owned(),
            tags: vec!["memory".to_owned(), "extract".to_owned(), "tags".to_owned()],
        },
        ManagedPromptKind::GlobalPreferenceSummary => ManagedPromptMetadata {
            id: "prompt.summarize.global-preference".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Global Preference Summary".to_owned(),
            kind: "summary".to_owned(),
            tags: vec![
                "memory".to_owned(),
                "global".to_owned(),
                "preference".to_owned(),
            ],
        },
        ManagedPromptKind::AssistantWenyanRewrite => ManagedPromptMetadata {
            id: "prompt.rewrite.assistant-wenyan".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Assistant Wenyan Rewrite".to_owned(),
            kind: "rewrite".to_owned(),
            tags: vec![
                "rewrite".to_owned(),
                "wenyan".to_owned(),
                "assistant".to_owned(),
            ],
        },
        ManagedPromptKind::ToolChatAssistant => ManagedPromptMetadata {
            id: "prompt.tool.chat-assistant".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Tool Chat Assistant".to_owned(),
            kind: "tool_chat".to_owned(),
            tags: vec!["tool".to_owned(), "chat".to_owned(), "assistant".to_owned()],
        },
        ManagedPromptKind::ToolRouter => ManagedPromptMetadata {
            id: "prompt.tool.router".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Tool Router".to_owned(),
            kind: "tool_router".to_owned(),
            tags: vec!["tool".to_owned(), "router".to_owned(), "json".to_owned()],
        },
        ManagedPromptKind::ToolNaturalLanguageBuilder => ManagedPromptMetadata {
            id: "prompt.tool.natural-language-builder".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Natural Language Tool Builder".to_owned(),
            kind: "tool_builder".to_owned(),
            tags: vec!["tool".to_owned(), "builder".to_owned(), "json".to_owned()],
        },
        ManagedPromptKind::AgentResponderSystem => ManagedPromptMetadata {
            id: "prompt.agent.responder-system".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Agent Responder System".to_owned(),
            kind: "agent_responder".to_owned(),
            tags: vec![
                "agent".to_owned(),
                "responder".to_owned(),
                "system".to_owned(),
            ],
        },
        ManagedPromptKind::AgentPlannerInput => ManagedPromptMetadata {
            id: "prompt.agent.planner-input".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Agent Planner Input".to_owned(),
            kind: "agent_planner".to_owned(),
            tags: vec!["agent".to_owned(), "planner".to_owned(), "json".to_owned()],
        },
        ManagedPromptKind::AgentWorkItemExecution => ManagedPromptMetadata {
            id: "prompt.agent.work-item-execution".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Agent Work Item Execution".to_owned(),
            kind: "agent_execution".to_owned(),
            tags: vec![
                "agent".to_owned(),
                "execution".to_owned(),
                "task".to_owned(),
            ],
        },
        ManagedPromptKind::ContextMemorySystem => ManagedPromptMetadata {
            id: "prompt.context.memory-system".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Context Memory System".to_owned(),
            kind: "context_memory".to_owned(),
            tags: vec![
                "context".to_owned(),
                "memory".to_owned(),
                "system".to_owned(),
            ],
        },
        ManagedPromptKind::ContextMemoryUsagePolicy => ManagedPromptMetadata {
            id: "prompt.context.memory-usage-policy".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Context Memory Usage Policy".to_owned(),
            kind: "context_policy".to_owned(),
            tags: vec![
                "context".to_owned(),
                "memory".to_owned(),
                "policy".to_owned(),
            ],
        },
        ManagedPromptKind::ContextLightweightChat => ManagedPromptMetadata {
            id: "prompt.context.lightweight-chat".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "Context Lightweight Chat".to_owned(),
            kind: "context_lightweight_chat".to_owned(),
            tags: vec![
                "context".to_owned(),
                "chat".to_owned(),
                "lightweight".to_owned(),
            ],
        },
        ManagedPromptKind::JsonSystemGuard => ManagedPromptMetadata {
            id: "prompt.system.json-guard".to_owned(),
            r#type: "prompt_template".to_owned(),
            title: "JSON System Guard".to_owned(),
            kind: "system_guard".to_owned(),
            tags: vec!["system".to_owned(), "json".to_owned(), "guard".to_owned()],
        },
    }
}

fn managed_prompt_default_body(kind: ManagedPromptKind) -> &'static str {
    match kind {
        ManagedPromptKind::MemoryOrganizer => {
            include_str!("../prompt-templates/organizer/memory-organizer.md")
        }
        ManagedPromptKind::PromptAssetSynthesizer => {
            include_str!("../prompt-templates/synthesis/prompt-asset-synthesizer.md")
        }
        ManagedPromptKind::SemanticTagSuggester => {
            include_str!("../prompt-templates/extract/semantic-tags.md")
        }
        ManagedPromptKind::GlobalPreferenceSummary => {
            include_str!("../prompt-templates/summarize/global-preference-summary.md")
        }
        ManagedPromptKind::AssistantWenyanRewrite => {
            include_str!("../prompt-templates/rewrite/assistant-wenyan.md")
        }
        ManagedPromptKind::ToolChatAssistant => {
            include_str!("../prompt-templates/tool/tool-chat-assistant.md")
        }
        ManagedPromptKind::ToolRouter => {
            include_str!("../prompt-templates/tool/tool-router.md")
        }
        ManagedPromptKind::ToolNaturalLanguageBuilder => {
            include_str!("../prompt-templates/tool/natural-language-tool-builder.md")
        }
        ManagedPromptKind::AgentResponderSystem => {
            include_str!("../prompt-templates/agent/responder-system.md")
        }
        ManagedPromptKind::AgentPlannerInput => {
            include_str!("../prompt-templates/agent/planner-input.md")
        }
        ManagedPromptKind::AgentWorkItemExecution => {
            include_str!("../prompt-templates/agent/work-item-execution.md")
        }
        ManagedPromptKind::ContextMemorySystem => {
            include_str!("../prompt-templates/context/memory-system.md")
        }
        ManagedPromptKind::ContextMemoryUsagePolicy => {
            include_str!("../prompt-templates/context/memory-usage-policy.md")
        }
        ManagedPromptKind::ContextLightweightChat => {
            include_str!("../prompt-templates/context/lightweight-chat.md")
        }
        ManagedPromptKind::JsonSystemGuard => {
            include_str!("../prompt-templates/system/json-guard.md")
        }
    }
}

fn parse_json_payload<T>(content: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    if let Ok(parsed) = serde_json::from_str::<T>(content.trim()) {
        return Ok(parsed);
    }

    if let Some(block) = extract_json_block(content) {
        return Ok(serde_json::from_str(block)?);
    }

    Err(anyhow::anyhow!("llm did not return valid json"))
}

fn extract_json_block(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    for (open, close) in [('{', '}'), ('[', ']')] {
        if let (Some(start), Some(end)) = (trimmed.find(open), trimmed.rfind(close))
            && start < end
        {
            return Some(&trimmed[start..=end]);
        }
    }
    None
}

fn prompt_asset_from_llm_item(
    memories: &[RetrievedMemory],
    index: usize,
    item: LlmPromptAssetItem,
) -> PromptAsset {
    let memory = item
        .source_memory_id
        .as_ref()
        .and_then(|id| memories.iter().find(|memory| &memory.id == id));
    let id = item
        .source_memory_id
        .clone()
        .or_else(|| memory.map(|memory| format!("prompt.asset.{}", memory.id)))
        .unwrap_or_else(|| format!("prompt.asset.synthetic.{index}"));
    let mut asset = PromptAsset::new(id, item.kind, item.title, item.content);
    for tag in item.tags {
        if !tag.trim().is_empty() {
            asset = asset.with_tag(tag);
        }
    }
    if let Some(memory) = memory {
        for tag in &memory.tags {
            if !asset
                .tags
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(tag))
            {
                asset.tags.push(tag.clone());
            }
        }
    }
    asset
}

fn default_prompt_asset_kind() -> PromptAssetKind {
    PromptAssetKind::PromptMemory
}

fn prompt_asset_stage_for_kind(kind: &PromptAssetKind) -> MemoryAssetStage {
    match kind {
        PromptAssetKind::PromptMemory => MemoryAssetStage::Procedural,
        PromptAssetKind::SystemPolicy
        | PromptAssetKind::BehaviorTemplate
        | PromptAssetKind::StyleGuide
        | PromptAssetKind::OutputContract => MemoryAssetStage::Compiled,
    }
}

fn prompt_asset_form_for_kind(kind: &PromptAssetKind) -> MemoryAssetForm {
    match kind {
        PromptAssetKind::SystemPolicy => MemoryAssetForm::Policy,
        PromptAssetKind::BehaviorTemplate
        | PromptAssetKind::StyleGuide
        | PromptAssetKind::OutputContract => MemoryAssetForm::Prompt,
        PromptAssetKind::PromptMemory => MemoryAssetForm::Policy,
    }
}

fn default_preference_memory_kind() -> MemoryKind {
    MemoryKind::Preference
}

fn memory_decision_from_llm_output(
    input: &MemoryOrganizationInput,
    fallback: MemoryOrganizationDecision,
    output: LlmMemoryOrganizationOutput,
) -> MemoryOrganizationDecision {
    let mut route = fallback.route;
    if let Some(room_layer) = output.room_layer {
        route.room_layer = room_layer;
    }
    if let Some(room_id) = output.room_id
        && !room_id.trim().is_empty()
    {
        route.room_id = room_id;
    }
    if let Some(title) = output.title
        && !title.trim().is_empty()
    {
        route.title = title;
    }
    if let Some(owner) = &input.owner
        && !route.owners.iter().any(|existing| existing == owner)
    {
        route.owners.push(owner.clone());
    }
    route.visibility = input.visibility.clone();

    let memory_kind = output.memory_kind.unwrap_or(fallback.memory_kind);
    let mut tags = fallback.tags;
    for tag in output.tags {
        if !tags
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&tag))
        {
            tags.push(tag);
        }
    }

    MemoryOrganizationDecision {
        route,
        memory_kind,
        tags,
        promotions: output.promotions,
    }
}

fn base_memory_decision_from_input(input: &MemoryOrganizationInput) -> MemoryOrganizationDecision {
    let room_layer = input.room_layer_hint.clone().unwrap_or(MemoryLayer::Chat);
    let room_id = input
        .room_id_hint
        .clone()
        .unwrap_or_else(|| format!("room.{}", slugify_for_memory(&input.namespace.user_id)));
    let mut owners = Vec::new();
    if let Some(owner) = &input.owner {
        owners.push(owner.clone());
    }

    MemoryOrganizationDecision {
        route: MemoryRoomRoute {
            room_id,
            room_layer,
            title: input
                .title_hint
                .clone()
                .unwrap_or_else(|| summarize_title_from_content(&input.content)),
            owners,
            visibility: input.visibility.clone(),
        },
        memory_kind: MemoryKind::Summary,
        tags: input.tags.clone(),
        promotions: Vec::new(),
    }
}

fn merged_prompt_assets(
    explicit_assets: &[PromptAsset],
    synthesized_assets: &[PromptAsset],
) -> Vec<PromptAsset> {
    let mut merged = explicit_assets.to_vec();
    for asset in synthesized_assets {
        if merged.iter().any(|existing| existing.id == asset.id) {
            continue;
        }
        merged.push(asset.clone());
    }
    merged
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
