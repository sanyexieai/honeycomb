use anyhow::{Context, Result, anyhow, bail};
use hc_context::runtime::{RuntimeIdentity, RuntimeVariables};
use hc_protocol::ApiNamespace;
use hc_scheduler::{
    ScheduleRepository, ScheduleStatus, ScheduledRun, ScheduledRunStatus, ScheduledTargetKind,
    ScheduledTask, now_unix,
};
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::{McpServerRepository, call_mcp_tool};
use serde::{Deserialize, Serialize};

use crate::ServiceConfig;

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

fn dispatch_scheduled_run(
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
        _ => Err(anyhow!(
            "scheduled target kind {:?} is not dispatchable by hc-service yet",
            run.target.kind
        )),
    };

    let finished_at = now_unix();
    match result {
        Ok(result_ref) => {
            run.status = ScheduledRunStatus::Succeeded;
            run.finished_at_unix = Some(finished_at);
            run.result_ref = Some(result_ref.clone());
            run.error = None;
            repository.write_run(&run)?;
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
    use hc_scheduler::{ScheduleKind, ScheduleSpec, ScheduledTarget, ScheduledTask};
    use serde_json::Map;
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hc-service-scheduler-{name}-{}", now_unix()))
    }

    #[test]
    fn unsupported_target_is_recorded_as_failed_run() {
        let root = temp_root("unsupported");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let repository = ScheduleRepository::with_namespace(&root, workspace_namespace);
        let task = ScheduledTask::new(
            "schedule.service.unsupported",
            "Unsupported Target",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(10),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Event,
                r#ref: "event.demo".to_owned(),
                action: Some("wake".to_owned()),
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
                .contains("not dispatchable")
        );
    }
}
