use serde::{Deserialize, Serialize};

use crate::TaskRequest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskPlanStatus {
    AwaitingPlannerInput,
    Drafted,
    Approved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub stage: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProposal {
    pub id: String,
    pub role: String,
    pub reason: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskPlan {
    pub task_id: String,
    pub status: TaskPlanStatus,
    pub planning_agent_role: String,
    pub planning_notes: Vec<String>,
    pub work_items: Vec<WorkItem>,
    pub agent_proposals: Vec<AgentProposal>,
}

impl TaskPlan {
    pub fn awaiting_planner_input(task: &TaskRequest) -> Self {
        Self {
            task_id: task.id.clone(),
            status: TaskPlanStatus::AwaitingPlannerInput,
            planning_agent_role: "planner".to_owned(),
            planning_notes: vec![
                "Planner agent created for this task.".to_owned(),
                "Awaiting explicit task decomposition into stages, work items, and agent proposals."
                    .to_owned(),
            ],
            work_items: Vec::new(),
            agent_proposals: Vec::new(),
        }
    }

    pub fn add_note(&mut self, note: impl Into<String>) {
        self.status = TaskPlanStatus::Drafted;
        self.planning_notes.push(note.into());
    }

    pub fn add_work_item(
        &mut self,
        stage: impl Into<String>,
        title: impl Into<String>,
        goal: impl Into<String>,
    ) -> String {
        self.status = TaskPlanStatus::Drafted;
        let id = format!("work-item.{:04}", self.work_items.len() + 1);
        self.work_items.push(WorkItem {
            id: id.clone(),
            title: title.into(),
            goal: goal.into(),
            stage: stage.into(),
            status: "planned".to_owned(),
        });
        id
    }

    pub fn add_agent_proposal(
        &mut self,
        role: impl Into<String>,
        reason: impl Into<String>,
    ) -> String {
        self.status = TaskPlanStatus::Drafted;
        let id = format!("agent-proposal.{:04}", self.agent_proposals.len() + 1);
        self.agent_proposals.push(AgentProposal {
            id: id.clone(),
            role: role.into(),
            reason: reason.into(),
            status: "proposed".to_owned(),
        });
        id
    }

    pub fn approve(&mut self) {
        self.status = TaskPlanStatus::Approved;
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(plan.planning_notes.len(), 2);
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
        assert_eq!(plan.planning_notes.len(), 3);
    }
}
