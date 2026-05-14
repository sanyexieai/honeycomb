use super::*;
use crate::{TaskBootstrapPreset, TaskRequest};
use anyhow::Context;

#[test]
fn planner_only_preset_materializes_single_planner_seed() {
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::PlannerOnly);

    assert_eq!(plan.task_id, "task.demo");
    assert_eq!(plan.namespace.tenant_id, "tenant-a");
    assert_eq!(plan.namespace.user_id, "user-a");
    assert_eq!(plan.seeds.len(), 1);
    assert_eq!(plan.seeds[0].role, "planner");
    assert!(
        plan.seeds
            .iter()
            .all(|seed| seed.token_budget_hint.is_some())
    );
}

#[test]
fn three_roles_demo_preset_materializes_planner_worker_reviewer() {
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);

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
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime
        .create_session_in_namespace("demo", hc_core::RuntimeNamespace::new("tenant-a", "user-a"));

    let outcome = materialize_plan(&mut runtime, &session.id, &plan)
        .context("plan should materialize")
        .expect("materialization should succeed");
    let agents = outcome.agents;

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
fn materialize_plan_with_limits_truncates_extra_seeds_with_notice() {
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime
        .create_session_in_namespace("demo", hc_core::RuntimeNamespace::new("tenant-a", "user-a"));

    let limits = MaterializePlanLimits {
        max_agents_per_task: Some(2),
        max_new_agents_per_round: None,
    };
    let outcome = materialize_plan_with_limits(&mut runtime, &session.id, &plan, limits)
        .expect("materialization should succeed");

    assert_eq!(outcome.agents.len(), 2);
    assert_eq!(outcome.planned_seeds, 3);
    assert_eq!(runtime.state().instances.len(), 2);
    assert_eq!(outcome.agents[0].seed.role, "planner");
    assert_eq!(outcome.agents[1].seed.role, "worker");
    assert_eq!(outcome.notices.len(), 1);
    assert!(
        outcome.notices[0].contains("materialization capped"),
        "{:?}",
        outcome.notices
    );
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
