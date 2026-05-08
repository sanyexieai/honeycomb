# ADR-001 Task Routing Tier

## Status

Accepted

## Context

Honeycomb needs a narrow routing layer between raw conversation and heavier
task/work-item orchestration.

Without an explicit tier decision:

- simple questions may be over-expanded into workflow noise
- complex requests may be handled by a single reply path and lose quality
- later tuning is difficult because misroutes are not observable

## Decision

Honeycomb classifies each incoming user message into one of three routing tiers:

- `L1`: direct conversational reply
- `L2`: implicit micro-task behind the current conversation
- `L3`: explicit task flow with plan and one or more work items

The first implementation must prefer predictable rules over opaque model
judgment.

### P0 Routing Authority

- `L1` and `L2` are selected by rules only
- `L3` is entered only when:
  - the user explicitly requests planning, decomposition, or collaboration
  - or a hard rule marks the request as multi-step and high-risk
- no extra model call is allowed just to decide the tier in P0

### P0 Routing Signals

Examples of signals that may promote beyond `L1`:

- explicit planning language such as `plan this`, `break this down`, `handle as a task`
- multiple concrete deliverables in one request
- implement plus verify plus review in one request
- requests that imply longer-running execution with artifacts to persist

Examples of signals that should stay in `L1` by default:

- explanation-only questions
- one-shot factual or conceptual requests
- simple small edits where no decomposition or review is requested

### User Correction

User correction has priority over automatic routing.

Supported correction intents:

- force `L1`: examples include `do not split this`, `just answer directly`
- force `L3`: examples include `turn this into a task`, `plan this with steps`

Correction phrases must not be hard-coded inline in orchestration code.
They must be loaded from a small configurable phrase list owned by
`hc-agent`, so wording and localization can evolve without changing routing
logic. Phrase content does not change routing semantics; it only maps user text
to `force_l1` or `force_l3`.

### Observability

Every routed message must emit a decision record with at least:

- `routing_tier`
- `routing_reason`
- `routing_signals`
- `routing_forced_by_user`
- `routing_rule_version`

This record must be available in trace output and test fixtures.

Task activation and implicit task creation decisions must use the same rule
version family as `routing_rule_version` so routing logs and task-binding logs
can be compared directly.

## Consequences

Benefits:

- misroutes are debuggable
- the system can stay chat-first while still opening task paths
- tuning can begin with deterministic fixtures

Costs:

- some requests that deserve `L3` will remain in `L1` until rules mature
- phrase-list maintenance becomes a small operational concern

## Notes

- `L2` exists to preserve a single conversational story while allowing a hidden
  work item to execute
- `L3` is intentionally gated; P0 should not auto-open large swarm flows often
