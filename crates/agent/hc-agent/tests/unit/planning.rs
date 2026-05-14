use super::*;
use crate::{
    HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID, TaskNamespace, TaskPlan, TaskPlanStatus, TaskRequest,
    WorkItem,
};
use hc_protocol::swarm::WorkItemLifecycleState;

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
    assert_eq!(
        plan.work_items[0].lifecycle,
        WorkItemLifecycleState::Assigned
    );
}

#[test]
fn resolve_assignment_is_idempotent_for_active_assignment() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item("phase-1", "Inspect repo", "Understand current layout");

    plan.add_work_item_claim(
        &work_item_id,
        "instance.reviewer",
        "reviewer",
        0.91,
        "best fit",
    );
    let first = plan
        .resolve_work_item_assignment(&work_item_id)
        .expect("first resolve");
    assert_eq!(plan.work_item_assignments.len(), 1);
    assert!(plan.resolve_work_item_assignment(&work_item_id).is_none());
    assert_eq!(plan.work_item_assignments.len(), 1);
    assert_eq!(plan.work_item_assignments[0].id, first);
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
    assert_eq!(
        plan.work_items[0].lifecycle,
        WorkItemLifecycleState::Assigned
    );
    assert_eq!(plan.work_item_assignments[0].status, "executing");
}

#[test]
fn assignment_breaks_score_tie_by_lower_workload() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let w1 = plan.add_work_item("p1", "First", "do first");
    let w2 = plan.add_work_item("p2", "Second", "do second");

    plan.add_work_item_claim(&w1, "instance.alice", "alice", 0.85, "fit");
    plan.resolve_work_item_assignment(&w1)
        .expect("assign first work item");

    plan.add_work_item_claim(&w2, "instance.alice", "alice", 0.81, "tie");
    plan.add_work_item_claim(&w2, "instance.bob", "bob", 0.81, "tie");

    plan.resolve_work_item_assignment(&w2)
        .expect("assign second after tie-break");
    let w2_assignment = plan
        .work_item_assignments
        .iter()
        .find(|a| a.work_item_id == w2)
        .expect("w2 assignment");
    assert_eq!(w2_assignment.agent_name, "bob");
}

#[test]
fn resolve_assignment_requires_positive_capability_score_under_p0_eligibility() {
    let task = TaskRequest::new("task.plan", "Plan", "Break this task down")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item("phase-1", "Inspect repo", "Understand layout");
    plan.add_work_item_claim(
        &work_item_id,
        "instance.reviewer",
        "reviewer",
        0.0,
        "scores at floor are ineligible under ADR P0 semantics",
    );
    assert!(
        plan.resolve_work_item_assignment(&work_item_id).is_none(),
        "claims with capability_score ≤ floor must not assign"
    );
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

#[test]
fn prune_http_implicit_holder_keeps_seed_when_only_implicit_placeholder() {
    let task = TaskRequest::new("task.imp.keep", "T", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "HTTP implicit intent holder".to_owned(),
        goal: "hold".to_owned(),
        stage: "implicit".to_owned(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });

    plan.prune_http_implicit_work_item_placeholder();

    assert_eq!(plan.work_items.len(), 1);
    assert_eq!(plan.work_items[0].id, HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID);
}

#[test]
fn prune_http_implicit_holder_drops_placeholder_when_other_work_items_exist() {
    let task = TaskRequest::new("task.imp.prune", "T", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "HTTP implicit intent holder".to_owned(),
        goal: "hold".to_owned(),
        stage: "implicit".to_owned(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });
    let real_id = plan.add_work_item(
        "phase-1",
        "Planner-owned item",
        "supersedes implicit HTTP seed row",
    );

    plan.prune_http_implicit_work_item_placeholder();

    assert_eq!(plan.work_items.len(), 1);
    assert_eq!(plan.work_items[0].id, real_id);
}

#[test]
fn add_work_item_numbering_skips_implicit_http_placeholder_id() {
    let task = TaskRequest::new("task.imp.wi-slot", "T", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "holder".into(),
        goal: "h".into(),
        stage: "implicit".into(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });

    let first = plan.add_work_item("p", "Planner item one", "a");
    assert_eq!(first.as_str(), "work-item.0001");

    let second = plan.add_work_item("p", "Planner item two", "b");
    assert_eq!(second.as_str(), "work-item.0002");
}

#[test]
fn mark_assigned_work_item_done_closes_executing_assignment_row() {
    let task = TaskRequest::new("task.mdone", "T", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let wi = plan.add_work_item("p", "w", "g");
    plan.add_work_item_claim(&wi, "a1", "ag", 1.0, "r");
    plan.resolve_work_item_assignment(&wi).expect("assign");
    plan.start_work_item_execution(&wi)
        .expect("start executing");
    assert!(
        plan.mark_assigned_work_item_done(&wi),
        "MarkDone should apply from assigned lifecycle with active assignment closed to completed"
    );

    let wi_ref = plan.work_items.iter().find(|w| w.id == wi).expect("wi");
    assert_eq!(wi_ref.lifecycle, WorkItemLifecycleState::Done);
    let a = plan
        .work_item_assignments
        .iter()
        .find(|row| row.work_item_id == wi)
        .expect("assignment");
    assert_eq!(a.status, "completed");
}
