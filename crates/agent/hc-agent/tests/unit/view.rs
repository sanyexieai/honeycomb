use super::*;
use crate::{TaskNamespace, TaskRequest, bootstrap_task_workbench};

#[test]
fn workspace_view_contains_agent_cards_and_activity() {
    let mut runtime = RuntimeSupervisor::new();
    let task = TaskRequest::new("task.view", "View Task", "Summarize workspace state")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let workbench =
        bootstrap_task_workbench(&mut runtime, task).expect("workbench should bootstrap");

    let view = build_workspace_view(&runtime, &workbench);

    assert_eq!(view.task_id, "task.view");
    assert_eq!(view.namespace_label, "tenant-a/user-a");
    assert_eq!(view.phase, "planning");
    assert_eq!(view.plan_status, "awaiting_planner_input");
    assert_eq!(view.work_item_lines, vec!["- none".to_owned()]);
    assert_eq!(view.assignment_lines, vec!["- none".to_owned()]);
    assert_eq!(view.work_item_count, 0);
    assert_eq!(view.proposed_agent_count, 0);
    assert_eq!(view.work_item_claim_count, 0);
    assert_eq!(view.work_item_assignment_count, 0);
    assert_eq!(view.agent_cards.len(), 1);
    assert_eq!(view.asset_summary.personas, 1);
    assert_eq!(view.asset_summary.capabilities, 1);
    assert!(view.recent_activity.len() >= 2);
    assert_eq!(view.recent_activity[0].title, "Task Accepted");
    assert_eq!(view.decision_traces[0].stage, "bootstrap");
    assert!(view.agent_cards[0].agent_code.starts_with("AGT-"));
}
