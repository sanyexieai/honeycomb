# Honeycomb ADRs

This directory captures implementation-shaping architectural decisions that are
small enough to be actionable and stable enough to guide code changes.

Current swarm-collaboration ADR set:

- [ADR-001 Task Routing Tier](./ADR-001-task-routing-tier.md)
- [ADR-002 Planner Only Semantics](./ADR-002-planner-only-semantics.md)
- [ADR-003 Work Item Persistence And Idempotency](./ADR-003-work-item-persistence-and-idempotency.md)
- [ADR-004 Conversation Task Room Scope](./ADR-004-conversation-task-room-scope.md)
- [ADR-005 Outward Speaker And Artifact Schema](./ADR-005-outward-speaker-and-artifact-schema.md)
- [ADR-006 Experience Lane Boundary](./ADR-006-experience-lane-boundary.md)

The intent of this set is to keep Honeycomb's swarm direction grounded in a
small P0/P1 state surface:

- keep the user-facing interaction centered on conversation
- let tasks and work items appear when needed
- make routing, assignment, and summarization visible and testable
- avoid introducing heavyweight orchestration objects before the narrow path is
  proven

Experience-lane note:

- ADR-006 defines a post-task, opt-in learning lane inspired by ACE
- it is intentionally separated from the P0 default execution path
