//! Application service layer shared by API, CLI, and future transports.

pub mod agent;
pub mod chat;
pub mod conversation;
pub mod human_inbox;
pub mod index;
pub mod room_routing;
pub mod scheduler;
pub(crate) mod session_swarm_state;
pub mod timed_turn;
pub mod tool;
pub mod tool_execution;
pub mod tool_turn;
pub mod turn;
pub mod turn_router;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub workspace_root: PathBuf,
}

impl ServiceConfig {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }

    /// 使用 [`hc_bootstrap::workspace_root`]（`HC_WORKSPACE_ROOT` 与默认目录）构造配置。
    pub fn from_env() -> Self {
        Self::new(hc_bootstrap::workspace_root())
    }
}
