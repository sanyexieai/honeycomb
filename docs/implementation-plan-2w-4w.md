# Honeycomb 2-Week / 4-Week Implementation Plan

## Purpose

This document turns the current architecture direction into an execution plan that starts from the lowest-dependency crates and moves upward only after the lower layer is stable enough to support it.

The plan is intentionally incremental:

- week 1-2 focuses on durable foundations and shared contracts
- week 3-4 focuses on wiring those foundations into orchestration and UI flow

This sequencing follows the existing repository guidance in [README.md](/D:/code/honeycomb/README.md), [docs/architecture.md](/D:/code/honeycomb/docs/architecture.md), [docs/storage.md](/D:/code/honeycomb/docs/storage.md), and [docs/ui-workbench.md](/D:/code/honeycomb/docs/ui-workbench.md).

## Current Assessment

What is already in good shape:

- workspace structure is clear and consistent
- `hc-core`, `hc-agent`, `hc-memory`, `hc-capability`, and `hc-persona` already have meaningful domain models and tests
- `hc-ui` already demonstrates a runnable workbench prototype
- the workspace test suite currently passes end-to-end

What is still incomplete:

- `hc-protocol` is still too thin for the role described in the architecture docs
- `hc-store` does Markdown parsing and writing, but derived indexing/query capabilities are still minimal
- task plans, assignment decisions, and trace outputs are not yet first-class durable assets
- `hc-trace` is mainly a view/helper layer, not a persisted observability layer
- `hc-ui` is still closer to a prototype shell than a product-grade single-workspace workbench

## Delivery Order

Implementation order should remain:

1. `hc-protocol`
2. `hc-store`
3. `hc-core`
4. `hc-responder` and `hc-llm`
5. `hc-persona`, `hc-capability`, `hc-memory`
6. `hc-agent`
7. `hc-ui`

This is not just architectural preference. It reduces rework:

- the protocol layer defines shared contracts
- the store layer makes those contracts durable and queryable
- runtime and orchestration can then persist decisions rather than keeping them only in memory
- UI can consume stable workspace views rather than owning product logic itself

## Two-Week Plan

### Sprint Goal

Create a stable durable-data base for task-driven workflows.

### Scope

- formalize more reusable shared schemas in `hc-protocol`
- add rebuildable Markdown indexing and query support in `hc-store`
- define and start using durable task-plan, assignment, and trace document shapes
- keep all changes test-backed and compatible with the current runnable workspace

### Work Items

1. Expand `hc-protocol` with shared document metadata and relation shapes used across storage-oriented crates.
2. Add namespace-scoped Markdown index rebuilding and querying to `hc-store`.
3. Define durable document conventions for:
   - task plans
   - assignment decisions
   - trace records
4. Start persisting at least one of those artifacts through the agent layer.
5. Add tests for index rebuild, query filters, and persisted task asset round-trips.

### Acceptance Criteria

- `hc-store` can rebuild a namespace index entirely from Markdown source documents
- index artifacts live under rebuildable `indexes/` paths
- at least one orchestration artifact beyond persona/capability/memory is persisted durably
- tests cover both source Markdown and derived index behavior

## Four-Week Plan

### Overall Goal

Turn the current prototype into a more product-shaped task workbench with durable planning and visible decision flow.

### Scope

- complete the durable asset path started in the two-week plan
- make `hc-agent` the primary workspace-model producer
- reduce `hc-ui` ownership of orchestration details
- keep responder mode and assignment decisions explicit and visible

### Work Items

1. Persist task plans, assignment decisions, and trace records through `hc-agent`.
2. Add retrieval/query helpers needed by persona, capability, and memory reuse.
3. Improve `hc-llm` and `hc-responder` boundaries so responder mode is explicit and reusable outside the UI.
4. Refactor `hc-ui` to consume more workspace/panel state from `hc-agent` view models.
5. Move toward the documented single-workspace-first UI model, while keeping detachable windows as a later enhancement rather than the primary shape.

### Acceptance Criteria

- a task can be created, planned, assigned, and partially persisted without relying on UI-only state
- task and decision artifacts are inspectable from the workspace directory
- `hc-agent` owns the main workspace projection used by UI
- `hc-ui` remains runnable but carries less orchestration policy locally

## Week-by-Week Breakdown

### Week 1

- add `hc-store` derived index and query support
- add tests for namespace-scoped indexing
- document durable task asset conventions

### Week 2

- add first durable task-plan or assignment artifact path in `hc-agent`
- connect index rebuild/query to the new asset path
- verify with tests and a sample workspace flow

### Week 3

- expand persistence to assignment decisions and trace records
- improve retrieval APIs used by memory/persona/capability reuse
- keep orchestration decisions visible rather than implicit

### Week 4

- refactor `hc-ui` around agent-produced workspace views
- reduce direct low-level product logic in UI where practical
- tighten the path toward the single main workspace model

## Immediate Execution

The first implementation task should be:

**add rebuildable Markdown indexing and query support to `hc-store`**

Why this goes first:

- it has the smallest dependency surface
- it is directly called for in [docs/storage.md](/D:/code/honeycomb/docs/storage.md)
- later persistence work in `hc-agent`, `hc-memory`, and `hc-ui` benefits from it immediately

## Risks And Guardrails

- do not add silent fallback behavior while wiring persistence or responder flow
- do not move product policy into `hc-core`
- do not let `hc-ui` become the only owner of task state
- keep derived indexes rebuildable from Markdown source-of-truth files

## Definition Of Done For This Iteration

This iteration is done when:

- this plan exists in the repo
- the first low-dependency task is implemented
- related tests pass
- the next task can build on a stable lower-layer API rather than another prototype shortcut
