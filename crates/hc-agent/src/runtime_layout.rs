//! 用户命名空间下的 agent 运行时目录（记忆、脚本等），与能力定义目录分离。
//!
//! 本会话独占内容在 `agent-runtime/sessions/<slug>/`（见 [`crate::session_bundle`]）；此处仍保证顶层 `memory`/`scripts` 兼容旧用法。

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use hc_store::store::{WorkspaceNamespace, WorkspaceStore};

pub const USER_AGENT_RUNTIME_REL: &str = "agent-runtime";

pub fn user_agent_runtime_dir(store: &WorkspaceStore, namespace: &WorkspaceNamespace) -> PathBuf {
    store.resolve_in_namespace(namespace, USER_AGENT_RUNTIME_REL)
}

/// 创建 `agent-runtime/memory` 与 `agent-runtime/scripts`（幂等）。
pub fn ensure_user_agent_runtime_layout(
    store: &WorkspaceStore,
    namespace: &WorkspaceNamespace,
) -> Result<()> {
    let root = user_agent_runtime_dir(store, namespace);
    fs::create_dir_all(root.join("memory"))?;
    fs::create_dir_all(root.join("scripts"))?;
    Ok(())
}

/// 用于 `agent-runtime/conversations/<slug>/` 等路径的目录名（归一化 session / 会话键）。
pub fn agent_conversation_slug(key: &str) -> String {
    slug_room_for_path(key)
}

fn slug_room_for_path(room_id: &str) -> String {
    let mapped: String = room_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = mapped.trim_matches('_').trim_matches('.');
    if trimmed.is_empty() {
        "default".to_owned()
    } else {
        trimmed.chars().take(128).collect()
    }
}
