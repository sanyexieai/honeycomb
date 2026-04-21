use hc_persona::{PersonaNamespace, PersonaRepository, PersonaVisibility, seed_persona_for_role};
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
fn persona_repository_roundtrips_profile_markdown() {
    let root = unique_temp_dir("persona-repo");
    let repository = PersonaRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let mut persona = seed_persona_for_role(
        PersonaNamespace::new("tenant-demo", "user-demo"),
        "task.demo",
        "planner",
        "planner",
        "Plan the work.",
    );
    persona.visibility = PersonaVisibility::TenantShared;

    let path = repository
        .write_profile(&persona)
        .expect("persona should be written");
    let relative = path
        .strip_prefix(&root)
        .expect("persona path should be under root")
        .strip_prefix(repository_namespace_path())
        .expect("persona path should be relative to namespace")
        .to_path_buf();

    let loaded = repository
        .read_profile(relative)
        .expect("persona should be read back");

    assert_eq!(loaded.id, persona.id);
    assert_eq!(loaded.namespace, persona.namespace);
    assert_eq!(loaded.visibility, persona.visibility);
    assert_eq!(loaded.role, persona.role);
    assert_eq!(loaded.description, persona.description);
    assert_eq!(loaded.goals, persona.goals);

    let _ = fs::remove_dir_all(root);
}

fn repository_namespace_path() -> std::path::PathBuf {
    WorkspaceNamespace::new("tenant-demo", "user-demo").scoped_prefix()
}
