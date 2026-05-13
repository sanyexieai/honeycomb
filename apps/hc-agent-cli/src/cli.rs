//! `hc-agent` 可执行文件：agent 目录、合并列表与专用对话。

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::chat;
use hc_agent::{
    AgentCatalog, AgentDefinitionLayer, AgentRepository, WORKSPACE_AGENT_DEFINITIONS_DIR,
    ensure_user_agent_runtime_layout, load_workspace_capability_profiles, user_agent_runtime_dir,
};
use hc_service::transport::{
    WorkspaceNamespace, load_local_env_file, tenant_id_from_env, user_id_from_env, workspace_root,
};
use hc_service::transport::workspace_markdown_index::WorkspaceStore;

#[derive(Parser)]
#[command(
    name = "hc-agent",
    about = "Honeycomb agent 专用 CLI（目录与对话）",
    subcommand_required = false,
    arg_required_else_help = false
)]
pub struct Cli {
    /// 未写子命令时默认进入对话（等价于 `chat`）
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// 列出合并后的 agent（工作区能力 + 用户运行时）
    List {
        #[arg(long, help = "仅列出工作区 agent-definitions")]
        workspace_only: bool,
        #[arg(long, help = "仅列出用户 agents/ 目录")]
        user_only: bool,
        #[arg(
            long,
            help = "合并列表时包含本会话 agent-runtime/sessions/<slug>/agent/（与 HC_SESSION_ID 一致）"
        )]
        session: Option<String>,
        #[arg(long, help = "JSON 输出")]
        json: bool,
    },
    /// 初始化用户命名空间下的 agent-runtime/{memory,scripts}
    InitRuntime,
    /// 打印工作区能力定义根路径与用户运行时根路径
    Paths,
    /// 进入对话（与 hc-api `/chat` 同源管线；本入口面向 agent 域，默认自动路由 agent）
    Chat {
        #[arg(long, help = "固定使用的 agent id（不填则按输入自动路由）")]
        agent_id: Option<String>,
        #[arg(long)]
        domain_id: Option<String>,
        #[arg(
            long,
            short = 'm',
            help = "只发一条用户消息后退出（非交互，便于脚本）"
        )]
        message: Option<String>,
    },
}

pub fn run() -> Result<()> {
    let _ = load_local_env_file();
    let cli = Cli::parse();
    let root = workspace_root();
    let namespace = WorkspaceNamespace::new(tenant_id_from_env(), user_id_from_env());
    let store = WorkspaceStore::new(&root);

    let command = cli.command.unwrap_or(Commands::Chat {
        agent_id: None,
        domain_id: None,
        message: None,
    });

    match command {
        Commands::List {
            workspace_only,
            user_only,
            session,
            json,
        } => {
            if workspace_only && user_only {
                anyhow::bail!("不能同时指定 --workspace-only 与 --user-only");
            }
            if session.is_some() && (workspace_only || user_only) {
                anyhow::bail!("--session 仅在与完整合并列表联用时有效（不要与 --workspace-only / --user-only 同用）");
            }
            let profiles = if workspace_only {
                load_workspace_capability_profiles(&store)?
            } else if user_only {
                AgentRepository::with_namespace(&root, namespace.clone()).list_profiles()?
            } else {
                let session_key = session
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_owned())
                    .or_else(|| {
                        std::env::var("HC_SESSION_ID")
                            .ok()
                            .map(|v| v.trim().to_owned())
                            .filter(|v| !v.is_empty())
                    });
                AgentCatalog::new(&root, namespace.clone())
                    .list_effective_profiles_with_session(session_key.as_deref())?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&profiles)?);
            } else {
                for p in profiles {
                    let layer = match p.definition_layer {
                        AgentDefinitionLayer::UserRuntime => "user_runtime",
                        AgentDefinitionLayer::WorkspaceCapability => "workspace_capability",
                        AgentDefinitionLayer::SessionRuntime => "session_runtime",
                    };
                    println!("{}\t{}\t{layer}", p.id, p.name);
                }
            }
        }
        Commands::InitRuntime => {
            ensure_user_agent_runtime_layout(&store, &namespace)?;
            println!(
                "ok: {}",
                user_agent_runtime_dir(&store, &namespace).display()
            );
        }
        Commands::Paths => {
            println!(
                "workspace_definitions: {}",
                PathBuf::from(&root)
                    .join(WORKSPACE_AGENT_DEFINITIONS_DIR)
                    .display()
            );
            println!(
                "user_agent_runtime: {}",
                user_agent_runtime_dir(&store, &namespace).display()
            );
            println!(
                "session_hc_agent_bin: set HC_AGENT_SESSION_BIN_MODE=copy|symlink|shortcut (optional HC_AGENT_SESSION_BIN_SOURCE, HC_AGENT_SESSION_BIN_REFRESH)"
            );
        }
        Commands::Chat {
            agent_id,
            domain_id,
            message,
        } => chat::run_chat(agent_id, domain_id, message)?,
    }
    Ok(())
}
