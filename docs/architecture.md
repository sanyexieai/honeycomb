# Honeycomb Architecture

## Overview

Honeycomb is split into runtime, orchestration, and workbench layers that cooperate through stable protocols, Markdown documents, CLI entry points, and local APIs.

For product-flow and implementation guardrails, also read:

- [task-driven-product.md](/d:/code/honeycomb/docs/task-driven-product.md)
- [working-rules.md](/d:/code/honeycomb/docs/working-rules.md)

The system should avoid becoming a tightly coupled super-app. Each part must be:

- independently testable
- replaceable behind a stable contract
- evolvable without forcing UI and runtime to collapse into one object model

## Product Layer

### `honeycomb-core`

Purpose:

- cross-platform command runtime
- instance communication
- non-blocking job orchestration
- shell-first operation without a GUI

Key rules:

- windows are views, not execution units
- instances communicate
- workers execute
- jobs never block the main loop
- PTY support is isolated behind dedicated workers

### `honeycomb-agent`

Purpose:

- turns user tasks into temporary collaborating agents
- binds runtime instances to persona, capability, memory, and LLM state
- coordinates claim, grant, reply, and incubation flows
- delegates concrete reply generation to pluggable backends instead of embedding provider logic

Non-goals:

- owning the low-level runtime
- replacing the storage layer
- acting as a provider transport

### `honeycomb-ui`

Purpose:

- optional graphical workbench above the agent layer
- maps agent-backed instances into windows or panels
- visualizes tasks, agents, messages, sessions, jobs, and persisted assets
- keeps the user in the loop throughout task execution

Non-goals:

- owning execution logic
- owning job lifecycle semantics
- replacing orchestration rules

See also: [ui-workbench.md](/d:/code/honeycomb/docs/ui-workbench.md)

## Internal Crates

### `hc-protocol`

Purpose:

- shared data structures
- common traits
- protocol schemas
- event envelopes

### `hc-core`

Purpose:

- runtime model for sessions, instances, workers, jobs, messages, and events

Responsibilities:

- main loop and supervision
- message routing
- job records and worker assignment
- run mode classification
- child instance promotion rules

Non-goals:

- terminal rendering
- provider-specific AI behavior

### `hc-agent`

Purpose:

- task bootstrap
- temporary agent seeding
- runtime binding between instances and higher-level agent state
- incubation and promotion flow
- tenant/user-aware orchestration above the runtime
- bootstrap-time materialization of temporary persona and capability profiles
- persistence hooks for persona, capability, and incubation-memory assets
- emits task-scoped activity and decision traces through `hc-trace`

Non-goals:

- low-level runtime ownership
- provider transport
- Markdown storage ownership

### `hc-claim`

Purpose:

- participation claim protocol
- nomination round and threshold policy
- speaking-right selection rules

Non-goals:

- LLM-specific relevance scoring
- provider configuration
- runtime ownership

### `hc-llm`

Purpose:

- minimal provider-neutral LLM interface
- normalized chat request and response objects
- pluggable provider registry

Non-goals:

- agent orchestration
- memory policy
- UI ownership

### `hc-memory`

Purpose:

- memory records
- writeback targets for task and incubation results
- future recall and summarization layer

Non-goals:

- low-level runtime ownership
- provider transport

Key distinction:

- persona describes identity, role, and collaboration style
- memory captures what a role or session has learned, decided, or prefers
- tenant and user boundaries should be preserved in both ownership metadata and storage layout
- visibility should be modeled explicitly at the object level, not inferred only from path layout

### `hc-capability`

Purpose:

- capability profiles
- domain and skill declarations
- input and output contract hints
- namespace and sharing rules for reusable abilities

Non-goals:

- runtime ownership
- persona ownership
- provider transport

### `hc-store`

Purpose:

- Markdown-first storage
- metadata parsing
- indexing and querying
- derived cache generation

### `hc-trace`

Purpose:

- shared activity and decision trace objects
- agent code and behavior mode code rules
- stable observability surface for runtime, orchestration, and UI

Non-goals:

- owning runtime state
- provider transport
- UI ownership

### `hc-ui`

Purpose:

- Slint-based cross-platform desktop workbench
- primary user-facing shell over `hc-agent`
- optional thin inspection access to `hc-core`

## Core Runtime Model

The runtime is built from four first-class concepts and two supporting concepts.

### `Instance`

An instance is a communicative node with identity and a mailbox.

Instances:

- can send and receive messages
- can own multiple jobs
- can be rendered as zero or more windows
- may be promoted children of another instance

### `Worker`

A worker is an execution unit.

Workers:

- execute jobs outside the main loop
- are not directly communicative peers
- do not automatically become windows
- may be implemented as async tasks, threads, or child processes

### `Job`

A job is a running task owned by an instance and executed by a worker.

Rules:

- every job runs off the main loop
- long-running non-PTY jobs still run in workers
- PTY jobs are handled by dedicated PTY workers
- jobs emit events rather than blocking callers

### `Window`

A window is only a view.

Rules:

- windows never define runtime identity
- windows attach to instances, not workers
- a job does not become a window by default

### `Session`

A session scopes communication, state, and persistence.

### `Event`

Events are the runtime stream emitted by instances, workers, and jobs.

## Promotion Rules

Only long-lived execution that needs independent identity should be promoted from a job into a child instance.

Examples that usually stay jobs:

- builds
- file watchers
- long-running servers
- batch scripts

Examples that may become child instances:

- interactive shells
- REPL-style runtimes
- future LLM-backed peers
- any node that needs direct messaging as a first-class participant

## PTY Rules

- the main loop must not directly host PTY execution
- PTY execution is isolated behind dedicated PTY workers
- PTY use is decided per run request, not by command name alone
- run requests should support `process`, `pty`, and `auto` modes
- `auto` should bias toward `process` unless terminal semantics are clearly required

## Collaboration Model

Projects cooperate through three layers:

1. File conventions
2. CLI contracts
3. Local IPC or API protocols

The preferred early-stage order is:

1. Markdown files as source of truth
2. CLI interoperability
3. JSON over stdio, local sockets, or HTTP only after contracts stabilize

## LLM Discussion Model

When LLM-backed instances participate in a session, the runtime should avoid forcing every visible peer to answer.

Recommended model:

- direct messages target a specific peer
- broadcast and channel messages open a distributed self-nomination phase
- each expert instance evaluates its own fit
- the runtime grants speaking rights based on staged nomination thresholds

Threshold guidance:

- start with a high-confidence round
- if nobody nominates in time, lower the threshold
- repeat until a suitable speaker appears or all rounds are exhausted

This keeps the system decentralized while still preventing many experts from replying at once.

## Dependency Direction

Recommended dependency direction:

```text
hc-ui ---------> hc-agent -----> hc-core -------> hc-protocol
   |                |               |
   |                |               +-----------> hc-claim
   |                |
   |                +-------------> hc-trace
   |                +-------------> hc-capability
   |                +-------------> hc-memory
   |                +-------------> hc-persona
   |                +-------------> hc-llm
   |                +-------------> hc-store
   |
   +-------------------------------------------> hc-core   (thin runtime inspection only)

hc-llm --------> hc-protocol
hc-llm --------> hc-claim   (optional, when implementing LLM-based claim generation)
hc-agent ------> hc-core
hc-agent ------> hc-claim
hc-agent ------> hc-responder
hc-agent ------> hc-trace
hc-agent ------> hc-capability
hc-agent ------> hc-memory
hc-memory -----> hc-persona
hc-memory -----> hc-store
hc-agent ------> hc-store
hc-ui ---------> hc-responder
hc-ui ---------> hc-trace
hc-llm --------> hc-responder
```

More concretely:

- every crate may depend on `hc-protocol`
- `hc-claim` should stay thin and protocol-oriented
- `hc-core` depends on `hc-protocol`
- `hc-core` may depend on `hc-claim` for participation flow
- `hc-agent` is the orchestration layer above `hc-core`
- `hc-capability` describes what roles can do and how those capabilities may be shared
- `hc-memory` holds durable memory-oriented records
- `hc-memory` may reference persona ownership without taking runtime ownership
- `hc-store` should remain low-level and dependency-light
- `hc-ui` should primarily depend on `hc-agent`
- `hc-ui` may keep a thin direct dependency on `hc-core` for runtime inspection and low-level shell affordances

## Delivery Phases

### Phase 1

Build the smallest runnable core:

- `hc-protocol`
- `hc-core`
- `hc-store`

Expected outcome:

- start a runtime
- create sessions and instances
- route messages
- launch non-blocking jobs
- support basic run mode classification

### Phase 2

Add the first operator-facing shell:

- `hc-ui`

Expected outcome:

- inspect sessions and instances
- open command-oriented windows
- attach to running jobs
- provide an initial user workbench over `hc-agent`

### Phase 3

Add the orchestration and durable role/memory layer:

- `hc-agent`
- `hc-persona`
- `hc-memory`
- `hc-capability`

Expected outcome:

- task-driven agent bootstrap
- persona, capability, and memory persistence
- higher-level collaboration above the runtime

## Repository Strategy

Recommended strategy during incubation:

- single repository
- multiple crates and binaries
- shared versioned protocols
- independent release targets

This keeps iteration fast while preserving architectural independence.
