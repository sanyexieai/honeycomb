use hc_memory::{
    ArtifactConsumer,
    ArtifactDraft,
    ArtifactEvolutionAction,
    ArtifactEvolutionEvent,
    // 新增：能力继承相关
    CapabilityRef,
    ExecutionContext,
    InheritanceType,
    MaterializationKind,
    MemoryAssetForm,
    MemoryAssetStage,
    MemoryCatalog,
    MemoryEntityKind,
    MemoryEntityRef,
    MemoryKind,
    MemoryLayer,
    MemoryNamespace,
    MemoryOwnerRef,
    MemoryQuery,
    MemoryRecord,
    MemoryRelation,
    MemoryRelationKind,
    MemoryRepository,
    MemoryRoom,
    MemoryRoomAsset,
    MemoryRoomAssetKind,
    MemoryRoomRepository,
    MemoryScope,
    MemoryVisibility,
    RoomCapabilityResolver,
    RoomConfig,
    RoomRoutingConfig,
    ScheduleRef,
    SkillRef,
    ToolRef,
};
use hc_store::store::WorkspaceNamespace;
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

#[test]
fn task_summary_builder_sets_expected_defaults() {
    let record = MemoryRecord::task_summary(
        "task.demo",
        "instance.0001",
        "Incubation Summary",
        "Captured useful observations.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_tag("incubation")
    .with_confidence_milli(920);

    assert_eq!(record.scope, MemoryScope::Task);
    assert_eq!(record.owner, MemoryOwnerRef::task("task.demo"));
    assert_eq!(record.kind, MemoryKind::Summary);
    assert_eq!(
        record.namespace,
        MemoryNamespace::new("tenant-demo", "user-demo")
    );
    assert_eq!(record.visibility, MemoryVisibility::Private);
    assert!(
        record
            .derived_from
            .iter()
            .any(|value| value == "instance.0001")
    );
    assert!(record.tags.iter().any(|value| value == "incubation"));
    assert_eq!(record.confidence_milli, 920);
}

#[test]
fn memory_query_filters_by_scope_owner_and_text() {
    let mut catalog = MemoryCatalog::new();
    catalog.insert(
        MemoryRecord::new(
            "memory.persona.0001",
            MemoryScope::Persona,
            MemoryOwnerRef::persona("persona.agent.reviewer"),
            MemoryKind::WorkflowMemory,
            "Review preference",
            "Prefer evidence-backed review comments.",
        )
        .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
        .with_tag("review"),
    );
    catalog.insert(
        MemoryRecord::new(
            "memory.task.0001",
            MemoryScope::Task,
            MemoryOwnerRef::task("task.demo"),
            MemoryKind::Summary,
            "Task summary",
            "Handled cargo check and review follow-up.",
        )
        .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
        .with_tag("task"),
    );

    let query = MemoryQuery {
        namespace: Some(MemoryNamespace::new("tenant-demo", "user-demo")),
        scope: Some(MemoryScope::Persona),
        owner: Some(MemoryOwnerRef::persona("persona.agent.reviewer")),
        kind: Some(MemoryKind::WorkflowMemory),
        tag: Some("review".to_owned()),
        text: Some("evidence".to_owned()),
    };

    let matches = catalog.find(&query);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, "memory.persona.0001");
}

#[test]
fn confidence_is_clamped_to_one_thousand() {
    let record = MemoryRecord::new(
        "memory.test.0001",
        MemoryScope::Global,
        MemoryOwnerRef::global(),
        MemoryKind::Knowledge,
        "Knowledge",
        "Useful fact",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_confidence_milli(5000);

    assert_eq!(record.confidence_milli, 1000);
}

#[test]
fn memory_repository_roundtrips_markdown_records() {
    let root = unique_temp_dir("memory-repo");
    let repository = MemoryRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let record = MemoryRecord::task_summary(
        "task.demo",
        "instance.0002",
        "Task Memory",
        "Summarized the incubation output.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_tag("incubation");

    let path = repository
        .write_record(&record)
        .expect("memory record should be written");
    assert!(path.exists());
    let rendered = path.to_string_lossy().replace('/', "\\");
    assert!(rendered.contains("tenants\\tenant-demo\\users\\user-demo\\memory\\task"));

    let root_relative = path
        .strip_prefix(&root)
        .expect("record path should be under repo root")
        .to_path_buf();
    let relative = root_relative
        .strip_prefix(repository.namespace().scoped_prefix())
        .expect("record path should be relative to namespace root")
        .to_path_buf();
    let loaded = repository
        .read_record(relative)
        .expect("memory record should be read");

    assert_eq!(loaded.id, record.id);
    assert_eq!(loaded.scope, record.scope);
    assert_eq!(loaded.owner, record.owner);
    assert_eq!(loaded.kind, record.kind);
    assert_eq!(loaded.namespace, record.namespace);
    assert_eq!(loaded.visibility, record.visibility);
    assert_eq!(loaded.summary, record.summary);
    assert!(loaded.tags.iter().any(|value| value == "incubation"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn tenant_shared_memory_is_visible_within_same_tenant_only() {
    let shared = MemoryRecord::new(
        "memory.persona.shared.0001",
        MemoryScope::Persona,
        MemoryOwnerRef::persona("persona.agent.shared"),
        MemoryKind::Knowledge,
        "Shared practice",
        "Useful tenant-wide guidance.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "alice"))
    .with_visibility(MemoryVisibility::TenantShared);

    assert!(shared.is_visible_to(&MemoryNamespace::new("tenant-demo", "bob")));
    assert!(!shared.is_visible_to(&MemoryNamespace::new("tenant-other", "carol")));
}

#[test]
fn memory_room_captures_layer_entities_and_relations() {
    let room = MemoryRoom::new(
        "room.task.runtime-refactor.0001",
        MemoryLayer::Task,
        "Runtime Refactor Task Room",
        "Tracks planning, assignment, and execution memory for one task.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_tag("runtime")
    .with_tag("assignment")
    .with_related_entity(MemoryEntityRef::new(
        MemoryEntityKind::Task,
        "task.runtime-refactor.0001",
    ))
    .with_related_entity(MemoryEntityRef::new(
        MemoryEntityKind::Crate,
        "crate.hc-core",
    ))
    .with_relation(
        MemoryRelation::new(
            MemoryRelationKind::About,
            "room.topic.runtime-architecture.0001",
        )
        .with_detail("task room is about runtime architecture"),
    )
    .with_source_doc("raw/doc.0001.user-request.md")
    .with_derived_doc("compressed/min.0001.summary.md");

    assert_eq!(room.layer, MemoryLayer::Task);
    assert_eq!(room.status, "active");
    assert!(room.tags.iter().any(|tag| tag == "runtime"));
    assert!(
        room.related_entities
            .iter()
            .any(|entity| entity.id == "crate.hc-core")
    );
    assert!(
        room.relations
            .iter()
            .any(|relation| relation.kind == MemoryRelationKind::About)
    );
    assert!(
        room.source_docs
            .iter()
            .any(|path| path == "raw/doc.0001.user-request.md")
    );
}

#[test]
fn tenant_shared_room_is_visible_within_same_tenant_only() {
    let room = MemoryRoom::new(
        "room.project.honeycomb.0001",
        MemoryLayer::Project,
        "Honeycomb Project Room",
        "Project-level architecture and conventions.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "alice"))
    .with_visibility(MemoryVisibility::TenantShared);

    assert!(room.is_visible_to(&MemoryNamespace::new("tenant-demo", "bob")));
    assert!(!room.is_visible_to(&MemoryNamespace::new("tenant-other", "carol")));
}

#[test]
fn memory_room_repository_roundtrips_room_markdown() {
    let root = unique_temp_dir("memory-room-repo");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.task.runtime-refactor.0001",
        MemoryLayer::Task,
        "Runtime Refactor Task Room",
        "Tracks planning and execution for a runtime refactor task.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_tag("runtime")
    .with_related_entity(MemoryEntityRef::new(
        MemoryEntityKind::Task,
        "task.runtime-refactor.0001",
    ))
    .with_relation(MemoryRelation::new(
        MemoryRelationKind::BelongsTo,
        "room.project.honeycomb.0001",
    ))
    .with_source_doc("raw/doc.0001.user-request.md")
    .with_derived_doc("compressed/min.0001.summary.md");

    let path = repository
        .write_room(&room)
        .expect("memory room should be written");
    assert!(path.exists());
    let rendered = path.to_string_lossy().replace('/', "\\");
    assert!(rendered.contains(
        "tenants\\tenant-demo\\users\\user-demo\\memory\\rooms\\task\\room.task.runtime-refactor.0001\\room.md"
    ));

    let root_relative = path
        .strip_prefix(&root)
        .expect("room path should be under repo root")
        .to_path_buf();
    let relative = root_relative
        .strip_prefix(repository.namespace().scoped_prefix())
        .expect("room path should be relative to namespace root")
        .to_path_buf();
    let loaded = repository
        .read_room(relative)
        .expect("memory room should be read");

    assert_eq!(loaded.id, room.id);
    assert_eq!(loaded.layer, room.layer);
    assert_eq!(loaded.title, room.title);
    assert_eq!(loaded.namespace, room.namespace);
    assert_eq!(loaded.visibility, room.visibility);
    assert_eq!(loaded.summary, room.summary);
    assert!(loaded.tags.iter().any(|value| value == "runtime"));
    assert!(
        loaded
            .source_docs
            .iter()
            .any(|value| value == "raw/doc.0001.user-request.md")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn memory_room_repository_builds_expected_asset_paths() {
    let room = MemoryRoom::new(
        "room.topic.streaming.0001",
        MemoryLayer::Topic,
        "Streaming Topic Room",
        "Collects streaming-related memory assets.",
    );

    assert_eq!(
        MemoryRoomRepository::relative_path_for(&room)
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/room.md"
    );
    assert_eq!(
        MemoryRoomRepository::raw_doc_relative_path(&room, "doc.0001.md")
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/raw/doc.0001.md"
    );
    assert_eq!(
        MemoryRoomRepository::compressed_doc_relative_path(&room, "min.0001.md")
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/compressed/min.0001.md"
    );
    assert_eq!(
        MemoryRoomRepository::literary_doc_relative_path(&room, "wenyan.0001.md")
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/literary/wenyan.0001.md"
    );
    assert_eq!(
        MemoryRoomRepository::prompt_doc_relative_path(&room, "prompt.style.md")
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/prompt/prompt.style.md"
    );
    assert_eq!(
        MemoryRoomRepository::facts_relative_path(&room)
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/facts.md"
    );
    assert_eq!(
        MemoryRoomRepository::timeline_relative_path(&room)
            .to_string_lossy()
            .replace('\\', "/"),
        "memory/rooms/topic/room.topic.streaming.0001/timeline.md"
    );
}

#[test]
fn memory_room_repository_roundtrips_compressed_assets() {
    let root = unique_temp_dir("memory-room-asset-repo");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.task.runtime-refactor.0001",
        MemoryLayer::Task,
        "Runtime Refactor Task Room",
        "Tracks planning and execution for a runtime refactor task.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"));
    repository
        .write_room(&room)
        .expect("memory room should be written");

    let asset = MemoryRoomAsset::new(
        "asset.room.task.runtime-refactor.0001.summary",
        room.id.clone(),
        "min.0001.summary.md",
        MemoryLayer::Task,
        MemoryRoomAssetKind::Compressed,
        "Runtime Refactor Summary",
        "Keep task plans and assignment decisions persisted together.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_memory_kind(MemoryKind::Decision)
    .with_owner(MemoryOwnerRef::task("task.runtime-refactor.0001"))
    .with_tag("runtime")
    .with_derived_from("raw/doc.0001.user-request.md")
    .with_source_doc("raw/doc.0001.user-request.md");

    let path = repository
        .write_asset(&room, &asset)
        .expect("memory room asset should be written");
    assert!(path.exists());
    assert!(path.to_string_lossy().replace('\\', "/").contains(
        "memory/rooms/task/room.task.runtime-refactor.0001/compressed/min.0001.summary.md"
    ));

    let root_relative = path
        .strip_prefix(&root)
        .expect("asset path should be under repo root")
        .to_path_buf();
    let relative = root_relative
        .strip_prefix(repository.namespace().scoped_prefix())
        .expect("asset path should be relative to namespace root")
        .to_path_buf();
    let loaded = repository
        .read_asset(relative)
        .expect("memory room asset should be read");

    assert_eq!(loaded.id, asset.id);
    assert_eq!(loaded.room_id, asset.room_id);
    assert_eq!(loaded.kind, MemoryRoomAssetKind::Compressed);
    assert_eq!(loaded.memory_kind, MemoryKind::Decision);
    assert_eq!(loaded.stage, MemoryAssetStage::Generalized);
    assert_eq!(loaded.form, MemoryAssetForm::Summary);
    assert_eq!(loaded.summary, asset.summary);
    assert!(loaded.tags.iter().any(|value| value == "runtime"));
    assert!(
        loaded
            .owners
            .iter()
            .any(|owner| owner == &MemoryOwnerRef::task("task.runtime-refactor.0001"))
    );

    let compressed_assets = repository
        .read_compressed_assets(&room)
        .expect("compressed assets should be listed");
    assert_eq!(compressed_assets.len(), 1);
    assert_eq!(compressed_assets[0].id, asset.id);

    let indexed_room = repository
        .read_room(MemoryRoomRepository::relative_path_for(&room))
        .expect("room should be readable after writing asset");
    assert!(
        indexed_room
            .derived_docs
            .iter()
            .any(|value| value == "compressed/min.0001.summary.md")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn memory_room_repository_indexes_raw_assets_as_sources() {
    let root = unique_temp_dir("memory-room-source-index");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.chat.local.default.0001",
        MemoryLayer::Chat,
        "Chat Room",
        "Interactive chat transcript and compressed reply memory.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"));
    repository
        .write_room(&room)
        .expect("memory room should be written");

    let asset = MemoryRoomAsset::new(
        "asset.room.chat.local.default.0001.turn.1.user",
        room.id.clone(),
        "0001.user-message.md",
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Raw,
        "User Message 1",
        "Please summarize the runtime refactor tradeoffs.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_memory_kind(MemoryKind::Knowledge)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_tag("chat")
    .with_tag("user");

    repository
        .write_asset(&room, &asset)
        .expect("raw room asset should be written");

    let indexed_room = repository
        .read_room(MemoryRoomRepository::relative_path_for(&room))
        .expect("room should be readable after writing raw asset");
    assert!(
        indexed_room
            .source_docs
            .iter()
            .any(|value| value == "raw/0001.user-message.md")
    );
    assert_eq!(
        indexed_room
            .source_docs
            .iter()
            .filter(|value| *value == "raw/0001.user-message.md")
            .count(),
        1
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn memory_room_repository_writes_evolution_events_to_timeline() {
    let root = unique_temp_dir("memory-room-timeline");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.tool.rg",
        MemoryLayer::Project,
        "RG Tool Room",
        "Reusable rg search guidance.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_tag("tool")
    .with_tag("rg");
    repository
        .write_room(&room)
        .expect("memory room should be written");

    let event = ArtifactEvolutionEvent::new(
        "event.asset.room.tool.rg.recipe.promoted.1",
        "asset.room.tool.rg.recipe",
        room.id.clone(),
        ArtifactEvolutionAction::Promoted,
        "eligible for promotion to compiled guidance",
    )
    .with_input("execution_succeeded")
    .with_output("promote_to:compiled")
    .with_tag("tool")
    .with_tag("rg")
    .with_created_at_ms(1234);

    let path = repository
        .write_evolution_event(&room, &event)
        .expect("timeline event should be written");
    assert!(path.exists());
    assert!(
        path.to_string_lossy()
            .replace('\\', "/")
            .ends_with("memory/rooms/project/room.tool.rg/timeline.md")
    );

    let contents = fs::read_to_string(&path).expect("timeline contents should be readable");
    assert!(contents.contains("event.asset.room.tool.rg.recipe.promoted.1"));
    assert!(contents.contains("eligible for promotion to compiled guidance"));
    assert!(contents.contains("promote_to:compiled"));

    let indexed_room = repository
        .read_room(MemoryRoomRepository::relative_path_for(&room))
        .expect("room should be readable after writing timeline event");
    assert!(
        indexed_room
            .derived_docs
            .iter()
            .any(|value| value == "timeline.md")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn memory_room_repository_materializes_evolution_events_with_metadata() {
    let root = unique_temp_dir("memory-room-materialize-event");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.tool.cargo-test",
        MemoryLayer::Project,
        "Cargo Test Tool Room",
        "Reusable cargo test guidance.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"));
    repository
        .write_room(&room)
        .expect("memory room should be written");

    let event = ArtifactEvolutionEvent::new(
        "event.asset.room.tool.cargo-test.recipe.promoted.1",
        "asset.room.tool.cargo-test.recipe",
        room.id.clone(),
        ArtifactEvolutionAction::Promoted,
        "eligible for promotion to compiled guidance",
    );

    let materialized = repository
        .materialize_evolution_event(&room, &event)
        .expect("evolution event should materialize");

    assert_eq!(materialized.kind, MaterializationKind::EvolutionEvent);
    assert_eq!(materialized.room_id, room.id);
    assert_eq!(materialized.room_relative_path, "timeline.md");
    assert!(materialized.path.exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn room_asset_preference_maps_to_policy_form() {
    let asset = MemoryRoomAsset::new(
        "asset.room.global.local.default.pref-language",
        "room.global.local.default",
        "pref.language.md",
        MemoryLayer::Global,
        MemoryRoomAssetKind::Compressed,
        "Language Preference",
        "User prefers responses in Chinese.",
    )
    .with_memory_kind(MemoryKind::Preference);

    assert_eq!(asset.stage, MemoryAssetStage::Generalized);
    assert_eq!(asset.form, MemoryAssetForm::Policy);
}

#[test]
fn memory_room_asset_can_be_built_from_artifact_draft() {
    let draft = ArtifactDraft::new(
        "room.task.runtime-refactor.0001",
        MemoryLayer::Task,
        MemoryRoomAssetKind::Compressed,
        "Runtime Refactor Policy",
        "Prefer keeping runtime ownership boundaries explicit.",
    )
    .with_memory_kind(MemoryKind::Preference)
    .with_stage(MemoryAssetStage::Compiled)
    .with_form(MemoryAssetForm::Prompt)
    .with_visibility(MemoryVisibility::TenantShared)
    .with_tag("runtime")
    .with_owner(MemoryOwnerRef::task("task.runtime-refactor.0001"))
    .with_consumer(ArtifactConsumer::PromptComposer)
    .with_derived_from("asset.room.task.runtime-refactor.0001.summary")
    .with_source_doc("prompts/runtime-refactor.md")
    .with_file_name("prompt.runtime-refactor.md");

    let asset = MemoryRoomAsset::from_draft(
        "asset.room.task.runtime-refactor.0001.prompt",
        MemoryNamespace::new("tenant-demo", "user-demo"),
        draft,
    );

    assert_eq!(asset.id, "asset.room.task.runtime-refactor.0001.prompt");
    assert_eq!(asset.room_id, "room.task.runtime-refactor.0001");
    assert_eq!(asset.file_name, "prompt.runtime-refactor.md");
    assert_eq!(asset.memory_kind, MemoryKind::Preference);
    assert_eq!(asset.stage, MemoryAssetStage::Compiled);
    assert_eq!(asset.form, MemoryAssetForm::Prompt);
    assert_eq!(asset.visibility, MemoryVisibility::TenantShared);
    assert_eq!(
        asset.namespace,
        MemoryNamespace::new("tenant-demo", "user-demo")
    );
    assert!(asset.tags.iter().any(|tag| tag == "runtime"));
    assert!(
        asset
            .owners
            .iter()
            .any(|owner| owner == &MemoryOwnerRef::task("task.runtime-refactor.0001"))
    );
    assert!(
        asset
            .derived_from
            .iter()
            .any(|source| source == "asset.room.task.runtime-refactor.0001.summary")
    );
    assert!(
        asset
            .source_docs
            .iter()
            .any(|doc| doc == "prompts/runtime-refactor.md")
    );
}

#[test]
fn memory_room_repository_writes_artifact_drafts() {
    let root = unique_temp_dir("memory-room-draft-repo");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.project.honeycomb",
        MemoryLayer::Project,
        "Honeycomb Project Room",
        "Project-level architecture and conventions.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"));
    repository
        .write_room(&room)
        .expect("memory room should be written");

    let draft = ArtifactDraft::new(
        room.id.clone(),
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "Architecture Prompt Memory",
        "Preserve layer boundaries when adding new runtime abstractions.",
    )
    .with_memory_kind(MemoryKind::WorkflowMemory)
    .with_stage(MemoryAssetStage::Compiled)
    .with_form(MemoryAssetForm::Prompt)
    .with_tag("architecture")
    .with_tag("project")
    .with_consumer(ArtifactConsumer::PromptComposer)
    .with_owner(MemoryOwnerRef::project(room.id.clone()))
    .with_file_name("prompt.architecture.md");

    let path = repository
        .write_artifact_draft(
            &room,
            "asset.room.project.honeycomb.prompt.architecture",
            draft,
        )
        .expect("artifact draft should be written");
    assert!(path.exists());
    assert!(
        path.to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/project/room.project.honeycomb/prompt/prompt.architecture.md")
    );

    let root_relative = path
        .strip_prefix(&root)
        .expect("asset path should be under repo root")
        .to_path_buf();
    let relative = root_relative
        .strip_prefix(repository.namespace().scoped_prefix())
        .expect("asset path should be relative to namespace root")
        .to_path_buf();
    let loaded = repository
        .read_asset(relative)
        .expect("artifact draft should roundtrip through asset loading");

    assert_eq!(
        loaded.id,
        "asset.room.project.honeycomb.prompt.architecture"
    );
    assert_eq!(loaded.file_name, "prompt.architecture.md");
    assert_eq!(loaded.stage, MemoryAssetStage::Compiled);
    assert_eq!(loaded.form, MemoryAssetForm::Prompt);
    assert_eq!(loaded.memory_kind, MemoryKind::WorkflowMemory);
    assert!(loaded.tags.iter().any(|tag| tag == "architecture"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn memory_room_repository_materializes_artifact_drafts_with_metadata() {
    let root = unique_temp_dir("memory-room-materialize-draft");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let room = MemoryRoom::new(
        "room.project.materialization",
        MemoryLayer::Project,
        "Materialization Room",
        "Tracks artifact materialization.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"));
    repository
        .write_room(&room)
        .expect("memory room should be written");

    let draft = ArtifactDraft::new(
        room.id.clone(),
        MemoryLayer::Project,
        MemoryRoomAssetKind::Compressed,
        "Prompt Contract",
        "Return compact markdown sections.",
    )
    .with_stage(MemoryAssetStage::Compiled)
    .with_form(MemoryAssetForm::Prompt)
    .with_file_name("prompt.contract.md");

    let materialized = repository
        .materialize_artifact_draft(
            &room,
            "asset.room.project.materialization.prompt.contract",
            draft,
        )
        .expect("artifact draft should materialize");

    assert_eq!(materialized.kind, MaterializationKind::Draft);
    assert_eq!(materialized.room_id, room.id);
    assert_eq!(materialized.room_relative_path, "prompt/prompt.contract.md");
    assert!(materialized.path.exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn room_capability_inheritance_roundtrips_through_storage() {
    let root = unique_temp_dir("room-capability-inheritance");
    let repository = MemoryRoomRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );

    // 创建带有能力继承的 Room
    let room = MemoryRoom::new(
        "room.project.honeycomb.capability-test",
        MemoryLayer::Project,
        "Honeycomb Capability Test Room",
        "Tests capability inheritance for Honeycomb project.",
    )
    .with_namespace(MemoryNamespace::new("tenant-demo", "user-demo"))
    .with_tag("rust")
    .with_tag("project")
    .with_inherited_capability(
        CapabilityRef::new("capability.rust.development")
            .with_inheritance_type(InheritanceType::Manual),
    )
    .with_inherited_tool(
        ToolRef::new("tool.cargo-test")
            .with_inheritance_type(InheritanceType::AutoDiscovered)
            .with_command_override(vec![
                "cargo".to_string(),
                "test".to_string(),
                "--all".to_string(),
            ]),
    )
    .with_inherited_skill(
        SkillRef::new("skill.rust.review")
            .with_inheritance_type(InheritanceType::FromParent)
            .with_instructions_override(
                "Focus on memory safety and performance in Rust code reviews.",
            ),
    )
    .with_inherited_schedule(
        ScheduleRef::new("schedule.daily-build")
            .with_inheritance_type(InheritanceType::FromSibling)
            .enabled(),
    )
    .with_room_config(
        RoomConfig::new()
            .with_auto_inherit_parent()
            .with_tool_filter_tag("rust")
            .with_execution_context(
                ExecutionContext::new()
                    .with_default_namespace("honeycomb")
                    .with_environment_var("RUST_LOG", "debug")
                    .with_working_directory("/workspace/honeycomb"),
            )
            .with_routing(
                RoomRoutingConfig::new()
                    .with_enabled_provider("mcp_tool")
                    .with_disabled_provider("timed")
                    .with_provider_weight("mcp_tool", 250)
                    .with_tool_whitelist("tool.cargo-test")
                    .with_tool_blacklist("tool.rm")
                    .with_capability_whitelist("capability.rust.development")
                    .with_capability_blacklist("capability.forbidden")
                    .with_skill_whitelist("skill.rust.review")
                    .with_skill_blacklist("skill.blocked")
                    .with_provider_argument_override("mcp_tool", {
                        let mut args = serde_json::Map::new();
                        args.insert(
                            "audience".to_string(),
                            serde_json::Value::String("room".to_string()),
                        );
                        args
                    }),
            ),
    );

    // 写入房间
    let path = repository
        .write_room(&room)
        .expect("room with capabilities should be written");
    assert!(path.exists());

    // 读取房间
    let loaded_room = repository
        .read_room(MemoryRoomRepository::relative_path_for(&room))
        .expect("room with capabilities should be read");

    // 验证基本信息
    assert_eq!(loaded_room.id, room.id);
    assert_eq!(loaded_room.title, room.title);
    assert_eq!(loaded_room.layer, MemoryLayer::Project);

    // 验证能力继承
    assert_eq!(loaded_room.inherited_capabilities.len(), 1);
    assert_eq!(
        loaded_room.inherited_capabilities[0].id,
        "capability.rust.development"
    );
    assert_eq!(
        loaded_room.inherited_capabilities[0].inheritance_type,
        InheritanceType::Manual
    );

    assert_eq!(loaded_room.inherited_tools.len(), 1);
    assert_eq!(loaded_room.inherited_tools[0].id, "tool.cargo-test");
    assert_eq!(
        loaded_room.inherited_tools[0].inheritance_type,
        InheritanceType::AutoDiscovered
    );
    assert_eq!(
        loaded_room.inherited_tools[0].command_override,
        Some(vec![
            "cargo".to_string(),
            "test".to_string(),
            "--all".to_string()
        ])
    );

    assert_eq!(loaded_room.inherited_skills.len(), 1);
    assert_eq!(loaded_room.inherited_skills[0].id, "skill.rust.review");
    assert_eq!(
        loaded_room.inherited_skills[0].inheritance_type,
        InheritanceType::FromParent
    );
    assert_eq!(
        loaded_room.inherited_skills[0].instructions_override,
        Some("Focus on memory safety and performance in Rust code reviews.".to_string())
    );

    assert_eq!(loaded_room.inherited_schedules.len(), 1);
    assert_eq!(
        loaded_room.inherited_schedules[0].id,
        "schedule.daily-build"
    );
    assert_eq!(
        loaded_room.inherited_schedules[0].inheritance_type,
        InheritanceType::FromSibling
    );
    assert!(loaded_room.inherited_schedules[0].enabled_in_room);

    // 验证房间配置
    assert!(loaded_room.room_config.auto_inherit_parent);
    assert!(!loaded_room.room_config.auto_inherit_siblings);
    assert!(
        loaded_room
            .room_config
            .tool_filter_tags
            .contains(&"rust".to_string())
    );
    assert_eq!(
        loaded_room.room_config.execution_context.default_namespace,
        Some("honeycomb".to_string())
    );
    assert_eq!(
        loaded_room
            .room_config
            .execution_context
            .environment
            .get("RUST_LOG"),
        Some(&"debug".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .enabled_providers
            .contains(&"mcp_tool".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .disabled_providers
            .contains(&"timed".to_string())
    );
    assert_eq!(
        loaded_room
            .room_config
            .routing
            .provider_weights
            .get("mcp_tool"),
        Some(&250)
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .tool_whitelist
            .contains(&"tool.cargo-test".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .tool_blacklist
            .contains(&"tool.rm".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .capability_whitelist
            .contains(&"capability.rust.development".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .capability_blacklist
            .contains(&"capability.forbidden".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .skill_whitelist
            .contains(&"skill.rust.review".to_string())
    );
    assert!(
        loaded_room
            .room_config
            .routing
            .skill_blacklist
            .contains(&"skill.blocked".to_string())
    );
    assert_eq!(
        loaded_room
            .room_config
            .routing
            .provider_argument_overrides
            .get("mcp_tool")
            .and_then(|args| args.get("audience")),
        Some(&serde_json::Value::String("room".to_string()))
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn room_capability_resolver_resolves_inherited_capabilities() {
    let namespace = MemoryNamespace::new("tenant-demo", "user-demo");
    let resolver = RoomCapabilityResolver::new(namespace);

    // 创建带有各种继承能力的房间
    let room = MemoryRoom::new(
        "room.task.rust-refactor",
        MemoryLayer::Task,
        "Rust Refactor Task Room",
        "Memory room for Rust code refactoring task.",
    )
    .with_tag("rust")
    .with_tag("refactor")
    .with_related_entity(MemoryEntityRef::new(
        MemoryEntityKind::Project,
        "project.honeycomb",
    ))
    .with_related_entity(MemoryEntityRef::new(
        MemoryEntityKind::Crate,
        "crate.hc-memory",
    ))
    .with_inherited_tool(
        ToolRef::new("tool.manual-added").with_inheritance_type(InheritanceType::Manual),
    )
    .with_room_config(
        RoomConfig::new()
            .with_auto_inherit_parent()
            .with_auto_inherit_siblings(),
    );

    // 解析能力
    let resolved = resolver
        .resolve_room_capabilities(&room)
        .expect("should resolve capabilities");

    // 验证房间 ID
    assert_eq!(resolved.room_id, "room.task.rust-refactor");

    // 验证手动添加的工具存在
    assert!(
        resolved
            .tools
            .iter()
            .any(|t| t.tool_ref.id == "tool.manual-added")
    );

    // 验证自动发现的能力
    // 基于 "rust" 标签应该发现 Rust 工具
    assert!(
        resolved
            .tools
            .iter()
            .any(|t| t.tool_ref.id == "tool.cargo-check")
    );
    assert!(
        resolved
            .tools
            .iter()
            .any(|t| t.tool_ref.id == "tool.cargo-test")
    );

    // 基于项目实体应该发现 Honeycomb 工具
    assert!(resolved.tools.iter().any(|t| t.tool_ref.id == "tool.rg"));
    assert!(
        resolved
            .tools
            .iter()
            .any(|t| t.tool_ref.id == "tool.local-file.read")
    );

    // 基于 "refactor" 标签应该发现重构工具
    // (由于我们在 inherit_task_capabilities 中处理包含 "refactor" 的任务)
    // 这个测试验证了自动发现机制在工作

    // 验证继承类型
    let auto_discovered_tools = resolved
        .tools
        .iter()
        .filter(|t| t.tool_ref.inheritance_type == InheritanceType::AutoDiscovered)
        .count();
    assert!(auto_discovered_tools > 0);
}

#[test]
fn room_capability_management_methods_work() {
    let mut room = MemoryRoom::new(
        "room.test.capability-management",
        MemoryLayer::Task,
        "Test Room",
        "Testing capability management methods.",
    );

    // 测试添加能力
    room.add_capability(CapabilityRef::new("capability.test1"));
    room.add_tool(ToolRef::new("tool.test1"));
    room.add_skill(SkillRef::new("skill.test1"));
    room.add_schedule(ScheduleRef::new("schedule.test1"));

    // 验证能力存在
    assert!(room.has_capability("capability.test1"));
    assert!(room.has_tool("tool.test1"));
    assert!(room.has_skill("skill.test1"));
    assert!(room.has_schedule("schedule.test1"));

    // 测试重复添加（应该被忽略）
    room.add_capability(CapabilityRef::new("capability.test1"));
    assert_eq!(room.inherited_capabilities.len(), 1);

    // 测试移除能力
    room.remove_capability("capability.test1");
    room.remove_tool("tool.test1");
    room.remove_skill("skill.test1");
    room.remove_schedule("schedule.test1");

    // 验证能力已移除
    assert!(!room.has_capability("capability.test1"));
    assert!(!room.has_tool("tool.test1"));
    assert!(!room.has_skill("skill.test1"));
    assert!(!room.has_schedule("schedule.test1"));
}

#[test]
fn capability_ref_builder_methods_work() {
    let capability_ref = CapabilityRef::new("capability.test")
        .with_source_room("room.source")
        .with_inheritance_type(InheritanceType::FromParent)
        .with_override_config(serde_json::json!({"key": "value"}));

    assert_eq!(capability_ref.id, "capability.test");
    assert_eq!(
        capability_ref.source_room_id,
        Some("room.source".to_string())
    );
    assert_eq!(capability_ref.inheritance_type, InheritanceType::FromParent);
    assert!(capability_ref.override_config.is_some());

    let tool_ref = ToolRef::new("tool.test")
        .with_source_room("room.source")
        .with_inheritance_type(InheritanceType::AutoDiscovered)
        .with_command_override(vec!["custom".to_string(), "command".to_string()]);

    assert_eq!(tool_ref.id, "tool.test");
    assert_eq!(tool_ref.inheritance_type, InheritanceType::AutoDiscovered);
    assert_eq!(
        tool_ref.command_override,
        Some(vec!["custom".to_string(), "command".to_string()])
    );

    let skill_ref = SkillRef::new("skill.test").with_instructions_override("Custom instructions");

    assert_eq!(skill_ref.id, "skill.test");
    assert_eq!(
        skill_ref.instructions_override,
        Some("Custom instructions".to_string())
    );

    let schedule_ref = ScheduleRef::new("schedule.test")
        .enabled()
        .with_schedule_override(serde_json::json!({"interval": 3600}));

    assert_eq!(schedule_ref.id, "schedule.test");
    assert!(schedule_ref.enabled_in_room);
    assert!(schedule_ref.schedule_override.is_some());

    // 测试 disabled
    let disabled_schedule = ScheduleRef::new("schedule.disabled").disabled();
    assert!(!disabled_schedule.enabled_in_room);
}
