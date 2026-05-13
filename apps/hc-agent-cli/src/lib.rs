//! Honeycomb **agent 专用** CLI：库入口与二进制共用 [`run()`]，便于测试与脚本调用（与 `hc-cli` 同构）。
//!
//! ## 与 `hc-cli` 的差异
//!
//! - **`hc-cli`**：统一入口，面向「全局」Honeycomb（聊天、记忆、工具编排等尽可能多能力）。
//! - **`hc-agent-cli`**：以 **agent** 为主（`list` / `paths` / `agent-runtime` 与 **`chat`**）；不写子命令时默认进入 **`chat`**。
//!   LLM 走 [`hc_service::chat::handle_chat_request`]；**轮次落盘**在 `agent-runtime/sessions/<slug>/conversations/turns/`，与 `hc-cli` 的 MemoryRoom 资产树 **分离**。
//!   可选：通过环境变量在本会话目录下安装 `hc-agent` 入口（复制 / 软链 / Windows 快捷方式），见 `hc_agent::session_hc_agent_bin` 模块文档（`HC_AGENT_SESSION_BIN_*`）。
//!
//! ## 路线图
//!
//! 后续可能将 **`hc-cli` 抽到 agent 上层**，作为更粗粒度的编排入口；本包则稳定承担 agent 子域 CLI。

mod chat;
mod cli;

/// 与 `hc_cli::run()` 对齐：安装后运行 `hc-agent`；仓库根 `cargo run -p hc-agent-cli`。
/// **无子命令时默认进入对话**（等同 `chat`）。其它：`list`、`paths`、`init-runtime`、`chat`（可带 `--agent-id` / `-m`）。
pub fn run() -> anyhow::Result<()> {
    cli::run()
}
