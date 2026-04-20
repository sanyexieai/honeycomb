//! Lightweight memory + llm composition without depending on hc-agent.

use anyhow::Result;
use hc_capability::CapabilityProfile;
use hc_llm::{
    ChatMessage, GenerateRequest, GenerateResponse, LlmError, MessageRole, ProviderRegistry,
    StreamChunk,
};
use hc_memory::{
    MemoryCatalog, MemoryKind, MemoryLayer, MemoryNamespace, MemoryOwnerKind, MemoryOwnerRef,
    MemoryQuery, MemoryRecord, MemoryRepository, MemoryRoomAsset, MemoryRoomAssetKind,
    MemoryRoomRepository, MemoryScope,
};
use hc_persona::PersonaProfile;
use hc_store::store::{MarkdownQuery, WorkspaceNamespace, WorkspaceStore};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
pub struct PromptPolicy {
    pub title: String,
    pub content: String,
}

impl PromptPolicy {
    pub fn new(title: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptAsset {
    pub id: String,
    pub kind: PromptAssetKind,
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
            format!("{}{}", capability.description, domains).trim().to_owned(),
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
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextMemoryQuery {
    pub memory_query: MemoryQuery,
    pub limit: Option<usize>,
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
            if !tags.iter().any(|existing| existing.eq_ignore_ascii_case(&tag)) {
                tags.push(tag);
            }
        }
        let promotions = self
            .promotion_advisor
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
}

impl MemoryRetriever for WorkspaceMemoryRetriever {
    fn retrieve(&self, query: &ContextMemoryQuery) -> Result<Vec<RetrievedMemory>> {
        let store = WorkspaceStore::new(self.root.clone());
        let repository = MemoryRepository::with_namespace(self.root.clone(), self.namespace.clone());
        let room_repository =
            MemoryRoomRepository::with_namespace(self.root.clone(), self.namespace.clone());
        let _ = store.rebuild_markdown_index_in_namespace(&self.namespace)?;

        let mut record_markdown_query = MarkdownQuery::default().with_path_prefix("memory");
        if let Some(tag) = &query.memory_query.tag {
            record_markdown_query = record_markdown_query.with_tag(tag.clone());
        }
        if let Some(text) = &query.memory_query.text {
            record_markdown_query = record_markdown_query.with_text(text.clone());
        }

        let entries =
            store.query_markdown_index_in_namespace(&self.namespace, &record_markdown_query)?;
        let mut catalog = MemoryCatalog::new();
        for entry in entries {
            if !entry.relative_path.starts_with("memory/") || entry.doc_type != "memory" {
                continue;
            }
            let record = repository.read_record(&entry.relative_path)?;
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

        let room_entries =
            store.query_markdown_index_in_namespace(&self.namespace, &room_markdown_query)?;
        for entry in room_entries {
            let asset = room_repository.read_asset(&entry.relative_path)?;
            let retrieved = RetrievedMemory::from(&asset);
            if room_asset_matches_query(query, &asset, &retrieved) {
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
        if let Some(limit) = query.limit {
            matches.truncate(limit);
        }
        Ok(matches)
    }
}

impl MemoryRoomRouter for RuleBasedMemoryRoomRouter {
    fn route_room(&self, input: &MemoryOrganizationInput) -> Result<MemoryRoomRoute> {
        let (room_id, room_layer) =
            if let (Some(room_id), Some(room_layer)) = (&input.room_id_hint, &input.room_layer_hint)
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
        } else if contains_any(&content, &["fact", "knowledge", "reference", "architecture"]) {
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
        let mut tags = Vec::new();
        for (keyword, tag) in [
            ("runtime", "runtime"),
            ("assignment", "assignment"),
            ("planning", "planning"),
            ("memory", "memory"),
            ("stream", "streaming"),
            ("review", "review"),
            ("trace", "trace"),
        ] {
            if content.contains(keyword) && !tags.iter().any(|value| value == tag) {
                tags.push(tag.to_owned());
            }
        }
        Ok(tags)
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
            system_sections.push(format!(
                "Relevant recalled memory:\n{}",
                recalled
            ));
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
    let memories = retriever.retrieve(&request.memory_query)?;
    let messages = composer.compose_messages(
        request.system_prompt.as_deref(),
        request.self_model.as_ref(),
        &request.prompt_policies,
        &request.prompt_assets,
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
    })
}

pub fn generate_with_context_stream(
    registry: &ProviderRegistry,
    retriever: &impl MemoryRetriever,
    composer: &impl ContextComposer,
    request: &ContextRequest,
    on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
) -> Result<ContextResponse> {
    let memories = retriever.retrieve(&request.memory_query)?;
    let messages = composer.compose_messages(
        request.system_prompt.as_deref(),
        request.self_model.as_ref(),
        &request.prompt_policies,
        &request.prompt_assets,
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
    })
}

pub fn workspace_namespace_from_memory_namespace(namespace: &MemoryNamespace) -> WorkspaceNamespace {
    WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

pub fn default_workspace_root() -> &'static Path {
    Path::new("workspace")
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
    let mut asset = MemoryRoomAsset::new(
        asset_id,
        request.room_id.clone(),
        file_name,
        request.room_layer.clone(),
        MemoryRoomAssetKind::Compressed,
        request.title.clone(),
        request.summary.clone(),
    )
    .with_namespace(MemoryNamespace::new(namespace.tenant_id, namespace.user_id))
    .with_visibility(request.visibility.clone())
    .with_memory_kind(request.memory_kind.clone());

    for owner in &request.owners {
        asset = asset.with_owner(owner.clone());
    }
    for tag in &request.tags {
        asset = asset.with_tag(tag.clone());
    }
    for source in &request.derived_from {
        asset = asset.with_derived_from(source.clone());
    }
    for source_doc in &request.source_docs {
        asset = asset.with_source_doc(source_doc.clone());
    }

    repository.write_asset(&room, &asset)
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

fn pseudo_record_for_room_asset(asset: &MemoryRoomAsset) -> MemoryRecord {
    let mut record = MemoryRecord::new(
        asset.id.clone(),
        memory_scope_for_layer(&asset.layer),
        asset
            .owners
            .first()
            .cloned()
            .unwrap_or_else(|| MemoryOwnerRef::new(owner_kind_for_layer(&asset.layer), asset.room_id.clone())),
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

fn default_room_asset_file_name(request: &RoomMemoryWriteRequest) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis();
    format!(
        "min.{}.{}.md",
        millis,
        format!("{:?}", request.memory_kind).to_ascii_lowercase()
    )
}

fn default_room_asset_id(request: &RoomMemoryWriteRequest) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis();
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
    candidates.iter().any(|candidate| content.contains(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hc_capability::CapabilityProfile;
    use hc_llm::{MessageRole, ModelRef};
    use hc_memory::{
        MemoryKind, MemoryLayer, MemoryOwnerRef, MemoryRoom, MemoryRoomAsset,
        MemoryRoomAssetKind, MemoryRoomRepository, MemoryVisibility,
    };
    use hc_persona::{PersonaKind, PersonaLifecycle, PersonaNamespace, PersonaProfile};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("honeycomb-{}-{}-{}", name, std::process::id(), nanos))
    }

    #[test]
    fn composer_injects_memory_into_system_message() {
        let composer = DefaultContextComposer;
        let memories = vec![RetrievedMemory::from(&MemoryRecord::new(
            "memory.task.0001",
            MemoryScope::Task,
            MemoryOwnerRef::task("task.demo"),
            MemoryKind::Summary,
            "Task Summary",
            "Remember the prior implementation detail.",
        ))];

        let messages = composer.compose_messages(
            Some("You are helpful."),
            Some(
                &SelfModel::new(
                    "self.reviewer",
                    "Reviewer",
                    "reviewer",
                    "Reviews plans and implementation risks.",
                )
                .with_style("critical and careful")
                .with_goal("Find regressions quickly")
                .with_capability(SelfCapability::new(
                    "risk_review",
                    "Identify likely bugs and regressions",
                ))
                .with_constraint(SelfConstraint::new(
                    "no_invention",
                    "Do not invent facts not present in context",
                )),
            ),
            &[PromptPolicy::new("Safety", "Be precise and concise.")],
            &[PromptAsset::new(
                "prompt.asset.0001",
                PromptAssetKind::BehaviorTemplate,
                "Reviewer Style",
                "Prioritize risks and regressions.",
            )],
            &memories,
            &[ChatMessage::new(MessageRole::User, "continue")],
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert!(messages[0].content.contains("Self context"));
        assert!(messages[0].content.contains("role: reviewer"));
        assert!(messages[0].content.contains("risk_review"));
        assert!(messages[0].content.contains("Prompt policies"));
        assert!(messages[0].content.contains("Be precise and concise."));
        assert!(messages[0].content.contains("Prompt assets"));
        assert!(messages[0].content.contains("Prioritize risks and regressions."));
        assert!(messages[0].content.contains("Relevant recalled memory"));
        assert!(messages[0].content.contains("Task Summary"));
        assert!(messages[0].content.contains("source=memory_record"));
        assert!(messages[0].content.contains("kind=Summary"));
    }

    #[test]
    fn workspace_retriever_reads_memory_records_from_workspace() {
        let root = unique_temp_dir("context-retriever");
        let namespace = MemoryNamespace::new("tenant-a", "user-a");
        let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
        let repository = MemoryRepository::with_namespace(&root, workspace_namespace.clone());
        let record = MemoryRecord::new(
            "memory.task.0002",
            MemoryScope::Task,
            MemoryOwnerRef::task("task.demo"),
            MemoryKind::Decision,
            "Runtime Decision",
            "Persist explicit assignment decisions.",
        )
        .with_namespace(namespace.clone())
        .with_visibility(MemoryVisibility::Private)
        .with_tag("runtime")
        .with_confidence_milli(920);
        repository
            .write_record(&record)
            .expect("memory record should be written");

        let retriever = WorkspaceMemoryRetriever::new(&root, workspace_namespace);
        let matches = retriever
            .retrieve(
                &ContextMemoryQuery::default()
                    .for_namespace(namespace)
                    .with_scope(MemoryScope::Task)
                    .with_tag("runtime")
                    .with_text("assignment")
                    .with_limit(5),
            )
            .expect("memory retrieval should succeed");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "memory.task.0002");
        assert_eq!(matches[0].source_kind, "memory_record");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_retriever_prefers_room_compressed_assets() {
        let root = unique_temp_dir("context-room-retriever");
        let namespace = MemoryNamespace::new("tenant-a", "user-a");
        let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
        let room_repository = MemoryRoomRepository::with_namespace(&root, workspace_namespace.clone());
        let room = MemoryRoom::new(
            "room.task.runtime-refactor.0001",
            MemoryLayer::Task,
            "Runtime Refactor Task Room",
            "Tracks planning and execution memory.",
        )
        .with_namespace(namespace.clone())
        .with_tag("runtime");
        room_repository
            .write_room(&room)
            .expect("memory room should be written");
        let asset = MemoryRoomAsset::new(
            "asset.room.task.runtime-refactor.0001.summary",
            room.id.clone(),
            "min.0001.summary.md",
            MemoryLayer::Task,
            MemoryRoomAssetKind::Compressed,
            "Runtime Refactor Summary",
            "Persist task plans and assignment decisions together.",
        )
        .with_namespace(namespace.clone())
        .with_memory_kind(MemoryKind::Decision)
        .with_owner(MemoryOwnerRef::task("task.demo"))
        .with_tag("runtime")
        .with_tag("task.demo");
        room_repository
            .write_asset(&room, &asset)
            .expect("compressed room asset should be written");

        let repository = MemoryRepository::with_namespace(&root, workspace_namespace.clone());
        let record = MemoryRecord::new(
            "memory.task.0002",
            MemoryScope::Task,
            MemoryOwnerRef::task("task.demo"),
            MemoryKind::Summary,
            "Legacy Task Summary",
            "Older flat memory record for the same task.",
        )
        .with_namespace(namespace.clone())
        .with_visibility(MemoryVisibility::Private)
        .with_tag("runtime")
        .with_confidence_milli(700);
        repository
            .write_record(&record)
            .expect("memory record should be written");

        let retriever = WorkspaceMemoryRetriever::new(&root, workspace_namespace);
        let matches = retriever
            .retrieve(
                &ContextMemoryQuery::default()
                    .for_namespace(namespace)
                    .with_scope(MemoryScope::Task)
                    .with_tag("runtime")
                    .with_text("assignment")
                    .with_limit(1),
            )
            .expect("memory retrieval should succeed");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "asset.room.task.runtime-refactor.0001.summary");
        assert_eq!(matches[0].source_kind, "room_compressed");
        assert_eq!(matches[0].kind, MemoryKind::Decision);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn request_holds_generation_and_memory_query() {
        let generation = GenerateRequest::new(
            ModelRef::new("openai", "gpt-4.1-mini"),
            vec![ChatMessage::new(MessageRole::User, "hello")],
        );
        let request = ContextRequest::new(generation)
            .with_self_model(SelfModel::new(
                "self.assistant",
                "Assistant",
                "helper",
                "Supports the user with concise execution.",
            ))
            .with_system_prompt("Use recalled context")
            .with_memory_query(
                ContextMemoryQuery::default()
                    .with_text("hello")
                    .with_limit(3),
            );

        assert_eq!(request.memory_query.limit, Some(3));
        assert_eq!(request.system_prompt.as_deref(), Some("Use recalled context"));
        assert_eq!(request.self_model.as_ref().map(|model| model.role.as_str()), Some("helper"));
    }

    #[test]
    fn persist_room_memory_writes_compressed_asset() {
        let root = unique_temp_dir("context-room-writeback");
        let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
        let request = RoomMemoryWriteRequest::new(
            "room.task.runtime-refactor.0001",
            MemoryLayer::Task,
            "Assignment Decision",
            "Persist the assignment decision as a compressed room asset.",
            MemoryKind::Decision,
        )
        .with_owner(MemoryOwnerRef::task("task.demo"))
        .with_tag("runtime")
        .with_file_name("min.assignment.md")
        .with_asset_id("asset.room.task.runtime-refactor.0001.decision");

        let path = persist_room_memory(&root, namespace.clone(), &request)
            .expect("room memory asset should be written");
        assert!(path.exists());

        let repository = MemoryRoomRepository::with_namespace(&root, namespace);
        let relative = PathBuf::from("memory/rooms/task/room.task.runtime-refactor.0001/compressed/min.assignment.md");
        let loaded = repository
            .read_asset(relative)
            .expect("room memory asset should roundtrip");

        assert_eq!(loaded.id, "asset.room.task.runtime-refactor.0001.decision");
        assert_eq!(loaded.memory_kind, MemoryKind::Decision);
        assert!(loaded
            .owners
            .iter()
            .any(|owner| owner == &MemoryOwnerRef::task("task.demo")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rule_based_organizer_routes_task_owner_into_task_room() {
        let organizer = CompositeMemoryOrganizer::new(
            RuleBasedMemoryRoomRouter,
            RuleBasedMemoryKindResolver,
            KeywordMemoryTagSuggester,
            NoopMemoryPromotionAdvisor,
        );
        let input = MemoryOrganizationInput::new(
            MemoryNamespace::new("tenant-a", "user-a"),
            "Assignment decision for runtime planning and memory flow.",
        )
        .with_title_hint("Assignment Decision")
        .with_owner(MemoryOwnerRef::task("task.demo"))
        .with_tag("manual");

        let decision = organizer
            .organize(&input)
            .expect("organization should succeed");

        assert_eq!(decision.route.room_layer, MemoryLayer::Task);
        assert_eq!(decision.route.room_id, "room.task.task.demo");
        assert_eq!(decision.memory_kind, MemoryKind::Decision);
        assert!(decision.tags.iter().any(|tag| tag == "manual"));
        assert!(decision.tags.iter().any(|tag| tag == "assignment"));
        assert!(decision.tags.iter().any(|tag| tag == "runtime"));
    }

    #[test]
    fn organization_can_be_converted_to_room_write_request() {
        let decision = MemoryOrganizationDecision {
            route: MemoryRoomRoute {
                room_id: "room.project.honeycomb".to_owned(),
                room_layer: MemoryLayer::Project,
                title: "Honeycomb Architecture".to_owned(),
                owners: vec![MemoryOwnerRef::project("project.honeycomb")],
                visibility: MemoryVisibility::TenantShared,
            },
            memory_kind: MemoryKind::Knowledge,
            tags: vec!["runtime".to_owned(), "architecture".to_owned()],
            promotions: Vec::new(),
        };

        let request = room_memory_write_request_from_organization(
            &decision,
            "Shared project architecture note.",
        );

        assert_eq!(request.room_id, "room.project.honeycomb");
        assert_eq!(request.room_layer, MemoryLayer::Project);
        assert_eq!(request.memory_kind, MemoryKind::Knowledge);
        assert_eq!(request.visibility, MemoryVisibility::TenantShared);
        assert!(request
            .owners
            .iter()
            .any(|owner| owner == &MemoryOwnerRef::project("project.honeycomb")));
        assert!(request.tags.iter().any(|tag| tag == "architecture"));
    }

    #[test]
    fn prompt_asset_can_be_derived_from_memory() {
        let memory = RetrievedMemory::from(&MemoryRecord::new(
            "memory.global.preference.0001",
            MemoryScope::Global,
            MemoryOwnerRef::global(),
            MemoryKind::Preference,
            "Writing Preference",
            "Respond in concise Chinese with markdown when useful.",
        )
        .with_tag("style"));

        let asset = prompt_asset_from_memory(
            &memory,
            PromptAssetKind::StyleGuide,
            "User Writing Style",
        );

        assert_eq!(asset.id, memory.id);
        assert_eq!(asset.kind, PromptAssetKind::StyleGuide);
        assert!(asset.content.contains("concise Chinese"));
        assert!(asset.tags.iter().any(|tag| tag == "style"));
    }

    #[test]
    fn self_model_can_be_built_from_persona_and_capabilities() {
        let persona = PersonaProfile::new(
            "persona.agent.reviewer",
            PersonaNamespace::new("tenant-a", "user-a"),
            PersonaKind::Agent,
            PersonaLifecycle::Stable,
            "Reviewer",
            "reviewer",
        );
        let mut persona = persona;
        persona.description = "Reviews plans and implementation details.".to_owned();
        persona.style = "critical and careful".to_owned();
        persona.goals = vec![
            "Find regressions quickly".to_owned(),
            "Protect long-term maintainability".to_owned(),
        ];

        let capability = CapabilityProfile::new("capability.review", "Code Review")
            .with_description("Review code and identify risks")
            .with_domain("rust")
            .with_constraint("Avoid inventing behavior not present in code");

        let self_model = self_model_from_persona_and_capabilities(&persona, &[capability]);

        assert_eq!(self_model.id, "persona.agent.reviewer");
        assert_eq!(self_model.role, "reviewer");
        assert_eq!(self_model.style.as_deref(), Some("critical and careful"));
        assert!(self_model
            .goals
            .iter()
            .any(|goal| goal == "Find regressions quickly"));
        assert!(self_model
            .capabilities
            .iter()
            .any(|capability| capability.name == "Code Review"));
        assert!(self_model
            .constraints
            .iter()
            .any(|constraint| constraint.description.contains("Avoid inventing behavior")));
    }
}
