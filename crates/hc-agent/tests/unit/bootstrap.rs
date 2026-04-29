use super::*;
use crate::TaskRequest;
use anyhow::Context;

#[test]
fn bootstrap_creates_default_seed_roles() {
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_task(&task);

    assert_eq!(plan.task_id, "task.demo");
    assert_eq!(plan.namespace.tenant_id, "tenant-a");
    assert_eq!(plan.namespace.user_id, "user-a");
    assert_eq!(plan.seeds.len(), 3);
    assert_eq!(plan.seeds[0].role, "planner");
    assert_eq!(plan.seeds[1].role, "worker");
    assert_eq!(plan.seeds[2].role, "reviewer");
    assert!(
        plan.seeds
            .iter()
            .all(|seed| seed.token_budget_hint.is_some())
    );
}

#[test]
fn materialize_plan_creates_runtime_instances() {
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_task(&task);
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime
        .create_session_in_namespace("demo", hc_core::RuntimeNamespace::new("tenant-a", "user-a"));

    let agents = materialize_plan(&mut runtime, &session.id, &plan)
        .context("plan should materialize")
        .expect("materialization should succeed");

    assert_eq!(agents.len(), 3);
    assert_eq!(runtime.state().instances.len(), 3);
    assert_eq!(
        agents[0].binding.persona_ref.as_deref(),
        Some("persona.seed.task.demo.planner")
    );
    assert_eq!(agents[0].capabilities.len(), 1);
    assert_eq!(agents[0].capabilities[0].id, "capability.seed.planner");
    assert_eq!(
        agents[0].persona.capability_refs,
        vec!["capability.seed.planner".to_owned()]
    );
    assert_eq!(
        agents[0].binding.capability_refs,
        vec!["capability.seed.planner".to_owned()]
    );
    assert_eq!(agents[0].binding.namespace.tenant_id, "tenant-a");
    assert_eq!(agents[0].binding.namespace.user_id, "user-a");
    assert_eq!(agents[0].persona.namespace.tenant_id, "tenant-a");
    assert_eq!(agents[0].persona.namespace.user_id, "user-a");
    assert_eq!(agents[0].capabilities[0].namespace.tenant_id, "tenant-a");
    assert_eq!(agents[0].capabilities[0].namespace.user_id, "user-a");
    assert_eq!(agents[0].persona.role, "planner");
    assert!(agents[0].runtime_budget.allocated_tokens > 0);
}

#[test]
fn bootstrap_planning_task_creates_only_planner_seed() {
    let task = TaskRequest::new("task.plan", "Planning Task", "Plan this task")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_planning_task(&task);

    assert_eq!(plan.seeds.len(), 1);
    assert_eq!(plan.seeds[0].role, "planner");
    assert_eq!(plan.seeds[0].proposed_name, "planner");
    assert!(plan.seeds[0].token_budget_hint.is_some());
}
