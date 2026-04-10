//! Task-to-agent orchestration layer for Honeycomb.

pub mod binding;
pub mod bootstrap;
pub mod incubation;
pub mod orchestrator;
pub mod planning;
pub mod persistence;
pub mod task;
pub mod view;
pub mod workbench;

pub use binding::{AgentRuntimeBinding, BindingNamespace};
pub use hc_capability::{
    CapabilityInputType, CapabilityNamespace, CapabilityOutputType, CapabilityProfile,
    CapabilityRepository, CapabilityVisibility, seed_capability_for_role,
};
pub use hc_memory::{MemoryRecord, MemoryScope, MemoryType};
pub use hc_persona::{
    CollaborationRules, PersonaKind, PersonaLifecycle, PersonaProfile, PersonaRepository,
    seed_persona_for_role,
};
pub use bootstrap::{
    AgentPlan, AgentSeed, MaterializedAgent, bootstrap_planning_task, bootstrap_task,
    materialize_plan, materialize_seed,
};
pub use incubation::{IncubationObservation, IncubationReport, PromotionDecision};
pub use orchestrator::AgentOrchestrator;
pub use planning::{AgentProposal, TaskPlan, TaskPlanStatus, WorkItem};
pub use persistence::{
    PersistedAgentAssets, PersistedIncubationArtifacts, persist_incubation_report,
    persist_materialized_agents,
};
pub use hc_responder::{
    HumanResponderConfig, LlmResponderConfig, ReplyRequest, ReplyResponse, ResponderBackend,
    ResponderBinding, ResponderKind, RuleResponderConfig, ScriptResponderConfig,
};
pub use hc_trace::{
    ActivityItemView, DecisionTraceView, agent_code_from, behavior_mode_code_from, code_from,
    summarize_trace_body,
};
pub use task::{TaskContext, TaskNamespace, TaskRequest};
pub use view::{AgentCardView, AssetSummaryView, WorkspaceViewModel, build_workspace_view};
pub use workbench::{AgentWorkbench, WorkspacePhase, bootstrap_task_workbench};
