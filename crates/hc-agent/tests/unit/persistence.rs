use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use hc_core::{MessageKind, MessageRecord, MessageRoute, RuntimeNamespace, RuntimeSupervisor};
use hc_protocol::swarm::{
    ImplicitIntentDedupeKey, ImplicitIntentDedupeRecord, RoutingTier, WorkItemLifecycleState,
};
use hc_store::{
    store::{WorkspaceNamespace, WorkspaceStore},
    task_coordination::{task_plan_markdown_relative, work_item_claims_journal_relative},
};

use crate::{
    HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID, IncubationObservation, IncubationReport, PromotionDecision,
    SwarmMessageClassification, TaskBootstrapPreset, TaskNamespace, TaskPlan, TaskRequest,
    WorkItem, bootstrap_task_with_preset, format_execution_results_digest_for_http_l23,
    format_plan_notes_digest_for_http_l23, format_review_notes_digest_for_http_l23,
    load_task_coordination_bundle_for_journal_updates, load_task_execution_result_artifacts_v1,
    load_task_plan_note_artifacts_v1, load_task_review_note_artifacts_v1, materialize_plan,
    persist_execution_result_artifact_v1, persist_http_chat_l23_degenerate_claim_assign,
    persist_plan_note_artifact_v1, persist_review_note_artifact_v1, swarm_routing,
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

#[test]
fn execution_result_artifact_v1_persists_json_under_task_execution() {
    let root = unique_temp_dir("exec-artifact");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.exec.art", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let path = persist_execution_result_artifact_v1(
        &root,
        &ns,
        &task,
        "work-item.0001",
        "Synthetic completion",
        Some("extra".into()),
        "test:producer",
    )
    .expect("persist execution_result");
    assert!(path.exists(), "path {path:?}");
    let raw = fs::read_to_string(&path).expect("read asset");
    assert!(raw.contains("execution_result"), "{raw}");
    assert!(raw.contains("artifact_schema_v1"), "{raw}");
    assert!(raw.contains("work-item.0001"), "{raw}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn load_task_execution_result_artifacts_sorted_and_digest_non_empty() {
    let root = unique_temp_dir("exec-load");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.load.exec", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    persist_execution_result_artifact_v1(&root, &ns, &task, "wi-a", "first outcome", None, "p1")
        .expect("persist 1");
    persist_execution_result_artifact_v1(&root, &ns, &task, "wi-b", "second outcome", None, "p2")
        .expect("persist 2");
    let loaded = load_task_execution_result_artifacts_v1(&root, &ns, &task.id).expect("load");
    assert_eq!(loaded.len(), 2);
    assert!(loaded[0].header.created_at_ms >= loaded[1].header.created_at_ms);
    let digest = format_execution_results_digest_for_http_l23(&loaded);
    assert!(
        digest.contains("first outcome") || digest.contains("second outcome"),
        "{digest}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn review_note_artifact_v1_persist_and_load_roundtrip() {
    let root = unique_temp_dir("review-artifact");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.review.art", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    persist_review_note_artifact_v1(
        &root,
        &ns,
        &task,
        "wi-r1",
        "Looks good",
        Some("approve".into()),
        None,
        "reviewer:test",
    )
    .expect("persist review");
    let loaded = load_task_review_note_artifacts_v1(&root, &ns, &task.id).expect("load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].summary, "Looks good");
    assert_eq!(loaded[0].verdict.as_deref(), Some("approve"));
    let digest = format_review_notes_digest_for_http_l23(&loaded);
    assert!(digest.contains("Looks good"), "{digest}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn plan_note_artifact_v1_persist_and_load_roundtrip() {
    let root = unique_temp_dir("plan-artifact");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.plan.art", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    persist_plan_note_artifact_v1(
        &root,
        &ns,
        &task,
        None,
        "Planner adjusted stage ordering",
        Some("Moved QA gate after migration rehearsal".into()),
        "planner:test",
    )
    .expect("persist plan");
    let loaded = load_task_plan_note_artifacts_v1(&root, &ns, &task.id).expect("load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].summary, "Planner adjusted stage ordering");
    assert_eq!(loaded[0].header.work_item_id, None);
    let digest = format_plan_notes_digest_for_http_l23(&loaded);
    assert!(digest.contains("Recent planning notes"), "{digest}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn load_task_plan_note_artifacts_sorted_and_digest_non_empty() {
    let root = unique_temp_dir("plan-load");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.load.plan", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    persist_plan_note_artifact_v1(&root, &ns, &task, None, "first planning note", None, "p1")
        .expect("persist 1");
    persist_plan_note_artifact_v1(
        &root,
        &ns,
        &task,
        Some("wi-b"),
        "second planning note",
        None,
        "p2",
    )
    .expect("persist 2");
    let loaded = load_task_plan_note_artifacts_v1(&root, &ns, &task.id).expect("load");
    assert_eq!(loaded.len(), 2);
    assert!(loaded[0].header.created_at_ms >= loaded[1].header.created_at_ms);
    let digest = format_plan_notes_digest_for_http_l23(&loaded);
    assert!(
        digest.contains("first planning note") || digest.contains("second planning note"),
        "{digest}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn plan_note_digest_includes_truncated_marker_when_first_item_is_too_large() {
    let root = unique_temp_dir("plan-digest-truncated");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.plan.digest.truncated", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let huge = "x".repeat(12_000);
    persist_plan_note_artifact_v1(&root, &ns, &task, None, huge, None, "planner:test")
        .expect("persist huge plan note");
    let loaded = load_task_plan_note_artifacts_v1(&root, &ns, &task.id).expect("load");
    let digest = format_plan_notes_digest_for_http_l23(&loaded);
    assert!(
        digest.contains("truncated; 1 plan_note record(s) on disk"),
        "{digest}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn execution_digest_includes_truncated_marker_when_first_item_is_too_large() {
    let root = unique_temp_dir("execution-digest-truncated");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.execution.digest.truncated", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let huge = "x".repeat(12_000);
    persist_execution_result_artifact_v1(
        &root,
        &ns,
        &task,
        "wi-truncated",
        huge,
        None,
        "worker:test",
    )
    .expect("persist huge execution");
    let loaded = load_task_execution_result_artifacts_v1(&root, &ns, &task.id).expect("load");
    let digest = format_execution_results_digest_for_http_l23(&loaded);
    assert!(
        digest.contains("truncated; 1 execution_result record(s) on disk"),
        "{digest}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn review_digest_includes_truncated_marker_when_first_item_is_too_large() {
    let root = unique_temp_dir("review-digest-truncated");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.review.digest.truncated", "T", "G")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let huge = "x".repeat(12_000);
    persist_review_note_artifact_v1(
        &root,
        &ns,
        &task,
        "wi-truncated",
        huge,
        Some("needs_revision".into()),
        None,
        "reviewer:test",
    )
    .expect("persist huge review");
    let loaded = load_task_review_note_artifacts_v1(&root, &ns, &task.id).expect("load");
    let digest = format_review_notes_digest_for_http_l23(&loaded);
    assert!(
        digest.contains("truncated; 1 review_note record(s) on disk"),
        "{digest}"
    );
    let _ = fs::remove_dir_all(root);
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
    let plan = bootstrap_task_with_preset(&task, TaskBootstrapPreset::ThreeRolesDemo);
    let mut runtime = RuntimeSupervisor::new();
    let session =
        runtime.create_session_in_namespace("demo", RuntimeNamespace::new("tenant-a", "user-a"));

    let outcome = materialize_plan(&mut runtime, &session.id, &plan)
        .context("plan should materialize")
        .expect("materialization should succeed");
    let agents = outcome.agents;

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
fn task_artifacts_can_be_persisted_under_coordination_subdirectory() {
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
            .contains("coordination\\task.demo\\")
    );
    assert!(
        persisted
            .task_plan_memory_path
            .to_string_lossy()
            .replace('\\', "/")
            .contains("compressed/task/plan/task-plan.summary.md")
    );

    let task_plan_content =
        fs::read_to_string(&persisted.task_plan_path).expect("task plan file should be readable");
    assert!(task_plan_content.contains("type: task_plan"));
    assert!(task_plan_content.contains("Inspect runtime"));
    assert!(task_plan_content.contains("# Work Item Claims"));

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

    assert!(
        persisted.assignment_memory_paths[0]
            .to_string_lossy()
            .replace('\\', "/")
            .contains("compressed/task/plan/assignment-decision.work.assignment.0001.md")
    );

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

#[test]
fn routing_binding_coordination_append_writes_one_json_line() {
    let root = unique_temp_dir("routing-binding-coord");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let message = MessageRecord {
        id: "msg.1".into(),
        session_id: "session.1".into(),
        from: "instance.1".into(),
        route: MessageRoute::Broadcast,
        kind: MessageKind::Chat,
        body: "plan this with steps".into(),
        reply_to: None,
    };
    let routing = swarm_routing::decide_routing_tier(&message.body);
    let task_binding = swarm_routing::decide_task_binding(&routing, None, Some("task.demo"));
    let swarm = SwarmMessageClassification {
        routing,
        task_binding,
    };
    let line = build_routing_binding_log_line_v1(4242, &message, "task.demo", &swarm);
    let written = append_routing_binding_log_line(&root, &namespace, "task.demo", &line)
        .expect("routing binding append should succeed");

    let store = WorkspaceStore::new(&root);
    let expected = store.resolve_in_namespace(&namespace, "coordination/task.demo.routing.jsonl");
    assert_eq!(written, expected);
    assert!(written.exists(), "expected {:?}", written);
    let text = fs::read_to_string(&written).expect("log readable");
    let first = text.lines().next().expect("one line");
    let parsed: RoutingBindingLogLineV1 = serde_json::from_str(first).expect("valid json");
    assert_eq!(parsed.schema, ROUTING_BINDING_LOG_SCHEMA_V1);
    assert_eq!(parsed.message_id, "msg.1");
    assert_eq!(parsed.task_scope_id, "task.demo");
    assert_eq!(parsed.conversation_id.as_deref(), Some("session.1"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn routing_binding_headless_matches_message_record_line_for_same_body() {
    let message = MessageRecord {
        id: "msg.1".into(),
        session_id: "session.1".into(),
        from: "instance.1".into(),
        route: MessageRoute::Broadcast,
        kind: MessageKind::Chat,
        body: "plan this with steps".into(),
        reply_to: None,
    };
    let routing = swarm_routing::decide_routing_tier(&message.body);
    let task_binding = swarm_routing::decide_task_binding(&routing, None, Some("task.demo"));
    let swarm = SwarmMessageClassification {
        routing: routing.clone(),
        task_binding: task_binding.clone(),
    };
    let from_record = build_routing_binding_log_line_v1(4242, &message, "task.demo", &swarm);
    let headless = build_routing_binding_log_line_v1_headless(
        4242,
        "msg.1",
        "session.1",
        "task.demo",
        &message.body,
        routing,
        task_binding,
    );
    assert_eq!(
        from_record.intent_fingerprint_hex,
        headless.intent_fingerprint_hex
    );
    assert_eq!(from_record.routing, headless.routing);
    assert_eq!(from_record.task_binding, headless.task_binding);

    let from_snap = build_routing_binding_log_line_v1_headless_from_snapshot(
        4242,
        "msg.1",
        "session.1",
        "task.demo",
        &message.body,
        swarm.routing_binding_snapshot(),
    );
    assert_eq!(from_record, from_snap);

    assert_eq!(
        from_record.routing_binding_snapshot(),
        from_snap.routing_binding_snapshot()
    );
}

#[test]
fn implicit_intent_dedupe_load_roundtrip() {
    let root = unique_temp_dir("implicit-intent-dedupe");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let key = ImplicitIntentDedupeKey::from_trigger("conv.1", "msg.1", "plan this with steps");
    let rec = ImplicitIntentDedupeRecord::from_key(&key, 99);
    append_implicit_intent_dedupe_record(&root, &namespace, "task.demo", &rec)
        .expect("append implicit intent record");
    let loaded = load_implicit_intent_dedupe_keys(&root, &namespace, "task.demo")
        .expect("load implicit intent keys");
    assert!(loaded.contains(&key));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn implicit_intent_journal_duplicate_appends_collapse_to_one_logical_key() {
    let root = unique_temp_dir("implicit-intent-dedupe-dup");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let key = ImplicitIntentDedupeKey::from_trigger("conv.1", "msg.1", "plan this with steps");
    let rec = ImplicitIntentDedupeRecord::from_key(&key, 99);
    append_implicit_intent_dedupe_record(&root, &namespace, "task.demo", &rec)
        .expect("first append");
    append_implicit_intent_dedupe_record(&root, &namespace, "task.demo", &rec)
        .expect("duplicate append (replay / double dispatch)");
    let loaded = load_implicit_intent_dedupe_keys(&root, &namespace, "task.demo")
        .expect("load implicit intent keys");
    assert_eq!(
        loaded.len(),
        1,
        "ADR-003 duplicate key must not appear twice in the dedupe set consumed by orchestration"
    );
    assert!(loaded.contains(&key));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn work_item_coordination_journals_roundtrip_via_hydrate() {
    let root = unique_temp_dir("wi-coord-journals");
    let namespace = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new(
        "task.journal.demo",
        "Journal Demo",
        "replay claims and assignments from jsonl journals",
    )
    .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let work_item_id = plan.add_work_item(
        "phase-1",
        "Inspect pipelines",
        "Confirm replay restores submitted and resolved claims",
    );
    let claim_id = plan.add_work_item_claim(
        &work_item_id,
        "instance.0101",
        "planner",
        0.91,
        "Strong fit",
    );

    append_work_item_claim_journal_line(
        &root,
        &namespace,
        &task.id,
        plan.work_item_claims
            .iter()
            .find(|row| row.id == claim_id)
            .expect("claim exists"),
    )
    .expect("append claim journal line");

    let assignment_id = plan
        .resolve_work_item_assignment(&work_item_id)
        .expect("assignment should resolve");
    for claim in plan
        .work_item_claims
        .iter()
        .filter(|row| row.work_item_id == work_item_id)
    {
        append_work_item_claim_journal_line(&root, &namespace, &task.id, claim)
            .expect("append post-resolve claim snapshot");
    }
    append_work_item_assignment_journal_line(
        &root,
        &namespace,
        &task.id,
        plan.work_item_assignments
            .iter()
            .find(|row| row.id == assignment_id)
            .expect("assignment exists"),
    )
    .expect("append assignment journal line");

    let mut replay_plan = TaskPlan::awaiting_planner_input(&task);
    replay_plan.work_items = plan.work_items.clone();
    hydrate_task_plan_work_item_coordination_journals(
        &root,
        &namespace,
        &task.id,
        &mut replay_plan,
    )
    .expect("hydrate journals into task plan");

    assert_eq!(
        replay_plan.work_item_claims, plan.work_item_claims,
        "claims hydrated from journals should equal committed plan snapshot"
    );
    assert_eq!(
        replay_plan.work_item_assignments,
        plan.work_item_assignments
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn http_implicit_task_plan_stub_idempotent_when_body_nonempty() {
    let root = unique_temp_dir("http-implicit-stub-idem");
    let namespace = hc_store::store::WorkspaceNamespace::new("tenant-a", "user-a");
    let task_id = "task.http.implicit.777001";
    ensure_http_implicit_task_plan_stub(
        &root,
        &namespace,
        task_id,
        "first goal line for stub",
        "test.routing.msg.1",
        "test.session.1",
    )
    .expect("first stub write");
    let first = read_task_artifact(&root, &namespace, task_plan_markdown_relative(task_id))
        .expect("read task_plan after stub");
    assert!(
        first.body.contains("first goal line for stub"),
        "awaiting_planner_input render should embed the triggering goal"
    );
    assert!(
        first.body.contains("routing_message_id=test.routing.msg.1"),
        "stub should record routing_message_id in planning notes"
    );
    assert!(
        first.body.contains("session_id=test.session.1"),
        "stub should record session_id in planning notes"
    );
    assert!(
        first.body.contains(HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID),
        "stub should persist the implicit Planned work-item placeholder"
    );

    ensure_http_implicit_task_plan_stub(
        &root,
        &namespace,
        task_id,
        "second call must not replace an existing non-empty plan",
        "ignored.msg",
        "ignored.session",
    )
    .expect("second call is a no-op");
    let second = read_task_artifact(&root, &namespace, task_plan_markdown_relative(task_id))
        .expect("read task_plan after idempotent call");
    assert_eq!(
        second.body, first.body,
        "ensure_http_implicit_task_plan_stub must not clobber materialized task_plan.md"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn persist_task_plan_drops_http_implicit_holder_when_planner_adds_work_items() {
    let root = unique_temp_dir("implicit-holder-prune-persist");
    let task = TaskRequest::new("task.imp.prpersist", "Prpersist", "user goal snippet")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "HTTP implicit intent holder".to_owned(),
        goal: "seed".to_owned(),
        stage: "implicit".to_owned(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });
    let _wi = plan.add_work_item(
        "phase-a",
        "Real coordination item",
        "Replaces implicit HTTP placeholder on persist",
    );

    let persisted = persist_task_artifacts(&root, &task, &plan).expect("persist task_plan");
    let md_raw = fs::read_to_string(&persisted.task_plan_path).expect("read task_plan.md");
    assert!(
        !md_raw.contains(HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID),
        "persist strips implicit holder once planner work items coexist in the same TaskPlan",
    );

    assert!(
        md_raw.contains("Real coordination item"),
        "real planner work item title should appear in rendered body"
    );
    assert!(
        md_raw.contains("work-item.0001"),
        "first planner work item keeps work-item.0001 id when implicit holder was pre-seeded"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn persist_task_artifacts_with_in_memory_prune_updates_live_plan() {
    let root = unique_temp_dir("implicit-inmem-prune");
    let task = TaskRequest::new("task.imp.sync", "Sync", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));

    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "holder".into(),
        goal: "h".into(),
        stage: "implicit".into(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });
    let wi = plan.add_work_item("p", "real", "r");
    assert_eq!(wi, "work-item.0001");

    persist_task_artifacts_with_in_memory_prune(&root, &task, &mut plan).expect("persist+sync");

    assert!(
        plan.work_items
            .iter()
            .all(|w| w.id != HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID),
        "mutable plan should drop implicit holder after wrapper persist when other work items exist"
    );
    assert_eq!(plan.work_items.len(), 1);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn load_task_coordination_bundle_round_trips_simple_task_plan() {
    let root = unique_temp_dir("coord-bundle-parse");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.coord.bundle.parse", "T", "Goal text")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.planning_notes = vec!["note one".into(), "note two".into()];
    let _wi = plan.add_work_item("p1", "Real item title", "Real item goal");

    persist_task_artifacts(&root, &task, &plan).expect("persist");

    let (_, p2) = load_task_coordination_bundle_for_journal_updates(&root, &ns, &task.id)
        .expect("bundle load io")
        .expect("parsed bundle exists");

    assert_eq!(
        p2.planning_notes,
        vec!["note one".to_owned(), "note two".to_owned()]
    );
    assert!(
        !p2.work_items
            .iter()
            .any(|w| w.id == HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID),
        "round-trip excludes implicit holders unless authored"
    );
    assert!(
        p2.work_items.iter().any(|w| w.id.starts_with("work-item.")),
        "expected persisted planner-owned work items"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn http_l23_claim_assign_false_when_only_implicit_holder_present() {
    let root = unique_temp_dir("http-claim-assign-skip-holder");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.imp.only-holder", "H", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.work_items.push(WorkItem {
        id: HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID.to_owned(),
        title: "holder".into(),
        goal: "g".into(),
        stage: "implicit".into(),
        lifecycle: WorkItemLifecycleState::Planned,
        estimated_token_cost: 0,
        estimated_time_minutes: 0,
    });

    persist_task_artifacts(&root, &task, &plan).expect("persist");

    let applied = persist_http_chat_l23_degenerate_claim_assign(
        &root,
        &ns,
        RoutingTier::L2,
        &task.id,
        "agent.planner",
        "Planner",
        None,
    )
    .expect("call succeeds");
    assert!(
        !applied,
        "implicit holder-only plan should skip degenerate coordination"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn http_l23_claim_assign_persists_claim_then_assignment_journal() {
    let root = unique_temp_dir("http-degen-assign");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.coord.http.degen", "Title", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.add_work_item("p", "Planner wi", "do work");

    persist_task_artifacts(&root, &task, &plan).expect("persist");

    assert!(
        persist_http_chat_l23_degenerate_claim_assign(
            &root,
            &ns,
            RoutingTier::L3,
            &task.id,
            "instance.route.agent",
            "Router Agent",
            None,
        )
        .expect("first degenerate coordination pass")
    );

    let store = WorkspaceStore::new(root.clone());
    let claims_path = store.resolve_in_namespace(&ns, work_item_claims_journal_relative(&task.id));
    let claims_raw = fs::read_to_string(&claims_path).expect("journal read");
    assert!(claims_raw.contains("work_item_claim_journal_v1"));
    assert!(
        !persist_http_chat_l23_degenerate_claim_assign(
            &root,
            &ns,
            RoutingTier::L2,
            &task.id,
            "instance.route.agent",
            "Router Agent",
            None,
        )
        .expect("second degenerate coordination pass")
    );

    let md_path = store.resolve_in_namespace(&ns, task_plan_markdown_relative(&task.id));
    let md = fs::read_to_string(&md_path).expect("read task plan");
    assert!(md.contains("# Assignments"));
    assert!(md.contains("work-assignment."));
    assert!(
        md.contains("lifecycle=done"),
        "single HTTP round should close the degenerate work item lifecycle"
    );
    assert!(
        md.contains("status=completed"),
        "assignment row should show completed after synthetic execution + MarkDone"
    );
    let execution_artifacts =
        load_task_execution_result_artifacts_v1(&root, &ns, &task.id).expect("load execution");
    assert_eq!(
        execution_artifacts.len(),
        1,
        "degenerate MarkDone path should write one execution_result artifact"
    );
    let review_artifacts =
        load_task_review_note_artifacts_v1(&root, &ns, &task.id).expect("load review");
    assert_eq!(
        review_artifacts.len(),
        1,
        "degenerate MarkDone path should also write one review_note artifact"
    );
    assert_eq!(
        review_artifacts[0].verdict.as_deref(),
        Some("synthetic_complete")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn http_l23_multiple_open_work_items_assign_only_without_mark_done() {
    let root = unique_temp_dir("http-degen-multi-wi");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.coord.http.multi", "T", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    plan.add_work_item("p1", "First parallel wi", "a");
    plan.add_work_item("p2", "Second parallel wi", "b");

    persist_task_artifacts(&root, &task, &plan).expect("persist");

    assert!(
        persist_http_chat_l23_degenerate_claim_assign(
            &root,
            &ns,
            RoutingTier::L3,
            &task.id,
            "instance.route.agent",
            "Router Agent",
            None,
        )
        .expect("degenerate coordination applies assign to first WI only")
    );

    let md_path = WorkspaceStore::new(root.clone())
        .resolve_in_namespace(&ns, task_plan_markdown_relative(&task.id));
    let md = fs::read_to_string(&md_path).expect("read task plan");

    assert!(
        !md.contains("lifecycle=done"),
        "with >1 open planner WI we must not infer which row the single LLM turn completed"
    );
    assert!(
        md.contains("lifecycle=assigned"),
        "first-selected WI stays assigned awaiting explicit orchestration/UI"
    );
    assert!(
        md.contains("| status=assigned |"),
        "assignment markdown should reflect assign-only degenerate snapshot"
    );
    assert!(
        !md.contains("status=completed"),
        "assign-only path must not emit completed assignment snapshots"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn http_l23_multiple_open_work_items_active_hint_marks_hinted_done() {
    let root = unique_temp_dir("http-degen-multi-wi-hint");
    let ns = WorkspaceNamespace::new("tenant-a", "user-a");
    let task = TaskRequest::new("task.coord.http.multi.hint", "T", "g")
        .with_namespace(TaskNamespace::new("tenant-a", "user-a"));
    let mut plan = TaskPlan::awaiting_planner_input(&task);
    let wi1 = plan.add_work_item("p1", "First parallel wi", "a");
    let wi2 = plan.add_work_item("p2", "Second parallel wi", "b");

    persist_task_artifacts(&root, &task, &plan).expect("persist");

    assert!(
        persist_http_chat_l23_degenerate_claim_assign(
            &root,
            &ns,
            RoutingTier::L3,
            &task.id,
            "instance.route.agent",
            "Router Agent",
            Some(wi2.as_str()),
        )
        .expect("degenerate coordination with explicit second-WI hint")
    );

    let md_path = WorkspaceStore::new(root.clone())
        .resolve_in_namespace(&ns, task_plan_markdown_relative(&task.id));
    let md = fs::read_to_string(&md_path).expect("read task plan");

    assert!(
        md.contains(&format!("- {wi2} | stage=p2 | lifecycle=done")),
        "hinted WI should reach done after synthetic execute + MarkDone"
    );
    assert!(
        md.contains(&format!("- {wi1} | stage=p1 | lifecycle=planned")),
        "non-hinted parallel WI must stay planned (no spurious assign)"
    );
    assert!(
        md.contains("status=completed"),
        "hinted row assignment should complete"
    );

    let _ = fs::remove_dir_all(root);
}
