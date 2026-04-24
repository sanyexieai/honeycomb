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
- Markdown content should stay readable on its own.
- Structure belongs to room layout and metadata, not to the body text of every `.md` file.

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

## Object Model

Honeycomb memory should follow an object-store style split:

- room path and subdirectories define structure
- Markdown body stores only meaningful human-readable content
- sidecar metadata stores retrieval and ownership data
- index files support fast lookup without polluting content files

This is intentionally close to object storage design:

- room directory ~= bucket or prefix
- `*.md` ~= object body
- `*.meta.json` ~= object metadata
- markdown index ~= catalog for discovery and retrieval

## Room Layout

```text
room.<layer>.<slug>/
  room.md
  raw/
    0001.user-message.md
    0001.user-message.meta.json
  compressed/
    0001.assistant-reply.md
    0001.assistant-reply.meta.json
  prompt/
    prompt.language-style-guide.md
    prompt.language-style-guide.meta.json
  literary/
    0001.assistant-wenyan.md
    0001.assistant-wenyan.meta.json
```

## Content Rules

### `room.md`

Keep room-level structure here:

- room identity
- layer
- status
- room summary
- related entities
- relations
- source and derived document references

Preferred shape:

```md
# Chat Room

Rolling local conversation window for one active exchange.

## Manifest

- room: room.chat.local.default.1776700677144
- layer: chat
- status: active

## Objects

- source: raw/0004.user-message.md
- derived: compressed/0004.assistant-reply.md
- derived: literary/0004.assistant-wenyan.md
```

### Asset Markdown

Asset Markdown should contain only the content itself.

Examples:

- raw user message text
- compressed assistant reply
- literary rewrite
- compiled prompt memory
- fact list
- timeline notes

Managed prompt templates may still be edited through local `prompts/**/*.md` files, but they are also mirrored into `room.project.prompt-library` as compiled prompt assets so they participate in room indexing and recall.

Do not include repeated structural sections such as:

- owners
- derived-from lists
- source-doc lists
- retrieval tags
- synthetic headers that add no meaning

### Sidecar Metadata

Each asset may have a same-name `*.meta.json` sidecar for metadata that retrieval needs but humans do not want mixed into the body:

- `id`
- `title`
- `room_id`
- `layer`
- `asset_kind`
- `memory_kind`
- `visibility`
- `tags`
- `owners`
- `derived_from`
- `source_docs`

## Naming Rules

File names should be meaningful without requiring frontmatter.

Prefer names like:

- `0001.user-message.md`
- `0001.assistant-reply.md`
- `0001.assistant-wenyan.md`
- `decision.assignment-policy.md`
- `fact.streaming-contract.md`

Avoid names that are only technical counters when a semantic label is available.

## Retrieval Rules

Retrieval should prefer:

1. room structure from the path
2. sidecar metadata for filtering and ownership
3. Markdown body for semantic matching

The body should not be forced to carry system structure just to satisfy indexing.

## Implementation Status

Implemented now:

- active chat room reuse
- rolling chat room window
- global preference promotion
- optional background memory workers
- plain-content room asset Markdown
- sidecar metadata for room assets

Still needed:

- first-class agent rooms
- first-class tool rooms
- first-class task rooms
- archive summarization into facts, timeline, entities, and relations
- promotion rules that route abstract tags to higher layers
