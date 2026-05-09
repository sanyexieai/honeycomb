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
    store::{
        MarkdownIndexEntry, MarkdownQuery, StoredMarkdown, WorkspaceNamespace, WorkspaceStore,
    },
    task_coordination::{
        assignment_decision_markdown_relative, coordination_segment_slug,
        implicit_intent_journal_relative, materialization_notices_journal_relative,
        routing_binding_journal_relative, task_plan_markdown_relative,
        work_item_assignments_journal_relative, work_item_claims_journal_relative,
    },
};
use serde::{Deserialize, Serialize};

use hc_protocol::swarm::{
    ARTIFACT_SCHEMA_V1, ArtifactHeaderV1, ArtifactKindV1, ExecutionResultArtifactV1,
    IMPLICIT_INTENT_RECORD_SCHEMA_V1, INTENT_HASH_VERSION_V1, ImplicitIntentDedupeKey,
    ImplicitIntentDedupeRecord, PlanNoteArtifactV1, ReviewNoteArtifactV1, RoutingDecisionRecord,
    RoutingTier, SwarmRoutingBindingSnapshot, TaskBindingDecisionRecord, WorkItemLifecycleState,
    claim_capability_eligible_for_p0_assign_v1, intent_fingerprint_v1_hex,
};

use crate::{
    TaskPlan, TaskRequest,
    bootstrap::MaterializedAgent,
    incubation::{IncubationReport, build_memory_record_from_report},
    planning::{
        HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID, TaskPlanStatus, WorkItem, WorkItemAssignment,
        WorkItemClaim,
    },
    task::{TaskBudget, TaskNamespace},
};

const HTTP_CHAT_L23_DEGENERATE_CLAIM_REASON: &str =
    "HTTP L2/L3 degenerate execution (ADR-002 single_llm_route_agent)";

/// Persists an ADR-005 **`execution_result`** as pretty JSON in the task room under
/// **`task/execution/execution-result.<slug>.json`** (compressed room asset).
pub fn persist_execution_result_artifact_v1(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task: &TaskRequest,
    work_item_id: impl AsRef<str>,
    summary: impl Into<String>,
    details: Option<String>,
    producer: impl Into<String>,
) -> Result<PathBuf> {
    let work_item_id = work_item_id.as_ref().trim();
    if work_item_id.is_empty() {
        anyhow::bail!("work_item_id required for execution_result artifact");
    }
    let ms = wall_clock_ms();
    let id = format!(
        "execution-result.http.{}.{}",
        coordination_segment_slug(work_item_id),
        ms
    );
    let artifact = ExecutionResultArtifactV1 {
        header: ArtifactHeaderV1 {
            id: id.clone(),
            task_id: task.id.clone(),
            work_item_id: Some(work_item_id.to_owned()),
            artifact_kind: ArtifactKindV1::ExecutionResult,
            schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
            created_at_ms: ms,
            producer: producer.into(),
        },
        summary: summary.into(),
        details,
    };
    artifact
        .validate()
        .map_err(|e| anyhow::anyhow!("execution_result artifact invalid: {e:?}"))?;
    let json = serde_json::to_string_pretty(&artifact)?;
    let room_id = task_room_id(task);
    let file_slug = coordination_segment_slug(&id);
    persist_room_memory(
        workspace_root.as_ref().to_path_buf(),
        namespace.clone(),
        &RoomMemoryWriteRequest::new(
            room_id.clone(),
            hc_memory::MemoryLayer::Task,
            format!("Execution result | {work_item_id}"),
            json,
            hc_memory::MemoryKind::WorkflowMemory,
        )
        .with_visibility(MemoryVisibility::Private)
        .with_owner(hc_memory::MemoryOwnerRef::task(task.id.clone()))
        .with_tag("task")
        .with_tag("execution")
        .with_tag("artifact_schema_v1")
        .with_derived_from(id.clone())
        .with_file_name(format!("task/execution/execution-result.{file_slug}.json"))
        .with_asset_id(format!("asset.{room_id}.execution-result.{file_slug}")),
    )
}

/// Persists an ADR-005 **`plan_note`** as pretty JSON under **`task/plan/plan-note.<slug>.json`**.
pub fn persist_plan_note_artifact_v1(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task: &TaskRequest,
    work_item_id: Option<&str>,
    summary: impl Into<String>,
    details: Option<String>,
    producer: impl Into<String>,
) -> Result<PathBuf> {
    let maybe_work_item_id = work_item_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let id_scope = maybe_work_item_id.as_deref().unwrap_or(task.id.as_str());
    let ms = wall_clock_ms();
    let id = format!(
        "plan-note.http.{}.{}",
        coordination_segment_slug(id_scope),
        ms
    );
    let artifact = PlanNoteArtifactV1 {
        header: ArtifactHeaderV1 {
            id: id.clone(),
            task_id: task.id.clone(),
            work_item_id: maybe_work_item_id,
            artifact_kind: ArtifactKindV1::PlanNote,
            schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
            created_at_ms: ms,
            producer: producer.into(),
        },
        summary: summary.into(),
        details,
    };
    artifact
        .validate()
        .map_err(|e| anyhow::anyhow!("plan_note artifact invalid: {e:?}"))?;
    let json = serde_json::to_string_pretty(&artifact)?;
    let room_id = task_room_id(task);
    let file_slug = coordination_segment_slug(&id);
    persist_room_memory(
        workspace_root.as_ref().to_path_buf(),
        namespace.clone(),
        &RoomMemoryWriteRequest::new(
            room_id.clone(),
            hc_memory::MemoryLayer::Task,
            format!("Plan note | {}", artifact.header.id),
            json,
            hc_memory::MemoryKind::Decision,
        )
        .with_visibility(MemoryVisibility::Private)
        .with_owner(hc_memory::MemoryOwnerRef::task(task.id.clone()))
        .with_tag("task")
        .with_tag("plan")
        .with_tag("artifact_schema_v1")
        .with_derived_from(id.clone())
        .with_file_name(format!("task/plan/plan-note.{file_slug}.json"))
        .with_asset_id(format!("asset.{room_id}.plan-note.{file_slug}")),
    )
}

/// Persists an ADR-005 **`review_note`** as pretty JSON under **`task/review/review-note.<slug>.json`**.
pub fn persist_review_note_artifact_v1(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task: &TaskRequest,
    work_item_id: impl AsRef<str>,
    summary: impl Into<String>,
    verdict: Option<String>,
    details: Option<String>,
    producer: impl Into<String>,
) -> Result<PathBuf> {
    let work_item_id = work_item_id.as_ref().trim();
    if work_item_id.is_empty() {
        anyhow::bail!("work_item_id required for review_note artifact");
    }
    let ms = wall_clock_ms();
    let id = format!(
        "review-note.http.{}.{}",
        coordination_segment_slug(work_item_id),
        ms
    );
    let artifact = ReviewNoteArtifactV1 {
        header: ArtifactHeaderV1 {
            id: id.clone(),
            task_id: task.id.clone(),
            work_item_id: Some(work_item_id.to_owned()),
            artifact_kind: ArtifactKindV1::ReviewNote,
            schema_version: ARTIFACT_SCHEMA_V1.to_owned(),
            created_at_ms: ms,
            producer: producer.into(),
        },
        summary: summary.into(),
        verdict,
        details,
    };
    artifact
        .validate()
        .map_err(|e| anyhow::anyhow!("review_note artifact invalid: {e:?}"))?;
    let json = serde_json::to_string_pretty(&artifact)?;
    let room_id = task_room_id(task);
    let file_slug = coordination_segment_slug(&id);
    persist_room_memory(
        workspace_root.as_ref().to_path_buf(),
        namespace.clone(),
        &RoomMemoryWriteRequest::new(
            room_id.clone(),
            hc_memory::MemoryLayer::Task,
            format!("Review note | {work_item_id}"),
            json,
            hc_memory::MemoryKind::Decision,
        )
        .with_visibility(MemoryVisibility::Private)
        .with_owner(hc_memory::MemoryOwnerRef::task(task.id.clone()))
        .with_tag("task")
        .with_tag("review")
        .with_tag("artifact_schema_v1")
        .with_derived_from(id.clone())
        .with_file_name(format!("task/review/review-note.{file_slug}.json"))
        .with_asset_id(format!("asset.{room_id}.review-note.{file_slug}")),
    )
}

const HTTP_L23_EXECUTION_DIGEST_MAX_ITEMS: usize = 16;
const HTTP_L23_EXECUTION_DIGEST_MAX_CHARS: usize = 8_000;
const HTTP_L23_PLAN_DIGEST_MAX_ITEMS: usize = 16;
const HTTP_L23_PLAN_DIGEST_MAX_CHARS: usize = 4_000;
const HTTP_L23_REVIEW_DIGEST_MAX_ITEMS: usize = 16;
const HTTP_L23_REVIEW_DIGEST_MAX_CHARS: usize = 4_000;
pub const HTTP_L23_EXECUTION_DIGEST_HEADING: &str = "## Recent execution results (ADR-005)";
const HTTP_L23_EXECUTION_DIGEST_INTRO: &str = "Read-only summaries from task room `task/execution/*.json`. Use for a coherent planner-facing reply.";
pub const HTTP_L23_PLAN_DIGEST_HEADING: &str = "## Recent planning notes (ADR-005)";
const HTTP_L23_PLAN_DIGEST_INTRO: &str = "Read-only summaries from task room `task/plan/*.json`.";
pub const HTTP_L23_REVIEW_DIGEST_HEADING: &str = "## Recent review notes (ADR-005)";
const HTTP_L23_REVIEW_DIGEST_INTRO: &str =
    "Read-only summaries from task room `task/review/*.json`.";

fn render_http_l23_digest(
    heading: &str,
    intro: &str,
    chunks: impl IntoIterator<Item = String>,
    max_chars: usize,
    total_records: usize,
    artifact_kind_label: &str,
) -> String {
    let mut parts: Vec<String> = vec![
        String::new(),
        "---".to_owned(),
        heading.to_owned(),
        intro.to_owned(),
        String::new(),
    ];
    let mut total = parts.join("\n").len();
    let mut added = 0usize;
    let mut truncated = false;
    for chunk in chunks {
        if total + chunk.len() > max_chars {
            parts.push(format!(
                "\n[... truncated; {total_records} {artifact_kind_label} record(s) on disk ...]"
            ));
            truncated = true;
            break;
        }
        let chunk_len = chunk.len();
        parts.push(chunk);
        total += chunk_len + 1;
        added += 1;
    }
    if added == 0 && !truncated {
        return String::new();
    }
    format!("{}\n", parts.join("\n"))
}

/// Loads validated **`plan_note`** JSON from `memory/.../compressed/task/plan/` (newest first).
pub fn load_task_plan_note_artifacts_v1(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: impl AsRef<str>,
) -> Result<Vec<PlanNoteArtifactV1>> {
    let tid = task_id.as_ref().trim();
    if tid.is_empty() {
        return Ok(Vec::new());
    }
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let room = hc_memory::MemoryRoom::new(
        format!("room.task.{tid}"),
        hc_memory::MemoryLayer::Task,
        "task",
        "",
    );
    let rel_dir = hc_memory::MemoryRoomRepository::room_root_relative_path(&room)
        .join("compressed/task/plan");
    let abs = store.resolve_in_namespace(namespace, &rel_dir);
    if !abs.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in fs::read_dir(&abs).with_context(|| format!("read_dir {}", abs.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        match serde_json::from_str::<PlanNoteArtifactV1>(&raw) {
            Ok(artifact) => {
                if artifact.validate().is_ok() {
                    out.push(artifact);
                } else {
                    tracing::debug!(
                        path = %path.display(),
                        "skip plan_note JSON: validate failed"
                    );
                }
            }
            Err(error) => tracing::debug!(
                path = %path.display(),
                ?error,
                "skip file: not PlanNoteArtifactV1 JSON"
            ),
        }
    }
    out.sort_by_key(|artifact| std::cmp::Reverse(artifact.header.created_at_ms));
    Ok(out)
}

/// Loads validated **`execution_result`** JSON files from the task room
/// `memory/.../compressed/task/execution/` (newest [`ExecutionResultArtifactV1::header.created_at_ms`] first).
pub fn load_task_execution_result_artifacts_v1(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: impl AsRef<str>,
) -> Result<Vec<ExecutionResultArtifactV1>> {
    let tid = task_id.as_ref().trim();
    if tid.is_empty() {
        return Ok(Vec::new());
    }
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let room = hc_memory::MemoryRoom::new(
        format!("room.task.{tid}"),
        hc_memory::MemoryLayer::Task,
        "task",
        "",
    );
    let rel_dir = hc_memory::MemoryRoomRepository::room_root_relative_path(&room)
        .join("compressed/task/execution");
    let abs = store.resolve_in_namespace(namespace, &rel_dir);
    if !abs.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in fs::read_dir(&abs).with_context(|| format!("read_dir {}", abs.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        match serde_json::from_str::<ExecutionResultArtifactV1>(&raw) {
            Ok(artifact) => {
                if artifact.validate().is_ok() {
                    out.push(artifact);
                } else {
                    tracing::debug!(
                        path = %path.display(),
                        "skip execution_result JSON: validate failed"
                    );
                }
            }
            Err(error) => tracing::debug!(
                path = %path.display(),
                ?error,
                "skip file: not ExecutionResultArtifactV1 JSON"
            ),
        }
    }
    out.sort_by_key(|artifact| std::cmp::Reverse(artifact.header.created_at_ms));
    Ok(out)
}

/// Loads validated **`review_note`** JSON from `memory/.../compressed/task/review/` (newest first).
pub fn load_task_review_note_artifacts_v1(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: impl AsRef<str>,
) -> Result<Vec<ReviewNoteArtifactV1>> {
    let tid = task_id.as_ref().trim();
    if tid.is_empty() {
        return Ok(Vec::new());
    }
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let room = hc_memory::MemoryRoom::new(
        format!("room.task.{tid}"),
        hc_memory::MemoryLayer::Task,
        "task",
        "",
    );
    let rel_dir = hc_memory::MemoryRoomRepository::room_root_relative_path(&room)
        .join("compressed/task/review");
    let abs = store.resolve_in_namespace(namespace, &rel_dir);
    if !abs.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in fs::read_dir(&abs).with_context(|| format!("read_dir {}", abs.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        match serde_json::from_str::<ReviewNoteArtifactV1>(&raw) {
            Ok(artifact) => {
                if artifact.validate().is_ok() {
                    out.push(artifact);
                } else {
                    tracing::debug!(
                        path = %path.display(),
                        "skip review_note JSON: validate failed"
                    );
                }
            }
            Err(error) => tracing::debug!(
                path = %path.display(),
                ?error,
                "skip file: not ReviewNoteArtifactV1 JSON"
            ),
        }
    }
    out.sort_by_key(|artifact| std::cmp::Reverse(artifact.header.created_at_ms));
    Ok(out)
}

/// Markdown-ish digest for HTTP L2/L3 system prompt (ADR-005 outward speaker context).
#[must_use]
pub fn format_execution_results_digest_for_http_l23(
    artifacts: &[ExecutionResultArtifactV1],
) -> String {
    if artifacts.is_empty() {
        return String::new();
    }
    let chunks = artifacts
        .iter()
        .take(HTTP_L23_EXECUTION_DIGEST_MAX_ITEMS)
        .map(|artifact| {
            let wi = artifact.header.work_item_id.as_deref().unwrap_or("?");
            format!(
                "- `{}` | work_item `{}` | producer `{}`\n  {}",
                artifact.header.id, wi, artifact.header.producer, artifact.summary
            )
        });
    render_http_l23_digest(
        HTTP_L23_EXECUTION_DIGEST_HEADING,
        HTTP_L23_EXECUTION_DIGEST_INTRO,
        chunks,
        HTTP_L23_EXECUTION_DIGEST_MAX_CHARS,
        artifacts.len(),
        "execution_result",
    )
}

/// Markdown digest for **`plan_note`** artifacts (`task/plan/*.json`).
#[must_use]
pub fn format_plan_notes_digest_for_http_l23(artifacts: &[PlanNoteArtifactV1]) -> String {
    if artifacts.is_empty() {
        return String::new();
    }
    let chunks = artifacts
        .iter()
        .take(HTTP_L23_PLAN_DIGEST_MAX_ITEMS)
        .map(|artifact| {
            let wi = artifact
                .header
                .work_item_id
                .as_deref()
                .map(|value| format!(" | work_item `{value}`"))
                .unwrap_or_default();
            format!(
                "- `{}`{wi} | producer `{}`\n  {}",
                artifact.header.id, artifact.header.producer, artifact.summary
            )
        });
    render_http_l23_digest(
        HTTP_L23_PLAN_DIGEST_HEADING,
        HTTP_L23_PLAN_DIGEST_INTRO,
        chunks,
        HTTP_L23_PLAN_DIGEST_MAX_CHARS,
        artifacts.len(),
        "plan_note",
    )
}

/// Markdown digest for **`review_note`** artifacts (`task/review/*.json`).
#[must_use]
pub fn format_review_notes_digest_for_http_l23(artifacts: &[ReviewNoteArtifactV1]) -> String {
    if artifacts.is_empty() {
        return String::new();
    }
    let chunks = artifacts
        .iter()
        .take(HTTP_L23_REVIEW_DIGEST_MAX_ITEMS)
        .map(|artifact| {
            let wi = artifact.header.work_item_id.as_deref().unwrap_or("?");
            let verdict = artifact
                .verdict
                .as_deref()
                .map(|v| format!(" | verdict `{v}`"))
                .unwrap_or_default();
            format!(
                "- `{}` | work_item `{}` | producer `{}`{verdict}\n  {}",
                artifact.header.id, wi, artifact.header.producer, artifact.summary
            )
        });
    render_http_l23_digest(
        HTTP_L23_REVIEW_DIGEST_HEADING,
        HTTP_L23_REVIEW_DIGEST_INTRO,
        chunks,
        HTTP_L23_REVIEW_DIGEST_MAX_CHARS,
        artifacts.len(),
        "review_note",
    )
}

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
    let path =
        store.resolve_in_namespace(namespace, work_item_assignments_journal_relative(task_id));
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
    let journal_claims =
        read_work_item_claims_journal_last_by_id(workspace_root.as_ref(), namespace, task_id)?;
    let journal_assignments =
        read_work_item_assignments_journal_last_by_id(workspace_root.as_ref(), namespace, task_id)?;

    let mut merged_claims: BTreeMap<String, WorkItemClaim> = plan
        .work_item_claims
        .iter()
        .map(|c| (c.id.clone(), c.clone()))
        .collect();
    for (id, claim) in journal_claims {
        merged_claims.insert(id, claim);
    }
    plan.work_item_claims = merged_claims.into_values().collect();
    plan.work_item_claims
        .sort_by(|left, right| left.id.cmp(&right.id));

    let mut merged_assignments: BTreeMap<String, WorkItemAssignment> = plan
        .work_item_assignments
        .iter()
        .map(|a| (a.id.clone(), a.clone()))
        .collect();
    for (id, assignment) in journal_assignments {
        merged_assignments.insert(id, assignment);
    }
    plan.work_item_assignments = merged_assignments.into_values().collect();
    plan.work_item_assignments
        .sort_by(|left, right| left.id.cmp(&right.id));

    Ok(())
}

fn strip_task_md_kv_line<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let line = line.trim();
    let prefix = format!("- {key}: ");
    line.strip_prefix(&prefix).map(str::trim)
}

fn first_task_md_kv<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    header
        .lines()
        .find_map(|line| strip_task_md_kv_line(line, key))
}

fn task_plan_md_section<'a>(body: &'a str, heading_no_hash: &'a str) -> Option<&'a str> {
    let marker = format!("# {heading_no_hash}\n\n");
    let start = body.find(&marker)? + marker.len();
    let rest = &body[start..];
    if let Some(ix) = rest.find("\n\n# ") {
        Some(rest[..ix].trim_end_matches('\n'))
    } else {
        Some(rest.trim_end_matches('\n'))
    }
}

fn parse_task_budget_parts(line: &str) -> Option<TaskBudget> {
    let trimmed = strip_task_md_kv_line(line, "budget")?;

    let (tokens_rest, after_tokens) = trimmed.split_once("tokens /")?;
    let token_budget: u32 = tokens_rest.trim().parse().ok()?;

    let (minutes_chunk, reserve_chunk) = if let Some(pair) = after_tokens.split_once("minutes /") {
        (pair.0.trim(), pair.1.trim())
    } else {
        (after_tokens.trim(), "0")
    };

    let time_budget_minutes: u32 = minutes_chunk
        .strip_suffix("minutes")
        .unwrap_or(minutes_chunk)
        .trim()
        .parse()
        .ok()?;

    let reserve_raw = reserve_chunk
        .strip_suffix("evolution reserve")
        .unwrap_or(reserve_chunk)
        .trim();
    let evolution_reserve_tokens: u32 = reserve_raw.parse().unwrap_or(0);

    Some(TaskBudget {
        token_budget,
        time_budget_minutes,
        evolution_reserve_tokens,
    })
}

fn parse_planning_notes_block(section: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in section.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("- ") else {
            continue;
        };
        if rest == "none" {
            continue;
        }
        out.push(rest.to_owned());
    }
    out
}

fn parse_work_item_header_line_first(line_trim: &str) -> Option<WorkItemHeaderParts<'_>> {
    let item_line = line_trim.strip_prefix('-')?.trim_start();
    let parts: Vec<&str> = item_line.split(" | ").collect();
    if parts.len() < 6 {
        return None;
    }
    let id = parts[0].trim();
    let stage = parts[1].strip_prefix("stage=")?.trim();
    let lifecycle = parts[2].strip_prefix("lifecycle=")?.trim();
    let title = parts[3].trim();
    let tokens = parts[4]
        .strip_suffix("tokens")?
        .trim()
        .parse::<u32>()
        .ok()?;
    let minutes = parts[5]
        .strip_suffix("minutes")?
        .trim()
        .parse::<u32>()
        .ok()?;
    Some(WorkItemHeaderParts {
        id,
        stage,
        lifecycle,
        title,
        estimated_token_cost: tokens,
        estimated_time_minutes: minutes,
    })
}

struct WorkItemHeaderParts<'a> {
    id: &'a str,
    stage: &'a str,
    lifecycle: &'a str,
    title: &'a str,
    estimated_token_cost: u32,
    estimated_time_minutes: u32,
}

fn lifecycle_state_from_coordination_token(raw: &str) -> Option<WorkItemLifecycleState> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "planned" => Some(WorkItemLifecycleState::Planned),
        "claiming" => Some(WorkItemLifecycleState::Claiming),
        "assigned" => Some(WorkItemLifecycleState::Assigned),
        "blocked" => Some(WorkItemLifecycleState::Blocked),
        "done" => Some(WorkItemLifecycleState::Done),
        "cancelled" => Some(WorkItemLifecycleState::Cancelled),
        _ => None,
    }
}

fn parse_work_items_coordination_block(section: &str) -> Result<Vec<WorkItem>> {
    let mut out = Vec::new();
    let lines: Vec<&str> = section.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line_trim = lines[i].trim();
        if line_trim.is_empty() || line_trim == "- none" {
            i += 1;
            continue;
        }
        let Some(hdr) = parse_work_item_header_line_first(line_trim) else {
            i += 1;
            continue;
        };
        i += 1;
        let mut goal = String::new();
        if i < lines.len() {
            let gl = lines[i].trim();
            if let Some(g) = gl.strip_prefix("goal:") {
                goal = g.trim().to_owned();
                i += 1;
            }
        }
        let lifecycle =
            lifecycle_state_from_coordination_token(hdr.lifecycle).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown work item lifecycle in task_plan.md: {:?}",
                    hdr.lifecycle
                )
            })?;

        out.push(WorkItem {
            id: hdr.id.to_owned(),
            title: hdr.title.to_owned(),
            goal,
            stage: hdr.stage.to_owned(),
            lifecycle,
            estimated_token_cost: hdr.estimated_token_cost,
            estimated_time_minutes: hdr.estimated_time_minutes,
        });
    }

    Ok(out)
}

fn task_plan_status_from_frontmatter(raw: &str) -> TaskPlanStatus {
    match raw.trim().to_ascii_lowercase().as_str() {
        "approved" => TaskPlanStatus::Approved,
        "drafted" => TaskPlanStatus::Drafted,
        _ => TaskPlanStatus::AwaitingPlannerInput,
    }
}

#[derive(Debug, Clone)]
struct ParsedTaskPlanBodySkeleton {
    task_id_in_body: String,
    title: String,
    goal: String,
    budget: TaskBudget,
    planning_notes: Vec<String>,
    work_items: Vec<WorkItem>,
}

fn parse_coordination_skeleton_from_task_plan_body(
    raw: &str,
) -> Result<ParsedTaskPlanBodySkeleton> {
    let body = raw.trim_start().replace("\r\n", "\n").replace('\r', "\n");
    let header =
        task_plan_md_section(body.as_str(), "Task").with_context(|| "missing # Task header")?;

    let task_id_in_body = first_task_md_kv(header, "id")
        .map(|value| value.to_owned())
        .unwrap_or_default();
    let title = first_task_md_kv(header, "title")
        .map(|value| value.to_owned())
        .unwrap_or_default();
    let goal = first_task_md_kv(header, "goal")
        .map(|value| value.to_owned())
        .unwrap_or_default();

    let mut budget_line: Option<TaskBudget> = None;
    for line in header.lines() {
        let t = line.trim();
        if t.starts_with("- budget:") {
            budget_line = parse_task_budget_parts(t);
            break;
        }
    }

    let planning_notes_block = task_plan_md_section(body.as_str(), "Planning Notes")
        .unwrap_or("")
        .to_owned();
    let planning_notes = parse_planning_notes_block(if planning_notes_block.is_empty() {
        "- none\n"
    } else {
        &planning_notes_block
    });

    let work_section = task_plan_md_section(body.as_str(), "Work Items")
        .with_context(|| "missing # Work Items header")?;
    let work_items = parse_work_items_coordination_block(work_section)?;

    Ok(ParsedTaskPlanBodySkeleton {
        task_id_in_body,
        title,
        goal,
        budget: budget_line.unwrap_or_default(),
        planning_notes,
        work_items,
    })
}

fn first_planner_work_item_id_for_http_degenerate(plan: &TaskPlan) -> Option<String> {
    plan.work_items.iter().find_map(|wi| {
        if wi.id == HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID || wi.lifecycle.is_terminal() {
            return None;
        }
        Some(wi.id.clone())
    })
}

fn count_open_planner_work_items_for_http_excerpt(plan: &TaskPlan) -> usize {
    plan.work_items
        .iter()
        .filter(|wi| wi.id != HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID && !wi.lifecycle.is_terminal())
        .count()
}

fn append_claim_journals_for_work_item(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
    plan: &TaskPlan,
    work_item_id: &str,
) -> Result<()> {
    let root = workspace_root.as_ref();
    for claim in plan
        .work_item_claims
        .iter()
        .filter(|row| row.work_item_id == work_item_id)
    {
        append_work_item_claim_journal_line(root, namespace, task_id, claim)?;
    }
    Ok(())
}

/// Hydrates **`TaskPlan` / `TaskRequest`** from persisted **`coordination/*/task_plan.md`** body slices
/// created by [`render_task_plan_body`], then overlays claim / assignment replay journals.
///
/// Returns [`None`] when the artifact is unreadable / not `task_plan` / body parse fails /
/// **`task_id` disagrees with the document body id**.
#[must_use]
pub fn load_task_coordination_bundle_for_journal_updates(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
) -> Result<Option<(TaskRequest, TaskPlan)>> {
    let workspace_root_path = workspace_root.as_ref().to_path_buf();
    let store = WorkspaceStore::new(workspace_root_path.clone());
    let rel = task_plan_markdown_relative(task_id);
    let stored: StoredMarkdown<TaskArtifactFrontmatter> =
        match store.read_markdown_in_namespace(namespace, &rel) {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };
    let Some(TaskArtifactKind::TaskPlan) =
        TaskArtifactKind::from_doc_type(&stored.frontmatter.doc_type)
    else {
        return Ok(None);
    };

    let skeleton = match parse_coordination_skeleton_from_task_plan_body(&stored.body) {
        Ok(s) => s,
        Err(error) => {
            tracing::warn!(
                ?error,
                task_id = %task_id,
                "parse task_plan.md skeleton for coordination reload"
            );
            return Ok(None);
        }
    };
    if skeleton.task_id_in_body.trim() != task_id.trim() {
        tracing::warn!(
            persisted_id = %skeleton.task_id_in_body.trim(),
            expected_id = %task_id.trim(),
            "skip task_coordination reload: persisted task_plan body id mismatches coordination key"
        );
        return Ok(None);
    }

    let task_ns = TaskNamespace::new(
        stored.frontmatter.tenant_id.trim(),
        stored.frontmatter.user_id.trim(),
    );
    let mut task = TaskRequest::new(
        skeleton.task_id_in_body.trim(),
        skeleton.title,
        skeleton.goal,
    )
    .with_namespace(task_ns)
    .with_budget(skeleton.budget.clone());

    if task.namespace.tenant_id.is_empty()
        || task.namespace.user_id.is_empty()
        || task.namespace.tenant_id.trim() != namespace.tenant_id
        || task.namespace.user_id.trim() != namespace.user_id
    {
        task.namespace.tenant_id = namespace.tenant_id.clone();
        task.namespace.user_id = namespace.user_id.clone();
    }

    let mut base = TaskPlan::awaiting_planner_input(&task);
    base.status = task_plan_status_from_frontmatter(&stored.frontmatter.status);
    base.planning_notes = skeleton.planning_notes;
    base.work_items = skeleton.work_items;
    hydrate_task_plan_work_item_coordination_journals(
        &workspace_root_path,
        namespace,
        task_id,
        &mut base,
    )?;

    Ok(Some((task, base)))
}

/// Persist claim → deterministic assign; when **exactly one** non-terminal planner work item exists
/// (excluding the HTTP implicit holder), or when **`active_work_item_hint`** names a valid open row,
/// also **synthetic executing + [`TaskPlan::mark_assigned_work_item_done`]** in the same HTTP round
/// (ADR-003 `single_llm_route_agent` degeneracy).
///
/// No-op (`Ok(false)`) unless routing tier is L2/L3, the target work item exists (skips
/// [`HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID`]), and the selected agent id is non-empty.
/// An unknown or terminal **hint** id also yields `Ok(false)` (see `tracing::debug`).
///
/// Failures are returned as [`Err`]; HTTP callers should **warn** and continue.
pub fn persist_http_chat_l23_degenerate_claim_assign(
    workspace_root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    routing_tier: RoutingTier,
    task_id: &str,
    selected_agent_instance_id: &str,
    selected_agent_display_name: &str,
    active_work_item_hint: Option<&str>,
) -> Result<bool> {
    if !matches!(routing_tier, RoutingTier::L2 | RoutingTier::L3) {
        return Ok(false);
    }
    if selected_agent_instance_id.trim().is_empty() {
        return Ok(false);
    }

    let Some((task, mut plan)) =
        load_task_coordination_bundle_for_journal_updates(&workspace_root, namespace, task_id)?
    else {
        return Ok(false);
    };

    let trimmed_hint = active_work_item_hint
        .map(str::trim)
        .filter(|segment| !segment.is_empty());
    let (target_wi, explicit_work_item_scope) = if let Some(hint_id) = trimmed_hint {
        let Some(wi) = plan.work_items.iter().find(|w| w.id == hint_id) else {
            tracing::debug!(
                hint_id = %hint_id,
                task_id = %task.id,
                "HTTP degenerate: active_work_item_hint not found on hydrated task plan"
            );
            return Ok(false);
        };
        if wi.id == HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID || wi.lifecycle.is_terminal() {
            tracing::debug!(
                hint_id = %hint_id,
                lifecycle = %wi.lifecycle,
                "HTTP degenerate: hinted work item is holder or terminal"
            );
            return Ok(false);
        }
        (wi.id.clone(), true)
    } else {
        let Some(first) = first_planner_work_item_id_for_http_degenerate(&plan) else {
            return Ok(false);
        };
        (first, false)
    };

    let open_non_terminal_planner_items = count_open_planner_work_items_for_http_excerpt(&plan);
    debug_assert!(
        open_non_terminal_planner_items >= 1,
        "target planner WI implies at least one open planner item"
    );
    let lone_open_planner_item = explicit_work_item_scope || open_non_terminal_planner_items <= 1;

    let has_active_assignment = plan.work_item_assignments.iter().any(|assignment| {
        assignment.work_item_id == target_wi
            && matches!(assignment.status.as_str(), "assigned" | "executing")
    });
    if has_active_assignment {
        return Ok(false);
    }

    let has_eligible_submitted = plan.work_item_claims.iter().any(|claim| {
        claim.work_item_id == target_wi
            && claim.status == "submitted"
            && claim_capability_eligible_for_p0_assign_v1(claim.score)
    });

    if !has_eligible_submitted {
        let display = if selected_agent_display_name.trim().is_empty() {
            selected_agent_instance_id.to_owned()
        } else {
            selected_agent_display_name.to_owned()
        };
        let claim_id = plan.add_work_item_claim(
            target_wi.clone(),
            selected_agent_instance_id.trim().to_owned(),
            display.trim().to_owned(),
            1.0,
            HTTP_CHAT_L23_DEGENERATE_CLAIM_REASON.to_owned(),
        );

        append_work_item_claim_journal_line(
            workspace_root.as_ref(),
            namespace,
            &task.id,
            plan.work_item_claims
                .iter()
                .find(|c| c.id == claim_id)
                .context("persisted degenerate HTTP claim journal row lookup")?,
        )?;
    }

    let assignment_id = match plan.resolve_work_item_assignment(&target_wi) {
        Some(id) => id,
        None => return Ok(false),
    };

    append_claim_journals_for_work_item(&workspace_root, namespace, &task.id, &plan, &target_wi)?;

    append_work_item_assignment_journal_line(
        workspace_root.as_ref(),
        namespace,
        &task.id,
        plan.work_item_assignments
            .iter()
            .find(|row| row.id == assignment_id)
            .context("assignment row lookup after resolve")?,
    )?;

    if !lone_open_planner_item {
        tracing::debug!(
            task_id = %task.id,
            work_item_id = %target_wi,
            open_planner_work_items = open_non_terminal_planner_items,
            "HTTP chat L2/L3: ambiguous multi-open work items; persisted assign-only (no synthetic MarkDone)"
        );
        persist_task_artifacts(workspace_root.as_ref(), &task, &plan)?;
        return Ok(true);
    }

    plan.start_work_item_execution(&target_wi)
        .with_context(|| {
            format!("http L2/L3 degenerate: start executing after assignment {assignment_id}")
        })?;
    append_work_item_assignment_journal_line(
        workspace_root.as_ref(),
        namespace,
        &task.id,
        plan.work_item_assignments
            .iter()
            .find(|row| row.id == assignment_id)
            .context("assignment journal after synthetic execution start")?,
    )?;

    if !plan.mark_assigned_work_item_done(&target_wi) {
        return Err(anyhow::anyhow!(
            "http L2/L3 degenerate: MarkDone failed for assigned work item {target_wi} after synthetic execution"
        ));
    }

    append_work_item_assignment_journal_line(
        workspace_root.as_ref(),
        namespace,
        &task.id,
        plan.work_item_assignments
            .iter()
            .find(|row| row.id == assignment_id)
            .context("assignment journal after MarkDone (completed)")?,
    )?;

    if let Err(error) = persist_execution_result_artifact_v1(
        workspace_root.as_ref(),
        namespace,
        &task,
        &target_wi,
        format!(
            "HTTP L2/L3 degenerate path: work item completed after synthetic assign/execute (agent {}).",
            selected_agent_display_name.trim()
        ),
        Some(format!(
            "assignment_id={assignment_id}; degenerate_claim_reason={}; selected_agent_instance_id={}",
            HTTP_CHAT_L23_DEGENERATE_CLAIM_REASON,
            selected_agent_instance_id.trim()
        )),
        format!("http_chat:agent:{}", selected_agent_instance_id.trim()),
    ) {
        tracing::warn!(
            ?error,
            task_id = %task.id,
            work_item_id = %target_wi,
            "persist ADR-005 execution_result artifact (non-fatal)"
        );
    }
    if let Err(error) = persist_review_note_artifact_v1(
        workspace_root.as_ref(),
        namespace,
        &task,
        &target_wi,
        format!(
            "HTTP L2/L3 degenerate path: synthetic completion recorded for reviewer trace (agent {}).",
            selected_agent_display_name.trim()
        ),
        Some("synthetic_complete".to_owned()),
        Some(format!(
            "assignment_id={assignment_id}; degenerate_claim_reason={}; selected_agent_instance_id={}",
            HTTP_CHAT_L23_DEGENERATE_CLAIM_REASON,
            selected_agent_instance_id.trim()
        )),
        format!("http_chat:agent:{}", selected_agent_instance_id.trim()),
    ) {
        tracing::warn!(
            ?error,
            task_id = %task.id,
            work_item_id = %target_wi,
            "persist ADR-005 review_note artifact (non-fatal)"
        );
    }

    persist_task_artifacts(workspace_root.as_ref(), &task, &plan)?;

    tracing::debug!(
        task_id = %task.id,
        work_item_id = %target_wi,
        assignment_id = %assignment_id,
        "HTTP chat L2/L3: persisted degenerate claim→execute→done replay"
    );

    Ok(true)
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
            p.starts_with("decisions/") || (p.starts_with("coordination/") && p.ends_with(".md"))
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
