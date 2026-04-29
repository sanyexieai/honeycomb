use super::*;
use hc_capability::CapabilityProfile;
use hc_llm::{
    FinishReason, GenerateResponse, LlmProvider, MessageRole, ModelRef, ProviderInfo,
    ProviderRegistry,
};
use hc_memory::{
    MemoryKind, MemoryLayer, MemoryOwnerRef, MemoryRoom, MemoryRoomAsset, MemoryRoomAssetKind,
    MemoryRoomRepository, MemoryVisibility,
};
use hc_persona::{PersonaKind, PersonaLifecycle, PersonaNamespace, PersonaProfile};
use hc_store::store::WorkspaceStore;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "honeycomb-{}-{}-{}",
        name,
        std::process::id(),
        nanos
    ))
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

struct AssertingProvider {
    id: String,
    required_substring: String,
    response_text: String,
}

impl AssertingProvider {
    fn new(id: &str, required_substring: &str, response_text: &str) -> Self {
        Self {
            id: id.to_owned(),
            required_substring: required_substring.to_owned(),
            response_text: response_text.to_owned(),
        }
    }
}

impl LlmProvider for AssertingProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            display_name: "Asserting Test Provider".to_owned(),
            supports_chat: true,
            supports_streaming: false,
        }
    }

    fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, hc_llm::LlmError> {
        let system_message = request
            .messages
            .iter()
            .find(|message| message.role == MessageRole::System)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        assert!(
            system_message.contains(&self.required_substring),
            "expected system message to contain {:?}, got {:?}",
            self.required_substring,
            system_message
        );
        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage::new(MessageRole::Assistant, self.response_text.clone()),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        })
    }
}

#[derive(Debug, Clone, Default)]
struct StaticMemoryRetriever {
    memories: Vec<RetrievedMemory>,
}

impl MemoryRetriever for StaticMemoryRetriever {
    fn retrieve(&self, _query: &ContextMemoryQuery) -> Result<Vec<RetrievedMemory>> {
        Ok(self.memories.clone())
    }
}

#[derive(Debug, Clone, Default)]
struct NoopPromptAssetSynthesizer;

impl PromptAssetSynthesizer for NoopPromptAssetSynthesizer {
    fn synthesize(&self, _memories: &[RetrievedMemory]) -> Result<Vec<PromptAsset>> {
        Ok(Vec::new())
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
    assert!(
        messages[0]
            .content
            .contains("Prioritize risks and regressions.")
    );
    assert!(messages[0].content.contains("Relevant recalled memory"));
    assert!(messages[0].content.contains("Task Summary"));
    assert!(messages[0].content.contains("source=memory_record"));
    assert!(messages[0].content.contains("kind=Summary"));
}

#[test]
fn generate_with_context_uses_compiled_prompt_assets_from_asset_views() {
    let mut registry = ProviderRegistry::new();
    registry.register(AssertingProvider::new(
        "test",
        "Preserve the repo's established patterns.",
        "ok",
    ));
    let retriever = StaticMemoryRetriever {
        memories: vec![RetrievedMemory {
            id: "asset.room.project.honeycomb.prompt.style".to_owned(),
            title: "Honeycomb Style Guide".to_owned(),
            summary: "Preserve the repo's established patterns.".to_owned(),
            scope: MemoryScope::Project,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Project),
            room_id: Some("room.project.honeycomb".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 980,
            tags: vec![
                "project".to_owned(),
                "prompt".to_owned(),
                "styleguide".to_owned(),
            ],
            derived_from: Vec::new(),
        }],
    };
    let composer = DefaultContextComposer;
    let request = ContextRequest::new(GenerateRequest::new(
        ModelRef::new("test", "mock"),
        vec![ChatMessage::new(MessageRole::User, "continue")],
    ));

    let response = generate_with_context_using_synthesizer(
        &registry,
        &retriever,
        &composer,
        &NoopPromptAssetSynthesizer,
        &request,
    )
    .expect("generation should succeed");

    assert!(response.synthesized_prompt_assets.is_empty());
    assert_eq!(response.response.message.content, "ok");
}

#[test]
fn prompt_policy_defaults_to_hard_runtime_policy() {
    let policy = PromptPolicy::new("Safety", "Be precise and concise.");

    assert_eq!(policy.kind, PromptPolicyKind::HardRuntime);
    assert_eq!(policy.stage, MemoryAssetStage::Compiled);
    assert_eq!(policy.form, MemoryAssetForm::Policy);
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
    assert_eq!(
        matches[0].id,
        "asset.room.task.runtime-refactor.0001.summary"
    );
    assert_eq!(matches[0].source_kind, "room_compressed");
    assert_eq!(matches[0].kind, MemoryKind::Decision);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_retriever_skips_stale_room_asset_index_entries() {
    let root = unique_temp_dir("context-stale-room-asset-index");
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
    let asset_path = room_repository
        .write_asset(&room, &asset)
        .expect("compressed room asset should be written");

    WorkspaceStore::new(&root)
        .rebuild_markdown_index_in_namespace(&workspace_namespace)
        .expect("markdown index should be built");
    fs::remove_file(asset_path).expect("asset file should be removed after indexing");

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
        .expect("memory retrieval should skip stale room asset entries");

    assert!(matches.is_empty());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_retriever_includes_global_preferences_for_short_turns() {
    let root = unique_temp_dir("context-global-preference-recall");
    let namespace = MemoryNamespace::new("tenant-a", "user-a");
    let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
    let room_repository = MemoryRoomRepository::with_namespace(&root, workspace_namespace.clone());
    let room = MemoryRoom::new(
        "room.global.tenant-a.user-a",
        MemoryLayer::Global,
        "Global Preference Room",
        "Durable user preferences.",
    )
    .with_namespace(namespace.clone())
    .with_tag("global");
    room_repository
        .write_room(&room)
        .expect("global room should be written");
    let asset = MemoryRoomAsset::new(
        "asset.room.global.tenant-a.user-a.assistant-name",
        room.id.clone(),
        "pref.assistant-name.md",
        MemoryLayer::Global,
        MemoryRoomAssetKind::Compressed,
        "Global Preference",
        "User prefers the assistant to be called 小八.",
    )
    .with_namespace(namespace.clone())
    .with_memory_kind(MemoryKind::Preference)
    .with_owner(MemoryOwnerRef::global())
    .with_tag("global")
    .with_tag("preference");
    room_repository
        .write_asset(&room, &asset)
        .expect("global preference should be written");

    let retriever = WorkspaceMemoryRetriever::new(&root, workspace_namespace);
    let matches = retriever
        .retrieve(
            &ContextMemoryQuery::default()
                .for_namespace(namespace)
                .with_text("你叫什么")
                .with_limit(5),
        )
        .expect("memory retrieval should include global preferences");

    assert!(matches.iter().any(|memory| memory.id
        == "asset.room.global.tenant-a.user-a.assistant-name"
        && memory.summary.contains("小八")));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn room_tool_asset_maps_to_tool_target() {
    let asset = MemoryRoomAsset::new(
        "asset.room.tool.rg.recipe",
        "room.tool.rg",
        "workflow.search-narrow-first.md",
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "RG Narrow Search First",
        "Prefer narrowing search scope before broad content search.",
    )
    .with_namespace(MemoryNamespace::new("tenant-a", "user-a"))
    .with_memory_kind(MemoryKind::WorkflowMemory)
    .with_tag("tool")
    .with_tag("rg")
    .with_tag("recipe");

    let view = asset_view_from_room_asset(&asset);

    assert_eq!(view.target, AssetTarget::Tool);
    assert_eq!(view.target_ref.as_deref(), Some("room.tool.rg"));
    assert!(
        view.consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Executor)
    );
}

#[test]
fn project_prompt_asset_maps_to_llm_consumer() {
    let asset = MemoryRoomAsset::new(
        "asset.room.project.honeycomb.prompt.style",
        "room.project.honeycomb",
        "prompt.honeycomb-style.md",
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "Honeycomb Style Guide",
        "Preserve the repo's established patterns.",
    )
    .with_namespace(MemoryNamespace::new("tenant-a", "user-a"))
    .with_memory_kind(MemoryKind::Preference)
    .with_stage(MemoryAssetStage::Compiled)
    .with_form(MemoryAssetForm::Prompt)
    .with_tag("project")
    .with_tag("prompt")
    .with_tag("styleguide");

    let view = asset_view_from_room_asset(&asset);

    assert_eq!(view.target, AssetTarget::Project);
    assert!(
        view.consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Llm)
    );
}

#[test]
fn retrieved_room_compressed_maps_to_generalized_stage() {
    let memory = RetrievedMemory {
        id: "asset.room.task.runtime-refactor.0001.summary".to_owned(),
        title: "Runtime Refactor Summary".to_owned(),
        summary: "Persist task plans and assignment decisions together.".to_owned(),
        scope: MemoryScope::Task,
        kind: MemoryKind::Decision,
        layer: Some(MemoryLayer::Task),
        room_id: Some("room.task.runtime-refactor.0001".to_owned()),
        source_kind: "room_compressed".to_owned(),
        confidence_milli: 980,
        tags: vec!["task".to_owned(), "runtime".to_owned()],
        derived_from: Vec::new(),
    };

    let view = asset_view_from_retrieved_memory(&memory);

    assert_eq!(view.stage, MemoryAssetStage::Generalized);
    assert_eq!(view.target, AssetTarget::Task);
}

#[test]
fn retrieved_compiled_tool_memory_maps_to_compiled_stage() {
    let memory = RetrievedMemory {
        id: "asset.room.tool.rg.compiled.recipe".to_owned(),
        title: "Compiled RG Recipe".to_owned(),
        summary: "Compiled guidance.".to_owned(),
        scope: MemoryScope::Project,
        kind: MemoryKind::WorkflowMemory,
        layer: Some(MemoryLayer::Project),
        room_id: Some("room.tool.rg".to_owned()),
        source_kind: "room_compressed".to_owned(),
        confidence_milli: 980,
        tags: vec![
            "tool".to_owned(),
            "rg".to_owned(),
            "recipe".to_owned(),
            "compiled".to_owned(),
        ],
        derived_from: Vec::new(),
    };

    let view = asset_view_from_retrieved_memory(&memory);

    assert_eq!(view.stage, MemoryAssetStage::Compiled);
}

#[test]
fn validation_tag_adds_executor_and_evaluator_consumers() {
    let memory = RetrievedMemory {
        id: "asset.room.tool.rg.validation".to_owned(),
        title: "RG Validation".to_owned(),
        summary: "If results are too broad, refine by path before answering.".to_owned(),
        scope: MemoryScope::Project,
        kind: MemoryKind::Knowledge,
        layer: Some(MemoryLayer::Project),
        room_id: Some("room.tool.rg".to_owned()),
        source_kind: "room_compressed".to_owned(),
        confidence_milli: 900,
        tags: vec!["tool".to_owned(), "rg".to_owned(), "validation".to_owned()],
        derived_from: Vec::new(),
    };

    let view = asset_view_from_retrieved_memory(&memory);

    assert!(
        view.consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Executor)
    );
    assert!(
        view.consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Evaluator)
    );
}

#[test]
fn draft_tag_maps_to_draft_status() {
    let record = MemoryRecord::new(
        "memory.task.0003",
        MemoryScope::Task,
        MemoryOwnerRef::task("task.demo"),
        MemoryKind::Summary,
        "Draft Memory",
        "This is still tentative.",
    )
    .with_tag("draft");

    let view = asset_view_from_memory_record(&record);

    assert_eq!(view.status, AssetStatus::Draft);
}

#[test]
fn workflow_memory_defaults_to_workflow_form() {
    let record = MemoryRecord::new(
        "memory.project.workflow.0001",
        MemoryScope::Project,
        MemoryOwnerRef::project("project.honeycomb"),
        MemoryKind::WorkflowMemory,
        "Workflow Rule",
        "Run targeted checks before wider validation.",
    );

    let view = asset_view_from_memory_record(&record);

    assert_eq!(view.form, MemoryAssetForm::Workflow);
    assert!(
        view.consumers
            .iter()
            .any(|consumer| consumer == &AssetConsumer::Planner)
    );
}

#[test]
fn prompt_asset_from_asset_view_accepts_llm_prompt_assets() {
    let asset = AssetView {
        id: "asset.room.project.honeycomb.prompt.style".to_owned(),
        title: "Honeycomb Style Guide".to_owned(),
        summary: "Preserve the repo's established patterns.".to_owned(),
        content: "Preserve the repo's established patterns.".to_owned(),
        kind: MemoryKind::Preference,
        stage: MemoryAssetStage::Compiled,
        form: MemoryAssetForm::Prompt,
        target: AssetTarget::Project,
        target_ref: Some("room.project.honeycomb".to_owned()),
        consumers: vec![AssetConsumer::Llm],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec!["prompt".to_owned(), "styleguide".to_owned()],
        owners: vec![MemoryOwnerRef::project("project.honeycomb")],
        derived_from: Vec::new(),
        source_docs: Vec::new(),
    };

    let prompt = prompt_asset_from_asset_view(&asset).expect("prompt asset should be created");

    assert_eq!(prompt.kind, PromptAssetKind::StyleGuide);
    assert!(prompt.content.contains("established patterns"));
}

#[test]
fn tool_binder_collects_rg_recipe_validation_and_recovery() {
    let assets = vec![
        AssetView {
            id: "asset.room.tool.rg.recipe".to_owned(),
            title: "RG Recipe".to_owned(),
            summary: "Prefer narrowing search scope before broad content search.".to_owned(),
            content: "Prefer narrowing search scope before broad content search.".to_owned(),
            kind: MemoryKind::WorkflowMemory,
            stage: MemoryAssetStage::Generalized,
            form: MemoryAssetForm::Workflow,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.rg".to_owned()),
            consumers: vec![AssetConsumer::Executor],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec!["tool".to_owned(), "rg".to_owned(), "recipe".to_owned()],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
        AssetView {
            id: "asset.room.tool.rg.validation".to_owned(),
            title: "RG Validation".to_owned(),
            summary: "If results are too broad, refine by path before answering.".to_owned(),
            content: "If results are too broad, refine by path before answering.".to_owned(),
            kind: MemoryKind::Knowledge,
            stage: MemoryAssetStage::Generalized,
            form: MemoryAssetForm::Policy,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.rg".to_owned()),
            consumers: vec![AssetConsumer::Executor, AssetConsumer::Evaluator],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec!["tool".to_owned(), "rg".to_owned(), "validation".to_owned()],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
        AssetView {
            id: "asset.room.tool.rg.recovery".to_owned(),
            title: "RG Recovery".to_owned(),
            summary: "If no matches are found, retry with alternate keywords.".to_owned(),
            content: "If no matches are found, retry with alternate keywords.".to_owned(),
            kind: MemoryKind::Decision,
            stage: MemoryAssetStage::Generalized,
            form: MemoryAssetForm::Policy,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.rg".to_owned()),
            consumers: vec![AssetConsumer::Executor],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec!["tool".to_owned(), "rg".to_owned(), "recovery".to_owned()],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
    ];

    let plan = DefaultToolExecutionBinder
        .bind("find memory prompt flow", &seed_tool_rg(), &assets)
        .expect("tool binding should succeed");

    assert_eq!(plan.tool_id, "tool.rg");
    assert_eq!(plan.guidance.len(), 1);
    assert_eq!(plan.validation_steps.len(), 1);
    assert_eq!(plan.recovery_steps.len(), 1);
}

#[test]
fn tool_binder_uses_declared_rg_default_command() {
    let plan = DefaultToolExecutionBinder
        .bind("find which file defines AssetView", &seed_tool_rg(), &[])
        .expect("tool binding should succeed");

    assert_eq!(
        plan.suggested_command,
        vec!["rg".to_owned(), "-n".to_owned()]
    );
}

#[test]
fn tool_binder_prefers_rg_line_search_for_content_goal() {
    let plan = DefaultToolExecutionBinder
        .bind("find memory prompt flow", &seed_tool_rg(), &[])
        .expect("tool binding should succeed");

    assert_eq!(
        plan.suggested_command,
        vec!["rg".to_owned(), "-n".to_owned()]
    );
}

#[test]
fn tool_binder_supports_cargo_test_tool() {
    let assets = vec![
        AssetView {
            id: "asset.room.tool.cargo-test.recipe".to_owned(),
            title: "Cargo Test Narrow First".to_owned(),
            summary: "Start with a targeted test filter before wider test runs.".to_owned(),
            content: "Start with a targeted test filter before wider test runs.".to_owned(),
            kind: MemoryKind::WorkflowMemory,
            stage: MemoryAssetStage::Generalized,
            form: MemoryAssetForm::Workflow,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.cargo-test".to_owned()),
            consumers: vec![AssetConsumer::Executor],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec![
                "tool".to_owned(),
                "cargo-test".to_owned(),
                "recipe".to_owned(),
            ],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
        AssetView {
            id: "asset.room.tool.cargo-test.validation".to_owned(),
            title: "Cargo Test Validation".to_owned(),
            summary: "Check whether the intended tests actually ran before trusting the result."
                .to_owned(),
            content: "Check whether the intended tests actually ran before trusting the result."
                .to_owned(),
            kind: MemoryKind::Knowledge,
            stage: MemoryAssetStage::Generalized,
            form: MemoryAssetForm::Policy,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.cargo-test".to_owned()),
            consumers: vec![AssetConsumer::Executor, AssetConsumer::Evaluator],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec![
                "tool".to_owned(),
                "cargo-test".to_owned(),
                "validation".to_owned(),
            ],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
    ];

    let plan = DefaultToolExecutionBinder
        .bind("run a focused test", &seed_tool_cargo_test(), &assets)
        .expect("tool binding should succeed");

    assert_eq!(plan.tool_id, "tool.cargo-test");
    assert_eq!(
        plan.suggested_command,
        vec!["cargo".to_owned(), "test".to_owned()]
    );
    assert_eq!(plan.guidance.len(), 1);
    assert_eq!(plan.validation_steps.len(), 1);
}

#[test]
fn tool_binder_prefers_compiled_assets_when_available() {
    let assets = vec![
        AssetView {
            id: "asset.room.tool.rg.recipe.generalized".to_owned(),
            title: "RG Recipe".to_owned(),
            summary: "Generalized recipe".to_owned(),
            content: "Generalized recipe".to_owned(),
            kind: MemoryKind::WorkflowMemory,
            stage: MemoryAssetStage::Generalized,
            form: MemoryAssetForm::Workflow,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.rg".to_owned()),
            consumers: vec![AssetConsumer::Executor],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec!["tool".to_owned(), "rg".to_owned(), "recipe".to_owned()],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
        AssetView {
            id: "asset.room.tool.rg.recipe.compiled".to_owned(),
            title: "RG Recipe Compiled".to_owned(),
            summary: "Compiled recipe".to_owned(),
            content: "Compiled recipe".to_owned(),
            kind: MemoryKind::WorkflowMemory,
            stage: MemoryAssetStage::Compiled,
            form: MemoryAssetForm::Workflow,
            target: AssetTarget::Tool,
            target_ref: Some("room.tool.rg".to_owned()),
            consumers: vec![AssetConsumer::Executor],
            status: AssetStatus::Active,
            visibility: MemoryVisibility::Private,
            tags: vec![
                "tool".to_owned(),
                "rg".to_owned(),
                "recipe".to_owned(),
                "compiled".to_owned(),
                "promotion".to_owned(),
            ],
            owners: Vec::new(),
            derived_from: Vec::new(),
            source_docs: Vec::new(),
        },
    ];

    let plan = DefaultToolExecutionBinder
        .bind("find asset view type", &seed_tool_rg(), &assets)
        .expect("tool binding should succeed");

    assert_eq!(plan.guidance, vec!["Compiled recipe".to_owned()]);
}

#[test]
fn build_tool_execution_plan_reads_rg_assets_from_workspace() {
    let root = unique_temp_dir("context-tool-plan");
    let namespace = MemoryNamespace::new("tenant-a", "user-a");
    let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
    let room_repository = MemoryRoomRepository::with_namespace(&root, workspace_namespace.clone());
    let room = MemoryRoom::new(
        "room.tool.rg",
        MemoryLayer::Project,
        "RG Tool Room",
        "Reusable rg search guidance.",
    )
    .with_namespace(namespace.clone())
    .with_tag("tool")
    .with_tag("rg")
    .with_tag("project");
    room_repository
        .write_room(&room)
        .expect("tool room should be written");

    let recipe = MemoryRoomAsset::new(
        "asset.room.tool.rg.recipe",
        room.id.clone(),
        "workflow.search-narrow-first.md",
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "RG Narrow Search First",
        "Prefer narrowing search scope before broad content search.",
    )
    .with_namespace(namespace.clone())
    .with_memory_kind(MemoryKind::WorkflowMemory)
    .with_tag("tool")
    .with_tag("rg")
    .with_tag("recipe");
    room_repository
        .write_asset(&room, &recipe)
        .expect("recipe asset should be written");

    let validation = MemoryRoomAsset::new(
        "asset.room.tool.rg.validation",
        room.id.clone(),
        "validation.refine-broad-results.md",
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "RG Refine Broad Results",
        "If results are too broad, refine by path before answering.",
    )
    .with_namespace(namespace.clone())
    .with_memory_kind(MemoryKind::Knowledge)
    .with_tag("tool")
    .with_tag("rg")
    .with_tag("validation");
    room_repository
        .write_asset(&room, &validation)
        .expect("validation asset should be written");

    let retriever = WorkspaceMemoryRetriever::new(&root, workspace_namespace);
    let plan = build_tool_execution_plan(
        &retriever,
        &DefaultToolExecutionBinder,
        namespace,
        "find memory prompt flow",
        &seed_tool_rg(),
    )
    .expect("tool execution plan should build");

    assert_eq!(plan.tool_id, "tool.rg");
    assert_eq!(
        plan.suggested_command,
        vec!["rg".to_owned(), "-n".to_owned()]
    );
    assert_eq!(plan.guidance.len(), 1);
    assert_eq!(plan.validation_steps.len(), 1);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn generalization_policy_allows_human_confirmed_asset() {
    let asset = AssetView {
        id: "asset.room.tool.rg.recipe".to_owned(),
        title: "RG Recipe".to_owned(),
        summary: "Prefer narrowing search scope before broad content search.".to_owned(),
        content: "Prefer narrowing search scope before broad content search.".to_owned(),
        kind: MemoryKind::WorkflowMemory,
        stage: MemoryAssetStage::Extracted,
        form: MemoryAssetForm::Workflow,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Executor],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec!["tool".to_owned(), "rg".to_owned()],
        owners: Vec::new(),
        derived_from: Vec::new(),
        source_docs: Vec::new(),
    };

    assert!(should_generalize(
        &asset,
        0,
        true,
        &GeneralizationPolicy::default()
    ));
}

#[test]
fn promotion_rule_requires_matching_stage_and_consumer() {
    let asset = AssetView {
        id: "asset.room.tool.rg.recipe".to_owned(),
        title: "RG Recipe".to_owned(),
        summary: "Prefer narrowing search scope before broad content search.".to_owned(),
        content: "Prefer narrowing search scope before broad content search.".to_owned(),
        kind: MemoryKind::WorkflowMemory,
        stage: MemoryAssetStage::Generalized,
        form: MemoryAssetForm::Workflow,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Executor],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec!["tool".to_owned(), "rg".to_owned(), "recipe".to_owned()],
        owners: Vec::new(),
        derived_from: Vec::new(),
        source_docs: Vec::new(),
    };
    let rule = PromotionRule {
        from_stage: MemoryAssetStage::Generalized,
        to_stage: MemoryAssetStage::Compiled,
        min_confidence_milli: 700,
        required_tags: vec!["recipe".to_owned()],
        required_consumers: vec![AssetConsumer::Executor],
    };

    assert!(can_promote(&asset, &rule));
}

#[test]
fn retirement_rule_retires_after_human_rejection() {
    assert!(should_retire(
        0,
        &[EvaluationSignal::HumanRejected],
        &RetirementRule::default(),
    ));
}

#[test]
fn infer_tool_execution_signals_marks_success_and_validation() {
    let plan = ToolExecutionPlan {
        tool_id: "tool.rg".to_owned(),
        suggested_command: vec!["rg".to_owned(), "-n".to_owned()],
        guidance: vec!["Prefer narrowing search scope.".to_owned()],
        validation_steps: vec!["Refine broad results.".to_owned()],
        recovery_steps: Vec::new(),
    };
    let outcome = ToolExecutionOutcome {
        tool_id: "tool.rg".to_owned(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: "find asset view".to_owned(),
        command: vec!["rg".to_owned(), "-n".to_owned(), "AssetView".to_owned()],
        success: true,
        summary: "Found 3 rg match lines.".to_owned(),
        observations: vec![
            "crates/hc-context/src/lib.rs:42:pub struct AssetView".to_owned(),
            "apps/hc-context-cli/src/main.rs:10:AssetView".to_owned(),
        ],
    };

    let signals = infer_tool_execution_signals(&plan, &outcome);
    assert!(signals.contains(&EvaluationSignal::ExecutionSucceeded));
    assert!(signals.contains(&EvaluationSignal::ValidationPassed));
    assert!(signals.contains(&EvaluationSignal::RepeatedReuse));
}

#[test]
fn evaluate_tool_execution_returns_promotable_assets() {
    let tool = seed_tool_rg();
    let asset = AssetView {
        id: "asset.room.tool.rg.recipe".to_owned(),
        title: "RG Recipe".to_owned(),
        summary: "Prefer narrowing search scope before broad content search.".to_owned(),
        content: "Prefer narrowing search scope before broad content search.".to_owned(),
        kind: MemoryKind::WorkflowMemory,
        stage: MemoryAssetStage::Generalized,
        form: MemoryAssetForm::Workflow,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Executor],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec!["tool".to_owned(), "rg".to_owned(), "recipe".to_owned()],
        owners: Vec::new(),
        derived_from: Vec::new(),
        source_docs: Vec::new(),
    };
    let plan = ToolExecutionPlan {
        tool_id: tool.id.clone(),
        suggested_command: vec!["rg".to_owned(), "-n".to_owned()],
        guidance: vec!["Prefer narrowing search scope.".to_owned()],
        validation_steps: vec!["Refine broad results.".to_owned()],
        recovery_steps: Vec::new(),
    };
    let outcome = ToolExecutionOutcome {
        tool_id: tool.id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: "find asset view".to_owned(),
        command: vec!["rg".to_owned(), "-n".to_owned(), "AssetView".to_owned()],
        success: true,
        summary: "Found rg matches.".to_owned(),
        observations: vec![
            "crates/hc-context/src/lib.rs:42:pub struct AssetView".to_owned(),
            "crates/hc-context/src/lib.rs:60:pub enum AssetTarget".to_owned(),
        ],
    };
    let evaluation = evaluate_tool_execution(
        &tool,
        &plan,
        &outcome,
        &[asset],
        &GeneralizationPolicy::default(),
        &PromotionRule {
            from_stage: MemoryAssetStage::Generalized,
            to_stage: MemoryAssetStage::Compiled,
            min_confidence_milli: 700,
            required_tags: vec!["tool".to_owned(), "rg".to_owned()],
            required_consumers: vec![AssetConsumer::Executor],
        },
        &RetirementRule::default(),
    );

    assert_eq!(evaluation.tool_id, "tool.rg");
    assert_eq!(
        evaluation.promote_candidate_ids,
        vec!["asset.room.tool.rg.recipe".to_owned()]
    );
    assert!(
        evaluation
            .signals
            .contains(&EvaluationSignal::ExecutionSucceeded)
    );
    assert!(
        evaluation
            .signals
            .contains(&EvaluationSignal::ValidationPassed)
    );
    assert!(
        evaluation
            .events
            .iter()
            .any(|event| matches!(event.action, EvolutionAction::Evaluated))
    );
}

#[test]
fn export_tool_capability_package_writes_clean_single_capability_bundle() {
    let root = unique_temp_dir("tool-export");
    let tool = seed_tool_rg();
    let recipe = AssetView {
        id: "asset.room.tool.rg.compiled.recipe".to_owned(),
        title: "RG Compiled Recipe".to_owned(),
        summary: "Use focused rg searches before broad scans.".to_owned(),
        content: "Use focused rg searches before broad scans.".to_owned(),
        kind: MemoryKind::WorkflowMemory,
        stage: MemoryAssetStage::Compiled,
        form: MemoryAssetForm::Workflow,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Executor],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec![
            "tool".to_owned(),
            "rg".to_owned(),
            "recipe".to_owned(),
            "compiled".to_owned(),
            "promotion".to_owned(),
        ],
        owners: Vec::new(),
        derived_from: vec!["asset.room.tool.rg.recipe.raw".to_owned()],
        source_docs: Vec::new(),
    };
    let evaluation_event = AssetView {
        id: "asset.room.tool.rg.event.evaluated".to_owned(),
        title: "RG Evaluated".to_owned(),
        summary: "This should stay out of the export.".to_owned(),
        content: "This should stay out of the export.".to_owned(),
        kind: MemoryKind::Summary,
        stage: MemoryAssetStage::Extracted,
        form: MemoryAssetForm::Summary,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Human],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec![
            "tool".to_owned(),
            "rg".to_owned(),
            "evaluation-event".to_owned(),
        ],
        owners: Vec::new(),
        derived_from: vec![recipe.id.clone()],
        source_docs: Vec::new(),
    };

    let package = export_tool_capability_package(&root, &tool, &[recipe, evaluation_event])
        .expect("tool capability export should succeed");

    assert_eq!(package.manifest.package_id, "capability.rg");
    assert_eq!(package.manifest.assets.len(), 1);
    assert_eq!(package.manifest.assets[0].role, "recipe");
    assert!(
        !package.manifest.assets[0]
            .tags
            .iter()
            .any(|tag| tag == "promotion")
    );
    assert!(root.join("README.md").exists());
    assert!(root.join("package.json").exists());
    assert!(root.join("portable").join("manifest.json").exists());
    assert!(root.join("runnable").join("run.sh").exists());
    assert!(
        root.join("portable")
            .join(&package.manifest.assets[0].file)
            .exists()
    );
}

#[test]
fn evaluate_tool_execution_retires_after_repeated_failures() {
    let tool = seed_tool_rg();
    let asset_id = "asset.room.tool.rg.recipe".to_owned();
    let asset = AssetView {
        id: asset_id.clone(),
        title: "RG Recipe".to_owned(),
        summary: "Prefer narrowing search scope before broad content search.".to_owned(),
        content: "Prefer narrowing search scope before broad content search.".to_owned(),
        kind: MemoryKind::WorkflowMemory,
        stage: MemoryAssetStage::Generalized,
        form: MemoryAssetForm::Workflow,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Executor],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec!["tool".to_owned(), "rg".to_owned(), "recipe".to_owned()],
        owners: Vec::new(),
        derived_from: Vec::new(),
        source_docs: Vec::new(),
    };
    let prior_failure_event = |suffix: &str| AssetView {
        id: format!("asset.room.tool.rg.event.{suffix}"),
        title: "RG Evaluated".to_owned(),
        summary: "Prior failed evaluation".to_owned(),
        content: "Prior failed evaluation".to_owned(),
        kind: MemoryKind::Summary,
        stage: MemoryAssetStage::Extracted,
        form: MemoryAssetForm::Summary,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Human],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec![
            "tool".to_owned(),
            "rg".to_owned(),
            "evaluation-event".to_owned(),
            "execution_failed".to_owned(),
        ],
        owners: Vec::new(),
        derived_from: vec![asset_id.clone()],
        source_docs: Vec::new(),
    };
    let plan = ToolExecutionPlan {
        tool_id: tool.id.clone(),
        suggested_command: vec!["rg".to_owned(), "-n".to_owned()],
        guidance: vec!["Prefer narrowing search scope.".to_owned()],
        validation_steps: vec!["Refine broad results.".to_owned()],
        recovery_steps: Vec::new(),
    };
    let outcome = ToolExecutionOutcome {
        tool_id: tool.id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: "find missing type".to_owned(),
        command: vec!["rg".to_owned(), "-n".to_owned(), "MissingType".to_owned()],
        success: false,
        summary: "No rg matches found.".to_owned(),
        observations: vec!["stderr: no matches".to_owned()],
    };
    let evaluation = evaluate_tool_execution(
        &tool,
        &plan,
        &outcome,
        &[
            asset,
            prior_failure_event("one"),
            prior_failure_event("two"),
        ],
        &GeneralizationPolicy::default(),
        &PromotionRule {
            from_stage: MemoryAssetStage::Generalized,
            to_stage: MemoryAssetStage::Compiled,
            min_confidence_milli: 700,
            required_tags: vec!["tool".to_owned(), "rg".to_owned()],
            required_consumers: vec![AssetConsumer::Executor],
        },
        &RetirementRule::default(),
    );

    assert_eq!(evaluation.revise_candidate_ids, Vec::<String>::new());
    assert_eq!(evaluation.retire_candidate_ids, vec![asset_id]);
    assert!(
        evaluation
            .events
            .iter()
            .any(|event| matches!(event.action, EvolutionAction::Retired))
    );
}

#[test]
fn tool_evaluation_write_requests_include_summary_and_events() {
    let tool = seed_tool_rg();
    let evaluation = ToolExecutionEvaluation {
        tool_id: tool.id.clone(),
        matched_asset_ids: vec!["asset.room.tool.rg.recipe".to_owned()],
        signals: vec![EvaluationSignal::ExecutionSucceeded],
        supporting_events: 1,
        generalize_candidate_ids: vec!["asset.room.tool.rg.recipe".to_owned()],
        promote_candidate_ids: vec!["asset.room.tool.rg.recipe".to_owned()],
        revise_candidate_ids: Vec::new(),
        retire_candidate_ids: Vec::new(),
        events: vec![AssetEvolutionEvent {
            id: "event.asset.room.tool.rg.recipe.evaluated".to_owned(),
            asset_id: "asset.room.tool.rg.recipe".to_owned(),
            action: EvolutionAction::Evaluated,
            reason: "signals: ExecutionSucceeded".to_owned(),
            inputs: vec!["rg".to_owned(), "-n".to_owned()],
            outputs: vec!["match".to_owned()],
            confidence_milli: 800,
            created_at_ms: 1,
        }],
    };

    let requests = room_memory_write_requests_from_tool_evaluation(&tool, &evaluation);
    assert_eq!(requests.len(), 2);
    assert!(requests[0].tags.iter().any(|tag| tag == "evaluation"));
    assert!(requests[1].tags.iter().any(|tag| tag == "evaluation-event"));
}

#[test]
fn persist_compiled_tool_assets_writes_promoted_assets() {
    let root = unique_temp_dir("context-tool-compiled");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let tool = seed_tool_rg();
    let asset = AssetView {
        id: "asset.room.tool.rg.recipe".to_owned(),
        title: "RG Recipe".to_owned(),
        summary: "Prefer narrowing search scope before broad content search.".to_owned(),
        content: "Prefer narrowing search scope before broad content search.".to_owned(),
        kind: MemoryKind::WorkflowMemory,
        stage: MemoryAssetStage::Generalized,
        form: MemoryAssetForm::Workflow,
        target: AssetTarget::Tool,
        target_ref: Some("room.tool.rg".to_owned()),
        consumers: vec![AssetConsumer::Executor],
        status: AssetStatus::Active,
        visibility: MemoryVisibility::Private,
        tags: vec!["tool".to_owned(), "rg".to_owned(), "recipe".to_owned()],
        owners: Vec::new(),
        derived_from: Vec::new(),
        source_docs: Vec::new(),
    };
    let evaluation = ToolExecutionEvaluation {
        tool_id: tool.id.clone(),
        matched_asset_ids: vec![asset.id.clone()],
        signals: vec![EvaluationSignal::ExecutionSucceeded],
        supporting_events: 1,
        generalize_candidate_ids: Vec::new(),
        promote_candidate_ids: vec![asset.id.clone()],
        revise_candidate_ids: Vec::new(),
        retire_candidate_ids: Vec::new(),
        events: Vec::new(),
    };

    let paths =
        persist_compiled_tool_assets(&root, namespace.clone(), &tool, &[asset], &evaluation)
            .expect("compiled tool assets should persist");

    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.starts_with("compiled."))
    );
    let contents = fs::read_to_string(&paths[0]).expect("compiled asset should be readable");
    assert!(contents.contains("Prefer narrowing search scope before broad content search."));
    assert!(!contents.contains("Compiled guidance for"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn persist_tool_evolution_events_writes_timeline_entries() {
    let root = unique_temp_dir("context-tool-events");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let tool = seed_tool_rg();
    let evaluation = ToolExecutionEvaluation {
        tool_id: tool.id.clone(),
        matched_asset_ids: vec!["asset.room.tool.rg.recipe".to_owned()],
        signals: vec![EvaluationSignal::ExecutionSucceeded],
        supporting_events: 1,
        generalize_candidate_ids: Vec::new(),
        promote_candidate_ids: vec!["asset.room.tool.rg.recipe".to_owned()],
        revise_candidate_ids: Vec::new(),
        retire_candidate_ids: Vec::new(),
        events: vec![AssetEvolutionEvent {
            id: "event.asset.room.tool.rg.recipe.promoted".to_owned(),
            asset_id: "asset.room.tool.rg.recipe".to_owned(),
            action: EvolutionAction::Promoted,
            reason: "eligible for promotion to compiled guidance".to_owned(),
            inputs: vec!["execution_succeeded".to_owned()],
            outputs: vec!["promote_to:compiled".to_owned()],
            confidence_milli: 900,
            created_at_ms: 42,
        }],
    };

    let paths = persist_tool_evolution_events(&root, namespace, &tool, &evaluation)
        .expect("tool evolution events should persist");

    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("memory/rooms/project/room.tool.rg/timeline.md")
    );
    let contents = fs::read_to_string(&paths[0]).expect("timeline should be readable");
    assert!(contents.contains("event.asset.room.tool.rg.recipe.promoted"));
    assert!(contents.contains("promote_to:compiled"));

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
    assert_eq!(
        request.system_prompt.as_deref(),
        Some("Use recalled context")
    );
    assert_eq!(
        request.self_model.as_ref().map(|model| model.role.as_str()),
        Some("helper")
    );
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
    let relative = PathBuf::from(
        "memory/rooms/task/room.task.runtime-refactor.0001/compressed/min.assignment.md",
    );
    let loaded = repository
        .read_asset(relative)
        .expect("room memory asset should roundtrip");

    assert_eq!(loaded.id, "asset.room.task.runtime-refactor.0001.decision");
    assert_eq!(loaded.memory_kind, MemoryKind::Decision);
    assert!(
        loaded
            .owners
            .iter()
            .any(|owner| owner == &MemoryOwnerRef::task("task.demo"))
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn synthesized_prompt_assets_can_be_persisted_as_room_assets() {
    let root = unique_temp_dir("context-prompt-writeback");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let response = ContextResponse {
        response: GenerateResponse {
            model: ModelRef::new("test", "mock"),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        },
        recalled_memories: vec![RetrievedMemory {
            id: "asset.room.global.tenant-a.user-a.pref-language".to_owned(),
            title: "Language Preference".to_owned(),
            summary: "User prefers responses in Chinese.".to_owned(),
            scope: MemoryScope::Global,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Global),
            room_id: Some("room.global.tenant-a.user-a".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 980,
            tags: vec!["global".to_owned(), "preference".to_owned()],
            derived_from: Vec::new(),
        }],
        synthesized_prompt_assets: vec![PromptAsset::new(
            "asset.room.global.tenant-a.user-a.pref-language",
            PromptAssetKind::StyleGuide,
            "Language Style Guide",
            "Respond in concise Chinese.",
        )],
    };

    let paths = persist_synthesized_prompt_assets(&root, namespace.clone(), &response)
        .expect("prompt assets should be persisted");
    assert_eq!(paths.len(), 1);

    let repository = MemoryRoomRepository::with_namespace(&root, namespace);
    let relative = PathBuf::from(
        "memory/rooms/global/room.global.tenant-a.user-a/prompt/prompt.language.style.guide.md",
    );
    let loaded = repository
        .read_asset(relative)
        .expect("persisted prompt asset should be readable");

    assert_eq!(loaded.stage, MemoryAssetStage::Compiled);
    assert_eq!(loaded.form, MemoryAssetForm::Prompt);
    assert_eq!(loaded.memory_kind, MemoryKind::Preference);
    assert!(loaded.tags.iter().any(|tag| tag == "prompt"));
    let timeline = root.join(
        "tenants/tenant-a/users/user-a/memory/rooms/global/room.global.tenant-a.user-a/timeline.md",
    );
    assert!(timeline.exists());
    let timeline_contents =
        fs::read_to_string(&timeline).expect("prompt timeline should be readable");
    assert!(timeline_contents.contains("compiled recalled memory into prompt asset"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn synthesized_prompt_assets_can_route_into_topic_room() {
    let root = unique_temp_dir("context-prompt-topic-writeback");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let response = ContextResponse {
        response: GenerateResponse {
            model: ModelRef::new("test", "mock"),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        },
        recalled_memories: vec![RetrievedMemory {
            id: "asset.room.chat.local.default.1.pref-review".to_owned(),
            title: "Review Style".to_owned(),
            summary: "Prioritize regression risks.".to_owned(),
            scope: MemoryScope::Session,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Chat),
            room_id: Some("room.chat.local.default.1".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 950,
            tags: vec!["topic".to_owned(), "review".to_owned()],
            derived_from: Vec::new(),
        }],
        synthesized_prompt_assets: vec![PromptAsset::new(
            "asset.room.chat.local.default.1.pref-review",
            PromptAssetKind::BehaviorTemplate,
            "Review Style Guide",
            "Prioritize regression risks.",
        )],
    };

    let paths = persist_synthesized_prompt_assets(&root, namespace, &response)
        .expect("topic prompt assets should be persisted");
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/topic/room.topic.review/prompt/")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn synthesized_prompt_assets_can_route_into_agent_room() {
    let root = unique_temp_dir("context-prompt-agent-writeback");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let response = ContextResponse {
        response: GenerateResponse {
            model: ModelRef::new("test", "mock"),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        },
        recalled_memories: vec![RetrievedMemory {
            id: "asset.room.chat.local.default.1.pref-reviewer".to_owned(),
            title: "Reviewer Habit".to_owned(),
            summary: "Focus on regressions first.".to_owned(),
            scope: MemoryScope::Session,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Chat),
            room_id: Some("room.chat.local.default.1".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 950,
            tags: vec!["agent".to_owned(), "reviewer".to_owned()],
            derived_from: Vec::new(),
        }],
        synthesized_prompt_assets: vec![PromptAsset::new(
            "asset.room.chat.local.default.1.pref-reviewer",
            PromptAssetKind::BehaviorTemplate,
            "Reviewer Behavior",
            "Focus on regressions first.",
        )],
    };

    let paths = persist_synthesized_prompt_assets(&root, namespace, &response)
        .expect("agent prompt assets should be persisted");
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/global/room.agent.reviewer/prompt/")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn synthesized_prompt_assets_can_route_into_tool_room() {
    let root = unique_temp_dir("context-prompt-tool-writeback");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let response = ContextResponse {
        response: GenerateResponse {
            model: ModelRef::new("test", "mock"),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        },
        recalled_memories: vec![RetrievedMemory {
            id: "asset.room.chat.local.default.1.pref-rg".to_owned(),
            title: "Ripgrep Workflow".to_owned(),
            summary: "Use rg first when searching the repo.".to_owned(),
            scope: MemoryScope::Session,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Chat),
            room_id: Some("room.chat.local.default.1".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 950,
            tags: vec!["tool".to_owned(), "rg".to_owned()],
            derived_from: Vec::new(),
        }],
        synthesized_prompt_assets: vec![PromptAsset::new(
            "asset.room.chat.local.default.1.pref-rg",
            PromptAssetKind::BehaviorTemplate,
            "RG Workflow",
            "Use rg first when searching the repo.",
        )],
    };

    let paths = persist_synthesized_prompt_assets(&root, namespace, &response)
        .expect("tool prompt assets should be persisted");
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/project/room.tool.rg/prompt/")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn synthesized_prompt_assets_can_route_into_project_room() {
    let root = unique_temp_dir("context-prompt-project-writeback");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let response = ContextResponse {
        response: GenerateResponse {
            model: ModelRef::new("test", "mock"),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        },
        recalled_memories: vec![RetrievedMemory {
            id: "asset.room.chat.local.default.1.pref-honeycomb".to_owned(),
            title: "Honeycomb Style".to_owned(),
            summary: "Preserve the repo's established patterns.".to_owned(),
            scope: MemoryScope::Session,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Chat),
            room_id: Some("room.chat.local.default.1".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 950,
            tags: vec!["project".to_owned(), "honeycomb".to_owned()],
            derived_from: Vec::new(),
        }],
        synthesized_prompt_assets: vec![PromptAsset::new(
            "asset.room.chat.local.default.1.pref-honeycomb",
            PromptAssetKind::StyleGuide,
            "Honeycomb Style Guide",
            "Preserve the repo's established patterns.",
        )],
    };

    let paths = persist_synthesized_prompt_assets(&root, namespace, &response)
        .expect("project prompt assets should be persisted");
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/project/room.project.honeycomb/prompt/")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn synthesized_prompt_assets_can_route_into_task_room() {
    let root = unique_temp_dir("context-prompt-task-writeback");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let response = ContextResponse {
        response: GenerateResponse {
            model: ModelRef::new("test", "mock"),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        },
        recalled_memories: vec![RetrievedMemory {
            id: "asset.room.chat.local.default.1.pref-runtime-refactor".to_owned(),
            title: "Runtime Refactor Rule".to_owned(),
            summary: "Land memory changes with green tests.".to_owned(),
            scope: MemoryScope::Session,
            kind: MemoryKind::Preference,
            layer: Some(MemoryLayer::Chat),
            room_id: Some("room.chat.local.default.1".to_owned()),
            source_kind: "room_compressed".to_owned(),
            confidence_milli: 950,
            tags: vec!["task".to_owned(), "runtime-refactor".to_owned()],
            derived_from: Vec::new(),
        }],
        synthesized_prompt_assets: vec![PromptAsset::new(
            "asset.room.chat.local.default.1.pref-runtime-refactor",
            PromptAssetKind::BehaviorTemplate,
            "Runtime Refactor Guide",
            "Land memory changes with green tests.",
        )],
    };

    let paths = persist_synthesized_prompt_assets(&root, namespace, &response)
        .expect("task prompt assets should be persisted");
    assert_eq!(paths.len(), 1);
    assert!(
        paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/task/room.task.runtime.refactor/prompt/")
    );

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
fn keyword_tag_suggester_infers_semantic_room_tags() {
    let suggester = KeywordMemoryTagSuggester;
    let input = MemoryOrganizationInput::new(
        MemoryNamespace::new("tenant-a", "user-a"),
        "This agent reviewer should use rg for honeycomb project refactor work.",
    )
    .with_title_hint("Reviewer RG Workflow");

    let tags = suggester
        .suggest_tags(&input)
        .expect("tag suggestion should succeed");

    assert!(tags.iter().any(|tag| tag == "agent"));
    assert!(tags.iter().any(|tag| tag == "reviewer"));
    assert!(tags.iter().any(|tag| tag == "tool"));
    assert!(tags.iter().any(|tag| tag == "rg"));
    assert!(tags.iter().any(|tag| tag == "project"));
    assert!(tags.iter().any(|tag| tag == "task"));
}

#[test]
fn organizer_merges_dynamic_semantic_tags() {
    let organizer = CompositeMemoryOrganizer::new(
        RuleBasedMemoryRoomRouter,
        RuleBasedMemoryKindResolver,
        KeywordMemoryTagSuggester,
        NoopMemoryPromotionAdvisor,
    );
    let input = MemoryOrganizationInput::new(
        MemoryNamespace::new("tenant-a", "user-a"),
        "Project convention: the reviewer agent should use rg when debugging runtime refactors.",
    )
    .with_title_hint("Honeycomb Reviewer Workflow")
    .with_tag("manual");

    let decision = organizer
        .organize(&input)
        .expect("organization should succeed");

    assert!(decision.tags.iter().any(|tag| tag == "manual"));
    assert!(decision.tags.iter().any(|tag| tag == "agent"));
    assert!(decision.tags.iter().any(|tag| tag == "reviewer"));
    assert!(decision.tags.iter().any(|tag| tag == "tool"));
    assert!(decision.tags.iter().any(|tag| tag == "rg"));
    assert!(decision.tags.iter().any(|tag| tag == "project"));
    assert!(decision.tags.iter().any(|tag| tag == "task"));
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

    let request =
        room_memory_write_request_from_organization(&decision, "Shared project architecture note.");

    assert_eq!(request.room_id, "room.project.honeycomb");
    assert_eq!(request.room_layer, MemoryLayer::Project);
    assert_eq!(request.memory_kind, MemoryKind::Knowledge);
    assert_eq!(request.visibility, MemoryVisibility::TenantShared);
    assert!(
        request
            .owners
            .iter()
            .any(|owner| owner == &MemoryOwnerRef::project("project.honeycomb"))
    );
    assert!(request.tags.iter().any(|tag| tag == "architecture"));
}

#[test]
fn prompt_asset_can_be_derived_from_memory() {
    let memory = RetrievedMemory::from(
        &MemoryRecord::new(
            "memory.global.preference.0001",
            MemoryScope::Global,
            MemoryOwnerRef::global(),
            MemoryKind::Preference,
            "Writing Preference",
            "Respond in concise Chinese with markdown when useful.",
        )
        .with_tag("style"),
    );

    let asset =
        prompt_asset_from_memory(&memory, PromptAssetKind::StyleGuide, "User Writing Style");

    assert_eq!(asset.id, memory.id);
    assert_eq!(asset.kind, PromptAssetKind::StyleGuide);
    assert_eq!(asset.stage, MemoryAssetStage::Compiled);
    assert_eq!(asset.form, MemoryAssetForm::Prompt);
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
        derived_from: Vec::new(),
    };

    let assets = DefaultPromptAssetSynthesizer
        .synthesize(&[memory])
        .expect("synthesis should succeed");

    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].kind, PromptAssetKind::BehaviorTemplate);
    assert_eq!(assets[0].stage, MemoryAssetStage::Compiled);
    assert_eq!(assets[0].form, MemoryAssetForm::Prompt);
    assert!(assets[0].content.contains("小八"));
}

#[test]
fn default_prompt_asset_synthesizer_prefers_compiled_prompt_memory() {
    let compiled_prompt = RetrievedMemory {
        id: "asset.room.global.local.default.prompt.language".to_owned(),
        title: "Language Style Guide".to_owned(),
        summary: "Respond in concise Chinese.".to_owned(),
        scope: MemoryScope::Global,
        kind: MemoryKind::Preference,
        layer: Some(MemoryLayer::Global),
        room_id: Some("room.global.local.default".to_owned()),
        source_kind: "room_compressed".to_owned(),
        confidence_milli: 990,
        tags: vec![
            "global".to_owned(),
            "preference".to_owned(),
            "prompt".to_owned(),
            "styleguide".to_owned(),
        ],
        derived_from: Vec::new(),
    };
    let source_preference = RetrievedMemory {
        id: "asset.room.global.local.default.pref-language".to_owned(),
        title: "Language Preference".to_owned(),
        summary: "User prefers responses in Chinese.".to_owned(),
        scope: MemoryScope::Global,
        kind: MemoryKind::Preference,
        layer: Some(MemoryLayer::Global),
        room_id: Some("room.global.local.default".to_owned()),
        source_kind: "room_compressed".to_owned(),
        confidence_milli: 980,
        tags: vec!["global".to_owned(), "preference".to_owned()],
        derived_from: Vec::new(),
    };

    let assets = DefaultPromptAssetSynthesizer
        .synthesize(&[compiled_prompt, source_preference])
        .expect("synthesis should succeed");

    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].kind, PromptAssetKind::StyleGuide);
    assert_eq!(assets[0].content, "Respond in concise Chinese.");
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
        WorkspaceNamespace::local_default(),
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
    registry.register(StaticProvider::new(
        "test",
        r#"{"note":"no prompt assets"}"#,
    ));
    let synthesizer = LlmPromptAssetSynthesizer::strict(
        &registry,
        ModelRef::new("test", "mock"),
        WorkspaceNamespace::local_default(),
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
fn llm_memory_tag_suggester_prefers_llm_output() {
    let mut registry = ProviderRegistry::new();
    registry.register(StaticProvider::new(
        "test",
        r#"{"tags":["agent","reviewer","tool","rg","task","runtime.refactor"]}"#,
    ));
    let suggester = LlmMemoryTagSuggester::new(
        &registry,
        ModelRef::new("test", "mock"),
        WorkspaceNamespace::local_default(),
        KeywordMemoryTagSuggester,
    );
    let input = MemoryOrganizationInput::new(
        MemoryNamespace::new("local", "default"),
        "The reviewer agent should use rg during the runtime refactor.",
    )
    .with_title_hint("Reviewer RG Workflow");

    let tags = suggester
        .suggest_tags(&input)
        .expect("llm tag suggestion should succeed");

    assert!(tags.iter().any(|tag| tag == "agent"));
    assert!(tags.iter().any(|tag| tag == "reviewer"));
    assert!(tags.iter().any(|tag| tag == "tool"));
    assert!(tags.iter().any(|tag| tag == "rg"));
    assert!(tags.iter().any(|tag| tag == "task"));
    assert!(tags.iter().any(|tag| tag == "runtime.refactor"));
}

#[test]
fn llm_memory_tag_suggester_falls_back_when_tags_missing() {
    let mut registry = ProviderRegistry::new();
    registry.register(StaticProvider::new(
        "test",
        r#"{"note":"no semantic tags"}"#,
    ));
    let suggester = LlmMemoryTagSuggester::new(
        &registry,
        ModelRef::new("test", "mock"),
        WorkspaceNamespace::local_default(),
        KeywordMemoryTagSuggester,
    );
    let input = MemoryOrganizationInput::new(
        MemoryNamespace::new("local", "default"),
        "This reviewer agent should use rg for project work.",
    )
    .with_title_hint("Reviewer RG Workflow");

    let tags = suggester
        .suggest_tags(&input)
        .expect("fallback tag suggestion should succeed");

    assert!(tags.iter().any(|tag| tag == "agent"));
    assert!(tags.iter().any(|tag| tag == "reviewer"));
    assert!(tags.iter().any(|tag| tag == "tool"));
    assert!(tags.iter().any(|tag| tag == "rg"));
    assert!(tags.iter().any(|tag| tag == "project"));
}

#[test]
fn load_managed_prompt_body_syncs_prompt_library_room_asset() {
    let root = unique_temp_dir("managed-prompt-room-asset");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");

    let body = load_managed_prompt_body(&root, &namespace, ManagedPromptKind::SemanticTagSuggester)
        .expect("managed prompt body should load");

    assert!(body.contains("Infer compact semantic tags"));

    let repository = MemoryRoomRepository::with_namespace(&root, namespace.clone());
    let room = managed_prompt_room(&namespace);
    let relative = MemoryRoomRepository::prompt_doc_relative_path(&room, "semantic-tags.md");
    let loaded = repository
        .read_asset(relative)
        .expect("managed prompt should be mirrored into prompt library room");

    assert_eq!(loaded.id, "prompt.extract.semantic-tags");
    assert_eq!(loaded.stage, MemoryAssetStage::Compiled);
    assert_eq!(loaded.form, MemoryAssetForm::Prompt);
    assert!(loaded.tags.iter().any(|tag| tag == "managed_prompt"));
    assert!(loaded.tags.iter().any(|tag| tag == "extract"));
    let timeline = root.join(
        "tenants/tenant-a/users/user-a/memory/rooms/project/room.project.prompt-library/timeline.md",
    );
    assert!(timeline.exists());
    let timeline_contents =
        fs::read_to_string(&timeline).expect("managed prompt timeline should be readable");
    assert!(timeline_contents.contains("synced managed prompt body into prompt library room"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn managed_prompt_sync_archives_revision_when_local_body_changes() {
    let root = unique_temp_dir("managed-prompt-revision");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");

    let _ = load_managed_prompt_body(&root, &namespace, ManagedPromptKind::SemanticTagSuggester)
        .expect("managed prompt body should load");

    let store = WorkspaceStore::new(PathBuf::from(&root));
    let relative = managed_prompt_relative_path(ManagedPromptKind::SemanticTagSuggester);
    store
        .write_text_in_namespace(
            &namespace,
            &relative,
            "Infer semantic tags.\n\nReturn strict JSON with reviewer and rg when relevant.\n",
        )
        .expect("updated prompt body should be written");

    let _ = load_managed_prompt_body(&root, &namespace, ManagedPromptKind::SemanticTagSuggester)
        .expect("managed prompt body should resync");

    let repository = MemoryRoomRepository::with_namespace(&root, namespace.clone());
    let room = managed_prompt_room(&namespace);
    let current_relative =
        MemoryRoomRepository::prompt_doc_relative_path(&room, "semantic-tags.md");
    let current = repository
        .read_asset(current_relative)
        .expect("current managed prompt asset should be readable");

    assert!(current.summary.contains("reviewer and rg"));
    assert!(
        current
            .derived_from
            .iter()
            .any(|item| item.starts_with("prompt.extract.semantic-tags.rev."))
    );

    let revision_dir = repository
        .root()
        .join("tenants")
        .join(&namespace.tenant_id)
        .join("users")
        .join(&namespace.user_id)
        .join("memory/rooms/project/room.project.prompt-library/prompt");
    let revision_files = fs::read_dir(&revision_dir)
        .expect("prompt directory should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("rev.") && name.ends_with(".md"))
        .collect::<Vec<_>>();
    assert!(!revision_files.is_empty());

    let _ = fs::remove_dir_all(root);
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
        derived_from: Vec::new(),
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
    assert!(
        messages[0]
            .content
            .contains("User prefers responses in Chinese.")
    );
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
    assert!(
        self_model
            .goals
            .iter()
            .any(|goal| goal == "Find regressions quickly")
    );
    assert!(
        self_model
            .capabilities
            .iter()
            .any(|capability| capability.name == "Code Review")
    );
    assert!(
        self_model
            .constraints
            .iter()
            .any(|constraint| constraint.description.contains("Avoid inventing behavior"))
    );
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

    let summary =
        summarize_global_preference(&input).expect("name preference summary should be generated");
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
    let organizer = LlmMemoryOrganizer::new(
        &registry,
        ModelRef::new("test", "mock"),
        WorkspaceNamespace::local_default(),
        fallback,
    );
    let input = MemoryOrganizationInput::new(MemoryNamespace::new("local", "default"), "??????")
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
    let input =
        MemoryOrganizationInput::new(MemoryNamespace::new("local", "default"), "你以后叫小八");

    let summary =
        summarize_global_preference_with_llm(&registry, &ModelRef::new("test", "mock"), &input)
            .expect("llm summary should succeed")
            .expect("summary should be present");

    assert_eq!(summary.1, MemoryKind::Preference);
    assert!(summary.0.contains("小八"));
}
