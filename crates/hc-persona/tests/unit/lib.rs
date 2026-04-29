use super::*;

#[test]
fn seed_persona_uses_seed_lifecycle_and_role_defaults() {
    let persona = seed_persona_for_role(
        PersonaNamespace::local_default(),
        "task.demo",
        "planner",
        "planner",
        "Plan the work for this task.",
    );

    assert_eq!(persona.kind, PersonaKind::Agent);
    assert_eq!(persona.lifecycle, PersonaLifecycle::Seed);
    assert_eq!(persona.namespace, PersonaNamespace::local_default());
    assert_eq!(persona.visibility, PersonaVisibility::Private);
    assert_eq!(persona.role, "planner");
    assert!(persona.collaboration_rules.auto_claim);
    assert_eq!(
        persona.collaboration_rules.default_reply_mode.as_deref(),
        Some("nominate_first")
    );
}
