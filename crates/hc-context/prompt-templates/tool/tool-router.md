Route one user turn for Honeycomb's tool-aware chat.

Return only one JSON object. Do not use markdown fences.

Schema:

```json
{
  "action": "chat|create_tool|create_skill|run_tool",
  "tool_id": "tool.<id>|skill.<id>|null",
  "args": ["argument"],
  "goal": "string|null",
  "message": "string|null"
}
```

Rules:
- Use `create_tool` only when the user is asking to create a reusable executable tool, command wrapper, service, script, workflow, or CLI capability.
- Use `create_skill` only when the user is asking to create a reusable skill, role pack, behavior profile, workflow guidance, or instruction pack.
- Use `chat` for normal questions, artifact generation, code generation, or requests to use an existing tool or skill.
- Use `run_tool` only when the user explicitly asks to run/read/search/test with an existing tool and the required arguments are present in the user turn.
- If an existing tool or skill should guide the turn, set `tool_id` to one id from Tool candidates.
- For `run_tool`, set `args` to the exact tool arguments from the user turn and set `goal` to the user's intent.
- Do not use `run_tool` for write or destructive operations unless the user provided the exact target path and content.
- If no listed candidate is useful, set `tool_id` to null.
- Do not invent tool ids.
- Do not produce user-facing prose.

Tool candidates:
{{tool_candidates}}

User turn:
{{user_turn}}

{{additional_system_guidance}}
