use anyhow::{Context, Result, bail};
use hc_context::runtime::{RuntimeIdentity, RuntimeVariables};
use hc_conversation::{ConversationEvent, ConversationRepository};
use hc_protocol::ApiNamespace;
use hc_scheduler::{
    ScheduleRepository, ScheduleStatus, ScheduledRun, ScheduledRunStatus, ScheduledTargetKind,
    ScheduledTask, now_unix,
};
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::{
    CommandToolExecutor, McpServerRepository, ToolExecutor, ToolRepository,
    build_default_tool_execution_plan, builtin_tool, call_mcp_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::process::Command;
use tracing::warn;

use crate::ServiceConfig;
use crate::conversation::process_conversation_inbox;

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerDispatchReport {
    pub now_unix: u64,
    pub queued_count: usize,
    pub receipts: Vec<SchedulerDispatchReceipt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerDispatchReceipt {
    pub run_id: String,
    pub schedule_id: String,
    pub target_kind: String,
    pub target_ref: String,
    pub status: String,
    pub result_ref: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub schedule: ScheduledTask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleStatusRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub schedule_id: String,
    pub status: ScheduleStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchedulerRunRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub now_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleWriteResponse {
    pub schedule: ScheduledTask,
    pub path: String,
}

pub fn list_schedules(
    config: &ServiceConfig,
    namespace: ApiNamespace,
) -> Result<Vec<ScheduledTask>> {
    let repository = schedule_repository(config, namespace);
    repository.list_schedules()
}

pub fn write_schedule(
    config: &ServiceConfig,
    request: ScheduleRequest,
) -> Result<ScheduleWriteResponse> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let repository = schedule_repository(config, namespace);
    let path = repository.write_schedule(&request.schedule)?;
    Ok(ScheduleWriteResponse {
        schedule: request.schedule,
        path: path.to_string_lossy().replace('\\', "/"),
    })
}

pub fn set_schedule_status(
    config: &ServiceConfig,
    request: ScheduleStatusRequest,
) -> Result<ScheduledTask> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let repository = schedule_repository(config, namespace);
    repository.set_schedule_status(&request.schedule_id, request.status)
}

pub fn list_scheduled_runs(
    config: &ServiceConfig,
    namespace: ApiNamespace,
) -> Result<Vec<ScheduledRun>> {
    let repository = schedule_repository(config, namespace);
    repository.list_runs()
}

pub fn queue_due_scheduled_runs(
    config: &ServiceConfig,
    request: SchedulerRunRequest,
) -> Result<Vec<ScheduledRun>> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let repository = schedule_repository(config, namespace);
    repository.queue_due_runs(request.now_unix.unwrap_or_else(now_unix))
}

pub fn dispatch_due_scheduled_runs(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    now: Option<u64>,
) -> Result<SchedulerDispatchReport> {
    let now = now.unwrap_or_else(now_unix);
    let workspace_namespace = workspace_namespace(namespace);
    let repository = ScheduleRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let queued = repository.queue_due_runs(now)?;
    let receipts = dispatch_queued_runs(config, &workspace_namespace, &repository, now)?;
    Ok(SchedulerDispatchReport {
        now_unix: now,
        queued_count: queued.len(),
        receipts,
    })
}

pub fn dispatch_queued_scheduled_runs(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    now: Option<u64>,
) -> Result<SchedulerDispatchReport> {
    let now = now.unwrap_or_else(now_unix);
    let workspace_namespace = workspace_namespace(namespace);
    let repository = ScheduleRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let receipts = dispatch_queued_runs(config, &workspace_namespace, &repository, now)?;
    Ok(SchedulerDispatchReport {
        now_unix: now,
        queued_count: 0,
        receipts,
    })
}

fn dispatch_queued_runs(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    repository: &ScheduleRepository,
    now: u64,
) -> Result<Vec<SchedulerDispatchReceipt>> {
    let mut receipts = Vec::new();
    for run in repository.queued_runs()? {
        receipts.push(dispatch_scheduled_run(
            config, namespace, repository, run, now,
        )?);
    }
    Ok(receipts)
}

/// Dispatches a single scheduled run (typically one already in `queued` state).
pub fn dispatch_scheduled_run(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    repository: &ScheduleRepository,
    mut run: ScheduledRun,
    now: u64,
) -> Result<SchedulerDispatchReceipt> {
    run.status = ScheduledRunStatus::Running;
    run.started_at_unix = Some(now);
    repository.write_run(&run)?;

    let result = match run.target.kind {
        ScheduledTargetKind::Mcp => dispatch_scheduled_mcp_run(config, namespace, &run),
        ScheduledTargetKind::Command => dispatch_scheduled_command_run(config, &run),
        ScheduledTargetKind::Tool => dispatch_scheduled_tool_run(config, namespace, &run),
        ScheduledTargetKind::Agent => dispatch_scheduled_agent_run(config, namespace, &run),
        ScheduledTargetKind::Event => dispatch_scheduled_event_run(config, namespace, &run),
    };

    let finished_at = now_unix();
    match result {
        Ok(result_ref) => {
            run.status = ScheduledRunStatus::Succeeded;
            run.finished_at_unix = Some(finished_at);
            run.result_ref = Some(result_ref.clone());
            run.error = None;
            repository.write_run(&run)?;
            if run.target.kind == ScheduledTargetKind::Agent {
                maybe_process_inbox_after_agent_schedule(config, namespace);
            }
            Ok(receipt(&run, "succeeded", Some(result_ref), None))
        }
        Err(error) => {
            run.status = ScheduledRunStatus::Failed;
            run.finished_at_unix = Some(finished_at);
            run.error = Some(error.to_string());
            repository.write_run(&run)?;
            Ok(receipt(&run, "failed", None, Some(error.to_string())))
        }
    }
}

fn dispatch_scheduled_mcp_run(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    run: &ScheduledRun,
) -> Result<String> {
    let tool_name = run
        .target
        .action
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("mcp scheduled target requires target.action")?;
    let repository =
        McpServerRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let server = repository.get_server(&run.target.r#ref)?;
    let mut arguments = serde_json::Map::new();
    for (key, value) in &server.default_args {
        arguments.insert(key.clone(), value.clone());
    }
    for (key, value) in &run.target.args {
        arguments.insert(key.clone(), value.clone());
    }
    let runtime = RuntimeVariables::new(RuntimeIdentity::from_optional(
        Some(namespace.tenant_id.clone()),
        Some(namespace.user_id.clone()),
        Some(run.schedule_id.clone()),
    ));
    runtime.inject_mcp_arguments(&mut arguments);
    let result = call_mcp_tool(&server, tool_name, serde_json::Value::Object(arguments))?;
    if result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        bail!("scheduled mcp call returned an error: {}", result);
    }
    Ok(format!("mcp:{}:{}", run.target.r#ref, tool_name))
}

fn dispatch_scheduled_command_run(config: &ServiceConfig, run: &ScheduledRun) -> Result<String> {
    let program = run.target.r#ref.trim();
    if program.is_empty() {
        bail!("scheduled command target requires non-empty ref (executable name or path)");
    }
    let mut cmd = Command::new(program);
    if let Some(action) = run
        .target
        .action
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        cmd.arg(action);
    }
    if let Some(Value::Array(argv)) = run.target.args.get("argv") {
        for item in argv {
            if let Some(s) = item.as_str() {
                cmd.arg(s);
            } else if let Some(n) = item.as_i64() {
                cmd.arg(n.to_string());
            } else if let Some(n) = item.as_u64() {
                cmd.arg(n.to_string());
            } else if let Some(n) = item.as_f64() {
                cmd.arg(n.to_string());
            }
        }
    }
    cmd.current_dir(&config.workspace_root);
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn scheduled command: {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "scheduled command {program:?} exited with {:?}: {}",
            output.status.code(),
            stderr.trim()
        );
    }
    Ok(format!("command:{program}"))
}

fn dispatch_scheduled_tool_run(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    run: &ScheduledRun,
) -> Result<String> {
    let tool_id = run.target.r#ref.trim();
    if tool_id.is_empty() {
        bail!("scheduled tool target requires non-empty ref (tool id)");
    }
    let repo = ToolRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let catalog = repo.load_catalog()?;
    let tool = if let Some(spec) = catalog.get(tool_id) {
        spec.clone()
    } else if let Some(spec) = builtin_tool(tool_id) {
        spec
    } else {
        bail!("scheduled tool not found: {tool_id}");
    };
    let goal = run
        .target
        .action
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "scheduled tool run".to_owned());
    let plan = build_default_tool_execution_plan(&tool, &goal)?;
    let executor = CommandToolExecutor::default().with_working_dir(config.workspace_root.clone());
    let outcome = executor.execute(&plan, &goal)?;
    if !outcome.success {
        let detail = outcome.observations.join("; ");
        bail!(
            "scheduled tool {} failed: {} ({})",
            tool.id,
            outcome.summary,
            detail
        );
    }
    Ok(format!("tool:{}", tool.id))
}

fn dispatch_scheduled_agent_run(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    run: &ScheduledRun,
) -> Result<String> {
    let agent_ref = run.target.r#ref.trim();
    if agent_ref.is_empty() {
        bail!("scheduled agent target requires non-empty ref (agent id)");
    }
    let repository =
        ConversationRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let kind = run
        .target
        .action
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "scheduled.agent_dispatch".to_owned());
    let mut event = ConversationEvent::new(&kind);
    event.id = format!("scheduled-agent.{}", run.id.replace('/', "-"));
    event.agent_id = Some(agent_ref.to_owned());
    event.room_id = optional_arg_string(&run.target.args, "room_id")
        .or_else(|| optional_arg_string(&run.target.args, "session_id"));
    event
        .payload
        .insert("schedule_id".to_owned(), json!(&run.schedule_id));
    event.payload.insert("run_id".to_owned(), json!(&run.id));
    event
        .payload
        .insert("args".to_owned(), Value::Object(run.target.args.clone()));
    event.tags.push("scheduled".to_owned());
    repository.write_event(&event)?;
    Ok(format!("agent-event:{agent_ref}:{}", event.id))
}

fn optional_arg_string(args: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(|value| match value {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_owned())
            }
        }
        _ => None,
    })
}

fn dispatch_scheduled_event_run(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    run: &ScheduledRun,
) -> Result<String> {
    let event_ref = run.target.r#ref.trim();
    if event_ref.is_empty() {
        bail!("scheduled event target requires non-empty ref");
    }
    let repository =
        ConversationRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let kind = run
        .target
        .action
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "scheduled.event".to_owned());
    let mut event = ConversationEvent::new(&kind);
    event.id = format!("scheduled-event.{}", run.id.replace('/', "-"));
    event.payload.insert("ref".to_owned(), json!(event_ref));
    event
        .payload
        .insert("args".to_owned(), Value::Object(run.target.args.clone()));
    event.tags.push("scheduled".to_owned());
    repository.write_event(&event)?;
    Ok(format!("event:{event_ref}"))
}

fn receipt(
    run: &ScheduledRun,
    status: &str,
    result_ref: Option<String>,
    error: Option<String>,
) -> SchedulerDispatchReceipt {
    SchedulerDispatchReceipt {
        run_id: run.id.clone(),
        schedule_id: run.schedule_id.clone(),
        target_kind: format!("{:?}", run.target.kind),
        target_ref: run.target.r#ref.clone(),
        status: status.to_owned(),
        result_ref,
        error,
    }
}

fn workspace_namespace(namespace: ApiNamespace) -> WorkspaceNamespace {
    WorkspaceNamespace::new(namespace.tenant_id, namespace.user_id)
}

/// When `HC_SCHEDULE_AGENT_AUTO_PROCESS_INBOX` is `1` / `true` / `yes`, runs
/// [`process_conversation_inbox`] so new scheduled agent events can become turn proposals
/// (does not call LLM; drafting is separate).
fn maybe_process_inbox_after_agent_schedule(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
) {
    if !schedule_agent_auto_process_inbox_enabled() {
        return;
    }
    let api_namespace = ApiNamespace {
        tenant_id: workspace_namespace.tenant_id.clone(),
        user_id: workspace_namespace.user_id.clone(),
    };
    match process_conversation_inbox(config, api_namespace, None) {
        Ok(report) => {
            tracing::debug!(
                processed = report.processed_events,
                followups = report.fired_followups,
                proposals = report.proposals.len(),
                "schedule agent inbox auto-process"
            );
        }
        Err(error) => warn!(
            %error,
            "HC_SCHEDULE_AGENT_AUTO_PROCESS_INBOX: process_conversation_inbox failed"
        ),
    }
}

fn schedule_agent_auto_process_inbox_enabled() -> bool {
    matches!(
        std::env::var("HC_SCHEDULE_AGENT_AUTO_PROCESS_INBOX").map(|raw| {
            let v = raw.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        }),
        Ok(true)
    )
}

fn schedule_repository(config: &ServiceConfig, namespace: ApiNamespace) -> ScheduleRepository {
    ScheduleRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace(namespace),
    )
}

fn normalized_namespace(
    mut namespace: ApiNamespace,
    tenant_id: Option<String>,
    user_id: Option<String>,
) -> ApiNamespace {
    if let Some(tenant_id) = tenant_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = user_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        namespace.user_id = user_id;
    }
    if namespace.tenant_id.trim().is_empty() {
        namespace.tenant_id = hc_context::runtime::DEFAULT_TENANT_ID.to_owned();
    }
    if namespace.user_id.trim().is_empty() {
        namespace.user_id = hc_context::runtime::DEFAULT_USER_ID.to_owned();
    }
    namespace
}

#[cfg(test)]
mod tests {
    use super::*;
    use hc_scheduler::{
        ScheduleKind, ScheduleSpec, ScheduledRunStatus, ScheduledTarget, ScheduledTargetKind,
        ScheduledTask,
    };
    use serde_json::Map;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hc-service-scheduler-{name}-{}", now_unix()))
    }

    #[test]
    fn failing_command_target_is_recorded_as_failed_run() {
        let root = temp_root("bad-command");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let repository = ScheduleRepository::with_namespace(&root, workspace_namespace.clone());
        let task = ScheduledTask::new(
            "schedule.service.fail-cmd",
            "Failing Command Target",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(10),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Command,
                r#ref: "this-program-does-not-exist-honeycomb-test".to_owned(),
                action: None,
                args: Map::new(),
            },
        );
        repository.write_schedule(&task).unwrap();

        let report = dispatch_due_scheduled_runs(&config, namespace, Some(10)).unwrap();

        assert_eq!(report.queued_count, 1);
        assert_eq!(report.receipts.len(), 1);
        assert_eq!(report.receipts[0].status, "failed");
        let runs = repository.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, ScheduledRunStatus::Failed);
        assert!(
            runs[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("failed to spawn")
                || runs[0].error.as_deref().unwrap_or("").contains("exited")
        );
    }

    #[test]
    fn event_target_dispatch_succeeds() {
        let root = temp_root("event-ok");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let repository = ScheduleRepository::with_namespace(&root, workspace_namespace);
        let task = ScheduledTask::new(
            "schedule.service.event",
            "Emit Event",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(10),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Event,
                r#ref: "demo.signal".to_owned(),
                action: Some("wake".to_owned()),
                args: Map::new(),
            },
        );
        repository.write_schedule(&task).unwrap();

        let report = dispatch_due_scheduled_runs(&config, namespace, Some(10)).unwrap();

        assert_eq!(report.queued_count, 1);
        assert_eq!(report.receipts.len(), 1);
        assert_eq!(report.receipts[0].status, "succeeded");
        assert_eq!(
            report.receipts[0].result_ref.as_deref(),
            Some("event:demo.signal")
        );
        let runs = repository.list_runs().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, ScheduledRunStatus::Succeeded);
    }
}
