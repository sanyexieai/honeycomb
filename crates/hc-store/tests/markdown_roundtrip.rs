use hc_store::store::{WorkspaceNamespace, WorkspaceStore, parse_markdown_document};
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Frontmatter {
    id: String,
    title: String,
}

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("honeycomb-{}-{}-{}", name, std::process::id(), nanos))
}

#[test]
fn workspace_store_roundtrips_markdown_documents() {
    let root = unique_temp_dir("store-roundtrip");
    let store = WorkspaceStore::new(&root);
    let frontmatter = Frontmatter {
        id: "memory.task.0001".to_owned(),
        title: "Task Memory".to_owned(),
    };

    let path = store
        .write_markdown("memory/task/memory.task.0001.md", &frontmatter, "# Summary\n\nHello")
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
    let content = "---\nid: memory.task.0002\ntitle: Example\n---\n\nBody line";

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
        title: "Scoped Memory".to_owned(),
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
