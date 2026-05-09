# Scheduled Tasks

## Goal

Honeycomb needs a generic way to run work later, repeat work, and keep long-lived work recoverable after process restarts.

The scheduler must stay small and decoupled:

- it decides **when** something is due
- it records **what target** should be invoked
- it emits a runtime job or event
- it does not understand business domains
- it does not directly contain agent, MCP, reminder, health, or ordering logic

Domain behavior belongs in agents, tools, MCP services, or application code invoked by the scheduled target.

## Non-Goals

The scheduler is not:

- an agent router
- a workflow engine
- a chat memory system
- an MCP client
- a UI component
- a replacement for `hc-core` workers

It should be possible to replace the scheduler without rewriting agents or tools.

## Layering

### `hc-scheduler`

Owns durable scheduled-task records and due-time calculation.

Responsibilities:

- read and write scheduled task documents
- compute `next_fire_at`
- mark runs as queued, running, succeeded, failed, cancelled, or paused
- produce generic due events
- reschedule recurring tasks

Non-goals:

- executing tools directly
- calling LLMs
- knowing MCP method names
- interpreting business payloads

### `hc-core`

Owns runtime jobs and workers.

The scheduler should hand due work to `hc-core` as a `RunRequest` or runtime command. Long-running work remains a worker/job concern.

### `hc-agent`

Owns task/agent interpretation.

If a scheduled target is an agent task, the scheduler should only wake the agent/task entry point. The agent layer decides how to reason, which tools to call, and what to persist.

### `hc-toolchain`

Owns tool execution.

If a scheduled target is a tool, the scheduler should emit a tool invocation request in generic form. `hc-toolchain` handles actual execution.

## Durable Document Shape

Scheduled tasks should be Markdown documents with YAML frontmatter so they remain inspectable and easy to migrate.

Suggested path:

```text
workspace/tenants/{tenant}/users/{user}/scheduled/{schedule_id}.md
```

Suggested frontmatter:

```yaml
---
id: schedule.medication.noon
type: scheduled_task
title: Noon Medication Check
tenant_id: local
user_id: default
status: active
schedule:
  kind: interval | once | daily | weekly | cron
  run_at: 2026-05-01T12:00:00+08:00
  timezone: Asia/Shanghai
  interval_seconds: 3600
  cron: "0 12 * * *"
target:
  kind: agent | tool | mcp | command | event
  ref: agent.careos.medication
  action: check_noon_medication
  args:
    user_id: 1
policy:
  misfire: run_once | skip | run_all
  max_retries: 3
  retry_delay_seconds: 60
  overlap: skip | queue | replace
state:
  next_fire_at: 2026-05-01T12:00:00+08:00
  last_fire_at:
  last_run_id:
  failure_count: 0
tags:
  - scheduled
  - careos
---

Human-readable notes about why this schedule exists.
```

Only `schedule`, `target`, `policy`, and `state` are scheduler-owned. The body is documentation.

## Target Contract

The scheduler should treat targets as opaque dispatch envelopes:

```rust
pub enum ScheduledTargetKind {
    Agent,
    Tool,
    Mcp,
    Command,
    Event,
}
```

Generic target fields:

- `kind`: dispatch family
- `ref`: target identifier
- `action`: optional action name meaningful to the target handler
- `args`: JSON object passed through unchanged

The scheduler must not branch on business-specific fields such as meal type, medication name, restaurant id, or health metric.

## Runtime Flow

1. A scheduler loop wakes at a fixed tick or receives a wake signal.
2. It loads active scheduled tasks.
3. It selects tasks where `next_fire_at <= now`.
4. For each due task, it atomically creates a `ScheduledRun` record.
5. It dispatches the run to the appropriate adapter.
6. The adapter creates a runtime job, tool request, MCP call, agent message, or event.
7. Worker/tool/agent execution writes results back.
8. Scheduler updates `state.last_fire_at`, `state.next_fire_at`, and failure counters.

The scheduler should be restart-safe: if the process dies after a run is queued, the next boot can inspect run state and apply the misfire/overlap policy.

## Run Record

Each actual firing should be stored separately from the schedule.

Suggested path:

```text
workspace/tenants/{tenant}/users/{user}/scheduled/runs/{run_id}.md
```

Suggested fields:

```yaml
---
id: scheduled-run.1777600000000.1
type: scheduled_run
schedule_id: schedule.medication.noon
status: queued | running | succeeded | failed | cancelled
scheduled_for: 2026-05-01T12:00:00+08:00
started_at:
finished_at:
target:
  kind: agent
  ref: agent.careos.medication
  action: check_noon_medication
  args: {}
result_ref:
error:
---
```

Run records make debugging and audit possible without coupling scheduler state to worker output.

## Dispatch Adapters

Adapters should be small and independently testable.

Suggested adapters:

- `AgentDispatchAdapter`: posts an agent/task message.
- `ToolDispatchAdapter`: calls `hc-toolchain`.
- `McpDispatchAdapter`: calls a configured MCP tool through `hc-toolchain`.
- `CommandDispatchAdapter`: creates `hc-core::RunRequest`.
- `EventDispatchAdapter`: appends a runtime event for another process to consume.

The scheduler depends on an adapter trait, not on concrete implementations.

```rust
pub trait ScheduledDispatch {
    fn dispatch(&self, run: &ScheduledRun) -> Result<DispatchReceipt>;
}
```

## Recurrence

Initial recurrence kinds:

- `once`
- `interval`
- `daily`
- `weekly`
- `cron`

The recurrence calculator should be pure and unit-tested. It should accept:

- previous `next_fire_at`
- current `now`
- timezone
- misfire policy

and return the next timestamp or `None`.

## Overlap Policy

When a task fires again while the previous run is unfinished:

- `skip`: do not queue a new run
- `queue`: queue another run
- `replace`: cancel/mark old run and queue the new one

Default should be `skip` for safety.

## CareOS Examples

These examples are intentionally generic at the scheduler layer.

### Medication Reminder

```yaml
target:
  kind: mcp
  ref: mcp.careos-medication-reminder
  action: get_today_schedule
  args:
    user_id: 1
```

### Meal Recommendation

```yaml
target:
  kind: agent
  ref: agent.careos.food_delivery
  action: recommend_meal
  args:
    user_id: 1
    meal_context: lunch
```

The scheduler does not know what lunch means. It only passes the args through.

### Follow Up Order Status

```yaml
target:
  kind: mcp
  ref: mcp.careos-food-delivery
  action: get_order_detail
  args:
    order_id: 89
```

## Implementation Plan

1. Add `hc-scheduler` crate with schedule/run structs and Markdown repository.
2. Add pure recurrence calculation and tests.
3. Add a polling loop that returns due run records without executing them.
4. Add dispatch adapter trait and a command adapter that creates `hc-core::RunRequest`.
5. Add tool/MCP adapter through `hc-toolchain`.
6. Add agent adapter through `hc-agent` or `hc-service`.
7. Add CLI commands:
   - `hc-cli schedule add`
   - `hc-cli schedule list`
   - `hc-cli schedule run-due`
   - `hc-cli schedule pause`
   - `hc-cli schedule resume`
   - `hc-cli schedule dispatch-queued`
   - `hc-cli schedule dispatch-due`
   - `hc-cli schedule watch`
8. Later, run the scheduler loop inside `hc-api` or a dedicated daemon.

## Current CLI Surface

The first implementation exposes a small generic control plane:

```text
hc-cli schedule add --id <id> --title <text> --kind <once|interval> --run-at-unix <ts> [--interval-seconds <n>] --target-kind <agent|tool|mcp|command|event> --target-ref <id> [--target-action <name>] [--arg key=value] [--json]
hc-cli schedule list [--json]
hc-cli schedule run-due [--now-unix <ts>] [--json]
hc-cli schedule runs [--json]
hc-cli schedule pause --id <id> [--json]
hc-cli schedule resume --id <id> [--json]
hc-cli schedule dispatch-queued [--now-unix <ts>] [--json]
hc-cli schedule dispatch-due [--now-unix <ts>] [--json]
hc-cli schedule watch [--tick-seconds <n>] [--max-ticks <n>] [--json]
hc-cli schedule followups list [--due-only] [--status <pending|fired|cancelled|failed>] [--json]
hc-cli schedule followups cancel --id <followup-id> [--json]
hc-cli schedule followups replay-events [--since-created-unix <ts>] [--json] [--no-print]
hc-cli schedule stats [--now-unix <ts>] [--json]
```

Follow-up rows live under `conversation/followups`; `followups cancel` also cancels the `timed.followup.{id}` mirror task when present and writes a `timed.followup.cancelled` event. `followups replay-events` lists persisted `timed.followup.fired` events and prints `assistant> …` (or `--json`); pass `--since-created-unix` after reconnect to replay only newer fires. `stats` calls `scheduler_operational_stats` (disk snapshot) for the current `HC_TENANT_ID` / `HC_USER_ID` namespace—follow-ups by lifecycle (`pending` / `pending_due` / `fired` / `cancelled` / `failed`), schedules by status (`active` / `paused` / `cancelled`, plus active timed-followup mirrors), and run lifecycle (`queued` / `running` / `succeeded` / `failed` / `cancelled`). Same **disk-backed** fields as **`GET /v1/schedules/operational-stats`**, except **hc-api** merges **process-local** `api_*` counters into that HTTP JSON/Prometheus (`api_followup_*`, `api_dispatch_*`, `api_scheduler_loop_tick_*`); CLI `stats` omits those or leaves them at **0**.

`dispatch-due` is restart-friendly: it first creates any due run records, then dispatches queued runs. `watch` is a thin polling loop over the same behavior and is intentionally not business-aware. Dispatch path matches `hc-service::scheduler::dispatch_due_scheduled_runs` (same stack as `hc-api` `/v1/schedules/dispatch-due`). With `watch --json`, each tick emits `now_unix`, `queued_count` (due runs newly enqueued that tick), and `receipts`.

`hc-api` can run the same loop in-process when explicitly enabled:

```text
HC_SCHEDULER_ENABLED=true
HC_SCHEDULER_TICK_SECONDS=30
HC_TENANT_ID=local
HC_USER_ID=default
```

The API loop delegates to `hc-service::scheduler`; it does not contain domain-specific scheduling or routing rules.

Read-only parity with the CLI recovery helpers:

```text
GET /v1/schedules/followup-fired-events?tenant_id=local&user_id=default&since_created_at_unix=<ts>
GET /v1/schedules/operational-stats?tenant_id=local&user_id=default&now_unix=<ts>
GET /v1/schedules/metrics/prometheus?tenant_id=local&user_id=default&now_unix=<ts>
```

`followup-fired-events` returns `TimedFollowupFiredEventRow` JSON. **`operational-stats`** returns **`SchedulerOperationalStats`**. **`metrics/prometheus`** returns OpenMetrics gauges (`honeycomb_scheduler_*` with `tenant_id` / `user_id` labels) — same counters as operational-stats, for Prometheus scraping (`since_created_at_unix` N/A; omit `now_unix` to use wall-clock). Snapshot backlog depth includes pending follow-ups (`honeycomb_scheduler_followups_pending`, `honeycomb_scheduler_followups_pending_due`) and run queue depth (`honeycomb_scheduler_runs_queued`, `honeycomb_scheduler_runs_running`). You can approximate terminal outcome mix in PromQL from `runs_succeeded` vs `runs_failed` / `runs_cancelled`; these are **point-in-time row counts**, not request-rate counters or latency histograms.

OpenMetrics exposes **`honeycomb_scheduler_api_followup_messages_delivered_total`** (preferred) and the legacy alias **`honeycomb_scheduler_api_followup_headless_messages_delivered_total`** with the **same value**. **`GET /v1/schedules/operational-stats`** JSON includes **`api_followup_messages_delivered_total`** (preferred) and **`api_followup_headless_messages_delivered_total`** (legacy); both are **omitted when zero** (`serde` `skip_serializing_if`). The count is a **process-local cumulative** of follow-up **messages** delivered by **this hc-api instance** via the scheduler loop (`HC_SCHEDULER_ENABLED=true`) and `POST /v1/schedules/dispatch-due` / `dispatch-queued` — for **`headless`** (event/store) and **`webhook`** (successful HTTP POST) — scoped by query `tenant_id` / `user_id`. Merged at **read time** only; **not** on disk; **resets on restart**. The standalone `hc_service::scheduler::scheduler_operational_stats` snapshot exposes `0` for both fields (typically omitted in JSON).

Additional **hc-api process counters** are merged the same way (JSON fields `api_dispatch_due_completed_total`, `api_dispatch_due_failed_total`, `api_dispatch_queued_completed_total`, `api_dispatch_queued_failed_total`, `api_scheduler_loop_tick_completed_total`, `api_scheduler_loop_tick_failed_total`; OpenMetrics `honeycomb_scheduler_api_dispatch_*`, `honeycomb_scheduler_api_scheduler_loop_tick_*`): one increment on dispatch worker success versus failure (`Err` inner result or blocked-task join failure), per queried tenant/user. Not persisted across restarts.

Last **successful** blocking-worker duration samples (milliseconds, wall time **inside** the `spawn_blocking` closure only) expose as JSON `api_dispatch_due_last_worker_wall_ms`, `api_dispatch_queued_last_worker_wall_ms`, `api_scheduler_loop_tick_last_worker_wall_ms`, and gauges `honeycomb_scheduler_*_last_worker_wall_ms`. Failed attempts keep the prior sample unchanged.

Successful **`POST /v1/schedules/dispatch-due`** workers additionally accumulate **`api_dispatch_due_worker_wall_ms_histogram`** (JSON: `count`, `sum_ms`, cumulative `bucket_ms_le_*`; OpenMetrics: `TYPE histogram`, `honeycomb_scheduler_api_dispatch_due_worker_wall_ms_bucket{_sum,_count}`, buckets `le=10|50|100|500|+Inf`). Successful **`POST /v1/schedules/dispatch-queued`** workers populate **`api_dispatch_queued_worker_wall_ms_histogram`** / **`honeycomb_scheduler_api_dispatch_queued_worker_wall_ms_*`** with the same bucket layout. Successful **`HC_SCHEDULER_ENABLED`** scheduler loop ticks (`dispatch_due_scheduled_runs` + followup delivery in-process) populate **`api_scheduler_loop_tick_worker_wall_ms_histogram`** / **`honeycomb_scheduler_api_scheduler_loop_tick_worker_wall_ms_*`**. CLI-only callers omit these histograms (same as other **`api_*`** counters). Persisted/read merge behavior matches other **`api_*`** process counters (`0` when empty or from disk-only callers).

**Scheduled-run dispatch slip** (milliseconds from `ScheduledRun.scheduled_for_unix` to when the run is marked **Running**) accumulates in **`scheduled_run_dispatch_slip_ms_histogram`** for **every code path** that calls `dispatch_scheduled_run` (CLI `schedule dispatch-due` / `watch`, **`hc-api`** `/dispatch-*` / scheduler loop, tests). Buckets (OpenMetrics `le`, ms): **1000 / 5000 / 30000 / 60000 / 300000 / 3600000 / +Inf**. Unix timestamps are second-resolution, so non-zero slip is usually a multiple of **1000 ms**. Same merge rules as other process-local histograms: merged at operational-stats / Prometheus read time; **not persisted**.

`hc-api` exposes an explicit scheduler followup delivery strategy:

```text
HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE=headless|off|webhook
HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL=<https URL>   # required when mode=webhook
HC_SCHEDULER_FOLLOWUP_WEBHOOK_BEARER_TOKEN=<secret>   # optional; sends Authorization: Bearer …
HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS=30   # optional; per-request timeout, default 30, clamped 1–300
```

Current modes:

- `headless` (default): resolve fired followup messages via scheduler receipts without stdout side effects (events / store as today).
- `off`: skip followup message delivery pass (dispatch still runs; followup message extraction is disabled).
- `webhook`: for each fired follow-up message, `POST` JSON to `HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL` with body `{"tenant_id","user_id","followup_id","message"}`. Requests send **`User-Agent: honeycomb-hc-api/<version>`**. If **`HC_SCHEDULER_FOLLOWUP_WEBHOOK_BEARER_TOKEN`** is set, requests include **`Authorization: Bearer <token>`**. Timeout is **`HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS`** (default **30**, clamped **1–300**) per request. Non-2xx or network error fails the dispatch worker. On startup, **hc-api** logs `followup_delivery_mode`, **`webhook_timeout_secs`**, and booleans **`webhook_url_configured`** / **`webhook_bearer_configured`** (no URLs or tokens); if the URL is missing, it also emits a **warn**.

Behavior matrix:

| Runtime path | Mode | Followup delivery pass |
| --- | --- | --- |
| API scheduler loop (`HC_SCHEDULER_ENABLED=true`) | `headless` | enabled |
| API scheduler loop (`HC_SCHEDULER_ENABLED=true`) | `off` | disabled |
| API scheduler loop (`HC_SCHEDULER_ENABLED=true`) | `webhook` | enabled (HTTP POST) |
| `POST /v1/schedules/dispatch-due` | `headless` | enabled |
| `POST /v1/schedules/dispatch-due` | `off` | disabled |
| `POST /v1/schedules/dispatch-due` | `webhook` | enabled (HTTP POST) |
| `POST /v1/schedules/dispatch-queued` | `headless` | enabled |
| `POST /v1/schedules/dispatch-queued` | `off` | disabled |
| `POST /v1/schedules/dispatch-queued` | `webhook` | enabled (HTTP POST) |

Notes:

- `hc-cli schedule watch` remains interactive and prints `assistant> ...` for fired followup messages.
- API logs include `delivered_followups` for scheduler loop and dispatch endpoints.
- In the `hc-cli` REPL, a single background ticker runs **`dispatch_due_scheduled_runs`** on the chat namespace (`HC_CLI_CHAT_SCHEDULER_TICK_MS`, default **500**, min **50**), then **`dispatch_fired_followup_messages_from_receipts`** with stdout `assistant>` — same primitives as **`hc-cli schedule watch`**. Keep it on by default; set **`HC_CLI_CHAT_SCHEDULER_ENABLED`** to **`false`/`0`/`no`/`off`** when you run **`schedule watch`** for the same tenant/user to avoid double dispatch. **`TimedDeliverMode::Interactive`** only persists follow-ups (plus mirrored `timed.followup` tasks); **`InteractiveSelfContained`** still spawns a per-turn thread that polls `dispatch_followups_until_fired`, for tooling without this REPL ticker.

### Timed follow-up mirror retries (`timed.followup.*`)

Reminder / countdown rows persist a mirrored `ScheduledTask` (`target=timed.followup`). Dispatch failures increment `failure_count` on that task and, while `failure_count <= policy.max_retries`, re-activate the task with:

`next_fire_at_unix = <dispatch finish time> + policy.retry_delay_seconds`.

Optional environment overrides applied when persisting mirrors from `hc-service::scheduler::persist_scheduled_followup_task`:

```text
HC_TIMED_FOLLOWUP_SCHEDULE_MAX_RETRIES=<u32>
HC_TIMED_FOLLOWUP_SCHEDULE_RETRY_DELAY_SECONDS=<u64 seconds, min 1 when set>
```

## Design Rule

No schedule implementation should contain business-specific matching logic.

If a business flow needs special behavior, encode it in:

- the target agent profile
- the tool/MCP capability
- routing/tag Markdown
- the target payload

not in scheduler code.
