use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hc_bootstrap::wall_clock_ms;
use hc_capability::CapabilityRepository;
use hc_context::{RoomMemoryWriteRequest, persist_room_memory};
use hc_core::MessageRecord;
use hc_memory::{MemoryNamespace, MemoryRepository, MemoryVisibility};
use hc_persona::PersonaRepository;
use hc_store::{
    task_coordination::{
        assignment_decision_markdown_relative, coordination_segment_slug,
        implicit_intent_journal_relative, materialization_notices_journal_relative,
        routing_binding_journal_relative, task_plan_markdown_relative,
        work_item_assignments_journal_relative, work_item_claims_journal_relative,
    },
    store::{
        MarkdownIndexEntry, MarkdownQuery, StoredMarkdown, WorkspaceNamespace, WorkspaceStore,
    },
};
use serde::{Deserialize, Serialize};

use hc_protocol::swarm::{
    IMPLICIT_INTENT_RECORD_SCHEMA_V1, INTENT_HASH_VERSION_V1, ImplicitIntentDedupeKey,
    ImplicitIntentDedupeRecord, RoutingDecisionRecord, SwarmRoutingBindingSnapshot,
    TaskBindingDecisionRecord, WorkItemLifecycleState, intent_fingerprint_v1_hex,
};

use crate::{
    TaskPlan, TaskRequest,
    bootstrap::MaterializedAgent,
    incubation::{IncubationReport, build_memory_record_from_report},
    planning::{
        HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID, TaskPlanStatus, WorkItem, WorkItemAssignment,
        WorkItemClaim,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedAgentAssets {
    pub persona_path: PathBuf,
    pub capability_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedIncubationArtifacts {
    pub memory_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedTaskArtifacts {
    pub task_plan_path: PathBuf,
    pub assignment_paths: Vec<PathBuf>,
    pub task_plan_memory_path: PathBuf,
    pub assignment_memory_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskArtifactKind {
    TaskPlan,
    AssignmentDecision,
}

impl TaskArtifactKind {
    fn as_doc_type(&self) -> &'static str {
        match self {
            Self::TaskPlan => "task_plan",
            Self::AssignmentDecision => "assignment_decision",
        }
    }

    fn from_doc_type(doc_type: &str) -> Option<Self> {
        match doc_type {
            "task_plan" => Some(Self::TaskPlan),
            "assignment_decision" => Some(Self::AssignmentDecision),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskArtifactSummary {
    pub id: String,
    pub kind: TaskArtifactKind,
    pub title: String,
    pub status: String,
    pub relative_path: String,
    pub tags: Vec<String>,
    pub task_hint: Option<String>,
    pub body_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskArtifactDocument {
    pub id: String,
    pub kind: TaskArtifactKind,
    pub title: String,
    pub status: String,
    pub relative_path: String,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskArtifactQuery {
    pub task_id: Option<String>,
    pub kind: Option<TaskArtifactKind>,
    pub status: Option<String>,
    pub limit: Option<usize>,
}

impl TaskArtifactQuery {
    pub fn for_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_kind(mut self, kind: TaskArtifactKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }
}

pub const ROUTING_BINDING_LOG_SCHEMA_V1: &str = "routing_binding_log_v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingBindingLogLineV1 {
    pub schema: String,
    pub created_at_ms: u64,
    pub message_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub task_scope_id: String,
    pub routing: RoutingDecisionRecord,
    pub task_binding: TaskBindingDecisionRecord,
    pub intent_hash_version: String,
    pub intent_fingerprint_hex: String,
}

impl RoutingBindingLogLineV1 {
    /// Semantic bundle of [`Self::routing`] + [`Self::task_binding`] (ADR-001 + ADR-004).
    #[must_use]
    pub fn routing_binding_snapshot(&self) -> SwarmRoutingBindingSnapshot {
        SwarmRoutingBindingSnapshot::new(self.routing.clone(), self.task_binding.clone())
    }
}

#[must_use]
pub fn build_routing_binding_log_line_v1(
    created_at_ms: u64,
    message: &MessageRecord,
    task_scope_id: &str,
    swarm: &crate::orchestrator::SwarmMessageClassification,
) -> RoutingBindingLogLineV1 {
    build_routing_binding_log_line_v1_headless_from_snapshot(
        created_at_ms,
        message.id.clone(),
        message.session_id.clone(),
        task_scope_id,
        &message.body,
        swarm.routing_binding_snapshot(),
    )
}

#[must_use]
pub fn build_routing_binding_log_line_v1_headless(
    created_at_ms: u64,
    message_id: impl Into<String>,
    session_id: impl Into<String>,
    task_scope_id: impl Into<String>,
    user_message_body: &str,
    routing: RoutingDecisionRecord,
    task_binding: TaskBindingDecisionRecord,
) -> RoutingBindingLogLineV1 {
    let message_id = message_id.into();
    let session_id = session_id.into();
    let task_scope_id = task_scope_id.into();
    RoutingBindingLogLineV1 {
        schema: ROUTING_BINDING_LOG_SCHEMA_V1.to_owned(),
        created_at_ms,
        message_id,
        session_id: session_id.clone(),
        conversation_id: Some(session_id),
        task_scope_id,
        routing,
        task_binding,
        intent_hash_version: INTENT_HASH_VERSION_V1.to_owned(),
        intent_fingerprint_hex: intent_fingerprint_v1_hex(user_message_body),
    }
}

/// Like [`build_routing_binding_log_line_v1_headless`], taking a [`SwarmRoutingBindingSnapshot`].
#[must_use]
pub fn build_routing_binding_log_line_v1_headless_from_snapshot(
    created_at_ms: u64,
    message_id: impl Into<String>,
    session_id: impl Into<String>,
    task_scope_id: impl Into<String>,
    user_message_body: &str,
    snapshot: SwarmRoutingBindingSnapshot,
) -> RoutingBindingLogLineV1 {
    build_routing_binding_log_line_v1_headless(
        created_at_ms,
        message_id,
        session_id,
        task_scope_id,
        user_message_body,
        snapshot.routing,
        snapshot.task_binding,
    )
}

pub fn append_routing_binding_log_line(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    line: &RoutingBindingLogLineV1,
) -> Result<PathBuf> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let rel = routing_binding_journal_relative(task_id);
    let json = serde_json::to_string(line).context("serialize routing binding log line")?;
    store.append_utf8_line_in_namespace(namespace, rel, &json)
}

pub fn coordination_implicit_intent_relative_path(task_id: &str) -> PathBuf {
    implicit_intent_journal_relative(task_id)
}

pub fn load_implicit_intent_dedupe_keys(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
) -> Result<HashSet<ImplicitIntentDedupeKey>> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let path = store.resolve_in_namespace(
        namespace,
        coordination_implicit_intent_relative_path(task_id),
    );
    if !path.exists() {
        return Ok(HashSet::new());
    }

    let text = fs::read_to_string(&path)
        .with_context(|| format!("read implicit-intent dedupe log {}", path.display()))?;
    let mut set = HashSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let rec: ImplicitIntentDedupeRecord = serde_json::from_str(line)
            .with_context(|| format!("parse implicit intent record in {}", path.display()))?;
        if rec.schema != IMPLICIT_INTENT_RECORD_SCHEMA_V1 {
            continue;
        }
        set.insert(rec.dedupe_key());
    }

    Ok(set)
}

pub fn append_implicit_intent_dedupe_record(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    record: &ImplicitIntentDedupeRecord,
) -> Result<PathBuf> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let rel = coordination_implicit_intent_relative_path(task_id);
    let json = serde_json::to_string(record).context("serialize implicit intent dedupe record")?;
    store.append_utf8_line_in_namespace(namespace, rel, &json)
}

pub const MATERIALIZATION_NOTICE_SCHEMA_V1: &str = "materialization_notice_v1";

/// One line in [`materialization_notices_journal_relative`](hc_store::task_coordination::materialization_notices_journal_relative).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterializationNoticeRecordV1 {
    pub schema: String,
    pub created_at_ms: u64,
    pub task_id: String,
    pub notice: String,
    pub planned_seed_count: usize,
    pub materialized_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_max_agents_per_task: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_max_new_agents_per_round: Option<usize>,
}

pub fn append_materialization_notice_record(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    record: &MaterializationNoticeRecordV1,
) -> Result<PathBuf> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let rel = materialization_notices_journal_relative(task_id);
    let json = serde_json::to_string(record).context("serialize materialization notice record")?;
    store.append_utf8_line_in_namespace(namespace, rel, &json)
}

pub const WORK_ITEM_CLAIM_JOURNAL_SCHEMA_V1: &str = "work_item_claim_journal_v1";
pub const WORK_ITEM_ASSIGNMENT_JOURNAL_SCHEMA_V1: &str = "work_item_assignment_journal_v1";

/// One line in [`hc_store::task_coordination::work_item_claims_journal_relative`] (ADR-003 P0 replay).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkItemClaimJournalLineV1 {
    pub schema: String,
    pub created_at_ms: u64,
    pub task_id: String,
    pub claim: WorkItemClaim,
}

/// One line in [`hc_store::task_coordination::work_item_assignments_journal_relative`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkItemAssignmentJournalLineV1 {
    pub schema: String,
    pub created_at_ms: u64,
    pub task_id: String,
    pub assignment: WorkItemAssignment,
}

pub fn append_work_item_claim_journal_line(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    claim: &WorkItemClaim,
) -> Result<PathBuf> {
    let line = WorkItemClaimJournalLineV1 {
        schema: WORK_ITEM_CLAIM_JOURNAL_SCHEMA_V1.to_owned(),
        created_at_ms: wall_clock_ms(),
        task_id: task_id.to_owned(),
        claim: claim.clone(),
    };
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let rel = work_item_claims_journal_relative(task_id);
    let json = serde_json::to_string(&line).context("serialize work item claim journal line")?;
    store.append_utf8_line_in_namespace(namespace, rel, &json)
}

pub fn append_work_item_assignment_journal_line(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    assignment: &WorkItemAssignment,
) -> Result<PathBuf> {
    let line = WorkItemAssignmentJournalLineV1 {
        schema: WORK_ITEM_ASSIGNMENT_JOURNAL_SCHEMA_V1.to_owned(),
        created_at_ms: wall_clock_ms(),
        task_id: task_id.to_owned(),
        assignment: assignment.clone(),
    };
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let rel = work_item_assignments_journal_relative(task_id);
    let json =
        serde_json::to_string(&line).context("serialize work item assignment journal line")?;
    store.append_utf8_line_in_namespace(namespace, rel, &json)
}

fn read_work_item_claims_journal_last_by_id(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
) -> Result<BTreeMap<String, WorkItemClaim>> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let path = store.resolve_in_namespace(namespace, work_item_claims_journal_relative(task_id));
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read work item claims journal {}", path.display()))?;

    let mut map: BTreeMap<String, WorkItemClaim> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let rec: WorkItemClaimJournalLineV1 = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if rec.schema != WORK_ITEM_CLAIM_JOURNAL_SCHEMA_V1 || rec.task_id != task_id {
            continue;
        }
        map.insert(rec.claim.id.clone(), rec.claim);
    }
    Ok(map)
}

fn read_work_item_assignments_journal_last_by_id(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
) -> Result<BTreeMap<String, WorkItemAssignment>> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let path = store.resolve_in_namespace(namespace, work_item_assignments_journal_relative(task_id));
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read work item assignments journal {}", path.display()))?;

    let mut map: BTreeMap<String, WorkItemAssignment> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let rec: WorkItemAssignmentJournalLineV1 = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if rec.schema != WORK_ITEM_ASSIGNMENT_JOURNAL_SCHEMA_V1 || rec.task_id != task_id {
            continue;
        }
        map.insert(rec.assignment.id.clone(), rec.assignment);
    }
    Ok(map)
}

/// Replay append-only journals into [`TaskPlan::work_item_claims`] and [`TaskPlan::work_item_assignments`].
///
/// Rows are keyed by **`claim.id` / `assignment.id`**; duplicate keys use **last line wins** inside each file.
/// In-memory entries not present in either journal are retained (typically empty on UI cold boot).
pub fn hydrate_task_plan_work_item_coordination_journals(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    plan: &mut TaskPlan,
) -> Result<()> {
    let journal_claims = read_work_item_claims_journal_last_by_id(
        workspace_root.as_ref(),
        namespace,
        task_id,
    )?;
    let journal_assignments = read_work_item_assignments_journal_last_by_id(
        workspace_root.as_ref(),
        namespace,
        task_id,
    )?;

    let mut merged_claims: BTreeMap<String, WorkItemClaim> = plan
        .work_item_claims
        .iter()
        .map(|c| (c.id.clone(), c.clone()))
        .collect();
    for (id, claim) in journal_claims {
        merged_claims.insert(id, claim);
    }
    plan.work_item_claims = merged_claims.into_values().collect();
    plan.work_item_claims.sort_by(|left, right| left.id.cmp(&right.id));

    let mut merged_assignments: BTreeMap<String, WorkItemAssignment> = plan
        .work_item_assignments
        .iter()
        .map(|a| (a.id.clone(), a.clone()))
        .collect();
    for (id, assignment) in journal_assignments {
        merged_assignments.insert(id, assignment);
    }
    plan.work_item_assignments = merged_assignments.into_values().collect();
    plan.work_item_assignments.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(())
}

pub fn persist_materialized_agents(
    workspace_root: impl AsRef<Path>,
    agents: &[MaterializedAgent],
) -> Result<Vec<PersistedAgentAssets>> {
    let workspace_root = workspace_root.as_ref();
    let mut persisted = Vec::new();

    for agent in agents {
        let namespace = WorkspaceNamespace::new(
            agent.persona.namespace.tenant_id.clone(),
            agent.persona.namespace.user_id.clone(),
        );
        let persona_repo = PersonaRepository::with_namespace(workspace_root, namespace.clone());
        let capability_repo = CapabilityRepository::with_namespace(workspace_root, namespace);

        let persona_path = persona_repo.write_profile(&agent.persona)?;
        let mut capability_paths = Vec::new();
        for capability in &agent.capabilities {
            capability_paths.push(capability_repo.write_profile(capability)?);
        }

        persisted.push(PersistedAgentAssets {
            persona_path,
            capability_paths,
        });
    }

    Ok(persisted)
}

pub fn persist_incubation_report(
    workspace_root: impl AsRef<Path>,
    namespace: WorkspaceNamespace,
    report: &IncubationReport,
) -> Result<PersistedIncubationArtifacts> {
    let repository =
        MemoryRepository::with_namespace(workspace_root.as_ref().to_path_buf(), namespace.clone());
    let record = build_memory_record_from_report(report)
        .with_namespace(MemoryNamespace::new(namespace.tenant_id, namespace.user_id))
        .with_visibility(MemoryVisibility::Private);
    let memory_path = repository.write_record(&record)?;

    Ok(PersistedIncubationArtifacts { memory_path })
}

const HTTP_IMPLICIT_TASK_GOAL_STUB_MAX_CHARS: usize = 4_096;

fn truncate_http_implicit_task_goal_stub(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return "(HTTP implicit task; awaiting planner input)".to_owned();
    }
    let count = t.chars().count();
    if count <= HTTP_IMPLICIT_TASK_GOAL_STUB_MAX_CHARS {
        return t.to_owned();
    }
    let head: String = t
        .chars()
        .take(HTTP_IMPLICIT_TASK_GOAL_STUB_MAX_CHARS)
        .collect();
    format!("{head}\n\n[truncated from {count} graphemes]")
}

/// Materialize **`coordination/<slug>/task_plan.md`** for an HTTP implicit task id (**`task.http.implicit.{ms}`**)
/// when the file is missing or has an empty body, using [`TaskPlan::awaiting_planner_input`] (ADR coordination layout).
///
/// Appends a **`Planning Notes`** line tying the stub to **`routing_message_id`** / **`session_id`** (same-turn trace join),
/// and seeds **one `Planned` work item** (via direct `work_items.push` — **does not** call `TaskPlan::add_work_item`
/// nor promote status away from **`AwaitingPlannerInput`**; full planner decomposition may add further items /
/// replace this holder later).
/// Non-empty existing bodies are preserved (manual edits / full planner persistence win).
pub fn ensure_http_implicit_task_plan_stub(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    implicit_task_id: &str,
    user_message_goal: &str,
    swarm_routing_message_id: &str,
    swarm_session_id: &str,
) -> Result<()> {
    let relative = task_plan_markdown_relative(implicit_task_id);
    let write_stub = match read_task_artifact(workspace_root.as_ref(), namespace, &relative) {
        Ok(doc) => doc.body.trim().is_empty(),
        Err(_) => true,
    };
    if !write_stub {
        return Ok(());
    }

    use crate::task::TaskNamespace;

    let goal = truncate_http_implicit_task_goal_stub(user_message_goal);
    let task =
        TaskRequest::new(implicit_task_id.to_owned(), "HTTP implicit task", goal).with_namespace(
            TaskNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()),
        );
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.planning_notes.push(format!(
        "HTTP implicit trace: routing_message_id={swarm_routing_message_id} session_id={swarm_session_id}",
    ));
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "HTTP implicit intent holder".to_owned(),
        goal: "Anchors the user L2/L3 turn until planner decomposition adds executable work items."
            .to_owned(),
        stage: "implicit".to_owned(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });
    persist_task_artifacts(workspace_root, &task, &plan)?;
    Ok(())
}

/// Persists `coordination/.../task_plan.md` (+ task room summary) and assignment decisions.
///
/// Clones `plan` internally, runs [`TaskPlan::prune_http_implicit_work_item_placeholder`] on the clone
/// before render, so the borrowed `plan` is **not** updated. Callers that keep a live [`TaskPlan`]
/// should use [`persist_task_artifacts_with_in_memory_prune`] instead of duplicating the post-persist prune.
pub fn persist_task_artifacts(
    workspace_root: impl AsRef<Path>,
    task: &TaskRequest,
    plan: &TaskPlan,
) -> Result<PersistedTaskArtifacts> {
    let mut plan_for_store = plan.clone();
    plan_for_store.prune_http_implicit_work_item_placeholder();
    let plan = &plan_for_store;

    let namespace = WorkspaceNamespace::new(
        task.namespace.tenant_id.clone(),
        task.namespace.user_id.clone(),
    );
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let timestamp = wall_clock_ms().to_string();
    let room_id = task_room_id(task);

    let task_plan_document_id = format!("task-plan.{}", task.id);
    let task_plan_relative_path = task_plan_markdown_relative(&task.id);
    let task_plan_path = store.write_markdown_in_namespace(
        &namespace,
        &task_plan_relative_path,
        &TaskArtifactFrontmatter {
            id: task_plan_document_id.clone(),
            doc_type: "task_plan".to_owned(),
            title: format!("Task Plan | {}", task.title),
            tenant_id: task.namespace.tenant_id.clone(),
            user_id: task.namespace.user_id.clone(),
            status: task_plan_status_label(&plan.status).to_owned(),
            tags: vec!["task".to_owned(), "planning".to_owned()],
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
        },
        &render_task_plan_body(task, plan),
    )?;
    let task_plan_memory_path = persist_room_memory(
        workspace_root.as_ref().to_path_buf(),
        namespace.clone(),
        &RoomMemoryWriteRequest::new(
            room_id.clone(),
            hc_memory::MemoryLayer::Task,
            format!("Task Plan | {}", task.title),
            summarize_task_plan_for_room_memory(task, plan),
            hc_memory::MemoryKind::Summary,
        )
        .with_visibility(MemoryVisibility::Private)
        .with_owner(hc_memory::MemoryOwnerRef::task(task.id.clone()))
        .with_tag("task")
        .with_tag("planning")
        .with_source_doc(task_plan_relative_path.to_string_lossy().replace('\\', "/"))
        .with_derived_from(task_plan_document_id.clone())
        .with_file_name("task/plan/task-plan.summary.md")
        .with_asset_id(format!("asset.{}.task-plan", room_id)),
    )?;

    let mut assignment_paths = Vec::new();
    let mut assignment_memory_paths = Vec::new();
    for assignment in &plan.work_item_assignments {
        let work_item = plan
            .work_items
            .iter()
            .find(|item| item.id == assignment.work_item_id);
        let assignment_relative_path =
            assignment_decision_markdown_relative(&task.id, &assignment.id);
        let assignment_document_id = format!("assignment-decision.{}", assignment.id);
        assignment_paths.push(store.write_markdown_in_namespace(
            &namespace,
            &assignment_relative_path,
            &TaskArtifactFrontmatter {
                id: assignment_document_id.clone(),
                doc_type: "assignment_decision".to_owned(),
                title: format!("Assignment | {} | {}", task.title, assignment.agent_name),
                tenant_id: task.namespace.tenant_id.clone(),
                user_id: task.namespace.user_id.clone(),
                status: assignment.status.clone(),
                tags: vec!["task".to_owned(), "assignment".to_owned()],
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            },
            &render_assignment_body(task, plan, assignment, work_item),
        )?);
        assignment_memory_paths.push(persist_room_memory(
            workspace_root.as_ref().to_path_buf(),
            namespace.clone(),
            &RoomMemoryWriteRequest::new(
                room_id.clone(),
                hc_memory::MemoryLayer::Task,
                format!("Assignment | {} | {}", task.title, assignment.agent_name),
                summarize_assignment_for_room_memory(task, assignment, work_item),
                hc_memory::MemoryKind::Decision,
            )
            .with_visibility(MemoryVisibility::Private)
            .with_owner(hc_memory::MemoryOwnerRef::task(task.id.clone()))
            .with_tag("task")
            .with_tag("assignment")
            .with_source_doc(
                assignment_relative_path
                    .to_string_lossy()
                    .replace('\\', "/"),
            )
            .with_derived_from(assignment_document_id)
            .with_file_name(format!(
                "task/plan/assignment-decision.{}.md",
                coordination_segment_slug(&assignment.id)
            ))
            .with_asset_id(format!("asset.{}.assignment.{}", room_id, assignment.id)),
        )?);
    }

    Ok(PersistedTaskArtifacts {
        task_plan_path,
        assignment_paths,
        task_plan_memory_path,
        assignment_memory_paths,
    })
}

/// Same as [`persist_task_artifacts`], then applies [`TaskPlan::prune_http_implicit_work_item_placeholder`]
/// to the **same** in-memory `plan` so UI / workbench registries match disk after persist.
pub fn persist_task_artifacts_with_in_memory_prune(
    workspace_root: impl AsRef<Path>,
    task: &TaskRequest,
    plan: &mut TaskPlan,
) -> Result<PersistedTaskArtifacts> {
    let artifacts = persist_task_artifacts(workspace_root, task, plan)?;
    plan.prune_http_implicit_work_item_placeholder();
    Ok(artifacts)
}

pub fn rebuild_task_artifact_index(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
) -> Result<Vec<TaskArtifactSummary>> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let index = store.rebuild_markdown_index_in_namespace(namespace)?;
    Ok(index
        .documents
        .into_iter()
        .filter_map(task_artifact_summary_from_index_entry)
        .collect())
}

pub fn query_task_artifacts(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    query: &TaskArtifactQuery,
) -> Result<Vec<TaskArtifactSummary>> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let mut markdown_query = MarkdownQuery::default();
    if let Some(kind) = &query.kind {
        markdown_query = markdown_query.with_doc_type(kind.as_doc_type());
    }
    if let Some(status) = &query.status {
        markdown_query = markdown_query.with_status(status);
    }
    if let Some(task_id) = &query.task_id {
        markdown_query = markdown_query.with_text(task_id);
    }
    if let Some(limit) = query.limit {
        markdown_query = markdown_query.with_limit(limit);
    }

    Ok(store
        .query_markdown_index_in_namespace(namespace, &markdown_query)?
        .into_iter()
        .filter(|entry| {
            let p = entry.relative_path.as_str();
            p.starts_with("decisions/")
                || (p.starts_with("coordination/") && p.ends_with(".md"))
        })
        .filter_map(task_artifact_summary_from_index_entry)
        .collect())
}

pub fn read_task_artifact(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    relative_path: impl AsRef<Path>,
) -> Result<TaskArtifactDocument> {
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let relative_path = relative_path.as_ref().to_path_buf();
    let stored: StoredMarkdown<TaskArtifactFrontmatter> =
        store.read_markdown_in_namespace(namespace, &relative_path)?;
    let kind = TaskArtifactKind::from_doc_type(&stored.frontmatter.doc_type).ok_or_else(|| {
        anyhow::anyhow!(
            "document at {} is not a recognized task artifact type: {}",
            relative_path.display(),
            stored.frontmatter.doc_type
        )
    })?;

    Ok(TaskArtifactDocument {
        id: stored.frontmatter.id,
        kind,
        title: stored.frontmatter.title,
        status: stored.frontmatter.status,
        relative_path: relative_path.to_string_lossy().replace('\\', "/"),
        tags: stored.frontmatter.tags,
        created_at: stored.frontmatter.created_at,
        updated_at: stored.frontmatter.updated_at,
        body: stored.body,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TaskArtifactFrontmatter {
    id: String,
    #[serde(rename = "type")]
    doc_type: String,
    title: String,
    tenant_id: String,
    user_id: String,
    status: String,
    tags: Vec<String>,
    created_at: String,
    updated_at: String,
}

fn task_artifact_summary_from_index_entry(
    entry: MarkdownIndexEntry,
) -> Option<TaskArtifactSummary> {
    let task_hint = task_hint_from_entry(&entry);
    let kind = TaskArtifactKind::from_doc_type(&entry.doc_type)?;
    let status = entry.status.unwrap_or_else(|| "unknown".to_owned());

    Some(TaskArtifactSummary {
        id: entry.id,
        kind,
        title: entry.title,
        status,
        relative_path: entry.relative_path,
        tags: entry.tags,
        task_hint,
        body_preview: entry.body_preview,
    })
}

fn task_hint_from_entry(entry: &MarkdownIndexEntry) -> Option<String> {
    if let Some(stripped) = entry.id.strip_prefix("task-plan.") {
        return Some(stripped.to_owned());
    }

    let lower_preview = entry.body_preview.to_ascii_lowercase();
    let marker = "- task: ";
    let position = lower_preview.find(marker)?;
    let value = &entry.body_preview[position + marker.len()..];
    let task_ref = value.split('|').next()?.trim();
    if task_ref.is_empty() {
        None
    } else {
        Some(task_ref.to_owned())
    }
}

fn render_task_plan_body(task: &TaskRequest, plan: &TaskPlan) -> String {
    let mut body = String::new();
    body.push_str("# Task\n\n");
    body.push_str(&format!("- id: {}\n", task.id));
    body.push_str(&format!("- title: {}\n", task.title));
    body.push_str(&format!("- goal: {}\n", task.goal));
    body.push_str(&format!(
        "- budget: {} tokens / {} minutes / {} evolution reserve\n\n",
        task.budget.token_budget,
        task.budget.time_budget_minutes,
        task.budget.evolution_reserve_tokens
    ));

    body.push_str("# Planning Notes\n\n");
    if plan.planning_notes.is_empty() {
        body.push_str("- none\n");
    } else {
        for note in &plan.planning_notes {
            body.push_str(&format!("- {}\n", note));
        }
    }

    body.push_str("\n# Work Items\n\n");
    if plan.work_items.is_empty() {
        body.push_str("- none\n");
    } else {
        for item in &plan.work_items {
            body.push_str(&format!(
                "- {} | stage={} | lifecycle={} | {} | {} tokens | {} minutes\n",
                item.id,
                item.stage,
                item.lifecycle,
                item.title,
                item.estimated_token_cost,
                item.estimated_time_minutes
            ));
            body.push_str(&format!("  goal: {}\n", item.goal));
        }
    }

    body.push_str("\n# Agent Proposals\n\n");
    if plan.agent_proposals.is_empty() {
        body.push_str("- none\n");
    } else {
        for proposal in &plan.agent_proposals {
            body.push_str(&format!(
                "- {} | role={} | status={} | {}\n",
                proposal.id, proposal.role, proposal.status, proposal.reason
            ));
        }
    }

    body.push_str("\n# Work Item Claims\n\n");
    if plan.work_item_claims.is_empty() {
        body.push_str("- none\n");
    } else {
        for claim in &plan.work_item_claims {
            body.push_str(&format!(
                "- {} | work_item={} | agent={} | status={} | score {:.2} | {}\n",
                claim.id,
                claim.work_item_id,
                claim.agent_name,
                claim.status,
                claim.score,
                claim.reason
            ));
        }
    }

    body.push_str("\n# Assignments\n\n");
    if plan.work_item_assignments.is_empty() {
        body.push_str("- none\n");
    } else {
        for assignment in &plan.work_item_assignments {
            body.push_str(&format!(
                "- {} | work_item={} | agent={} | status={} | {}\n",
                assignment.id,
                assignment.work_item_id,
                assignment.agent_name,
                assignment.status,
                assignment.rationale
            ));
        }
    }

    body
}

fn render_assignment_body(
    task: &TaskRequest,
    plan: &TaskPlan,
    assignment: &crate::planning::WorkItemAssignment,
    work_item: Option<&crate::planning::WorkItem>,
) -> String {
    let mut body = String::new();
    body.push_str("# Assignment Decision\n\n");
    body.push_str(&format!("- task: {} | {}\n", task.id, task.title));
    body.push_str(&format!("- assignment: {}\n", assignment.id));
    body.push_str(&format!("- work item: {}\n", assignment.work_item_id));
    body.push_str(&format!(
        "- assigned agent: {} ({})\n",
        assignment.agent_name, assignment.agent_instance_id
    ));
    body.push_str(&format!("- assignment status: {}\n", assignment.status));
    body.push_str(&format!("- rationale: {}\n", assignment.rationale));
    if let Some(item) = work_item {
        body.push_str(&format!("- work item title: {}\n", item.title));
        body.push_str(&format!("- work item goal: {}\n", item.goal));
        body.push_str(&format!("- work item stage: {}\n", item.stage));
    }

    let related_claims = plan
        .work_item_claims
        .iter()
        .filter(|claim| claim.work_item_id == assignment.work_item_id)
        .collect::<Vec<_>>();
    body.push_str("\n# Claims\n\n");
    if related_claims.is_empty() {
        body.push_str("- none\n");
    } else {
        for claim in related_claims {
            body.push_str(&format!(
                "- {} | {} | score={:.2} | status={} | {}\n",
                claim.id, claim.agent_name, claim.score, claim.status, claim.reason
            ));
        }
    }

    body
}

fn task_plan_status_label(status: &TaskPlanStatus) -> &'static str {
    match status {
        TaskPlanStatus::AwaitingPlannerInput => "awaiting_planner_input",
        TaskPlanStatus::Drafted => "drafted",
        TaskPlanStatus::Approved => "approved",
    }
}

fn task_room_id(task: &TaskRequest) -> String {
    format!("room.task.{}", task.id)
}

fn summarize_task_plan_for_room_memory(task: &TaskRequest, plan: &TaskPlan) -> String {
    let planning_note = plan
        .planning_notes
        .first()
        .cloned()
        .unwrap_or_else(|| "No explicit planning notes recorded.".to_owned());
    format!(
        "Task {} is {}. Goal: {}. Work items: {}. Assignments: {}. First planning note: {}",
        task.id,
        task_plan_status_label(&plan.status),
        task.goal,
        plan.work_items.len(),
        plan.work_item_assignments.len(),
        planning_note
    )
}

fn summarize_assignment_for_room_memory(
    task: &TaskRequest,
    assignment: &crate::planning::WorkItemAssignment,
    work_item: Option<&crate::planning::WorkItem>,
) -> String {
    let work_item_summary = work_item
        .map(|item| format!("{} | {} | stage={}", item.title, item.goal, item.stage))
        .unwrap_or_else(|| assignment.work_item_id.clone());
    format!(
        "Task {} assigned work item {} to {} ({}) with status {}. Rationale: {}",
        task.id,
        work_item_summary,
        assignment.agent_name,
        assignment.agent_instance_id,
        assignment.status,
        assignment.rationale
    )
}

#[cfg(test)]
#[path = "../tests/unit/persistence.rs"]
mod tests;
