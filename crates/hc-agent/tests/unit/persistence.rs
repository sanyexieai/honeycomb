use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use hc_core::{RuntimeNamespace, RuntimeSupervisor};

use crate::{
    IncubationObservation, IncubationReport, PromotionDecision, TaskNamespace, TaskPlan,
    TaskRequest, bootstrap_task, materialize_plan,
};

use super::*;

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "honeycomb-{}-{}-{}",
        name,
        std::process::id(),
        nanos
    ))
}

fn sidecar_path(path: &std::path::Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("path should have a utf-8 file name");
    path.with_file_name(format!("{}.meta.json", file_name.trim_end_matches(".md")))
}

#[test]
fn materialized_agents_can_be_persisted_to_workspace() {
    let root = unique_temp_dir("agent-persist");
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let plan = bootstrap_task(&task);
    let mut runtime = RuntimeSupervisor::new();
    let session =
        runtime.create_session_in_namespace("demo", RuntimeNamespace::new("tenant-a", "user-a"));

    let agents = materialize_plan(&mut runtime, &session.id, &plan)
        .context("plan should materialize")
        .expect("materialization should succeed");

    let persisted = persist_materialized_agents(&root, &agents)
        .context("agents should persist")
        .expect("persistence should succeed");

    assert_eq!(persisted.len(), agents.len());
    assert!(persisted[0].persona_path.exists());
    assert_eq!(persisted[0].capability_paths.len(), 1);
    assert!(persisted[0].capability_paths[0].exists());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn incubation_reports_can_be_persisted_to_memory_workspace() {
    let root = unique_temp_dir("incubation-persist");
    let report = IncubationReport {
        task_id: "task.demo".to_owned(),
        instance_id: "instance.0001".to_owned(),
        observations: vec![IncubationObservation {
            kind: "strength".to_owned(),
            detail: "handled review well".to_owned(),
        }],
        promotion: PromotionDecision::ContinueIncubating,
    };

    let persisted = persist_incubation_report(
        &root,
        WorkspaceNamespace::new("tenant-a", "user-a"),
        &report,
    )
    .context("incubation report should persist")
    .expect("memory persistence should succeed");

    assert!(persisted.memory_path.exists());
    assert!(
        persisted
            .memory_path
            .to_string_lossy()
            .replace('/', "\\")
            .contains("tenants\\tenant-a\\users\\user-a\\memory\\task")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn task_artifacts_can_be_persisted_to_decision_workspace() {
    let root = unique_temp_dir("task-artifacts");
    let task = TaskRequest::new("task.demo", "Demo Task", "Build a demo")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item(
        "phase-1",
        "Inspect runtime",
        "Understand runtime and storage boundaries",
    );
    plan.add_work_item_claim(
        &work_item_id,
        "instance.0002",
        "reviewer",
        0.92,
        "Best fit for reviewing runtime changes",
    );
    plan.resolve_work_item_assignment(&work_item_id)
        .expect("assignment should resolve");
    plan.approve();

    let persisted = persist_task_artifacts(&root, &task, &plan)
        .context("task artifacts should persist")
        .expect("task persistence should succeed");

    assert!(persisted.task_plan_path.exists());
    assert_eq!(persisted.assignment_paths.len(), 1);
    assert!(persisted.assignment_paths[0].exists());
    assert!(persisted.task_plan_memory_path.exists());
    assert_eq!(persisted.assignment_memory_paths.len(), 1);
    assert!(persisted.assignment_memory_paths[0].exists());
    assert!(
        persisted
            .task_plan_path
            .to_string_lossy()
            .replace('/', "\\")
            .contains("tenants\\tenant-a\\users\\user-a\\decisions\\")
    );
    assert!(persisted
        .task_plan_memory_path
        .to_string_lossy()
        .replace('/', "\\")
        .contains("tenants\\tenant-a\\users\\user-a\\memory\\rooms\\task\\room.task.task.demo\\compressed\\"));

    let task_plan_content =
        fs::read_to_string(&persisted.task_plan_path).expect("task plan file should be readable");
    assert!(task_plan_content.contains("type: task_plan"));
    assert!(task_plan_content.contains("Inspect runtime"));

    let assignment_content = fs::read_to_string(&persisted.assignment_paths[0])
        .expect("assignment file should be readable");
    assert!(assignment_content.contains("type: assignment_decision"));
    assert!(assignment_content.contains("reviewer"));

    let task_plan_memory_content = fs::read_to_string(&persisted.task_plan_memory_path)
        .expect("task plan room memory should be readable");
    assert!(task_plan_memory_content.contains("Task task.demo is approved"));
    let task_plan_memory_meta = fs::read_to_string(sidecar_path(&persisted.task_plan_memory_path))
        .expect("task plan room memory sidecar should be readable");
    assert!(task_plan_memory_meta.contains(r#""type": "memory_room_asset""#));
    assert!(task_plan_memory_meta.contains(r#""memory_kind": "summary""#));

    let assignment_memory_content = fs::read_to_string(&persisted.assignment_memory_paths[0])
        .expect("assignment room memory should be readable");
    assert!(assignment_memory_content.contains("assigned work item"));
    let assignment_memory_meta =
        fs::read_to_string(sidecar_path(&persisted.assignment_memory_paths[0]))
            .expect("assignment room memory sidecar should be readable");
    assert!(assignment_memory_meta.contains(r#""type": "memory_room_asset""#));
    assert!(assignment_memory_meta.contains(r#""memory_kind": "decision""#));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn task_artifacts_can_be_rebuilt_queried_and_read() {
    let root = unique_temp_dir("task-artifact-query");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new(
        "task.demo.query",
        "Query Demo",
        "Persist and inspect task assets",
    )
    .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item(
        "phase-1",
        "Inspect decisions",
        "Confirm decision artifacts can be queried",
    );
    plan.add_work_item_claim(
        &work_item_id,
        "instance.0003",
        "planner",
        0.88,
        "Planner owns the initial coordination work",
    );
    plan.resolve_work_item_assignment(&work_item_id)
        .expect("assignment should resolve");
    plan.approve();

    let persisted = persist_task_artifacts(&root, &task, &plan)
        .context("task artifacts should persist")
        .expect("task persistence should succeed");

    let rebuilt = rebuild_task_artifact_index(&root, &namespace)
        .context("task index should rebuild")
        .expect("task index should rebuild");
    assert_eq!(rebuilt.len(), 2);
    assert!(
        rebuilt
            .iter()
            .any(|artifact| artifact.kind == TaskArtifactKind::TaskPlan)
    );
    assert!(
        rebuilt
            .iter()
            .any(|artifact| artifact.kind == TaskArtifactKind::AssignmentDecision)
    );

    let assignment_matches = query_task_artifacts(
        &root,
        &namespace,
        &TaskArtifactQuery::default()
            .for_task("task.demo.query")
            .with_kind(TaskArtifactKind::AssignmentDecision)
            .with_status("assigned"),
    )
    .context("assignment artifacts should query")
    .expect("assignment query should succeed");
    assert_eq!(assignment_matches.len(), 1);
    assert_eq!(
        assignment_matches[0].task_hint.as_deref(),
        Some("task.demo.query")
    );

    let relative_path = persisted.assignment_paths[0]
        .strip_prefix(WorkspaceStore::new(&root).resolve(namespace.scoped_prefix()))
        .expect("assignment path should stay inside namespace root")
        .to_path_buf();
    let document = read_task_artifact(&root, &namespace, &relative_path)
        .context("task artifact should read")
        .expect("task artifact should read");
    assert_eq!(document.kind, TaskArtifactKind::AssignmentDecision);
    assert_eq!(
        document.relative_path,
        relative_path.to_string_lossy().replace('\\', "/")
    );
    assert!(document.body.contains("Inspect decisions"));
    assert!(persisted.task_plan_memory_path.exists());
    assert_eq!(persisted.assignment_memory_paths.len(), 1);

    let _ = fs::remove_dir_all(root);
}
