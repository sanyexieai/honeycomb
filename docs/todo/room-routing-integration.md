# Room Routing Integration TODO（已完成）

本节 room 专用清单已逐项落地。**路由分层 L1/L2/L3**、trace 与同轮 classify 的更细勾选见 [`swarm-p0-rollout.md`](./swarm-p0-rollout.md) Phase 3。

原先目标：让 `room` 不只影响 prompt 展示，还进入 turn 路由与能力选择，把「能力启用、候选筛选、默认参数」从硬编码主流程迁到 room-aware routing。实现参见 `hc-service` 下 `room_routing.rs`、`turn_router.rs`。

## Phase 0：统一路由骨架

- [x] 在 `hc-service` 引入 `room_routing` 模块，统一解析 `room -> resolved capabilities -> routing context`。
- [x] 在 `turn` 主流程中引入 candidate-based router，不再由主流程硬编码分支细节。
- [x] 让 room 中解析出的 `tools` 真正约束 MCP 候选集合，而不只是展示在 API 响应中。
- [x] 将 `pending_confirmation / timed / mcp_tool / chat_fallback` 抽成统一 provider，支持显式 reason 与 score。
- [x] 保留 `pending_confirmation / timed / chat_fallback` 为内核级 provider，不受 room 缺省配置误伤。

## Phase 1：统一 Provider Registry

- [x] 引入 `TurnCandidateProvider` 抽象，替代主流程里的顺序式 `if let Some(...)`。
- [x] 为 `pending_confirmation / timed / mcp_tool / chat_fallback` 提供统一 candidate 输出结构。
- [x] 在 candidate 中补充 `provider_id / score / reason / route`。
- [x] 引入 registry 级 provider 配置加载，而不只是 builtin defaults。

## Phase 2：Room 配置进入路由层

- [x] 在 `RoomConfig` 上扩展 routing 配置，例如 `enabled_providers`、`disabled_providers`、`provider_weights`。
- [x] 支持 room 工具白黑名单约束，并接入 MCP 候选筛选。
- [x] 支持 room 为不同 provider 提供默认参数与参数覆盖。
- [x] 支持 room 对 skills/capabilities 做白名单与黑名单约束，而不只是工具约束。

## Phase 3：统一观测与 API 暴露

- [x] 在 `ChatResponse` 或独立调试接口中暴露本轮 `selected_provider` 与 `decision_reasoning`。
- [x] 提供 “room routing explain” 能力，方便查看 room 解析后可用的 providers/tools/skills。
- [x] 将 API 当前的 room capability prompt 增强与 service 层 routing context 收敛为同一数据来源。

## Phase 4：迁移与收敛

- [x] 让 CLI 与 API 共享同一套 service orchestrator，减少平行编排漂移。
- [x] 把 timed / reminder / scheduler 进一步纳入统一 provider 框架。
- [x] **Chat 路径 `route_agent`（单次 LLM）**：暂不迁入 turn candidate registry，与 swarm rollout **渐进迁移**一致；工作台 L1 nomination 与工作台/`hc-agent` orchestrator、HTTP chat 仍为两条路径——见 [`swarm-p0-rollout.md`](./swarm-p0-rollout.md) Phase 5 **`L1`** / **`L2/L3`** 说明。
