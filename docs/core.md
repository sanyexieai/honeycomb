# Core Runtime Notes

## Goal

`honeycomb-core` should be usable without any graphical environment and should be capable of serving as a terminal replacement over time.

The first stage target is:

- shell-first
- multi-instance
- message-oriented
- non-blocking
- PTY-capable through dedicated workers

## Main Loop Rules

The main loop is a supervisor and router.

Early implementation direction:

- supervisor methods should gradually converge behind a command-driven dispatch surface
- both direct dispatch and queued command consumption are valid early runtime modes

It may:

- receive messages
- update records
- dispatch work
- process events

Event kinds should also converge to typed protocol values rather than free-form strings.

It must not:

- directly host PTY execution
- synchronously wait for long-running jobs
- continuously read process output inline

## Core Object Model

### `Window`

Pure view. Optional. Not part of the core runtime contract.

### `Instance`

Identity-bearing peer with a mailbox.

Early implementation note:

- mailbox can begin as a read model projected from stored messages before becoming a live queue

### `Channel`

Session-scoped discussion lane.

Early implementation direction:

- channels are first-class runtime records
- instances explicitly join or leave channels
- channel messages are only visible to subscribed instances

### `Worker`

Execution unit for jobs. Internal implementation detail from the communication perspective.

Workers should report back through a small typed protocol, for example:

- state changed
- stdout
- stderr
- exited

The first concrete worker can be a simple process worker for non-PTY jobs.

That process worker should prefer streaming stdout and stderr into reports instead of waiting for full buffered completion.
It should also be able to return a handle for longer-running process jobs so the runtime can poll, wait, or terminate it.

### `Job`

Running work item attached to an instance and executed by a worker.

Early implementation note:

- job lifecycle should use typed states rather than free-form strings

### `RunRequest`

Describes how a job should be launched.

Important field:

- `run_mode`: `process`, `pty`, or `auto`

The supervisor may also derive a worker plan from a run request:

- job kind
- worker kind
- child-instance disposition

### `Child Instance`

A promoted instance created when a task needs independent identity and direct communication rather than just execution.

## Decision Rules

## Communication Modes

The core runtime should support three routing modes:

- direct message: one instance to one instance
- broadcast: one instance to all instances in the session
- channel message: one instance to all subscribed instances in a session channel

Early implementation rule:

- direct routing uses `to`
- broadcast uses neither `to` nor `channel`
- channel routing uses `channel` and requires membership

### When work stays a job

- it only needs execution
- it does not need direct messaging
- it can be represented as status plus event stream

### When work becomes a child instance

- it needs direct first-class messaging
- it needs long-lived independent context
- it should be represented as a conversational peer

Early runtime actions should include:

- submit a run request
- update job state
- append job-originated events
- promote a qualifying job into a child instance

## Persistence Guidance

Recommended early persistence split:

- Markdown for sessions, instance definitions, and summaries
- JSONL for high-volume runtime events
