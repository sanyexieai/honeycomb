# Conversation Runtime

## Goal

Honeycomb agents should not be limited to one user turn and one assistant answer.

The conversation runtime is the generic layer that lets agents:

- send a first answer and later supplement it
- react when a long-running task or tool result becomes available
- initiate a message from a scheduled or external event
- keep pending follow-ups durable across process restarts
- respect per-agent speaking policy

This layer is intentionally separate from agent profiles, the scheduler, and MCP tools.

## Layering

```text
hc-api / CLI / UI
  -> hc-service
    -> hc-conversation
      -> agent runtime
      -> scheduler
      -> tool/MCP runtime
      -> memory
```

`hc-conversation` owns durable event and follow-up records. It does not call LLMs, route MCP tools, or contain business logic.

## Responsibilities

Conversation runtime owns:

- `ConversationEvent`: something happened and may deserve an agent turn
- `PendingFollowUp`: an agent should revisit a topic later
- `AgentTurnProposal`: a durable candidate for an agent to speak
- `ConversationPolicy`: whether an agent may initiate or follow up
- event inbox queries such as pending events and due follow-ups

Agent runtime owns:

- how to interpret an event
- what to say
- which tools to call
- whether to create more follow-ups

Scheduler owns:

- time calculation
- emitting due events
- not natural-language generation

Tools and MCP services own:

- domain capabilities
- factual data
- tool result payloads

## Durable Paths

```text
workspace/tenants/{tenant}/users/{user}/conversation/events/{event_id}.md
workspace/tenants/{tenant}/users/{user}/conversation/followups/{followup_id}.md
workspace/tenants/{tenant}/users/{user}/conversation/proposals/{proposal_id}.md
```

## Agent Policy

Agents declare speaking capability in their Markdown frontmatter:

```yaml
conversation_policy:
  can_initiate: true
  can_follow_up: true
  follow_up_style: concise
  proactive_triggers:
    - order.status_changed
    - scheduled.followup_due
    - tool.result_ready
  quiet_hours:
    start: "22:00"
    end: "08:00"
```

This policy is declarative. It does not by itself send messages.

## Event Examples

```yaml
kind: order.status_changed
agent_id: agent.careos.food_delivery
room_id: room.chat.local.default
payload:
  order_id: 1001
  status: delivering
```

```yaml
kind: tool.result_ready
agent_id: agent.careos.travel
payload:
  result_ref: tool-result.abc
```

## Design Rule

Do not put proactive conversation mechanics into a domain tool or MCP service.

Domain-specific meaning belongs in agents and tools. Speaking timing, pending follow-ups, and event inbox state belong in `hc-conversation`.

## Current API Surface

```text
GET  /v1/conversation/inbox
POST /v1/conversation/events
POST /v1/conversation/process
POST /v1/conversation/proposals/draft
POST /v1/conversation/proposals/sent
POST /v1/conversation/proposals/dismiss
```

Processing the inbox does not directly push a message to the user yet. It creates durable `AgentTurnProposal` records after checking the target agent's `conversation_policy`. A UI, API loop, or later agent runtime can accept/send those proposals.

`draft` asks the selected agent to produce user-facing text and stores it on the proposal payload as `draft_message`. `sent` and `dismiss` only move proposal status; delivery remains a UI/channel concern.
