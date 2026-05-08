# ADR-002 Planner Only Semantics

## Status

Accepted

## Context

Honeycomb's product direction is task-first and should not depend on a fixed
hard-coded team. At the same time, the current codebase benefits from a
predictable bootstrap path for testing and demos.

The system needs a precise meaning for `planner_only` so bootstrap, routing,
and assignment do not drift apart.

## Decision

`planner_only` means:

- bootstrap materializes exactly one runtime agent: the planner
- no default worker pool is created
- no reviewer is created unless a later plan or rule requires it

The existing fixed trio `planner/worker/reviewer` remains valid only as a
scaffold preset for demos and tests. It is not the default product bootstrap
mode.

### Planner Responsibilities

The planner may:

- create or update a `TaskPlan`
- create implicit or explicit `WorkItem` records
- propose when a new execution agent should be materialized
- propose when review is required
- emit outward summaries when acting as the task's public voice

The planner may not:

- silently become the default execution worker for every task
- silently bypass assignment rules once another execution candidate exists
- create unbounded numbers of agents

### Planner And Assignment

In P0, the planner proposes work structure, but assignment resolution is rule
driven and separate from planner reasoning.

This separation avoids making the planner both:

- the author of the work breakdown
- and the final arbiter of who wins each unit of work

### Relationship To Existing Nomination

- `L1` keeps the current message-level nomination path
- once a request enters `L2` or `L3`, work-item claim/assign becomes the
  primary collaboration path
- message-level nomination does not decide ownership for work items

### Agent Materialization Caps

Planner-triggered materialization must be capped in P0:

- per task hard cap: `max_agents_per_task = 4`
- per planning update hard cap: `max_new_agents_per_round = 2`

These caps are product policy, not provider policy. They protect:

- latency
- token budget stability
- deterministic testing
- accidental planner over-expansion

If a cap is hit, the planner must record a visible note rather than silently
retrying.

## Consequences

Benefits:

- bootstrap semantics are precise
- default flow remains small and explainable
- existing trio-based demos can coexist without becoming product truth

Costs:

- planner-only tasks may require extra steps before execution agents appear
- some very small tasks may still need a direct `single_agent_execute` fallback

## Notes

- P0 may allow an explicit `single_agent_execute` path for small `L2` work when
  spawning a new execution agent would cost more than it saves
- this path is an `L2`-only degradation in P0 and is not the default execution
  mode for explicit `L3` task flows
- that path must remain visible in traces and must not be mistaken for
  planner-only bootstrap
