# Memory House

## Purpose

Honeycomb should not create a brand-new chat room for every CLI launch, and it should not store every abstract idea inside chat rooms.

The memory house model separates:

- transient conversation rooms
- durable higher-layer rooms
- derived artifacts produced from rooms

This keeps the workspace readable while reducing redundant memory files.

## Principles

- A chat room is a rolling local window, not a permanent bucket for everything.
- Reuse the current active chat room when possible.
- Roll a chat room only when the local window is full or the room is explicitly closed.
- Abstract tags such as people, capabilities, tools, and recurring tasks belong to higher-level rooms.
- Promotion should move memory upward instead of duplicating the same idea in every chat room.

## Layers

### Chat

Use for:

- one active conversational window
- raw turns
- compressed turn summaries
- optional literary variants

Do not use for:

- durable agent identity
- stable tool descriptions
- long-lived task facts
- project-wide conventions

### Global

Use for:

- user preferences
- durable assistant naming
- stable response style preferences
- cross-session facts that are not tied to one task

Examples:

- user prefers the assistant to be called Xiao Ba
- user prefers concise Chinese responses

### Agent

Use for:

- stable persona and role memory for one agent
- capabilities and operating boundaries
- recurring habits of a named agent

Examples:

- planner specializes in decomposition
- reviewer focuses on regressions and tests

### Task

Use for:

- one concrete unit of work
- decisions, progress, blockers, outputs

Examples:

- implement chat room rollover
- debug MiniMax provider configuration

### Tool

Use for:

- durable facts about tools and integrations
- invocation conventions
- known failure modes

Examples:

- MiniMax OpenAI-compatible base URL rules
- git workflow constraints

### Project

Use for:

- repository-wide architecture and conventions
- module boundaries
- glossary and operating rules

Examples:

- Honeycomb memory layering rules
- workspace layout conventions

## Routing Rules

### Keep in chat room

Store only local conversational material in the active chat room:

- raw user and assistant turns
- compressed assistant replies
- local derived assets for that room

### Promote upward

Promote memory out of chat when it is:

- durable across sessions
- reusable outside the current conversation
- attached to a named higher-level entity

Examples:

- naming preference -> global
- recurring planner behavior -> agent
- stable tool usage rule -> tool
- implementation decision for one issue -> task

## Lifecycle

1. Reuse the latest active chat room by default.
2. Append turns to that room until the local window is full.
3. Flush pending derived memory work.
4. Mark the room as archived.
5. Create the next active chat room.
6. Preserve durable knowledge by promoting it into higher-level rooms instead of copying it into the next chat room.

## Implementation Status

Implemented now:

- active chat room reuse
- rolling chat room window
- global preference promotion
- optional background memory workers

Still needed:

- first-class agent rooms
- first-class tool rooms
- first-class task rooms
- archive summarization into facts, timeline, entities, and relations
- promotion rules that route abstract tags to higher layers
