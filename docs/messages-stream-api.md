# `/v1/messages/stream` 调用文档

## 概览

`POST /v1/messages/stream` 是面向前端或业务调用方的轻量消息流式接口。调用方只需要提交用户可见的文本消息，服务端会根据运行时配置决定模型、记忆检索、Agent 路由和工具调用。

接口使用 Server-Sent Events（SSE）返回流式结果：

- 请求体：`application/json`
- 响应体：`text/event-stream`
- 每个 SSE 事件的 `data` 字段都是一段 JSON 字符串
- 常规聊天链路会依次返回 `turn.started`、`chat.started`、多个 `chat.delta`、`chat.completed`、`turn.completed`
- 如果命中已配置的工具回合，可能返回 `turn.started`、`turn.tool`、`turn.completed`
- 执行失败时会返回 `chat.error`

## 请求

```http
POST /v1/messages/stream HTTP/1.1
Host: {host}
Content-Type: application/json
Accept: text/event-stream
```

### 请求字段

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `text` | string | 是 | 无 | 用户输入的消息文本。 |
| `tenant_id` | string | 否 | `local` | 租户 ID。空字符串会按默认租户处理。 |
| `user_id` | string | 否 | `default` | 用户 ID。空字符串会按默认用户处理。 |
| `session_id` | string | 否 | 命名空间默认会话 | 会话 ID。用于维持同一轮对话上下文。 |
| `agent_id` | string | 否 | 自动路由 | 指定 Agent ID。传入后服务端优先使用该 Agent。 |
| `domain_id` | string | 否 | 无 | 领域路由提示。未指定 `agent_id` 时可辅助服务端选择 Agent。 |

### 最小请求示例

```bash
curl -N \
  -X POST "http://127.0.0.1:3000/v1/messages/stream" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"text":"中午推荐我吃什么？"}'
```

### 带会话和路由信息的请求示例

```bash
curl -N \
  -X POST "http://127.0.0.1:3000/v1/messages/stream" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "tenant_id": "local",
    "user_id": "alice",
    "session_id": "chat-2026-05-01",
    "domain_id": "life",
    "text": "根据我最近的偏好，帮我安排今天午饭。"
  }'
```

## 响应事件

### SSE 格式

服务端返回标准 SSE。单个事件形态如下：

```text
id: chat.delta.1714550400
event: chat.delta
data: {"type":"chat","event":{"type":"delta","delta":"可以考虑","finish_reason":null}}
```

客户端应根据 `event` 字段分发事件，并解析 `data` 中的 JSON。

### `turn.started`

表示一次对话回合开始。

```json
{
  "type": "started",
  "tenant_id": "local",
  "user_id": "alice",
  "session_id": "chat-2026-05-01"
}
```

### `chat.started`

表示模型聊天生成开始。该事件包裹在 turn 事件中。

```json
{
  "type": "chat",
  "event": {
    "type": "started",
    "tenant_id": "local",
    "user_id": "alice",
    "session_id": "chat-2026-05-01"
  }
}
```

### `chat.delta`

模型增量输出。调用方通常将 `delta` 追加到当前助手消息中。

```json
{
  "type": "chat",
  "event": {
    "type": "delta",
    "delta": "可以考虑一份清淡的牛肉饭",
    "finish_reason": null
  }
}
```

字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `event.delta` | string | 本次增量文本。 |
| `event.finish_reason` | string/null | 模型侧结束原因；生成过程中通常为 `null`。 |

### `chat.completed`

表示聊天生成完成，并返回完整结构化响应。

```json
{
  "type": "chat",
  "event": {
    "type": "completed",
    "response": {
      "message": {
        "role": "assistant",
        "content": "可以考虑一份清淡的牛肉饭，搭配蔬菜和汤。"
      },
      "model": "qwen2.5-32b",
      "provider": "openai-compatible",
      "tenant_id": "local",
      "user_id": "alice",
      "session_id": "chat-2026-05-01",
      "selected_agent_id": "life-agent",
      "selected_domain_id": "life",
      "recalled_memories": [],
      "synthesized_prompt_asset_count": 0
    }
  }
}
```

`response` 字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `message.role` | string | 固定为助手消息角色，通常是 `assistant`。 |
| `message.content` | string | 完整助手回复。 |
| `model` | string | 实际使用的模型。 |
| `provider` | string | 实际使用的模型提供方。 |
| `tenant_id` | string/null | 归一化后的租户 ID。 |
| `user_id` | string/null | 归一化后的用户 ID。 |
| `session_id` | string/null | 归一化后的会话 ID。 |
| `selected_agent_id` | string/null | 服务端选中的 Agent ID。 |
| `selected_domain_id` | string/null | 服务端选中的领域 ID。 |
| `recalled_memories` | array | 本次生成召回的记忆列表。 |
| `synthesized_prompt_asset_count` | number | 本次合成的提示资产数量。 |

### `turn.tool`

表示当前回合命中已配置的工具调用链路。

```json
{
  "type": "tool",
  "tool_id": "weather.lookup",
  "server_id": "local-tools",
  "tool_name": "lookup_weather"
}
```

### `turn.completed`

表示整个回合完成。

```json
{
  "type": "completed"
}
```

### `chat.error`

表示流式生成或工具执行失败。该事件通常是最终事件。

```json
{
  "type": "chat_error",
  "error": "chat stream worker failed: ..."
}
```

## 前端调用示例

浏览器原生 `EventSource` 只支持 GET，不适合直接发送 JSON POST。前端可以使用 `fetch` 读取 SSE 流：

```js
async function streamMessage(payload, onDelta) {
  const response = await fetch("/v1/messages/stream", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Accept": "text/event-stream",
    },
    body: JSON.stringify(payload),
  });

  if (!response.ok || !response.body) {
    throw new Error(`stream request failed: ${response.status}`);
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;

    buffer += decoder.decode(value, { stream: true });
    const chunks = buffer.split("\n\n");
    buffer = chunks.pop() || "";

    for (const chunk of chunks) {
      const event = parseSseEvent(chunk);
      if (!event.data) continue;

      const data = JSON.parse(event.data);
      if (event.event === "chat.delta") {
        onDelta(data.event.delta);
      }
      if (event.event === "chat.error") {
        throw new Error(data.error);
      }
    }
  }
}

function parseSseEvent(raw) {
  const event = {};
  for (const line of raw.split("\n")) {
    const index = line.indexOf(":");
    if (index === -1) continue;
    const key = line.slice(0, index);
    const value = line.slice(index + 1).trimStart();
    if (key === "data") {
      event.data = event.data ? `${event.data}\n${value}` : value;
    } else {
      event[key] = value;
    }
  }
  return event;
}

streamMessage(
  {
    tenant_id: "local",
    user_id: "alice",
    session_id: "chat-2026-05-01",
    text: "给我一个三句话的午餐建议。",
  },
  (delta) => {
    console.log(delta);
  },
);
```

## 调用建议

- 建议业务方始终传入稳定的 `tenant_id`、`user_id`、`session_id`，这样记忆检索和会话归属更可控。
- 只做普通用户消息发送时，优先使用 `/v1/messages/stream`；需要自定义模型、系统提示词、历史消息数组或生成参数时，使用 `/v1/turn/stream`。
- 客户端应优先展示 `chat.delta`，并在 `chat.completed` 到达后用完整 `response.message.content` 校准最终文本。
- 客户端应监听 `chat.error`，并在连接中断、JSON 解析失败或长时间无事件时做重试或降级。
- 服务端每 15 秒发送一次 SSE keep-alive 注释，客户端解析时可以忽略非 `event/data` 行。
