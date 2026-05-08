# ADR-004 Conversation Task Room Scope

## Status

Accepted

## Context

Honeycomb must stay conversation-first for users while still isolating task
state, work results, and memory retrieval.

Without explicit scope rules:

- one conversation may leak artifacts from one task into another
- summaries may pull the wrong task context
- room boundaries may become user-visible clutter or internal inconsistency

## Decision

Honeycomb keeps separate but linked scopes for conversation, task, and room.

### Core Relationship

- one `Conversation` may relate to many tasks over time
- one `Task` has one primary conversation thread
- one `Task` owns one default task room

P0 does not introduce a second user-visible room type for work items.
Complicated work items may later graduate into their own room, but the default
is one task room per task.

### Active Task Binding

At any moment, an incoming message may have:

- no task binding, for pure `L1` conversation
- or exactly one `active_task_id`

Routing should prefer reusing the current active task when the new message is
still about that task. A new implicit task may be created only when:

- no active task exists
- or the active task is clearly unrelated and the routing rules demand tasking

### Observability (task binding)

This subsection is authoritative for rollout and tests that reference “ADR-004
Observability (task binding)”. Implementations must expose the same facts in
`hc-protocol` types and traces.

Task activation and implicit task creation must emit a structured **task binding
decision record** on the same inbound user message pipeline as tier routing,
using the **same rule version family** as `routing_rule_version` in ADR-001 so
tier logs and binding logs can be joined without ambiguity.

The record must be available in trace output and tests. P0 minimum fields
(names are normative for `hc-protocol`; enum variants for `task_binding_action`
are defined there):

| Field | Meaning |
| --- | --- |
| `active_task_id` | Effective task id after the decision; use empty or a dedicated sentinel for “no active task” per protocol convention (must be unambiguous in traces) |
| `task_binding_action` | What the orchestrator did (e.g. reuse, create implicit task, clear binding) |
| `task_binding_reason` | Human-readable or structured reason code for tests and support |
| `task_binding_signals` | Rule hits / features that drove the decision (same idea as `routing_signals` in ADR-001) |
| `task_binding_rule_version` | Revision id in the **same family** as `routing_rule_version` (ADR-001); may equal it or pair with it, but must be documented so two logs sort into the same policy release |

### Retrieval And Summarization

Scope rules:

- `L1`: retrieve from recent conversation context only
- `L2/L3`: retrieve from the bound `task_id` first, then nearby conversation
  messages if needed
- task artifacts from unrelated tasks must not be injected into the current
  answer path by default

### Room Layout Rule

`task room` means one room per task id, not one room per session or one room per
conversation.

The room path and retrieval filters must be task-scoped so parallel tasks in the
same conversation do not mix:

- room id authority: `task_id`
- retrieval default filter: `task_id`
- summarization default filter: `task_id`

## Consequences

Benefits:

- users keep one readable conversational story
- task execution stays isolated
- memory recall becomes safer under parallel task load

Costs:

- task-binding logic must stay explicit
- some conversation-only features cannot blindly reuse task-scoped memory

## Notes

- this ADR intentionally avoids adding a user-visible work-item room in P0
- if work-item rooms are introduced later, they must define authority and merge
  rules explicitly rather than inheriting them implicitly
