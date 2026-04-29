use super::{SkillCatalog, SkillProfile, SkillRepository};
use hc_capability::ModelDependence;
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::{ToolExecutionKind, ToolProvider};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn skill_repository_roundtrips_markdown_profile() {
    let root = unique_temp_dir("skill-repo");
    let namespace = WorkspaceNamespace::new("tenant-a", "alice");
    let repository = SkillRepository::with_namespace(&root, namespace.clone());
    let skill = SkillProfile::new("skill.search.workspace", "Workspace Search")
        .with_namespace(namespace)
        .with_description("Searches the current workspace with rg.")
        .with_instructions("Use rg first, then narrow by path if the result set is broad.")
        .with_tool_id("tool.skill.search.workspace")
        .with_execution_kind(ToolExecutionKind::Cli)
        .with_model_dependence(ModelDependence::Optional)
        .with_default_command(["rg", "-n"])
        .with_tool_ref("tool.rg")
        .with_tag("search");

    repository
        .write_profile(&skill)
        .expect("skill profile should be written");
    let loaded = repository
        .read_profile("skills/skill.search.workspace.md")
        .expect("skill profile should be read");

    assert_eq!(loaded.id, skill.id);
    assert_eq!(loaded.tool_id, skill.tool_id);
    assert_eq!(loaded.default_command, skill.default_command);
    assert_eq!(loaded.instructions, skill.instructions);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn skill_catalog_acts_as_tool_provider() {
    let mut catalog = SkillCatalog::new();
    catalog.insert(
        SkillProfile::new("skill.test.runner", "Test Runner")
            .with_description("Runs a narrow test target.")
            .with_instructions("Prefer cargo test <name> over broad test sweeps.")
            .with_execution_kind(ToolExecutionKind::Cli)
            .with_default_command(["cargo", "test"])
            .with_tag("testing"),
    );

    let tools = catalog.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].id, "tool.test.runner");
    assert!(tools[0].tags.iter().any(|tag| tag == "skill"));
}

#[test]
fn repository_loads_catalog_from_namespace_skills() {
    let root = unique_temp_dir("skill-catalog");
    let repository = SkillRepository::new(&root);
    repository
        .write_profile(
            &SkillProfile::new("skill.docs.lookup", "Docs Lookup")
                .with_description("Looks up project documentation.")
                .with_instructions("Check docs before making code changes.")
                .with_execution_kind(ToolExecutionKind::Builtin),
        )
        .expect("skill should be written");

    let catalog = repository.load_catalog().expect("catalog should load");
    assert!(catalog.get("skill.docs.lookup").is_some());
    assert_eq!(catalog.list_tools().len(), 1);

    let _ = fs::remove_dir_all(root);
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should work")
        .as_nanos();
    std::env::temp_dir().join(format!("hc-{prefix}-{stamp}"))
}
