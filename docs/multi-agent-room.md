# Multi-Agent Room Collaboration

## Purpose

Honeycomb's primary chat unit is not a 1-on-1 user-LLM exchange. It is a **group room** containing one or more users and one or more agents. Long-running tasks are driven by participants inside that room, not by a separate orchestration layer hanging off it.

This document captures the design for:

- the room-as-spine model
- the ledger as the room's anchor
- the Coordinator role that drives long tasks
- the audit chain that gates irreversible actions
- the multi-user foundations that must be in place before single-user features ship

It complements, and sits above, the existing layered docs:

- [task-driven-product.md](./task-driven-product.md) — task lifecycle and planning
- [channel-conversation.md](./channel-conversation.md) — multi-turn conversation primitives
- [conversation-runtime.md](./conversation-runtime.md) — event / follow-up / proposal layer
- [scheduled-tasks.md](./scheduled-tasks.md) — durable scheduling

## Core Principle

Chat = group room with multiple participants. Everything else falls out:

- there is no "main conversation" with separate "drivers" attached
- task drivers (self-check loop, drone agent) are **participants**, not separate subsystems
- task lifecycle = room lifecycle: open while the ledger has items, closeable when the ledger is empty
- "is everything resolved?" is answered by inspecting the ledger, not by asking an LLM

## Principal Model

Every actor is a `Principal`. There is no implicit "the user" singleton.

```yaml
Principal:
  kind: user | agent
  id: <stable id>
  tenant_id: ...
```

Every message, event, action, and decision carries an `actor_principal`. Single-user mode is just a room with one user-principal among the owners — not a special case in the data model.

This is the foundation requirement for multi-user. It cannot be retrofitted later without a major rewrite.

## Room

The room is the primary collaboration object.

### Lifecycle

- `opened` — created with an initial principal set and an initial ledger
- `active` — ledger has open items, work is happening
- `paused` — a user pressed pause, or a budget was hit
- `awaiting_close` — ledger is empty, waiting for owner to confirm or extend
- `closed` — terminal; rooms are not reopened, they are forked

### ACL

```yaml
roles:
  primary_owner: <principal>   # exactly one; conflict-resolution authority
  owners: [...]                # full power: invite, close, kill, override
  members: [...]               # speak, propose invites, propose close
  observers: [...]             # read-only
  auditors: [...]              # sign off on gated actions; do not drive
```

Power matrix:

- **invite participant** — owners directly; members propose, owner approves
- **close room** — owners only
- **pause / kill** — owners only
- **kick** — owners only

When multiple owners issue conflicting instructions, the Coordinator follows `primary_owner` and surfaces the conflict to other owners as a ledger item. No voting in P0.

### Visibility

Every message carries `visible_to` (default: full room). The model supports subsets even if the P0 UI does not expose them. This keeps space for:

- private user-to-Coordinator notes
- agent-to-agent technical exchanges that do not page the user
- per-user side discussions in future multi-user rooms

### Sub-rooms

A complex ledger item may spawn a child room.

- child inherits parent ACL by default; may narrow it
- child owns its own ledger
- on close, child reports a summary to the parent — not the full transcript
- the user can drill into a child room at any time
- parent rooms display rolled-up state, not nested chat

This is how depth is achieved without overflowing a single room.

## Ledger

The ledger is the room's spine. It is an explicit list of open items.

```yaml
LedgerItem:
  id: ...
  description: ...
  created_by: <principal>
  state: open | claimed | done | deferred
  assigned_principal: <principal | null>
  required_capabilities: [...]
  audit_required: <scope | null>
  parent_item: <id | null>
  child_room: <room_id | null>
```

Room state derives from the ledger:

- room is active ↔ ledger has open items
- task complete ↔ ledger empty
- room closeable ↔ ledger empty AND no owner activity for N minutes
- needs new agent ↔ ledger has an item with no current participant matching `required_capabilities`

This turns "are we done?" into an objective query.

## Coordinator

Each room has exactly one Coordinator. It is the merger of what was first proposed as two roles ("fixed self-check loop" and "drone agent") — they are the same thing at different aggressiveness levels.

### Responsibilities

- on every new message, update the ledger (add / close / amend items)
- when an item is unclaimed and no participant is working it: open nomination
- when no current participant matches an item's required capability: propose recruiting a new agent
- post checkpoint summaries when sub-tasks complete or the ledger materially changes
- propose room closure when the ledger goes empty

### Autonomy default

The Coordinator is autonomous by default:

- recruits matching agents from the existing capability pool without per-invite user approval
- allows agents to address each other directly within the room
- runs continuously while the ledger has open items

The brakes are not "ask before acting":

- user can `pause / redirect / kill` at any moment
- all Coordinator actions are visible in the room and the trace
- only irreversible / externally-visible actions go through the audit chain (see below)

This mirrors Claude Code's own model: autonomous most of the time, gated for irreversible things, fully observable.

### Configuration tiers

The same Coordinator supports three tiers via configuration, not separate implementations:

- `passive` — only checks the ledger during silence; does not recruit
- `default` — recruits, drives, posts checkpoints
- `driving` — aggressive recruitment, frequent ledger updates; used for long autonomous runs

## Autonomy Budgets

Budgets are hard rails the Coordinator cannot exceed:

- max agent-to-agent consecutive turns before user input is required
- max active agents in room
- room wall-clock cap
- room token budget

Hitting any budget transitions the room to `paused` and surfaces a ledger item asking the owner to extend, redirect, or close.

These are the **only** runtime controls on routine actions. Per-invite or per-message approvals are not used.

## Auditor and Audit Chain

The audit chain is the **only** gate on irreversible actions. It does not gate routine work.

### Auditor as capability

`can_audit { scope }` is a capability held by a Principal. Both users and agents may hold it. There is no fixed Auditor identity — any Principal with matching scope is eligible.

### Scope tiers

Audit scope is declared at action definition time, not negotiated at request time:

- `low` — no audit (internal memory updates, draft files in workspace)
- `medium` — single auditor (small PR, internal API call, agent invitation, ledger reorganization)
- `high` — full ordered chain (prod deploy, external send, cross-user write, destructive ops, persona promotion to durable store)

Most actions should land in `low` or `medium`. Only truly unrecoverable actions land in `high`.

### Audit chain (high tier)

```yaml
AuditChain:
  scope: prod_deploy
  steps:
    - principal: agent.reviewer
    - principal: agent.lead
    - principal: user.alice
  current_step: 0
  decisions: []
  status: pending | approved | rejected
```

Rules:

- **all-of, ordered**. Every step must approve, in declared order.
- **chain order is declared in the scope binding**. The requester cannot pick or reorder.
- **a rejection at any step kills the chain**. Resubmission of a modified proposal restarts from step 1 — prior approvals were on the old artifact.
- **propose-alternative = reject + new request**. Auditors do not mutate a chain in flight.
- **later auditors see prior auditors' comments**. This is the value of sequential vs parallel.
- **agent auditor decisions record full input + reason in trace**. Human auditor decisions record reason text only.
- **user is the implicit final authority**. Users may reverse an agent-audited decision after the fact; the offending auditor's scope is automatically narrowed.
- **chain pending on one ledger item must not freeze the rest of the room**. Other unrelated items continue.

### Per-step timeout

```yaml
on_timeout: stall | deny | skip_to_next
```

- default: `stall` — never decide for an absent human
- `deny` allowed for safety-critical scopes
- `skip_to_next` allowed only when the entire chain is agent-staffed
- `approve` on timeout is **not** an option

### Delegated budgets

A chain may pre-approve a class of future requests under a budget:

```yaml
DelegatedBudget:
  scope: refactor
  conditions:
    max_lines: 200
    same_repo: true
  expires_at: ...
  remaining_uses: 50
  fallback_chain: [reviewer-agent, user.alice]
```

While the budget is active, matching requests need only the budget holder's signature. Out-of-condition requests fall back to the original chain. Owners may revoke a budget at any time.

This is the throughput mitigation for long autonomous runs. Without it, an all-of-sequential chain on every small action would starve the system.

## Multi-User Foundations (must be present from day 1)

- Principal everywhere; no `the_user` singleton
- Room ACL with `primary_owner + owners + members + observers + auditors`
- `visible_to` on every message (default may be full room in P0 UI)
- `actor_principal` on every action / event / decision
- Conflict rule: `primary_owner` authority; conflicts surface as ledger items
- Auditor scoping by principal: alice's auditor agent does not audit bob's actions unless explicitly delegated

## Explicitly Deferred

- multi-user cross-room collaboration
- shared workspaces and shared persona memory across users
- private user-to-user channels within a room
- complex permission delegation policies
- voting or consensus models for room governance
- parallel audit chains within a single tier
- reopening of `closed` rooms (use fork instead)

## Anti-Patterns

- treating the user as the only valid auditor
- gating routine agent invitations behind user approval
- letting requesters pick or reorder their own auditors
- auto-approve on auditor timeout
- agent-to-agent private side-channels
- one pending audit chain freezing the entire room
- "is everything resolved?" judged by an LLM over the full transcript instead of by the ledger

## Mapping to Existing Crates

This design lands on existing layers; no new top-level crate is needed.

- `hc-core` — room, message, event, principal (already aligned)
- `hc-conversation` — ledger as a typed event/state surface; Coordinator participates here
- `hc-claim` — nomination drives unclaimed ledger items
- `hc-agent` — Coordinator implementation, sub-room spawn, recruit flow
- `hc-capability` — `can_audit { scope }` extends the capability vocabulary
- `hc-trace` — every audit decision and Coordinator action becomes a trace record
- `hc-scheduler` — Coordinator's "revisit later" and per-step timeouts delegate here

New types and traits are added to existing crates rather than spinning a new one.

## Open Questions

- default `max_agent_to_agent_turns` — needs empirical tuning; starting suggestion `5`
- default Coordinator tier for new rooms — `default`; `driving` opt-in per task
- delegated budget defaults — none granted by default; users opt in per scope
- whether sub-rooms should inherit parent ledger items as references or as copies

These can be decided during implementation without affecting the model above.
