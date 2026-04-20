# Honeycomb Channel Conversation Mode

## Purpose

Honeycomb needs an explicit channel conversation mode for cases such as:

- two LLM-backed agents and one user chatting in the same channel
- a user and multiple agents discussing a work item before assignment
- a mixed human/LLM specialist thread that remains open across multiple turns

This is not the same as the current single-message nomination flow.

Current behavior:

- a broadcast or channel message opens nomination
- one winner is selected
- the winner replies once

Required behavior for channel conversation mode:

- the channel stays alive as a conversation thread
- multiple participants remain present in the thread
- turns continue across multiple messages
- users and agents can both speak into the same ongoing conversation

## Product Goal

The target interaction is:

1. user creates or enters a channel thread
2. selected participants join that thread
3. user and agents continue discussing in the same channel
4. each new message may trigger one or more candidate replies
5. turn-taking remains explicit and traceable
6. the channel can later produce planning, assignment, or execution outcomes

## Key Distinction

### Existing Channel Messaging

What exists now:

- channel membership
- channel post
- nomination on a message
- one winning speaker
- one reply

This is a message-triggered response path.

### Channel Conversation Mode

What is still needed:

- a persistent conversation object above raw messages
- conversation participants
- turn-taking rules across multiple rounds
- explicit stop conditions
- explicit user participation as a first-class participant

This is a multi-turn collaboration mode.

## Core Model

The new top-level object should be:

- `ChannelConversation`

Not just:

- a channel id
- a loose list of messages

### `ChannelConversation`

Suggested minimum fields:

- `id`
- `session_id`
- `channel_id`
- `title`
- `status`
- `participant_refs`
- `turn_policy`
- `stop_policy`
- `started_at`
- `last_activity_at`

### `ConversationParticipant`

Suggested minimum fields:

- `participant_ref`
- `kind`
- `display_name`
- `role`
- `responder_binding_ref`
- `conversation_mode`
- `state`

Participant kinds:

- `user`
- `agent`

Conversation modes:

- `active`
- `listen_only`
- `manual_only`
- `nominate_first`

States:

- `idle`
- `waiting`
- `speaking`
- `muted`

## Required Turn-Taking Rules

Channel conversation mode must not silently become a free-for-all.

Minimum rules:

1. seeing a message does not automatically mean replying
2. participants may self-nominate for a turn
3. the system must decide who gets the next turn
4. the user may speak at any time
5. user input has highest priority
6. the system must avoid infinite agent loops
7. each turn should be visible in trace output

## Turn Policy

Suggested first version:

- every new message opens a turn window
- candidates self-nominate
- one speaker is selected
- selected speaker posts one turn
- thread returns to waiting state

This means:

- conversation remains multi-turn
- each turn is still explicit
- no hidden free-running agent chatter

### Why This Matters

The channel should feel like a group discussion, but the implementation should still remain:

- traceable
- interruptible
- non-magical

## User Participation

The user must be a first-class channel participant.

This means:

- the user is not outside the thread
- the user does not just observe logs
- the user can speak into the same conversation as agents
- the user can interrupt, redirect, or close the discussion

## Agent Participation

Agents should not all auto-reply by default.

Each participant should have an explicit conversation mode:

- `active`
  - may self-nominate normally
- `listen_only`
  - reads but never self-nominates
- `manual_only`
  - can speak only by explicit human action
- `nominate_first`
  - speaks only after winning a turn

## Responder Model

Channel conversation mode must remain responder-agnostic.

Participants may be backed by:

- `llm`
- `human`
- `rule`
- `script`

The conversation system should ask:

- who may take the next turn
- who won the turn
- which responder should produce that turn

Not:

- which LLM should answer

## UI Expectations

The UI should treat channel conversation mode as a dedicated thread mode.

It should show:

- channel title
- participants
- participant responder type
- current turn state
- who is waiting to speak
- who won the current turn
- recent turn history

The UI should not present this as raw runtime logs only.

## Trace Expectations

Every major step should be traced.

Examples:

- conversation created
- participant joined
- participant mode changed
- turn window opened
- participant self-nominated
- speaker selected
- user interrupted
- turn closed

These should later be persisted through `hc-trace`.

## Layer Placement

### `hc-core`

Should continue to own:

- channels
- messages
- nominations
- grants

Should not silently own:

- product-level conversation policy
- hard-coded turn-taking UX

### `hc-agent`

Should own:

- conversation orchestration above message routing
- participant policy
- self-nomination behavior
- turn resolution integration

### `hc-ui`

Should own:

- channel thread presentation
- user input into the thread
- participant visibility
- turn-state visibility

## Anti-Patterns

Avoid:

- treating channel conversation as just repeated single-message replies
- auto-replying from every participant
- hiding turn-selection logic
- silently promoting channel messages into execution without visible decisions

## Summary

Channel conversation mode should be implemented as:

- a persistent multi-turn conversation layer
- participant-based
- responder-agnostic
- user-first
- explicit in turn-taking
- traceable end-to-end
