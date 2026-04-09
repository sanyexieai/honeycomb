# Markdown-First Storage

## Principles

- Markdown is the primary durable data format
- machine-oriented indexes are derived artifacts
- derived artifacts must be rebuildable from Markdown
- humans and agents should both be able to inspect and edit source data

## Workspace Data Layout

```text
workspace/
  sessions/     Session logs and summaries
  instances/    Instance definitions and snapshots
  decisions/    Architecture and product decisions
  indexes/      Rebuildable derived indexes
  attachments/  Binary assets referenced by documents
```

## Document Model

Each primary document should be a Markdown file with YAML frontmatter.

Example:

```md
---
id: session.bootstrap.0001
type: session
title: Bootstrap session notes
tags: [core, runtime]
status: active
created_at: 2026-04-09T10:00:00+08:00
updated_at: 2026-04-09T10:00:00+08:00
source: manual
relations:
  - type: references
    target: instance.shell.main.0001
---

# Summary

...
```

## Required Frontmatter Fields

- `id`
- `type`
- `title`
- `created_at`
- `updated_at`

Recommended fields:

- `tags`
- `status`
- `source`
- `relations`
- `owners`
- `capabilities`

## Data Tiers

### Tier 1: Source of truth

- Markdown documents
- binary attachments explicitly referenced from Markdown

### Tier 2: Derived query artifacts

- JSON indexes
- SQLite caches
- full-text search indexes
- embedding stores
- graph projections

Tier 2 artifacts must never become the only durable copy of knowledge.

## File Naming

Recommended file naming:

```text
<type>.<domain>.<slug>.<sequence>.md
```

Examples:

- `session.bootstrap.0001.md`
- `instance.shell.main.0001.md`
- `decision.storage.markdown-first.0001.md`

## Linking Rules

Preferred link styles:

- wiki-like semantic ids resolved by the store
- relative Markdown links for human portability

Examples:

- `[[instance.shell.main.0001]]`
- `[Storage Decision](../decisions/decision.storage.markdown-first.0001.md)`

## Events and Sessions

High-volume runtime data should not bloat primary Markdown files.

Recommended pattern:

- append runtime events to `workspace/indexes/events.jsonl`
- periodically summarize those events into session Markdown documents

## Storage Boundaries

`hc-store` owns:

- parsing
- validation
- indexing
- querying

`hc-memory` owns:

- extraction
- summarization
- memory scoring
- evolution policies
