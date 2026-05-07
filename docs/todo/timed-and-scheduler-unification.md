# Timed / Countdown / Scheduler 统一 TODO

目标：把“定时提醒、倒计时序列、计划任务”收敛到同一套调度语义，减少分叉逻辑，统一观测与重试。

## Phase 0：最小统一入口（本次已落地）

- [x] 为提醒能力补充内置规则，避免仅依赖租户配置导致 miss。
- [x] 将内置提醒规则接入 `ToolRoutingTags` 的默认加载流程。
- [x] 增加“`两秒后叫我` 能命中提醒规则”的回归测试。

## Phase 1：模型统一（数据结构）

- [x] 在 `timed_turn` 内部引入统一 `TimedTaskSpec/TimedRunSpec`，让提醒与倒计时共享同一入库路径（仍落 `followup`，对外行为不变）。
- [ ] 抽象统一 `Run`（一次执行实例）状态流转：`queued -> running -> fired/done/failed`。
- [ ] 给 `Run` 引入幂等键：`task_id + fire_at + sequence_index`。

## Phase 2：执行统一（调度流水线）

- [x] `timed_turn` 写入 followup 时同步镜像到 `hc-scheduler`（`ScheduledTask`，`target=event:timed.followup`），为统一队列执行铺路。
- [x] interactive timed 投递改为“轮询调度分发 + followup 状态观察”，去掉提醒专用线程路径（倒计时与提醒共用同一投递循环）。
- [ ] `schedule watch/dispatch` 成为统一 worker，消费所有到期 `Run`（当前 interactive 已复用调度分发，后台常驻 watcher 仍待完全接管）。
- [ ] 将倒计时 tick 统一表达为序列化 `Run`，不再单独维护一条并行路径。

## Phase 3：投递统一（CLI/API）

- [x] `timed.followup` 调度分发时同步写入 `conversation event`（`timed.followup.fired`），打通 headless 事件链路。
- [x] `hc-cli schedule watch` 在调度回执中识别 `followup:*`，读取 fired followup 并输出 `assistant>` 提醒文案。
- [x] 在 `hc-service::scheduler` 下沉 `fired_followup_messages_from_receipts`，`timed_turn` 与 `hc-cli` 复用同一回执解码逻辑。
- [x] `hc-cli schedule` 内部调度回执改为强类型 `SchedulerDispatchReceipt`，移除 `serde_json::Value` 中转。
- [x] 在 `hc-service::scheduler` 增加 `dispatch_fired_followup_messages_from_receipts`（sink 回调），`timed_turn` 与 `hc-cli` 复用同一投递循环。
- [x] 新增 `FollowUpMessageSink` trait，并在 `timed_turn` / `hc-cli` 中以 sink 实现接入统一分发函数。
- [x] 补充 `NoopFollowUpMessageSink`（headless adapter）与 `CollectFollowUpMessageSink`（测试/采集 adapter）。
- [x] `hc-api` scheduler loop 与 `/v1/schedules/dispatch-*` 显式接线 `dispatch_fired_followup_messages_headless`。
- [x] `hc-api` 调度日志补充 `delivered_followups` 指标（loop + dispatch API debug）。
- [x] `hc-api` 增加 `HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE=headless|off`，并在 loop/API 统一按策略分发 followup 消息。
- [x] 文档补充 `HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE` 与 API/loop 行为矩阵（`docs/scheduled-tasks.md`）。
- [x] 部署示例补充 `HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE`（`deploy/hc-api/docker-compose.yml`）。
- [x] 环境变量模板补充 `HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE`（`deploy/hc-api/.env.example`、`.env.example`）。
- [ ] 统一投递适配层：`interactive(stdout)` 与 `headless(event/store)`（当前已完成 sink 抽象、显式接线、指标、策略与文档；后续可扩展更细粒度模式）。
- [ ] 统一失败重试策略与最大重试次数。
- [ ] 增加运行事件日志，支持会话恢复后补发。

## Phase 4：可观测性与治理

- [ ] 增加调度指标：待执行、成功率、失败率、延迟。
- [ ] 增加运维命令：按任务/序列查询与取消。
- [ ] 补齐跨模块测试：解析、入队、调度、投递、重试。

