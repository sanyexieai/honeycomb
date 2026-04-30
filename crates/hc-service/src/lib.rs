//! Application service layer shared by API, CLI, and future transports.

pub mod agent;
pub mod chat;
pub mod conversation;
pub mod scheduler;
pub mod tool;

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
}
