# Memory Prompt Unification

## Purpose

Honeycomb should not treat prompt material as a completely separate world from memory.

Prompts and memories are both durable guidance artifacts:

- some tell the system what happened
- some tell the system what is true
- some tell the system what usually works
- some tell the system what to do next time

The useful distinction is not "memory vs prompt" as two unrelated storage systems.
The useful distinction is "which stage of consolidation is this artifact in?"

## Core Thesis

Prompts can be modeled as a later-stage form of memory.

In this model:

- factual memory is remembered knowledge
- episodic memory is remembered experience
- procedural memory is remembered method
- prompt material is compiled procedural memory

This makes prompt assets close to an agent's "muscle memory":

- they are not raw facts
- they are not just transcripts
- they are reusable behavioral compression

## Unified Lifecycle

One memory object can evolve through stages:

1. `captured`
   Raw material from the world.
   Examples: user turns, assistant turns, observations, copied notes.

2. `extracted`
   Structured interpretation pulled from raw material.
   Examples: entities, topics, relations, preference candidates, decisions.

3. `generalized`
   Durable cross-instance memory.
   Examples: a stable preference, a reusable workflow, a known tool rule.

4. `procedural`
   Behavior-oriented memory.
   Examples: reviewer habits, project writing conventions, tool invocation style.

5. `compiled`
   Directly injectable guidance for model execution.
   Examples: prompt assets, style guides, output contracts, behavior templates.

This means prompt generation is not special magic.
It is a compilation step in the memory pipeline.

## Forms

To avoid flattening everything into one blob, memory objects should still declare a form.

Suggested forms:

- `raw_note`
- `summary`
- `entity`
- `topic`
- `relation`
- `fact`
- `workflow`
- `policy`
- `prompt`
- `rewrite`

Together, `stage` and `form` answer different questions:

- `stage`: how consolidated is this artifact?
- `form`: what kind of artifact is it?

## Room Placement

The room model still matters.
Unified memory does not mean all artifacts go into one bucket.

Recommended room ownership:

- `chat`: raw turns, local summaries, local rewrites
- `topic`: topic summaries, concept notes, reference memory
- `task`: decisions, progress, execution patterns, task-specific prompts
- `project`: conventions, architecture guidance, project prompt material
- `global`: user preferences, stable long-lived defaults
- `agent`: role-specific procedural and compiled memory
- `tool`: tool-specific procedural and compiled memory

Examples:

- "User prefers concise Chinese" -> `global`, `generalized`, `policy`
- "When acting as reviewer, focus on regressions first" -> `agent`, `procedural`, `policy`
- "Use this exact JSON output contract for CI triage" -> `tool` or `project`, `compiled`, `prompt`

## Prompt As Compiled Memory

Prompt material should be derived, not authored in isolation by default.

A healthy chain looks like:

1. capture local evidence
2. extract durable signal
3. generalize signal into stable memory
4. compile stable memory into prompt-ready assets

Examples:

- raw user turns -> extracted preference -> global policy memory -> style guide prompt
- repeated successful reviews -> agent workflow memory -> reviewer behavior template
- tool troubleshooting notes -> tool policy memory -> tool invocation prompt

This preserves provenance and makes prompts auditable.

## Runtime View

At runtime, the composer may still keep separate sections:

- recalled memory
- procedural memory
- compiled prompt assets
- hard runtime policy
- self model

These can remain separate in request assembly even if they are unified in storage.

This is an important distinction:

- unified storage model
- differentiated runtime composition

## What Should Stay Separate

Not everything should collapse into memory.

Two things still deserve special treatment:

### Hard Runtime Policy

Some instructions are operator-enforced or product-enforced.
They are not learned from experience.

Examples:

- safety constraints
- test-only mode
- debug-only output mode
- "return JSON only" wrapper instructions for a single internal call

These may live near the memory system, but should still be marked as hard policy.

### Self Model

The self model is adjacent to memory, but not identical to it.

It answers:

- who am I?
- what role am I acting in?
- what capabilities and limits do I have?

It can produce memory and consume memory, but should remain a distinct layer.

## Suggested Data Model

Instead of separate first-class worlds for memory and prompt assets, use one broader artifact model.

Suggested core fields:

- `id`
- `room_id`
- `layer`
- `stage`
- `form`
- `title`
- `content`
- `tags`
- `owners`
- `derived_from`
- `source_docs`

Optional execution-oriented fields:

- `activation_score`
- `compilation_target`
- `valid_for_roles`
- `valid_for_tools`
- `output_schema`
- `expires_at`

## Migration Direction

Honeycomb does not need a big-bang merge.

A safer path is:

1. keep current `PromptAsset` and `PromptPolicy` runtime APIs
2. start storing prompt templates as memory-adjacent local assets
3. add `stage` and `form` to room assets
4. treat prompt synthesis as compilation from generalized/procedural memory
5. gradually shrink the conceptual gap between `PromptAsset` and memory assets
6. keep default managed prompt bodies as repo templates, then copy them into local `prompts/**/*.md` on first use

That last step keeps two properties at once:

- defaults stay versioned in the repo
- active prompt assets remain editable as local workspace memory objects

## Practical Rule

Use this heuristic:

- if the artifact is primarily for recall, it is memory
- if the artifact is primarily for execution, it is compiled memory

Both belong in the same family.

## Recommendation

Adopt this model:

- prompts are not foreign to memory
- prompts are compiled procedural memory
- room structure remains the organizing backbone
- runtime composition can still keep memory, prompts, policy, and self-model in separate sections

This gives Honeycomb a system that is:

- easier to reason about
- easier to audit
- easier to evolve
- more natural for local Markdown storage
