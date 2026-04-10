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

- lower the threshold by round
- if still nobody fits, create a new seed agent

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
