# Honeycomb Task-Driven Product Flow

## Purpose

Honeycomb should start from a user task, not from a pre-existing chat with pre-existing agents.

The product's primary loop is:

1. user creates a task
2. system creates a planning agent for that task
3. planning agent decomposes the task into work items
4. existing agents self-nominate for work items
5. if no suitable agent exists, the system creates a new seed agent
6. LLM or user fills in the new agent's responder and role details
7. work proceeds
8. useful roles and results are persisted as durable assets

## Related Product Modes

In addition to the main task lifecycle, Honeycomb also needs a dedicated multi-turn channel discussion mode.

See:

- [channel-conversation.md](./channel-conversation.md)

That mode is for:

- user + agent discussion before assignment
- specialist channel threads
- multi-turn discussion inside a shared channel

It should not be treated as the same thing as the normal single-message nomination path.

## Primary Product Model

The primary product object is:

- `Task`

Not:

- raw chat session
- static agent roster
- shell-first workbench

## Required Runtime Roles

### 1. User

The user initiates tasks and remains a first-class participant throughout execution.

### 2. Planning Agent

Every new task should begin by creating a planning agent dedicated to that task.

Responsibilities:

- interpret the task
- propose phases
- propose work items
- propose needed agent roles
- identify missing capabilities

Responder options:

- `llm`
- `human`
- future: `rule`, `script`

If no LLM is configured, planning must still be possible through a human responder.

### 3. Execution Agents

Execution agents should not be assumed to exist in advance.

They may come from:

- already materialized task-scoped agents
- existing reusable personas/capabilities
- newly created seed agents

## Task Lifecycle

### Phase 1. Task Creation

User enters a task.

Expected output:

- `TaskDraft`
- task namespace
- initial user intent

### Phase 2. Planning

System creates a planning agent for the task.

Expected output:

- `TaskPlan`
- list of phases
- list of work items
- list of suggested roles
- list of missing capabilities or missing agents

### Phase 3. Assignment

For each work item:

- existing agents may self-nominate
- nomination and grant decide who should take it

If nobody is suitable:

- create a new seed agent
- attach provisional persona/capability metadata
- select responder mode

### Phase 4. Execution

Assigned agents execute the work and produce replies, artifacts, and decisions.

### Phase 5. Consolidation

Persist:

- persona candidates
- capability candidates
- memory
- traces
- decisions

## Agent Creation Rules

Agent creation should be task-scoped by default.

Do not assume a global hard-coded team such as:

- planner
- worker
- reviewer

unless a task plan explicitly calls for those roles.

Instead:

- planning proposes roles
- roles are matched against existing agents
- if unmatched, new seed agents are created

## Agent Suitability Rules

When assigning work:

- existing agents should self-nominate
- self-nomination should be claim-based
- claim resolution should select the best candidate for the current round

If no one clears the threshold:

- keep the item in claiming or replan according to the active assignment policy
- if the system later decides no existing agent fits, create a new seed agent

> **Note (P0 vs P1):** per [ADR-003](../adr/ADR-003-work-item-persistence-and-idempotency.md) rollout, **lowering the eligible / claim threshold across rounds** is tracked as **P1** (optional product evolution), not part of the minimal P0 assign rule set. P0 behavior is a **fixed** deterministic winner order and a single eligible bar per round unless replan changes the work item.

## New Agent Completion

Newly created seed agents may be completed by:

- an LLM responder
- a human responder

Completion should fill in at least:

- role
- persona basics
- capability refs
- responder binding
- initial behavior mode

## UI Expectations

The UI should reflect this flow explicitly.

Desired top-level sequence:

1. New Task
2. Planning
3. Assignment
4. Execution
5. Consolidation

The UI should not begin with a static default multi-agent chat unless that team was created by the current task flow.

Default user experience should still remain conversation-first.

This means:

- the main interaction surface may remain a conversation thread
- implicit `L2` work items may execute behind that thread without forcing the
  user into a task board
- explicit planning, assignment, and execution boards are primarily for `L3`
  flows or when the user expands task detail deliberately

## Persistence Expectations

The system should persist:

- task plans
- assignment decisions
- created seed agents
- promoted persona/capability assets
- memory
- traces

These should be tenant/user aware.

## Anti-Patterns

Avoid:

- starting from a fixed hard-coded team
- assuming an LLM is always present
- treating fallback responders as invisible implementation details
- using a static "chat with planner" as the main product loop

## Summary

Honeycomb should be:

- task-first
- planning-first
- assignment-driven
- agent-generating
- responder-agnostic
- persistence-oriented

## Decision Records

For the current narrow-path swarm collaboration rollout, also see:

- [Swarm P0 rollout checklist](./todo/swarm-p0-rollout.md) (implementation status vs ADR-001～005 narrow path)

For the same scope, formal ADRs are listed below.

- [ADR Index](./adr/README.md)
- [ADR-001 Task Routing Tier](./adr/ADR-001-task-routing-tier.md)
- [ADR-002 Planner Only Semantics](./adr/ADR-002-planner-only-semantics.md)
- [ADR-003 Work Item Persistence And Idempotency](./adr/ADR-003-work-item-persistence-and-idempotency.md)
- [ADR-004 Conversation Task Room Scope](./adr/ADR-004-conversation-task-room-scope.md)
- [ADR-005 Outward Speaker And Artifact Schema](./adr/ADR-005-outward-speaker-and-artifact-schema.md)
- [ADR-006 Experience Lane Boundary](./adr/ADR-006-experience-lane-boundary.md)

## Experience Boundary

Honeycomb may later adopt an ACE-inspired experience lane for outcome capture,
reflection, and curated playbook evolution.

That lane is not the default P0 execution path.

For the current rollout:

- main task execution remains conversation-first and task-scoped
- learning or playbook evolution must remain opt-in and post-task
- main completion must not depend on successful learning-side processing
