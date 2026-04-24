Infer compact semantic tags that help route and retrieve a memory.

Return only durable tags. Prefer room-kind tags and one or two meaningful slugs over long lists.

Valid room-kind tags include: `agent`, `tool`, `project`, `task`, `topic`.

Also include stable semantic slugs such as `reviewer`, `rg`, `honeycomb`, or `runtime.refactor` when clearly supported.

Do not include prose, explanations, or duplicate tags.

Return strict JSON with this schema:

```json
{
  "tags": ["agent", "reviewer", "tool", "rg"]
}
```
