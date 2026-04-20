use hc_memory::{
    MemoryCatalog, MemoryEntityKind, MemoryEntityRef, MemoryKind, MemoryLayer, MemoryNamespace,
    MemoryOwnerRef, MemoryQuery, MemoryRecord, MemoryRelation, MemoryRelationKind, MemoryRepository,
    MemoryRoom, MemoryRoomAsset, MemoryRoomAssetKind, MemoryRoomRepository, MemoryScope,
    MemoryVisibility,
};
use hc_store::store::WorkspaceNamespace;
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
    assert_eq!(record.namespace, MemoryNamespace::new("tenant-demo", "user-demo"));
    assert_eq!(record.visibility, MemoryVisibility::Private);
    assert!(record.derived_from.iter().any(|value| value == "instance.0001"));
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
    assert!(room
        .related_entities
        .iter()
        .any(|entity| entity.id == "crate.hc-core"));
    assert!(room
        .relations
        .iter()
        .any(|relation| relation.kind == MemoryRelationKind::About));
    assert!(room
        .source_docs
        .iter()
        .any(|path| path == "raw/doc.0001.user-request.md"));
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
    assert!(loaded
        .source_docs
        .iter()
        .any(|value| value == "raw/doc.0001.user-request.md"));

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
    assert!(
        path.to_string_lossy()
            .replace('\\', "/")
            .contains("memory/rooms/task/room.task.runtime-refactor.0001/compressed/min.0001.summary.md")
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
        .expect("memory room asset should be read");

    assert_eq!(loaded.id, asset.id);
    assert_eq!(loaded.room_id, asset.room_id);
    assert_eq!(loaded.kind, MemoryRoomAssetKind::Compressed);
    assert_eq!(loaded.memory_kind, MemoryKind::Decision);
    assert_eq!(loaded.summary, asset.summary);
    assert!(loaded.tags.iter().any(|value| value == "runtime"));
    assert!(loaded
        .owners
        .iter()
        .any(|owner| owner == &MemoryOwnerRef::task("task.runtime-refactor.0001")));

    let compressed_assets = repository
        .read_compressed_assets(&room)
        .expect("compressed assets should be listed");
    assert_eq!(compressed_assets.len(), 1);
    assert_eq!(compressed_assets[0].id, asset.id);

    let _ = fs::remove_dir_all(root);
}
