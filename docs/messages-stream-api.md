# `/v1/messages/stream` 调用文档

## 概览

轻量接口 **`POST /v1/messages/stream`**（SSE）与 **`POST /v1/messages`**（单次 JSON）共用请求体 schema **`UserMessageBody`**（OpenAPI 中为向后兼容仍保留已弃用别名 **`UserMessageStreamBody`** → `UserMessageBody`）。二者与完整 **`POST /v1/chat`**（`ChatRequest`）相比字段更少；**`/v1/messages`** 的响应体形状与 **`POST /v1/chat`** 相同（`ChatResponse`）。

**`/v1/messages/stream`** 使用 Server-Sent Events（SSE）时的约定如下：

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

### 同步 `POST /v1/messages`

与上表相同的 JSON 请求体，路由为 **`POST /v1/messages`**，`Content-Type: application/json`，响应为 **`ChatResponse` JSON**（与 **`POST /v1/chat`** 一致），无需 `Accept: text/event-stream`。

```bash
curl -s -X POST "http://127.0.0.1:3000/v1/messages" \
  -H "Content-Type: application/json" \
  -d '{"text":"一句话介绍你自己。"}'
```

### 请求字段（流式与同步共用）

| 字段 | 类型 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- | --- |
| `text` | string | 是 | 无 | 用户输入的消息文本。 |
| `tenant_id` | string | 否 | `local` | 租户 ID。空字符串会按默认租户处理。 |
| `user_id` | string | 否 | `default` | 用户 ID。空字符串会按默认用户处理。 |
| `session_id` | string | 否 | 命名空间默认会话 | 会话 ID。用于维持同一轮对话上下文。 |
| `room_id` | string | 否 | 无 | 记忆房间 id；与完整 `ChatRequest.room_id` 一致，用于房间能力、路由上下文与可选的首个 SSE `chat.room_capabilities`。 |
| `behavior_pattern` | string | 否 | 无 | 行为模式名称，与 `ChatRequest.behavior_pattern` 一致；可选值见 **`GET /v1/behavior/patterns`**。 |
| `thinking_depth` | integer (0–255) | 否 | 无 | 思维深度覆盖，与 `ChatRequest.thinking_depth` 一致；由行为引擎在合并配置时使用。 |
| `agent_id` | string | 否 | 自动路由 | 指定 Agent ID（路由/本回合说话者）。与 `ChatRequest.agent_id` 一致。 |
| `domain_id` | string | 否 | 无 | 领域路由提示。未指定 `agent_id` 时可辅助服务端选择 Agent。 |
| `active_agent_id` | string | 否 | 无 | 当前任务/会话上下文中的 active agent，与 `ChatRequest.active_agent_id` 一致（与用于强制路由的 **`agent_id`** 不同）。 |
| `active_task_id` | string | 否 | 无 | 显式绑定当前任务 id（与完整 `ChatRequest` 的 `active_task_id` 一致），用于任务域记忆与 swarm 观测。 |
| `active_work_item_id` | string | 否 | 无 | 可选 `work-item.*` id；在 HTTP L2/L3 单轮退化协调下，当计划中存在多个未终态 planner 工项时，用于指明本轮应对哪一行做 claim/assign 及（在合法时）合成完工。 |
| `provider` | string | 否 | 无 | LLM 提供商 id，与 `ChatRequest.provider` 一致。 |
| `model` | string | 否 | 无 | 模型名，与 `ChatRequest.model` 一致。 |
| `system_prompt` | string | 否 | 无 | 本轮系统提示覆盖，与 `ChatRequest.system_prompt` 一致（空串会按服务端归一规则忽略）。 |
| `temperature` | number | 否 | 无 | 采样温度，与 `ChatRequest.temperature` 一致。 |
| `max_output_tokens` | integer (≥1) | 否 | 无 | 最大输出 token 上限，与 `ChatRequest.max_output_tokens` 一致。 |
| `messages` | `ApiChatMessage[]` | 否 | `[]` | 本轮之前的对话历史，与 `ChatRequest.messages` 一致；与 `text` 同时存在时由服务端按与完整 `ChatRequest` 相同规则合并。 |
| `memory` | `ApiMemoryQuery` | 否 | 无 | 记忆检索参数。默认 `namespace` 由顶层 `tenant_id` / `user_id` 推导；若 body 中 `memory.namespace` 与默认 `local`/`default` 不同，则以其为准并参与归一化。可在此设置 `scope` / `kind` / `tag` / `text` / `limit` 等子字段。 |

OpenAPI 中该请求体 schema 为 **`UserMessageBody`**（历史名称 **`UserMessageStreamBody`** 仍作为 **`deprecated`** 别名保留在 `openapi.json` 中）。若还需 **`ChatRequest`** 上尚未镜像的字段，请改用 **`POST /v1/chat`** 或 **`POST /v1/chat/stream`**。

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
    "room_id": "room-kitchen",
    "domain_id": "life",
    "text": "根据我最近的偏好，帮我安排今天午饭。"
  }'
```

### 带任务与工项作用域的示例

```bash
curl -N \
  -X POST "http://127.0.0.1:3000/v1/messages/stream" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{
    "tenant_id": "local",
    "user_id": "alice",
    "session_id": "chat-2026-05-01",
    "active_task_id": "task.coord.example",
    "active_work_item_id": "work-item.0002",
    "text": "按当前工项完成说明，输出一句状态摘要。"
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
- 在多工项并行、且走 HTTP L2/L3 退化协调时，若需对**指定** planner 工项打点持久化（而非仅落到「第一个开放式工项」），应随请求带上 `active_task_id` 与 `active_work_item_id`。
- 当路由进入 HTTP `L2/L3` 且存在 task 锚时，服务端会按稳定顺序 `execution_result` → `plan_note` → `review_note` 将任务房摘要只读注入 system prompt，客户端无需重复拼接这三类历史摘要。
- 需要 **`messages`**、**`memory`** 等轻量字段时优先使用 **`POST /v1/messages`**（单次 JSON）或 **`POST /v1/messages/stream`**（SSE）；仍需 **`ChatRequest`** 专有字段时再改用 **`POST /v1/chat`** 或 **`POST /v1/chat/stream`**。
- 若走 **SSE**（`/v1/messages/stream` 或 `/v1/chat/stream`），客户端应优先展示 **`chat.delta`**，并在 **`chat.completed`** 到达后用完整 **`response.message.content`** 校准最终文本。
- 走 SSE 时，客户端应监听 **`chat.error`**；走 **`POST /v1/messages`** / **`POST /v1/chat`** 时则应检查 HTTP 状态与非 2xx 的报错体。
- 服务端在 **`/v1/messages/stream`**（及 **`/v1/chat/stream`**）上约每 **15** 秒发送 SSE keep-alive；客户端解析时可忽略非 **`event`/`data`** 行。
