//! `hc-agent` 专用聊天落盘：**不**写入 MemoryRoom 资产树，在 `agent-runtime/sessions/<slug>/conversations/` 下写扁平 Markdown。

use std::fs;

use std::path::PathBuf;

use anyhow::{Context, Result};
use hc_context::ChatTurnPersistence;
use hc_store::store::{WorkspaceNamespace, WorkspaceStore};

use crate::runtime_layout::ensure_user_agent_runtime_layout;
use crate::session_bundle::{ensure_session_agent_bundle, session_runtime_rel};

fn agent_chat_persist_enabled_from_env() -> bool {
    std::env::var("HC_AGENT_CHAT_PERSIST")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

/// 每轮写入 `agent-runtime/sessions/<session_slug>/conversations/turns/0001-user.md` 等；与 `hc-cli` 的 MemoryRoom 结构 **不同**。
#[derive(Debug, Clone)]
pub struct AgentRuntimeChatTurnSink {
    enabled: bool,
    turns_dir: PathBuf,
    session_key: String,
    workspace_namespace: WorkspaceNamespace,
    workspace_root: PathBuf,
}

impl AgentRuntimeChatTurnSink {
    pub fn try_new(
        workspace_root: impl Into<PathBuf>,
        namespace: &WorkspaceNamespace,
        session_key: &str,
    ) -> Result<Self> {
        let root = workspace_root.into();
        let store = WorkspaceStore::new(&root);
        ensure_user_agent_runtime_layout(&store, namespace).context("agent-runtime 目录")?;
        ensure_session_agent_bundle(&store, namespace, session_key).context("会话 agent 目录")?;

        let enabled = agent_chat_persist_enabled_from_env();
        let conversations = store.resolve_in_namespace(
            namespace,
            session_runtime_rel(session_key).join("conversations"),
        );
        let turns_dir = conversations.join("turns");

        if enabled {
            fs::create_dir_all(&turns_dir)?;
        }

        Ok(Self {
            enabled,
            turns_dir,
            session_key: session_key.to_owned(),
            workspace_namespace: namespace.clone(),
            workspace_root: root,
        })
    }

    fn write_turn(&self, turn_index: usize, role: &str, content: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self
            .turns_dir
            .join(format!("{turn_index:04}-{role}.md"));
        let body = format!(
            "---\nrole: {role}\nturn: {turn_index}\n---\n\n{content}"
        );
        fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

impl ChatTurnPersistence for AgentRuntimeChatTurnSink {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn init_session(&self) -> Result<()> {
        let store = WorkspaceStore::new(&self.workspace_root);
        ensure_session_agent_bundle(&store, &self.workspace_namespace, &self.session_key)?;
        if self.enabled {
            fs::create_dir_all(&self.turns_dir)?;
        }
        Ok(())
    }

    fn persist_user_turn(&self, turn_index: usize, content: &str) -> Result<()> {
        self.write_turn(turn_index, "user", content)
    }

    fn persist_assistant_turn(&self, turn_index: usize, content: &str) -> Result<()> {
        self.write_turn(turn_index, "assistant", content)
    }
}
