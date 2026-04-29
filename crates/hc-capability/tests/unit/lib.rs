use super::*;

#[test]
fn tenant_shared_capability_is_visible_within_same_tenant() {
    let capability =
        seed_capability_for_role(CapabilityNamespace::new("tenant-a", "alice"), "planner");

    assert!(capability.is_visible_to(&CapabilityNamespace::new("tenant-a", "bob")));
    assert!(!capability.is_visible_to(&CapabilityNamespace::new("tenant-b", "carol")));
}

#[test]
fn custom_capability_builder_keeps_namespace_and_visibility() {
    let capability = CapabilityProfile::new("capability.custom.rust", "Rust Coding")
        .with_namespace(CapabilityNamespace::new("tenant-a", "alice"))
        .with_visibility(CapabilityVisibility::CrossTenantShared)
        .with_tier(CapabilityTier::KnowledgeInterface)
        .with_model_dependence(ModelDependence::Required)
        .with_domain("rust")
        .with_skill("debugging")
        .with_input_type(CapabilityInputType::NaturalLanguage)
        .with_output_type(CapabilityOutputType::ChatReply)
        .with_tag("shared");

    assert_eq!(capability.namespace.tenant_id, "tenant-a");
    assert_eq!(
        capability.visibility,
        CapabilityVisibility::CrossTenantShared
    );
    assert!(capability.tags.iter().any(|tag| tag == "shared"));
}

#[test]
fn capability_can_be_classified_as_atomic_deterministic_unit() {
    let capability = CapabilityProfile::new("capability.atomic.reply", "Reply Formatter")
        .with_tier(CapabilityTier::AtomicUnit)
        .with_model_dependence(ModelDependence::None)
        .with_dependency_ref("capability.foundation.reply")
        .with_optimization_of_ref("capability.foundation.reply")
        .with_constraint("Must not call external models.");

    assert!(capability.is_atomic_unit());
    assert!(capability.is_fully_deterministic());
    assert!(capability.is_optimization_of_runtime_foundation());
    assert_eq!(
        capability.dependency_refs,
        vec!["capability.foundation.reply".to_owned()]
    );
}
