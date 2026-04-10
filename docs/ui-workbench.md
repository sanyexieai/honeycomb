# Honeycomb UI Workbench

## Product Position

Honeycomb UI should be a complete desktop product, not a thin runtime demo shell.

This document should be read together with:

- [task-driven-product.md](/d:/code/honeycomb/docs/task-driven-product.md)
- [working-rules.md](/d:/code/honeycomb/docs/working-rules.md)

The primary shape is:

- task-first
- user-in-the-loop
- multi-agent aware
- single-workspace by default
- detachable multi-window as an enhancement, not the default skeleton

The closest mental model is not "multi chat windows". It is:

**an AI-native task workbench with agent collaboration panels**

## Core Principles

### 1. Task First

The main entry is a task, not a raw chat session and not a list of agents.

Users should open the UI into a workspace centered on:

- current task
- current phase
- active participants
- current outputs
- current risks or decisions

### 2. User Always In The Loop

The user is a first-class participant during the whole task, not just the initial requester.

The UI must make it easy for the user to:

- redirect the task
- interrupt or reprioritize
- address one agent directly
- comment on an intermediate result
- approve or reject a proposal
- add context, files, or constraints

### 3. Single Workspace By Default

The default experience should be one main window containing the full task workspace.

Do not start with many separate agent windows.

The main window should contain panels or views for:

- main task thread
- agents
- terminal
- memory and assets
- current details and inspection

### 4. Detached Windows Are Related Views

Detached windows are derived views of the same workspace.

They should:

- remain attached to the same task and session
- reflect the same runtime and orchestration state
- be re-attachable to the main workspace

Detached windows should not become separate products or separate sessions by default.

### 5. UI Mainly Sits Above `hc-agent`

The UI should primarily consume `hc-agent` concepts:

- task workspace
- materialized agents
- persona
- capability
- memory
- nomination and grant status

The UI may still inspect `hc-core` directly for low-level runtime details, but that should not be the main product model.

## Main Window Structure

The first product version should default to one main workspace window with the following structure.

### Top Bar

Purpose:

- show the current task
- show current namespace and workspace identity
- expose global actions

Suggested contents:

- task title
- tenant/user/project badge
- task phase
- quick actions: pause, reroute, summarize, detach panel

### Left Rail

Purpose:

- navigation for the current workspace
- awareness of what is active

Suggested contents:

- task outline
- active agents
- channels
- assets
- saved summaries or decisions

### Main Center Pane

Purpose:

- primary work thread
- where the user mostly reads and speaks

This is the main conversation and action stream.

It should contain:

- user instructions
- agent proposals
- important runtime actions
- selected system summaries
- approval or review prompts

This is not just a chat log. It is the primary task thread.

### Right Inspector

Purpose:

- context for the current selection

Depending on what is selected, it can show:

- agent details
- persona summary
- capability list
- memory hits
- nomination and speaking state
- task metadata

### Bottom Composer

Purpose:

- main user intervention point

The user should always have a unified place to:

- talk to the workspace as a whole
- target a specific agent
- add constraints
- request a tool action

## Required User Intervention Modes

The UI should support four user intervention modes from early versions.

### 1. Workspace-Level Intervention

The user talks to the whole task.

Examples:

- "Change direction"
- "Pause implementation and explain the plan first"
- "Focus on risk"

The system may route this to one or more agents, but the user should not need to manually route every message.

### 2. Agent-Directed Intervention

The user talks to a specific agent.

Examples:

- `@planner split this into phases`
- `@reviewer check security risk`
- `@coder do not edit files yet`

The UI should make this easy through direct selection, not only command syntax.

### 3. Result Review Intervention

The user can act on an intermediate output.

Examples:

- approve
- reject
- ask for revision
- pin as decision
- convert to task

### 4. Governance Intervention

The user can influence system behavior.

Examples:

- disable an agent from auto-claiming
- raise or lower autonomy
- require approval before external actions
- control broadcast or channel reply behavior

## Multi-Window Model

Multi-window should be modeled as detached panels, not independent app silos.

### Default State

Everything begins inside the main workspace window.

### Detachable Panels

Panels that may be detached:

- agent detail
- terminal
- logs
- memory document
- task inspector

### Rules

- detached panels stay in the same workspace
- detached panels keep the same task/session identity
- detached panels can be brought back into the main window
- detached panels should not create new runtime identity by default

## Panel Types

The first product design should recognize at least these panel types.

### `MainThreadPanel`

The main task thread and user intervention surface.

### `AgentBoardPanel`

A visual board of active agents, their roles, status, and claim behavior.

### `TerminalPanel`

Task-linked terminal or command output.

### `MemoryPanel`

Relevant memory, summaries, and durable notes.

### `InspectorPanel`

Selection-specific detail panel.

## What Agents Look Like In The UI

Agents should not default to becoming independent windows.

By default, they should appear as:

- cards
- rows
- compact panels
- status threads

Each agent card should be able to show:

- name
- role
- current status
- whether it is idle, claiming, speaking, or executing
- capability summary
- recent output

Only after user intent should an agent detail panel be expanded or detached.

## Nomination And Speaking Visibility

Honeycomb has a distinct collaboration model, so the UI should expose it clearly.

The user should be able to see:

- which message opened a nomination round
- which agents nominated themselves
- their confidence band or round
- who received speaking rights
- whether a reply was generated

This should appear as structured task activity, not as noisy low-level debug logs.

## Input Model

The main input bar should support three user intents.

### 1. Talk To Workspace

Default mode.

### 2. Talk To Specific Agent

Selected by mention or by UI target selection.

### 3. Trigger Tool Or Terminal Work

Explicitly chosen, not confused with ordinary natural-language input.

For desktop product quality, plain natural-language input should not be accidentally treated as a shell command.

Shell execution should be explicit.

## First Usable Version

The first product-grade version of the UI should include:

- one main workspace window
- task header
- main task thread
- agent board
- inspector
- unified input bar
- detachable terminal panel
- detachable agent detail panel
- visible nomination and grant state
- user ability to intervene at any time

It does not need, in the first version, to include:

- freeform layout system
- full docking engine
- visual workflow editor
- graph view of the whole system

## Implementation Guidance

Implementation should proceed in this order:

1. Define a workspace view model in `hc-agent`
2. Make `hc-ui` consume workspace and panel state from `hc-agent`
3. Keep direct `hc-core` reads thin and local to runtime inspection
4. Replace command-heavy shell interactions with explicit product actions where possible
5. Add detachable panel support after the single-window workspace feels complete

## Summary

Honeycomb UI should be:

- task-centered
- user-steerable at all times
- agent-aware
- single-workspace-first
- multi-window-capable through detachable related views

That is a stronger fit for Honeycomb than a pure chat app or a simple runtime shell.
