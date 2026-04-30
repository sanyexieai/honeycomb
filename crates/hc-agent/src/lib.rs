//! Task-to-agent orchestration layer for Honeycomb.

pub mod binding;
pub mod bootstrap;
pub mod conversation;
pub mod domain;
pub mod incubation;
pub mod orchestrator;
pub mod persistence;
pub mod planning;
pub mod profile;
pub mod routing;
pub mod task;
pub mod view;
pub mod workbench;

pub use binding::{AgentRuntimeBinding, BindingNamespace};
pub use bootstrap::{
    AgentPlan, AgentSeed, MaterializedAgent, bootstrap_planning_task, bootstrap_task,
    materialize_plan, materialize_seed,
};
pub use conversation::{
    ChannelConversation, ConversationParticipant, ConversationParticipantKind,
    ConversationParticipantMode, ConversationParticipantState, ConversationStatus,
    ConversationStopPolicy, ConversationTurnPolicy, ConversationTurnState,
};
pub use domain::{DomainKind, DomainProfile, DomainProfileSummary, DomainRepository};
pub use hc_capability::{
    CapabilityInputType, CapabilityNamespace, CapabilityOutputType, CapabilityProfile,
    CapabilityRepository, CapabilityTier, CapabilityVisibility, ModelDependence,
    seed_capability_for_role,
};
pub use hc_memory::{MemoryRecord, MemoryScope, MemoryType};
pub use hc_persona::{
    CollaborationRules, PersonaKind, PersonaLifecycle, PersonaProfile, PersonaRepository,
    seed_persona_for_role,
};
pub use hc_responder::{
    HumanResponderConfig, LlmResponderConfig, ReplyRequest, ReplyResponse, ResponderBackend,
    ResponderBinding, ResponderKind, RuleResponderConfig, ScriptResponderConfig,
};
pub use hc_trace::{
    ActivityItemView, DecisionTraceView, agent_code_from, behavior_mode_code_from, code_from,
    summarize_trace_body,
};
pub use incubation::{IncubationObservation, IncubationReport, PromotionDecision};
pub use orchestrator::AgentOrchestrator;
pub use persistence::{
    PersistedAgentAssets, PersistedIncubationArtifacts, PersistedTaskArtifacts,
    TaskArtifactDocument, TaskArtifactKind, TaskArtifactQuery, TaskArtifactSummary,
    persist_incubation_report, persist_materialized_agents, persist_task_artifacts,
    query_task_artifacts, read_task_artifact, rebuild_task_artifact_index,
};
pub use planning::{
    AgentProposal, AgentRuntimeBudget, EvolutionIssue, TaskPlan, TaskPlanStatus, WorkItem,
};
pub use profile::{AgentKind, AgentProfile, AgentProfileSummary, AgentRepository};
pub use routing::{
    best_phrase_match_score, phrase_match_score, phrase_match_score_with_stop_terms,
    route_match_terms, route_match_terms_with_stop_terms,
};
pub use task::{TaskBudget, TaskContext, TaskNamespace, TaskRequest};
pub use view::{AgentCardView, AssetSummaryView, WorkspaceViewModel, build_workspace_view};
pub use workbench::{AgentWorkbench, WorkspacePhase, bootstrap_task_workbench};
