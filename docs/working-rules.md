# Honeycomb Working Rules

## Purpose

This document is the local implementation guardrail for Honeycomb work.

It should be checked before making structural product or architecture changes.

## Non-Negotiable Rules

### 1. Avoid Hard-Coding

Avoid hard-coding:

- fixed agent teams
- fixed provider names
- fixed model names
- fixed workflow routes
- fixed responder choices
- fixed task decomposition paths

Allowed only when:

- used as temporary local developer scaffolding
- clearly isolated
- explicitly marked
- easy to remove

### 2. Avoid Silent Fallback Chains

Do not hide fallback chains behind implicit logic.

Examples to avoid:

- silently switching provider A to provider B
- silently replacing LLM with rule mode
- silently choosing a default team
- silently routing a task to a fallback agent

Preferred behavior:

- explicit configuration
- explicit visible mode
- explicit trace or decision record

### 3. If a Fallback Is Truly Necessary, Stop and Notify

If a fallback must exist for product continuity, it must be:

- explicit in code
- explicit in UI or CLI output
- explicit in trace/decision records

And it should be surfaced to the user before being treated as acceptable product behavior.

### 4. Task First

The main product flow starts from a task.

Do not make static pre-existing agent chat the main entry path.

### 5. Responder Agnostic

The system should ask:

- who should answer
- through what responder

Not:

- which LLM should answer

### 6. Prefer Visible Decisions Over Hidden Magic

Important orchestration choices should become visible traceable decisions.

Examples:

- planning agent created
- work item assigned
- agent self-nominated
- grant issued
- new seed agent created
- responder mode selected

### 7. Separate Product Logic From Demo Logic

Do not leave demo shortcuts in the main product path once product flow has been defined.

Examples:

- pre-opening hard-coded windows
- defaulting to a fake static team
- using debug-only assumptions as user-facing behavior

## Pre-Task Checklist

Before implementing a new feature or refactor, verify:

1. Is this task-first?
2. Does this introduce a hidden hard-coded path?
3. Does this introduce a silent fallback?
4. If there is a fallback, is it explicit and visible?
5. Is this logic in the correct layer?
6. Should this become a shared protocol/system instead of a local convenience?

## Layer Guidance

### `hc-core`

Should own:

- runtime
- message routing
- nomination/grant flow
- instance/session/job state

Should not own:

- hidden product policy
- provider-specific responder logic
- fixed task flow assumptions

### `hc-agent`

Should own:

- task lifecycle
- planning
- assignment
- task-scoped agent creation
- orchestration

Should not own:

- hidden responder/provider fallback behavior

### `hc-ui`

Should own:

- product workflow presentation
- user intervention surfaces
- workspace visibility

Should not hide:

- responder mode
- decision outcomes
- assignment logic

## Required Documentation Updates

When changing main product flow or architectural boundaries, update at least one of:

- `docs/task-driven-product.md`
- `docs/architecture.md`
- `docs/ui-workbench.md`

## Summary

Honeycomb should prefer:

- explicitness over hidden convenience
- visible decisions over silent heuristics
- generated task-scoped teams over static hard-coded teams
- responder abstraction over LLM-only thinking
