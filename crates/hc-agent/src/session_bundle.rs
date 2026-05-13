//! 本会话专属目录：`agent-runtime/sessions/<slug>/`（与全局 `agents/*.md` 分离）。
//!
//! 首次使用时创建占位结构（`status: temporary`、说明文件），对话落盘在 `conversations/` 下。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hc_store::store::{StoredMarkdown, WorkspaceNamespace, WorkspaceStore};

use crate::profile::{AgentDefinitionLayer, AgentProfile, AgentProfileFrontmatter};
use crate::runtime_layout::agent_conversation_slug;

pub const SESSIONS_SUBDIR: &str = "sessions";

pub fn session_runtime_rel(session_key: &str) -> PathBuf {
    let slug = agent_conversation_slug(session_key);
    Path::new(crate::runtime_layout::USER_AGENT_RUNTIME_REL)
        .join(SESSIONS_SUBDIR)
        .join(slug)
}

pub fn session_agent_md_rel(session_key: &str) -> PathBuf {
    session_runtime_rel(session_key).join("agent").join("agent.md")
}

pub fn session_description_md_rel(session_key: &str) -> PathBuf {
    session_runtime_rel(session_key).join("agent").join("description.md")
}

/// 创建本会话目录树；若尚无 `agent/agent.md` 则写入占位描述（`status: temporary`）。
pub fn ensure_session_agent_bundle(
    store: &WorkspaceStore,
    namespace: &WorkspaceNamespace,
    session_key: &str,
) -> Result<()> {
    let root = store.resolve_in_namespace(namespace, session_runtime_rel(session_key));
    fs::create_dir_all(root.join("agent"))?;
    fs::create_dir_all(root.join("conversations"))?;
    fs::create_dir_all(root.join("memory"))?;
    fs::create_dir_all(root.join("scripts"))?;

    let readme = root.join("README.txt");
    if !readme.exists() {
        let slug = agent_conversation_slug(session_key);
        fs::write(
            &readme,
            format!(
                "本会话 agent 根目录（hc-agent）\n\
                 session_key={session_key}\n\
                 slug={slug}\n\
                 - agent/agent.md       本会话 agent 配置（占位可改）\n\
                 - agent/description.md 能力简述（占位可改）\n\
                 - conversations/       对话轮次落盘\n\
                 - memory/、scripts/    结构占位\n\
                 \n\
                 可选：在本目录安装 hc-agent 入口（复制/软链/快捷方式）\n\
                 - HC_AGENT_SESSION_BIN_MODE=off|copy|symlink|shortcut\n\
                 - HC_AGENT_SESSION_BIN_SOURCE=源 hc-agent 可执行文件路径（可选）\n\
                 - HC_AGENT_SESSION_BIN_REFRESH=1 强制覆盖已存在的入口\n"
            ),
        )
        .context("write session README.txt")?;
    }

    let desc_path = root.join("agent").join("description.md");
    if !desc_path.exists() {
        fs::write(
            &desc_path,
            "(占位) 能力说明待补充。可编辑本文件或同目录 agent.md 中的 capability_description。\n",
        )
        .context("write session description.md")?;
    }

    let agent_md = root.join("agent").join("agent.md");
    if !agent_md.exists() {
        let slug = agent_conversation_slug(session_key);
        let id = format!("agent.session.{slug}");
        let body = "本会话专用 agent：请在本目录补充行为边界、工具与记忆策略说明。\n\
            \n\
            全局共享能力仍在工作区 `agent-definitions/`；跨会话共享覆盖仍在用户 `agents/`。\n";
        let fm = format!(
            "---\n\
             id: {id}\n\
             type: agent_profile\n\
             title: 本会话 Agent（占位）\n\
             kind: other\n\
             priority: 2000000000\n\
             status: temporary\n\
             tags: []\n\
             capability_description: 能力范围待定义；请编辑 agent/description.md 与同目录 agent.md。\n\
             ---\n\n\
             {body}"
        );
        fs::write(&agent_md, fm).context("write session agent.md")?;
    }

    crate::session_hc_agent_bin::maybe_install_session_hc_agent_bin(
        &root,
        store.root(),
        &crate::session_hc_agent_bin::SessionHcAgentBinOptions::from_env(),
    )?;

    Ok(())
}

/// 读取本会话 `agent/agent.md`；若目录尚未初始化则返回 `None`。
pub fn load_session_agent_profile(
    store: &WorkspaceStore,
    namespace: &WorkspaceNamespace,
    session_key: &str,
) -> Result<Option<AgentProfile>> {
    let rel = session_agent_md_rel(session_key);
    let abs = store.resolve_in_namespace(namespace, &rel);
    if !abs.exists() {
        return Ok(None);
    }
    let stored: StoredMarkdown<AgentProfileFrontmatter> =
        store.read_markdown_in_namespace(namespace, &rel)?;
    let mut profile = AgentProfile::from_document(stored.frontmatter, stored.body)?;
    profile.definition_layer = AgentDefinitionLayer::SessionRuntime;
    profile.relative_path = rel.to_string_lossy().replace('\\', "/");

    let desc_rel = session_description_md_rel(session_key);
    let desc_abs = store.resolve_in_namespace(namespace, &desc_rel);
    if desc_abs.is_file() {
        if let Ok(text) = fs::read_to_string(&desc_abs) {
            let t = text.trim();
            if !t.is_empty() {
                profile.capability_description = t.to_owned();
            }
        }
    }

    Ok(Some(profile))
}
