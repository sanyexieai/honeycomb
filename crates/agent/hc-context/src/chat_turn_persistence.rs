//! 聊天轮次落盘抽象：各入口（如 `hc-cli` / `hc-agent-cli`）自行选择是否与 MemoryRoom 资产树一致。

use std::path::PathBuf;

use anyhow::Result;
use hc_store::store::WorkspaceNamespace;

use crate::{
    ChatCaptureOptions, MemoryRoom, prepare_chat_capture_room, persist_chat_turn_assistant_reply,
    persist_chat_turn_user_message,
};

/// 单轮用户/助手文本的持久化策略；**不负责** LLM 生成或记忆检索查询构造。
pub trait ChatTurnPersistence {
    fn enabled(&self) -> bool;

    /// 会话开始时可创建目录或元数据；默认无操作。
    fn init_session(&self) -> Result<()> {
        Ok(())
    }

    fn persist_user_turn(&self, turn_index: usize, content: &str) -> Result<()>;
    fn persist_assistant_turn(&self, turn_index: usize, content: &str) -> Result<()>;

    /// 与 MemoryRoom 生态互操作（如 CLI 节点、偏好落盘）；纯文件实现返回 `None`。
    fn as_memory_room(&self) -> Option<&MemoryRoom> {
        None
    }
}

/// `hc-cli` 默认：沿用现有 **MemoryRoom + Artifact** 布局（与 `prepare_chat_capture_room` 一致）。
#[derive(Debug, Clone)]
pub struct MemoryRoomChatTurnSink {
    root: PathBuf,
    workspace_namespace: WorkspaceNamespace,
    room: Option<MemoryRoom>,
}

impl MemoryRoomChatTurnSink {
    pub fn try_new(
        root: impl Into<PathBuf>,
        workspace_namespace: WorkspaceNamespace,
        options: &ChatCaptureOptions,
    ) -> Result<Self> {
        let root = root.into();
        let room = prepare_chat_capture_room(&root, workspace_namespace.clone(), options)?;
        Ok(Self {
            root,
            workspace_namespace,
            room,
        })
    }
}

impl ChatTurnPersistence for MemoryRoomChatTurnSink {
    fn enabled(&self) -> bool {
        self.room.is_some()
    }

    fn persist_user_turn(&self, turn_index: usize, content: &str) -> Result<()> {
        if let Some(room) = &self.room {
            persist_chat_turn_user_message(
                self.root.clone(),
                self.workspace_namespace.clone(),
                room,
                turn_index,
                content.to_owned(),
            )?;
        }
        Ok(())
    }

    fn persist_assistant_turn(&self, turn_index: usize, content: &str) -> Result<()> {
        if let Some(room) = &self.room {
            persist_chat_turn_assistant_reply(
                self.root.clone(),
                self.workspace_namespace.clone(),
                room,
                turn_index,
                content.to_owned(),
            )?;
        }
        Ok(())
    }

    fn as_memory_room(&self) -> Option<&MemoryRoom> {
        self.room.as_ref()
    }
}
