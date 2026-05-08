# ADR-003 Work Item Persistence And Idempotency

## Status

Accepted

## Context

Even a small work-item lifecycle across `planned`, `claiming`, `assigned`,
`blocked`, `done`, and `cancelled` is unreliable
without persistence, deduplication, and explicit blocked-exit rules.

Honeycomb needs a P0 contract for where work items live, how implicit work is
deduplicated, and how assignment is resolved without introducing a heavyweight
workflow engine.

## Decision

Work-item state is durable in P0.

### Persistence Boundary

The following records must be persisted through `hc-store`, under the task room
namespace rather than session-only memory:

- `TaskPlan`
- `WorkItem`
- `WorkItemClaim`
- `WorkItemAssignment`

CLI disconnect or process restart does not implicitly close a task. A task
continues to exist until it is completed, cancelled, or archived explicitly.

### State Model

P0 work-item states are:

- `planned`
- `claiming`
- `assigned`
- `blocked`
- `done`
- `cancelled`

`done` and `cancelled` are terminal states for purposes of coordination. An item in
`cancelled` is not eligible for reassignment unless explicitly replanned as a new
work item (out of scope for P0 state machine shortcuts).

Only one active assignment may exist for a work item at a time.

### Idempotency

Implicit work-item creation must be idempotent.

The dedupe key is:

- `(conversation_id, triggering_message_id, normalized_intent_hash, intent_hash_version)`

The first version is `intent_hash_v1`.

#### `normalized_intent_hash`

`normalized_intent_hash` is calculated from a normalized form of the triggering
message text.

For `intent_hash_v1`, normalization is:

1. trim leading and trailing whitespace
2. Unicode NFC normalization when available in the implementation layer
3. lowercase ASCII letters
4. collapse internal runs of whitespace to a single space
5. do not stem
6. do not apply heavy tokenization
7. preserve punctuation except for leading/trailing punctuation-only noise

The intent of `v1` is stability, not semantic cleverness. Future changes to
normalization must create a new version label instead of silently reusing
`intent_hash_v1`.

### Minimal Assign Algorithm

P0 assignment is rule based:

- if there is exactly one eligible claim, assign it
- if there are multiple eligible claims, choose the highest capability score
- if capability scores tie, choose the lower current workload
- if workload also ties, choose the earliest submitted claim in the current
  claim round
- if no claim clears the eligibility threshold, keep or return the item to
  `claiming`

The threshold policy itself may remain simple in P0, but the winner-selection
order must stay deterministic.

For P0, score inputs come from stable lightweight sources:

- `capability_score`: static capability/profile match scoring from the current
  agent/task metadata; if unavailable, treat as `0`
- `current_workload`: count of work items that have a **current active
  assignment** to this candidate agent **and** whose work-item state is
  **non-terminal**. For P0 tie-breaking, **terminal** means `done` or
  `cancelled` (both must **not** increase load; `cancelled` is not “still busy”).
  Equivalently: **non-terminal assigned work item count**. If unavailable, treat
  as `0`

P0 does not require a learned scoring system.

### Blocked Exit Rules

A blocked work item must carry:

- `blocked_reason`
- `blocked_at`

Blocked items may only leave `blocked` through:

- `user_resume`
- `planner_replan`
- `timeout_requeue`
- `manual_cancel`

### P0 transition targets

This subsection is authoritative for rollout and tests referencing “ADR-003 P0
transition targets”. Every exit from `blocked` must specify a deterministic next
state. Transitions must be observable in traces.

From `blocked`:

- `timeout_requeue` → `claiming`
- `manual_cancel` → `cancelled`
- Timeout policy that abandons work (no retry) → `cancelled`
- `user_resume` and `planner_replan` → `claiming` or `assigned` per product
  rule, recorded in traces; the orchestration layer must not treat these as
  implicit `done`

Implementations must document the chosen targets for `user_resume` /
`planner_replan` in code or a short appendix; divergent implementations between
workers are unacceptable.

### Threshold Progression

P0 does not require multi-round threshold lowering. If no claim clears the
eligibility threshold, the work item remains in `claiming` until:

- a new eligible claim appears
- the planner replans
- a timeout policy requeues or cancels the item

Round-based threshold lowering is a possible P1 behavior and must not be
assumed in P0 implementations.

## Consequences

Benefits:

- work-item coordination is restart-safe
- implicit task creation is deduplicated
- assignment remains small but deterministic

Costs:

- persistence code arrives earlier than a pure in-memory prototype
- future smarter intent normalization must respect version drift explicitly

## Notes

- P0 scoring inputs are fixed by the paragraphs above (`capability_score` and
  `current_workload`); changing their meaning requires an ADR revision, not ad
  hoc constants in one crate
- the winner-selection order must stay deterministic under test
- if a claim algorithm changes materially, that decision should be captured in a
  follow-up ADR rather than changing behavior silently
