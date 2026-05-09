# Timed / Countdown / Scheduler 统一 TODO

目标：把“定时提醒、倒计时序列、计划任务”收敛到同一套调度语义，减少分叉逻辑，统一观测与重试。

## Phase 0：最小统一入口（本次已落地）

- [x] 为提醒能力补充内置规则，避免仅依赖租户配置导致 miss。
- [x] 将内置提醒规则接入 `ToolRoutingTags` 的默认加载流程。
- [x] 增加“`两秒后叫我` 能命中提醒规则”的回归测试。

## Phase 1：模型统一（数据结构）

- [x] 在 `timed_turn` 内部引入统一 `TimedTaskSpec/TimedRunSpec`，让提醒与倒计时共享同一入库路径（仍落 `followup`，对外行为不变）。
- [x] 抽象统一 `Run`（一次执行实例）状态流转（协议层）：`hc-protocol::timed_run::TimedRunLifecycle`（`queued`/`running`/`fired`/`done`/`failed`）+ `hc-service::scheduler::timed_run_lifecycle_resolve`（合并 `FollowUpStatus` 与 `ScheduledRunStatus`）；调度器磁盘上的 `ScheduledRun` 仍为 `Queued/Running/Succeeded/Failed`。
- [x] 给 `Run` 引入幂等键：`task_id + fire_at + sequence_index` → `timed_run_idempotency_key_v1`（FNV-1a 稳定摘要，前缀 `timed.idem.v1.`）；写入 follow-up `payload` 与 `timed.followup` 调度 `target.args`；`ScheduledFollowUpRunSpec` 含 `logical_task_id` + `sequence_index`。

## Phase 2：执行统一（调度流水线）

- [x] `timed_turn` 写入 followup 时同步镜像到 `hc-scheduler`（`ScheduledTask`，`target=event:timed.followup`），为统一队列执行铺路。
- [x] interactive timed 投递改为“轮询调度分发 + followup 状态观察”，去掉提醒专用线程路径（倒计时与提醒共用同一投递循环）。
- [x] **CLI dispatcher 对齐 API**：`hc-cli schedule dispatch-due`、`schedule watch`、`schedule dispatch-queued` 调用 `hc_service::scheduler::{dispatch_due_scheduled_runs, dispatch_queued_scheduled_runs}`，与同 crate 被 `hc-api` `/v1/schedules/dispatch-*` 复用的实现对齐（不再手写 `queue_due → queued_runs → dispatch_scheduled_run` 分叉）。
- [x] `schedule watch/dispatch` 成为统一 worker，消费所有到期 `Run`：在 **`hc-cli` 交互聊天**会话内，`handle_chat` 启动唯一后台 ticker（同上 `dispatch_due_scheduled_runs` + 回执解码），与同命名空间 CLI `schedule watch` 同源；timed 计划在 REPL 中仅 **`Interactive` 入库**（countdown / reminder 同上），不再默认为每次请求另起 **`dispatch_followups_until_fired` 专线程**。**`TimedDeliverMode::InteractiveSelfContained`** 仍可在无 REPL ticker 的宿主中恢复「单次后台直至全部 fire」语义。若并行运行 **`schedule watch` 与同租户 REPL**，会重复派发：将 **`HC_CLI_CHAT_SCHEDULER_ENABLED=false`**（或 `off`/`0`/`no`）可关闭 REPL 内 tick，仅保留 **watch**。

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
- [x] **投递 sink 统一入口**：`FollowUpMessageSink`、`dispatch_fired_followup_messages_*` 等已覆盖 **`hc-cli`/API loop** 的 interactive(stdout) 与 **`hc-api`/headless** 的 event/store 路径。
- [x] **Webhook 投递（hc-api）**：`HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE=webhook` + **`HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL`**，对已 fire follow-up 文案逐条 **HTTP POST JSON**（`User-Agent: honeycomb-hc-api/<version>`）；可选 **`HC_SCHEDULER_FOLLOWUP_WEBHOOK_BEARER_TOKEN`**（`Authorization: Bearer`）；可选 **`HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS`**（默认 30，限制 1–300）。失败则调度 worker **报错**；成功条数计入 **`api_followup_messages_delivered_total`** / 遗留 **`api_followup_headless_messages_delivered_total`**（同值）。**队列等异步投递**仍可外挂接收端实现，不在此 crate 内置 broker。
- [x] **timed.followup 镜像任务**：入队幂等——若已有 **Pending** follow-up 与相同 `timed_run_idempotency_key_v1`，`persist_scheduled_followup_task` **不再**重复写入；派发失败时在 `failure_count <= max_retries` 下 **重新激活**镜像 `ScheduledTask`（`next_fire_at = 失败时刻 + retry_delay_seconds`）；策略可通过 `HC_TIMED_FOLLOWUP_SCHEDULE_MAX_RETRIES`、`HC_TIMED_FOLLOWUP_SCHEDULE_RETRY_DELAY_SECONDS` 覆盖（persist 时生效；默认沿用 `SchedulePolicy`）。
- [x] 增加运行事件日志，支持会话恢复后补发：`timed.followup.fired` 事件可持续化；**CLI** `hc-cli schedule followups replay-events` 按 `created_at_unix` 过滤并重放文案（增量 `--since-created-unix`）；**API** `GET /v1/schedules/followup-fired-events` 返回同一数据源（增量 `since_created_at_unix`，默认 0）；**CLI** `schedule stats` 与 **API** `GET /v1/schedules/operational-stats` 在 **磁盘快照字段**上一致；**API** 另在应答中并入 **hc-api 进程内** `api_*` 计数（CLI 走 `scheduler_operational_stats` 时这些计数为 **0**，非 JSON 导出时通常为省略）。

## Phase 4：可观测性与治理

- [x] **轻量调度快照（CLI）**：`hc-cli schedule stats` 输出 follow-up / schedule / queued run 计数（当前租户用户命名空间）。
- [x] **快照级「待执行」队列深度**：`followup_pending` / `followup_pending_due`（follow-up）、`run_queued` / `run_running`（scheduled runs）已进入 **`SchedulerOperationalStats`**、`GET /v1/schedules/operational-stats` 与 **`honeycomb_scheduler_*`** OpenMetrics gauge；多租户汇总需在抓取侧用 PromQL 聚合。
- [x] **派生率（进程内计数，hc-api）**：`dispatch-due` / `dispatch-queued` 与 **`HC_SCHEDULER_ENABLED`** 内置 loop 在成功或失败（含 `spawn_blocking` join 失败）时按命名空间累加；字段 `api_dispatch_*_total` / `api_scheduler_loop_tick_*_total` 与对应 `honeycomb_scheduler_api_*` gauge 在 **`GET /v1/schedules/operational-stats`** / **prometheus** 读时合并。**不落盘**，**重启归零**。
- [x] **进程内最近一次成功 worker 耗时（毫秒）**：`api_dispatch_due_last_worker_wall_ms`、`api_dispatch_queued_last_worker_wall_ms`、`api_scheduler_loop_tick_last_worker_wall_ms` 与同名的 `honeycomb_scheduler_*_last_worker_wall_ms` gauge（仅度量 **`spawn_blocking` 闭包内**墙体时钟）；失败路径 **不刷新**最近一次成功样本。
- [x] **`dispatch-due` / `dispatch-queued` / 内置 scheduler loop tick** 成功路径 wall-time histogram（进程内）：仅在 **`spawn_blocking`** worker **成功**时取样；毫秒桶 10 / 50 / 100 / 500；字段 `api_dispatch_*_worker_wall_ms_histogram`、**`api_scheduler_loop_tick_worker_wall_ms_histogram`**（仅 **`HC_SCHEDULER_ENABLED`** tick）与同前缀 OpenMetrics **`honeycomb_scheduler_api_*_worker_wall_ms_*`** 在读时与其它 `api_*` 并入，**不落盘**。
- [x] **计划 Run 派达迟滞（进程内直方图）**：每次 `hc_service::scheduler::dispatch_scheduled_run` 将 `(now - scheduled_for_unix) * 1000` 毫秒（下限 0）记入 `scheduled_run_dispatch_slip_ms_histogram`；OpenMetrics `honeycomb_scheduler_scheduled_run_dispatch_slip_ms_*`；**与 hc-api 进程 `api_*` 不同**，**hc-cli 与 API 同进程**凡调用调度分发均累计，**不落盘、进程结束即失**；更粗队列→执行全链路由外部 Tracing 承接（仍可选）。
- [x] **hc-api follow-up 投递累计（进程内）**：JSON **`api_followup_messages_delivered_total`**（主）与 **`api_followup_headless_messages_delivered_total`**（遗留，同值）；Prometheus 同名主从 gauge（**headless** + **webhook** 成功条数；读时合并；**不落盘**）。
- [x] **磁盘快照补强**：`scheduler_operational_stats` / **`GET /v1/schedules/operational-stats`** / **`hc-cli schedule stats`** 提供 follow-up (`fired`/`cancelled`/`failed`)，schedule (`paused`/`cancelled`)，run (`running`/`succeeded`/`failed`/`cancelled`) 计数，便于人工排障与非时间序列巡检。
- [x] **运维命令**：`hc-cli schedule followups list|cancel` — 查询、取消 Pending follow-up，并联动取消 `timed.followup.*` 镜像计划。
- [x] 补齐跨模块测试：解析、入队、调度、投递、重试。（含：`list_timed_followup_fired_events_since_created`、`persist_scheduled_followup_*`、cancel、re-arm、replay 语义相关用例。**hc-api**：`merge_scheduler_operational_stats_with_api_counters` 与进程内映射合并的单元测试 `api_process_counter_merge_tests`。）

