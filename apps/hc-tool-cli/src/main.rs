use std::{
    borrow::Cow,
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use encoding_rs::GB18030;
use hc_agent::phrase_match_score;
use hc_capability::ModelDependence;
use hc_context::{
    ChatCaptureOptions, ChatMemoryOptions, MemoryNamespace, MemoryRetriever, MemoryRoom,
    RetrievedMemory, WorkspaceMemoryRetriever, load_tool_chat_prompt,
    load_tool_natural_language_builder_prompt, load_tool_router_prompt, memory_kind_label,
    memory_scope_label, parse_memory_kind, parse_memory_scope, persist_chat_turn_assistant_reply,
    persist_chat_turn_user_message, persist_global_preference_from_chat_input,
    prepare_chat_capture_room, render_recalled_memory_context,
    runtime::{RuntimeIdentity, RuntimeVariableRepository, RuntimeVariables},
    workspace_namespace_from_memory_namespace,
};
use hc_conversation::{ConversationRepository, FollowUpStatus, PendingFollowUp};
use hc_llm::{
    ChatMessage, GenerateRequest, MessageRole, ModelRef, ProviderRegistry, default_model_from_env,
    default_provider_from_env, default_registry_from_env, is_timeout_error,
    sanitize_assistant_text,
};
use hc_protocol::{ApiChatMessage, ApiMemoryQuery, ApiMessageRole, ApiNamespace, ChatRequest};
use hc_scheduler::{
    ScheduleRepository, ScheduleSpec, ScheduleStatus, ScheduledRun, ScheduledRunStatus,
    ScheduledTarget, ScheduledTargetKind, ScheduledTask, now_unix,
};
use hc_service::{
    ServiceConfig,
    tool_turn::{
        PendingToolConfirmation, ToolTurnSessionState, load_tool_turn_session_state,
        save_tool_turn_session_state, try_handle_configured_mcp_turn,
        try_handle_persisted_pending_confirmation,
    },
};
use hc_skill::{SkillProfile, SkillRepository};
use hc_store::{
    index::{
        DEFAULT_LOCAL_EMBEDDING_DIMS, LocalJsonVectorIndex, RebuildableIndex, VectorIndex,
        VectorQuery, indexed_documents_from_markdown_index, local_hash_embedding,
        vector_documents_from_indexed_documents,
    },
    store::{WorkspaceNamespace, WorkspaceStore},
};
use hc_toolchain::{
    CommandToolExecutor, McpServerRepository, McpServerSpec, McpTransportKind, ToolCatalog,
    ToolComposition, ToolExecutionKind, ToolExecutionOutcome, ToolExecutor, ToolRepository,
    ToolSpec, ToolStability, build_default_tool_execution_plan, call_mcp_tool,
    default_tool_catalog, is_mcp_tool_command, normalize_mcp_server_id,
};
use rustyline::{DefaultEditor, error::ReadlineError};
use serde::Deserialize;

#[derive(Debug, Clone, Default)]
struct CliRuntimeContext {
    tenant_id: Option<String>,
    user_id: Option<String>,
    session_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct CommonOptions {
    json: bool,
}

#[derive(Debug, Clone, Default)]
struct RunOptions {
    json: bool,
    path: Option<PathBuf>,
    package: Option<String>,
    goal: Option<String>,
    content: Option<String>,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
struct CreateOptions {
    id: String,
    name: String,
    description: String,
    execution_kind: ToolExecutionKind,
    command: Vec<String>,
    tags: Vec<String>,
    json: bool,
}

#[derive(Debug, Clone, Default)]
struct McpAddOptions {
    id: String,
    name: String,
    description: String,
    transport: Option<McpTransportKind>,
    url: Option<String>,
    command: Vec<String>,
    tags: Vec<String>,
    json: bool,
}

#[derive(Debug, Clone)]
struct ResolvedToolTarget {
    tool: ToolSpec,
    delegated_tool: Option<ToolSpec>,
    skill: Option<SkillProfile>,
}

#[derive(Debug, Clone)]
struct ToolSelection {
    selected: Option<ToolSpec>,
    candidates: Vec<ToolSelectionCandidate>,
}

#[derive(Debug, Clone)]
struct ToolSelectionCandidate {
    tool: ToolSpec,
    score: i32,
}

#[derive(Debug, Clone)]
struct TurnFrame {
    user_turn: String,
    runtime: RuntimeVariables,
    namespace: MemoryNamespace,
    workspace_namespace: WorkspaceNamespace,
    session_id: Option<String>,
    turn_index: usize,
    selection_input: String,
    selection: ToolSelection,
    recalled_memories: Vec<RetrievedMemory>,
    tool_execution_context: Option<String>,
    selected_agent_id: Option<String>,
    selected_domain_id: Option<String>,
}

#[derive(Debug, Clone)]
struct TurnNodeReply {
    reply: Option<String>,
    warning: Option<String>,
    clear_pending_confirmation: bool,
    next_pending_confirmation: Option<PendingToolConfirmation>,
    stop_pipeline: bool,
    reset_system_prompt: bool,
}

#[derive(Debug, Clone)]
enum NormalChatNodeResult {
    CreatedTool {
        path: PathBuf,
    },
    AssistantReply {
        content: String,
        artifact_paths: Vec<PathBuf>,
    },
    InvalidCreateCommand {
        content: String,
        warning: String,
    },
    Error {
        message: String,
    },
}

enum ExplicitCommandNodeResult {
    Continue,
    Exit,
}

impl TurnFrame {
    fn new(
        user_turn: impl Into<String>,
        namespace: MemoryNamespace,
        session_id: Option<String>,
        turn_index: usize,
        selection_input: String,
        selection: ToolSelection,
        recalled_memories: Vec<RetrievedMemory>,
    ) -> Self {
        let user_turn = user_turn.into();
        let runtime = runtime_variables_for_namespace(&namespace, session_id.as_deref());
        let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
        Self {
            user_turn,
            runtime,
            namespace,
            workspace_namespace,
            session_id,
            turn_index,
            selection_input,
            selection,
            recalled_memories,
            tool_execution_context: None,
            selected_agent_id: None,
            selected_domain_id: None,
        }
    }

    fn room_id(&self) -> Option<String> {
        self.session_id.clone()
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn set_tool_execution_context(&mut self, context: impl Into<String>) {
        self.tool_execution_context = Some(context.into());
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolRoutingTags {
    #[serde(default)]
    timed_sequence_rules: Vec<TimedSequenceRule>,
    #[serde(default)]
    reminder_rules: Vec<ReminderRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TimedSequenceRule {
    #[serde(default)]
    id: String,
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    default_end: Option<i64>,
    #[serde(default = "default_timed_sequence_interval_seconds")]
    interval_seconds: u64,
    #[serde(default = "default_timed_sequence_max_items")]
    max_items: usize,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    scheduled_reply: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReminderRule {
    #[serde(default)]
    id: String,
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default = "default_reminder_delay_seconds")]
    default_delay_seconds: u64,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    scheduled_reply: Option<String>,
    #[serde(default)]
    due_reply: Option<String>,
}

impl Default for ReminderRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            hints: Vec::new(),
            default_delay_seconds: default_reminder_delay_seconds(),
            agent_id: None,
            trigger: None,
            scheduled_reply: None,
            due_reply: None,
        }
    }
}

fn default_reminder_delay_seconds() -> u64 {
    60
}

fn default_timed_sequence_interval_seconds() -> u64 {
    1
}

fn default_timed_sequence_max_items() -> usize {
    120
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolResponseRenderingConfig {
    #[serde(default)]
    renderers: Vec<ToolResponseRenderer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolResponseRenderer {
    #[serde(default)]
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    selectors: Vec<String>,
    #[serde(default)]
    empty_reply: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    failure_reply: Option<String>,
}

#[derive(Debug, Clone)]
struct CodeBlock {
    language: Option<String>,
    content: String,
}

trait ToolSelector {
    fn select(&self, input: &str, catalog: &ToolCatalog) -> Result<ToolSelection>;
}

#[derive(Debug, Clone, Copy)]
struct KeywordToolSelector {
    limit: usize,
}

impl Default for KeywordToolSelector {
    fn default() -> Self {
        Self { limit: 5 }
    }
}

impl ToolSelector for KeywordToolSelector {
    fn select(&self, input: &str, catalog: &ToolCatalog) -> Result<ToolSelection> {
        let mut candidates: Vec<ToolSelectionCandidate> = catalog
            .list()
            .into_iter()
            .map(|tool| ToolSelectionCandidate {
                tool: tool.clone(),
                score: score_tool_for_goal(tool, input),
            })
            .collect();
        candidates.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.tool.id.cmp(&right.tool.id))
        });
        candidates.truncate(self.limit);
        let selected = candidates
            .first()
            .filter(|candidate| candidate.score > 0)
            .map(|candidate| candidate.tool.clone());
        Ok(ToolSelection {
            selected,
            candidates,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolBuild {
    action: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    tool: Option<NaturalLanguageToolDraft>,
    #[serde(default)]
    skill: Option<NaturalLanguageSkillDraft>,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolRoute {
    action: String,
    #[serde(default)]
    tool_id: Option<String>,
    #[serde(default)]
    fallback_tool_ids: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolDraft {
    id: String,
    name: String,
    description: String,
    execution_kind: Option<String>,
    default_command: Vec<String>,
    #[serde(default)]
    files: Vec<NaturalLanguageToolFileDraft>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageToolFileDraft {
    path: String,
    content: String,
    #[serde(default)]
    executable: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct NaturalLanguageSkillDraft {
    id: String,
    name: String,
    description: String,
    instructions: String,
    tool_id: Option<String>,
    execution_kind: Option<String>,
    default_command: Vec<String>,
    tool_refs: Vec<String>,
    tags: Vec<String>,
}

fn main() -> Result<()> {
    configure_console_encoding();
    load_local_env_file()?;
    let args: Vec<String> = env::args().skip(1).collect();
    let (context, args) = parse_cli_runtime_context(args)?;
    let _ = CLI_RUNTIME_CONTEXT.set(context);
    match args.as_slice() {
        [] => handle_chat(&[]),
        [cmd] if is_help(cmd) => {
            print_help();
            Ok(())
        }
        [cmd, rest @ ..] if cmd == "chat" => handle_chat(rest),
        [cmd, rest @ ..] if cmd == "create" => handle_create(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_list(rest),
        [cmd, rest @ ..] if cmd == "show" => handle_show(rest),
        [cmd, rest @ ..] if cmd == "plan" => handle_plan(rest),
        [cmd, rest @ ..] if cmd == "run" => handle_run(rest),
        [cmd, rest @ ..] if cmd == "mcp" => handle_mcp(rest),
        [cmd, rest @ ..] if cmd == "schedule" => handle_schedule(rest),
        [cmd, rest @ ..] if cmd == "index" => handle_index(rest),
        [other, ..] => bail!("unknown command: {other}"),
    }
}

fn configure_console_encoding() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
        let _ = SetConsoleCP(65001);
        let _ = SetConsoleOutputCP(65001);
    }
}

static CLI_RUNTIME_CONTEXT: OnceLock<CliRuntimeContext> = OnceLock::new();

fn parse_cli_runtime_context(args: Vec<String>) -> Result<(CliRuntimeContext, Vec<String>)> {
    let mut context = CliRuntimeContext::default();
    let mut rest = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--tenant-id" => {
                context.tenant_id = normalized_optional_cli_value(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tenant-id")?,
                );
                index += 2;
            }
            "--user-id" => {
                context.user_id = normalized_optional_cli_value(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --user-id")?,
                );
                index += 2;
            }
            "--session-id" => {
                context.session_id = normalized_optional_cli_value(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --session-id")?,
                );
                index += 2;
            }
            _ => {
                rest.extend(args[index..].iter().cloned());
                break;
            }
        }
    }
    Ok((context, rest))
}

fn normalized_optional_cli_value(value: String) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn apply_cli_context_to_chat_options(
    memory_options: &mut ChatMemoryOptions,
    capture_options: &mut ChatCaptureOptions,
) {
    let context = cli_runtime_context();
    if let Some(tenant_id) = context.tenant_id {
        memory_options.namespace.tenant_id = tenant_id.clone();
        capture_options.namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = context.user_id {
        memory_options.namespace.user_id = user_id.clone();
        capture_options.namespace.user_id = user_id;
    }
    if let Some(session_id) = context.session_id {
        capture_options.room_id = Some(session_id);
    }
}

fn cli_runtime_context() -> CliRuntimeContext {
    CLI_RUNTIME_CONTEXT.get().cloned().unwrap_or_default()
}

fn handle_chat(args: &[String]) -> Result<()> {
    let mut provider = default_provider();
    let mut model = default_model();
    let mut system_message = env::var("HC_LLM_SYSTEM").ok();
    let mut memory_options = ChatMemoryOptions::from_env();
    let mut capture_options = ChatCaptureOptions::from_env();
    apply_cli_context_to_chat_options(&mut memory_options, &mut capture_options);
    let mut show_memory = false;

    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--provider" => {
                provider = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --provider")?;
                index += 2;
            }
            "--model" => {
                model = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --model")?;
                index += 2;
            }
            "--system" => {
                system_message = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --system")?,
                );
                index += 2;
            }
            "--tenant-id" => {
                if let Some(value) = normalized_optional_cli_value(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tenant-id")?,
                ) {
                    memory_options.namespace.tenant_id = value.clone();
                    capture_options.namespace.tenant_id = value;
                }
                index += 2;
            }
            "--user-id" => {
                if let Some(value) = normalized_optional_cli_value(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --user-id")?,
                ) {
                    memory_options.namespace.user_id = value.clone();
                    capture_options.namespace.user_id = value;
                }
                index += 2;
            }
            "--session-id" => {
                capture_options.room_id = normalized_optional_cli_value(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --session-id")?,
                );
                index += 2;
            }
            "--no-memory" => {
                memory_options.enabled = false;
                capture_options.enabled = false;
                index += 1;
            }
            "--memory-limit" => {
                memory_options.limit = parse_usize_arg(
                    args.get(index + 1)
                        .context("missing value for --memory-limit")?,
                    "--memory-limit",
                )?;
                index += 2;
            }
            "--scope" => {
                memory_options.scope = Some(parse_memory_scope(
                    args.get(index + 1).context("missing value for --scope")?,
                )?);
                index += 2;
            }
            "--memory-kind" => {
                memory_options.kind = Some(parse_memory_kind(
                    args.get(index + 1)
                        .context("missing value for --memory-kind")?,
                )?);
                index += 2;
            }
            "--tag" => {
                memory_options.tag = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--show-memory" => {
                show_memory = true;
                index += 1;
            }
            other => bail!("unknown chat option: {other}"),
        }
    }
    if capture_options.room_id.as_deref().is_none_or(str::is_empty) {
        capture_options.room_id = Some(default_chat_session_id(
            &memory_options.namespace.tenant_id,
            &memory_options.namespace.user_id,
        ));
    }

    let registry = default_registry();
    let catalog = load_cli_tool_catalog()?;
    let tool_prompt = render_tool_chat_system_prompt(
        &catalog,
        system_message.as_deref(),
        &memory_options.namespace,
        capture_options.room_id.as_deref(),
    )?;
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_options.namespace);
    let memory_retriever =
        WorkspaceMemoryRetriever::new(workspace_root(), workspace_namespace.clone());
    let chat_room = prepare_chat_capture_room(
        workspace_root(),
        workspace_namespace.clone(),
        &capture_options,
    )?;

    println!("hc-tool chat");
    println!("provider={provider} model={model}");
    println!(
        "memory={} namespace={}/{} session={} limit={}",
        if memory_options.enabled { "on" } else { "off" },
        memory_options.namespace.tenant_id,
        memory_options.namespace.user_id,
        capture_options.room_id.as_deref().unwrap_or("default"),
        memory_options.limit
    );
    println!("Type /help for commands, /quit to exit.");

    let mut editor = DefaultEditor::new().context("failed to initialize line editor")?;
    let mut history = vec![ChatMessage::new(MessageRole::System, tool_prompt)];
    loop {
        let Some(input) = prompt_raw(&mut editor)? else {
            break;
        };
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(command_result) = run_explicit_command_node(
            trimmed,
            &mut history,
            system_message.as_deref(),
            &memory_options,
            &capture_options,
        )? {
            match command_result {
                ExplicitCommandNodeResult::Continue => continue,
                ExplicitCommandNodeResult::Exit => break,
            }
        }

        let catalog = load_cli_tool_catalog()?;
        let mut pending_confirmation = load_cli_pending_confirmation(
            &memory_options.namespace,
            capture_options.room_id.as_deref(),
        )?;
        let selector = KeywordToolSelector::default();
        let selection_input = selection_input_from_history(&history, trimmed);
        let recalled_memories = if memory_options.enabled {
            let query = memory_options.build_query(trimmed);
            memory_retriever.retrieve(&query)?
        } else {
            Vec::new()
        };
        if show_memory {
            print_recalled_memories(&recalled_memories);
        }
        let turn_index = history
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count()
            + 1;
        let mut frame = TurnFrame::new(
            trimmed.to_owned(),
            memory_options.namespace.clone(),
            capture_options.room_id.clone(),
            turn_index,
            selection_input.clone(),
            selector.select(&selection_input, &catalog)?,
            recalled_memories,
        );
        if let Some(room) = &chat_room
            && let Err(error) = persist_chat_turn_user_message(
                workspace_root(),
                frame.workspace_namespace.clone(),
                room,
                frame.turn_index,
                frame.user_turn.clone(),
            )
        {
            println!("warning> chat memory write skipped: {error}");
        }
        if let Some(node_reply) = run_pending_confirmation_node(&mut frame, &catalog)? {
            apply_turn_node_reply_state(&node_reply, &mut pending_confirmation);
            save_cli_pending_confirmation(
                &memory_options.namespace,
                capture_options.room_id.as_deref(),
                &pending_confirmation,
            )?;
            print_turn_node_warning(&node_reply);
            if emit_turn_node_reply(&frame, &node_reply, &mut history, chat_room.as_ref())? {
                continue;
            }
            if node_reply.stop_pipeline {
                continue;
            }
        }
        if let Some(node_reply) = run_timed_sequence_node(&frame, &history)? {
            apply_turn_node_reply_state(&node_reply, &mut pending_confirmation);
            save_cli_pending_confirmation(
                &memory_options.namespace,
                capture_options.room_id.as_deref(),
                &pending_confirmation,
            )?;
            print_turn_node_warning(&node_reply);
            emit_turn_node_reply(&frame, &node_reply, &mut history, chat_room.as_ref())?;
            if node_reply.stop_pipeline {
                continue;
            }
        }
        if let Some(node_reply) = run_configured_agent_mcp_node(&mut frame, &catalog)? {
            apply_turn_node_reply_state(&node_reply, &mut pending_confirmation);
            save_cli_pending_confirmation(
                &memory_options.namespace,
                capture_options.room_id.as_deref(),
                &pending_confirmation,
            )?;
            print_turn_node_warning(&node_reply);
            emit_turn_node_reply(&frame, &node_reply, &mut history, chat_room.as_ref())?;
            if node_reply.stop_pipeline {
                continue;
            }
        } else {
            let node_reply = run_llm_tool_router_node(
                &registry,
                &provider,
                &model,
                &mut frame,
                &catalog,
                system_message.as_deref(),
            )?;
            print_turn_node_warning(&node_reply);
            if node_reply.reset_system_prompt {
                let catalog = load_cli_tool_catalog()?;
                history.clear();
                history.push(ChatMessage::new(
                    MessageRole::System,
                    render_tool_chat_system_prompt(
                        &catalog,
                        system_message.as_deref(),
                        &memory_options.namespace,
                        capture_options.room_id.as_deref(),
                    )?,
                ));
            }
            if node_reply.stop_pipeline {
                continue;
            }
        }
        print!("assistant> ");
        io::stdout().flush().context("failed to flush stdout")?;
        match run_normal_chat_node(&registry, &provider, &model, &history, &frame)? {
            NormalChatNodeResult::CreatedTool { path } => {
                println!("created> {}", path.display());
                let catalog = load_cli_tool_catalog()?;
                history.clear();
                history.push(ChatMessage::new(
                    MessageRole::System,
                    render_tool_chat_system_prompt(
                        &catalog,
                        system_message.as_deref(),
                        &memory_options.namespace,
                        capture_options.room_id.as_deref(),
                    )?,
                ));
            }
            NormalChatNodeResult::AssistantReply {
                content,
                artifact_paths,
            } => {
                emit_normal_chat_assistant_reply(
                    &frame,
                    content,
                    artifact_paths,
                    &mut history,
                    chat_room.as_ref(),
                )?;
                if memory_options.enabled {
                    match persist_global_preference_from_chat_input(
                        workspace_root(),
                        workspace_namespace.clone(),
                        memory_options.namespace.clone(),
                        chat_room.as_ref().map(|room| room.id.clone()),
                        frame.user_turn.clone(),
                        &registry,
                        &ModelRef::new(provider.clone(), model.clone()),
                    ) {
                        Ok(paths) => {
                            if show_memory {
                                for path in paths {
                                    println!("memory saved> {}", path.display());
                                }
                            }
                        }
                        Err(error) => {
                            println!("warning> global memory write skipped: {error}");
                        }
                    }
                }
            }
            NormalChatNodeResult::InvalidCreateCommand { content, warning } => {
                emit_invalid_create_command_reply(&frame, content, warning, &mut history);
            }
            NormalChatNodeResult::Error { message } => {
                println!("{message}");
            }
        }
    }

    Ok(())
}

fn handle_list(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let catalog = load_cli_tool_catalog()?;
    let tools: Vec<&ToolSpec> = catalog.list();

    if options.json {
        println!("{}", serde_json::to_string_pretty(&tools)?);
        return Ok(());
    }

    for tool in tools {
        println!(
            "{} | {} | {:?} | {:?}",
            tool.id, tool.name, tool.execution_kind, tool.stability
        );
    }
    Ok(())
}

fn handle_show(args: &[String]) -> Result<()> {
    let (selector, options) = parse_selector_and_common_options(args, "show")?;
    let catalog = load_cli_tool_catalog()?;
    let tool = resolve_tool(&catalog, &selector)?;

    if options.json {
        println!("{}", serde_json::to_string_pretty(&tool)?);
        return Ok(());
    }

    println!("id> {}", tool.id);
    println!("name> {}", tool.name);
    println!("description> {}", tool.description);
    println!("kind> {:?}", tool.execution_kind);
    println!("composition> {:?}", tool.composition);
    println!("stability> {:?}", tool.stability);
    println!("command> {}", tool.default_command.join(" "));
    if !tool.tags.is_empty() {
        println!("tags> {}", tool.tags.join(", "));
    }
    Ok(())
}

fn handle_create(args: &[String]) -> Result<()> {
    let options = parse_create_options(args)?;
    let tool = tool_from_create_options(&options)?;
    let path = tool_repository().write_tool(&tool)?;

    if options.json {
        let payload = serde_json::json!({
            "tool": tool,
            "path": path,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("created> {}", path.display());
    println!("tool> {}", tool.id);
    println!("command> {}", tool.default_command.join(" "));
    Ok(())
}

fn handle_create_from_chat(input: &str) -> Result<PathBuf> {
    let args = split_chat_command(input);
    let options = parse_create_options(&args)?;
    let tool = tool_from_create_options(&options)?;
    tool_repository().write_tool(&tool)
}

fn handle_mcp(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "add" => handle_mcp_add(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_mcp_list(rest),
        [cmd, rest @ ..] if cmd == "tools" => handle_mcp_tools(rest),
        [cmd, rest @ ..] if cmd == "call" => handle_mcp_call(rest),
        [] => bail!("usage: hc-tool-cli mcp <add|list|tools|call> ..."),
        [other, ..] => bail!("unknown mcp command: {other}"),
    }
}

fn handle_schedule(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "add" => handle_schedule_add(rest),
        [cmd, rest @ ..] if cmd == "list" => handle_schedule_list(rest),
        [cmd, rest @ ..] if cmd == "run-due" => handle_schedule_run_due(rest),
        [cmd, rest @ ..] if cmd == "runs" => handle_schedule_runs(rest),
        [cmd, rest @ ..] if cmd == "pause" => {
            handle_schedule_set_status(rest, ScheduleStatus::Paused)
        }
        [cmd, rest @ ..] if cmd == "resume" => {
            handle_schedule_set_status(rest, ScheduleStatus::Active)
        }
        [cmd, rest @ ..] if cmd == "dispatch-due" => handle_schedule_dispatch_due(rest),
        [cmd, rest @ ..] if cmd == "dispatch-queued" => handle_schedule_dispatch_queued(rest),
        [cmd, rest @ ..] if cmd == "watch" => handle_schedule_watch(rest),
        [] => bail!(
            "usage: hc-tool-cli schedule <add|list|run-due|runs|pause|resume|dispatch-due|dispatch-queued|watch> ..."
        ),
        [other, ..] => bail!("unknown schedule command: {other}"),
    }
}

fn handle_index(args: &[String]) -> Result<()> {
    match args {
        [cmd, rest @ ..] if cmd == "rebuild" => handle_index_rebuild(rest),
        [cmd, rest @ ..] if cmd == "search" => handle_index_search(rest),
        [] => bail!("usage: hc-tool-cli index <rebuild|search> ..."),
        [other, ..] => bail!("unknown index command: {other}"),
    }
}

fn handle_index_rebuild(args: &[String]) -> Result<()> {
    let mut json = false;
    let mut vector = false;
    let mut dims = DEFAULT_LOCAL_EMBEDDING_DIMS;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--vector" => {
                vector = true;
                index += 1;
            }
            "--dims" => {
                dims = parse_usize_arg(
                    args.get(index + 1).context("missing value for --dims")?,
                    "--dims",
                )?;
                index += 2;
            }
            other => bail!("unexpected argument for index rebuild: {other}"),
        }
    }

    let namespace = runtime_namespace();
    let store = WorkspaceStore::new(workspace_root());
    let markdown_index = store.rebuild_markdown_index_in_namespace(&namespace)?;
    let mut vector_path = None;
    let mut vector_count = 0usize;
    if vector {
        let indexed_documents = indexed_documents_from_markdown_index(&markdown_index);
        let vector_documents =
            vector_documents_from_indexed_documents(&indexed_documents, |doc| {
                Ok(local_hash_embedding(
                    &format!(
                        "{} {} {} {}",
                        doc.title,
                        doc.doc_type,
                        doc.tags.join(" "),
                        doc.text
                    ),
                    dims,
                ))
            })?;
        vector_count = vector_documents.len();
        let vector_index = LocalJsonVectorIndex::new(workspace_root());
        vector_index.rebuild(&namespace, &vector_documents)?;
        vector_path = Some(vector_index.path_in_namespace(&namespace));
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "namespace": namespace,
                "markdown_documents": markdown_index.documents.len(),
                "markdown_index_path": store.markdown_index_path_in_namespace(&markdown_index.namespace),
                "markdown_search_index_path": store.markdown_search_index_path_in_namespace(&markdown_index.namespace),
                "vector_documents": vector_count,
                "vector_index_path": vector_path,
            }))?
        );
        return Ok(());
    }

    println!(
        "index> markdown documents {}",
        markdown_index.documents.len()
    );
    println!(
        "index> markdown {}",
        store
            .markdown_index_path_in_namespace(&markdown_index.namespace)
            .display()
    );
    println!(
        "index> search {}",
        store
            .markdown_search_index_path_in_namespace(&markdown_index.namespace)
            .display()
    );
    if let Some(path) = vector_path {
        println!("index> vector documents {vector_count}");
        println!("index> vector {}", path.display());
    }
    Ok(())
}

fn handle_index_search(args: &[String]) -> Result<()> {
    let mut text = None;
    let mut json = false;
    let mut vector = false;
    let mut rebuild = false;
    let mut limit = 10usize;
    let mut dims = DEFAULT_LOCAL_EMBEDDING_DIMS;
    let mut filters = BTreeMap::<String, String>::new();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--text" => {
                text = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --text")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--vector" => {
                vector = true;
                index += 1;
            }
            "--rebuild" => {
                rebuild = true;
                index += 1;
            }
            "--limit" => {
                limit = parse_usize_arg(
                    args.get(index + 1).context("missing value for --limit")?,
                    "--limit",
                )?;
                index += 2;
            }
            "--dims" => {
                dims = parse_usize_arg(
                    args.get(index + 1).context("missing value for --dims")?,
                    "--dims",
                )?;
                index += 2;
            }
            "--filter" => {
                let filter = args.get(index + 1).context("missing value for --filter")?;
                let (key, value) = parse_key_value(filter)?;
                filters.insert(key, value);
                index += 2;
            }
            value if text.is_none() => {
                text = Some(value.to_owned());
                index += 1;
            }
            other => bail!("unexpected argument for index search: {other}"),
        }
    }

    let text = text.context("missing search text")?;
    let namespace = runtime_namespace();
    let store = WorkspaceStore::new(workspace_root());
    if rebuild {
        let markdown_index = store.rebuild_markdown_index_in_namespace(&namespace)?;
        if vector {
            rebuild_local_vector_index(&namespace, &markdown_index, dims)?;
        }
    }

    if vector {
        let vector_index = LocalJsonVectorIndex::new(workspace_root());
        if !vector_index.path_in_namespace(&namespace).exists() {
            let markdown_index = store.read_or_rebuild_markdown_index_in_namespace(&namespace)?;
            rebuild_local_vector_index(&namespace, &markdown_index, dims)?;
        }
        let hits = vector_index.search(
            &namespace,
            &VectorQuery {
                embedding: local_hash_embedding(&text, dims),
                filters,
                limit: Some(limit.saturating_mul(4).max(limit)),
            },
        )?;
        let markdown_index = store.read_or_rebuild_markdown_index_in_namespace(&namespace)?;
        let hits = rerank_vector_hits_with_markdown_text(hits, &markdown_index, &text, limit);
        if json {
            println!("{}", serde_json::to_string_pretty(&hits)?);
            return Ok(());
        }
        for hit in hits {
            println!("{:.3} | {} | {}", hit.score, hit.id, hit.source_path);
        }
        return Ok(());
    }

    let mut query = hc_store::store::MarkdownQuery::default()
        .with_text(text)
        .with_limit(limit);
    if let Some(doc_type) = filters.get("doc_type") {
        query = query.with_doc_type(doc_type.clone());
    }
    if let Some(status) = filters.get("status") {
        query = query.with_status(status.clone());
    }
    if let Some(tag) = filters.get("tag") {
        query = query.with_tag(tag.clone());
    }
    if let Some(path_prefix) = filters.get("path_prefix") {
        query = query.with_path_prefix(path_prefix.clone());
    }
    let matches = store.query_markdown_index_in_namespace(&namespace, &query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&matches)?);
        return Ok(());
    }
    for entry in matches {
        println!(
            "{} | {} | {}",
            entry.id, entry.doc_type, entry.relative_path
        );
    }
    Ok(())
}

fn rebuild_local_vector_index(
    namespace: &WorkspaceNamespace,
    markdown_index: &hc_store::store::MarkdownIndex,
    dims: usize,
) -> Result<()> {
    let indexed_documents = indexed_documents_from_markdown_index(markdown_index);
    let vector_documents = vector_documents_from_indexed_documents(&indexed_documents, |doc| {
        Ok(local_hash_embedding(
            &format!(
                "{} {} {} {}",
                doc.title,
                doc.doc_type,
                doc.tags.join(" "),
                doc.text
            ),
            dims,
        ))
    })?;
    LocalJsonVectorIndex::new(workspace_root()).rebuild(namespace, &vector_documents)
}

fn rerank_vector_hits_with_markdown_text(
    mut hits: Vec<hc_store::index::IndexHit>,
    markdown_index: &hc_store::store::MarkdownIndex,
    text: &str,
    limit: usize,
) -> Vec<hc_store::index::IndexHit> {
    let by_path = markdown_index
        .documents
        .iter()
        .map(|entry| (entry.relative_path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    for hit in &mut hits {
        let Some(entry) = by_path.get(hit.source_path.as_str()) else {
            continue;
        };
        let lexical_score = phrase_match_score(text, &entry.semantic_text) as f32 / 100.0;
        hit.score += lexical_score;
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    hits.truncate(limit);
    hits
}

fn handle_schedule_add(args: &[String]) -> Result<()> {
    let mut id = None;
    let mut title = None;
    let mut kind = None;
    let mut run_at_unix = None;
    let mut interval_seconds = None;
    let mut target_kind = None;
    let mut target_ref = None;
    let mut target_action = None;
    let mut target_args = serde_json::Map::new();
    let mut tags = Vec::new();
    let mut json = false;

    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --id")?,
                );
                index += 2;
            }
            "--title" => {
                title = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --title")?,
                );
                index += 2;
            }
            "--kind" => {
                kind = Some(parse_schedule_kind(
                    args.get(index + 1).context("missing value for --kind")?,
                )?);
                index += 2;
            }
            "--run-at-unix" => {
                run_at_unix = Some(parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --run-at-unix")?,
                    "--run-at-unix",
                )?);
                index += 2;
            }
            "--interval-seconds" => {
                interval_seconds = Some(parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --interval-seconds")?,
                    "--interval-seconds",
                )?);
                index += 2;
            }
            "--target-kind" => {
                target_kind = Some(parse_scheduled_target_kind(
                    args.get(index + 1)
                        .context("missing value for --target-kind")?,
                )?);
                index += 2;
            }
            "--target-ref" => {
                target_ref = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --target-ref")?,
                );
                index += 2;
            }
            "--target-action" => {
                target_action = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --target-action")?,
                );
                index += 2;
            }
            "--arg" => {
                let arg = args.get(index + 1).context("missing value for --arg")?;
                let (key, value) = arg
                    .split_once('=')
                    .with_context(|| format!("schedule --arg must use key=value form: {arg}"))?;
                target_args.insert(key.to_owned(), parse_jsonish_argument_value(value));
                index += 2;
            }
            "--tag" => {
                tags.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule add argument: {other}"),
        }
    }

    let task = ScheduledTask::new(
        id.context("missing --id")?,
        title.context("missing --title")?,
        ScheduleSpec {
            kind: kind.context("missing --kind")?,
            run_at_unix,
            interval_seconds,
        },
        ScheduledTarget {
            kind: target_kind.context("missing --target-kind")?,
            r#ref: target_ref.context("missing --target-ref")?,
            action: target_action,
            args: target_args,
        },
    );
    let mut task = task;
    if !tags.is_empty() {
        task.tags = normalized_tags(tags, "scheduled");
    }
    let path = schedule_repository().write_schedule(&task)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schedule": task,
                "path": path,
            }))?
        );
    } else {
        println!("schedule> {}", task.id);
        println!("path> {}", path.display());
        println!("next_fire_at_unix> {:?}", task.state.next_fire_at_unix);
    }
    Ok(())
}

fn handle_schedule_list(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let schedules = schedule_repository().list_schedules()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&schedules)?);
        return Ok(());
    }
    for schedule in schedules {
        println!(
            "{} | {:?} | next={:?} | {:?}:{}",
            schedule.id,
            schedule.status,
            schedule.state.next_fire_at_unix,
            schedule.target.kind,
            schedule.target.r#ref
        );
    }
    Ok(())
}

fn handle_schedule_run_due(args: &[String]) -> Result<()> {
    let mut now = now_unix();
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--now-unix" => {
                now = parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule run-due argument: {other}"),
        }
    }
    let runs = schedule_repository().queue_due_runs(now)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(());
    }
    if runs.is_empty() {
        println!("schedule> no due runs");
    } else {
        for run in runs {
            println!(
                "run> {} schedule={} target={:?}:{}",
                run.id, run.schedule_id, run.target.kind, run.target.r#ref
            );
        }
    }
    Ok(())
}

fn handle_schedule_runs(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let runs = schedule_repository().list_runs()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(());
    }
    for run in runs {
        println!(
            "{} | schedule={} | {:?} | target={:?}:{}",
            run.id, run.schedule_id, run.status, run.target.kind, run.target.r#ref
        );
    }
    Ok(())
}

fn handle_schedule_set_status(args: &[String], status: ScheduleStatus) -> Result<()> {
    let mut id = None;
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --id")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule status argument: {other}"),
        }
    }
    let task = schedule_repository().set_schedule_status(&id.context("missing --id")?, status)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&task)?);
    } else {
        println!("schedule> {} {:?}", task.id, task.status);
    }
    Ok(())
}

fn handle_schedule_dispatch_due(args: &[String]) -> Result<()> {
    let mut now = now_unix();
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--now-unix" => {
                now = parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule dispatch-due argument: {other}"),
        }
    }

    let receipts = dispatch_due_scheduled_runs(now)?;
    print_schedule_dispatch_receipts(receipts, json)
}

fn handle_schedule_watch(args: &[String]) -> Result<()> {
    let mut tick_seconds = 30u64;
    let mut max_ticks = None;
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--tick-seconds" => {
                tick_seconds = parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --tick-seconds")?,
                    "--tick-seconds",
                )?;
                index += 2;
            }
            "--max-ticks" => {
                max_ticks = Some(parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --max-ticks")?,
                    "--max-ticks",
                )?);
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule watch argument: {other}"),
        }
    }
    if tick_seconds == 0 {
        bail!("--tick-seconds must be > 0");
    }

    let mut ticks = 0u64;
    loop {
        let now = now_unix();
        let receipts = dispatch_due_scheduled_runs(now)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "now_unix": now,
                    "receipts": receipts,
                }))?
            );
        } else if receipts.is_empty() {
            println!("schedule> tick now={} no due runs", now);
        } else {
            println!("schedule> tick now={} dispatched={}", now, receipts.len());
            for receipt in receipts {
                println!(
                    "dispatch> {} status={}",
                    receipt
                        .get("run_id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown"),
                    receipt
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                );
            }
        }
        ticks += 1;
        if max_ticks.is_some_and(|limit| ticks >= limit) {
            break;
        }
        std::thread::sleep(Duration::from_secs(tick_seconds));
    }
    Ok(())
}

fn dispatch_due_scheduled_runs(now: u64) -> Result<Vec<serde_json::Value>> {
    let mut receipts = Vec::new();
    schedule_repository().queue_due_runs(now)?;
    for run in schedule_repository().queued_runs()? {
        receipts.push(dispatch_scheduled_run(run, now)?);
    }
    Ok(receipts)
}

fn handle_schedule_dispatch_queued(args: &[String]) -> Result<()> {
    let mut now = now_unix();
    let mut json = false;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--now-unix" => {
                now = parse_u64_arg(
                    args.get(index + 1)
                        .context("missing value for --now-unix")?,
                    "--now-unix",
                )?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected schedule dispatch-queued argument: {other}"),
        }
    }

    let mut receipts = Vec::new();
    for run in schedule_repository().queued_runs()? {
        receipts.push(dispatch_scheduled_run(run, now)?);
    }
    print_schedule_dispatch_receipts(receipts, json)
}

fn print_schedule_dispatch_receipts(receipts: Vec<serde_json::Value>, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&receipts)?);
        return Ok(());
    }
    if receipts.is_empty() {
        println!("schedule> no due runs");
    } else {
        for receipt in receipts {
            println!(
                "dispatch> {} target={:?}:{} status={} result={}",
                receipt
                    .get("run_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown"),
                receipt
                    .get("target_kind")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown"),
                receipt
                    .get("target_ref")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown"),
                receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown"),
                receipt
                    .get("result_ref")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            );
        }
    }
    Ok(())
}

fn dispatch_scheduled_run(mut run: ScheduledRun, now: u64) -> Result<serde_json::Value> {
    run.status = ScheduledRunStatus::Running;
    run.started_at_unix = Some(now);
    schedule_repository().write_run(&run)?;

    let result = match run.target.kind {
        ScheduledTargetKind::Mcp => dispatch_scheduled_mcp_run(&run),
        _ => Err(anyhow!(
            "scheduled target kind {:?} is not dispatchable by hc-tool-cli yet",
            run.target.kind
        )),
    };

    let finished_at = now_unix();
    match result {
        Ok(result_ref) => {
            run.status = ScheduledRunStatus::Succeeded;
            run.finished_at_unix = Some(finished_at);
            run.result_ref = Some(result_ref.clone());
            run.error = None;
            schedule_repository().write_run(&run)?;
            Ok(serde_json::json!({
                "run_id": run.id,
                "target_kind": "mcp",
                "target_ref": run.target.r#ref,
                "status": "succeeded",
                "result_ref": result_ref,
            }))
        }
        Err(error) => {
            run.status = ScheduledRunStatus::Failed;
            run.finished_at_unix = Some(finished_at);
            run.error = Some(error.to_string());
            schedule_repository().write_run(&run)?;
            Ok(serde_json::json!({
                "run_id": run.id,
                "target_kind": format!("{:?}", run.target.kind),
                "target_ref": run.target.r#ref,
                "status": "failed",
                "error": error.to_string(),
            }))
        }
    }
}

fn dispatch_scheduled_mcp_run(run: &ScheduledRun) -> Result<String> {
    let tool_name = run
        .target
        .action
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("mcp scheduled target requires target.action")?;
    let server = mcp_server_repository().get_server(&run.target.r#ref)?;
    let mut arguments = serde_json::Map::new();
    for (key, value) in &server.default_args {
        arguments.insert(key.clone(), value.clone());
    }
    for (key, value) in &run.target.args {
        arguments.insert(key.clone(), value.clone());
    }
    let runtime =
        runtime_variables_for_workspace_namespace(&runtime_namespace(), Some(&run.schedule_id));
    runtime.inject_mcp_arguments(&mut arguments);
    let result = call_mcp_tool(&server, tool_name, serde_json::Value::Object(arguments))?;
    if result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        bail!(
            "scheduled mcp call returned an error: {}",
            compact_single_line(&result.to_string(), 300)
        );
    }
    Ok(format!("mcp:{}:{}", run.target.r#ref, tool_name))
}

fn handle_mcp_add(args: &[String]) -> Result<()> {
    let options = parse_mcp_add_options(args)?;
    let transport = options.transport.unwrap_or_else(|| {
        if options.url.is_some() {
            McpTransportKind::StreamableHttp
        } else {
            McpTransportKind::Stdio
        }
    });
    let server = McpServerSpec {
        id: normalize_mcp_server_id(&options.id),
        name: options.name,
        description: options.description,
        enabled: true,
        transport,
        url: options.url,
        command: options.command,
        default_args: BTreeMap::new(),
        tags: normalized_tags(options.tags, "mcp"),
    };
    let path = mcp_server_repository().write_server(&server)?;

    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "server": server,
                "path": path,
            }))?
        );
        return Ok(());
    }

    println!("mcp> {}", server.id);
    println!("path> {}", path.display());
    println!("transport> {:?}", server.transport);
    if let Some(url) = &server.url {
        println!("url> {url}");
    }
    if !server.command.is_empty() {
        println!("command> {}", server.command.join(" "));
    }
    Ok(())
}

fn handle_mcp_list(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let servers = mcp_server_repository().list_servers()?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&servers)?);
        return Ok(());
    }
    for server in servers {
        let endpoint = server
            .url
            .clone()
            .unwrap_or_else(|| server.command.join(" "));
        println!(
            "{} | {} | {} | {:?} | {}",
            server.id,
            server.name,
            if server.enabled {
                "enabled"
            } else {
                "disabled"
            },
            server.transport,
            endpoint
        );
    }
    Ok(())
}

fn handle_mcp_tools(args: &[String]) -> Result<()> {
    let options = parse_common_options(args)?;
    let servers = mcp_server_repository().list_servers()?;
    let mut tools = Vec::new();
    for server in servers {
        if !server.enabled {
            continue;
        }
        let cache = mcp_server_repository().refresh_tool_cache(&server)?;
        tools.extend(cache.tools);
    }
    if options.json {
        println!("{}", serde_json::to_string_pretty(&tools)?);
        return Ok(());
    }
    for tool in tools {
        println!("{} | {} | {}", tool.id, tool.name, tool.description);
    }
    Ok(())
}

fn handle_mcp_call(args: &[String]) -> Result<()> {
    if args.len() < 2 {
        bail!("usage: hc-tool-cli mcp call <server-id> <tool-name> [key=value ...] [--json]");
    }
    let mut json_output = false;
    let server_id = args[0].clone();
    let tool_name = args[1].clone();
    let mut call_args = Vec::new();
    for arg in &args[2..] {
        if arg == "--json" {
            json_output = true;
        } else {
            call_args.push(arg.clone());
        }
    }
    let server = mcp_server_repository().get_server(&server_id)?;
    let mut arguments = arguments_from_run_args(&call_args, None)?;
    insert_missing_platform_mcp_runtime_arguments(&mut arguments);
    let result = call_mcp_tool(&server, &tool_name, serde_json::Value::Object(arguments))?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        print_mcp_result(&result);
    }
    Ok(())
}

fn handle_natural_language_tool_create(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    input: &str,
    user_system: Option<&str>,
) -> Result<bool> {
    let catalog = load_cli_tool_catalog()?;
    let build = synthesize_tool_build_from_natural_language(
        registry,
        provider,
        model,
        input,
        &catalog,
        user_system,
    )?;

    match build.action.as_str() {
        "ignore" => Ok(false),
        "ask_clarification" => {
            println!(
                "assistant> {}",
                build.message.unwrap_or_else(|| {
                    "I need a little more detail before creating that tool.".to_owned()
                })
            );
            Ok(true)
        }
        "create_tool" => {
            let draft = build.tool.context("tool creation response missed tool")?;
            let generated_files = draft.files.clone();
            let tool = tool_from_natural_language_draft(draft)?;
            if catalog.contains(&tool.id) {
                bail!(
                    "tool {} already exists. Use /create-tool for an explicit overwrite.",
                    tool.id
                );
            }
            let file_paths = write_generated_tool_files(&generated_files)?;
            let path = tool_repository().write_tool(&tool)?;
            println!(
                "assistant> created tool {} ({}) at {}",
                tool.id,
                tool.name,
                path.display()
            );
            for file_path in file_paths {
                println!("file> {}", file_path.display());
            }
            println!("command> {}", tool.default_command.join(" "));
            Ok(true)
        }
        "create_skill" => {
            let draft = build
                .skill
                .context("skill creation response missed skill")?;
            let skill = skill_from_natural_language_draft(draft)?;
            let path = SkillRepository::relative_path_for(&skill);
            if skill_repository().read_profile(&path).is_ok() {
                println!(
                    "assistant> skill {} ({}) already exists at {}",
                    skill.id,
                    skill.name,
                    path.display()
                );
                return Ok(true);
            }
            let path = skill_repository().write_profile(&skill)?;
            println!(
                "assistant> created skill {} ({}) at {}",
                skill.id,
                skill.name,
                path.display()
            );
            if let Some(tool_id) = skill.delegated_tool_id() {
                println!("delegates> {tool_id}");
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn handle_plan(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: hc-tool-cli plan <auto|rg|cargo-test|tool-id> <goal...> [--json]");
    }

    let mut json = false;
    let mut positional = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => positional.push(other.to_owned()),
        }
    }

    let selector = positional
        .first()
        .cloned()
        .context("missing tool selector for plan")?;
    let goal = positional
        .get(1..)
        .filter(|parts| !parts.is_empty())
        .map(|parts| parts.join(" "))
        .context("missing goal for plan")?;

    let catalog = load_cli_tool_catalog()?;
    let (resolved, selected_tool, candidates) = if selector == "auto" {
        let (selected_tool, candidates) = auto_select_tool(&catalog, &goal)?;
        (
            resolve_tool_selector(&selected_tool.id)?,
            selected_tool,
            candidates,
        )
    } else {
        let resolved = resolve_tool_selector(&selector)?;
        (resolved.clone(), resolved.tool, Vec::new())
    };
    let planning_tool = resolved.delegated_tool.as_ref().unwrap_or(&resolved.tool);
    let mut plan = build_default_tool_execution_plan(planning_tool, &goal)?;
    plan.tool_id = resolved.tool.id.clone();
    if let Some(skill) = &resolved.skill
        && !skill.instructions.trim().is_empty()
    {
        plan.guidance.insert(0, skill.instructions.clone());
    }

    if json {
        let payload = serde_json::json!({
            "tool": resolved.tool,
            "delegated_tool": resolved.delegated_tool,
            "skill": resolved.skill,
            "plan": plan,
            "candidates": candidates,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if selector == "auto" {
        println!("selected> {}", selected_tool.id);
    }
    println!("tool> {}", plan.tool_id);
    println!("command> {}", plan.suggested_command.join(" "));
    print_lines("guidance", &plan.guidance);
    print_lines("validation", &plan.validation_steps);
    print_lines("recovery", &plan.recovery_steps);
    Ok(())
}

fn handle_run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: hc-tool-cli run <rg|cargo-test|tool-id> [args...] [--json]");
    }

    let selector = args[0].clone();
    let options = parse_run_options(&args[1..])?;
    let (plan, outcome) = execute_tool_by_selector(&selector, &options)?;

    if options.json {
        let payload = serde_json::json!({
            "plan": plan,
            "outcome": outcome,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("tool> {}", outcome.tool_id);
    println!("success> {}", outcome.success);
    println!("summary> {}", outcome.summary);
    println!("command> {}", outcome.command.join(" "));
    print_lines("output", &outcome.observations);
    Ok(())
}

fn execute_tool_by_selector(
    selector: &str,
    options: &RunOptions,
) -> Result<(hc_toolchain::ToolExecutionPlan, ToolExecutionOutcome)> {
    let resolved = resolve_tool_selector(selector)?;
    execute_resolved_tool(resolved, options)
}

fn execute_resolved_tool(
    resolved: ResolvedToolTarget,
    options: &RunOptions,
) -> Result<(hc_toolchain::ToolExecutionPlan, ToolExecutionOutcome)> {
    let tool = resolved.tool.clone();
    let delegated_tool = resolved
        .delegated_tool
        .clone()
        .unwrap_or_else(|| tool.clone());
    let goal = options
        .goal
        .clone()
        .unwrap_or_else(|| default_run_goal(&tool, &options.args));
    let mut plan = build_default_tool_execution_plan(&delegated_tool, &goal)?;
    plan.tool_id = tool.id.clone();
    if let Some(skill) = &resolved.skill
        && !skill.instructions.trim().is_empty()
    {
        plan.guidance.insert(0, skill.instructions.clone());
    }
    plan.suggested_command = runnable_command(&delegated_tool, &options)?;

    let delegated_plan = hc_toolchain::ToolExecutionPlan {
        tool_id: delegated_tool.id.clone(),
        suggested_command: plan.suggested_command.clone(),
        guidance: plan.guidance.clone(),
        validation_steps: plan.validation_steps.clone(),
        recovery_steps: plan.recovery_steps.clone(),
    };
    let atomic_outcome = if is_mcp_tool_command(&delegated_tool.default_command) {
        execute_mcp_tool(&delegated_tool, &delegated_plan, &options, &goal)?
    } else {
        match execute_builtin_tool(&delegated_tool, &delegated_plan, &options, &goal)? {
            Some(outcome) => outcome,
            None => {
                let executor = match &options.path {
                    Some(path) => CommandToolExecutor::new().with_working_dir(path),
                    None => CommandToolExecutor::new(),
                };
                executor.execute(&delegated_plan, &goal)?
            }
        }
    };
    let outcome = if delegated_tool.id != tool.id {
        atomic_outcome.wrapped_by(tool.id.clone())
    } else {
        atomic_outcome
    };

    Ok((plan, outcome))
}

fn parse_common_options(args: &[String]) -> Result<CommonOptions> {
    let mut options = CommonOptions::default();
    for arg in args {
        match arg.as_str() {
            "--json" => options.json = true,
            other => bail!("unexpected argument: {other}"),
        }
    }
    Ok(options)
}

fn parse_selector_and_common_options(
    args: &[String],
    command_name: &str,
) -> Result<(String, CommonOptions)> {
    let mut selector = None;
    let mut options = CommonOptions::default();
    for arg in args {
        match arg.as_str() {
            "--json" => options.json = true,
            other if selector.is_none() => selector = Some(other.to_owned()),
            other => bail!("unexpected argument for {command_name}: {other}"),
        }
    }
    let selector = selector.with_context(|| format!("missing tool selector for {command_name}"))?;
    Ok((selector, options))
}

fn parse_run_options(args: &[String]) -> Result<RunOptions> {
    let mut options = RunOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                options.json = true;
                index += 1;
            }
            "--path" => {
                options.path = Some(PathBuf::from(
                    args.get(index + 1)
                        .context("missing value for --path")?
                        .as_str(),
                ));
                index += 2;
            }
            "--package" | "-p" => {
                options.package = Some(args.get(index + 1).cloned().context("missing package")?);
                index += 2;
            }
            "--goal" => {
                options.goal = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --goal")?,
                );
                index += 2;
            }
            "--content" => {
                options.content = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --content")?,
                );
                index += 2;
            }
            value => {
                options.args.push(value.to_owned());
                index += 1;
            }
        }
    }
    Ok(options)
}

fn execute_routed_tool(route: &NaturalLanguageToolRoute) -> Result<String> {
    let (plan, outcome) = execute_routed_tool_outcome(route)?;
    Ok(render_tool_execution_context(&plan, &outcome))
}

fn execute_routed_tool_outcome(
    route: &NaturalLanguageToolRoute,
) -> Result<(hc_toolchain::ToolExecutionPlan, ToolExecutionOutcome)> {
    let tool_id = route
        .tool_id
        .as_deref()
        .filter(|tool_id| !tool_id.trim().is_empty())
        .context("tool router selected run_tool without tool_id")?;
    let mut selectors = vec![tool_id.to_owned()];
    selectors.extend(
        route
            .fallback_tool_ids
            .iter()
            .filter(|tool_id| !tool_id.trim().is_empty())
            .cloned(),
    );
    selectors.dedup();

    let mut last_result = None;
    for selector in selectors {
        let options = RunOptions {
            goal: route.goal.clone(),
            args: route.args.clone(),
            ..RunOptions::default()
        };
        match execute_tool_by_selector(&selector, &options) {
            Ok((plan, outcome)) if outcome.success => return Ok((plan, outcome)),
            Ok((plan, outcome)) => {
                last_result = Some(Ok((plan, outcome)));
            }
            Err(error) => {
                last_result = Some(Ok(synthetic_routed_tool_error_outcome(
                    &selector, route, error,
                )));
            }
        }
    }
    last_result.context("no routed tool candidates were available")?
}

fn synthetic_routed_tool_error_outcome(
    selector: &str,
    route: &NaturalLanguageToolRoute,
    error: anyhow::Error,
) -> (hc_toolchain::ToolExecutionPlan, ToolExecutionOutcome) {
    let summary = format!(
        "tool call failed: {}",
        compact_single_line(&error.to_string(), 300)
    );
    (
        hc_toolchain::ToolExecutionPlan {
            tool_id: selector.to_owned(),
            suggested_command: Vec::new(),
            guidance: Vec::new(),
            validation_steps: Vec::new(),
            recovery_steps: Vec::new(),
        },
        ToolExecutionOutcome {
            tool_id: selector.to_owned(),
            parent_tool_id: None,
            invoked_tool_ids: Vec::new(),
            goal: route.goal.clone().unwrap_or_default(),
            command: Vec::new(),
            success: false,
            summary: summary.clone(),
            observations: vec![summary],
        },
    )
}

fn run_explicit_command_node(
    trimmed: &str,
    history: &mut Vec<ChatMessage>,
    system_message: Option<&str>,
    memory_options: &ChatMemoryOptions,
    capture_options: &ChatCaptureOptions,
) -> Result<Option<ExplicitCommandNodeResult>> {
    match trimmed {
        "/quit" | "/exit" => Ok(Some(ExplicitCommandNodeResult::Exit)),
        "/help" => {
            println!("/help");
            println!("/clear");
            println!("/tools");
            println!("/plan <goal>");
            println!(
                "/create-tool <id> <name> --description <text> --command <token> [--command <token>] [--tag <tag>]"
            );
            println!("/mcp add|list|tools|call ...");
            println!(
                "chat options: --tenant-id <id> --user-id <id> --session-id <id> --no-memory --memory-limit <n> --scope <scope> --memory-kind <kind> --tag <tag> --show-memory"
            );
            println!("/quit");
            Ok(Some(ExplicitCommandNodeResult::Continue))
        }
        "/clear" => {
            reset_chat_history(history, system_message, memory_options, capture_options)?;
            println!("history cleared");
            Ok(Some(ExplicitCommandNodeResult::Continue))
        }
        "/tools" => {
            let catalog = load_cli_tool_catalog()?;
            for tool in catalog.list() {
                println!("{} | {} | {}", tool.id, tool.name, tool.description);
            }
            Ok(Some(ExplicitCommandNodeResult::Continue))
        }
        _ if trimmed.starts_with("/plan ") => {
            let catalog = load_cli_tool_catalog()?;
            let goal = trimmed
                .strip_prefix("/plan ")
                .map(str::trim)
                .unwrap_or_default();
            if goal.is_empty() {
                println!("usage: /plan <goal>");
                return Ok(Some(ExplicitCommandNodeResult::Continue));
            }
            let (tool, _) = auto_select_tool(&catalog, goal)?;
            let plan = build_default_tool_execution_plan(&tool, goal)?;
            println!("tool> {}", plan.tool_id);
            println!("command> {}", plan.suggested_command.join(" "));
            print_lines("guidance", &plan.guidance);
            Ok(Some(ExplicitCommandNodeResult::Continue))
        }
        _ if trimmed.starts_with("/create-tool ") => {
            match handle_create_from_chat(trimmed.strip_prefix("/create-tool ").unwrap_or("")) {
                Ok(path) => {
                    println!("created> {}", path.display());
                    reset_chat_history(history, system_message, memory_options, capture_options)?;
                }
                Err(error) => println!("error> {error}"),
            }
            Ok(Some(ExplicitCommandNodeResult::Continue))
        }
        _ => Ok(None),
    }
}

fn reset_chat_history(
    history: &mut Vec<ChatMessage>,
    system_message: Option<&str>,
    memory_options: &ChatMemoryOptions,
    capture_options: &ChatCaptureOptions,
) -> Result<()> {
    let catalog = load_cli_tool_catalog()?;
    history.clear();
    history.push(ChatMessage::new(
        MessageRole::System,
        render_tool_chat_system_prompt(
            &catalog,
            system_message,
            &memory_options.namespace,
            capture_options.room_id.as_deref(),
        )?,
    ));
    Ok(())
}

fn run_pending_confirmation_node(
    frame: &mut TurnFrame,
    _: &ToolCatalog,
) -> Result<Option<TurnNodeReply>> {
    let request = chat_request_from_turn_frame(frame);
    let Some(tool_result) =
        try_handle_persisted_pending_confirmation(&ServiceConfig::new(workspace_root()), &request)?
    else {
        return Ok(None);
    };
    frame.set_tool_execution_context(format!(
        "Service-layer pending confirmation handled by {}/{}.",
        tool_result.server_id, tool_result.tool_name
    ));
    Ok(Some(TurnNodeReply {
        reply: Some(tool_result.response.message.content),
        warning: None,
        clear_pending_confirmation: true,
        next_pending_confirmation: None,
        stop_pipeline: true,
        reset_system_prompt: false,
    }))
}

fn run_timed_sequence_node(
    frame: &TurnFrame,
    history: &[ChatMessage],
) -> Result<Option<TurnNodeReply>> {
    let Some(reply) = handle_timed_sequence_turn(
        &frame.user_turn,
        history,
        &frame.runtime.identity.tenant_id,
        &frame.runtime.identity.user_id,
        frame.room_id(),
    )?
    else {
        return Ok(None);
    };
    Ok(Some(TurnNodeReply {
        reply: Some(reply),
        warning: None,
        clear_pending_confirmation: false,
        next_pending_confirmation: None,
        stop_pipeline: true,
        reset_system_prompt: false,
    }))
}

fn run_configured_agent_mcp_node(
    frame: &mut TurnFrame,
    _: &ToolCatalog,
) -> Result<Option<TurnNodeReply>> {
    let request = chat_request_from_turn_frame(frame);
    let tool_result = match try_handle_configured_mcp_turn(
        &ServiceConfig::new(workspace_root()),
        &request,
    ) {
        Ok(Some(tool_result)) => tool_result,
        Ok(None) => return Ok(None),
        Err(error) => {
            frame.set_tool_execution_context(format!(
                "Internal note: configured MCP turn failed before producing a user-presentable result: {}. Continue ordinary intent handling without inventing concrete tool data.",
                compact_single_line(&error.to_string(), 300)
            ));
            return Ok(Some(TurnNodeReply {
                reply: None,
                warning: None,
                clear_pending_confirmation: false,
                next_pending_confirmation: None,
                stop_pipeline: false,
                reset_system_prompt: false,
            }));
        }
    };
    Ok(Some(TurnNodeReply {
        reply: Some(tool_result.response.message.content),
        warning: None,
        clear_pending_confirmation: false,
        next_pending_confirmation: None,
        stop_pipeline: true,
        reset_system_prompt: false,
    }))
}

fn chat_request_from_turn_frame(frame: &TurnFrame) -> ChatRequest {
    ChatRequest {
        tenant_id: Some(frame.runtime.identity.tenant_id.clone()),
        user_id: Some(frame.runtime.identity.user_id.clone()),
        session_id: Some(frame.runtime.identity.session_id.clone()),
        input: Some(frame.user_turn.clone()),
        messages: vec![ApiChatMessage {
            role: ApiMessageRole::User,
            content: frame.user_turn.clone(),
            name: None,
        }],
        provider: None,
        model: None,
        system_prompt: None,
        agent_id: frame.selected_agent_id.clone(),
        domain_id: frame.selected_domain_id.clone(),
        active_agent_id: frame.selected_agent_id.clone(),
        active_task_id: None,
        memory: ApiMemoryQuery {
            namespace: ApiNamespace {
                tenant_id: frame.namespace.tenant_id.clone(),
                user_id: frame.namespace.user_id.clone(),
            },
            scope: None,
            kind: None,
            tag: None,
            text: None,
            limit: None,
        },
        temperature: None,
        max_output_tokens: None,
    }
}

fn run_llm_tool_router_node(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    frame: &mut TurnFrame,
    catalog: &ToolCatalog,
    system_message: Option<&str>,
) -> Result<TurnNodeReply> {
    match route_tool_turn(
        registry,
        provider,
        model,
        &frame.user_turn,
        &frame.selection,
        system_message,
    ) {
        Ok(route) if route.action == "create_tool" || route.action == "create_skill" => {
            match handle_natural_language_tool_create(
                registry,
                provider,
                model,
                &frame.user_turn,
                system_message,
            ) {
                Ok(true) => Ok(TurnNodeReply {
                    reply: None,
                    warning: None,
                    clear_pending_confirmation: false,
                    next_pending_confirmation: None,
                    stop_pipeline: true,
                    reset_system_prompt: true,
                }),
                Ok(false) => Ok(TurnNodeReply {
                    reply: None,
                    warning: None,
                    clear_pending_confirmation: false,
                    next_pending_confirmation: None,
                    stop_pipeline: false,
                    reset_system_prompt: false,
                }),
                Err(error) => Ok(TurnNodeReply {
                    reply: None,
                    warning: Some(format!("warning> tool builder skipped: {error}")),
                    clear_pending_confirmation: false,
                    next_pending_confirmation: None,
                    stop_pipeline: false,
                    reset_system_prompt: false,
                }),
            }
        }
        Ok(mut route) if route.action == "run_tool" => {
            append_platform_args_to_mcp_route(
                &mut route,
                catalog,
                &frame.namespace,
                frame.session_id(),
            );
            let context = execute_routed_tool(&route)?;
            apply_tool_route(&mut frame.selection, catalog, route)?;
            frame.set_tool_execution_context(context);
            Ok(TurnNodeReply {
                reply: None,
                warning: None,
                clear_pending_confirmation: false,
                next_pending_confirmation: None,
                stop_pipeline: false,
                reset_system_prompt: false,
            })
        }
        Ok(route) => {
            apply_tool_route(&mut frame.selection, catalog, route)?;
            Ok(TurnNodeReply {
                reply: None,
                warning: None,
                clear_pending_confirmation: false,
                next_pending_confirmation: None,
                stop_pipeline: false,
                reset_system_prompt: false,
            })
        }
        Err(error) => Ok(TurnNodeReply {
            reply: None,
            warning: Some(render_router_warning(&error)),
            clear_pending_confirmation: false,
            next_pending_confirmation: None,
            stop_pipeline: false,
            reset_system_prompt: false,
        }),
    }
}

fn run_normal_chat_node(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    history: &[ChatMessage],
    frame: &TurnFrame,
) -> Result<NormalChatNodeResult> {
    let request_history = build_chat_request_history(
        history,
        merge_optional_contexts([
            render_turn_frame_context(frame),
            render_recalled_memory_context(&frame.recalled_memories),
            render_tool_selection_context(&frame.selection),
            frame.tool_execution_context.clone(),
        ]),
        &frame.user_turn,
    );
    let request = GenerateRequest::new(
        ModelRef::new(provider.to_owned(), model.to_owned()),
        request_history,
    );
    let response = match registry.generate(&request) {
        Ok(response) => response,
        Err(error) => {
            return Ok(NormalChatNodeResult::Error {
                message: render_chat_error(&error),
            });
        }
    };
    let content = sanitize_model_response(&response.message.content);
    match try_execute_create_tool_command_from_response(&content) {
        Ok(Some(path)) => Ok(NormalChatNodeResult::CreatedTool { path }),
        Ok(None) => Ok(NormalChatNodeResult::AssistantReply {
            artifact_paths: persist_response_artifacts(&frame.user_turn, &content)?,
            content,
        }),
        Err(error) => Ok(NormalChatNodeResult::InvalidCreateCommand {
            content,
            warning: format!("warning> ignored invalid create command from model: {error}"),
        }),
    }
}

fn apply_turn_node_reply_state(
    node_reply: &TurnNodeReply,
    pending_confirmation: &mut Option<PendingToolConfirmation>,
) {
    if node_reply.clear_pending_confirmation {
        *pending_confirmation = None;
    }
    if let Some(next_pending) = &node_reply.next_pending_confirmation {
        *pending_confirmation = Some(next_pending.clone());
    }
}

fn print_turn_node_warning(node_reply: &TurnNodeReply) {
    if let Some(warning) = &node_reply.warning {
        println!("{warning}");
    }
}

fn emit_turn_node_reply(
    frame: &TurnFrame,
    node_reply: &TurnNodeReply,
    history: &mut Vec<ChatMessage>,
    room: Option<&MemoryRoom>,
) -> Result<bool> {
    let Some(reply) = &node_reply.reply else {
        return Ok(false);
    };
    if !reply.trim().is_empty() {
        println!("assistant> {reply}");
    }
    history.push(ChatMessage::new(MessageRole::User, frame.user_turn.clone()));
    if !reply.trim().is_empty() {
        persist_assistant_reply(frame, reply.clone(), history, room)?;
    }
    Ok(true)
}

fn emit_normal_chat_assistant_reply(
    frame: &TurnFrame,
    content: String,
    artifact_paths: Vec<PathBuf>,
    history: &mut Vec<ChatMessage>,
    room: Option<&MemoryRoom>,
) -> Result<()> {
    if content.trim().is_empty() {
        println!(
            "warning> model emitted a provider tool-call marker instead of normal text; ignored it. Please retry."
        );
    } else {
        println!("{content}");
        for path in artifact_paths {
            println!("saved> {}", path.display());
        }
    }
    history.push(ChatMessage::new(MessageRole::User, frame.user_turn.clone()));
    persist_assistant_reply(frame, content, history, room)
}

fn emit_invalid_create_command_reply(
    frame: &TurnFrame,
    content: String,
    warning: String,
    history: &mut Vec<ChatMessage>,
) {
    println!("{content}");
    println!("{warning}");
    history.push(ChatMessage::new(MessageRole::User, frame.user_turn.clone()));
    history.push(ChatMessage::new(MessageRole::Assistant, content));
}

fn persist_assistant_reply(
    frame: &TurnFrame,
    reply: String,
    history: &mut Vec<ChatMessage>,
    room: Option<&MemoryRoom>,
) -> Result<()> {
    history.push(ChatMessage::new(MessageRole::Assistant, reply.clone()));
    if let Some(room) = room
        && let Err(error) = persist_chat_turn_assistant_reply(
            workspace_root(),
            frame.workspace_namespace.clone(),
            room,
            frame.turn_index,
            reply,
        )
    {
        println!("warning> chat memory write skipped: {error}");
    }
    Ok(())
}

fn handle_timed_sequence_turn(
    user_turn: &str,
    history: &[ChatMessage],
    tenant_id: &str,
    user_id: &str,
    room_id: Option<String>,
) -> Result<Option<String>> {
    if let Some(reply) = handle_reminder_turn(user_turn, tenant_id, user_id, room_id.clone())? {
        return Ok(Some(reply));
    }

    let Some((rule, values)) = timed_sequence_for_turn_with_history(user_turn, history) else {
        return Ok(None);
    };
    let namespace = WorkspaceNamespace::new(tenant_id.to_owned(), user_id.to_owned());
    let repository = ConversationRepository::with_namespace(workspace_root(), namespace);
    let now = now_unix();
    let sequence_prefix = if rule.id.trim().is_empty() {
        "timed-sequence"
    } else {
        rule.id.trim()
    };
    let sequence_id = format!("{sequence_prefix}.{now}");
    let trigger = rule
        .trigger
        .clone()
        .unwrap_or_else(|| "timed_sequence.tick".to_owned());
    let agent_id = rule
        .agent_id
        .clone()
        .unwrap_or_else(|| "agent.system.timer".to_owned());

    for (index, value) in values.iter().enumerate() {
        let mut followup = PendingFollowUp::new(
            agent_id.clone(),
            trigger.clone(),
            now.saturating_add(rule.interval_seconds.saturating_mul(index as u64)),
        );
        followup.id = format!("{sequence_id}.{index}");
        followup.room_id = room_id.clone();
        followup.payload.insert(
            "sequence_id".to_owned(),
            serde_json::Value::String(sequence_id.clone()),
        );
        followup
            .payload
            .insert("index".to_owned(), serde_json::json!(index));
        followup.payload.insert(
            "draft_message".to_owned(),
            serde_json::Value::String(value.to_string()),
        );
        followup.notes = format!("Timed sequence tick {index}: {value}");
        repository.write_followup(&followup)?;
    }

    let reply = rule
        .scheduled_reply
        .clone()
        .unwrap_or_else(|| "scheduled timed sequence".to_owned());
    println!("assistant> {reply}");
    deliver_timed_sequence_followups(&repository, &sequence_id, values.len())?;
    Ok(Some(String::new()))
}

fn handle_reminder_turn(
    user_turn: &str,
    tenant_id: &str,
    user_id: &str,
    room_id: Option<String>,
) -> Result<Option<String>> {
    let Some((rule, delay_seconds)) = reminder_for_turn(user_turn) else {
        return Ok(None);
    };
    let namespace = WorkspaceNamespace::new(tenant_id.to_owned(), user_id.to_owned());
    let repository = ConversationRepository::with_namespace(workspace_root(), namespace.clone());
    let now = now_unix();
    let reminder_prefix = if rule.id.trim().is_empty() {
        "reminder"
    } else {
        rule.id.trim()
    };
    let reminder_id = format!("{reminder_prefix}.{now}");
    let trigger = rule
        .trigger
        .clone()
        .unwrap_or_else(|| "reminder.due".to_owned());
    let agent_id = rule
        .agent_id
        .clone()
        .unwrap_or_else(|| "agent.system.reminder".to_owned());
    let due_reply = rule
        .due_reply
        .clone()
        .unwrap_or_else(|| "到时间了。".to_owned());
    let mut followup = PendingFollowUp::new(agent_id, trigger, now.saturating_add(delay_seconds));
    followup.id = reminder_id.clone();
    followup.room_id = room_id;
    followup.payload.insert(
        "draft_message".to_owned(),
        serde_json::Value::String(due_reply),
    );
    followup.payload.insert(
        "source_turn".to_owned(),
        serde_json::Value::String(user_turn.to_owned()),
    );
    followup.notes = format!("Reminder due in {delay_seconds} seconds.");
    repository.write_followup(&followup)?;
    spawn_reminder_worker(namespace, reminder_id, delay_seconds);
    Ok(Some(
        rule.scheduled_reply
            .clone()
            .unwrap_or_else(|| "好，到时间我会提醒您。".to_owned()),
    ))
}

fn reminder_for_turn(user_turn: &str) -> Option<(&'static ReminderRule, u64)> {
    tool_routing_tags().reminder_rules.iter().find_map(|rule| {
        if !text_matches_any(user_turn, &rule.hints) {
            return None;
        }
        let delay_seconds = reminder_delay_seconds(user_turn, rule.default_delay_seconds)?;
        Some((rule, delay_seconds))
    })
}

fn reminder_delay_seconds(text: &str, default_delay_seconds: u64) -> Option<u64> {
    let numbers = extract_i64_numbers(text);
    let value = numbers
        .first()
        .copied()
        .filter(|value| *value > 0)
        .map(|value| value as u64);
    let unit_seconds = if contains_any(text, &["\u{6beb}\u{79d2}", "ms"]) {
        0
    } else if contains_any(
        text,
        &["\u{5c0f}\u{65f6}", "\u{949f}\u{5934}", "hour", "hours", "h"],
    ) {
        60 * 60
    } else if contains_any(
        text,
        &["\u{5206}\u{949f}", "\u{5206}", "minute", "minutes", "min"],
    ) {
        60
    } else if contains_any(text, &["\u{79d2}", "second", "seconds", "sec", "s"]) {
        1
    } else {
        return Some(default_delay_seconds);
    };
    Some(value.unwrap_or(1).saturating_mul(unit_seconds).max(1))
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn spawn_reminder_worker(namespace: WorkspaceNamespace, followup_id: String, delay_seconds: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(delay_seconds));
        let repository = ConversationRepository::with_namespace(workspace_root(), namespace);
        let relative_path = ConversationRepository::followup_relative_path_for(&followup_id);
        let Ok(mut followup) = repository.read_followup(relative_path) else {
            return;
        };
        if followup.status != FollowUpStatus::Pending {
            return;
        }
        if let Some(message) = followup
            .payload
            .get("draft_message")
            .and_then(serde_json::Value::as_str)
        {
            println!("\nassistant> {message}");
        }
        followup.status = FollowUpStatus::Fired;
        followup.notes = format!("Reminder fired at {}", now_unix());
        let _ = repository.write_followup(&followup);
    });
}

fn timed_sequence_for_turn_with_history(
    user_turn: &str,
    history: &[ChatMessage],
) -> Option<(&'static TimedSequenceRule, Vec<i64>)> {
    if let Some(sequence) = timed_sequence_for_turn(user_turn) {
        return Some(sequence);
    }
    let current_matches_timing = tool_routing_tags()
        .timed_sequence_rules
        .iter()
        .any(|rule| text_matches_any(user_turn, &rule.hints));
    if !current_matches_timing {
        return None;
    }
    let previous_user_turn = history
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.content.trim())
        .filter(|content| !content.is_empty())?;
    timed_sequence_for_turn(&format!("{previous_user_turn} {user_turn}"))
}

fn timed_sequence_for_turn(user_turn: &str) -> Option<(&'static TimedSequenceRule, Vec<i64>)> {
    tool_routing_tags()
        .timed_sequence_rules
        .iter()
        .find_map(|rule| {
            if !text_matches_any(user_turn, &rule.hints) {
                return None;
            }
            let numbers = extract_i64_numbers(user_turn);
            let start = *numbers.first()?;
            let end = timed_sequence_end(user_turn, start, &numbers, rule);
            let values = build_timed_sequence_values(start, end, rule)?;
            Some((rule, values))
        })
}

fn timed_sequence_end(
    user_turn: &str,
    start: i64,
    numbers: &[i64],
    rule: &TimedSequenceRule,
) -> i64 {
    if let Some(end) = numbers.get(1).copied() {
        return end;
    }
    if is_count_quantity_turn(user_turn) && rule.direction == "countdown" {
        return 1;
    }
    rule.default_end.unwrap_or(start)
}

fn is_count_quantity_turn(text: &str) -> bool {
    (text.contains("个数") || text.contains("个数字") || text.contains("个"))
        && !text.contains("到")
}

fn build_timed_sequence_values(start: i64, end: i64, rule: &TimedSequenceRule) -> Option<Vec<i64>> {
    let descending = if rule.direction == "countdown" {
        true
    } else if rule.direction == "countup" {
        false
    } else {
        start >= end
    };
    let mut values = Vec::new();
    if descending {
        let mut current = start;
        while current >= end {
            values.push(current);
            if values.len() > rule.max_items {
                return None;
            }
            current -= 1;
        }
    } else {
        let mut current = start;
        while current <= end {
            values.push(current);
            if values.len() > rule.max_items {
                return None;
            }
            current += 1;
        }
    }
    Some(values)
}

fn extract_i64_numbers(text: &str) -> Vec<i64> {
    let mut numbers = Vec::new();
    let mut ascii = String::new();
    let mut chinese = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() || (ch == '-' && ascii.is_empty() && chinese.is_empty()) {
            flush_chinese_number(&mut chinese, &mut numbers);
            ascii.push(ch);
        } else if is_chinese_number_char(ch) {
            flush_ascii_number(&mut ascii, &mut numbers);
            chinese.push(ch);
        } else {
            flush_ascii_number(&mut ascii, &mut numbers);
            flush_chinese_number(&mut chinese, &mut numbers);
        }
    }
    flush_ascii_number(&mut ascii, &mut numbers);
    flush_chinese_number(&mut chinese, &mut numbers);
    numbers
}

fn flush_ascii_number(current: &mut String, numbers: &mut Vec<i64>) {
    if !current.is_empty() {
        if let Ok(value) = current.parse::<i64>() {
            numbers.push(value);
        }
        current.clear();
    }
}

fn flush_chinese_number(current: &mut String, numbers: &mut Vec<i64>) {
    if !current.is_empty() {
        if let Some(value) = parse_chinese_i64(current) {
            numbers.push(value);
        }
        current.clear();
    }
}

fn is_chinese_number_char(ch: char) -> bool {
    matches!(
        ch,
        '零' | '〇'
            | '一'
            | '二'
            | '两'
            | '三'
            | '四'
            | '五'
            | '六'
            | '七'
            | '八'
            | '九'
            | '十'
            | '百'
            | '千'
    )
}

fn chinese_digit_value(ch: char) -> Option<i64> {
    match ch {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

fn parse_chinese_i64(text: &str) -> Option<i64> {
    if text.is_empty() {
        return None;
    }
    let mut total = 0i64;
    let mut current = 0i64;
    let mut saw_unit = false;
    for ch in text.chars() {
        match ch {
            '十' => {
                total += current.max(1) * 10;
                current = 0;
                saw_unit = true;
            }
            '百' => {
                total += current.max(1) * 100;
                current = 0;
                saw_unit = true;
            }
            '千' => {
                total += current.max(1) * 1000;
                current = 0;
                saw_unit = true;
            }
            other => {
                current = chinese_digit_value(other)?;
            }
        }
    }
    let value = if saw_unit { total + current } else { current };
    Some(value)
}

fn deliver_timed_sequence_followups(
    repository: &ConversationRepository,
    sequence_id: &str,
    expected_count: usize,
) -> Result<()> {
    let mut delivered = 0usize;
    while delivered < expected_count {
        let now = now_unix();
        let due = repository.due_followups(now)?;
        let mut matched_any = false;
        for mut followup in due {
            if followup
                .payload
                .get("sequence_id")
                .and_then(serde_json::Value::as_str)
                != Some(sequence_id)
            {
                continue;
            }
            matched_any = true;
            if let Some(message) = followup
                .payload
                .get("draft_message")
                .and_then(serde_json::Value::as_str)
            {
                println!("assistant> {message}");
            }
            followup.status = FollowUpStatus::Fired;
            repository.write_followup(&followup)?;
            delivered += 1;
        }
        if delivered >= expected_count {
            break;
        }
        if !matched_any {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(())
}

fn platform_mcp_runtime_run_args(
    namespace: &MemoryNamespace,
    session_id: Option<&str>,
) -> Vec<String> {
    let runtime = runtime_variables_for_namespace(namespace, session_id);
    vec![
        format!("tenant_id={}", runtime.identity.tenant_id),
        format!("user_id={}", runtime.identity.user_id),
        format!("session_id={}", runtime.identity.session_id),
        format!(
            "runtime={}",
            serde_json::Value::Object(runtime.values).to_string()
        ),
    ]
}

fn append_platform_args_to_mcp_route(
    route: &mut NaturalLanguageToolRoute,
    catalog: &ToolCatalog,
    namespace: &MemoryNamespace,
    session_id: Option<&str>,
) {
    let Some(tool_id) = route.tool_id.as_deref() else {
        return;
    };
    let Some(tool) = catalog.list().into_iter().find(|tool| tool.id == tool_id) else {
        return;
    };
    if !is_mcp_tool_command(&tool.default_command) {
        return;
    }
    route
        .args
        .splice(0..0, platform_mcp_runtime_run_args(namespace, session_id));
}

fn insert_missing_platform_mcp_runtime_arguments(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
) {
    let namespace = runtime_namespace();
    let runtime = runtime_variables_for_workspace_namespace(
        &namespace,
        cli_runtime_context().session_id.as_deref(),
    );
    runtime.inject_mcp_arguments(arguments);
}

fn runtime_variables_for_namespace(
    namespace: &MemoryNamespace,
    session_id: Option<&str>,
) -> RuntimeVariables {
    let identity = RuntimeIdentity::from_optional(
        Some(namespace.tenant_id.clone()),
        Some(namespace.user_id.clone()),
        session_id.map(ToOwned::to_owned),
    );
    load_runtime_variables(identity)
}

fn runtime_variables_for_workspace_namespace(
    namespace: &WorkspaceNamespace,
    session_id: Option<&str>,
) -> RuntimeVariables {
    let identity = RuntimeIdentity::from_optional(
        Some(namespace.tenant_id.clone()),
        Some(namespace.user_id.clone()),
        session_id.map(ToOwned::to_owned),
    );
    load_runtime_variables(identity)
}

fn load_runtime_variables(identity: RuntimeIdentity) -> RuntimeVariables {
    RuntimeVariableRepository::new(workspace_root())
        .load(identity.clone(), None)
        .unwrap_or_else(|_| RuntimeVariables::new(identity))
}

fn cli_api_namespace(namespace: &MemoryNamespace) -> ApiNamespace {
    ApiNamespace {
        tenant_id: namespace.tenant_id.clone(),
        user_id: namespace.user_id.clone(),
    }
}

fn cli_session_id(namespace: &MemoryNamespace, session_id: Option<&str>) -> String {
    runtime_variables_for_namespace(namespace, session_id)
        .identity
        .session_id
}

fn load_cli_pending_confirmation(
    namespace: &MemoryNamespace,
    session_id: Option<&str>,
) -> Result<Option<PendingToolConfirmation>> {
    let api_namespace = cli_api_namespace(namespace);
    let session_id = cli_session_id(namespace, session_id);
    let state = load_tool_turn_session_state(
        &ServiceConfig::new(workspace_root()),
        &api_namespace,
        &session_id,
    )?;
    Ok(state.pending_confirmation)
}

fn save_cli_pending_confirmation(
    namespace: &MemoryNamespace,
    session_id: Option<&str>,
    pending_confirmation: &Option<PendingToolConfirmation>,
) -> Result<()> {
    let api_namespace = cli_api_namespace(namespace);
    let session_id = cli_session_id(namespace, session_id);
    save_tool_turn_session_state(
        &ServiceConfig::new(workspace_root()),
        &api_namespace,
        &session_id,
        &ToolTurnSessionState {
            pending_confirmation: pending_confirmation.clone(),
        },
    )
}

fn tool_routing_tags() -> &'static ToolRoutingTags {
    static ROUTING_TAGS: OnceLock<ToolRoutingTags> = OnceLock::new();
    ROUTING_TAGS.get_or_init(|| load_tool_routing_tags().unwrap_or_default())
}

fn tool_response_rendering() -> &'static ToolResponseRenderingConfig {
    static RENDERING: OnceLock<ToolResponseRenderingConfig> = OnceLock::new();
    RENDERING.get_or_init(|| load_tool_response_rendering().unwrap_or_default())
}

fn load_tool_routing_tags() -> Result<ToolRoutingTags> {
    let path = workspace_namespace_root()
        .join("routing")
        .join("tool-routing-tags.md");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read tool routing tags: {}", path.display()))?;
    let frontmatter = markdown_frontmatter(&content)
        .with_context(|| format!("missing frontmatter in {}", path.display()))?;
    serde_yaml::from_str(frontmatter)
        .with_context(|| format!("failed to parse tool routing tags: {}", path.display()))
}

fn load_tool_response_rendering() -> Result<ToolResponseRenderingConfig> {
    let path = workspace_namespace_root()
        .join("rendering")
        .join("tool-response-rendering.md");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read tool response rendering: {}", path.display()))?;
    let frontmatter = markdown_frontmatter(&content)
        .with_context(|| format!("missing frontmatter in {}", path.display()))?;
    serde_yaml::from_str(frontmatter).with_context(|| {
        format!(
            "failed to parse tool response rendering: {}",
            path.display()
        )
    })
}

fn markdown_frontmatter(content: &str) -> Option<&str> {
    let content = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;
    let (frontmatter, _) = content
        .split_once("\r\n---")
        .or_else(|| content.split_once("\n---"))?;
    Some(frontmatter)
}

fn text_matches_any(text: &str, selectors: &[String]) -> bool {
    selectors
        .iter()
        .any(|selector| phrase_match_score(text, selector) > 0)
}

fn text_contains_selector(text: &str, selector: &str) -> bool {
    let selector = selector.trim();
    if selector.is_empty() {
        return false;
    }
    text.contains(selector)
        || text
            .to_ascii_lowercase()
            .contains(&selector.to_ascii_lowercase())
}

fn execute_builtin_tool(
    tool: &ToolSpec,
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<Option<ToolExecutionOutcome>> {
    if tool.execution_kind != ToolExecutionKind::Builtin {
        return Ok(None);
    }

    let Some(token) = tool.default_command.first() else {
        return Ok(None);
    };

    match token.as_str() {
        "hc.local-file.read" => execute_local_file_read(plan, options, goal).map(Some),
        "hc.local-file.write" => execute_local_file_write(plan, options, goal).map(Some),
        "hc.local-dir.list" => execute_local_dir_list(plan, options, goal).map(Some),
        _ => bail!("unsupported builtin tool command: {token}"),
    }
}

fn execute_mcp_tool(
    tool: &ToolSpec,
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let server_id = tool
        .default_command
        .get(1)
        .context("mcp tool command missed server id")?;
    let tool_name = tool
        .default_command
        .get(2)
        .context("mcp tool command missed tool name")?;
    let server = mcp_server_repository().get_server(server_id)?;
    let mut arguments = arguments_from_run_args(&options.args, options.content.as_deref())?;
    insert_missing_platform_mcp_runtime_arguments(&mut arguments);
    let arguments = serde_json::Value::Object(arguments);
    let result = call_mcp_tool(&server, tool_name, arguments)?;
    let success = !result
        .get("isError")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success,
        summary: if success {
            "mcp tool call completed".to_owned()
        } else {
            "mcp tool call returned an error result".to_owned()
        },
        observations: mcp_result_observations(&result),
    })
}

fn execute_local_file_read(
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let path_arg = options
        .args
        .first()
        .context("missing file path for local file read")?;
    let path = resolve_run_file_path(options.path.as_deref(), path_arg)?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read local file: {}", path.display()))?;

    let mut observations = vec![
        format!("path: {}", path.display()),
        format!("bytes: {}", content.len()),
    ];
    for line in content.lines().take(120) {
        observations.push(format!("content: {line}"));
    }
    if content.lines().count() > 120 {
        observations.push("content: ... truncated".to_owned());
    }

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: format!("read {} bytes from {}", content.len(), path.display()),
        observations,
    })
}

fn execute_local_file_write(
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let path_arg = options
        .args
        .first()
        .context("missing file path for local file write")?;
    let content = options
        .content
        .clone()
        .or_else(|| {
            if options.args.len() > 1 {
                Some(
                    options
                        .args
                        .iter()
                        .skip(1)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            } else {
                None
            }
        })
        .context("missing content for local file write; use --content <text>")?;
    let path = resolve_run_file_path(options.path.as_deref(), path_arg)?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory: {}", parent.display()))?;
    }
    fs::write(&path, content.as_bytes())
        .with_context(|| format!("failed to write local file: {}", path.display()))?;

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: format!("wrote {} bytes to {}", content.len(), path.display()),
        observations: vec![
            format!("path: {}", path.display()),
            format!("bytes: {}", content.len()),
        ],
    })
}

fn execute_local_dir_list(
    plan: &hc_toolchain::ToolExecutionPlan,
    options: &RunOptions,
    goal: &str,
) -> Result<ToolExecutionOutcome> {
    let path_arg = options.args.first().map(String::as_str).unwrap_or(".");
    let path = resolve_run_file_path(options.path.as_deref(), path_arg)?;
    let mut entries = fs::read_dir(&path)
        .with_context(|| format!("failed to list local directory: {}", path.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read local directory entry: {}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut observations = vec![format!("path: {}", path.display())];
    observations.push(format!("entries: {}", entries.len()));
    for entry in entries.iter().take(200) {
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type: {}", entry.path().display()))?;
        let kind = if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        observations.push(format!(
            "entry: {kind} {}",
            entry.file_name().to_string_lossy()
        ));
    }
    if entries.len() > 200 {
        observations.push("entry: ... truncated".to_owned());
    }

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command: plan.suggested_command.clone(),
        success: true,
        summary: format!("listed {} entries in {}", entries.len(), path.display()),
        observations,
    })
}

fn resolve_run_file_path(base: Option<&Path>, path_arg: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path_arg);
    if path.is_absolute() {
        return Ok(path);
    }
    let base = match base {
        Some(base) => base.to_path_buf(),
        None => env::current_dir().context("failed to resolve current directory")?,
    };
    Ok(base.join(path))
}

fn arguments_from_run_args(
    args: &[String],
    content: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut arguments = serde_json::Map::new();
    for arg in args {
        let Some((key, value)) = arg.split_once('=') else {
            bail!("mcp arguments must use key=value form: {arg}");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("mcp argument key cannot be empty");
        }
        arguments.insert(key.to_owned(), parse_jsonish_argument_value(value));
    }
    if let Some(content) = content {
        arguments.insert(
            "content".to_owned(),
            serde_json::Value::String(content.to_owned()),
        );
    }
    Ok(arguments)
}

fn parse_jsonish_argument_value(value: &str) -> serde_json::Value {
    serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_owned()))
}

fn parse_key_value(value: &str) -> Result<(String, String)> {
    let Some((key, value)) = value.split_once('=') else {
        bail!("expected key=value, got: {value}");
    };
    let key = key.trim();
    if key.is_empty() {
        bail!("key cannot be empty");
    }
    Ok((key.to_owned(), value.trim().to_owned()))
}

fn mcp_result_observations(result: &serde_json::Value) -> Vec<String> {
    let mut observations = Vec::new();
    if let Some(content) = result.get("content").and_then(serde_json::Value::as_array) {
        for item in content.iter().take(40) {
            if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                observations.push(format!("text: {text}"));
            } else {
                observations.push(format!("content: {item}"));
            }
        }
        if content.len() > 40 {
            observations.push("content: ... truncated".to_owned());
        }
    }
    if observations.is_empty() {
        observations.push(format!("result: {result}"));
    }
    observations
}

fn render_unrenderable_tool_reply(outcome: &ToolExecutionOutcome) -> String {
    renderer_for_outcome(outcome, None)
        .and_then(|renderer| renderer.empty_reply.clone())
        .unwrap_or_else(|| {
            "I found a service result, but could not turn it into a clear list yet.".to_owned()
        })
}

#[allow(dead_code)]
fn render_tool_failure_reply(outcome: &ToolExecutionOutcome) -> String {
    renderer_for_outcome(outcome, None)
        .and_then(|renderer| renderer.failure_reply.clone())
        .unwrap_or_else(|| "I did not get a usable result, so I will not invent one.".to_owned())
}

fn renderer_for_outcome<'a>(
    outcome: &ToolExecutionOutcome,
    kind: Option<&str>,
) -> Option<&'a ToolResponseRenderer> {
    tool_response_rendering()
        .renderers
        .iter()
        .filter(|renderer| kind.is_none_or(|kind| renderer.kind == kind))
        .find(|renderer| renderer_matches_outcome(renderer, outcome))
}

fn renderer_matches_outcome(
    renderer: &ToolResponseRenderer,
    outcome: &ToolExecutionOutcome,
) -> bool {
    if renderer.selectors.is_empty() {
        return false;
    }
    renderer.selectors.iter().any(|selector| {
        text_contains_selector(&renderer.id, selector)
            || text_contains_selector(&outcome.tool_id, selector)
            || outcome
                .command
                .iter()
                .any(|part| text_contains_selector(part, selector))
    })
}

#[allow(dead_code)]
fn legacy_render_grounded_tool_reply(outcome: &ToolExecutionOutcome) -> Option<String> {
    let value = extract_tool_json_from_observations(&outcome.observations)?;
    let items = legacy_extract_ranked_items(&value);
    if items.is_empty() {
        return Some(render_unrenderable_tool_reply(outcome));
    }

    let mut lines = vec![if outcome_has_health_context(outcome) {
        "我结合您的健康数据，查到了这些真实可选项：".to_owned()
    } else {
        "我查到了这些真实可选项：".to_owned()
    }];
    let mut items = items.into_iter().take(3);
    if let Some(primary) = items.next() {
        lines.push("默认推荐：".to_owned());
        lines.extend(render_primary_recommendation(primary));
    }
    let alternatives = items.collect::<Vec<_>>();
    if !alternatives.is_empty() {
        lines.push("备选：".to_owned());
        for (index, item) in alternatives.into_iter().enumerate() {
            lines.push(render_alternative_recommendation(index + 2, item));
        }
    }
    lines.push("如果确认默认推荐，请回复“确认下单”；想换备选，就回复对应序号。".to_owned());
    Some(lines.join("\n"))
}

#[allow(dead_code)]
fn render_primary_recommendation(item: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    let name = item_display_name(item);
    lines.push(format!("1. {name}"));
    if let Some(provider) = item_provider_name(item) {
        lines.push(format!("   商家：{provider}"));
    }
    if let Some(area) = item_area(item) {
        lines.push(format!("   位置：{}", compact_single_line(area, 80)));
    }
    if let Some(price) = item_price_text(item) {
        lines.push(format!("   价格：{price}"));
    }
    if let Some(reason) = readable_item_reason(item) {
        lines.push(format!("   推荐理由：{reason}"));
    }
    lines
}

#[allow(dead_code)]
fn render_alternative_recommendation(index: usize, item: &serde_json::Value) -> String {
    let mut details = Vec::new();
    if let Some(provider) = item_provider_name(item) {
        details.push(provider.to_owned());
    }
    if let Some(price) = item_price_text(item) {
        details.push(price);
    }
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(" - {}", details.join("；"))
    };
    format!("{index}. {}{suffix}", item_display_name(item))
}

#[allow(dead_code)]
fn item_display_name(item: &serde_json::Value) -> &str {
    item.get("name")
        .or_else(|| item.get("title"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("未命名")
}

#[allow(dead_code)]
fn item_provider_name(item: &serde_json::Value) -> Option<&str> {
    item.get("provider_name")
        .or_else(|| item.get("restaurant_name"))
        .or_else(|| item.get("merchant_name"))
        .and_then(serde_json::Value::as_str)
}

#[allow(dead_code)]
fn item_area(item: &serde_json::Value) -> Option<&str> {
    item.get("coverage_area")
        .or_else(|| item.get("address"))
        .and_then(serde_json::Value::as_str)
}

#[allow(dead_code)]
fn item_price_cents(item: &serde_json::Value) -> Option<i64> {
    item.get("average_price_cents")
        .or_else(|| item.get("price_cents"))
        .or_else(|| item.get("total_cents"))
        .and_then(serde_json::Value::as_i64)
}

#[allow(dead_code)]
fn item_price_text(item: &serde_json::Value) -> Option<String> {
    if let Some(price) = item_price_cents(item) {
        return Some(format!("约 {:.2} 元", price as f64 / 100.0));
    }
    item.get("total")
        .or_else(|| item.get("unit_price"))
        .and_then(serde_json::Value::as_f64)
        .map(|price| format!("约 {:.2} 元", price))
}

#[allow(dead_code)]
fn item_provider_id(item: &serde_json::Value) -> Option<i64> {
    item.get("provider_id")
        .or_else(|| item.get("restaurant_id"))
        .or_else(|| item.get("backend_provider_id"))
        .and_then(serde_json::Value::as_i64)
}

#[allow(dead_code)]
fn item_menu_id(item: &serde_json::Value) -> Option<i64> {
    item.get("menu_id")
        .or_else(|| item.get("id"))
        .or_else(|| item.get("listing_id"))
        .or_else(|| item.get("backend_listing_id"))
        .and_then(serde_json::Value::as_i64)
}

#[allow(dead_code)]
fn outcome_has_health_context(outcome: &ToolExecutionOutcome) -> bool {
    outcome
        .command
        .iter()
        .any(|part| part.trim_start().starts_with("health_context="))
}

#[allow(dead_code)]
fn readable_item_reason(item: &serde_json::Value) -> Option<String> {
    for key in ["recommendation_reasons", "health_advice"] {
        if let Some(values) = item.get(key).and_then(serde_json::Value::as_array) {
            let rendered = values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|value| is_user_readable_text(value))
                .take(2)
                .map(|value| compact_single_line(value, 80))
                .collect::<Vec<_>>()
                .join("；");
            if !rendered.trim().is_empty() {
                return Some(rendered);
            }
        }
    }
    for key in [
        "recommendation_reason",
        "health_reason",
        "reason",
        "why",
        "description",
        "summary",
    ] {
        if let Some(value) = item.get(key).and_then(serde_json::Value::as_str) {
            let value = compact_single_line(value, 110);
            if !value.trim().is_empty() && is_user_readable_text(&value) {
                return Some(value);
            }
        }
    }
    None
}

fn is_user_readable_text(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    let has_cjk = trimmed
        .chars()
        .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch));
    has_cjk || (!trimmed.contains('_') && !trimmed.contains('-'))
}

#[allow(dead_code)]
fn legacy_render_unrenderable_tool_reply(outcome: &ToolExecutionOutcome) -> String {
    let _ = outcome;
    "我已经查到了服务结果，但暂时不能整理成清晰的候选列表。请稍后再试一次。".to_owned()
}

#[allow(dead_code)]
fn legacy_render_tool_failure_reply(outcome: &ToolExecutionOutcome) -> String {
    let _ = outcome;
    "我刚才没有查到可用结果，先不编推荐。请稍后再试一次。".to_owned()
}

#[allow(dead_code)]
fn legacy_render_order_reply(outcome: &ToolExecutionOutcome) -> Option<String> {
    let value = extract_tool_json_from_observations(&outcome.observations)?;
    let order_id = value
        .get("order_id")
        .or_else(|| value.get("id"))
        .or_else(|| value.pointer("/order/id"))
        .and_then(serde_json::Value::as_i64);
    let status = value
        .get("status")
        .or_else(|| value.pointer("/order/status"))
        .and_then(serde_json::Value::as_str);
    let total_cents = value
        .get("total_cents")
        .or_else(|| value.get("price_cents"))
        .or_else(|| value.pointer("/order/total_cents"))
        .and_then(serde_json::Value::as_i64);
    let total_yuan = value
        .get("total")
        .or_else(|| value.pointer("/order/total"))
        .and_then(serde_json::Value::as_f64);
    let message = value.get("message").and_then(serde_json::Value::as_str);
    let mut lines = vec!["已按默认推荐为您提交订单。".to_owned()];
    if let Some(order_id) = order_id {
        lines.push(format!("订单号：{order_id}"));
    }
    if let Some(status) = status {
        lines.push(format!("状态：{}", readable_order_status(status)));
    }
    if let Some(total_cents) = total_cents {
        lines.push(format!("金额：约 {:.2} 元", total_cents as f64 / 100.0));
    } else if let Some(total_yuan) = total_yuan {
        lines.push(format!("金额：约 {:.2} 元", total_yuan));
    }
    if let Some(message) = message {
        lines.push(compact_single_line(message, 120));
    }
    Some(lines.join("\n"))
}

#[allow(dead_code)]
fn readable_order_status(status: &str) -> &str {
    match status {
        "pending" => "待确认",
        "confirmed" => "已确认",
        "cancelled" => "已取消",
        "created" => "已创建",
        _ => status,
    }
}

fn extract_tool_json_from_observations(observations: &[String]) -> Option<serde_json::Value> {
    for observation in observations {
        let text = observation
            .strip_prefix("text: ")
            .or_else(|| observation.strip_prefix("result: "))
            .or_else(|| observation.strip_prefix("content: "))
            .unwrap_or(observation);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
            return Some(value);
        }
    }
    None
}

#[allow(dead_code)]
fn legacy_extract_ranked_items(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    if let Some(array) = value.as_array() {
        return array.iter().filter(|item| item.is_object()).collect();
    }
    for key in [
        "recommended",
        "restaurants",
        "items",
        "results",
        "products",
        "orders",
        "dishes",
        "meals",
        "menus",
        "menu",
        "combos",
        "recommendations",
        "data",
    ] {
        if let Some(array) = value.get(key).and_then(serde_json::Value::as_array) {
            return array.iter().filter(|item| item.is_object()).collect();
        }
    }
    Vec::new()
}

fn print_mcp_result(result: &serde_json::Value) {
    for observation in mcp_result_observations(result) {
        println!("mcp> {observation}");
    }
}

fn parse_create_options(args: &[String]) -> Result<CreateOptions> {
    if args.len() < 2 {
        bail!(
            "usage: hc-tool-cli create <tool-id> <name> --description <text> --command <token> [--command <token>] [--kind <cli|builtin|script|workflow|service>] [--tag <tag>] [--json]"
        );
    }

    let id = normalize_tool_id(&args[0]);
    let name = args[1].clone();
    let mut description: Option<String> = None;
    let mut execution_kind = ToolExecutionKind::Cli;
    let mut command = Vec::new();
    let mut tags = Vec::new();
    let mut json = false;

    let mut index = 2usize;
    while index < args.len() {
        match args[index].as_str() {
            "--description" => {
                description = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --description")?,
                );
                index += 2;
            }
            "--kind" => {
                execution_kind = parse_tool_execution_kind(
                    args.get(index + 1).context("missing value for --kind")?,
                )?;
                index += 2;
            }
            "--command" => {
                command.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --command")?,
                );
                index += 2;
            }
            "--tag" => {
                tags.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected argument for create: {other}"),
        }
    }

    let description = description.context("missing --description for create")?;
    if command.is_empty()
        && matches!(
            execution_kind,
            ToolExecutionKind::Cli | ToolExecutionKind::Script
        )
    {
        bail!("missing --command for executable tool");
    }

    Ok(CreateOptions {
        id,
        name,
        description,
        execution_kind,
        command,
        tags,
        json,
    })
}

fn parse_mcp_add_options(args: &[String]) -> Result<McpAddOptions> {
    if args.len() < 2 {
        bail!(
            "usage: hc-tool-cli mcp add <server-id> <name> --description <text> [--url <endpoint> | --command <token> ...] [--transport <stdio|streamable_http|sse>] [--tag <tag>] [--json]"
        );
    }

    let mut options = McpAddOptions {
        id: args[0].clone(),
        name: args[1].clone(),
        ..McpAddOptions::default()
    };
    let mut index = 2usize;
    while index < args.len() {
        match args[index].as_str() {
            "--description" => {
                options.description = args
                    .get(index + 1)
                    .cloned()
                    .context("missing value for --description")?;
                index += 2;
            }
            "--command" => {
                options.command.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --command")?,
                );
                index += 2;
            }
            "--url" => {
                options.url = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --url")?,
                );
                index += 2;
            }
            "--transport" => {
                options.transport = Some(parse_mcp_transport_kind(
                    args.get(index + 1)
                        .context("missing value for --transport")?,
                )?);
                index += 2;
            }
            "--tag" => {
                options.tags.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--json" => {
                options.json = true;
                index += 1;
            }
            other => bail!("unexpected argument for mcp add: {other}"),
        }
    }
    if options.description.trim().is_empty() {
        bail!("missing --description for mcp add");
    }
    let transport = options.transport.clone().unwrap_or_else(|| {
        if options.url.is_some() {
            McpTransportKind::StreamableHttp
        } else {
            McpTransportKind::Stdio
        }
    });
    match transport {
        McpTransportKind::Stdio if options.command.is_empty() => {
            bail!("missing --command for stdio mcp add");
        }
        McpTransportKind::StreamableHttp | McpTransportKind::Sse if options.url.is_none() => {
            bail!("missing --url for http mcp add");
        }
        _ => {}
    }
    Ok(options)
}

fn parse_mcp_transport_kind(value: &str) -> Result<McpTransportKind> {
    match value {
        "stdio" => Ok(McpTransportKind::Stdio),
        "streamable_http" | "http" => Ok(McpTransportKind::StreamableHttp),
        "sse" => Ok(McpTransportKind::Sse),
        other => bail!("unsupported mcp transport: {other}"),
    }
}

fn tool_from_create_options(options: &CreateOptions) -> Result<ToolSpec> {
    let tags = normalized_tags(options.tags.clone(), "tool");

    let tool = ToolSpec {
        id: options.id.clone(),
        name: options.name.clone(),
        description: options.description.clone(),
        execution_kind: options.execution_kind.clone(),
        composition: ToolComposition::Atomic,
        stability: ToolStability::Managed,
        model_dependence: ModelDependence::Optional,
        default_command: options.command.clone(),
        tags,
    };
    hc_toolchain::validate_tool_spec(&tool)?;
    Ok(tool)
}

fn normalized_tags(mut tags: Vec<String>, required: &str) -> Vec<String> {
    if !tags.iter().any(|tag| tag == required) {
        tags.push(required.to_owned());
    }
    tags.sort();
    tags.dedup();
    tags
}

fn tool_from_natural_language_draft(draft: NaturalLanguageToolDraft) -> Result<ToolSpec> {
    let execution_kind = match draft.execution_kind.as_deref() {
        Some(value) => parse_tool_execution_kind(value)?,
        None => ToolExecutionKind::Cli,
    };
    let options = CreateOptions {
        id: normalize_tool_id(draft.id.trim()),
        name: draft.name.trim().to_owned(),
        description: draft.description.trim().to_owned(),
        execution_kind,
        command: draft.default_command,
        tags: draft.tags,
        json: false,
    };
    tool_from_create_options(&options)
}

fn skill_from_natural_language_draft(draft: NaturalLanguageSkillDraft) -> Result<SkillProfile> {
    let execution_kind = match draft.execution_kind.as_deref() {
        Some(value) => parse_tool_execution_kind(value)?,
        None => ToolExecutionKind::Builtin,
    };
    let mut profile = SkillProfile::new(normalize_skill_id(draft.id.trim()), draft.name.trim())
        .with_namespace(runtime_namespace())
        .with_description(draft.description.trim())
        .with_instructions(draft.instructions.trim())
        .with_execution_kind(execution_kind)
        .with_model_dependence(ModelDependence::Optional)
        .with_default_command(draft.default_command);
    if let Some(tool_id) = draft.tool_id.filter(|value| !value.trim().is_empty()) {
        profile = profile.with_tool_id(normalize_tool_id(tool_id.trim()));
    }
    for tool_ref in draft.tool_refs {
        if !tool_ref.trim().is_empty() {
            profile = profile.with_tool_ref(normalize_tool_id(tool_ref.trim()));
        }
    }
    for tag in draft.tags {
        if !tag.trim().is_empty() {
            profile = profile.with_tag(tag);
        }
    }
    if !profile.tags.iter().any(|tag| tag == "skill") {
        profile = profile.with_tag("skill");
    }
    Ok(profile)
}

fn write_generated_tool_files(files: &[NaturalLanguageToolFileDraft]) -> Result<Vec<PathBuf>> {
    write_generated_tool_files_under(files, &workspace_namespace_root())
}

fn write_generated_tool_files_under(
    files: &[NaturalLanguageToolFileDraft],
    root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for file in files {
        let path = resolve_workspace_relative_file_under(&file.path, root)?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }
        fs::write(&path, file.content.as_bytes())
            .with_context(|| format!("failed to write generated tool file: {}", path.display()))?;
        set_executable_if_requested(&path, file.executable)?;
        written.push(path);
    }
    Ok(written)
}

fn resolve_workspace_relative_file_under(path: &str, root: &Path) -> Result<PathBuf> {
    let relative = safe_relative_path(path)?;
    Ok(root.join(relative))
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        bail!("generated file path cannot be empty");
    }
    if path.is_absolute() {
        bail!("generated file path must be relative to the user workspace");
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("generated file path cannot contain parent directory components");
    }
    Ok(path)
}

fn set_executable_if_requested(path: &Path, executable: bool) -> Result<()> {
    if !executable {
        return Ok(());
    }
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .with_context(|| format!("failed to read permissions: {}", path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to set executable bit: {}", path.display()))?;
    }
    Ok(())
}

fn parse_tool_execution_kind(value: &str) -> Result<ToolExecutionKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "cli" => Ok(ToolExecutionKind::Cli),
        "builtin" => Ok(ToolExecutionKind::Builtin),
        "script" => Ok(ToolExecutionKind::Script),
        "workflow" => Ok(ToolExecutionKind::Workflow),
        "service" => Ok(ToolExecutionKind::Service),
        other => bail!(
            "unsupported tool execution kind: {other}. supported kinds: cli, builtin, script, workflow, service"
        ),
    }
}

fn normalize_tool_id(value: &str) -> String {
    if value.starts_with("tool.") {
        value.to_owned()
    } else {
        format!("tool.{value}")
    }
}

fn normalize_skill_id(value: &str) -> String {
    if value.starts_with("skill.") {
        value.to_owned()
    } else if let Some(rest) = value.strip_prefix("tool.") {
        format!("skill.{rest}")
    } else {
        format!("skill.{value}")
    }
}

fn resolve_tool(catalog: &ToolCatalog, selector: &str) -> Result<ToolSpec> {
    let normalized = match selector {
        "rg" => "tool.rg",
        "cargo-test" => "tool.cargo-test",
        other => other,
    };
    catalog
        .get(normalized)
        .cloned()
        .with_context(|| format!("unknown tool selector: {selector}"))
}

fn resolve_tool_selector(selector: &str) -> Result<ResolvedToolTarget> {
    let normalized = normalize_tool_selector(selector);

    if normalized.starts_with("skill.") {
        let skill = skill_repository()
            .read_profile(format!("skills/{normalized}.md"))
            .with_context(|| format!("failed to load skill {normalized}"))?;
        let delegated_tool = match skill.delegated_tool_id() {
            Some(tool_id) => load_cli_tool_catalog()?
                .get(tool_id)
                .cloned()
                .with_context(|| format!("skill {normalized} references unknown tool {tool_id}"))?,
            None => skill.to_tool_spec(),
        };
        return Ok(ResolvedToolTarget {
            tool: skill.to_tool_spec(),
            delegated_tool: Some(delegated_tool),
            skill: Some(skill),
        });
    }

    let catalog = load_cli_tool_catalog()?;
    if let Some(tool) = catalog.get(&normalized) {
        if tool.composition == ToolComposition::Composite {
            if let Some(skill) = skill_repository()
                .list_profiles()?
                .into_iter()
                .find(|skill| skill.resolved_tool_id() == tool.id)
            {
                let delegated_tool = match skill.delegated_tool_id() {
                    Some(tool_id) => load_cli_tool_catalog()?
                        .get(tool_id)
                        .cloned()
                        .with_context(|| {
                            format!("skill {} references unknown tool {tool_id}", skill.id)
                        })?,
                    None => skill.to_tool_spec(),
                };
                return Ok(ResolvedToolTarget {
                    tool: tool.clone(),
                    delegated_tool: Some(delegated_tool),
                    skill: Some(skill),
                });
            }
        }

        return Ok(ResolvedToolTarget {
            tool: tool.clone(),
            delegated_tool: None,
            skill: None,
        });
    }

    bail!("unsupported tool or skill selector: {selector}")
}

fn normalize_tool_selector(selector: &str) -> String {
    match selector {
        "rg" => "tool.rg".to_owned(),
        "cargo-test" => "tool.cargo-test".to_owned(),
        other => other.to_owned(),
    }
}

fn auto_select_tool(
    catalog: &ToolCatalog,
    goal: &str,
) -> Result<(ToolSpec, Vec<serde_json::Value>)> {
    let mut scored: Vec<(ToolSpec, i32)> = catalog
        .list()
        .into_iter()
        .map(|tool| (tool.clone(), score_tool_for_goal(tool, goal)))
        .collect();
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.id.cmp(&right.0.id))
    });

    let selected = scored
        .first()
        .map(|(tool, _)| tool.clone())
        .context("no tools are registered")?;
    let candidates = scored
        .into_iter()
        .map(|(tool, score)| {
            serde_json::json!({
                "tool_id": tool.id,
                "name": tool.name,
                "score": score,
            })
        })
        .collect();
    Ok((selected, candidates))
}

fn score_tool_for_goal(tool: &ToolSpec, goal: &str) -> i32 {
    let mut score = 0;
    score += phrase_match_score(goal, &tool.id) / 25;
    score += phrase_match_score(goal, &tool.name) / 20;
    score += phrase_match_score(goal, &tool.description) / 35;
    score += tool
        .tags
        .iter()
        .map(|tag| phrase_match_score(goal, tag) / 35)
        .sum::<i32>();
    for token in goal_match_terms(goal) {
        if token.is_empty() {
            continue;
        }
        let token_lowered = token.to_ascii_lowercase();
        if tool.id.to_ascii_lowercase().contains(&token_lowered) {
            score += 4;
        }
        if tool.name.to_ascii_lowercase().contains(&token_lowered) || tool.name.contains(&token) {
            score += 3;
        }
        if tool
            .description
            .to_ascii_lowercase()
            .contains(&token_lowered)
            || tool.description.contains(&token)
        {
            score += 2;
        }
        if tool
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(&token_lowered) || tag.contains(&token))
        {
            score += 2;
        }
    }
    score
}

fn goal_match_terms(goal: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lowered = goal.to_ascii_lowercase();
    for token in lowered.split(|ch: char| !ch.is_alphanumeric()) {
        if !token.is_empty() {
            terms.push(token.to_owned());
        }
    }

    let cjk_runs = goal
        .split(|ch: char| ch.is_ascii() || ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|part| !part.is_empty());
    for run in cjk_runs {
        terms.push(run.to_owned());
        let chars: Vec<char> = run.chars().collect();
        for window in chars.windows(2) {
            terms.push(window.iter().collect());
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn runnable_command(tool: &ToolSpec, options: &RunOptions) -> Result<Vec<String>> {
    match tool.id.as_str() {
        "tool.rg" => {
            let pattern = options
                .args
                .first()
                .context("missing search pattern for rg")?;
            let mut command = vec!["rg".to_owned(), "-n".to_owned(), pattern.clone()];
            command.extend(options.args.iter().skip(1).cloned());
            Ok(command)
        }
        "tool.cargo-test" => {
            let mut command = vec!["cargo".to_owned(), "test".to_owned()];
            if let Some(package) = &options.package {
                command.extend(["-p".to_owned(), package.clone()]);
            }
            command.extend(options.args.iter().cloned());
            Ok(command)
        }
        _ => {
            let mut command = tool
                .default_command
                .iter()
                .map(|token| expand_default_command_token(token))
                .collect::<Result<Vec<_>>>()?;
            command.extend(options.args.iter().cloned());
            if options.content.is_some() {
                command.extend(["--content".to_owned(), "<content>".to_owned()]);
            }
            Ok(command)
        }
    }
}

fn expand_default_command_token(token: &str) -> Result<String> {
    expand_default_command_token_in_root(token, &workspace_namespace_root())
}

fn expand_default_command_token_in_root(token: &str, root: &Path) -> Result<String> {
    let Some(path) = token.strip_prefix("@file:") else {
        return Ok(token.to_owned());
    };
    Ok(resolve_workspace_relative_file_under(path, root)?
        .display()
        .to_string())
}

fn default_run_goal(tool: &ToolSpec, args: &[String]) -> String {
    if args.is_empty() {
        return format!("run {}", tool.name);
    }
    format!("run {} with {}", tool.name, args.join(" "))
}

fn print_lines(label: &str, lines: &[String]) {
    for line in lines {
        println!("{label}> {line}");
    }
}

fn build_chat_request_history(
    history: &[ChatMessage],
    selected_tool_context: Option<String>,
    user_input: &str,
) -> Vec<ChatMessage> {
    let mut request_history = history.to_vec();
    if let Some(selected_tool_context) = selected_tool_context
        && !selected_tool_context.trim().is_empty()
    {
        append_system_context(&mut request_history, &selected_tool_context);
    }
    request_history.push(ChatMessage::new(MessageRole::User, user_input.to_owned()));
    request_history
}

fn merge_optional_contexts<const N: usize>(contexts: [Option<String>; N]) -> Option<String> {
    let merged = contexts
        .into_iter()
        .flatten()
        .filter(|context| !context.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if merged.trim().is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn selection_input_from_history(history: &[ChatMessage], current_input: &str) -> String {
    let _ = history;
    current_input.trim().to_owned()
}

fn append_system_context(messages: &mut Vec<ChatMessage>, context: &str) {
    if let Some(system_message) = messages
        .iter_mut()
        .find(|message| message.role == MessageRole::System)
    {
        if !system_message.content.ends_with("\n\n") {
            system_message.content.push_str("\n\n");
        }
        system_message.content.push_str(context.trim());
        return;
    }

    messages.insert(
        0,
        ChatMessage::new(MessageRole::System, context.trim().to_owned()),
    );
}

fn route_tool_turn(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    user_turn: &str,
    selection: &ToolSelection,
    user_system: Option<&str>,
) -> Result<NaturalLanguageToolRoute> {
    let prompt = render_tool_router_prompt(selection, user_turn, user_system)?;
    let mut request = GenerateRequest::new(
        ModelRef::new(provider.to_owned(), model.to_owned()),
        vec![
            ChatMessage::new(MessageRole::System, prompt),
            ChatMessage::new(MessageRole::User, user_turn.to_owned()),
        ],
    );
    request.temperature = Some(0.0);
    request.max_output_tokens = Some(256);
    let response = registry
        .generate(&request)
        .map_err(|error| anyhow::anyhow!(error))?;
    parse_tool_route_response(&response.message.content)
}

fn parse_tool_route_response(content: &str) -> Result<NaturalLanguageToolRoute> {
    let json_text = extract_json_object(content).context("tool router did not return JSON")?;
    serde_json::from_str(json_text).context("failed to parse tool route JSON")
}

fn apply_tool_route(
    selection: &mut ToolSelection,
    catalog: &ToolCatalog,
    route: NaturalLanguageToolRoute,
) -> Result<()> {
    let _ = route.message.as_deref();
    let Some(tool_id) = route.tool_id.filter(|tool_id| !tool_id.trim().is_empty()) else {
        selection.selected = None;
        return Ok(());
    };
    let normalized = normalize_tool_selector(tool_id.trim());
    if let Some(tool) = catalog.get(&normalized) {
        selection.selected = Some(tool.clone());
        return Ok(());
    }
    bail!("tool router selected unknown tool: {tool_id}")
}

fn render_tool_execution_context(
    plan: &hc_toolchain::ToolExecutionPlan,
    outcome: &ToolExecutionOutcome,
) -> String {
    let observations = outcome
        .observations
        .iter()
        .take(80)
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Internal execution record for this user turn. Use it to answer naturally, but do not reveal tool ids, MCP server names, method names, commands, or raw implementation identifiers to the user.\n- tool_id: {}\n- success: {}\n- summary: {}\n- command: {}\n- planned_tool_id: {}\n- observations:\n{}",
        outcome.tool_id,
        outcome.success,
        outcome.summary,
        outcome.command.join(" "),
        plan.tool_id,
        observations
    )
}

fn render_chat_error(error: &hc_llm::LlmError) -> String {
    let message = error.to_string();
    if message.contains("invalid chat setting") {
        return format!(
            "error> provider rejected the chat request: invalid chat setting. Current session is preserved; continue typing or use /clear and retry.\nprovider> {message}"
        );
    }
    if is_timeout_error(error) {
        return format!(
            "error> provider request timed out. Current session is preserved; retry, switch HC_LLM_PROVIDER/HC_LLM_MODEL, or try again later.\nprovider> {message}"
        );
    }
    format!("error> {message}")
}

fn render_router_warning(error: &anyhow::Error) -> String {
    let message = error.to_string();
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("timed out") || lowered.contains("timeout") {
        return "warning> tool router timed out; continuing with candidate tools only.".to_owned();
    }
    format!("warning> tool router skipped: {message}")
}

fn load_cli_tool_catalog() -> Result<ToolCatalog> {
    let mut catalog = default_tool_catalog();
    if let Ok(custom_catalog) = tool_repository().load_catalog() {
        catalog.register_provider(&custom_catalog);
    }
    if let Ok(servers) = mcp_server_repository().list_servers() {
        for server in servers {
            if !server.enabled {
                continue;
            }
            if let Ok(cache) = mcp_server_repository().read_tool_cache(&server.id) {
                catalog.register_many(cache.tools);
            }
        }
    }
    if let Ok(skill_catalog) = skill_repository().load_catalog() {
        catalog.register_provider(&skill_catalog);
    }
    Ok(catalog)
}

fn tool_repository() -> ToolRepository {
    ToolRepository::with_namespace(workspace_root(), runtime_namespace())
}

fn mcp_server_repository() -> McpServerRepository {
    McpServerRepository::with_namespace(workspace_root(), runtime_namespace())
}

fn skill_repository() -> SkillRepository {
    SkillRepository::with_namespace(workspace_root(), runtime_namespace())
}

fn schedule_repository() -> ScheduleRepository {
    ScheduleRepository::with_namespace(workspace_root(), runtime_namespace())
}

fn runtime_namespace() -> WorkspaceNamespace {
    let context = cli_runtime_context();
    let tenant_id = context
        .tenant_id
        .or_else(|| env::var("HC_TENANT_ID").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local".to_owned());
    let user_id = context
        .user_id
        .or_else(|| env::var("HC_USER_ID").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "default".to_owned());
    WorkspaceNamespace::new(tenant_id, user_id)
}

fn default_chat_session_id(tenant_id: &str, user_id: &str) -> String {
    format!("session.{tenant_id}.{user_id}.default")
}

fn workspace_root() -> PathBuf {
    env::var("HC_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("workspace"))
}

fn workspace_namespace_root() -> PathBuf {
    let namespace = runtime_namespace();
    workspace_root()
        .join("tenants")
        .join(namespace.tenant_id)
        .join("users")
        .join(namespace.user_id)
}

fn print_recalled_memories(memories: &[RetrievedMemory]) {
    if memories.is_empty() {
        println!("memory> no recalled memories");
        return;
    }
    println!("memory> recalled {}", memories.len());
    for memory in memories {
        println!(
            "- {} | {} | {} | {:.2}",
            memory.id,
            memory_scope_label(&memory.scope),
            memory_kind_label(&memory.kind),
            f32::from(memory.confidence_milli) / 1000.0
        );
    }
}

fn parse_usize_arg(value: &str, name: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .with_context(|| format!("invalid value for {name}: {value}"))?;
    if parsed == 0 {
        bail!("{name} must be greater than 0");
    }
    Ok(parsed)
}

fn parse_u64_arg(value: &str, name: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("invalid value for {name}: {value}"))
}

fn parse_schedule_kind(value: &str) -> Result<hc_scheduler::ScheduleKind> {
    match value {
        "once" => Ok(hc_scheduler::ScheduleKind::Once),
        "interval" => Ok(hc_scheduler::ScheduleKind::Interval),
        other => bail!("unsupported schedule kind: {other}"),
    }
}

fn parse_scheduled_target_kind(value: &str) -> Result<ScheduledTargetKind> {
    match value {
        "agent" => Ok(ScheduledTargetKind::Agent),
        "tool" => Ok(ScheduledTargetKind::Tool),
        "mcp" => Ok(ScheduledTargetKind::Mcp),
        "command" => Ok(ScheduledTargetKind::Command),
        "event" => Ok(ScheduledTargetKind::Event),
        other => bail!("unsupported scheduled target kind: {other}"),
    }
}

fn split_chat_command(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '\0';
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if in_quotes {
            if ch == quote_char {
                in_quotes = false;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            in_quotes = true;
            quote_char = ch;
            continue;
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(current.clone());
                current.clear();
            }
            continue;
        }
        current.push(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn synthesize_tool_build_from_natural_language(
    registry: &ProviderRegistry,
    provider: &str,
    model: &str,
    input: &str,
    catalog: &ToolCatalog,
    user_system: Option<&str>,
) -> Result<NaturalLanguageToolBuild> {
    let prompt = render_tool_builder_prompt(catalog, user_system)?;

    let mut request = GenerateRequest::new(
        ModelRef::new(provider.to_owned(), model.to_owned()),
        vec![
            ChatMessage::new(MessageRole::System, prompt),
            ChatMessage::new(MessageRole::User, input.to_owned()),
        ],
    );
    request.temperature = Some(0.0);
    request.max_output_tokens = Some(2048);
    let response = registry
        .generate(&request)
        .map_err(|error| anyhow::anyhow!(error))?;
    match parse_tool_build_response(&response.message.content) {
        Ok(build) => Ok(build),
        Err(error) => {
            if let Some(command) = extract_create_tool_command(&response.message.content) {
                return build_from_create_tool_command(&command);
            }
            Err(error)
        }
    }
}

fn parse_tool_build_response(content: &str) -> Result<NaturalLanguageToolBuild> {
    let json_text = extract_json_object(content).context("LLM did not return a JSON object")?;
    serde_json::from_str(json_text).context("failed to parse tool creation JSON")
}

fn extract_json_object(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if start <= end {
        Some(&trimmed[start..=end])
    } else {
        None
    }
}

fn try_execute_create_tool_command_from_response(content: &str) -> Result<Option<PathBuf>> {
    let Some(command) = extract_create_tool_command(content) else {
        return Ok(None);
    };
    handle_create_from_chat(&command).map(Some)
}

fn build_from_create_tool_command(command: &str) -> Result<NaturalLanguageToolBuild> {
    let args = split_chat_command(command);
    let options = parse_create_options(&args)?;
    Ok(NaturalLanguageToolBuild {
        action: "create_tool".to_owned(),
        message: None,
        tool: Some(NaturalLanguageToolDraft {
            id: options.id,
            name: options.name,
            description: options.description,
            execution_kind: Some(tool_execution_kind_label(&options.execution_kind).to_owned()),
            default_command: options.command,
            files: Vec::new(),
            tags: options.tags,
        }),
        skill: None,
    })
}

fn tool_execution_kind_label(kind: &ToolExecutionKind) -> &'static str {
    match kind {
        ToolExecutionKind::Script => "script",
        ToolExecutionKind::Workflow => "workflow",
        ToolExecutionKind::Cli => "cli",
        ToolExecutionKind::Service => "service",
        ToolExecutionKind::Builtin => "builtin",
    }
}

fn extract_create_tool_command(content: &str) -> Option<String> {
    for line in content.lines() {
        let mut candidate = line.trim();
        candidate = candidate
            .trim_start_matches('`')
            .trim_end_matches('`')
            .trim();
        if let Some(rest) = candidate.strip_prefix("$ ") {
            candidate = rest.trim();
        }
        if let Some(index) = candidate.find("/create-tool ") {
            let command = &candidate[index + "/create-tool ".len()..];
            return Some(command.trim().to_owned());
        }
        if let Some(index) = candidate.find("hc-tool-cli create ") {
            let command = &candidate[index + "hc-tool-cli create ".len()..];
            return Some(command.trim().to_owned());
        }
    }
    None
}

fn sanitize_model_response(content: &str) -> String {
    sanitize_assistant_text(content)
}

fn persist_response_artifacts(user_input: &str, content: &str) -> Result<Vec<PathBuf>> {
    let blocks = extract_code_blocks(content);
    if blocks.is_empty() {
        return Ok(Vec::new());
    }

    let artifact_dir = artifact_output_dir();
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;

    let mut paths = Vec::new();
    let mut index = 0usize;
    for block in blocks {
        let Some(extension) = code_block_extension(&block) else {
            continue;
        };
        if !looks_like_complete_artifact(&block, extension) {
            continue;
        }
        index += 1;
        let file_name = artifact_file_name(user_input, extension, index);
        let path = unique_artifact_path(&artifact_dir, &file_name);
        fs::write(&path, block.content.trim_start_matches('\n'))
            .with_context(|| format!("failed to write {}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

fn extract_code_blocks(content: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let language = info
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .map(|value| value.trim().to_ascii_lowercase());
        let mut body = String::new();
        for body_line in lines.by_ref() {
            if body_line.trim_start().starts_with("```") {
                break;
            }
            body.push_str(body_line);
            body.push('\n');
        }
        blocks.push(CodeBlock {
            language,
            content: body,
        });
    }
    blocks
}

fn code_block_extension(block: &CodeBlock) -> Option<&'static str> {
    match block.language.as_deref().unwrap_or_default() {
        "html" => Some("html"),
        "css" => Some("css"),
        "js" | "javascript" => Some("js"),
        "ts" | "typescript" => Some("ts"),
        "jsx" => Some("jsx"),
        "tsx" => Some("tsx"),
        "json" => Some("json"),
        "md" | "markdown" => Some("md"),
        _ => infer_code_extension(&block.content),
    }
}

fn infer_code_extension(content: &str) -> Option<&'static str> {
    let trimmed = content.trim_start();
    if trimmed.starts_with("<!DOCTYPE html") || trimmed.starts_with("<html") {
        return Some("html");
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some("json");
    }
    None
}

fn looks_like_complete_artifact(block: &CodeBlock, extension: &str) -> bool {
    let content = block.content.trim();
    if content.len() < 40 {
        return false;
    }
    match extension {
        "html" => {
            content.contains("<html")
                || content.contains("<body")
                || content.contains("<!DOCTYPE html")
        }
        "css" => content.contains('{') && content.contains('}'),
        "js" | "ts" | "jsx" | "tsx" => {
            content.contains("function")
                || content.contains("=>")
                || content.contains("import ")
                || content.contains("const ")
                || content.contains("let ")
                || content.contains("class ")
        }
        "json" => content.starts_with('{') || content.starts_with('['),
        "md" => content.lines().any(|line| line.starts_with('#')),
        _ => false,
    }
}

fn artifact_output_dir() -> PathBuf {
    let namespace = runtime_namespace();
    workspace_root()
        .join("tenants")
        .join(namespace.tenant_id)
        .join("users")
        .join(namespace.user_id)
        .join("artifacts")
}

fn artifact_file_name(user_input: &str, extension: &str, index: usize) -> String {
    let slug = artifact_slug(user_input);
    let suffix = if index > 1 {
        format!("-{index}")
    } else {
        String::new()
    };
    format!("{slug}{suffix}.{extension}")
}

fn artifact_slug(input: &str) -> String {
    let lowered = input.to_ascii_lowercase();
    let mut slug = lowered
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "artifact".to_owned();
    }
    slug
}

fn unique_artifact_path(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact");
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("txt");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    dir.join(format!("{stem}.{timestamp}.{extension}"))
}

fn default_registry() -> ProviderRegistry {
    default_registry_from_env()
}

fn render_tool_chat_system_prompt(
    catalog: &ToolCatalog,
    user_system: Option<&str>,
    namespace: &MemoryNamespace,
    session_id: Option<&str>,
) -> Result<String> {
    let user_guidance = render_optional_guidance(user_system);
    let runtime = runtime_variables_for_namespace(namespace, session_id);
    let guidance = merge_optional_contexts([
        Some(
            "User-facing wording rule: never expose internal tool ids, MCP server ids, method names, commands, JSON-RPC details, or implementation identifiers. Describe capabilities in plain user language instead."
                .to_owned(),
        ),
        Some(hc_context::runtime::runtime_identity_prompt(&runtime.identity)),
        (!user_guidance.trim().is_empty()).then_some(user_guidance),
    ]);
    render_prompt_template(
        load_tool_chat_prompt(&runtime_namespace())?,
        &[
            ("available_tools", render_available_tools(catalog)),
            ("selected_tool", String::new()),
            ("additional_system_guidance", guidance.unwrap_or_default()),
        ],
    )
}

fn render_tool_builder_prompt(catalog: &ToolCatalog, user_system: Option<&str>) -> Result<String> {
    render_prompt_template(
        load_tool_natural_language_builder_prompt(&runtime_namespace())?,
        &[
            ("existing_tools", render_existing_tools(catalog)),
            (
                "additional_system_guidance",
                render_optional_guidance(user_system),
            ),
        ],
    )
}

fn render_tool_router_prompt(
    selection: &ToolSelection,
    user_turn: &str,
    user_system: Option<&str>,
) -> Result<String> {
    render_prompt_template(
        load_tool_router_prompt(&runtime_namespace())?,
        &[
            ("tool_candidates", render_tool_route_candidates(selection)),
            ("user_turn", user_turn.to_owned()),
            (
                "additional_system_guidance",
                render_optional_guidance(user_system),
            ),
        ],
    )
}

fn render_prompt_template(template: String, values: &[(&str, String)]) -> Result<String> {
    let mut rendered = template;
    for (key, value) in values {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    Ok(rendered)
}

fn render_tool_route_candidates(selection: &ToolSelection) -> String {
    selection
        .candidates
        .iter()
        .map(|candidate| {
            format!(
                "- id={} | score={} | name={} | kind={:?} | composition={:?} | tags={} | description={} | default_command={}",
                candidate.tool.id,
                candidate.score,
                candidate.tool.name,
                candidate.tool.execution_kind,
                candidate.tool.composition,
                candidate.tool.tags.join(", "),
                compact_single_line(&candidate.tool.description, 600),
                candidate.tool.default_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_available_tools(catalog: &ToolCatalog) -> String {
    catalog
        .list()
        .into_iter()
        .map(|tool| {
            format!(
                "- {} | name={} | kind={:?} | composition={:?} | tags={} | description={} | default_command={}",
                tool.id,
                tool.name,
                tool.execution_kind,
                tool.composition,
                tool.tags.join(", "),
                tool.description,
                tool.default_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_tool_selection_context(selection: &ToolSelection) -> Option<String> {
    if selection.selected.is_none() && selection.candidates.is_empty() {
        return None;
    }

    let mut sections = Vec::new();
    if let Some(tool) = &selection.selected {
        sections.push(format!(
            "Internal selected tool for this user turn. Do not reveal these ids, commands, or method names to the user:\n{}",
            render_tool_context(tool)
        ));
    }

    let candidates = selection
        .candidates
        .iter()
        .map(|candidate| {
            format!(
                "- score={} | {}",
                candidate.score,
                render_tool_context(&candidate.tool).replace('\n', " | ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !candidates.trim().is_empty() {
        sections.push(format!(
            "Internal tool candidates for this user turn. Do not reveal these ids, commands, or method names to the user:\n{candidates}"
        ));
    }

    Some(sections.join("\n\n"))
}

fn render_turn_frame_context(frame: &TurnFrame) -> Option<String> {
    let mut lines = vec![
        "Internal turn frame. Use this as orchestration context; do not reveal ids or runtime fields to the user:"
            .to_owned(),
        format!("tenant_id: {}", frame.runtime.identity.tenant_id),
        format!("user_id: {}", frame.runtime.identity.user_id),
        format!("session_id: {}", frame.runtime.identity.session_id),
        format!("selection_input: {}", frame.selection_input),
    ];
    if let Some(agent_id) = &frame.selected_agent_id {
        lines.push(format!("selected_agent_id: {agent_id}"));
    }
    if let Some(domain_id) = &frame.selected_domain_id {
        lines.push(format!("selected_domain_id: {domain_id}"));
    }
    Some(lines.join("\n"))
}

fn render_tool_context(tool: &ToolSpec) -> String {
    let mut rendered = format!(
        "- id: {}\n- name: {}\n- kind: {:?}\n- composition: {:?}\n- description: {}\n- tags: {}\n- default_command: {}",
        tool.id,
        tool.name,
        tool.execution_kind,
        tool.composition,
        tool.description,
        tool.tags.join(", "),
        tool.default_command.join(" ")
    );
    if let Some(skill) = skill_profile_for_tool(tool)
        && !skill.instructions.trim().is_empty()
    {
        rendered.push_str("\n- skill_instructions: ");
        rendered.push_str(&compact_single_line(skill.instructions.trim(), 1200));
    }
    rendered
}

fn skill_profile_for_tool(tool: &ToolSpec) -> Option<SkillProfile> {
    if tool.composition != ToolComposition::Composite && !tool.tags.iter().any(|tag| tag == "skill")
    {
        return None;
    }
    skill_repository()
        .list_profiles()
        .ok()?
        .into_iter()
        .find(|skill| skill.resolved_tool_id() == tool.id || skill.id == tool.id)
}

fn compact_single_line(value: &str, max_chars: usize) -> String {
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn render_existing_tools(catalog: &ToolCatalog) -> String {
    catalog
        .list()
        .into_iter()
        .map(|tool| {
            format!(
                "- {}: {} command={}",
                tool.id,
                tool.description,
                tool.default_command.join(" ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_optional_guidance(user_system: Option<&str>) -> String {
    user_system
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("Additional system guidance:\n{}", value.trim()))
        .unwrap_or_default()
}

fn prompt_raw(editor: &mut DefaultEditor) -> Result<Option<String>> {
    match editor.readline("you> ") {
        Ok(input) => {
            let input = repair_console_mojibake(&input);
            if !input.trim().is_empty() {
                let _ = editor.add_history_entry(input.as_str());
            }
            Ok(Some(input))
        }
        Err(ReadlineError::Interrupted) => Ok(Some(String::new())),
        Err(ReadlineError::Eof) => Ok(None),
        Err(error) => Err(anyhow::Error::new(error)).context("failed to read interactive input"),
    }
}

fn repair_console_mojibake(input: &str) -> String {
    if !looks_like_utf8_decoded_as_gbk(input) {
        return input.to_owned();
    }
    let (bytes, _, _) = GB18030.encode(input);
    let bytes = match bytes {
        Cow::Borrowed(bytes) => bytes.to_vec(),
        Cow::Owned(bytes) => bytes,
    };
    let Ok(repaired) = String::from_utf8(bytes) else {
        return input.to_owned();
    };
    if repair_score(&repaired) > repair_score(input) {
        repaired
    } else {
        input.to_owned()
    }
}

fn looks_like_utf8_decoded_as_gbk(input: &str) -> bool {
    let suspicious = [
        "\u{6d93}", "\u{9391}", "\u{937a}", "\u{6d60}", "\u{6d94}", "\u{953b}", "\u{9239}",
        "\u{99c3}", "\u{20ac}",
    ];
    suspicious.iter().any(|marker| input.contains(marker))
}

fn repair_score(input: &str) -> i32 {
    let cjk = input
        .chars()
        .filter(|ch| ('\u{4e00}'..='\u{9fff}').contains(ch))
        .count() as i32;
    let suspicious = [
        "\u{6d93}", "\u{9391}", "\u{937a}", "\u{6d60}", "\u{6d94}", "\u{953b}", "\u{9239}",
        "\u{99c3}", "\u{20ac}",
    ]
    .iter()
    .filter(|marker| input.contains(*marker))
    .count() as i32;
    cjk - suspicious * 10
}

fn env_file_path() -> Result<PathBuf> {
    Ok(env::current_dir()
        .context("failed to read current directory")?
        .join(".env"))
}

fn load_local_env_file() -> Result<()> {
    let env_path = env_file_path()?;
    if !env_path.exists() {
        return Ok(());
    }

    for (key, value) in read_env_map(&env_path)? {
        if env::var_os(&key).is_none() {
            unsafe { env::set_var(key, value) };
        }
    }

    Ok(())
}

fn read_env_map(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut vars = BTreeMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        vars.insert(key.trim().to_owned(), value.trim().to_owned());
    }
    Ok(vars)
}

fn default_provider() -> String {
    default_provider_from_env()
}

fn default_model() -> String {
    default_model_from_env()
}

fn is_help(value: &str) -> bool {
    matches!(value, "help" | "--help" | "-h")
}

fn print_help() {
    println!("hc-tool-cli                    # start tool-aware chat");
    println!("global options: --tenant-id <id> --user-id <id> --session-id <id>");
    println!("hc-tool-cli chat [--provider <id>] [--model <name>] [--system <text>]");
    println!(
        "hc-tool-cli create <tool-id> <name> --description <text> --command <token> [--command <token>] [--kind <cli|builtin|script|workflow|service>] [--tag <tag>] [--json]"
    );
    println!("hc-tool-cli list [--json]");
    println!(
        "hc-tool-cli mcp add <server-id> <name> --description <text> --command <token> [--command <token>] [--tag <tag>] [--json]"
    );
    println!("hc-tool-cli mcp list [--json]");
    println!("hc-tool-cli mcp tools [--json]");
    println!("hc-tool-cli mcp call <server-id> <tool-name> [key=value ...] [--json]");
    println!(
        "hc-tool-cli schedule add --id <id> --title <text> --kind <once|interval> --run-at-unix <ts> [--interval-seconds <n>] --target-kind <agent|tool|mcp|command|event> --target-ref <id> [--target-action <name>] [--arg key=value] [--json]"
    );
    println!("hc-tool-cli schedule list [--json]");
    println!("hc-tool-cli schedule run-due [--now-unix <ts>] [--json]");
    println!("hc-tool-cli schedule runs [--json]");
    println!("hc-tool-cli schedule pause --id <id> [--json]");
    println!("hc-tool-cli schedule resume --id <id> [--json]");
    println!("hc-tool-cli schedule dispatch-due [--now-unix <ts>] [--json]");
    println!("hc-tool-cli schedule dispatch-queued [--now-unix <ts>] [--json]");
    println!("hc-tool-cli schedule watch [--tick-seconds <n>] [--max-ticks <n>] [--json]");
    println!("hc-tool-cli index rebuild [--vector] [--dims <n>] [--json]");
    println!(
        "hc-tool-cli index search <text> [--vector] [--rebuild] [--filter key=value] [--limit <n>] [--json]"
    );
    println!("hc-tool-cli show <rg|cargo-test|tool-id> [--json]");
    println!("hc-tool-cli plan <auto|rg|cargo-test|tool-id> <goal...> [--json]");
    println!(
        "hc-tool-cli run <rg|tool.rg> <pattern> [extra rg args...] [--path <dir>] [--goal <text>] [--json]"
    );
    println!(
        "hc-tool-cli run <cargo-test|tool.cargo-test> [filter] [--package <pkg>] [--path <dir>] [--goal <text>] [--json]"
    );
    println!(
        "hc-tool-cli run <tool.local-file.read|tool.local-file.write|tool.local-dir.list> <path> [--content <text>] [--path <dir>] [--json]"
    );
}

#[cfg(test)]
#[path = "../tests/unit/cli.rs"]
mod tests;
