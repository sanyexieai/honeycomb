use anyhow::{Context, Result, bail};
use hc_context::runtime::{RuntimeIdentity, RuntimeVariables};
use hc_conversation::{ConversationEvent, ConversationRepository, FollowUpStatus};
use hc_protocol::ApiNamespace;
use hc_protocol::timed_run::{TimedRunLifecycle, timed_run_idempotency_key_v1};
use hc_scheduler::{
    SchedulePolicy, ScheduleRepository, ScheduleStatus, ScheduledRun, ScheduledRunStatus,
    ScheduledTargetKind, ScheduledTask, now_unix,
};
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::{
    CommandToolExecutor, McpServerRepository, ToolExecutor, ToolRepository,
    build_default_tool_execution_plan, builtin_tool, call_mcp_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use tracing::warn;

use crate::ServiceConfig;
use crate::conversation::process_conversation_inbox;

#[derive(Debug, Clone)]
pub struct ScheduledFollowUpRunSpec {
    pub id: String,
    pub due_at_unix: u64,
    pub draft_message: String,
    pub notes: String,
    pub payload: serde_json::Map<String, serde_json::Value>,
    /// Logical task id for stable idempotency (e.g. countdown `sequence_id`, or reminder scope key).
    pub logical_task_id: String,
    /// Index within a multi-run task; `0` for single-shot reminders.
    pub sequence_index: u32,
}

#[derive(Debug, Clone)]
pub struct ScheduledFollowUpTaskSpec {
    pub agent_id: String,
    pub trigger: String,
    pub room_id: Option<String>,
    pub runs: Vec<ScheduledFollowUpRunSpec>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerDispatchReport {
    pub now_unix: u64,
    pub queued_count: usize,
    pub receipts: Vec<SchedulerDispatchReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct FiredFollowUpMessage {
    pub followup_id: String,
    pub message: String,
}

pub trait FollowUpMessageSink {
    fn on_fired_followup_message(&mut self, message: &FiredFollowUpMessage);
}

#[derive(Default)]
pub struct NoopFollowUpMessageSink;

impl FollowUpMessageSink for NoopFollowUpMessageSink {
    fn on_fired_followup_message(&mut self, _: &FiredFollowUpMessage) {}
}

#[derive(Default)]
pub struct CollectFollowUpMessageSink {
    messages: Vec<FiredFollowUpMessage>,
}

impl CollectFollowUpMessageSink {
    pub fn into_messages(self) -> Vec<FiredFollowUpMessage> {
        self.messages
    }
}

impl FollowUpMessageSink for CollectFollowUpMessageSink {
    fn on_fired_followup_message(&mut self, message: &FiredFollowUpMessage) {
        self.messages.push(message.clone());
    }
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

/// Prometheus-style cumulative histogram (**milliseconds**) for successful **`dispatch-due`**
/// blocking-worker wall times (`hc-api` process). Buckets (`le`): 10 / 50 / 100 / 500 / **+Inf** via count.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct ApiDispatchDueWorkerWallMillisecondsHistogram {
    #[serde(rename = "count")]
    pub count: u64,
    #[serde(rename = "sum_ms")]
    pub sum_ms: u64,
    /// Cumulative count of observations with wall ms ≤ **10**.
    #[serde(rename = "bucket_ms_le_10")]
    pub bucket_le_ms_10: u64,
    /// Cumulative count with wall ms ≤ **50**.
    #[serde(rename = "bucket_ms_le_50")]
    pub bucket_le_ms_50: u64,
    /// Cumulative count with wall ms ≤ **100**.
    #[serde(rename = "bucket_ms_le_100")]
    pub bucket_le_ms_100: u64,
    /// Cumulative count with wall ms ≤ **500**.
    #[serde(rename = "bucket_ms_le_500")]
    pub bucket_le_ms_500: u64,
}

/// Wall-clock delay from run's [`ScheduledRun::scheduled_for_unix`] to when dispatch marks it
/// **Running** (`now` passed into [`dispatch_scheduled_run`]). Milliseconds; **process-local**
/// cumulative histogram (survives across CLI/hc-api callers in this OS process; **not** on disk).
///
/// Timestamps are whole Unix **seconds**; slip is `(now - scheduled_for_unix) * 1000`, clamped at zero.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct ScheduledRunDispatchSlipMillisecondsHistogram {
    #[serde(rename = "count")]
    pub count: u64,
    #[serde(rename = "sum_ms")]
    pub sum_ms: u64,
    #[serde(rename = "bucket_ms_le_1000")]
    pub bucket_le_ms_1000: u64,
    #[serde(rename = "bucket_ms_le_5000")]
    pub bucket_le_ms_5000: u64,
    #[serde(rename = "bucket_ms_le_30000")]
    pub bucket_le_ms_30000: u64,
    #[serde(rename = "bucket_ms_le_60000")]
    pub bucket_le_ms_60000: u64,
    #[serde(rename = "bucket_ms_le_300000")]
    pub bucket_le_ms_300000: u64,
    #[serde(rename = "bucket_ms_le_3600000")]
    pub bucket_le_ms_3600000: u64,
}

impl ScheduledRunDispatchSlipMillisecondsHistogram {
    pub fn observe(&mut self, slip_ms: u64) {
        self.count = self.count.saturating_add(1);
        self.sum_ms = self.sum_ms.saturating_add(slip_ms);
        if slip_ms <= 1000 {
            self.bucket_le_ms_1000 = self.bucket_le_ms_1000.saturating_add(1);
        }
        if slip_ms <= 5000 {
            self.bucket_le_ms_5000 = self.bucket_le_ms_5000.saturating_add(1);
        }
        if slip_ms <= 30_000 {
            self.bucket_le_ms_30000 = self.bucket_le_ms_30000.saturating_add(1);
        }
        if slip_ms <= 60_000 {
            self.bucket_le_ms_60000 = self.bucket_le_ms_60000.saturating_add(1);
        }
        if slip_ms <= 300_000 {
            self.bucket_le_ms_300000 = self.bucket_le_ms_300000.saturating_add(1);
        }
        if slip_ms <= 3_600_000 {
            self.bucket_le_ms_3600000 = self.bucket_le_ms_3600000.saturating_add(1);
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

fn slip_ms_histogram_is_empty(h: &ScheduledRunDispatchSlipMillisecondsHistogram) -> bool {
    h.is_empty()
}

static SCHEDULED_RUN_DISPATCH_SLIP_HISTOGRAMS: LazyLock<
    Mutex<HashMap<(String, String), ScheduledRunDispatchSlipMillisecondsHistogram>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn record_scheduled_run_dispatch_slip(namespace: &WorkspaceNamespace, slip_ms: u64) {
    let key = (namespace.tenant_id.clone(), namespace.user_id.clone());
    if let Ok(mut guard) = SCHEDULED_RUN_DISPATCH_SLIP_HISTOGRAMS.lock() {
        guard.entry(key).or_default().observe(slip_ms);
    }
}

/// Merge process-local [`ScheduledRunDispatchSlipMillisecondsHistogram`] for `tenant_id` / `user_id`
/// (filled whenever this process dispatches a [`ScheduledRun`] via [`dispatch_scheduled_run`]).
pub fn merge_scheduler_operational_stats_with_dispatch_slip_histogram(
    mut stats: SchedulerOperationalStats,
    tenant_id: &str,
    user_id: &str,
) -> SchedulerOperationalStats {
    if let Ok(guard) = SCHEDULED_RUN_DISPATCH_SLIP_HISTOGRAMS.lock() {
        if let Some(h) = guard.get(&(tenant_id.to_owned(), user_id.to_owned())) {
            stats.scheduled_run_dispatch_slip_ms_histogram = *h;
        }
    }
    stats
}

#[cfg(test)]
pub fn reset_scheduled_run_dispatch_slip_histograms_for_test() {
    if let Ok(mut guard) = SCHEDULED_RUN_DISPATCH_SLIP_HISTOGRAMS.lock() {
        guard.clear();
    }
}

/// Test-only hook to simulate slip observations without running a full dispatch.
#[cfg(test)]
pub fn record_scheduled_run_dispatch_slip_for_test(namespace: &WorkspaceNamespace, slip_ms: u64) {
    record_scheduled_run_dispatch_slip(namespace, slip_ms);
}

impl ApiDispatchDueWorkerWallMillisecondsHistogram {
    /// Record **one** successful worker sample (milliseconds, whole ms).
    pub fn observe(&mut self, wall_ms: u64) {
        self.count = self.count.saturating_add(1);
        self.sum_ms = self.sum_ms.saturating_add(wall_ms);
        if wall_ms <= 10 {
            self.bucket_le_ms_10 = self.bucket_le_ms_10.saturating_add(1);
        }
        if wall_ms <= 50 {
            self.bucket_le_ms_50 = self.bucket_le_ms_50.saturating_add(1);
        }
        if wall_ms <= 100 {
            self.bucket_le_ms_100 = self.bucket_le_ms_100.saturating_add(1);
        }
        if wall_ms <= 500 {
            self.bucket_le_ms_500 = self.bucket_le_ms_500.saturating_add(1);
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerOperationalStats {
    pub now_unix: u64,
    pub followup_total: usize,
    pub followup_pending: usize,
    pub followup_pending_due: usize,
    pub followup_fired: usize,
    pub followup_cancelled: usize,
    pub followup_failed: usize,
    pub schedule_total: usize,
    pub schedule_active: usize,
    pub schedule_paused: usize,
    pub schedule_cancelled: usize,
    pub schedule_timed_mirror_active: usize,
    pub run_queued: usize,
    pub run_running: usize,
    pub run_succeeded: usize,
    pub run_failed: usize,
    pub run_cancelled: usize,
    /// Process-local cumulative follow-up messages delivered (**`headless`** + **`webhook`**). Preferred JSON key; always `0` from [`scheduler_operational_stats`] alone. After **`hc-api`** merge, matches **`api_followup_headless_messages_delivered_total`**.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_followup_messages_delivered_total: u64,
    /// Legacy JSON field name; same semantics and value as **`api_followup_messages_delivered_total`**.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_followup_headless_messages_delivered_total: u64,
    /// Successful **`POST /v1/schedules/dispatch-due`** worker completions (this hc-api process).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_dispatch_due_completed_total: u64,
    /// Failed **`POST /v1/schedules/dispatch-due`** attempts (join or inner `anyhow` error).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_dispatch_due_failed_total: u64,
    /// Successful **`POST /v1/schedules/dispatch-queued`** worker completions.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_dispatch_queued_completed_total: u64,
    /// Failed **`POST /v1/schedules/dispatch-queued`** attempts.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_dispatch_queued_failed_total: u64,
    /// Successful in-process scheduler loop ticks (`HC_SCHEDULER_ENABLED=true`).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_scheduler_loop_tick_completed_total: u64,
    /// Failed scheduler loop ticks (same scope as above).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_scheduler_loop_tick_failed_total: u64,
    /// **`hc-api` process**: wall-clock milliseconds for the **last successful** **`POST /v1/schedules/dispatch-due`** blocking worker (excluding join/poll latency outside the worker closure).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_dispatch_due_last_worker_wall_ms: u64,
    /// Last successful **`POST /v1/schedules/dispatch-queued`** blocking worker milliseconds (same semantics).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_dispatch_queued_last_worker_wall_ms: u64,
    /// Last successful in-process **`HC_SCHEDULER_ENABLED`** tick blocking worker milliseconds.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub api_scheduler_loop_tick_last_worker_wall_ms: u64,
    /// **`hc-api` process**: cumulative wall-time histogram (success-only **`dispatch-due`** worker milliseconds inside `spawn_blocking`).
    #[serde(default, skip_serializing_if = "wall_ms_histogram_is_empty")]
    pub api_dispatch_due_worker_wall_ms_histogram: ApiDispatchDueWorkerWallMillisecondsHistogram,
    /// **`hc-api` process**: cumulative wall-time histogram (success-only **`dispatch-queued`** worker milliseconds inside `spawn_blocking`).
    #[serde(default, skip_serializing_if = "wall_ms_histogram_is_empty")]
    pub api_dispatch_queued_worker_wall_ms_histogram: ApiDispatchDueWorkerWallMillisecondsHistogram,
    /// **`hc-api` process**: cumulative wall-time histogram (success-only **`HC_SCHEDULER_ENABLED`** tick blocking worker milliseconds).
    #[serde(default, skip_serializing_if = "wall_ms_histogram_is_empty")]
    pub api_scheduler_loop_tick_worker_wall_ms_histogram:
        ApiDispatchDueWorkerWallMillisecondsHistogram,
    /// **Process-local** (this OS process): slip from run `scheduled_for_unix` to dispatch start
    /// ([`dispatch_scheduled_run`] `now`), merged from [`merge_scheduler_operational_stats_with_dispatch_slip_histogram`].
    #[serde(default, skip_serializing_if = "slip_ms_histogram_is_empty")]
    pub scheduled_run_dispatch_slip_ms_histogram: ScheduledRunDispatchSlipMillisecondsHistogram,
}

fn wall_ms_histogram_is_empty(h: &ApiDispatchDueWorkerWallMillisecondsHistogram) -> bool {
    h.is_empty()
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
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

pub fn list_conversation_followups(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
) -> Result<Vec<hc_conversation::PendingFollowUp>> {
    ConversationRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    )
    .list_followups()
}

/// One `timed.followup.fired` conversation event, for session recovery / replay.
#[derive(Debug, Clone, Serialize)]
pub struct TimedFollowupFiredEventRow {
    pub event_id: String,
    pub created_at_unix: u64,
    pub followup_id: String,
    pub draft_message: Option<String>,
}

/// Lists `timed.followup.fired` events with `created_at_unix >= since_created_at_unix`.
/// Events are returned in the same order as [`ConversationRepository::list_events`] (chronological).
pub fn list_timed_followup_fired_events_since_created(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
    since_created_at_unix: u64,
) -> Result<Vec<TimedFollowupFiredEventRow>> {
    let conv = ConversationRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let mut rows = Vec::new();
    for event in conv.list_events()? {
        if event.kind != "timed.followup.fired" || event.created_at_unix < since_created_at_unix {
            continue;
        }
        let followup_id = event
            .payload
            .get("followup_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if followup_id.is_empty() {
            continue;
        }
        let draft_message = event
            .payload
            .get("draft_message")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        rows.push(TimedFollowupFiredEventRow {
            event_id: event.id.clone(),
            created_at_unix: event.created_at_unix,
            followup_id: followup_id.to_owned(),
            draft_message,
        });
    }
    Ok(rows)
}

pub fn scheduler_operational_stats(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
    now: Option<u64>,
) -> Result<SchedulerOperationalStats> {
    let now = now.unwrap_or_else(now_unix);
    let followups = list_conversation_followups(config, workspace_namespace)?;
    let followup_total = followups.len();
    let followup_pending = followups
        .iter()
        .filter(|f| f.status == FollowUpStatus::Pending)
        .count();
    let followup_pending_due = followups
        .iter()
        .filter(|f| f.status == FollowUpStatus::Pending && f.due_at_unix <= now)
        .count();
    let followup_fired = followups
        .iter()
        .filter(|f| f.status == FollowUpStatus::Fired)
        .count();
    let followup_cancelled = followups
        .iter()
        .filter(|f| f.status == FollowUpStatus::Cancelled)
        .count();
    let followup_failed = followups
        .iter()
        .filter(|f| f.status == FollowUpStatus::Failed)
        .count();

    let sched_repo = ScheduleRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let schedules = sched_repo.list_schedules()?;
    let schedule_total = schedules.len();
    let schedule_active = schedules
        .iter()
        .filter(|s| s.status == ScheduleStatus::Active)
        .count();
    let schedule_paused = schedules
        .iter()
        .filter(|s| s.status == ScheduleStatus::Paused)
        .count();
    let schedule_cancelled = schedules
        .iter()
        .filter(|s| s.status == ScheduleStatus::Cancelled)
        .count();
    let schedule_timed_mirror_active = schedules
        .iter()
        .filter(|s| {
            s.status == ScheduleStatus::Active
                && s.tags.iter().any(|t| t == "timed")
                && s.target.r#ref == "timed.followup"
        })
        .count();
    let runs = sched_repo.list_runs()?;
    let run_queued = runs
        .iter()
        .filter(|r| r.status == ScheduledRunStatus::Queued)
        .count();
    let run_running = runs
        .iter()
        .filter(|r| r.status == ScheduledRunStatus::Running)
        .count();
    let run_succeeded = runs
        .iter()
        .filter(|r| r.status == ScheduledRunStatus::Succeeded)
        .count();
    let run_failed = runs
        .iter()
        .filter(|r| r.status == ScheduledRunStatus::Failed)
        .count();
    let run_cancelled = runs
        .iter()
        .filter(|r| r.status == ScheduledRunStatus::Cancelled)
        .count();

    Ok(SchedulerOperationalStats {
        now_unix: now,
        followup_total,
        followup_pending,
        followup_pending_due,
        followup_fired,
        followup_cancelled,
        followup_failed,
        schedule_total,
        schedule_active,
        schedule_paused,
        schedule_cancelled,
        schedule_timed_mirror_active,
        run_queued,
        run_running,
        run_succeeded,
        run_failed,
        run_cancelled,
        api_followup_messages_delivered_total: 0,
        api_followup_headless_messages_delivered_total: 0,
        api_dispatch_due_completed_total: 0,
        api_dispatch_due_failed_total: 0,
        api_dispatch_queued_completed_total: 0,
        api_dispatch_queued_failed_total: 0,
        api_scheduler_loop_tick_completed_total: 0,
        api_scheduler_loop_tick_failed_total: 0,
        api_dispatch_due_last_worker_wall_ms: 0,
        api_dispatch_queued_last_worker_wall_ms: 0,
        api_scheduler_loop_tick_last_worker_wall_ms: 0,
        api_dispatch_due_worker_wall_ms_histogram:
            ApiDispatchDueWorkerWallMillisecondsHistogram::default(),
        api_dispatch_queued_worker_wall_ms_histogram:
            ApiDispatchDueWorkerWallMillisecondsHistogram::default(),
        api_scheduler_loop_tick_worker_wall_ms_histogram:
            ApiDispatchDueWorkerWallMillisecondsHistogram::default(),
        scheduled_run_dispatch_slip_ms_histogram:
            ScheduledRunDispatchSlipMillisecondsHistogram::default(),
    })
}

/// OpenMetrics-compatible text exposition (Prometheus scrape) for [`SchedulerOperationalStats`].
///
/// Labels: `tenant_id`, `user_id`. All metrics are gauges reflecting a snapshot at collection time.
pub fn scheduler_operational_stats_openmetrics_text(
    stats: &SchedulerOperationalStats,
    tenant_id: &str,
    user_id: &str,
) -> String {
    fn prometheus_escape_label(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                _ => out.push(ch),
            }
        }
        out
    }
    fn push_gauge(
        out: &mut String,
        name: &'static str,
        help: &str,
        labels: &str,
        val: impl std::fmt::Display,
    ) {
        use std::fmt::Write;
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} gauge");
        let _ = writeln!(out, "{name}{{{labels}}} {val}");
    }

    fn append_api_dispatch_wall_ms_histogram_openmetrics(
        out: &mut String,
        labels: &str,
        base: &'static str,
        help: &'static str,
        h: &ApiDispatchDueWorkerWallMillisecondsHistogram,
    ) {
        use std::fmt::Write;
        if h.is_empty() {
            return;
        }
        let _ = writeln!(out, "# HELP {} {}", base, help);
        let _ = writeln!(out, "# TYPE {} histogram", base);
        for (le_ms, cumulative) in [
            ("10", h.bucket_le_ms_10),
            ("50", h.bucket_le_ms_50),
            ("100", h.bucket_le_ms_100),
            ("500", h.bucket_le_ms_500),
        ] {
            let _ = writeln!(
                out,
                "{}_bucket{{{},le=\"{}\"}} {}",
                base, labels, le_ms, cumulative
            );
        }
        let _ = writeln!(out, "{}_bucket{{{},le=\"+Inf\"}} {}", base, labels, h.count);
        let _ = writeln!(out, "{}_sum{{{}}} {}", base, labels, h.sum_ms);
        let _ = writeln!(out, "{}_count{{{}}} {}", base, labels, h.count);
    }

    fn append_scheduled_run_dispatch_slip_ms_histogram_openmetrics(
        out: &mut String,
        labels: &str,
        base: &'static str,
        help: &'static str,
        h: &ScheduledRunDispatchSlipMillisecondsHistogram,
    ) {
        use std::fmt::Write;
        if h.is_empty() {
            return;
        }
        let _ = writeln!(out, "# HELP {} {}", base, help);
        let _ = writeln!(out, "# TYPE {} histogram", base);
        for (le_ms, cumulative) in [
            ("1000", h.bucket_le_ms_1000),
            ("5000", h.bucket_le_ms_5000),
            ("30000", h.bucket_le_ms_30000),
            ("60000", h.bucket_le_ms_60000),
            ("300000", h.bucket_le_ms_300000),
            ("3600000", h.bucket_le_ms_3600000),
        ] {
            let _ = writeln!(
                out,
                "{}_bucket{{{},le=\"{}\"}} {}",
                base, labels, le_ms, cumulative
            );
        }
        let _ = writeln!(out, "{}_bucket{{{},le=\"+Inf\"}} {}", base, labels, h.count);
        let _ = writeln!(out, "{}_sum{{{}}} {}", base, labels, h.sum_ms);
        let _ = writeln!(out, "{}_count{{{}}} {}", base, labels, h.count);
    }

    let labels = format!(
        r#"tenant_id="{}",user_id="{}""#,
        prometheus_escape_label(tenant_id),
        prometheus_escape_label(user_id)
    );

    let mut out = String::with_capacity(1024);

    macro_rules! gauge {
        ($n:literal, $h:literal, $v:expr) => {
            push_gauge(&mut out, $n, $h, &labels, $v);
        };
    }

    gauge!(
        "honeycomb_scheduler_now_unix",
        "Unix reference time for this snapshot (typically request now_unix)",
        stats.now_unix
    );
    gauge!(
        "honeycomb_scheduler_followups_total",
        "Follow-up records in namespace",
        stats.followup_total
    );
    gauge!(
        "honeycomb_scheduler_followups_pending",
        "Follow-ups in Pending status",
        stats.followup_pending
    );
    gauge!(
        "honeycomb_scheduler_followups_pending_due",
        "Pending follow-ups due at snapshot now",
        stats.followup_pending_due
    );
    gauge!(
        "honeycomb_scheduler_followups_fired",
        "Follow-ups in Fired terminal status",
        stats.followup_fired
    );
    gauge!(
        "honeycomb_scheduler_followups_cancelled",
        "Follow-ups in Cancelled terminal status",
        stats.followup_cancelled
    );
    gauge!(
        "honeycomb_scheduler_followups_failed",
        "Follow-ups in Failed terminal status",
        stats.followup_failed
    );

    gauge!(
        "honeycomb_scheduler_schedules_total",
        "Scheduled tasks in repository",
        stats.schedule_total
    );
    gauge!(
        "honeycomb_scheduler_schedules_active",
        "Scheduled tasks Active",
        stats.schedule_active
    );
    gauge!(
        "honeycomb_scheduler_schedules_paused",
        "Scheduled tasks Paused",
        stats.schedule_paused
    );
    gauge!(
        "honeycomb_scheduler_schedules_cancelled",
        "Scheduled tasks Cancelled",
        stats.schedule_cancelled
    );
    gauge!(
        "honeycomb_scheduler_timed_mirror_schedules_active",
        "Active mirrored timed.followup schedules",
        stats.schedule_timed_mirror_active
    );

    gauge!(
        "honeycomb_scheduler_runs_queued",
        "Scheduled run rows in Queued status",
        stats.run_queued
    );
    gauge!(
        "honeycomb_scheduler_runs_running",
        "Scheduled run rows in Running status",
        stats.run_running
    );
    gauge!(
        "honeycomb_scheduler_runs_succeeded",
        "Scheduled run rows in Succeeded status",
        stats.run_succeeded
    );
    gauge!(
        "honeycomb_scheduler_runs_failed",
        "Scheduled run rows in Failed status",
        stats.run_failed
    );
    gauge!(
        "honeycomb_scheduler_runs_cancelled",
        "Scheduled run rows in Cancelled status",
        stats.run_cancelled
    );
    gauge!(
        "honeycomb_scheduler_api_followup_messages_delivered_total",
        "hc-api process counter: follow-up messages delivered successfully (headless event/store or webhook; not persisted)",
        stats.api_followup_messages_delivered_total
    );
    gauge!(
        "honeycomb_scheduler_api_followup_headless_messages_delivered_total",
        "legacy name; same value as honeycomb_scheduler_api_followup_messages_delivered_total",
        stats.api_followup_headless_messages_delivered_total
    );
    gauge!(
        "honeycomb_scheduler_api_dispatch_due_completed_total",
        "hc-api process counter: dispatch-due worker successes",
        stats.api_dispatch_due_completed_total
    );
    gauge!(
        "honeycomb_scheduler_api_dispatch_due_failed_total",
        "hc-api process counter: dispatch-due worker failures",
        stats.api_dispatch_due_failed_total
    );
    gauge!(
        "honeycomb_scheduler_api_dispatch_queued_completed_total",
        "hc-api process counter: dispatch-queued worker successes",
        stats.api_dispatch_queued_completed_total
    );
    gauge!(
        "honeycomb_scheduler_api_dispatch_queued_failed_total",
        "hc-api process counter: dispatch-queued worker failures",
        stats.api_dispatch_queued_failed_total
    );
    gauge!(
        "honeycomb_scheduler_api_scheduler_loop_tick_completed_total",
        "hc-api process counter: scheduler loop tick successes",
        stats.api_scheduler_loop_tick_completed_total
    );
    gauge!(
        "honeycomb_scheduler_api_scheduler_loop_tick_failed_total",
        "hc-api process counter: scheduler loop tick failures",
        stats.api_scheduler_loop_tick_failed_total
    );
    gauge!(
        "honeycomb_scheduler_api_dispatch_due_last_worker_wall_ms",
        "hc-api: last successful dispatch-due blocking worker wall time in milliseconds",
        stats.api_dispatch_due_last_worker_wall_ms
    );
    gauge!(
        "honeycomb_scheduler_api_dispatch_queued_last_worker_wall_ms",
        "hc-api: last successful dispatch-queued blocking worker wall time in milliseconds",
        stats.api_dispatch_queued_last_worker_wall_ms
    );
    gauge!(
        "honeycomb_scheduler_api_scheduler_loop_tick_last_worker_wall_ms",
        "hc-api: last successful scheduler loop tick blocking worker wall time in milliseconds",
        stats.api_scheduler_loop_tick_last_worker_wall_ms
    );

    append_api_dispatch_wall_ms_histogram_openmetrics(
        &mut out,
        &labels,
        "honeycomb_scheduler_api_dispatch_due_worker_wall_ms",
        "hc-api cumulative histogram (ms) inside spawn_blocking on successful POST /dispatch-due",
        &stats.api_dispatch_due_worker_wall_ms_histogram,
    );
    append_api_dispatch_wall_ms_histogram_openmetrics(
        &mut out,
        &labels,
        "honeycomb_scheduler_api_dispatch_queued_worker_wall_ms",
        "hc-api cumulative histogram (ms) inside spawn_blocking on successful POST /dispatch-queued",
        &stats.api_dispatch_queued_worker_wall_ms_histogram,
    );
    append_api_dispatch_wall_ms_histogram_openmetrics(
        &mut out,
        &labels,
        "honeycomb_scheduler_api_scheduler_loop_tick_worker_wall_ms",
        "hc-api cumulative histogram (ms) inside spawn_blocking on successful HC_SCHEDULER_ENABLED tick",
        &stats.api_scheduler_loop_tick_worker_wall_ms_histogram,
    );

    append_scheduled_run_dispatch_slip_ms_histogram_openmetrics(
        &mut out,
        &labels,
        "honeycomb_scheduler_scheduled_run_dispatch_slip_ms",
        "milliseconds from ScheduledRun.scheduled_for_unix to Running (dispatch start); cumulative in this process",
        &stats.scheduled_run_dispatch_slip_ms_histogram,
    );

    out.push_str("# EOF\n");
    out
}

/// Cancel a **Pending** follow-up, cancel its `timed.followup.{id}` mirror schedule when present,
/// and append a `timed.followup.cancelled` conversation event for headless recovery / audit.
pub fn cancel_followup_with_timed_mirror(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
    followup_id: &str,
) -> Result<hc_conversation::PendingFollowUp> {
    if followup_id.trim().is_empty() {
        bail!("follow-up id cannot be empty");
    }
    let conv = ConversationRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let relative = ConversationRepository::followup_relative_path_for(followup_id);
    let peek = conv.read_followup(&relative)?;
    if peek.status != FollowUpStatus::Pending {
        bail!(
            "follow-up {followup_id} is {:?}, only Pending can be cancelled here",
            peek.status
        );
    }

    let mut followup = conv.update_followup_status(followup_id, FollowUpStatus::Cancelled)?;
    let stamp = now_unix();
    followup.notes = format!(
        "{}\n\noperator cancelled at unix {stamp}",
        followup.notes.trim()
    )
    .trim()
    .to_owned();
    conv.write_followup(&followup)?;

    let sched_repo = ScheduleRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace.clone(),
    );
    let mirror_id = format!("timed.followup.{followup_id}");
    if let Ok(mut task) = sched_repo.get_schedule(&mirror_id) {
        if task.status != ScheduleStatus::Cancelled {
            task.status = ScheduleStatus::Cancelled;
            sched_repo.write_schedule(&task)?;
        }
    }

    let mut event = ConversationEvent::new("timed.followup.cancelled");
    event.id = format!(
        "timed-followup-cancelled.{}",
        followup_id.replace(['/', '\\'], "-")
    );
    event.room_id = followup.room_id.clone();
    event.agent_id = Some(followup.agent_id.clone());
    event
        .payload
        .insert("followup_id".to_owned(), json!(followup_id));
    event
        .payload
        .insert("cancelled_at_unix".to_owned(), json!(stamp));
    event.tags.push("timed".to_owned());
    event.tags.push("scheduled".to_owned());
    conv.write_event(&event)?;

    Ok(followup)
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

pub fn fired_followup_messages_from_receipts(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    receipts: &[SchedulerDispatchReceipt],
) -> Result<Vec<FiredFollowUpMessage>> {
    let workspace_namespace = workspace_namespace(namespace);
    let repository =
        ConversationRepository::with_namespace(config.workspace_root.clone(), workspace_namespace);
    let mut messages = Vec::new();
    for receipt in receipts {
        let Some(result_ref) = receipt.result_ref.as_deref() else {
            continue;
        };
        let Some(raw_followup_id) = result_ref.strip_prefix("followup:") else {
            continue;
        };
        let followup_id = raw_followup_id
            .split(':')
            .next()
            .unwrap_or(raw_followup_id)
            .to_owned();
        let relative_path = ConversationRepository::followup_relative_path_for(&followup_id);
        let Ok(followup) = repository.read_followup(relative_path) else {
            continue;
        };
        if followup.status != FollowUpStatus::Fired {
            continue;
        }
        let Some(message) = followup
            .payload
            .get("draft_message")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        messages.push(FiredFollowUpMessage {
            followup_id,
            message: message.to_owned(),
        });
    }
    Ok(messages)
}

pub fn dispatch_fired_followup_messages_from_receipts(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    receipts: &[SchedulerDispatchReceipt],
    sink: &mut impl FollowUpMessageSink,
) -> Result<Vec<String>> {
    let messages = fired_followup_messages_from_receipts(config, namespace, receipts)?;
    let mut delivered_ids = Vec::with_capacity(messages.len());
    for message in &messages {
        sink.on_fired_followup_message(message);
        delivered_ids.push(message.followup_id.clone());
    }
    Ok(delivered_ids)
}

pub fn dispatch_fired_followup_messages_headless(
    config: &ServiceConfig,
    namespace: ApiNamespace,
    receipts: &[SchedulerDispatchReceipt],
) -> Result<Vec<String>> {
    let mut sink = NoopFollowUpMessageSink;
    dispatch_fired_followup_messages_from_receipts(config, namespace, receipts, &mut sink)
}

pub fn persist_scheduled_followup_task(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    spec: ScheduledFollowUpTaskSpec,
) -> Result<Vec<String>> {
    let conversation_repository =
        ConversationRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let schedule_repository =
        ScheduleRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let mut followup_ids = Vec::new();
    for run in spec.runs {
        let logical_task_id = run.logical_task_id.trim();
        if logical_task_id.is_empty() {
            bail!("ScheduledFollowUpRunSpec.logical_task_id must be non-empty");
        }

        let idem =
            timed_run_idempotency_key_v1(logical_task_id, run.due_at_unix, run.sequence_index);
        if let Some(existing) =
            conversation_repository.find_pending_followup_id_by_timed_idempotency_key(&idem)?
        {
            followup_ids.push(existing);
            continue;
        }

        let mut followup = hc_conversation::PendingFollowUp::new(
            spec.agent_id.clone(),
            spec.trigger.clone(),
            run.due_at_unix,
        );
        followup.id = run.id;
        followup.room_id = spec.room_id.clone();
        followup.payload = run.payload;
        followup.payload.insert(
            "draft_message".to_owned(),
            serde_json::Value::String(run.draft_message),
        );
        followup
            .payload
            .insert("timed_run_idempotency_key_v1".to_owned(), json!(idem));
        followup.notes = run.notes;
        conversation_repository.write_followup(&followup)?;

        let mut target_args = serde_json::Map::new();
        target_args.insert(
            "followup_id".to_owned(),
            serde_json::Value::String(followup.id.clone()),
        );
        target_args.insert("timed_run_idempotency_key_v1".to_owned(), json!(idem));
        let mut schedule = ScheduledTask::new(
            format!("timed.followup.{}", followup.id),
            format!("Timed followup {}", followup.id),
            hc_scheduler::ScheduleSpec {
                kind: hc_scheduler::ScheduleKind::Once,
                run_at_unix: Some(followup.due_at_unix),
                interval_seconds: None,
            },
            hc_scheduler::ScheduledTarget {
                kind: hc_scheduler::ScheduledTargetKind::Event,
                r#ref: "timed.followup".to_owned(),
                action: Some("timed.followup.fire".to_owned()),
                args: target_args,
            },
        );
        schedule.tags = vec![
            "scheduled".to_owned(),
            "timed".to_owned(),
            "followup".to_owned(),
        ];
        schedule.policy = timed_followup_schedule_policy_from_env();
        schedule.notes = "Mirrored from timed_turn followup queue.".to_owned();
        schedule_repository.write_schedule(&schedule)?;
        followup_ids.push(followup.id);
    }
    Ok(followup_ids)
}

/// Unified lifecycle view for timed follow-ups: merges conversation follow-up status with the
/// scheduler run row (`timed.followup.{id}`) when available.
pub fn timed_run_lifecycle_resolve(
    followup: FollowUpStatus,
    scheduler_run: Option<ScheduledRunStatus>,
) -> TimedRunLifecycle {
    match followup {
        FollowUpStatus::Cancelled => TimedRunLifecycle::Done,
        FollowUpStatus::Failed => TimedRunLifecycle::Failed,
        FollowUpStatus::Fired => TimedRunLifecycle::Fired,
        FollowUpStatus::Pending => match scheduler_run {
            None | Some(ScheduledRunStatus::Queued) => TimedRunLifecycle::Queued,
            Some(ScheduledRunStatus::Running) => TimedRunLifecycle::Running,
            Some(ScheduledRunStatus::Succeeded) => TimedRunLifecycle::Fired,
            Some(ScheduledRunStatus::Failed) => TimedRunLifecycle::Failed,
            Some(ScheduledRunStatus::Cancelled) => TimedRunLifecycle::Done,
        },
    }
}

pub fn dispatch_followups_until_fired(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    followup_ids: &[String],
    sink: &mut impl FollowUpMessageSink,
) -> Result<()> {
    let mut pending: std::collections::BTreeSet<String> = followup_ids.iter().cloned().collect();
    while !pending.is_empty() {
        let report = dispatch_due_scheduled_runs(config, namespace.clone(), Some(now_unix()))?;
        let delivered_ids = dispatch_fired_followup_messages_from_receipts(
            config,
            namespace.clone(),
            &report.receipts,
            sink,
        )?;
        for id in delivered_ids {
            pending.remove(&id);
        }
        if !pending.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    Ok(())
}

fn timed_followup_schedule_policy_from_env() -> SchedulePolicy {
    use std::env;

    let mut policy = SchedulePolicy::default();
    if let Ok(raw) = env::var("HC_TIMED_FOLLOWUP_SCHEDULE_MAX_RETRIES") {
        let t = raw.trim();
        if !t.is_empty() {
            if let Ok(v) = t.parse::<u32>() {
                policy.max_retries = v;
            }
        }
    }
    if let Ok(raw) = env::var("HC_TIMED_FOLLOWUP_SCHEDULE_RETRY_DELAY_SECONDS") {
        let t = raw.trim();
        if !t.is_empty() {
            if let Ok(v) = t.parse::<u64>() {
                policy.retry_delay_seconds = v.max(1);
            }
        }
    }
    policy
}

/// When a `timed.followup` mirror dispatch fails, bump the parent task's `failure_count` and, if
/// within `policy.max_retries`, re-activate the task with `next_fire_at = now + retry_delay`.
fn maybe_rearm_timed_followup_schedule_after_failed_dispatch(
    repository: &ScheduleRepository,
    run: &ScheduledRun,
    failure_time_unix: u64,
) -> Result<()> {
    if run.target.kind != ScheduledTargetKind::Event || run.target.r#ref.trim() != "timed.followup"
    {
        return Ok(());
    }
    let Ok(mut task) = repository.get_schedule(&run.schedule_id) else {
        return Ok(());
    };
    if !task.tags.iter().any(|t| t == "timed") {
        return Ok(());
    }

    task.state.failure_count = task.state.failure_count.saturating_add(1);
    if task.state.failure_count > task.policy.max_retries {
        warn!(
            schedule_id = %task.id,
            failure_count = task.state.failure_count,
            max_retries = task.policy.max_retries,
            "timed.followup mirror schedule will not retry further after dispatch failure"
        );
        repository.write_schedule(&task)?;
        return Ok(());
    }

    let delay = task.policy.retry_delay_seconds.max(1);
    task.status = ScheduleStatus::Active;
    task.state.next_fire_at_unix = Some(failure_time_unix.saturating_add(delay));
    warn!(
        schedule_id = %task.id,
        retry_at = ?task.state.next_fire_at_unix,
        failure_count = task.state.failure_count,
        max_retries = task.policy.max_retries,
        "re-arming timed.followup mirror schedule after dispatch failure"
    );
    repository.write_schedule(&task)?;
    Ok(())
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
    let slip_seconds = now.saturating_sub(run.scheduled_for_unix);
    let slip_ms = slip_seconds.saturating_mul(1000);
    record_scheduled_run_dispatch_slip(namespace, slip_ms);

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
            if let Err(rearm_err) = maybe_rearm_timed_followup_schedule_after_failed_dispatch(
                repository,
                &run,
                finished_at,
            ) {
                warn!(%rearm_err, run_id=%run.id, "failed to re-arm timed.followup mirror schedule after dispatch failure");
            }
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
    if event_ref == "timed.followup" {
        return dispatch_timed_followup_event(config, namespace, run);
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

fn dispatch_timed_followup_event(
    config: &ServiceConfig,
    namespace: &WorkspaceNamespace,
    run: &ScheduledRun,
) -> Result<String> {
    let followup_id = optional_arg_string(&run.target.args, "followup_id")
        .context("timed.followup event requires args.followup_id")?;
    let repository =
        ConversationRepository::with_namespace(config.workspace_root.clone(), namespace.clone());
    let relative = ConversationRepository::followup_relative_path_for(&followup_id);
    let mut followup = repository.read_followup(relative)?;
    if followup.status == FollowUpStatus::Fired {
        return Ok(format!("followup:{followup_id}:already-fired"));
    }
    if followup.status != FollowUpStatus::Pending {
        bail!(
            "timed follow-up {followup_id} is {:?}, refuse to fire",
            followup.status
        );
    }
    let draft_message = followup
        .payload
        .get("draft_message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    followup.status = FollowUpStatus::Fired;
    followup.notes = format!(
        "{}\n\nscheduler fired at {}",
        followup.notes.trim(),
        now_unix()
    )
    .trim()
    .to_owned();
    repository.write_followup(&followup)?;
    let mut event = ConversationEvent::new("timed.followup.fired");
    event.id = format!("timed-followup-fired.{}", run.id.replace('/', "-"));
    event.room_id = followup.room_id.clone();
    event.agent_id = Some(followup.agent_id.clone());
    event
        .payload
        .insert("followup_id".to_owned(), json!(followup_id.clone()));
    if let Some(message) = draft_message {
        event
            .payload
            .insert("draft_message".to_owned(), json!(message));
    }
    event.tags.push("scheduled".to_owned());
    event.tags.push("timed".to_owned());
    repository.write_event(&event)?;
    Ok(format!("followup:{followup_id}"))
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
    use hc_conversation::{ConversationRepository, FollowUpStatus, PendingFollowUp};
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

    #[test]
    fn timed_followup_event_dispatch_marks_followup_fired_and_emits_event() {
        let root = temp_root("timed-followup");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let schedule_repo = ScheduleRepository::with_namespace(&root, workspace_namespace.clone());
        let conversation_repo =
            ConversationRepository::with_namespace(&root, workspace_namespace.clone());
        let mut followup = PendingFollowUp::new("agent.system.reminder", "reminder.due", 10);
        followup.id = "test-followup-1".to_owned();
        followup
            .payload
            .insert("draft_message".to_owned(), json!("到时间了"));
        conversation_repo.write_followup(&followup).unwrap();
        let mut args = Map::new();
        args.insert("followup_id".to_owned(), json!("test-followup-1"));
        let task = ScheduledTask::new(
            "schedule.service.timed-followup",
            "Timed Followup Event",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(10),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Event,
                r#ref: "timed.followup".to_owned(),
                action: Some("timed.followup.fire".to_owned()),
                args,
            },
        );
        schedule_repo.write_schedule(&task).unwrap();

        let report = dispatch_due_scheduled_runs(&config, namespace.clone(), Some(10)).unwrap();
        assert_eq!(report.queued_count, 1);
        assert_eq!(report.receipts.len(), 1);
        assert_eq!(report.receipts[0].status, "succeeded");
        assert_eq!(
            report.receipts[0].result_ref.as_deref(),
            Some("followup:test-followup-1")
        );

        let refreshed = conversation_repo
            .read_followup(ConversationRepository::followup_relative_path_for(
                "test-followup-1",
            ))
            .unwrap();
        assert_eq!(refreshed.status, FollowUpStatus::Fired);
        let events = conversation_repo.list_events().unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "timed.followup.fired"
                && event
                    .payload
                    .get("followup_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("test-followup-1")
        }));

        let rows = list_timed_followup_fired_events_since_created(&config, &workspace_namespace, 0)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].followup_id, "test-followup-1");
        assert_eq!(rows[0].draft_message.as_deref(), Some("到时间了"));
        assert!(
            list_timed_followup_fired_events_since_created(&config, &workspace_namespace, u64::MAX)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn timed_run_lifecycle_resolve_covers_pending_and_terminal() {
        use hc_protocol::timed_run::TimedRunLifecycle as L;

        assert_eq!(
            timed_run_lifecycle_resolve(FollowUpStatus::Pending, None),
            L::Queued
        );
        assert_eq!(
            timed_run_lifecycle_resolve(FollowUpStatus::Pending, Some(ScheduledRunStatus::Running)),
            L::Running
        );
        assert_eq!(
            timed_run_lifecycle_resolve(
                FollowUpStatus::Pending,
                Some(ScheduledRunStatus::Succeeded)
            ),
            L::Fired
        );
        assert_eq!(
            timed_run_lifecycle_resolve(FollowUpStatus::Fired, None),
            L::Fired
        );
        assert_eq!(
            timed_run_lifecycle_resolve(FollowUpStatus::Cancelled, None),
            L::Done
        );
    }

    #[test]
    fn persist_scheduled_followup_task_writes_idempotency_key() {
        use hc_protocol::timed_run::timed_run_idempotency_key_v1;

        let root = temp_root("idem-followup");
        let config = ServiceConfig::new(&root);
        let workspace_namespace = WorkspaceNamespace::new(
            ApiNamespace::default().tenant_id,
            ApiNamespace::default().user_id,
        );
        let ltid = "agent::trigger::pfx::sess";
        let due = 99u64;
        let idx = 2u32;
        let expected = timed_run_idempotency_key_v1(ltid, due, idx);
        persist_scheduled_followup_task(
            &config,
            &workspace_namespace,
            ScheduledFollowUpTaskSpec {
                agent_id: "agent".to_owned(),
                trigger: "trigger".to_owned(),
                room_id: None,
                runs: vec![ScheduledFollowUpRunSpec {
                    id: "run-a".to_owned(),
                    due_at_unix: due,
                    draft_message: "hi".to_owned(),
                    notes: String::new(),
                    payload: serde_json::Map::new(),
                    logical_task_id: ltid.to_owned(),
                    sequence_index: idx,
                }],
            },
        )
        .unwrap();

        let conversation_repo =
            ConversationRepository::with_namespace(root.clone(), workspace_namespace.clone());
        let followup = conversation_repo
            .read_followup(ConversationRepository::followup_relative_path_for("run-a"))
            .unwrap();
        assert_eq!(
            followup.payload.get("timed_run_idempotency_key_v1"),
            Some(&json!(expected))
        );
        let schedule_repo = ScheduleRepository::with_namespace(root, workspace_namespace);
        let schedule = schedule_repo
            .get_schedule(&format!("timed.followup.{}", followup.id))
            .unwrap();
        assert_eq!(
            schedule.target.args.get("timed_run_idempotency_key_v1"),
            Some(&json!(expected))
        );
    }

    #[test]
    fn persist_scheduled_followup_task_dedupes_pending_idempotency_key() {
        let root = temp_root("idem-dedupe");
        let config = ServiceConfig::new(&root);
        let workspace_namespace = WorkspaceNamespace::new(
            ApiNamespace::default().tenant_id,
            ApiNamespace::default().user_id,
        );
        let ltid = "logical::task::dedupe";
        let spec_base = || ScheduledFollowUpTaskSpec {
            agent_id: "agent".to_owned(),
            trigger: "trigger".to_owned(),
            room_id: None,
            runs: vec![],
        };

        let mut first_spec = spec_base();
        first_spec.runs.push(ScheduledFollowUpRunSpec {
            id: "run-first".to_owned(),
            due_at_unix: 200,
            draft_message: "a".to_owned(),
            notes: String::new(),
            payload: serde_json::Map::new(),
            logical_task_id: ltid.to_owned(),
            sequence_index: 0,
        });
        let ids1 =
            persist_scheduled_followup_task(&config, &workspace_namespace, first_spec).unwrap();
        assert_eq!(ids1, vec!["run-first".to_owned()]);

        let mut second_spec = spec_base();
        second_spec.runs.push(ScheduledFollowUpRunSpec {
            id: "run-second".to_owned(),
            due_at_unix: 200,
            draft_message: "b".to_owned(),
            notes: String::new(),
            payload: serde_json::Map::new(),
            logical_task_id: ltid.to_owned(),
            sequence_index: 0,
        });
        let ids2 =
            persist_scheduled_followup_task(&config, &workspace_namespace, second_spec).unwrap();
        assert_eq!(ids2, vec!["run-first".to_owned()]);

        let conversation_repo =
            ConversationRepository::with_namespace(root, workspace_namespace.clone());
        assert_eq!(conversation_repo.list_followups().unwrap().len(), 1);
    }

    #[test]
    fn failed_timed_followup_dispatch_rearms_parent_schedule_when_within_max_retries() {
        let root = temp_root("timed-rearm");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let schedule_repo = ScheduleRepository::with_namespace(&root, workspace_namespace);

        let mut args = Map::new();
        args.insert("followup_id".to_owned(), json!("missing-followup"));

        let mut task = ScheduledTask::new(
            "timed.followup.missing-followup",
            "Mirror timed follow-up",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(500_000),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Event,
                r#ref: "timed.followup".to_owned(),
                action: Some("timed.followup.fire".to_owned()),
                args,
            },
        );
        task.tags = vec![
            "scheduled".to_owned(),
            "timed".to_owned(),
            "followup".to_owned(),
        ];
        task.policy.retry_delay_seconds = 37;
        task.policy.max_retries = 2;
        schedule_repo.write_schedule(&task).unwrap();

        let before_dispatch = now_unix();
        let report = dispatch_due_scheduled_runs(&config, namespace, Some(500_000)).unwrap();
        let after_dispatch = now_unix();

        assert_eq!(report.receipts.len(), 1);
        assert_eq!(report.receipts[0].status, "failed");

        let updated = schedule_repo
            .get_schedule("timed.followup.missing-followup")
            .unwrap();
        assert_eq!(updated.status, ScheduleStatus::Active);
        assert_eq!(updated.state.failure_count, 1);
        let retry_at = updated.state.next_fire_at_unix.unwrap();
        assert!(retry_at >= before_dispatch.saturating_add(37));
        assert!(retry_at <= after_dispatch.saturating_add(37).saturating_add(1));
    }

    #[test]
    fn cancel_followup_with_timed_mirror_cancels_schedule_and_writes_event() {
        let root = temp_root("fu-cancel-flow");
        let config = ServiceConfig::new(&root);
        let workspace_namespace = WorkspaceNamespace::new(
            ApiNamespace::default().tenant_id.clone(),
            ApiNamespace::default().user_id.clone(),
        );

        persist_scheduled_followup_task(
            &config,
            &workspace_namespace,
            ScheduledFollowUpTaskSpec {
                agent_id: "agent".into(),
                trigger: "trigger".into(),
                room_id: None,
                runs: vec![ScheduledFollowUpRunSpec {
                    id: "fu-to-cancel".into(),
                    due_at_unix: 300,
                    draft_message: "x".into(),
                    notes: String::new(),
                    payload: serde_json::Map::new(),
                    logical_task_id: "logical::cancel::case".into(),
                    sequence_index: 0,
                }],
            },
        )
        .unwrap();

        cancel_followup_with_timed_mirror(&config, &workspace_namespace, "fu-to-cancel").unwrap();

        let conv =
            ConversationRepository::with_namespace(root.clone(), workspace_namespace.clone());
        let f = conv
            .read_followup(ConversationRepository::followup_relative_path_for(
                "fu-to-cancel",
            ))
            .unwrap();
        assert_eq!(f.status, FollowUpStatus::Cancelled);

        let task = ScheduleRepository::with_namespace(root.clone(), workspace_namespace.clone())
            .get_schedule("timed.followup.fu-to-cancel")
            .unwrap();
        assert_eq!(task.status, ScheduleStatus::Cancelled);

        assert!(
            conv.list_events()
                .unwrap()
                .iter()
                .any(|e| e.kind == "timed.followup.cancelled"
                    && e.payload.get("followup_id").and_then(|v| v.as_str())
                        == Some("fu-to-cancel"))
        );

        let st = scheduler_operational_stats(&config, &workspace_namespace, Some(301)).unwrap();
        assert_eq!(st.followup_pending, 0);
        assert_eq!(st.followup_total, 1);
        assert_eq!(st.followup_cancelled, 1);
        assert_eq!(st.followup_fired, 0);
        assert_eq!(st.schedule_timed_mirror_active, 0);
    }

    #[test]
    fn dispatch_timed_followup_fails_when_followup_cancelled() {
        let root = temp_root("fu-cancel-fired");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let ws = WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        persist_scheduled_followup_task(
            &config,
            &ws,
            ScheduledFollowUpTaskSpec {
                agent_id: "a".into(),
                trigger: "t".into(),
                room_id: None,
                runs: vec![ScheduledFollowUpRunSpec {
                    id: "fu-x".into(),
                    due_at_unix: 77,
                    draft_message: "m".into(),
                    notes: String::new(),
                    payload: serde_json::Map::new(),
                    logical_task_id: "z::z::z::_".into(),
                    sequence_index: 0,
                }],
            },
        )
        .unwrap();

        let conv =
            ConversationRepository::with_namespace(config.workspace_root.clone(), ws.clone());
        conv.update_followup_status("fu-x", FollowUpStatus::Cancelled)
            .unwrap();

        let report = dispatch_due_scheduled_runs(&config, namespace, Some(77)).unwrap();
        assert_eq!(report.receipts.len(), 1);
        assert_eq!(report.receipts[0].status, "failed");
    }

    #[test]
    fn collect_sink_receives_fired_followup_messages() {
        let root = temp_root("collect-sink");
        let config = ServiceConfig::new(&root);
        let namespace = ApiNamespace::default();
        let workspace_namespace =
            WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
        let schedule_repo = ScheduleRepository::with_namespace(&root, workspace_namespace.clone());
        let conversation_repo = ConversationRepository::with_namespace(&root, workspace_namespace);
        let mut followup = PendingFollowUp::new("agent.system.reminder", "reminder.due", 10);
        followup.id = "test-followup-collect".to_owned();
        followup
            .payload
            .insert("draft_message".to_owned(), json!("收到了"));
        conversation_repo.write_followup(&followup).unwrap();
        let mut args = Map::new();
        args.insert("followup_id".to_owned(), json!("test-followup-collect"));
        let task = ScheduledTask::new(
            "schedule.service.collect-followup",
            "Collect Followup Event",
            ScheduleSpec {
                kind: ScheduleKind::Once,
                run_at_unix: Some(10),
                interval_seconds: None,
            },
            ScheduledTarget {
                kind: ScheduledTargetKind::Event,
                r#ref: "timed.followup".to_owned(),
                action: Some("timed.followup.fire".to_owned()),
                args,
            },
        );
        schedule_repo.write_schedule(&task).unwrap();

        let report = dispatch_due_scheduled_runs(&config, namespace.clone(), Some(10)).unwrap();
        let mut sink = CollectFollowUpMessageSink::default();
        let delivered = dispatch_fired_followup_messages_from_receipts(
            &config,
            namespace,
            &report.receipts,
            &mut sink,
        )
        .unwrap();
        let messages = sink.into_messages();

        assert_eq!(delivered, vec!["test-followup-collect".to_owned()]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].followup_id, "test-followup-collect");
        assert_eq!(messages[0].message, "收到了");
    }

    #[test]
    fn openmetrics_exporter_covers_known_metrics() {
        let stats = SchedulerOperationalStats {
            now_unix: 100,
            followup_total: 1,
            followup_pending: 0,
            followup_pending_due: 0,
            followup_fired: 1,
            followup_cancelled: 2,
            followup_failed: 0,
            schedule_total: 3,
            schedule_active: 1,
            schedule_paused: 1,
            schedule_cancelled: 1,
            schedule_timed_mirror_active: 0,
            run_queued: 0,
            run_running: 0,
            run_succeeded: 4,
            run_failed: 0,
            run_cancelled: 0,
            api_followup_messages_delivered_total: 7,
            api_followup_headless_messages_delivered_total: 7,
            api_dispatch_due_completed_total: 1,
            api_dispatch_due_failed_total: 2,
            api_dispatch_queued_completed_total: 3,
            api_dispatch_queued_failed_total: 4,
            api_scheduler_loop_tick_completed_total: 5,
            api_scheduler_loop_tick_failed_total: 6,
            api_dispatch_due_last_worker_wall_ms: 11,
            api_dispatch_queued_last_worker_wall_ms: 12,
            api_scheduler_loop_tick_last_worker_wall_ms: 13,
            api_dispatch_due_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram {
                    count: 2,
                    sum_ms: 55,
                    bucket_le_ms_10: 1,
                    bucket_le_ms_50: 2,
                    bucket_le_ms_100: 2,
                    bucket_le_ms_500: 2,
                },
            api_dispatch_queued_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram {
                    count: 1,
                    sum_ms: 33,
                    bucket_le_ms_10: 0,
                    bucket_le_ms_50: 1,
                    bucket_le_ms_100: 1,
                    bucket_le_ms_500: 1,
                },
            api_scheduler_loop_tick_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram {
                    count: 4,
                    sum_ms: 444,
                    bucket_le_ms_10: 1,
                    bucket_le_ms_50: 2,
                    bucket_le_ms_100: 3,
                    bucket_le_ms_500: 4,
                },
            scheduled_run_dispatch_slip_ms_histogram:
                ScheduledRunDispatchSlipMillisecondsHistogram {
                    count: 3,
                    sum_ms: 12_000,
                    bucket_le_ms_1000: 2,
                    bucket_le_ms_5000: 3,
                    bucket_le_ms_30000: 3,
                    bucket_le_ms_60000: 3,
                    bucket_le_ms_300000: 3,
                    bucket_le_ms_3600000: 3,
                },
        };
        let text =
            scheduler_operational_stats_openmetrics_text(&stats, r#"tenant"quote"#, "plain_user");
        assert!(text.ends_with("# EOF\n"), "{text}");
        assert!(
            text.contains("honeycomb_scheduler_now_unix"),
            "missing primary gauge: {text}"
        );
        assert!(
            text.contains("tenant_id=\"tenant\\\"quote\",user_id=\"plain_user\""),
            "escaped label line missing: {text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_followups_cancelled"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_followup_messages_delivered_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_followup_headless_messages_delivered_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_due_completed_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_due_failed_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_queued_completed_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_queued_failed_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_scheduler_loop_tick_completed_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_scheduler_loop_tick_failed_total"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_due_last_worker_wall_ms"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_queued_last_worker_wall_ms"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_scheduler_loop_tick_last_worker_wall_ms"),
            "{text}"
        );
        assert!(
            text.contains("TYPE honeycomb_scheduler_api_dispatch_due_worker_wall_ms histogram"),
            "{text}"
        );
        assert!(
            text.contains("TYPE honeycomb_scheduler_api_dispatch_queued_worker_wall_ms histogram"),
            "{text}"
        );
        assert!(
            text.contains(
                "TYPE honeycomb_scheduler_api_scheduler_loop_tick_worker_wall_ms histogram"
            ),
            "{text}"
        );
        assert!(
            text.contains("TYPE honeycomb_scheduler_scheduled_run_dispatch_slip_ms histogram"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_due_worker_wall_ms_bucket"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_due_worker_wall_ms_sum"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_due_worker_wall_ms_count"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_dispatch_queued_worker_wall_ms_bucket"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_api_scheduler_loop_tick_worker_wall_ms_bucket"),
            "{text}"
        );
        assert!(
            text.contains(r#"le="+Inf"#),
            "expected +Inf bucket line: {text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_scheduled_run_dispatch_slip_ms_bucket"),
            "{text}"
        );
        assert!(
            text.contains("honeycomb_scheduler_scheduled_run_dispatch_slip_ms_count"),
            "{text}"
        );
    }

    #[test]
    fn merge_dispatch_slip_histogram_reads_process_store() {
        reset_scheduled_run_dispatch_slip_histograms_for_test();
        let ws = WorkspaceNamespace::new("tenant-slip-merge", "user-slip-merge");
        record_scheduled_run_dispatch_slip_for_test(&ws, 4000);

        let stats = SchedulerOperationalStats {
            now_unix: 1,
            followup_total: 0,
            followup_pending: 0,
            followup_pending_due: 0,
            followup_fired: 0,
            followup_cancelled: 0,
            followup_failed: 0,
            schedule_total: 0,
            schedule_active: 0,
            schedule_paused: 0,
            schedule_cancelled: 0,
            schedule_timed_mirror_active: 0,
            run_queued: 0,
            run_running: 0,
            run_succeeded: 0,
            run_failed: 0,
            run_cancelled: 0,
            api_followup_messages_delivered_total: 0,
            api_followup_headless_messages_delivered_total: 0,
            api_dispatch_due_completed_total: 0,
            api_dispatch_due_failed_total: 0,
            api_dispatch_queued_completed_total: 0,
            api_dispatch_queued_failed_total: 0,
            api_scheduler_loop_tick_completed_total: 0,
            api_scheduler_loop_tick_failed_total: 0,
            api_dispatch_due_last_worker_wall_ms: 0,
            api_dispatch_queued_last_worker_wall_ms: 0,
            api_scheduler_loop_tick_last_worker_wall_ms: 0,
            api_dispatch_due_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram::default(),
            api_dispatch_queued_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram::default(),
            api_scheduler_loop_tick_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram::default(),
            scheduled_run_dispatch_slip_ms_histogram:
                ScheduledRunDispatchSlipMillisecondsHistogram::default(),
        };

        let merged = merge_scheduler_operational_stats_with_dispatch_slip_histogram(
            stats,
            "tenant-slip-merge",
            "user-slip-merge",
        );
        assert_eq!(merged.scheduled_run_dispatch_slip_ms_histogram.count, 1);
        assert_eq!(merged.scheduled_run_dispatch_slip_ms_histogram.sum_ms, 4000);
        assert_eq!(
            merged
                .scheduled_run_dispatch_slip_ms_histogram
                .bucket_le_ms_5000,
            1
        );

        reset_scheduled_run_dispatch_slip_histograms_for_test();
    }
}
