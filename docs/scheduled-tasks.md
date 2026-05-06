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
```

`dispatch-due` is restart-friendly: it first creates any due run records, then dispatches queued runs. `watch` is a thin polling loop over the same behavior and is intentionally not business-aware.

`hc-api` can run the same loop in-process when explicitly enabled:

```text
HC_SCHEDULER_ENABLED=true
HC_SCHEDULER_TICK_SECONDS=30
HC_TENANT_ID=local
HC_USER_ID=default
```

The API loop delegates to `hc-service::scheduler`; it does not contain domain-specific scheduling or routing rules.

## Design Rule

No schedule implementation should contain business-specific matching logic.

If a business flow needs special behavior, encode it in:

- the target agent profile
- the tool/MCP capability
- routing/tag Markdown
- the target payload

not in scheduler code.
