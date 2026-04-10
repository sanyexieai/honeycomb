use hc_memory::{
    MemoryCatalog, MemoryKind, MemoryNamespace, MemoryOwnerRef, MemoryQuery, MemoryRecord,
    MemoryRepository, MemoryScope, MemoryVisibility,
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
