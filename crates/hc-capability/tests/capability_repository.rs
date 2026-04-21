use hc_capability::{
    CapabilityNamespace, CapabilityRepository, CapabilityVisibility, seed_capability_for_role,
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
fn capability_repository_roundtrips_profile_markdown() {
    let root = unique_temp_dir("capability-repo");
    let repository = CapabilityRepository::with_namespace(
        &root,
        WorkspaceNamespace::new("tenant-demo", "user-demo"),
    );
    let capability = seed_capability_for_role(
        CapabilityNamespace::new("tenant-demo", "user-demo"),
        "reviewer",
    )
    .with_visibility(CapabilityVisibility::TenantShared);

    let path = repository
        .write_profile(&capability)
        .expect("capability should be written");
    let relative = path
        .strip_prefix(&root)
        .expect("capability path should be under root")
        .strip_prefix(repository_namespace_path())
        .expect("capability path should be relative to namespace")
        .to_path_buf();

    let loaded = repository
        .read_profile(relative)
        .expect("capability should be read back");

    assert_eq!(loaded.id, capability.id);
    assert_eq!(loaded.namespace, capability.namespace);
    assert_eq!(loaded.visibility, capability.visibility);
    assert_eq!(loaded.domains, capability.domains);
    assert_eq!(loaded.skills, capability.skills);
    assert_eq!(loaded.description, capability.description);

    let _ = fs::remove_dir_all(root);
}

fn repository_namespace_path() -> std::path::PathBuf {
    WorkspaceNamespace::new("tenant-demo", "user-demo").scoped_prefix()
}
