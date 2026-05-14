Turn the recalled memories into prompt assets that should shape future model behavior.

Only produce assets for durable instruction-like memories such as user preferences, naming, language, style, output constraints, or other long-lived behavior guidance.

Prefer zero assets over low-confidence assets.

Return strict JSON with this schema:

```json
{
  "assets": [
    {
      "source_memory_id": "optional memory id",
      "kind": "system_policy|behavior_template|style_guide|output_contract|prompt_memory",
      "title": "short title",
      "content": "instruction text for the model",
      "tags": ["tag"]
    }
  ]
}
```
