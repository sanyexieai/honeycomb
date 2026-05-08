use std::env;

use anyhow::Result;
use hc_capability::{CapabilityNamespace, CapabilityProfile, seed_capability_for_role};
use hc_context::load_agent_responder_system_prompt;
use hc_core::{RuntimeCommand, RuntimeCommandResult, RuntimeSupervisor};
use hc_persona::{PersonaNamespace, PersonaProfile, seed_persona_for_role};
use hc_responder::{LlmResponderConfig, ResponderBinding};
use hc_store::store::WorkspaceNamespace;
use serde::{Deserialize, Serialize};

use crate::{
    AgentRuntimeBinding, AgentRuntimeBudget, BindingNamespace, TaskNamespace, TaskRequest,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentSeed {
    pub id: String,
    pub proposed_name: String,
    pub role: String,
    pub goal: String,
    pub capability_hints: Vec<String>,
    pub token_budget_hint: Option<u32>,
}

impl AgentSeed {
    pub fn new(
        id: impl Into<String>,
        proposed_name: impl Into<String>,
        role: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            proposed_name: proposed_name.into(),
            role: role.into(),
            goal: goal.into(),
            capability_hints: Vec::new(),
            token_budget_hint: None,
        }
    }

    pub fn with_token_budget_hint(mut self, token_budget_hint: u32) -> Self {
        self.token_budget_hint = Some(token_budget_hint);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPlan {
    pub task_id: String,
    pub namespace: TaskNamespace,
    pub seeds: Vec<AgentSeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterializedAgent {
    pub seed: AgentSeed,
    pub persona: PersonaProfile,
    pub capabilities: Vec<CapabilityProfile>,
    pub binding: AgentRuntimeBinding,
    pub runtime_budget: AgentRuntimeBudget,
}

/// Caps for one [`materialize_plan`] / [`materialize_plan_with_limits`] batch (ADR-002 Phase 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MaterializePlanLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_agents_per_task: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_agents_per_round: Option<usize>,
}

impl MaterializePlanLimits {
    /// Reads **`HC_MAX_AGENTS_PER_TASK`** and **`HC_MAX_NEW_AGENTS_PER_ROUND`** (positive integers).
    /// Unset or invalid values mean **no limit** for that knob.
    pub fn from_env() -> Self {
        Self {
            max_agents_per_task: parse_env_positive_usize("HC_MAX_AGENTS_PER_TASK"),
            max_new_agents_per_round: parse_env_positive_usize("HC_MAX_NEW_AGENTS_PER_ROUND"),
        }
    }

    pub const fn uncapped() -> Self {
        Self {
            max_agents_per_task: None,
            max_new_agents_per_round: None,
        }
    }

    #[must_use]
    pub fn effective_seed_cap(&self, plan_len: usize) -> usize {
        let mut cap = plan_len;
        if let Some(m) = self.max_agents_per_task {
            cap = cap.min(m);
        }
        if let Some(m) = self.max_new_agents_per_round {
            cap = cap.min(m);
        }
        cap
    }
}

fn parse_env_positive_usize(key: &str) -> Option<usize> {
    let raw = env::var(key).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
}

/// Result of [`materialize_plan`] with optional cap observability (`notices`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializePlanOutcome {
    pub agents: Vec<MaterializedAgent>,
    pub planned_seeds: usize,
    pub limits: MaterializePlanLimits,
    pub notices: Vec<String>,
}

/// How many agent seeds [`bootstrap_task`] materializes before assignment / execution ramps up (ADR-002 Phase 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskBootstrapPreset {
    /// Default: planner only (`bootstrap_planning_task` equivalent).
    #[default]
    PlannerOnly,
    /// Historical demo/tests: planner + worker + reviewer in one bootstrap.
    ThreeRolesDemo,
}

/// Reads [`TaskBootstrapPreset`] from **`HC_TASK_BOOTSTRAP_PRESET`** (case-insensitive):
/// unset / empty / `planner_only` / `planner-only` → [`TaskBootstrapPreset::PlannerOnly`];
/// `three_roles` / `three-roles` / `demo` / `full` → [`TaskBootstrapPreset::ThreeRolesDemo`];
/// unknown values fall back to **planner_only**.
pub fn bootstrap_task_preset_from_env() -> TaskBootstrapPreset {
    match env::var("HC_TASK_BOOTSTRAP_PRESET") {
        Ok(raw) => task_bootstrap_preset_from_str(&raw),
        Err(_) => TaskBootstrapPreset::PlannerOnly,
    }
}

fn task_bootstrap_preset_from_str(raw: &str) -> TaskBootstrapPreset {
    let key = raw.trim().to_ascii_lowercase();
    match key.as_str() {
        "" | "planner_only" | "planner-only" | "planning_only" | "planning-only" => {
            TaskBootstrapPreset::PlannerOnly
        }
        "three_roles" | "three-roles" | "demo" | "full" => TaskBootstrapPreset::ThreeRolesDemo,
        _ => TaskBootstrapPreset::PlannerOnly,
    }
}

#[must_use]
pub fn bootstrap_task_with_preset(task: &TaskRequest, preset: TaskBootstrapPreset) -> AgentPlan {
    match preset {
        TaskBootstrapPreset::PlannerOnly => bootstrap_planning_task(task),
        TaskBootstrapPreset::ThreeRolesDemo => bootstrap_task_three_roles_demo(task),
    }
}

pub fn bootstrap_task(task: &TaskRequest) -> AgentPlan {
    bootstrap_task_with_preset(task, bootstrap_task_preset_from_env())
}

fn bootstrap_task_three_roles_demo(task: &TaskRequest) -> AgentPlan {
    let base = task.id.replace(' ', "-");
    let execution_pool = task
        .budget
        .token_budget
        .saturating_sub(task.budget.evolution_reserve_tokens);
    let worker_budget = execution_pool / 2;
    let reviewer_budget = worker_budget / 2;
    let planner_budget = execution_pool
        .saturating_sub(worker_budget)
        .saturating_sub(reviewer_budget);

    AgentPlan {
        task_id: task.id.clone(),
        namespace: task.namespace.clone(),
        seeds: vec![
            AgentSeed::new(
                format!("{base}.planner"),
                "planner",
                "planner",
                format!("Plan the work for task: {}", task.title),
            )
            .with_token_budget_hint(planner_budget),
            AgentSeed::new(
                format!("{base}.worker"),
                "worker",
                "worker",
                format!("Execute the main work for task: {}", task.goal),
            )
            .with_token_budget_hint(worker_budget),
            AgentSeed::new(
                format!("{base}.reviewer"),
                "reviewer",
                "reviewer",
                format!("Review outputs and identify gaps for task: {}", task.title),
            )
            .with_token_budget_hint(reviewer_budget),
        ],
    }
}

pub fn bootstrap_planning_task(task: &TaskRequest) -> AgentPlan {
    let base = task.id.replace(' ', "-");
    AgentPlan {
        task_id: task.id.clone(),
        namespace: task.namespace.clone(),
        seeds: vec![
            AgentSeed::new(
                format!("{base}.planner"),
                "planner",
                "planner",
                format!("Plan the work for task: {}", task.goal),
            )
            .with_token_budget_hint(
                task.budget
                    .token_budget
                    .saturating_sub(task.budget.evolution_reserve_tokens),
            ),
        ],
    }
}

pub fn materialize_plan_with_limits(
    runtime: &mut RuntimeSupervisor,
    session_id: &str,
    plan: &AgentPlan,
    limits: MaterializePlanLimits,
) -> Result<MaterializePlanOutcome> {
    let planned_seeds = plan.seeds.len();
    let cap = limits.effective_seed_cap(planned_seeds);
    let mut notices = Vec::<String>::new();

    if cap == 0 && planned_seeds > 0 {
        anyhow::bail!(
            "materialization limits allow 0 agents (planned {planned_seeds} seed(s)); check HC_MAX_AGENTS_PER_TASK / HC_MAX_NEW_AGENTS_PER_ROUND"
        );
    }

    if cap < planned_seeds {
        let msg = format!(
            "materialization capped: planned {planned_seeds} agent seed(s), materializing first {cap} only (max_agents_per_task={:?}, max_new_agents_per_round={:?}); extra seeds skipped to prevent silent agent inflation",
            limits.max_agents_per_task,
            limits.max_new_agents_per_round
        );
        tracing::warn!(
            task_id = %plan.task_id,
            notice = %msg,
            "agent materialization limit"
        );
        notices.push(msg);
    }

    let mut agents = Vec::new();
    for seed in plan.seeds.iter().take(cap) {
        agents.push(materialize_seed(
            runtime,
            session_id,
            &plan.task_id,
            &plan.namespace,
            seed,
        )?);
    }

    if agents.is_empty() {
        anyhow::bail!("no agent seeds were materialized");
    }

    Ok(MaterializePlanOutcome {
        agents,
        planned_seeds,
        limits,
        notices,
    })
}

pub fn materialize_plan(
    runtime: &mut RuntimeSupervisor,
    session_id: &str,
    plan: &AgentPlan,
) -> Result<MaterializePlanOutcome> {
    materialize_plan_with_limits(runtime, session_id, plan, MaterializePlanLimits::from_env())
}

pub fn materialize_seed(
    runtime: &mut RuntimeSupervisor,
    session_id: &str,
    task_id: &str,
    namespace: &TaskNamespace,
    seed: &AgentSeed,
) -> Result<MaterializedAgent> {
    let result = runtime.dispatch(RuntimeCommand::CreateInstance {
        session_id: session_id.to_owned(),
        name: seed.proposed_name.clone(),
        parent_instance_id: None,
    })?;
    let RuntimeCommandResult::Instance(instance) = result else {
        anyhow::bail!("unexpected runtime result while creating instance");
    };

    let persona = seed_persona_for_role(
        PersonaNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()),
        task_id,
        &seed.proposed_name,
        &seed.role,
        &seed.goal,
    );
    let capability = seed_capability_for_role(
        CapabilityNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()),
        &seed.role,
    );
    let mut binding =
        AgentRuntimeBinding::new(instance.id.clone()).with_namespace(BindingNamespace::new(
            persona.namespace.tenant_id.clone(),
            persona.namespace.user_id.clone(),
        ));
    let mut capability_refs = vec![capability.id.clone()];
    capability_refs.extend(seed.capability_hints.clone());
    capability_refs.sort();
    capability_refs.dedup();
    binding.capability_refs = capability_refs.clone();
    binding.persona_ref = Some(persona.id.clone());
    binding.memory_scope_refs = vec![format!("memory_scope.task.{task_id}")];
    binding.responder_binding_ref = Some("responder.default".to_owned());
    let responder_system_prompt = render_agent_responder_system_prompt(
        &persona.namespace,
        &persona.name,
        &persona.role,
        &persona.style,
    )?;
    binding.responder = Some(ResponderBinding::Llm(LlmResponderConfig {
        provider: "openai".to_owned(),
        model: "gpt-4.1-mini".to_owned(),
        system_prompt: Some(responder_system_prompt),
    }));

    let allocated_tokens = seed.token_budget_hint.unwrap_or(0);
    let reserved_for_evolution_tokens = allocated_tokens / 5;
    let reserved_for_execution_tokens =
        allocated_tokens.saturating_sub(reserved_for_evolution_tokens);
    let runtime_budget = AgentRuntimeBudget {
        agent_instance_id: instance.id.clone(),
        agent_name: persona.name.clone(),
        allocated_tokens,
        reserved_for_execution_tokens,
        reserved_for_evolution_tokens,
        consumed_tokens: 0,
        consumed_time_minutes: 0,
    };

    Ok(MaterializedAgent {
        seed: seed.clone(),
        persona: PersonaProfile {
            capability_refs,
            ..persona
        },
        capabilities: vec![capability],
        binding,
        runtime_budget,
    })
}

fn render_agent_responder_system_prompt(
    namespace: &PersonaNamespace,
    agent_name: &str,
    role_name: &str,
    style: &str,
) -> Result<String> {
    let workspace_namespace =
        WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
    Ok(load_agent_responder_system_prompt(&workspace_namespace)?
        .replace("{{agent_name}}", agent_name)
        .replace("{{role_name}}", role_name)
        .replace("{{style}}", style))
}

#[cfg(test)]
mod bootstrap_preset_parse_tests {
    use super::{TaskBootstrapPreset, task_bootstrap_preset_from_str};

    #[test]
    fn planner_only_aliases() {
        for raw in [
            "",
            "   ",
            "PLANNER_ONLY",
            "planner-only",
            "Planning-Only",
        ] {
            assert_eq!(
                task_bootstrap_preset_from_str(raw),
                TaskBootstrapPreset::PlannerOnly
            );
        }
    }

    #[test]
    fn three_roles_aliases() {
        for raw in ["three_roles", "THREE-ROLES", "demo", "full"] {
            assert_eq!(
                task_bootstrap_preset_from_str(raw),
                TaskBootstrapPreset::ThreeRolesDemo
            );
        }
    }

    #[test]
    fn unknown_alias_falls_back_to_planner_only() {
        assert_eq!(
            task_bootstrap_preset_from_str("not-a-known-preset"),
            TaskBootstrapPreset::PlannerOnly
        );
    }
}

#[cfg(test)]
#[path = "../tests/unit/bootstrap.rs"]
mod tests;
