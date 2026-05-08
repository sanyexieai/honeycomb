# ADR-006 Experience Lane Boundary

## Status

Accepted

## Context

Honeycomb wants to learn from execution over time, and ACE provides a useful
reference pattern: execution feedback should not disappear into logs, but should
become structured experience that can improve future behavior.

At the same time, Honeycomb's current narrow-path rollout is intentionally
conservative:

- conversation-first
- thin orchestration
- strong degradation paths
- deterministic and testable P0 behavior

If experience capture, reflection, and curation are inserted into the main
execution path too early, they will add:

- latency
- token cost
- storage growth
- prompt injection surface
- new failure modes and queue semantics

This ADR defines the boundary between Honeycomb's main execution lane and its
experience lane.

## Decision

Honeycomb adopts an **opt-in, post-task, non-blocking experience lane**.

This lane is inspired by ACE's execution-feedback and playbook-evolution ideas,
but it is not part of Honeycomb's P0 default execution flow.

Suggested implementation phases (informative, not a second status field):

| Phase | Scope |
| ----- | ----- |
| P1 | raw outcome capture only |
| P1.5 | delta proposals without automatic canonical merge |
| P2 | curator merge and reading accepted playbook material into constrained planner or assignment context |

Promotion beyond task scope is later than these phases.

## 1. Trigger Boundary

The experience lane does not trigger from a user-visible conversational ending.
It triggers from persisted task/work-item lifecycle state.

Authoritative rule:

- experience-lane triggering is based on **persisted terminal task/work-item
  state**
- not on whether the conversation appears visually complete

For P1/P2 implementations, terminal state includes:

- `done`
- `cancelled`

If review gates exist for the task, triggering occurs only after the persisted
review outcome has closed the relevant task/work-item path.

## 2. Scope And Default Enablement

Experience capture is not universally enabled.

Default policy:

- `L1`: disabled
- `L2`: disabled by default; raw outcome capture may be enabled only with
  explicit opt-in or policy rate limiting
- `L3`: may capture raw outcomes when allowed by policy

The effective enablement hierarchy is:

1. tenant policy decides whether learning is allowed at all
2. task-level flag decides whether this task uses the experience lane
3. conversation-level hints may request learning, but may not override tenant
   policy or silently enable high-risk learning

Recommended P1 effective switch:

- `enable_learning` is evaluated at **task scope**
- conversation-level user language may propose it
- tenant policy must authorize it

**Reflection and curation vs raw capture:** reflection, delta proposals, and
curator merge require stricter preconditions than raw outcome logging. Even for
`L3`, prefer requiring both an **elevated risk profile** (policy-defined, for
example high-risk work) and **authorized `enable_learning` on the task** before
running these stages. Raw outcome capture alone may remain policy-gated on a
separate, looser tier.

## 3. Outcome Capture Policy

Outcome labels must not rely only on model self-judgment.

Preferred P1/P2 signal sources:

- tests passed / failed
- work item or task ended in `done` or `cancelled`
- review accepted / rejected
- explicit user positive / negative feedback
- timeout / retry / escalation counters

LLM reflection may explain likely causes, but should not be the sole authority
for outcome labeling.

### L2 Raw Outcome Limits

If `L2` raw outcome capture is enabled, it must be bounded.

Implementations should enforce one or more of:

- per-task count limits
- per-tenant per-day byte limits
- sampling
- compaction into aggregate counters once limits are reached

The goal is to avoid turning lightweight conversation-backed execution into a
high-volume experience log by default.

## 4. Proposal And Merge Gates

Experience updates must not write directly into a canonical playbook.

Required stages:

1. outcome capture
2. reflection or delta proposal
3. curator merge or explicit acceptance

Allowed artifact classes for the experience lane include:

- raw outcome record
- reflection note
- playbook delta proposal
- accepted playbook entry

### Merge Policy

P1/P2 should support at least these merge modes:

- `proposal_only`
- `auto_merge_low_risk`

High-risk tasks should default to:

- proposal only
- or human/policy approval before canonical merge

This ADR explicitly acknowledges that curator is a trust concentration point.
Even when reflection and curation are automated, high-risk merge paths should be
able to degrade to proposal-only behavior.

## 5. Playbook Form And Storage

Honeycomb should distinguish between:

- structured canonical experience entries
- human-readable playbook projection

The canonical source of truth should be structured entry storage, keyed by
stable ids such as `strategy_id` or `delta_id`.

`task.playbook.md` should be treated as a readable projection or merged view,
not as the sole authoritative store for attribution and merge operations.

This avoids:

- paragraph-level unstable references
- lossy diffing
- ambiguous attribution

### Read gates for prompt context

Planner, assignment, or review contexts should consume **accepted** canonical
entries only. Proposed or unverified deltas must stay out of high-weight prompt
slots unless a named experiment or evaluation mode explicitly allows them.

Accepted entries should carry traceable provenance in `hc-protocol` (for example
`author`, `evidence_ref`, `created_from_work_item_id`, and `status`) so reads can
filter and budget safely.

## 6. Concurrency And Single Writer Rule

Multiple execution paths may produce concurrent reflections or delta proposals.

Therefore:

- multiple reflectors may append proposals concurrently
- only one curator merge path may update the canonical accepted playbook for a
  task at a time

Implementations should use a single-writer queue, version check, CAS-style
guard, or equivalent mechanism to avoid last-writer-wins corruption.

## 7. Attribution Strength

Strong causal attribution is often not justified in multi-factor agent flows.

Default expectation:

- use weak or partial attribution by default
- allow multiple influencing references
- reserve strong attribution for explicit experiments such as A/B routing or
  narrow policy trials

This reduces the risk of inventing false certainty about why an outcome
occurred.

## 8. Async Job Semantics

The experience lane is an async side lane.

It must be:

- non-blocking for main task completion
- observable
- joinable with main execution traces

Minimum requirements:

- each side-lane job has a `trace_id`
- task/work-item linkage is recorded
- retries are finite and visible
- failed jobs are observable as failed or dead-lettered work

Main execution must not depend on successful completion of the experience lane
to mark a task complete.

## 9. Read Budget For Accepted Experience

If accepted task-level experience is later read into planner prompts or other
contexts, it must be budgeted.

P2+ implementations should define:

- max accepted entry count
- token budget
- ordering strategy such as recency, evidence strength, and scope match

The experience lane must not crowd out:

- the active conversation context
- current task plan context
- execution artifacts needed for the present turn

## 10. Promotion Beyond Task Scope

Promotion from task-level experience to broader project or global rules is not
automatic.

A candidate threshold may nominate entries for broader review, but nomination is
not acceptance.

Any promotion beyond task scope should include:

- scope metadata such as tenant/project/language/task type/toolchain
- confidence
- review schedule such as `review_after`
- expiry or revocation condition

`review_after` and `expires_at` are only useful if someone or something
actually evaluates them.

Therefore promotion review must name an executor, such as:

- a human reviewer
- a scheduled maintenance job
- a policy-controlled curator pass

Silent expiration fields with no review path are not sufficient.

## Consequences

Benefits:

- Honeycomb can borrow ACE's learning discipline without destabilizing P0
- execution and learning are separated cleanly
- experience is captured with traceability and bounded trust

Costs:

- learning improvements arrive later than an aggressively online system
- additional async job and structured-entry machinery will be needed in P1/P2

## Notes

- this ADR is intentionally conservative: it protects the conversation-first P0
  lane from premature learning-path complexity
- Honeycomb should treat ACE as inspiration for a controlled experience lane,
  not as a mandate to make every turn self-evolving
