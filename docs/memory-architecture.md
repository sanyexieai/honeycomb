# Honeycomb Memory Architecture

## Purpose

Honeycomb memory should not be modeled as a single flat list of notes.

It should support:

- short-lived conversation context
- reusable task knowledge
- long-lived topic accumulation
- project-level conventions
- user or tenant global memory

This document defines a layered memory model that keeps Markdown as the durable source of truth while allowing different products and crates to retrieve the right level of memory without hard-coding an agent-specific flow.

## Principles

- Markdown remains the durable canonical format
- memory is layered, not flat
- rooms are memory containers, not only chat transcripts
- raw and derived documents both matter
- every derived memory artifact must point back to source documents
- retrieval should prefer nearby memory before broad global memory
- `hc-memory` owns memory structure and visibility, not provider logic
- `hc-context` and `hc-agent` may consume the same memory room model

## Layers

Honeycomb memory should distinguish at least these layers:

### `chat`

Purpose:

- one concrete conversation or exchange
- strongest local context
- shortest lifetime

Examples:

- one terminal-assisted discussion
- one review conversation
- one user + agent exchange thread

### `topic`

Purpose:

- durable knowledge around one subject
- can span many chats and tasks

Examples:

- streaming responses in Rust
- assignment policy
- workspace indexing

### `task`

Purpose:

- memory attached to one actionable task
- can include planning, execution, decisions, and outcomes

Examples:

- implement persisted task assets
- add streaming support to `hc-llm-cli`

### `project`

Purpose:

- memory shared across many tasks in one project
- conventions, architecture, module boundaries, glossary

Examples:

- Honeycomb runtime design rules
- storage layout conventions

### `global`

Purpose:

- long-lived user or tenant memory
- stable preferences and general reusable knowledge

Examples:

- preferred output style
- stable writing rules
- long-lived domain facts

## Retrieval Priority

Retrieval should usually prefer memory in this order:

1. `chat`
2. `task`
3. `topic`
4. `project`
5. `global`

This keeps local context strong while still allowing fallback to wider memory.

## Memory Rooms

A memory room is the main container for related memory assets.

Examples:

- `room.chat.20260420.0001`
- `room.topic.rust-streaming.0001`
- `room.task.honeycomb-memory-refactor.0001`
- `room.project.honeycomb.0001`
- `room.global.user-default.0001`

Rooms should use a shared internal structure, regardless of layer.

## Recommended Room Directory Layout

```text
workspace/
  tenants/
    <tenant-id>/
      users/
        <user-id>/
          memory/
            rooms/
              chat/
                room.chat.20260420.0001/
                  room.md
                  raw/
                  compressed/
                  literary/
                  derived/
                  facts.md
                  timeline.md
                  entities.md
                  relations.md
              task/
                room.task.runtime-refactor.0001/
              topic/
                room.topic.assignment-policy.0001/
              project/
                room.project.honeycomb.0001/
              global/
                room.global.user-default.0001/
```

## Required Room Assets

Each room should have these conceptual assets:

### `room.md`

Purpose:

- room identity
- room summary
- layer
- status
- related task/topic/project refs
- pointers to room assets

### `raw/`

Purpose:

- original material
- user prompts
- transcripts
- observations
- source excerpts

### `compressed/`

Purpose:

- minimal summaries
- compact recall documents
- key facts
- decision digests

This is the preferred retrieval target for `hc-context`.

### `literary/`

Purpose:

- stylistic rewrites
- alternate rhetorical forms
- for example, classical Chinese or audience-specific versions

These are derived expression documents, not primary fact sources.

### `facts.md`

Purpose:

- compact stable factual inventory
- easy machine retrieval

### `timeline.md`

Purpose:

- ordered room events
- progression through planning, assignment, execution, review, and consolidation

### `entities.md`

Purpose:

- related people, agents, crates, tasks, sessions, projects, and topics

### `relations.md`

Purpose:

- explicit room-level and entity-level links

## Entity And Tag Guidance

Memory should not rely only on free-form tags.

Use three levels:

1. tags
2. related entities
3. explicit relations

### Tags

Use for broad filtering:

- `runtime`
- `assignment`
- `streaming`
- `review`

### Related Entities

Use stable ids:

- `user.default`
- `agent.planner`
- `crate.hc-core`
- `task.demo`
- `topic.assignment-policy`

### Relations

Use explicit directional links:

- `belongs_to`
- `about`
- `references`
- `derived_from`
- `summarizes`
- `aggregates`

## Suggested `room.md` Frontmatter

```md
---
id: room.task.runtime-refactor.0001
type: memory_room
title: Runtime Refactor Task Room
tenant_id: local
user_id: default
layer: task
room_kind: implementation
status: active
task_ref: task.runtime-refactor.0001
project_ref: room.project.honeycomb.0001
topic_refs:
  - room.topic.runtime-architecture.0001
tags:
  - runtime
  - refactor
  - assignment
related_entities:
  - type: user
    id: user.default
  - type: agent
    id: agent.planner
  - type: crate
    id: crate.hc-core
source_docs:
  - raw/doc.0001.user-request.md
  - raw/doc.0002.repo-scan.md
derived_docs:
  - compressed/task/plan/task-plan.summary.md
  - compressed/task/plan/assignment-decision.planner.001.md
  - compressed/min.0001.summary.md
  - literary/wenyan.0001.md
created_at: 2026-04-20T10:00:00+08:00
updated_at: 2026-04-20T12:00:00+08:00
---
```

Task room **compressed** assets written by **`persist_task_artifacts`** (`hc-agent`) use the ADR-005 **`task/plan/`** prefix (e.g. `task-plan.summary.md`, `assignment-decision.*.md`); older examples may show flat `compressed/min.*.md` filenames for topic/chat rooms.

## Relationship Model

Rooms should be linkable across layers.

Examples:

- a chat room `belongs_to` a task room
- a task room `about` a topic room
- a task room `belongs_to` a project room
- a project room `references` a global room

This means Honeycomb memory should behave like a layered graph, not just a folder of notes.

## Crate Responsibilities

### `hc-memory`

Should own:

- layered memory data model
- room metadata
- room relations
- visibility and namespace rules
- room repository layout rules

Should not own:

- provider prompts
- LLM transport
- agent-specific workflow policy

### `hc-context`

Should own:

- retrieval priority across layers
- memory recall selection
- context composition for model calls
- pluggable memory organization strategies for routing, tagging, classification, and promotion
- composition of prompt policy, prompt assets, recalled memory, and user input into one request

Should not own:

- durable storage rules
- room repository ownership

### `hc-agent`

May own:

- when to consult memory
- which task/topic/project/global rooms matter in a workflow
- whether to persist new task outcomes into rooms

But it should reuse the same room model defined in `hc-memory`.

## Pluggable Organization Strategy

Memory organization should not be hard-coded to either rules or LLMs.

Use replaceable strategy interfaces so the system can start rule-first and later add LLM-assisted enrichment without changing storage or callers.

Recommended strategy slots:

- `MemoryRoomRouter`: decides which room and layer a memory should enter
- `MemoryKindResolver`: decides whether a note is a summary, decision, preference, knowledge, or workflow memory
- `MemoryTagSuggester`: proposes normalized tags
- `MemoryPromotionAdvisor`: suggests whether a memory should later be promoted to topic, project, or global layers

These strategies can be composed into one organizer pipeline.

Examples:

- `RuleBasedMemoryRoomRouter`
- `KeywordMemoryTagSuggester`
- `LlmMemoryRoomRouter`
- `LlmMemoryPromotionAdvisor`
- `HybridMemoryOrganizer`

The key boundary is:

- `hc-memory` stores the result
- `hc-context` owns the organization policy plug points
- callers such as `hc-context-cli` and `hc-agent` choose which strategy implementation to use

## Prompt As Compiled Memory

Prompt material should not be modeled as a foreign system outside memory.
It is better treated as a later-stage, execution-oriented form of memory.

Use three distinct runtime inputs when composing a model request:

- hard runtime policy
- compiled prompt assets
- recalled memory

In storage and lifecycle terms, prompt assets should be derived from the same broader memory graph:

- raw memory captures source material
- extracted memory captures structure
- generalized memory captures stable reuse
- compiled memory captures directly injectable guidance

This keeps prompt execution and memory provenance aligned without forcing runtime composition into one flattened blob.

See also: `docs/memory-prompt-unification.md`

## Self Model Layer

Honeycomb should also keep a separate self-model layer.

This layer is not the same as memory and not the same as prompt policy.

Use it for:

- current role
- identity
- capability summaries
- operating constraints
- stable working goals

Examples:

- planner persona
- reviewer role
- tool-use boundaries
- collaboration limits

Recommended separation:

- memory answers: "what do I know or remember?"
- prompt policy answers: "what should I do in this run?"
- self model answers: "who am I, what can I do, and what should I avoid?"

At runtime, `hc-context` should compose these as separate sections instead of flattening them into one undifferentiated prompt blob.

In the current crate layout, a practical default source is:

- `hc-persona` -> role, identity, style, goals
- `hc-capability` -> capability summaries and constraints

These can be bridged into one `SelfModel` before context composition.

## Recommended Next Implementation Steps

1. add `MemoryLayer`, `MemoryRoom`, and `MemoryRelation` to `hc-memory`
2. add room-aware repository support under `memory/rooms/<layer>/...`
3. make `hc-context` prefer `compressed/` room documents during recall
4. later let `hc-agent` query the same room structures rather than creating its own memory shape
