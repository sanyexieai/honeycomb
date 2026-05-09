# ADR-005 Outward Speaker And Artifact Schema

## Status

Accepted

## Context

Honeycomb may perform multi-step internal execution while still needing to feel
like one coherent assistant externally.

At the same time, the system needs a minimal artifact contract for plan,
execution, and review notes without building a heavyweight artifact platform.

## Decision

P0 defines both:

- a fixed outward-speaker policy
- a minimal shared artifact schema surface

## Outward Speaker Policy

### Public Voice

- `L1`: the executing path may answer directly
- `L2/L3`: the outward voice is the planner

In P0, the planner and consolidator are the same runtime identity. There is no
separate consolidator instance.

This means:

- planner may summarize one or more internal execution results
- users experience a stable voice across task execution
- no second public-facing role is introduced yet

### When To Consolidate

- if there is exactly one execution result and no review note, planner may
  lightly wrap and forward it
- if there are multiple execution results or any review note, planner must
  synthesize a consolidated outward reply

The additional synthesis call is accepted in these cases to preserve coherence.

## Minimal Artifact Schema

P0 recognizes only three artifact kinds:

- `plan_note`
- `execution_result`
- `review_note`

These artifacts are stored as task-room assets using agreed paths rather than a
new standalone artifact service.

Suggested path families:

- `task/plan/`
- `task/execution/`
- `task/review/`

### Shared Header

Artifact schema belongs in `hc-protocol`.

Each artifact must include at least:

- `id`
- `task_id`
- `work_item_id`
- `artifact_kind`
- `schema_version`
- `created_at`
- `producer`

The first shared schema version is `artifact_schema_v1`.

Payload-specific fields may differ by kind, but versioning is mandatory so old
task-room files and newer code can coexist without hidden drift.

For `artifact_schema_v1`:

- `work_item_id` is optional at the schema level
- `plan_note` may omit `work_item_id` for task-level planning artifacts
- `execution_result` should include `work_item_id`
- `review_note` should include `work_item_id`

## Consequences

Benefits:

- one stable outward speaker for task execution
- limited artifact sprawl
- a single protocol-level source of truth for artifact headers

Costs:

- planner becomes a visible bottleneck for external phrasing in `L2/L3`
- artifact migration concerns begin earlier, even with a small schema

## Notes

- this ADR does not require a full migration framework yet
- review-trigger policy remains controlled elsewhere; this ADR only defines how
  review output is surfaced once it exists
- current HTTP `L2/L3` implementation appends read-only task-room digests to
  system prompt in stable order: `execution_result` -> `plan_note` ->
  `review_note`
