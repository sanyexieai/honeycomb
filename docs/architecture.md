# Honeycomb Architecture

## Overview

Honeycomb is split into two product-facing projects that cooperate through stable protocols, Markdown documents, CLI entry points, and local APIs.

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

### `honeycomb-ui`

Purpose:

- optional graphical shell on top of the core
- maps instances into windows or panels
- visualizes messages, sessions, and jobs

Non-goals:

- owning execution logic
- owning job lifecycle semantics

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

### `hc-store`

Purpose:

- Markdown-first storage
- metadata parsing
- indexing and querying
- derived cache generation

### `hc-ui`

Purpose:

- Slint-based cross-platform desktop shell

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

## Dependency Direction

Recommended dependency direction:

```text
hc-ui ---------> hc-core -------> hc-protocol
                    |
                    +-----------> hc-store
```

More concretely:

- every crate may depend on `hc-protocol`
- `hc-core` depends on `hc-protocol`
- `hc-store` should remain low-level and dependency-light
- `hc-ui` depends on `hc-core` and `hc-protocol`

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

### Phase 3

Add optional higher-level capabilities as extensions over the runtime.

## Repository Strategy

Recommended strategy during incubation:

- single repository
- multiple crates and binaries
- shared versioned protocols
- independent release targets

This keeps iteration fast while preserving architectural independence.
