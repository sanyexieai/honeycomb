//! Agent 域专用对话：`hc-service` 负责生成；轮次落盘由 [`hc_agent::AgentRuntimeChatTurnSink`]（与 `hc-cli` 的 MemoryRoom 策略分离）。

use anyhow::{Context, Result};
use hc_agent::AgentRuntimeChatTurnSink;
use hc_context::ChatTurnPersistence;
use hc_protocol::{
    ApiChatMessage, ApiMemoryQuery, ApiMessageRole, ApiNamespace, ChatRequest,
};
use hc_service::{
    ServiceConfig,
    chat::handle_chat_request,
    transport::{
        WorkspaceNamespace, init_console_tracing, load_local_env_file, tenant_id_from_env,
        user_id_from_env,
    },
};
use rustyline::{DefaultEditor, error::ReadlineError};

fn agent_cli_room_id(tenant: &str, user: &str, session: &str) -> String {
    format!("agent.session.{tenant}.{user}.{session}")
}

pub fn run_chat(
    agent_id: Option<String>,
    domain_id: Option<String>,
    message: Option<String>,
) -> Result<()> {
    init_console_tracing();
    let _ = load_local_env_file();
    let config = ServiceConfig::from_env();
    let tenant = tenant_id_from_env();
    let user = user_id_from_env();
    let session = std::env::var("HC_SESSION_ID")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| hc_context::runtime::default_session_id(&tenant, &user));

    let workspace_ns = WorkspaceNamespace::new(tenant.clone(), user.clone());
    let turn_sink = AgentRuntimeChatTurnSink::try_new(&config.workspace_root, &workspace_ns, &session)
        .context("初始化 agent 对话落盘")?;
    turn_sink.init_session()?;

    let room_id = agent_cli_room_id(&tenant, &user, &session);

    if let Some(text) = message {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("--message 不能为空");
        }
        return run_one_turn(
            &config,
            &workspace_ns,
            &tenant,
            &user,
            &session,
            &room_id,
            &turn_sink,
            agent_id,
            domain_id,
            text,
            &[],
            1,
        );
    }

    let mut history: Vec<ApiChatMessage> = Vec::new();
    let mut editor = DefaultEditor::new()?;

    eprintln!("hc-agent chat（落盘：`agent-runtime/sessions/<slug>/conversations/turns/`；`HC_AGENT_CHAT_PERSIST=off` 关闭）");
    eprintln!("会话目录：`agent-runtime/sessions/<slug>/`（含 `agent/` 占位描述，`status: temporary` 可改）");
    eprintln!("session={session} tenant={tenant} user={user} room_id={room_id}");
    if !turn_sink.enabled() {
        eprintln!("提示: 当前未写入对话文件（已关闭持久化）。");
    }
    if let Some(ref id) = agent_id {
        eprintln!("固定 agent-id: {id}");
    }
    if let Some(ref id) = domain_id {
        eprintln!("domain-id: {id}");
    }
    eprintln!("输入 /quit 或 Ctrl+D 退出\n");

    loop {
        match editor.readline("agent> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" || line == "/exit" {
                    break;
                }
                let turn_index = history
                    .iter()
                    .filter(|message| message.role == ApiMessageRole::User)
                    .count()
                    + 1;

                if let Err(error) = turn_sink.persist_user_turn(turn_index, line) {
                    eprintln!("warning> 用户轮次落盘跳过: {error}");
                }

                let request = build_chat_request(
                    &tenant,
                    &user,
                    &session,
                    &room_id,
                    agent_id.clone(),
                    domain_id.clone(),
                    line,
                    &history,
                );
                let response = handle_chat_request(&config, request, None)
                    .context("chat 请求失败")?;
                let reply = response.message.content.trim();
                println!("{reply}\n");

                if let Err(error) = turn_sink.persist_assistant_turn(turn_index, reply) {
                    eprintln!("warning> 助手轮次落盘跳过: {error}");
                }

                history.push(ApiChatMessage {
                    role: ApiMessageRole::User,
                    content: line.to_owned(),
                    name: None,
                });
                history.push(response.message);
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

fn build_chat_request(
    tenant: &str,
    user: &str,
    session: &str,
    room_id: &str,
    agent_id: Option<String>,
    domain_id: Option<String>,
    input: &str,
    history: &[ApiChatMessage],
) -> ChatRequest {
    ChatRequest {
        tenant_id: Some(tenant.to_owned()),
        user_id: Some(user.to_owned()),
        session_id: Some(session.to_owned()),
        room_id: Some(room_id.to_owned()),
        behavior_pattern: None,
        thinking_depth: None,
        input: Some(input.to_owned()),
        messages: history.to_vec(),
        provider: None,
        model: None,
        system_prompt: None,
        agent_id: agent_id.clone(),
        domain_id: domain_id.clone(),
        active_agent_id: agent_id,
        active_task_id: None,
        active_work_item_id: None,
        memory: ApiMemoryQuery {
            namespace: ApiNamespace::from_tenant_user(tenant, user),
            ..Default::default()
        },
        temperature: None,
        max_output_tokens: None,
    }
}

fn run_one_turn(
    config: &ServiceConfig,
    _workspace_ns: &WorkspaceNamespace,
    tenant: &str,
    user: &str,
    session: &str,
    room_id: &str,
    turn_sink: &AgentRuntimeChatTurnSink,
    agent_id: Option<String>,
    domain_id: Option<String>,
    input: &str,
    history: &[ApiChatMessage],
    turn_index: usize,
) -> Result<()> {
    if let Err(error) = turn_sink.persist_user_turn(turn_index, input) {
        eprintln!("warning> 用户轮次落盘跳过: {error}");
    }

    let request = build_chat_request(
        tenant,
        user,
        session,
        room_id,
        agent_id,
        domain_id,
        input,
        history,
    );
    let response = handle_chat_request(config, request, None).context("chat 请求失败")?;
    let reply = response.message.content.trim();
    println!("{reply}");

    if let Err(error) = turn_sink.persist_assistant_turn(turn_index, reply) {
        eprintln!("warning> 助手轮次落盘跳过: {error}");
    }
    Ok(())
}
