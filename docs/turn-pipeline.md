# Turn Pipeline

Honeycomb turn handling is moving from ad-hoc command branches toward a shared turn frame plus node pipeline.

The pipeline should stay generic. Agent, domain, project, and tool documents provide policy and capability details; the platform pipeline only owns lifecycle and orchestration.

## Turn Frame

`TurnFrame` is the per-turn working state. It is created after command handling and before any intent-specific flow runs.

Current frame fields:

- `user_turn`: current user input.
- `runtime`: resolved runtime variables, including tenant, user, and session identity.
- `namespace`: memory namespace.
- `workspace_namespace`: workspace document namespace.
- `session_id`: active conversation/session id.
- `turn_index`: conversation turn number.
- `selection_input`: text used for tool selection.
- `selection`: current tool candidates and selected tool.
- `recalled_memories`: context memories retrieved for this turn.
- `pending_confirmation`: pending action from previous tool output.
- `tool_execution_context`: observations from any tool executed during the turn.
- `selected_agent_id`: selected agent once agent routing is node-backed.
- `selected_domain_id`: selected domain once domain routing is node-backed.

## Node Layers

Recommended layers:

1. `input`
   Normalize the raw user turn, handle explicit slash commands, and assign session identity.

2. `context`
   Resolve runtime variables, memory, recent conversation state, pending actions, project/domain context, and tool catalogs.

3. `intent`
   Produce intent candidates and slots. This node should be able to use rules, md hints, and an LLM fallback.

4. `agent_route`
   Select an agent/domain from project, domain, and agent documents.

5. `policy`
   Load agent/domain policy such as confirmation requirements, minimal-question behavior, health-context requirements, and fallback strategy.

6. `plan`
   Turn the intent plus policy into one or more actions. Actions can be tool calls, schedules, confirmations, or normal chat.

7. `act`
   Execute selected actions. Tool execution must receive runtime variables and should return structured observations.

8. `observe`
   Normalize tool results, record failures, update pending actions, and produce follow-up events.

9. `respond`
   Render a user-facing reply or schedule/publish an asynchronous follow-up.

10. `learn`
   Persist useful failures, successful flow traces, preference updates, and threshold feedback.

## Design Rule

The pipeline order should be configurable only after the frame contract is stable. Otherwise configuration would merely move today's hardcoded branches into YAML.

The first migration step is to keep behavior intact while making every branch read and write `TurnFrame`.

## Current Migration

The CLI loop currently has these frame-backed nodes:

- `pending_confirmation`
  Reads `pending_confirmation`, executes the confirmed action, records failures, and can return a user reply or schedule a retry.

- `timed_sequence`
  Reads current turn plus recent user history, creates timed followups, and returns a scheduling acknowledgement. It also handles configured reminder rules such as "remind me later" by creating a delayed conversation followup.

- `configured_agent_mcp`
  Uses agent/domain/tool routing metadata to call configured MCP tools, records tool failures, and can set pending confirmation for follow-up actions.

- `llm_tool_router`
  Uses the model-backed tool router as a fallback when configured routing does not decide the turn. It can create tools/skills, execute a selected tool, or update tool selection context.

- `normal_chat`
  Builds the final LLM chat request from the frame, memory, tool context, and observations. It also normalizes create-tool command markers emitted by the model.

- `explicit_command`
  Handles slash commands such as `/help`, `/clear`, `/tools`, `/plan`, and `/create-tool` before normal turn routing starts.

The CLI also now has shared response helpers for node replies and normal chat replies. These helpers are intentionally transport-shaped: today they print to CLI and persist chat memory; later the same boundary can publish SSE events or WebSocket messages. This keeps response rendering out of the individual routing nodes.

The remaining branches are still inline and should be migrated next:

- command response rendering

`TurnNodeReply` is the current minimal node result. It can provide a reply, clear or set pending confirmation, and decide whether the pipeline should stop for the turn.
