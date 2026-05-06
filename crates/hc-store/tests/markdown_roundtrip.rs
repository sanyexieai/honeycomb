use hc_store::{
    index::{
        LocalJsonVectorIndex, RebuildableIndex, VectorDocument, VectorIndex, VectorQuery,
        indexed_documents_from_markdown_index, local_hash_embedding,
        vector_documents_from_indexed_documents,
    },
    store::{MarkdownQuery, WorkspaceNamespace, WorkspaceStore, parse_markdown_document},
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Frontmatter {
    id: String,
    #[serde(rename = "type")]
    doc_type: String,
    title: String,
    tags: Vec<String>,
    status: String,
}

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
fn workspace_store_roundtrips_markdown_documents() {
    let root = unique_temp_dir("store-roundtrip");
    let store = WorkspaceStore::new(&root);
    let frontmatter = Frontmatter {
        id: "memory.task.0001".to_owned(),
        doc_type: "memory".to_owned(),
        title: "Task Memory".to_owned(),
        tags: vec!["task".to_owned(), "memory".to_owned()],
        status: "active".to_owned(),
    };

    let path = store
        .write_markdown(
            "memory/task/memory.task.0001.md",
            &frontmatter,
            "# Summary\n\nHello",
        )
        .expect("markdown should be written");
    assert!(path.exists());

    let stored: hc_store::store::StoredMarkdown<Frontmatter> = store
        .read_markdown("memory/task/memory.task.0001.md")
        .expect("markdown should be read");

    assert_eq!(stored.frontmatter, frontmatter);
    assert!(stored.body.contains("# Summary"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn parse_markdown_document_extracts_frontmatter_and_body() {
    let content = "---\nid: memory.task.0002\ntype: memory\ntitle: Example\ntags: [example]\nstatus: active\n---\n\nBody line";

    let parsed: hc_store::store::StoredMarkdown<Frontmatter> =
        parse_markdown_document(content).expect("content should parse");

    assert_eq!(parsed.frontmatter.id, "memory.task.0002");
    assert_eq!(parsed.body, "Body line");
}

#[test]
fn workspace_store_can_write_under_tenant_and_user_namespace() {
    let root = unique_temp_dir("store-namespace");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let frontmatter = Frontmatter {
        id: "memory.task.0003".to_owned(),
        doc_type: "memory".to_owned(),
        title: "Scoped Memory".to_owned(),
        tags: vec!["scoped".to_owned()],
        status: "draft".to_owned(),
    };

    let path = store
        .write_markdown_in_namespace(
            &namespace,
            "memory/task/memory.task.0003.md",
            &frontmatter,
            "Scoped body",
        )
        .expect("scoped markdown should be written");

    let rendered = path.to_string_lossy().replace('/', "\\");
    assert!(rendered.contains("tenants\\tenant-a\\users\\user-a\\memory\\task"));

    let stored: hc_store::store::StoredMarkdown<Frontmatter> = store
        .read_markdown_in_namespace(&namespace, "memory/task/memory.task.0003.md")
        .expect("scoped markdown should be read");

    assert_eq!(stored.frontmatter.id, "memory.task.0003");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_store_rebuilds_namespace_markdown_index() {
    let root = unique_temp_dir("store-index");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");

    let task_doc = r#"---
id: task.plan.0001
type: task_plan
title: Demo Task Plan
tenant_id: tenant-a
user_id: user-a
tags: [planning, rust]
status: drafted
visibility: private
created_at: 2026-04-20T12:00:00+08:00
updated_at: 2026-04-20T12:15:00+08:00
relations:
  - type: references
    target: task.demo
---

# Plan

Break the runtime work into phases.
"#;
    let assignment_doc = r#"---
id: assignment.0001
type: assignment
title: Reviewer Assignment
tenant_id: tenant-a
user_id: user-a
tags: [assignment, review]
status: assigned
visibility: private
owners: [planner]
capabilities: [review]
---

Assigned to the reviewer agent.
"#;

    fs::create_dir_all(store.resolve_in_namespace(&namespace, "plans"))
        .expect("plans directory should exist");
    fs::create_dir_all(store.resolve_in_namespace(&namespace, "decisions"))
        .expect("decisions directory should exist");
    fs::write(
        store.resolve_in_namespace(&namespace, "plans/task.plan.0001.md"),
        task_doc,
    )
    .expect("task plan should be written");
    fs::write(
        store.resolve_in_namespace(&namespace, "decisions/assignment.0001.md"),
        assignment_doc,
    )
    .expect("assignment should be written");

    let index = store
        .rebuild_markdown_index_in_namespace(&namespace)
        .expect("index should rebuild");

    assert_eq!(index.documents.len(), 2);
    assert!(store.markdown_index_path_in_namespace(&namespace).exists());
    assert_eq!(
        index.documents[0].relative_path,
        "decisions/assignment.0001.md"
    );
    assert_eq!(index.documents[1].relative_path, "plans/task.plan.0001.md");
    assert_eq!(index.documents[1].relations, vec!["task.demo".to_owned()]);
    assert_eq!(index.documents[0].owners, vec!["planner".to_owned()]);
    assert_eq!(index.documents[0].capabilities, vec!["review".to_owned()]);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_store_queries_namespace_markdown_index() {
    let root = unique_temp_dir("store-query");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");

    let memory_doc = r#"---
id: memory.task.0004
type: memory
title: Runtime Investigation
tenant_id: tenant-a
user_id: user-a
tags: [runtime, investigation]
status: active
---

Investigate runtime bootstrap and session flow.
"#;
    let decision_doc = r#"---
id: decision.0002
type: decision
title: Keep Task Flow Explicit
tenant_id: tenant-a
user_id: user-a
tags: [decision, planning]
status: accepted
---

Avoid hidden fallback behavior.
"#;

    fs::create_dir_all(store.resolve_in_namespace(&namespace, "memory"))
        .expect("memory directory should exist");
    fs::create_dir_all(store.resolve_in_namespace(&namespace, "decisions"))
        .expect("decisions directory should exist");
    fs::write(
        store.resolve_in_namespace(&namespace, "memory/memory.task.0004.md"),
        memory_doc,
    )
    .expect("memory doc should be written");
    fs::write(
        store.resolve_in_namespace(&namespace, "decisions/decision.0002.md"),
        decision_doc,
    )
    .expect("decision doc should be written");

    store
        .rebuild_markdown_index_in_namespace(&namespace)
        .expect("index should rebuild");

    let memory_matches = store
        .query_markdown_index_in_namespace(
            &namespace,
            &MarkdownQuery::default()
                .with_doc_type("memory")
                .with_tag("runtime")
                .with_text("bootstrap"),
        )
        .expect("memory query should succeed");
    assert_eq!(memory_matches.len(), 1);
    assert_eq!(memory_matches[0].id, "memory.task.0004");

    let limited_matches = store
        .query_markdown_index_in_namespace(
            &namespace,
            &MarkdownQuery::default()
                .with_path_prefix("decisions")
                .with_status("accepted")
                .with_limit(1),
        )
        .expect("decision query should succeed");
    assert_eq!(limited_matches.len(), 1);
    assert_eq!(limited_matches[0].id, "decision.0002");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_store_builds_sqlite_search_index_for_text_queries() {
    let root = unique_temp_dir("store-search-index");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");

    let room_doc = r#"---
id: memory.room.0001
type: memory_room_asset
title: Deployment Notes
tenant_id: tenant-a
user_id: user-a
tags: [deploy, qwen]
status: active
---

Qwen runtime should use the local vLLM deployment endpoint.
"#;
    let other_doc = r#"---
id: memory.room.0002
type: memory_room_asset
title: Review Notes
tenant_id: tenant-a
user_id: user-a
tags: [review]
status: active
---

Code review should prefer focused findings.
"#;

    fs::create_dir_all(store.resolve_in_namespace(&namespace, "memory/rooms/project/room.demo"))
        .expect("room directory should exist");
    fs::write(
        store.resolve_in_namespace(&namespace, "memory/rooms/project/room.demo/deployment.md"),
        room_doc,
    )
    .expect("room doc should be written");
    fs::write(
        store.resolve_in_namespace(&namespace, "memory/rooms/project/room.demo/review.md"),
        other_doc,
    )
    .expect("other room doc should be written");

    store
        .rebuild_markdown_index_in_namespace(&namespace)
        .expect("index should rebuild");

    assert!(
        store
            .markdown_search_index_path_in_namespace(&namespace)
            .exists()
    );

    let matches = store
        .query_markdown_index_in_namespace(
            &namespace,
            &MarkdownQuery::default()
                .with_doc_type("memory_room_asset")
                .with_path_prefix("memory/rooms")
                .with_text("How should qwen deployment be configured?")
                .with_limit(3),
        )
        .expect("search query should succeed");

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].id, "memory.room.0001");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn local_hash_embedding_is_stable_and_normalized() {
    let left = local_hash_embedding("中午推荐我吃什么", 64);
    let right = local_hash_embedding("中午推荐我吃什么", 64);
    assert_eq!(left, right);
    let norm = left.iter().map(|value| value * value).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 0.001);
}

#[test]
fn local_json_vector_index_is_rebuildable_and_filterable() {
    let root = unique_temp_dir("store-vector-index");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let index = LocalJsonVectorIndex::new(&root);

    let mut memory_metadata = std::collections::BTreeMap::new();
    memory_metadata.insert("doc_type".to_owned(), "memory_room_asset".to_owned());
    let mut agent_metadata = std::collections::BTreeMap::new();
    agent_metadata.insert("doc_type".to_owned(), "agent_profile".to_owned());

    let documents = vec![
        VectorDocument {
            id: "memory.food".to_owned(),
            source_path: "memory/rooms/global/food.md".to_owned(),
            embedding: vec![1.0, 0.0, 0.0],
            text_preview: "lunch preference".to_owned(),
            metadata: memory_metadata,
        },
        VectorDocument {
            id: "agent.music".to_owned(),
            source_path: "agents/music.md".to_owned(),
            embedding: vec![0.0, 1.0, 0.0],
            text_preview: "music playback".to_owned(),
            metadata: agent_metadata,
        },
    ];
    index
        .rebuild(&namespace, &documents)
        .expect("vector index should rebuild");

    let mut filters = std::collections::BTreeMap::new();
    filters.insert("doc_type".to_owned(), "memory_room_asset".to_owned());
    let hits = index
        .search(
            &namespace,
            &VectorQuery {
                embedding: vec![1.0, 0.1, 0.0],
                filters,
                limit: Some(5),
            },
        )
        .expect("vector search should succeed");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "memory.food");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn markdown_index_can_be_projected_to_vector_documents() {
    let root = unique_temp_dir("store-vector-projection");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    fs::create_dir_all(store.resolve_in_namespace(&namespace, "agents"))
        .expect("agents directory should exist");
    fs::write(
        store.resolve_in_namespace(&namespace, "agents/demo.md"),
        r#"---
id: agent.demo
type: agent_profile
title: Demo Agent
tags: [demo]
---

Demo agent routing notes.
"#,
    )
    .expect("agent doc should be written");

    let markdown_index = store
        .rebuild_markdown_index_in_namespace(&namespace)
        .expect("markdown index should rebuild");
    let documents = indexed_documents_from_markdown_index(&markdown_index);
    let vector_documents = vector_documents_from_indexed_documents(&documents, |document| {
        Ok(vec![document.text.len() as f32, document.tags.len() as f32])
    })
    .expect("vector projection should succeed");

    assert_eq!(vector_documents.len(), 1);
    assert_eq!(vector_documents[0].id, "agent.demo");
    assert_eq!(vector_documents[0].source_path, "agents/demo.md");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_store_indexes_room_asset_sidecar_metadata() {
    let root = unique_temp_dir("store-room-asset-meta");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let asset_dir =
        store.resolve_in_namespace(&namespace, "memory/rooms/task/room.task.demo/compressed");
    fs::create_dir_all(&asset_dir).expect("asset directory should exist");
    fs::write(
        asset_dir.join("min.summary.md"),
        "Persist task plans and assignment decisions together.",
    )
    .expect("asset body should be written");
    fs::write(
        asset_dir.join("min.summary.meta.json"),
        r#"{
  "id": "asset.room.task.demo.summary",
  "type": "memory_room_asset",
  "title": "Task Demo Summary",
  "tenant_id": "tenant-a",
  "user_id": "user-a",
  "visibility": "private",
  "room_id": "room.task.demo",
  "layer": "task",
  "asset_kind": "compressed",
  "memory_kind": "decision",
  "stage": "generalized",
  "form": "summary",
  "file_name": "min.summary.md",
  "tags": ["runtime"],
  "owners": [{"kind": "task", "id": "task.demo"}],
  "derived_from": [],
  "source_docs": []
}"#,
    )
    .expect("asset sidecar should be written");

    let index = store
        .rebuild_markdown_index_in_namespace(&namespace)
        .expect("index should rebuild");
    let entry = index
        .documents
        .iter()
        .find(|entry| entry.id == "asset.room.task.demo.summary")
        .expect("asset should be indexed");

    assert_eq!(entry.room_id.as_deref(), Some("room.task.demo"));
    assert_eq!(entry.layer.as_deref(), Some("task"));
    assert_eq!(entry.asset_kind.as_deref(), Some("compressed"));
    assert_eq!(entry.memory_kind.as_deref(), Some("decision"));
    assert_eq!(entry.owners, vec!["task.demo".to_owned()]);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn workspace_store_rebuilds_index_when_title_is_missing() {
    let root = unique_temp_dir("store-missing-title");
    let store = WorkspaceStore::new(&root);
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");

    let missing_title_doc = r#"---
id: human-inbox.message.0001.instance.0001
type: human_inbox
tenant_id: tenant-a
user_id: user-a
status: completed
---

# Human Inbox Message

Archived human handoff content.
"#;

    fs::create_dir_all(store.resolve_in_namespace(&namespace, "inbox/completed"))
        .expect("inbox directory should exist");
    fs::write(
        store.resolve_in_namespace(
            &namespace,
            "inbox/completed/human-inbox.message.0001.instance.0001.md",
        ),
        missing_title_doc,
    )
    .expect("missing title doc should be written");

    let index = store
        .rebuild_markdown_index_in_namespace(&namespace)
        .expect("index should rebuild even when title is missing");

    assert_eq!(index.documents.len(), 1);
    assert_eq!(
        index.documents[0].id,
        "human-inbox.message.0001.instance.0001"
    );
    assert_eq!(index.documents[0].title, "Human Inbox Message");

    let _ = fs::remove_dir_all(root);
}
