use serde::{Deserialize, Serialize};

use crate::TaskRequest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeBudget {
    pub agent_instance_id: String,
    pub agent_name: String,
    pub allocated_tokens: u32,
    pub reserved_for_execution_tokens: u32,
    pub reserved_for_evolution_tokens: u32,
    pub consumed_tokens: u32,
    pub consumed_time_minutes: u32,
}

impl AgentRuntimeBudget {
    pub fn idle_tokens(&self) -> u32 {
        self.allocated_tokens.saturating_sub(self.consumed_tokens)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvolutionIssue {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub source_agent_instance_id: String,
    pub status: String,
    pub estimated_token_cost: u32,
    pub related_work_item_id: Option<String>,
}

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
    pub estimated_token_cost: u32,
    pub estimated_time_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProposal {
    pub id: String,
    pub role: String,
    pub reason: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkItemClaim {
    pub id: String,
    pub work_item_id: String,
    pub agent_instance_id: String,
    pub agent_name: String,
    pub score: f32,
    pub reason: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemAssignment {
    pub id: String,
    pub work_item_id: String,
    pub agent_instance_id: String,
    pub agent_name: String,
    pub rationale: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskPlan {
    pub task_id: String,
    pub status: TaskPlanStatus,
    pub planning_agent_role: String,
    pub task_token_budget: u32,
    pub task_time_budget_minutes: u32,
    pub evolution_reserve_tokens: u32,
    pub planning_notes: Vec<String>,
    pub work_items: Vec<WorkItem>,
    pub agent_proposals: Vec<AgentProposal>,
    pub work_item_claims: Vec<WorkItemClaim>,
    pub work_item_assignments: Vec<WorkItemAssignment>,
    pub agent_runtime_budgets: Vec<AgentRuntimeBudget>,
    pub evolution_issues: Vec<EvolutionIssue>,
}

impl TaskPlan {
    pub fn awaiting_planner_input(task: &TaskRequest) -> Self {
        Self {
            task_id: task.id.clone(),
            status: TaskPlanStatus::AwaitingPlannerInput,
            planning_agent_role: "planner".to_owned(),
            task_token_budget: task.budget.token_budget,
            task_time_budget_minutes: task.budget.time_budget_minutes,
            evolution_reserve_tokens: task.budget.evolution_reserve_tokens,
            planning_notes: vec![
                "Planner agent created for this task.".to_owned(),
                "Awaiting explicit task decomposition into stages, work items, and agent proposals."
                    .to_owned(),
                format!(
                    "Budget: {} tokens, {} minutes, {} evolution reserve tokens.",
                    task.budget.token_budget,
                    task.budget.time_budget_minutes,
                    task.budget.evolution_reserve_tokens
                ),
            ],
            work_items: Vec::new(),
            agent_proposals: Vec::new(),
            work_item_claims: Vec::new(),
            work_item_assignments: Vec::new(),
            agent_runtime_budgets: Vec::new(),
            evolution_issues: Vec::new(),
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
        self.add_work_item_with_budget(stage, title, goal, 0, 0)
    }

    pub fn add_work_item_with_budget(
        &mut self,
        stage: impl Into<String>,
        title: impl Into<String>,
        goal: impl Into<String>,
        estimated_token_cost: u32,
        estimated_time_minutes: u32,
    ) -> String {
        self.status = TaskPlanStatus::Drafted;
        let id = format!("work-item.{:04}", self.work_items.len() + 1);
        self.work_items.push(WorkItem {
            id: id.clone(),
            title: title.into(),
            goal: goal.into(),
            stage: stage.into(),
            status: "planned".to_owned(),
            estimated_token_cost,
            estimated_time_minutes,
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

    pub fn register_agent_runtime_budget(&mut self, budget: AgentRuntimeBudget) {
        self.agent_runtime_budgets
            .retain(|existing| existing.agent_instance_id != budget.agent_instance_id);
        self.agent_runtime_budgets.push(budget);
    }

    pub fn queue_evolution_issue(
        &mut self,
        source_agent_instance_id: impl Into<String>,
        title: impl Into<String>,
        summary: impl Into<String>,
        estimated_token_cost: u32,
        related_work_item_id: Option<String>,
    ) -> String {
        let id = format!("evolution-issue.{:04}", self.evolution_issues.len() + 1);
        self.evolution_issues.push(EvolutionIssue {
            id: id.clone(),
            title: title.into(),
            summary: summary.into(),
            source_agent_instance_id: source_agent_instance_id.into(),
            status: "open".to_owned(),
            estimated_token_cost,
            related_work_item_id,
        });
        id
    }

    pub fn planned_token_cost(&self) -> u32 {
        self.work_items
            .iter()
            .map(|item| item.estimated_token_cost)
            .sum()
    }

    pub fn planned_time_minutes(&self) -> u32 {
        self.work_items
            .iter()
            .map(|item| item.estimated_time_minutes)
            .sum()
    }

    pub fn add_work_item_claim(
        &mut self,
        work_item_id: impl Into<String>,
        agent_instance_id: impl Into<String>,
        agent_name: impl Into<String>,
        score: f32,
        reason: impl Into<String>,
    ) -> String {
        let work_item_id = work_item_id.into();
        let id = format!("work-claim.{:04}", self.work_item_claims.len() + 1);
        self.work_item_claims.push(WorkItemClaim {
            id: id.clone(),
            work_item_id,
            agent_instance_id: agent_instance_id.into(),
            agent_name: agent_name.into(),
            score,
            reason: reason.into(),
            status: "submitted".to_owned(),
        });
        id
    }

    pub fn resolve_work_item_assignment(&mut self, work_item_id: &str) -> Option<String> {
        let best_claim_index = self
            .work_item_claims
            .iter()
            .enumerate()
            .filter(|(_, claim)| claim.work_item_id == work_item_id && claim.status == "submitted")
            .max_by(|(_, left), (_, right)| left.score.total_cmp(&right.score))
            .map(|(index, _)| index)?;

        let best_claim = self.work_item_claims[best_claim_index].clone();
        self.work_item_claims[best_claim_index].status = "won".to_owned();
        for claim in &mut self.work_item_claims {
            if claim.work_item_id == work_item_id
                && claim.id != best_claim.id
                && claim.status == "submitted"
            {
                claim.status = "lost".to_owned();
            }
        }
        if let Some(work_item) = self
            .work_items
            .iter_mut()
            .find(|item| item.id == work_item_id)
        {
            work_item.status = "assigned".to_owned();
        }
        let assignment_id = format!(
            "work-assignment.{:04}",
            self.work_item_assignments.len() + 1
        );
        self.work_item_assignments.push(WorkItemAssignment {
            id: assignment_id.clone(),
            work_item_id: work_item_id.to_owned(),
            agent_instance_id: best_claim.agent_instance_id,
            agent_name: best_claim.agent_name,
            rationale: best_claim.reason,
            status: "assigned".to_owned(),
        });
        Some(assignment_id)
    }

    pub fn start_work_item_execution(&mut self, work_item_id: &str) -> Option<String> {
        let work_item = self
            .work_items
            .iter_mut()
            .find(|item| item.id == work_item_id && item.status == "assigned")?;
        work_item.status = "in_progress".to_owned();

        let assignment = self.work_item_assignments.iter_mut().find(|assignment| {
            assignment.work_item_id == work_item_id && assignment.status == "assigned"
        })?;
        assignment.status = "executing".to_owned();
        Some(assignment.agent_instance_id.clone())
    }
}

#[cfg(test)]
#[path = "../tests/unit/planning.rs"]
mod tests;
