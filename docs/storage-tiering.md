# Storage Tiering

Honeycomb uses three storage tiers. The boundary is architectural, not
implementation-specific.

## Tier 1: Files

Files are the source of truth for human-editable logic and durable knowledge.

Use Markdown or sidecar JSON for:

- agent and domain profiles
- prompts, routing rules, rendering rules
- MCP server declarations and rebuildable tool caches
- memory rooms, summaries, facts, timelines, and relations
- declarative schedules and conversation policies
- design decisions and project documentation

These files should remain inspectable, portable, and versionable. Runtime indexes
may accelerate access, but they must be rebuildable from these files.

## Tier 2: Database

Databases are for runtime state that needs concurrency, transactions, locks,
pagination, or status-machine updates.

Use a database for:

- high-volume conversation turns in API deployments
- scheduler run state, retries, locks, and queued work
- proactive inbox events, follow-ups, and proposal delivery state
- MCP invocation history and idempotency records
- user, tenant, device, and authorization state
- business entities that require transactional consistency

The database should store operational truth, not editable agent behavior.

## Tier 3: Vector Store

Vector stores are semantic indexes. They are not the only durable copy of any
knowledge.

Use a vector store for:

- memory recall over compressed room assets and facts
- semantic routing over agent, domain, prompt, and tool descriptions
- MCP tool capability discovery
- long-term preference and knowledge retrieval
- large documentation or knowledge-base snippets

Every vector record should include:

- `id`
- `source_path` or `source_id`
- `tenant_id`
- `user_id`
- `metadata`
- index version or generated timestamp

The source content should remain in files or a database and the vector index
should be rebuildable.

## Current Implementation

`hc-store` owns the first local index abstraction:

- `RebuildableIndex`
- `TextIndex`
- `VectorIndex`
- `LocalJsonVectorIndex`
- Markdown index projection helpers

The local JSON vector implementation is intentionally simple. It gives the rest
of the system a stable interface before choosing a production backend such as
SQLite vector extensions, pgvector, Qdrant, Milvus, or another managed vector
store.

## Rule Of Thumb

- Files answer: what is the system supposed to know or do?
- Databases answer: what is happening right now?
- Vector stores answer: what is semantically close enough to recall?
