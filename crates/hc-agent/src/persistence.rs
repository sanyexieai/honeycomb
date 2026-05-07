use std::path::{Path, PathBuf};

use anyhow::Result;
use hc_bootstrap::wall_clock_ms;
use hc_capability::CapabilityRepository;
use hc_context::{RoomMemoryWriteRequest, persist_room_memory};
use hc_memory::{MemoryNamespace, MemoryRepository, MemoryVisibility};
use hc_persona::PersonaRepository;
use hc_store::store::{
    MarkdownIndexEntry, MarkdownQuery, StoredMarkdown, WorkspaceNamespace, WorkspaceStore,
};
use serde::{Deserialize, Serialize};

use crate::{
    TaskPlan, TaskRequest,
    bootstrap::MaterializedAgent,
    incubation::{IncubationReport, build_memory_record_from_report},
    planning::TaskPlanStatus,
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

pub fn persist_task_artifacts(
    workspace_root: impl AsRef<Path>,
    task: &TaskRequest,
    plan: &TaskPlan,
) -> Result<PersistedTaskArtifacts> {
    let namespace = WorkspaceNamespace::new(
        task.namespace.tenant_id.clone(),
        task.namespace.user_id.clone(),
    );
    let store = WorkspaceStore::new(workspace_root.as_ref().to_path_buf());
    let timestamp = wall_clock_ms().to_string();
    let task_slug = slugify(&task.id);
    let room_id = task_room_id(task);

    let task_plan_document_id = format!("task-plan.{}", task.id);
    let task_plan_relative_path = PathBuf::from(format!("decisions/{task_slug}.task-plan.md"));
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
        .with_file_name("min.task-plan.summary.md")
        .with_asset_id(format!("asset.{}.task-plan", room_id)),
    )?;

    let mut assignment_paths = Vec::new();
    let mut assignment_memory_paths = Vec::new();
    for assignment in &plan.work_item_assignments {
        let work_item = plan
            .work_items
            .iter()
            .find(|item| item.id == assignment.work_item_id);
        let assignment_slug = slugify(&assignment.id);
        let assignment_relative_path = PathBuf::from(format!(
            "decisions/{task_slug}.{assignment_slug}.assignment.md"
        ));
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
            .with_file_name(format!("min.assignment.{}.md", assignment_slug))
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
    let mut markdown_query = MarkdownQuery::default().with_path_prefix("decisions");
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
                "- {} | stage={} | status={} | {} | {} tokens | {} minutes\n",
                item.id,
                item.stage,
                item.status,
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

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('.') && !slug.ends_with('-') {
            slug.push('.');
        }
    }
    slug.trim_matches(&['.', '-'][..]).to_owned()
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
