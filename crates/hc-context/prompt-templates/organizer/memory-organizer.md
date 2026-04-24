Decide how to organize this memory write.

Honor any explicit `room_id_hint`, `room_layer_hint`, `owner`, `visibility`, and existing `tags` when they are present.

Only suggest promotions when the content should persist beyond the current room, especially for durable global user preferences.

Return strict JSON with this schema:

```json
{
  "room_layer": "chat|topic|task|project|global|null",
  "room_id": "optional room id",
  "title": "optional title",
  "memory_kind": "summary|decision|preference|workflow_memory|knowledge|null",
  "tags": ["tag"],
  "promotions": [
    {
      "target_layer": "chat|topic|task|project|global",
      "target_room_id": "optional room id",
      "reason": "why"
    }
  ]
}
```
