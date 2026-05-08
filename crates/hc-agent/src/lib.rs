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
pub mod swarm_routing;
mod swarm_routing_phrase_table;
pub mod task;
pub mod view;
pub mod workbench;

pub use binding::{AgentRuntimeBinding, BindingNamespace};
pub use bootstrap::{
    AgentPlan, AgentSeed, MaterializePlanLimits, MaterializePlanOutcome, MaterializedAgent,
    bootstrap_planning_task, bootstrap_task, bootstrap_task_preset_from_env,
    bootstrap_task_with_preset, materialize_plan, materialize_plan_with_limits, materialize_seed,
    TaskBootstrapPreset,
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
pub use orchestrator::{AgentOrchestrator, NominationCycleOutcome, SwarmMessageClassification};
pub use persistence::{
    PersistedAgentAssets, PersistedIncubationArtifacts, PersistedTaskArtifacts,
    ROUTING_BINDING_LOG_SCHEMA_V1, RoutingBindingLogLineV1, TaskArtifactDocument,
    TaskArtifactKind, TaskArtifactQuery, TaskArtifactSummary,
    WORK_ITEM_ASSIGNMENT_JOURNAL_SCHEMA_V1,
    WORK_ITEM_CLAIM_JOURNAL_SCHEMA_V1, WorkItemAssignmentJournalLineV1,
    WorkItemClaimJournalLineV1, append_implicit_intent_dedupe_record,
    append_routing_binding_log_line, append_work_item_assignment_journal_line,
    append_work_item_claim_journal_line, build_routing_binding_log_line_v1,
    build_routing_binding_log_line_v1_headless,
    build_routing_binding_log_line_v1_headless_from_snapshot,
    ensure_http_implicit_task_plan_stub,
    hydrate_task_plan_work_item_coordination_journals, load_implicit_intent_dedupe_keys,
    persist_incubation_report, persist_materialized_agents, persist_task_artifacts,
    persist_task_artifacts_with_in_memory_prune, query_task_artifacts, read_task_artifact, rebuild_task_artifact_index,
};
pub use planning::{
    AgentProposal, AgentRuntimeBudget, EvolutionIssue, HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID, TaskPlan,
    TaskPlanStatus, WorkItem,
};
pub use profile::{AgentKind, AgentProfile, AgentProfileSummary, AgentRepository};
pub use routing::{
    best_phrase_match_score, phrase_match_score, phrase_match_score_with_stop_terms,
    route_match_terms, route_match_terms_with_stop_terms,
};
pub use swarm_routing_phrase_table::SwarmRoutingPhraseTable;
pub use task::{TaskBudget, TaskContext, TaskNamespace, TaskRequest};
pub use view::{AgentCardView, AssetSummaryView, WorkspaceViewModel, build_workspace_view};
pub use workbench::{AgentWorkbench, WorkspacePhase, bootstrap_task_workbench};
