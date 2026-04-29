use super::*;
use crate::{TaskNamespace, TaskRequest};

#[test]
fn awaiting_plan_starts_empty_and_explicit() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = TaskPlan::awaiting_planner_input(&task);

    assert_eq!(plan.task_id, "task.plan");
    assert_eq!(plan.status, TaskPlanStatus::AwaitingPlannerInput);
    assert_eq!(plan.planning_agent_role, "planner");
    assert!(plan.work_items.is_empty());
    assert!(plan.agent_proposals.is_empty());
    assert!(plan.work_item_claims.is_empty());
    assert!(plan.work_item_assignments.is_empty());
    assert!(plan.agent_runtime_budgets.is_empty());
    assert!(plan.evolution_issues.is_empty());
    assert_eq!(plan.planning_notes.len(), 3);
}

#[test]
fn plan_can_be_drafted_incrementally() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);

    let work_item_id = plan.add_work_item("phase-1", "Inspect repo", "Understand current layout");
    let proposal_id = plan.add_agent_proposal("reviewer", "Need a reviewer for risk checks");
    plan.add_note("Start with repository structure and runtime boundaries.");
    plan.approve();

    assert_eq!(work_item_id, "work-item.0001");
    assert_eq!(proposal_id, "agent-proposal.0001");
    assert_eq!(plan.status, TaskPlanStatus::Approved);
    assert_eq!(plan.work_items.len(), 1);
    assert_eq!(plan.agent_proposals.len(), 1);
    assert_eq!(plan.planning_notes.len(), 4);
}

#[test]
fn work_item_claims_can_be_resolved_explicitly() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item("phase-1", "Inspect repo", "Understand current layout");

    plan.add_work_item_claim(
        &work_item_id,
        "instance.worker",
        "worker",
        0.72,
        "general fit",
    );
    plan.add_work_item_claim(
        &work_item_id,
        "instance.reviewer",
        "reviewer",
        0.91,
        "best fit for risk analysis",
    );
    let assignment_id = plan
        .resolve_work_item_assignment(&work_item_id)
        .expect("assignment should resolve");

    assert_eq!(assignment_id, "work-assignment.0001");
    assert_eq!(plan.work_item_assignments.len(), 1);
    assert_eq!(plan.work_item_assignments[0].agent_name, "reviewer");
    assert_eq!(plan.work_items[0].status, "assigned");
}

#[test]
fn assigned_work_item_can_enter_execution() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item("phase-1", "Inspect repo", "Understand current layout");
    plan.add_work_item_claim(&work_item_id, "instance.worker", "worker", 0.8, "fit");
    plan.resolve_work_item_assignment(&work_item_id)
        .expect("assignment should resolve");

    let agent_id = plan
        .start_work_item_execution(&work_item_id)
        .expect("work item should start");

    assert_eq!(agent_id, "instance.worker");
    assert_eq!(plan.work_items[0].status, "in_progress");
    assert_eq!(plan.work_item_assignments[0].status, "executing");
}

#[test]
fn idle_budget_can_be_turned_into_evolution_issue() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);

    plan.register_agent_runtime_budget(AgentRuntimeBudget {
        agent_instance_id: "instance.worker".to_owned(),
        agent_name: "worker".to_owned(),
        allocated_tokens: 3000,
        reserved_for_execution_tokens: 2400,
        reserved_for_evolution_tokens: 600,
        consumed_tokens: 1200,
        consumed_time_minutes: 10,
    });

    let issue_id = plan.queue_evolution_issue(
        "instance.worker",
        "Refine worker execution path",
        "Use idle tokens to split execution into more deterministic units.",
        300,
        None,
    );

    assert_eq!(issue_id, "evolution-issue.0001");
    assert_eq!(plan.agent_runtime_budgets[0].idle_tokens(), 1800);
    assert_eq!(plan.evolution_issues.len(), 1);
}
