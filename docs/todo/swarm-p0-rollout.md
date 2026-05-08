# Swarm 落地清单：执行主路径（P0）与经验副链（P1+）

本文档分两截，避免把 **ADR-001～005** 的执行主路径与 **ADR-006** 的经验副链混在一起排期：

| 章节 | ADR | 阻塞关系 |
| --- | --- | --- |
| **A. Phase 0–9 + M1–M4** | ADR-001～005、[task-driven-product](../task-driven-product.md) | 产品默认必须先打通 |
| **B. Experience lane** | [ADR-006](../adr/ADR-006-experience-lane-boundary.md) | **默认关闭**；失败或延期**不得**挡住 A 节的里程碑 |

**Part A（下文 Phase 0～Phase 9 与 M1～M4）** 对齐 [ADR-001](../adr/ADR-001-task-routing-tier.md)～[ADR-005](../adr/ADR-005-outward-speaker-and-artifact-schema.md) 与 [task-driven-product.md](../task-driven-product.md)。

> **勾选说明**：条目与 **`hc-protocol` / `hc-agent` / `hc-service` / `hc-api` 当前实现** 对齐后勾 `[x]`。大块功能合入时请 **顺手更新本节**，避免文实漂移。最近一次人工对齐仓库：**2026-05-08**（持续小步合入时请更新本日期）。

---

## Phase 0：契约与类型（阻塞其它模块）

- [x] **`hc-protocol`**：定义路由决策结构体（`routing_tier`, `routing_reason`, `routing_signals`, `routing_forced_by_user`, `routing_rule_version`），便于 trace 与测试 fixture 共用。
- [x] **`hc-protocol`**：定义 **task binding** 决策结构体，**字段名与语义**以 [ADR-004 § Observability (task binding)](../adr/ADR-004-conversation-task-room-scope.md#observability-task-binding) 表格为准；规则版本家族与 [ADR-001](../adr/ADR-001-task-routing-tier.md) 的 `routing_rule_version` 对齐（见 ADR-004 表中 `task_binding_rule_version`）。
- [x] **`hc-protocol`**：定义 P0 工件公共头 + 三种 `artifact_kind`（`plan_note` / `execution_result` / `review_note`），`schema_version = artifact_schema_v1`；明确 **`work_item_id` 可选**（task 级 `plan_note` 允许缺省）。
- [x] **`hc-protocol`（或暂时集中在 `hc-agent`）**：`TaskPlan`、`WorkItem`、`WorkItemClaim`、`WorkItemAssignment` 的序列化形态与 **P0 状态枚举**见 [ADR-003 § State Model](../adr/ADR-003-work-item-persistence-and-idempotency.md#state-model)；从 `blocked` 迁出的下一状态见同文 **[§ P0 transition targets](../adr/ADR-003-work-item-persistence-and-idempotency.md#p0-transition-targets)**（rollout 不得另写一套映射）。
- [x] **`hc-protocol`**：`blocked_reason`、`blocked_at`、blocked 出口枚举（`user_resume` / `planner_replan` / `timeout_requeue` / `manual_cancel` / **`timeout_abandon`** ——「放弃重试」超时与 `manual_cancel` 区分审计，目标态均为 **`cancelled`**）。

---

## Phase 1：存储布局与持久化（ADR-003 + ADR-004）

- [x] **`hc-store` + 路径约定**：`hc_store::task_coordination` 为每 `task_id` 规范化 **slug** 与相对路径——**Markdown**：`coordination/{slug}/task_plan.md`、`assignment_decision.{assignment_slug}.md`、预留 `work_items/{wi_slug}.md`；**追加 JSONL**：`coordination/{slug}.routing.jsonl` / `.implicit-intent.jsonl`（与既有实现一致），另预留 `coordination/{slug}/work_item_claims.jsonl`、`work_item_assignments.jsonl`、**`materialization_notices.jsonl`**（物化上限说明，Phase 4）。`persist_task_artifacts`（`hc-agent`）新写入走 **per-task** 子目录；**`query_task_artifacts`** 仍兼容检索旧 **`decisions/*.md`** 与 **`coordination/**/*.md`**。
- [x] **`intent_hash_v1`**：按 ADR-003 实现 normalize + 单测（固定输入 → 固定 hash）。
- [x] **幂等**：隐式 intent 键 dedupe（JSONL + `HashSet` / UI 内存门，见 Phase 8）；**单 work item 至多一条 active assignment（`assigned`/`executing`）**：`TaskPlan::resolve_work_item_assignment` 重复调用返回 **`None`**。**跨重启 claim / assignment 回放**：追加 JSONL `coordination/<slug>/work_item_claims.jsonl` 与 `work_item_assignments.jsonl`（`work_item_claim_journal_v1` / `work_item_assignment_journal_v1`，同 id **末行胜出**）；`persist_task_plan.md` 增加 **# Work Item Claims**；UI 工作台在 claim / resolve（含自动 assign）、以及进入 **`executing`** 时追加相应行，`build_registry_for_task` 起始调用 [`hydrate_task_plan_work_item_coordination_journals`](../../crates/hc-agent/src/persistence.rs)。
- [x] **超时策略（契约层）**：`hc-protocol::swarm::blocked_exit_next_state_v1` —— `timeout_requeue` → **`claiming`**；`manual_cancel` / **`timeout_abandon`**（放弃重试）→ **`cancelled`**；其余出口同 ADR。单测：`blocked_exit_p0_targets_match_adr003`、`p0_blocked_timeout_abandon_targets_cancelled`。调度在出口落库时打 **`hc_trace`** 仍属编排层（非 `hc-protocol`）。

---

## Phase 2：会话 ↔ 任务绑定（ADR-004）

- [x] **会话状态**：`active_task_id` **0/1 槽位**——工作区内按 tenant/user 会话键持久化：`tenants/<t>/users/<u>/conversation/session_swarm/<session_slug>.json`（与 swarm 可观测性同一 `session_key`）。客户端未传 `active_task_id` 时，`hc-service` chat / turn 可观测路径用该槽位参与分类；每轮结束后写回绑定快照的 `active_task_id`（清空则写 `null`）。HTTP **`ChatResponse.active_task_id`** 回传本轮规范绑定，供客户端与 CLI 透传下一请求。规则版本仍与 ADR-001 **`routing_rule_version`** / **`task_binding_rule_version`** 家族一致（见绑定 fixture）。
- [x] **可观测（第二条决策链）**：字段与语义以 ADR-004 **Observability (task binding)** 表格为准；每条相关用户消息写 **task binding 决策**，写入 trace / fixture（避免只做存储、不可测）。
- [x] **检索裁剪**：若请求未显式传 `memory.scope`，`hc-service` 的 chat 准备路径与 swarm 路由/绑定共用同一次分类；**`L1` → `session` 近窗语义**；**`L2/L3`** 在有 task 锚时 → **`task` scope + `room_anchor`**——锚含 **请求/会话 `active_task_id`**、task-layer **`room_id` 推导**、以及 **同轮 HTTP 隐式分配的 `task.http.implicit.*`**（[`prepare_chat_request`](../../crates/hc-service/src/chat.rs) 在 `emit_swarm_observability_from_classified` 之后并入 memory hint）；**无锚**时退化为 `session`（避免扩大跨 task 召回）。客户端显式 `memory.scope` 时仍优先生效。

---

## Phase 3：路由层（ADR-001）

- [x] **规则引擎（无额外模型调用）**：实现 **L1/L2/L3** 判定；`L3` 为「用户显式短语」+「硬规则」两入口。（实现：`hc-agent::swarm_routing`。）
- [x] **用户纠偏**：从 **`hc-agent` 可配置短语表**映射 `force_l1` / `force_l3`，优先级高于自动规则。——`{workspace_root}/swarm_routing_phrases.json`（或 **`HC_SWARM_ROUTING_PHRASES_FILE`**），`extends_builtin` 合并/覆盖规则见 `swarm_routing_phrase_table.rs`。
- [x] **可观测**：每条用户消息写 **路由**决策记录（字段齐 ADR-001），写入 trace / 结构化日志；与 Phase 2 的 **task binding** 决策记录配套出现（同一次处理管线、同一套规则版本家族），避免两条链脱节。（另含 `coordination/<task>.routing.jsonl`、`SwarmRoutingBindingSnapshot`；HTTP `hc-api` / CLI 与 headless 对齐。）
- [x] **`Turn` 路由单次 classify**：[`try_handle_service_turn`](../../crates/hc-service/src/turn.rs)（同步 `/ turn` outcome）与 **`handle_turn_stream_request`**（流式）共用 **`resolve_turn_fallback_room_attachment`** + **`classify_turn_route_decision`** + **`emit_turn_route_swarm_observability_non_chat_fallback`**，保证单次 **`TurnProviderRegistry::decide`**；stream 仍为 **room attach → `Started` → decide**。回归：[`shared_classifier_plain_message_chat_fallback_then_try_handle_aligned`](../../crates/hc-service/src/turn.rs)（ChatFallback）、[`classify_turn_cn_reminder_selects_timed_provider`](../../crates/hc-service/src/turn.rs)（中文提醒 → **`Timed`**）、[`classify_turn_cn_reminder_room_disables_timed_selects_chat_fallback`](../../crates/hc-service/src/turn.rs)（房间 **`enabled_providers` 不含 `timed`** → ChatFallback）、[`classify_turn_pending_confirmation_wins_when_session_awaits_confirmation`](../../crates/hc-service/src/turn.rs)（会话 **`pending_confirmation`** + 确认话术 → **`pending_confirmation`**；`tool-routing-tags.md` 含 **`id`/`type`**；房间 stub 关 **`mcp_tool`**）、[`classify_turn_cn_confirm_room_disables_pending_selects_chat_fallback`](../../crates/hc-service/src/turn.rs)（同上 pending 会话 + 确认话术，但房间 stub 关掉 **`pending_confirmation`**（及 **`mcp_tool`**）→ ChatFallback）。

---

## Phase 4：Bootstrap 与 planner_only（ADR-002）

- [x] **`bootstrap`**：**`bootstrap_task` 默认**为 **planner_only**（等价于 `bootstrap_planning_task` / 单 `planner` seed）。**三角色**（planner + worker + reviewer）通过显式 API [`bootstrap_task_with_preset`](../../crates/hc-agent/src/bootstrap.rs)（`TaskBootstrapPreset::ThreeRolesDemo`）或环境变量 **`HC_TASK_BOOTSTRAP_PRESET=three_roles`**（别名 `three-roles`、`demo`、`full`）启用，供 demo / 单测。详见 `hc-agent::bootstrap`。
- [x] **Materialization 上限**：环境变量 **`HC_MAX_AGENTS_PER_TASK`**、**`HC_MAX_NEW_AGENTS_PER_ROUND`**（正整数；未设置或无效视为该维度不限制）——[`materialize_plan`](../../crates/hc-agent/src/bootstrap.rs) 对每个 `AgentPlan` 取 **min(seeds 数, 两上限)**，只物化 **前缀 seeds**；触顶 **`tracing::warn`** + [`MaterializePlanOutcome::notices`](../../crates/hc-agent/src/bootstrap.rs)。[`bootstrap_task_workbench`](../../crates/hc-agent/src/workbench.rs) 在有 notice 时追加 **`coordination/<slug>/materialization_notices.jsonl`**（`materialization_notice_v1`）。另有 [`materialize_plan_with_limits`](../../crates/hc-agent/src/bootstrap.rs) 供测试显式传 [`MaterializePlanLimits`](../../crates/hc-agent/src/bootstrap.rs)。**跨轮累计**同一 task 已存活实例数的硬会计仍待编排状态；当前为 **单次 batch / 单 plan** 截断。
- [x] **文档对齐**：`task-driven-product.md` 中「阈值」表述与 ADR-003 对齐——**逐轮降低 eligible 阈值** 列为 **P1**（未在 P0 / 当前 assign 实现）；P0 固定 eligible 规则见 rollout Phase 6。

---

## Phase 5：编排主路径（ADR-002 + 渐进迁移）

- [x] **`L1`**：保留现有 **message-level nomination → reply**——**工作台 / `hc-agent` headless**：`AgentOrchestrator::run_nomination_cycle`（[`orchestrator.rs`](../../crates/hc-agent/src/orchestrator.rs)）对 **`RoutingTier::L1`** 的 Chat 消息走 **`suggest_claims_for_message` → `ResolveSpeakingGrant` → `generate_and_post_reply`**；**`L2`/`L3`** 同路径下 **`message_nomination_skipped_for_routing_tier`**（`grant: None`，打 `hc_trace`）。——**HTTP `hc-service` Chat** 仍为 **`route_agent` + 单次 LLM 生成**（无双实例 nomination）；与上文 **渐进迁移** 并存，P0 不强制远端与工作台完全一致。
- [ ] **`L2/L3`**：主路径为 **TaskPlan → WorkItem → claim → assign → 执行**；**不以 message-level nomination 决定 work item owner**。（**工作台 / headless runtime** 在 L2/L3 已跳过 message nomination，走 task 侧编排。）**HTTP `hc-service` chat**：仍为 **`route_agent` + 单次生成**；当路由为 **L2/L3** 且存在 **task 锚**——含 HTTP **隐式工单**分配的 **`task.http.implicit.*`**（[`prepare_chat_request`](../../crates/hc-service/src/chat.rs)）、或绑定 `active_task_id` / 请求 **`active_task_id`** / task-layer room **`room_id` 推导**——且工作区已有 **`coordination/<slug>/task_plan.md`** 时，同上函数将其正文 **节选**并入 **system prompt**，只读、非运行时状态机（回归：[`l23_reuse_active_task_appends_task_plan_excerpt_and_speaker_appendix`](../../crates/hc-service/src/chat.rs)、[`l1_with_task_on_disk_does_not_append_task_plan_excerpt`](../../crates/hc-service/src/chat.rs)、[`l23_http_implicit_task_anchor_appends_task_plan_excerpt_when_file_preseeded`](../../crates/hc-service/src/chat.rs) —— 隐式 slug 与 [`prepare_chat_request_with_swarm_clock`](../../crates/hc-service/src/chat.rs) 固定 **`swarm_created_at_ms`** 对齐）。全量 **claim → assign → 执行**编排仍待本项收口。
- [ ] **`L2` 隐式工单**：用户侧仍为连续对话；内部创建/复用 work item，结果经 **对外 speaker 策略**回灌会话。——**HTTP P0**：**无 task 锚** 的 L2/L3 轮次由 [`emit_swarm_observability_from_classified`](../../crates/hc-service/src/chat.rs) 分配 **`task.http.implicit.{wall_clock_ms}`**（与同轮 **`{message_id_prefix}.{ms}`** 时间桶一致：生产调用处传入当前 [`wall_clock_ms`](../../crates/hc-bootstrap/src/lib.rs)；**单测**用同一毫秒驱动 [`prepare_chat_request_with_swarm_clock`](../../crates/hc-service/src/chat.rs)；直连 **`/chat`** 时前缀 **`chat.api`**，**[`handle_turn_request`](../../crates/hc-service/src/turn.rs)** 回落 **ChatFallback** 再走 prepare 时为 **`turn.chat_fallback`**）、**会话槽**写入 **`persist_session_swarm_active_task_binding`**，并在 **`coordination/<slug>.implicit-intent.jsonl`** 上按 ADR-003 对 **`ImplicitIntentDedupeKey::from_trigger(session_id, routing_message_id, user_text)`** 做 **`contains` → `append`**；**`routing_and_binding`** trace 回填 **`active_task_id`**，[`emit_http_chat_create_implicit_task_binding_trace`](../../crates/hc-agent/src/swarm_routing.rs) 携带 **`http_implicit_task_id`** 与 journal 相对路径。**首协同事务化占位**：同轮在隐式 id 下调用 [`ensure_http_implicit_task_plan_stub`](../../crates/hc-agent/src/persistence.rs)（缺省/空 body 时走 [`persist_task_artifacts`](../../crates/hc-agent/src/persistence.rs) + **`TaskPlan::awaiting_planner_input`**；在 **Planning Notes** 写入 **`routing_message_id` / `session_id`**；并 **直接 `work_items.push` 一条 `Planned` 占位项**（[`HTTP_IMPLICIT_WORK_ITEM_HOLDER_ID`](../../crates/hc-agent/src/planning.rs)，不把 plan 标为 **Drafted**）；**planner 在同一 `TaskPlan` 上新增其它 work item 后**，[`persist_task_artifacts`](../../crates/hc-agent/src/persistence.rs) 对 **内存克隆** 先调用 [`prune_http_implicit_work_item_placeholder`](../../crates/hc-agent/src/planning.rs) 再写盘，去掉占位行；**工作台** [`refresh_persisted_task_artifacts`](../../crates/hc-ui/src/lib.rs) 经 [`persist_task_artifacts_with_in_memory_prune`](../../crates/hc-agent/src/persistence.rs) 同步寄存（成功 persist 后对同一 `task_plan` prune，与 **`persist` 克隆渲染**一致；[`persist_task_artifacts`](../../crates/hc-agent/src/persistence.rs) 自身仍不修改借用方 `plan`）；已有非空正文不覆盖），使 HTTP **L2/L3** 同轮 **`task_plan.md` 节选**可不经人工预置即生效；失败仅 **`tracing::warn`**，不阻塞 chat。**回归**：[`l23_http_implicit_task_anchor_appends_task_plan_excerpt_when_file_preseeded`](../../crates/hc-service/src/chat.rs)、[`l23_http_implicit_prepare_materializes_task_plan_stub_for_prompt_excerpt`](../../crates/hc-service/src/chat.rs)、[`http_implicit_task_plan_stub_idempotent_when_body_nonempty`](../../crates/hc-agent/tests/unit/persistence.rs)、[`persist_task_plan_drops_http_implicit_holder_when_planner_adds_work_items`](../../crates/hc-agent/tests/unit/persistence.rs)、[`persist_task_artifacts_with_in_memory_prune_updates_live_plan`](../../crates/hc-agent/tests/unit/persistence.rs)。**仍待**：完整 work item 编排与对外结构化回灌（Phase 7）。
- [x] **`single_agent_execute` 退化（可选 P0）**：小 L2 可跳过新开 execution agent；**必须在 trace 中标明**，避免与 planner_only 语义混淆。——**HTTP chat**：L2/L3 在 `routing_and_binding` 之后追加 [`emit_http_chat_single_agent_execute_degenerate_trace`](../../crates/hc-agent/src/swarm_routing.rs)（`code`: `http_chat_single_agent_execute_degenerate`，`execution_mode`: `single_llm_route_agent`，含 `selected_agent_id`）；与工作台 `message_nomination_skipped_for_routing_tier` 对称。

---

## Phase 6：Assign 规则（ADR-003）

- [x] **Assign 胜者选择（ADR-003）**：确定性顺序在 **`hc-protocol::swarm::select_assign_winner_claim_index_v1`** —— （仅对 **eligible** 行）最高 **`capability_score`** → 最低 **`current_workload`** → 最低 claim 向量下标（同轮更早提交的稳定代理）；单行输入即胜者。载荷侧由 **`hc-agent::planning::TaskPlan::resolve_work_item_assignment`** 组装 eligible 候选（`claimed.submitted`、`capability_score` 经 **`claim_capability_eligible_for_p0_assign_v1`**，P0 floor 严格 **`>` `P0_ASSIGN_CAPABILITY_EXCLUSIVE_FLOOR`（`0.0`）**）。超时未选出胜者而 **退回 `claiming` / blocked 出口** 仍属编排与 **`blocked_exit_next_state_v1`**，不单列在此函数。
- [x] **P0 分值来源**：**`capability_score`** ↔ `WorkItemClaim::score`（元数据/static match 的外部产物；不可用则 **`0`** → **ineligible**，不赋值）；**`current_workload`** ↔ **`TaskPlan::current_workload_for_agent`**（该 agent 的有效 **`assigned`|`executing`** assignment 对应 **非终端** `WorkItem.lifecycle`）；终端含 **`done`/`cancelled`**。**禁止**在未修订 ADR 的情况下在其它 crate 复制排序语义。
- [x] **eligible threshold**：P0 仍为 **严格正分** (`> 0.0`) 方为 eligible；全 `≤ floor` ⇒ **不分配**，work item **保持 `claiming` 直至** replan / 新 claim / 超时策略（ADR-003 “Threshold progression”）；**逐轮自动降阈值** 明确 **P1**，不得当作 P0 默认。

---

## Phase 7：对外发言与工件落盘（ADR-005）

- [ ] **Speaker + 工件全链 + consolidate 编排**：完整 **L1 直出 / L2+L3 planner 对外 / 多 `execution_result`+`review_note` 必 consolidate** 的 **结构化**编排与 **`artifact_schema_v1`** 挂载仍待收口。
- [x] **HTTP L2/L3 发言附录（P0 软约束）**：[`prepare_chat_request`](../../crates/hc-service/src/chat.rs) 对 **L2/L3** 在 **可选**、**只读** `task_plan.md` 节选之后追加 **ADR-005 Outward speaker** 短文（**planner** 对外身份、多源时简要 synthesize）；**不构成** consolidate 闭环或结构化工件编排。
- [x] **`hc-memory` 任务 room 路径前缀**：[`persist_task_artifacts`](../../crates/hc-agent/src/persistence.rs) 经 `persist_room_memory` 写入的任务 room **compressed** 镜像采用 ADR-005 **建议路径族**：**`task/plan/task-plan.summary.md`**、**`task/plan/assignment-decision.{slug}.md`**（slug 与 coordination markdown 一致）；**`task/execution/`、`task/review/`** 预留给 **`execution_result` / `review_note`** 等新资产写入。

---

## Phase 8：测试（可与实现并行）

- [x] **路由 fixture**：给定输入 → 断言 `routing_tier` + `routing_reason`（及需要的 `routing_signals`）。——`hc-agent::swarm_routing` / orchestrator 单测。
- [x] **task binding fixture**：给定输入 → 断言 ADR-004 表中各字段（含 `task_binding_rule_version` 与 **`routing_rule_version` 同家族**）。——同上 + persistence/coordination 行构造单测。
- [x] **状态机**：`hc-protocol::swarm` —— `apply_work_item_lifecycle_command_v1`、`work_item_may_accept_new_assignment_v1`（单测：`planned → claiming → assigned → done`；`blocked` 经 `timeout_requeue`/`manual_cancel`/resume 等与 `blocked_exit_next_state_v1` 对齐）。**无双 active assignment**：仅 `claiming` 可作 `AssignWinner`，`assigned` 上二次 assign 为非法。**HTTP 全量 TaskPlan 生命周期编排**仍属 Phase 5；命令层「超时放弃 → `cancelled`」用 **`Cancel`** 表达。
- [x] **幂等**：`ImplicitIntentDedupeKey`（conversation + `triggering_message_id` + `intent_hash_v1` + version）在同会话、同 **`triggering_message_id`**、规范化等价正文下唯一；`load_implicit_intent_dedupe_keys`（`hc-agent::persistence`）以 **`HashSet`** 消费 journal，同一键重复追加 JSONL **只占一个逻辑 dedupe 槽**。**`hc-ui`** 与 **HTTP `hc-service`** 在 **`CreateImplicitTask` + L2/L3** 路径均先 **`contains`** 再 **`append`**（HTTP 使用 **`session_id`** + 本轮 **`routing_message_id`** 作 trigger，**`task_id`** 为 **`task.http.implicit.{ms}`** slug）。单测：`implicit_intent_duplicate_replay_same_message_id_collapses_dedupe_key`（`hc-protocol`）、`implicit_intent_journal_duplicate_appends_collapse_to_one_logical_key`。**任务 coordination 写库**：claims/assignments JSONL、`task_plan.md`（Phase 1）；隐式 intent journal 路径键仍为 **task_slug**。
- [x] **作用域**：当 **`MemoryScope::Task`** 且 `ContextMemoryQuery.room_anchor_ids` 非空时，`WorkspaceMemoryRetriever::retrieve` 收窄 indexed 命中——**task owned 平面 record** 须 `owner.id`（Task）属于「锚 id ∪ `anchor-room`/`anchor-related` 候选 room」；**task room assets** 须索引条目的 `room_id` 落在同一集合，避免并行 task 仅靠全文命中串台。单测：`workspace_retriever_task_room_anchor_filters_parallel_task_assets`。约定：与 HTTP chat 注入的 **task room 锚** 对齐时，宜让 **Task owner id 与同字符串 task room id** 一致（或服务层回填同一字面）。

---

## Phase 9：产品与文档收口

- [x] **`task-driven-product.md`**：与 ADR rollout 校对——「Decision Records」已链至 ADR-001～006；本节补充 **执行清单** [`swarm-p0-rollout.md`](./swarm-p0-rollout.md)，注明 **L2/L3 工单主链（TaskPlan → Work Item → …）仍在 Phase 5/6 rollout**。
- [x] **`docs/adr/README.md`**：新 ADR 入索引时维持相对路径（当前已符合）。
- [x] **命名**：代码与 API **统一 `task_id` / Task 语义**；`Swarm` 为文档与用户可见文案隐喻。实现侧 **`hc-protocol::swarm`** 为路由/工作项状态的 **Rust 模块名**（非第二套聚合 id）；全仓 **未发现 `swarm_id`** 与 `task_id` 同义双键。

---

## Part B：Experience lane（ADR-006，P1 / P1.5 / P2）

**硬性约束**：与 [ADR-006](../adr/ADR-006-experience-lane-boundary.md) 一致——**任务主链结束不依赖副链成功**；`L1` 不跑 playbook evolution；reflection / curator 严于 raw outcome（须 **租户允许 + task 授权 `enable_learning` + 通常高风险档位**）。

### B-0 策略与协议打底

- [ ] **策略模型**：tenant「是否允许 learning」、task 级 `enable_learning`、conversation 仅可提议不可越权（ADR-006 §2）。
- [ ] **`hc-protocol`（或等价）**：outcome 原始记录、proposal、accepted entry 的 **类型边界**；accepted 条目的 provenance 字段（`author`、`evidence_ref`、`created_from_work_item_id`、`status` 等）与 **弱归因** 字段（`influencing_refs`、`decision_context_refs`）；强归因仅 **命名实验**（A/B）启用。

### B-1 P1：Outcome 原始记录

- [ ] **触发**：以**持久化任务/work item 终态**为准（`done` / `cancelled`及 review 收口），非「对话看起来结束」（ADR-006 §1）。
- [ ] **标签来源**：仅硬信号 + 规则指标；LLM 只可写 **reflection 解释**，不可单独定 outcome 标签（ADR-006 §3）。
- [ ] **L2**：默认关；若开 raw capture，须 **限额/采样/per-tenant 字节上限**（ADR-006 §3）。

### B-2 P1.5：Delta proposal（不自动 merge）

- [ ] proposal 仅存 **delta / proposal** 分区，**不可**直达 canonical；
- [ ] curator 缺席时 **无 merge**、`task.playbook.md` 若为投影则不含未接纳 proposal。

### B-3 P2：Curator merge + 投影 +读闸门

- [ ] **真源**：结构化 canonical（如 JSONL/sidecar），`strategy_id` 级归因；`task.playbook.md` **仅 accepted 投影**；
- [ ] **单 writer**：curator merge 串行队列 + playbook **version/CAS**，失败重试、禁止静默覆盖（ADR-006 §6）；
- [ ] **读取**：planner / assign / review **默认仅** accepted；proposal 不进高权重 slot（ADR-006 §5 read gates + §9 预算）。

### B-4 异步作业与可观测（贯穿 P1～P2）

- [ ] 副链作业 **non-blocking**，带 `trace_id`、可与主链 trace join；有限重试、失败/死信 **可见**（ADR-006 §8）。

### B-5 测试与门禁

- [ ] 副链全挂时任务仍可标 **完成**；
- [ ] merge 竞态下无 last-winner 腐化；
- [ ] read gate：**默认** proposal 不进入 planner context fixture。

### B-6 升层与其它（次于 task playbook）

- [ ] 「出现 N 次」仅 **候选提名**，晋升须带 scope / confidence / `review_after` / `expires_at` 且有 **executor**（人/定时/policy）（ADR-006 §10）。

**经验复盘节奏（轻量）**：可与 **Part A M3/M4** 对齐做一次 outcome / delta review，**不强制**每期里程碑长文——模板控制在「有效/无效/下轮保留至多 3 条 proposal」即可。

---

## 里程碑建议

### Part A（执行主路径，默认必须）

| 里程碑 | 交付物 |
| **M1** | Phase 0–1 + 幂等/状态机测试骨架 |
| **M2** | Phase 3–4 + L1 旧路径仍通过 CI |
| **M3** | Phase 5–6：L2 端到端（隐式 WI + claim/assign + planner 对外说） |
| **M4** | Phase 7 + L3 显式入口 + Phase 8 测全 |

### Part B（经验副链，默认关闭，顺延不挡 A）

| 里程碑 | 交付物 |
|--------|--------|
| **EL-M1** | B-0 + B-1（outcome raw 可走通即可，feature 默认关） |
| **EL-M2** | B-2（proposal-only 链路） |
| **EL-M3** | B-3 + B-4 + B-5 |

**EL-M4+（可选）**：B-6（project/global 晋升）须在 task 级 playbook 稳定后再排。

---

## 相关决策文档

- [ADR 索引](../adr/README.md)
- [ADR-001 路由分层](../adr/ADR-001-task-routing-tier.md)
- [ADR-002 planner_only](../adr/ADR-002-planner-only-semantics.md)
- [ADR-003 持久化与幂等](../adr/ADR-003-work-item-persistence-and-idempotency.md)
- [ADR-004 会话·任务·房间作用域](../adr/ADR-004-conversation-task-room-scope.md)
- [ADR-005 对外发言与工件](../adr/ADR-005-outward-speaker-and-artifact-schema.md)
- [ADR-006 经验副链边界](../adr/ADR-006-experience-lane-boundary.md)
- [任务驱动产品概要](../task-driven-product.md)
