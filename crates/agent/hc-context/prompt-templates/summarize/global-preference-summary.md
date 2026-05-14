Read the input and decide whether it contains a durable user preference that should be stored as global memory.

If it does, return a short factual summary in third person, suitable for future recall.

The `memory_kind` should usually be `preference`.

Return strict JSON with this schema:

```json
{
  "summary": "short durable preference summary",
  "memory_kind": "preference|summary|knowledge|workflow_memory|decision"
}
```
