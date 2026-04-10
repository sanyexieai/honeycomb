use anyhow::Result;
use hc_core::{RuntimeNamespace, RuntimeSupervisor, SessionRecord};
use serde::{Deserialize, Serialize};

use crate::{
    AgentPlan, MaterializedAgent, TaskPlan, TaskRequest, bootstrap_planning_task, materialize_plan,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePhase {
    Planning,
    Assignment,
    Execution,
    Consolidation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkbench {
    pub task: TaskRequest,
    pub session: SessionRecord,
    pub phase: WorkspacePhase,
    pub task_plan: TaskPlan,
    pub plan: AgentPlan,
    pub agents: Vec<MaterializedAgent>,
}

pub fn bootstrap_task_workbench(
    runtime: &mut RuntimeSupervisor,
    task: TaskRequest,
) -> Result<AgentWorkbench> {
    let namespace = RuntimeNamespace::new(
        task.namespace.tenant_id.clone(),
        task.namespace.user_id.clone(),
    );
    let session =
        runtime.create_session_in_namespace(format!("task-{}", task.id.replace(' ', "-")), namespace);
    let plan = bootstrap_planning_task(&task);
    let task_plan = TaskPlan::awaiting_planner_input(&task);
    let agents = materialize_plan(runtime, &session.id, &plan)?;

    Ok(AgentWorkbench {
        task,
        session,
        phase: WorkspacePhase::Planning,
        task_plan,
        plan,
        agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskNamespace;

    #[test]
    fn bootstrap_task_workbench_creates_planning_workspace() {
        let mut runtime = RuntimeSupervisor::new();
        let task = TaskRequest::new(
            "task.ui.bootstrap",
            "UI Bootstrap",
            "Create a working multi-agent workspace",
        )
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

        let workbench =
            bootstrap_task_workbench(&mut runtime, task).expect("workbench should bootstrap");

        assert_eq!(workbench.session.namespace.tenant_id, "tenant-a");
        assert_eq!(workbench.session.namespace.user_id, "user-a");
        assert_eq!(workbench.phase, WorkspacePhase::Planning);
        assert_eq!(workbench.agents.len(), 1);
        assert_eq!(runtime.state().instances.len(), 1);
        assert_eq!(workbench.agents[0].persona.role, "planner");
        assert_eq!(
            workbench.task_plan.status,
            crate::TaskPlanStatus::AwaitingPlannerInput
        );
    }
}
