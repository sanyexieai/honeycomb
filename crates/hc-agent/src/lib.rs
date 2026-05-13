//! Task-to-agent orchestration layer for Honeycomb.

pub mod binding;
pub mod bootstrap;
pub mod catalog;
pub mod agent_chat_turn_sink;
pub mod conversation;
pub mod domain;
pub mod experience_lane;
mod http_l2l3_planner_steering;
pub mod incubation;
pub mod orchestrator;
pub mod persistence;
pub mod planning;
pub mod profile;
pub mod runtime_layout;
pub mod routing;
pub mod session_bundle;
pub mod session_hc_agent_bin;
pub mod swarm_routing;
mod swarm_routing_phrase_table;
pub mod task;
pub mod view;
pub mod workbench;

pub use agent_chat_turn_sink::AgentRuntimeChatTurnSink;
pub use binding::{AgentRuntimeBinding, BindingNamespace};
pub use bootstrap::{
    AgentPlan, AgentSeed, MaterializePlanLimits, MaterializePlanOutcome, MaterializedAgent,
    TaskBootstrapPreset, bootstrap_planning_task, bootstrap_task, bootstrap_task_preset_from_env,
    bootstrap_task_with_preset, materialize_plan, materialize_plan_with_limits, materialize_seed,
};
pub use catalog::{
    AgentCatalog, WORKSPACE_AGENT_DEFINITIONS_DIR, load_workspace_capability_profiles,
};
pub use conversation::{
    ChannelConversation, ConversationParticipant, ConversationParticipantKind,
    ConversationParticipantMode, ConversationParticipantState, ConversationStatus,
    ConversationStopPolicy, ConversationTurnPolicy, ConversationTurnState,
};
pub use domain::{DomainKind, DomainProfile, DomainProfileSummary, DomainRepository};
pub use experience_lane::{task_learning_effective, tenant_learning_allowed_from_env};
pub use http_l2l3_planner_steering::{
    http_l2l3_planner_steering_enabled_from_env, maybe_apply_http_l2l3_planner_steering,
};
pub use incubation::{IncubationObservation, IncubationReport, PromotionDecision};
pub use orchestrator::{AgentOrchestrator, NominationCycleOutcome, SwarmMessageClassification};
pub use persistence::{
    HTTP_L23_EXECUTION_DIGEST_HEADING, HTTP_L23_PLAN_DIGEST_HEADING,
    HTTP_L23_REVIEW_DIGEST_HEADING, PersistedAgentAssets, PersistedIncubationArtifacts,
    PersistedTaskArtifacts, ROUTING_BINDING_LOG_SCHEMA_V1, RoutingBindingLogLineV1,
    TaskArtifactDocument, TaskArtifactKind, TaskArtifactQuery, TaskArtifactSummary,
    WORK_ITEM_ASSIGNMENT_JOURNAL_SCHEMA_V1, WORK_ITEM_CLAIM_JOURNAL_SCHEMA_V1,
    WorkItemAssignmentJournalLineV1, WorkItemClaimJournalLineV1,
    append_implicit_intent_dedupe_record, append_routing_binding_log_line,
    append_work_item_assignment_journal_line, append_work_item_claim_journal_line,
    build_routing_binding_log_line_v1, build_routing_binding_log_line_v1_headless,
    build_routing_binding_log_line_v1_headless_from_snapshot, ensure_http_implicit_task_plan_stub,
    format_execution_results_digest_for_http_l23, format_plan_notes_digest_for_http_l23,
    format_review_notes_digest_for_http_l23, hydrate_task_plan_work_item_coordination_journals,
    load_implicit_intent_dedupe_keys, load_task_coordination_bundle_for_journal_updates,
    load_task_execution_result_artifacts_v1, load_task_plan_note_artifacts_v1,
    load_task_review_note_artifacts_v1, persist_execution_result_artifact_v1,
    persist_http_chat_l23_degenerate_claim_assign, persist_incubation_report,
    persist_materialized_agents, persist_plan_note_artifact_v1, persist_review_note_artifact_v1,
    persist_task_artifacts, persist_task_artifacts_with_in_memory_prune, query_task_artifacts,
    read_task_artifact, rebuild_task_artifact_index,
};
pub use planning::{
    AgentProposal, AgentRuntimeBudget, EvolutionIssue, HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID, TaskPlan,
    TaskPlanStatus, WorkItem,
};
pub use profile::{
    AgentDefinitionLayer, AgentKind, AgentProfile, AgentProfileSummary, AgentRepository,
};
pub use runtime_layout::{
    USER_AGENT_RUNTIME_REL, agent_conversation_slug, ensure_user_agent_runtime_layout,
    user_agent_runtime_dir,
};
pub use session_bundle::{
    SESSIONS_SUBDIR, ensure_session_agent_bundle, load_session_agent_profile, session_agent_md_rel,
    session_runtime_rel,
};
pub use session_hc_agent_bin::{
    HcAgentSessionBinMode, SessionHcAgentBinOptions, maybe_install_session_hc_agent_bin,
    session_hc_agent_dest_file_name,
};
pub use routing::{
    best_phrase_match_score, phrase_match_score, phrase_match_score_with_stop_terms,
    route_match_terms, route_match_terms_with_stop_terms,
};
pub use swarm_routing_phrase_table::SwarmRoutingPhraseTable;
pub use task::{TaskBudget, TaskContext, TaskNamespace, TaskRequest};
pub use view::{AgentCardView, AssetSummaryView, WorkspaceViewModel, build_workspace_view};
pub use workbench::{AgentWorkbench, WorkspacePhase, bootstrap_task_workbench};
