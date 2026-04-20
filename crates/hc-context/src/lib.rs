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
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
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
    fallback: F,
    fallback_on_error: bool,
}

#[derive(Clone)]
pub struct LlmMemoryOrganizer<'a, F> {
    registry: &'a ProviderRegistry,
    model: hc_llm::ModelRef,
    fallback: F,
    fallback_on_error: bool,
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
    pub fn new(registry: &'a ProviderRegistry, model: hc_llm::ModelRef, fallback: F) -> Self {
        Self {
            registry,
            model,
            fallback,
            fallback_on_error: true,
        }
    }

    pub fn strict(registry: &'a ProviderRegistry, model: hc_llm::ModelRef, fallback: F) -> Self {
        Self {
            registry,
            model,
            fallback,
            fallback_on_error: false,
        }
    }
}

impl<'a, F> LlmMemoryOrganizer<'a, F> {
    pub fn new(registry: &'a ProviderRegistry, model: hc_llm::ModelRef, fallback: F) -> Self {
        Self {
            registry,
            model,
            fallback,
            fallback_on_error: true,
        }
    }

    pub fn strict(registry: &'a ProviderRegistry, model: hc_llm::ModelRef, fallback: F) -> Self {
        Self {
            registry,
            model,
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

    pub fn discover_room_candidates(&self, query: &ContextMemoryQuery) -> Result<Vec<RoomCandidate>> {
        discover_room_candidates(&self.root, &self.namespace, query)
    }
}

impl MemoryRetriever for WorkspaceMemoryRetriever {
    fn retrieve(&self, query: &ContextMemoryQuery) -> Result<Vec<RetrievedMemory>> {
        let store = WorkspaceStore::new(self.root.clone());
        let repository = MemoryRepository::with_namespace(self.root.clone(), self.namespace.clone());
        let room_repository =
            MemoryRoomRepository::with_namespace(self.root.clone(), self.namespace.clone());
        let _ = store.rebuild_markdown_index_in_namespace(&self.namespace)?;
        let room_candidates = self.discover_room_candidates(query)?;
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
        let mut seen_match_ids = matches
            .iter()
            .map(|memory| memory.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for entry in room_entries {
            let asset = room_repository.read_asset(&entry.relative_path)?;
            let mut retrieved = RetrievedMemory::from(&asset);
            if let Some(boost) = room_candidate_boosts.get(&asset.room_id) {
                retrieved.confidence_milli = retrieved.confidence_milli.saturating_add(*boost / 4).min(1000);
            }
            if room_asset_matches_query(query, &asset, &retrieved) {
                seen_match_ids.insert(retrieved.id.clone());
                matches.push(retrieved);
            }
        }

        for candidate in room_candidates.iter().filter(|candidate| {
            candidate.score_milli >= 700
                && candidate
                    .reasons
                    .iter()
                    .any(|reason| reason == "anchor-room" || reason == "anchor-related")
        }) {
            let Some((room, _)) = read_room_by_id(&store, &room_repository, &self.namespace, &candidate.room_id)? else {
                continue;
            };
            let assets = room_repository.read_compressed_assets(&room)?;
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
        Ok(matches)
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
        } else if room_id.starts_with("room.topic.") || memory.tags.iter().any(|tag| tag == "topic") {
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

pub fn discover_room_candidates(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    query: &ContextMemoryQuery,
) -> Result<Vec<RoomCandidate>> {
    let root = root.as_ref().to_path_buf();
    let store = WorkspaceStore::new(root.clone());
    let room_repository = MemoryRoomRepository::with_namespace(root, namespace.clone());
    let mut markdown_query = MarkdownQuery::default()
        .with_path_prefix("memory/rooms")
        .with_doc_type("memory_room");

    if let Some(text) = &query.memory_query.text {
        markdown_query = markdown_query.with_text(text.clone());
    }
    if let Some(tag) = &query.memory_query.tag {
        markdown_query = markdown_query.with_tag(tag.clone());
    }

    let entries = store.query_markdown_index_in_namespace(namespace, &markdown_query)?;
    let mut candidates = Vec::new();
    let mut seen_room_ids = std::collections::BTreeSet::new();
    let mut related_room_ids = BTreeMap::<String, (u16, Vec<String>)>::new();

    for entry in entries {
        let room = room_repository.read_room(&entry.relative_path)?;
        let modified_at = modified_time_for_relative_path(&store, namespace, &entry.relative_path);
        collect_related_room_ids(
            &room,
            120,
            "related-room",
            &mut related_room_ids,
        );
        seen_room_ids.insert(room.id.clone());
        candidates.push(build_room_candidate(&room, query, modified_at, 0, Vec::new()));
    }

    for room_id in &query.room_anchor_ids {
        if seen_room_ids.contains(room_id) {
            continue;
        }
        let Some((room, modified_at)) = read_room_by_id(&store, &room_repository, namespace, room_id)? else {
            continue;
        };
        collect_related_room_ids(
            &room,
            220,
            "anchor-related",
            &mut related_room_ids,
        );
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
        let Some((room, modified_at)) =
            read_room_by_id(&store, &room_repository, namespace, &related_room_id)?
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
    room_id: &str,
) -> Result<Option<(hc_memory::MemoryRoom, SystemTime)>> {
    let query = MarkdownQuery::default()
        .with_path_prefix("memory/rooms")
        .with_doc_type("memory_room")
        .with_id(room_id.to_owned())
        .with_limit(1);
    let Some(entry) = store
        .query_markdown_index_in_namespace(namespace, &query)?
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
        let haystack = format!("{} {} {}", room.title, room.summary, room.tags.join(" "))
            .to_ascii_lowercase();
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
            "tool",
            "api",
            "git",
            "cargo",
            "minimax",
            "openai",
            "工具",
            "命令",
            "接口",
            "sdk",
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

    keywords.iter().any(|keyword| lowered_query.contains(keyword))
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
            &[
                "????",
                "????",
                "????",
                "be concise",
                "shorter answers",
            ],
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
            Err(error) if self.fallback_on_error => self.fallback.synthesize(memories).or(Err(error)),
            Err(error) => Err(error),
        }
    }
}

impl<'a, F> LlmPromptAssetSynthesizer<'a, F>
where
    F: PromptAssetSynthesizer,
{
    fn try_synthesize(&self, memories: &[RetrievedMemory]) -> Result<Vec<PromptAsset>> {
        let response = self
            .registry
            .generate(&GenerateRequest {
                model: self.model.clone(),
                messages: vec![
                    ChatMessage::new(
                        MessageRole::System,
                        "You convert durable recalled memories into prompt assets. Return JSON only.",
                    ),
                    ChatMessage::new(
                        MessageRole::User,
                        format!(
                            "{}\n\nMemories JSON:\n{}",
                            llm_prompt_asset_synthesizer_instructions(),
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
        let response = self
            .registry
            .generate(&GenerateRequest {
                model: self.model.clone(),
                messages: vec![
                    ChatMessage::new(
                        MessageRole::System,
                        "You organize memory writes into rooms and promotions. Return JSON only.",
                    ),
                    ChatMessage::new(
                        MessageRole::User,
                        format!(
                            "{}\n\nInput JSON:\n{}",
                            llm_memory_organizer_instructions(),
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
    let synthesizer = LlmPromptAssetSynthesizer::new(
        registry,
        request.generation.model.clone(),
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
    let synthesized_prompt_assets = synthesizer.synthesize(&memories)?;
    let prompt_assets = merged_prompt_assets(&request.prompt_assets, &synthesized_prompt_assets);
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
    })
}

pub fn generate_with_context_stream(
    registry: &ProviderRegistry,
    retriever: &impl MemoryRetriever,
    composer: &impl ContextComposer,
    request: &ContextRequest,
    on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
) -> Result<ContextResponse> {
    let synthesizer = LlmPromptAssetSynthesizer::new(
        registry,
        request.generation.model.clone(),
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
    let synthesized_prompt_assets = synthesizer.synthesize(&memories)?;
    let prompt_assets = merged_prompt_assets(&request.prompt_assets, &synthesized_prompt_assets);
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

pub fn summarize_global_preference(input: &MemoryOrganizationInput) -> Option<(String, MemoryKind)> {
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
        &[
            "????",
            "????",
            "????",
            "be concise",
            "shorter answers",
        ],
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
    let response = registry
        .generate(&GenerateRequest {
            model: model.clone(),
            messages: vec![
                ChatMessage::new(
                    MessageRole::System,
                    "You summarize durable user preferences into compact memory entries. Return JSON only.",
                ),
                ChatMessage::new(
                    MessageRole::User,
                    format!(
                        "{}\n\nInput JSON:\n{}",
                        llm_global_preference_summary_instructions(),
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

fn detect_assistant_name_preference(content: &str) -> Option<String> {
    for marker in ["\u{4f60}\u{4ee5}\u{540e}\u{53eb}", "\u{4ee5}\u{540e}\u{53eb}\u{4f60}", "\u{4ee5}\u{540e}\u{4f60}\u{53eb}", "call you "] {
        if let Some(rest) = content.split_once(marker).map(|(_, rest)| rest.trim()) {
            let candidate = rest
                .trim_matches(|character: char| character.is_ascii_punctuation() || character.is_whitespace())
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|character: char| character.is_ascii_punctuation() || character.is_whitespace());
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
        &["concise", "style", "language", "中文", "markdown", "shorter"],
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

fn llm_prompt_asset_synthesizer_instructions() -> &'static str {
    "Turn the recalled memories into prompt assets that should shape future model behavior.\n\
Only produce assets for durable instruction-like memories such as user preferences, naming, language, style, output constraints, or other long-lived behavior guidance.\n\
Prefer zero assets over low-confidence assets.\n\
Return strict JSON with this schema:\n\
{\"assets\":[{\"source_memory_id\":\"optional memory id\",\"kind\":\"system_policy|behavior_template|style_guide|output_contract|prompt_memory\",\"title\":\"short title\",\"content\":\"instruction text for the model\",\"tags\":[\"tag\"]}]}"
}

fn llm_memory_organizer_instructions() -> &'static str {
    "Decide how to organize this memory write.\n\
Honor any explicit room_id_hint, room_layer_hint, owner, visibility, and tags when they are present.\n\
Only suggest promotions when the content should persist beyond the current room, especially global user preferences.\n\
Return strict JSON with this schema:\n\
{\"room_layer\":\"chat|topic|task|project|global|null\",\"room_id\":\"optional room id\",\"title\":\"optional title\",\"memory_kind\":\"summary|decision|preference|workflow_memory|knowledge|null\",\"tags\":[\"tag\"],\"promotions\":[{\"target_layer\":\"chat|topic|task|project|global\",\"target_room_id\":\"optional room id\",\"reason\":\"why\"}]}"
}

fn llm_global_preference_summary_instructions() -> &'static str {
    "Read the input and decide whether it contains a durable user preference that should be stored as global memory.\n\
If it does, return a short factual summary in third person, suitable for future recall.\n\
The memory_kind should usually be \"preference\".\n\
Return strict JSON with this schema:\n\
{\"summary\":\"short durable preference summary\",\"memory_kind\":\"preference|summary|knowledge|workflow_memory|decision\"}"
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
            if !asset.tags.iter().any(|existing| existing.eq_ignore_ascii_case(tag)) {
                asset.tags.push(tag.clone());
            }
        }
    }
    asset
}

fn default_prompt_asset_kind() -> PromptAssetKind {
    PromptAssetKind::PromptMemory
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
        if !tags.iter().any(|existing| existing.eq_ignore_ascii_case(&tag)) {
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
mod tests {
    use super::*;
    use hc_capability::CapabilityProfile;
    use hc_llm::{
        FinishReason, GenerateResponse, LlmProvider, MessageRole, ModelRef, ProviderInfo,
        ProviderRegistry,
    };
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

    struct StaticProvider {
        id: String,
        response_text: String,
    }

    impl StaticProvider {
        fn new(id: &str, response_text: &str) -> Self {
            Self {
                id: id.to_owned(),
                response_text: response_text.to_owned(),
            }
        }
    }

    impl LlmProvider for StaticProvider {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.id.clone(),
                display_name: "Static Test Provider".to_owned(),
                supports_chat: true,
                supports_streaming: false,
            }
        }

        fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, hc_llm::LlmError> {
            Ok(GenerateResponse {
                model: request.model.clone(),
                message: ChatMessage::new(MessageRole::Assistant, self.response_text.clone()),
                finish_reason: FinishReason::Stop,
                usage: None,
                raw: None,
            })
        }
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
    fn default_prompt_asset_synthesizer_turns_global_preference_into_prompt_asset() {
        let memory = RetrievedMemory {
            id: "asset.room.global.local.default.pref-name".to_owned(),
            title: "Global Preference".to_owned(),
            summary: "User prefers the assistant to be called 小八.".to_owned(),
            scope: MemoryScope::Global,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Global),
            room_id: Some("room.global.local.default".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 980,
            tags: vec!["global".to_owned(), "preference".to_owned()],
        };

        let assets = DefaultPromptAssetSynthesizer
            .synthesize(&[memory])
            .expect("synthesis should succeed");

        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].kind, PromptAssetKind::BehaviorTemplate);
        assert!(assets[0].content.contains("小八"));
    }

    #[test]
    fn llm_prompt_asset_synthesizer_prefers_llm_output() {
        let mut registry = ProviderRegistry::new();
        registry.register(StaticProvider::new(
            "test",
            r#"```json
{"assets":[{"source_memory_id":"memory.global.preference.0001","kind":"behavior_template","title":"Assistant Name","content":"Call yourself 小八 when addressing the user.","tags":["global","identity"]}]}
```"#,
        ));
        let synthesizer = LlmPromptAssetSynthesizer::new(
            &registry,
            ModelRef::new("test", "mock"),
            DefaultPromptAssetSynthesizer,
        );
        let memory = RetrievedMemory::from(&MemoryRecord::new(
            "memory.global.preference.0001",
            MemoryScope::Global,
            MemoryOwnerRef::global(),
            MemoryKind::Preference,
            "Assistant Naming",
            "User prefers the assistant to be called 小八.",
        ));

        let assets = synthesizer
            .synthesize(&[memory])
            .expect("llm synthesizer should succeed");

        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].kind, PromptAssetKind::BehaviorTemplate);
        assert!(assets[0].content.contains("小八"));
        assert!(assets[0].tags.iter().any(|tag| tag == "identity"));
    }

    #[test]
    fn llm_prompt_asset_synthesizer_tolerates_missing_assets_field() {
        let mut registry = ProviderRegistry::new();
        registry.register(StaticProvider::new("test", r#"{"note":"no prompt assets"}"#));
        let synthesizer = LlmPromptAssetSynthesizer::strict(
            &registry,
            ModelRef::new("test", "mock"),
            DefaultPromptAssetSynthesizer,
        );
        let memory = RetrievedMemory::from(&MemoryRecord::new(
            "memory.global.preference.0001",
            MemoryScope::Global,
            MemoryOwnerRef::global(),
            MemoryKind::Preference,
            "Assistant Naming",
            "User prefers the assistant to be called 小八.",
        ));

        let assets = synthesizer
            .synthesize(&[memory])
            .expect("missing assets should be treated as no assets");

        assert!(assets.is_empty());
    }

    #[test]
    fn composer_renders_synthesized_prompt_assets() {
        let composer = DefaultContextComposer;
        let memory = RetrievedMemory {
            id: "asset.room.global.local.default.pref-language".to_owned(),
            title: "Global Preference".to_owned(),
            summary: "User prefers responses in Chinese.".to_owned(),
            scope: MemoryScope::Global,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Global),
            room_id: Some("room.global.local.default".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 980,
            tags: vec!["global".to_owned(), "preference".to_owned()],
        };
        let synthesized = DefaultPromptAssetSynthesizer
            .synthesize(&[memory.clone()])
            .expect("synthesis should succeed");

        let messages = composer.compose_messages(
            Some("You are helpful."),
            None,
            &[],
            &synthesized,
            &[memory],
            &[ChatMessage::new(MessageRole::User, "继续")],
        );

        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.contains("Prompt assets"));
        assert!(messages[0].content.contains("User prefers responses in Chinese."));
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

    #[test]
    fn rule_based_promotion_advisor_detects_global_name_preference() {
        let advisor = RuleBasedMemoryPromotionAdvisor;
        let input = MemoryOrganizationInput::new(
            MemoryNamespace::new("local", "default"),
            "\u{4f60}\u{4ee5}\u{540e}\u{53eb}\u{5c0f}\u{516b}",
        );
        let route = MemoryRoomRoute {
            room_id: "room.chat.local.default.1".to_owned(),
            room_layer: MemoryLayer::Chat,
            title: "Chat Room".to_owned(),
            owners: vec![MemoryOwnerRef::session("room.chat.local.default.1")],
            visibility: MemoryVisibility::Private,
        };

        let promotions = advisor
            .suggest_promotions(&input, &route, MemoryKind::Preference)
            .expect("promotion suggestions should succeed");

        assert_eq!(promotions.len(), 1);
        assert_eq!(promotions[0].target_layer, MemoryLayer::Global);
        assert_eq!(
            promotions[0].target_room_id.as_deref(),
            Some("room.global.local.default")
        );

        let summary = summarize_global_preference(&input)
            .expect("name preference summary should be generated");
        assert!(summary.0.contains("小八"));
        assert_eq!(summary.1, MemoryKind::Preference);
    }


    #[test]
    fn llm_memory_organizer_prefers_llm_output() {
        let mut registry = ProviderRegistry::new();
        registry.register(StaticProvider::new(
            "test",
            r#"{"room_layer":"chat","room_id":"room.chat.local.default.1","title":"Chat Room","memory_kind":"preference","tags":["chat","identity"],"promotions":[{"target_layer":"global","target_room_id":"room.global.local.default","reason":"assistant naming should persist"}]}"#,
        ));
        let fallback = CompositeMemoryOrganizer::new(
            RuleBasedMemoryRoomRouter,
            RuleBasedMemoryKindResolver,
            KeywordMemoryTagSuggester,
            RuleBasedMemoryPromotionAdvisor,
        );
        let organizer = LlmMemoryOrganizer::new(&registry, ModelRef::new("test", "mock"), fallback);
        let input = MemoryOrganizationInput::new(
            MemoryNamespace::new("local", "default"),
            "??????",
        )
        .with_room_hint("room.chat.local.default.1", MemoryLayer::Chat)
        .with_owner(MemoryOwnerRef::session("room.chat.local.default.1"));

        let decision = organizer
            .organize(&input)
            .expect("llm organizer should succeed");

        assert_eq!(decision.memory_kind, MemoryKind::Preference);
        assert!(decision.tags.iter().any(|tag| tag == "identity"));
        assert_eq!(decision.promotions.len(), 1);
        assert_eq!(decision.promotions[0].target_layer, MemoryLayer::Global);
        assert_eq!(
            decision.promotions[0].target_room_id.as_deref(),
            Some("room.global.local.default")
        );
    }

    #[test]
    fn llm_global_preference_summary_uses_llm_output() {
        let mut registry = ProviderRegistry::new();
        registry.register(StaticProvider::new(
            "test",
            r#"{"summary":"User prefers the assistant to be called 小八.","memory_kind":"preference"}"#,
        ));
        let input = MemoryOrganizationInput::new(
            MemoryNamespace::new("local", "default"),
            "你以后叫小八",
        );

        let summary = summarize_global_preference_with_llm(
            &registry,
            &ModelRef::new("test", "mock"),
            &input,
        )
        .expect("llm summary should succeed")
        .expect("summary should be present");

        assert_eq!(summary.1, MemoryKind::Preference);
        assert!(summary.0.contains("小八"));
    }

}
