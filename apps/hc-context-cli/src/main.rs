use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Condvar, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hc_context::{
    CompositeMemoryOrganizer, ContextMemoryQuery, ContextRequest, ContextResponse,
    DefaultContextComposer, DefaultPromptAssetSynthesizer, DefaultToolExecutionBinder,
    EvaluationSignal, GeneralizationPolicy, KeywordMemoryTagSuggester, LlmMemoryOrganizer,
    LlmMemoryTagSuggester, LlmPromptAssetSynthesizer, MemoryOrganizationInput, MemoryOrganizer,
    PromotionRule, PromptAssetSynthesizer, PromptPolicy, RetirementRule,
    RuleBasedMemoryKindResolver, RuleBasedMemoryPromotionAdvisor, RuleBasedMemoryRoomRouter,
    ToolCatalog, ToolComposition, ToolExecutionKind, ToolExecutionOutcome, ToolRepository,
    WorkspaceMemoryRetriever, build_tool_execution_plan_from_assets, default_tool_catalog,
    default_workspace_root, evaluate_tool_execution, export_tool_capability_package,
    generate_with_context_stream_using_synthesizer, generate_with_context_using_synthesizer,
    load_assistant_wenyan_prompt, load_context_lightweight_chat_prompt,
    load_context_memory_system_prompt, load_context_memory_usage_policy_prompt, load_tool_assets,
    persist_compiled_tool_assets, persist_retired_tool_assets, persist_revised_tool_assets,
    persist_room_memory, persist_synthesized_prompt_assets, persist_tool_evolution_events,
    room_memory_write_request_from_response, room_memory_write_request_from_tool_outcome,
    room_memory_write_requests_from_tool_evaluation, summarize_global_preference,
    summarize_global_preference_with_llm, workspace_namespace_from_memory_namespace,
};
use hc_llm::{
    ChatMessage, GenerateRequest, MessageRole, ModelRef, ProviderRegistry, StreamChunk,
    default_model_from_env, default_provider_from_env, default_registry_from_env,
};
use hc_log::CliLogger;
use hc_memory::{
    ArtifactDraft, ArtifactEvolutionAction, ArtifactEvolutionEvent, MemoryEntityKind,
    MemoryEntityRef, MemoryKind, MemoryLayer, MemoryNamespace, MemoryOwnerKind, MemoryOwnerRef,
    MemoryRelation, MemoryRelationKind, MemoryRoom, MemoryRoomAsset, MemoryRoomAssetKind,
    MemoryRoomRepository, MemoryScope, MemoryVisibility,
};
use hc_skill::{SkillProfile, SkillRepository};
use hc_store::store::{MarkdownQuery, WorkspaceNamespace, WorkspaceStore};
use hc_trace::{
    TraceContext, TraceScopeGuard, current_trace_context, new_trace_id, replace_trace_context,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestMode {
    Direct,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategyMode {
    Auto,
    Llm,
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromotionTriggerMode {
    Immediate,
    Deferred,
    WindowFull,
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextRoomKind {
    Topic,
    Task,
    Agent,
    Tool,
    Project,
}

enum ContextRoomResolution {
    Created(MemoryRoom),
    Reused(MemoryRoom),
    None,
}

enum BackgroundMemoryTask {
    GlobalPromotion { content: String },
    Literary { turn_index: usize, content: String },
    Shutdown,
}

struct BackgroundMemoryWorker {
    sender: mpsc::Sender<BackgroundMemoryTask>,
    handle: thread::JoinHandle<()>,
}

#[derive(Default)]
struct LlmPriorityState {
    active_foreground: usize,
    active_background: usize,
}

#[derive(Default)]
struct LlmPriorityGate {
    state: Mutex<LlmPriorityState>,
    changed: Condvar,
}

enum LlmPriorityClass {
    Foreground,
    Background,
}

struct LlmPriorityPermit {
    gate: Arc<LlmPriorityGate>,
    class: LlmPriorityClass,
}

#[derive(Debug, Clone, Copy)]
struct OutputStyle {
    typewriter: bool,
    typewriter_delay_ms: u64,
}

static CLI_LOGGER: OnceLock<CliLogger> = OnceLock::new();
const TRACE_COMPONENT: &str = "hc-context-cli";

fn init_cli_trace(namespace: &WorkspaceNamespace) {
    let logger = CliLogger::init_for_workspace_run(
        default_workspace_root(),
        namespace,
        "hc-context-cli",
        TRACE_COMPONENT,
    );
    let _ = CLI_LOGGER.set(logger);
}

fn current_run_id() -> Option<String> {
    cli_logger().current_run_id()
}

fn enter_flow_context(flow_id: impl Into<String>) -> TraceScopeGuard {
    cli_logger().enter_flow_context(flow_id)
}

fn emit_cli_trace(stage: &str, action: &str, status: Option<&str>, message: impl Into<String>) {
    cli_logger().emit(stage, action, status, message);
}

fn emit_cli_trace_with_fields(
    stage: &str,
    action: &str,
    status: Option<&str>,
    message: impl Into<String>,
    fields: BTreeMap<String, String>,
) {
    cli_logger().emit_with_fields(stage, action, status, message, fields);
}

fn print_organize_status(stage: &str, detail: impl AsRef<str>) {
    cli_logger().print_status(stage, detail);
}

fn eprint_organize_status(stage: &str, detail: impl AsRef<str>) {
    cli_logger().eprint_status(stage, detail);
}

fn set_active_prompt(prompt: &str) {
    cli_logger().set_active_prompt(prompt);
}

fn clear_active_prompt() {
    cli_logger().clear_active_prompt();
}

fn cli_logger() -> &'static CliLogger {
    CLI_LOGGER
        .get()
        .expect("cli trace logger should be initialized before use")
}

impl LlmPriorityGate {
    fn acquire_foreground(self: &Arc<Self>) -> LlmPriorityPermit {
        let mut state = self.state.lock().expect("llm priority state should lock");
        // Foreground work should not wait for an already-running background task.
        // Background admission is still gated on foreground activity below, so this
        // keeps interactive replies responsive without letting background jobs pile up.
        state.active_foreground += 1;
        if priority_debug_enabled() {
            eprint_organize_status(
                "priority",
                format!(
                    "class=foreground action=acquired active_foreground={} active_background={}",
                    state.active_foreground, state.active_background
                ),
            );
        }
        LlmPriorityPermit {
            gate: Arc::clone(self),
            class: LlmPriorityClass::Foreground,
        }
    }

    fn acquire_background(self: &Arc<Self>) -> LlmPriorityPermit {
        let mut state = self.state.lock().expect("llm priority state should lock");
        while state.active_foreground > 0 || state.active_background > 0 {
            if priority_debug_enabled() {
                eprint_organize_status(
                    "priority",
                    format!(
                        "class=background action=waiting active_foreground={} active_background={}",
                        state.active_foreground, state.active_background
                    ),
                );
            }
            state = self
                .changed
                .wait(state)
                .expect("llm priority state should wait");
        }
        state.active_background += 1;
        if priority_debug_enabled() {
            eprint_organize_status(
                "priority",
                format!(
                    "class=background action=acquired active_foreground={} active_background={}",
                    state.active_foreground, state.active_background
                ),
            );
        }
        LlmPriorityPermit {
            gate: Arc::clone(self),
            class: LlmPriorityClass::Background,
        }
    }
}

impl Drop for LlmPriorityPermit {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("llm priority state should lock");
        match self.class {
            LlmPriorityClass::Foreground => {
                state.active_foreground = state.active_foreground.saturating_sub(1);
            }
            LlmPriorityClass::Background => {
                state.active_background = state.active_background.saturating_sub(1);
            }
        }
        if priority_debug_enabled() {
            let class = match self.class {
                LlmPriorityClass::Foreground => "foreground",
                LlmPriorityClass::Background => "background",
            };
            eprint_organize_status(
                "priority",
                format!(
                    "class={class} action=released active_foreground={} active_background={}",
                    state.active_foreground, state.active_background
                ),
            );
        }
        self.gate.changed.notify_all();
    }
}

fn main() -> Result<()> {
    load_local_env_file()?;
    let trace_namespace = workspace_namespace_from_memory_namespace(&runtime_memory_namespace());
    init_cli_trace(&trace_namespace);
    let args: Vec<String> = env::args().skip(1).collect();
    let registry = default_registry();
    emit_cli_trace(
        "runtime",
        "start",
        Some("started"),
        format!(
            "starting hc-context-cli command {}",
            args.first().cloned().unwrap_or_else(|| "chat".to_owned())
        ),
    );

    if args.is_empty() {
        return handle_chat(&registry, &[]);
    }

    match args.first().map(String::as_str) {
        Some("generate") => handle_generate(&registry, &args[1..]),
        Some("tool-plan") => handle_tool_plan(&registry, &args[1..]),
        Some("tool-run") => handle_tool_run(&args[1..]),
        Some("tool-export") => handle_tool_export(&args[1..]),
        Some("tool-seed") => handle_tool_seed(&args[1..]),
        Some("skill") => handle_skill(&args[1..]),
        Some("chat") => handle_chat(&registry, &args[1..]),
        Some("help") | Some("--help") | Some("-h") => print_help(),
        Some(other) => bail!("unknown command: {other}"),
        None => unreachable!("args emptiness handled above"),
    }
}

fn handle_generate(registry: &ProviderRegistry, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "usage: hc-context-cli generate <prompt> [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--show-memory] [--json] [--prompt-asset-mode <auto|llm|rule>] [--write-room-id <id> --write-room-layer <layer>]"
        );
    }
    let generate_flow = new_trace_id("flow.generate");
    let _flow_guard = enter_flow_context(generate_flow.clone());
    emit_cli_trace(
        "generate",
        "start",
        Some("started"),
        "starting generate command",
    );

    let mut provider = default_provider();
    let mut model = default_model();
    let mut system_message = env::var("HC_LLM_SYSTEM").ok();
    let mut prompt_parts = Vec::new();
    let mut request_mode = default_request_mode();
    let mut output_style = OutputStyle {
        typewriter: false,
        typewriter_delay_ms: default_typewriter_delay_ms(),
    };
    let mut show_memory = false;
    let mut json = false;
    let mut prompt_asset_mode = default_prompt_asset_mode();
    let mut memory_query = ContextMemoryQuery::default().for_namespace(runtime_memory_namespace());
    let mut write_room_id: Option<String> = None;
    let mut write_room_layer: Option<MemoryLayer> = None;
    let mut write_title: Option<String> = None;
    let mut write_memory_kind: Option<MemoryKind> = None;
    let mut write_visibility: MemoryVisibility = MemoryVisibility::Private;
    let mut write_owner_kind: Option<MemoryOwnerKind> = None;
    let mut write_owner_id: Option<String> = None;
    let mut write_tags = Vec::new();
    let mut write_file_name: Option<String> = None;

    let mut owner_kind: Option<MemoryOwnerKind> = None;
    let mut owner_id: Option<String> = None;

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
            "--scope" => {
                memory_query = memory_query.with_scope(parse_memory_scope(
                    args.get(index + 1).context("missing value for --scope")?,
                )?);
                index += 2;
            }
            "--owner-kind" => {
                owner_kind = Some(parse_memory_owner_kind(
                    args.get(index + 1)
                        .context("missing value for --owner-kind")?,
                )?);
                index += 2;
            }
            "--owner-id" => {
                owner_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --owner-id")?,
                );
                index += 2;
            }
            "--memory-kind" => {
                memory_query.memory_query.kind = Some(parse_memory_kind(
                    args.get(index + 1)
                        .context("missing value for --memory-kind")?,
                )?);
                index += 2;
            }
            "--tag" => {
                memory_query = memory_query.with_tag(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--memory-limit" => {
                memory_query = memory_query.with_limit(
                    args.get(index + 1)
                        .context("missing value for --memory-limit")?
                        .parse::<usize>()
                        .context("invalid value for --memory-limit")?,
                );
                index += 2;
            }
            "--write-room-id" => {
                write_room_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --write-room-id")?,
                );
                index += 2;
            }
            "--write-room-layer" => {
                write_room_layer = Some(parse_memory_layer(
                    args.get(index + 1)
                        .context("missing value for --write-room-layer")?,
                )?);
                index += 2;
            }
            "--write-title" => {
                write_title = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --write-title")?,
                );
                index += 2;
            }
            "--write-memory-kind" => {
                write_memory_kind = Some(parse_memory_kind(
                    args.get(index + 1)
                        .context("missing value for --write-memory-kind")?,
                )?);
                index += 2;
            }
            "--write-visibility" => {
                write_visibility = parse_memory_visibility(
                    args.get(index + 1)
                        .context("missing value for --write-visibility")?,
                )?;
                index += 2;
            }
            "--write-owner-kind" => {
                write_owner_kind = Some(parse_memory_owner_kind(
                    args.get(index + 1)
                        .context("missing value for --write-owner-kind")?,
                )?);
                index += 2;
            }
            "--write-owner-id" => {
                write_owner_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --write-owner-id")?,
                );
                index += 2;
            }
            "--write-tag" => {
                write_tags.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --write-tag")?,
                );
                index += 2;
            }
            "--write-file-name" => {
                write_file_name = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --write-file-name")?,
                );
                index += 2;
            }
            "--request-mode" => {
                request_mode = parse_request_mode(
                    args.get(index + 1)
                        .context("missing value for --request-mode")?,
                )?;
                index += 2;
            }
            "--stream" => {
                request_mode = RequestMode::Stream;
                index += 1;
            }
            "--direct" => {
                request_mode = RequestMode::Direct;
                index += 1;
            }
            "--typewriter" => {
                output_style.typewriter = true;
                index += 1;
            }
            "--typewriter-delay-ms" => {
                output_style.typewriter_delay_ms = args
                    .get(index + 1)
                    .context("missing value for --typewriter-delay-ms")?
                    .parse::<u64>()
                    .context("invalid value for --typewriter-delay-ms")?;
                index += 2;
            }
            "--show-memory" => {
                show_memory = true;
                index += 1;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--prompt-asset-mode" => {
                prompt_asset_mode = parse_strategy_mode(
                    args.get(index + 1)
                        .context("missing value for --prompt-asset-mode")?,
                )?;
                index += 2;
            }
            value => {
                prompt_parts.push(value.to_owned());
                index += 1;
            }
        }
    }

    if let (Some(owner_kind), Some(owner_id)) = (owner_kind.clone(), owner_id.clone()) {
        memory_query.memory_query.owner = Some(MemoryOwnerRef::new(owner_kind, owner_id));
    } else if owner_kind.is_some() || owner_id.is_some() {
        bail!("--owner-kind and --owner-id must be used together");
    }

    if prompt_parts.is_empty() {
        bail!("missing prompt");
    }

    if write_room_id.is_some() ^ write_room_layer.is_some() {
        bail!("--write-room-id and --write-room-layer must be used together");
    }
    if write_owner_kind.is_some() ^ write_owner_id.is_some() {
        bail!("--write-owner-kind and --write-owner-id must be used together");
    }

    let generation = GenerateRequest::new(
        ModelRef::new(provider, model),
        vec![ChatMessage::new(MessageRole::User, prompt_parts.join(" "))],
    );
    let prompt_text = prompt_parts.join(" ");
    let effective_memory_query = if memory_query.memory_query.text.is_some() {
        memory_query
    } else {
        memory_query.with_text(prompt_text.clone())
    };
    let context_namespace = workspace_namespace_from_memory_namespace(
        &effective_memory_query
            .memory_query
            .namespace
            .clone()
            .unwrap_or_else(runtime_memory_namespace),
    );
    let system_prompt = match system_message {
        Some(system_message) => system_message,
        None => load_context_memory_system_prompt(&context_namespace)?,
    };
    let request = ContextRequest::new(generation)
        .with_memory_query(effective_memory_query.clone())
        .with_system_prompt(system_prompt)
        .with_prompt_policy(PromptPolicy::new(
            "Memory Usage Policy",
            load_context_memory_usage_policy_prompt(&context_namespace)?,
        ));

    let memory_namespace = runtime_memory_namespace();
    let retriever = WorkspaceMemoryRetriever::new(
        default_workspace_root(),
        workspace_namespace_from_memory_namespace(&memory_namespace),
    );
    match ensure_context_room_for_input(
        default_workspace_root(),
        &workspace_namespace_from_memory_namespace(&memory_namespace),
        &memory_namespace,
        &retriever,
        &effective_memory_query,
        &prompt_text,
        None,
    )? {
        ContextRoomResolution::Created(room) => {
            println!(
                "room> created {} room: {}",
                format!("{:?}", room.layer).to_ascii_lowercase(),
                room.id
            );
        }
        ContextRoomResolution::Reused(room) => {
            println!(
                "room> reused {} room: {}",
                format!("{:?}", room.layer).to_ascii_lowercase(),
                room.id
            );
        }
        ContextRoomResolution::None => {}
    }
    let composer = DefaultContextComposer;
    let prompt_asset_model = ModelRef::new(
        request.generation.model.provider.clone(),
        request.generation.model.model.clone(),
    );
    let prompt_asset_synthesizer = build_prompt_asset_synthesizer(
        registry,
        &prompt_asset_model,
        workspace_namespace_from_memory_namespace(&memory_namespace),
        prompt_asset_mode,
    );

    let response = match request_mode {
        RequestMode::Direct => {
            let response = generate_with_context_using_synthesizer(
                registry,
                &retriever,
                &composer,
                prompt_asset_synthesizer.as_ref(),
                &request,
            )?;
            if !json {
                render_output(&response.response.message.content, output_style)?;
                println!();
            }
            response
        }
        RequestMode::Stream => {
            let mut callback = |chunk: StreamChunk| -> Result<(), hc_llm::LlmError> {
                if !json {
                    render_output(&chunk.delta, output_style)
                        .map_err(|error| hc_llm::LlmError::ProviderFailure(error.to_string()))?;
                }
                Ok(())
            };
            let response = generate_with_context_stream_using_synthesizer(
                registry,
                &retriever,
                &composer,
                prompt_asset_synthesizer.as_ref(),
                &request,
                &mut callback,
            )?;
            if !json {
                println!();
            }
            response
        }
    };

    let persisted_path = if let (Some(room_id), Some(room_layer)) =
        (write_room_id, write_room_layer)
    {
        let write_title = write_title.unwrap_or_else(|| summarize_title_from_prompt(&prompt_parts));
        let write_memory_kind = write_memory_kind.unwrap_or(MemoryKind::Summary);
        let mut write_request = room_memory_write_request_from_response(
            room_id,
            room_layer,
            write_title,
            write_memory_kind,
            &response,
        )
        .with_visibility(write_visibility);
        if let (Some(owner_kind), Some(owner_id)) = (write_owner_kind, write_owner_id) {
            write_request = write_request.with_owner(MemoryOwnerRef::new(owner_kind, owner_id));
        }
        if let Some(file_name) = write_file_name {
            write_request = write_request.with_file_name(file_name);
        }
        for tag in write_tags {
            write_request = write_request.with_tag(tag);
        }

        Some(persist_room_memory(
            default_workspace_root(),
            workspace_namespace_from_memory_namespace(&memory_namespace),
            &write_request,
        )?)
    } else {
        None
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).context("failed to serialize response")?
        );
    } else if show_memory {
        let room_candidates = retriever.discover_room_candidates(&effective_memory_query)?;
        print_room_candidates_for_generate(&room_candidates);
        print_recalled_memories_for_generate(&response.recalled_memories);
    }

    if !json && let Some(path) = &persisted_path {
        println!("persisted room memory: {}", path.display());
    }

    emit_cli_trace_with_fields(
        "generate",
        "finish",
        Some("completed"),
        "generate command completed",
        BTreeMap::from([
            ("flow_id".to_owned(), generate_flow),
            (
                "recalled_count".to_owned(),
                response.recalled_memories.len().to_string(),
            ),
            (
                "persisted".to_owned(),
                persisted_path
                    .as_ref()
                    .map(|_| "true".to_owned())
                    .unwrap_or_else(|| "false".to_owned()),
            ),
        ]),
    );

    Ok(())
}

fn default_registry() -> ProviderRegistry {
    default_registry_from_env()
}

fn print_help() -> Result<()> {
    println!("hc-context-cli");
    println!("hc-context-cli                    # start chat");
    println!(
        "hc-context-cli chat [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--no-typewriter] [--typewriter-delay-ms <n>] [--show-memory] [--chat-memory] [--no-chat-memory] [--literary-memory] [--no-literary-memory] [--chat-room-id <id>] [--organizer-mode <auto|llm|rule>] [--prompt-asset-mode <auto|llm|rule>] [--preference-summary-mode <auto|llm|rule>] [--promotion-trigger <immediate|deferred|window_full|background>] [--promotion-window-size <n>] [--literary-trigger <immediate|deferred|window_full|background>] [--literary-window-size <n>] [--chat-room-window-size <n>]"
    );
    println!(
        "hc-context-cli generate <prompt> [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--typewriter-delay-ms <n>] [--show-memory] [--json] [--prompt-asset-mode <auto|llm|rule>] [--write-room-id <id> --write-room-layer <layer>]"
    );
    println!("hc-context-cli tool-plan <auto|rg|cargo-test|tool-id|skill-id> <goal...> [--json]");
    println!(
        "hc-context-cli tool-run <rg|tool.rg|skill.workspace.search> <pattern> [--goal <text>] [--path <path>] [--json] [--persist-outcome] [--persist-evaluation] [--persist-promotions] [--persist-revisions] [--persist-retirements]"
    );
    println!(
        "hc-context-cli tool-run <cargo-test|tool.cargo-test|skill.rust.test> [<filter>] [--goal <text>] [--package <pkg>] [--json] [--persist-outcome] [--persist-evaluation] [--persist-promotions] [--persist-revisions] [--persist-retirements]"
    );
    println!("hc-context-cli tool-export <rg|cargo-test|tool-id|skill-id> [--out <dir>] [--json]");
    println!("hc-context-cli tool-seed <rg|cargo-test>");
    println!("hc-context-cli skill list [--json]");
    println!(
        "hc-context-cli skill create <skill-id> <name> --description <text> --instructions <text> [--tool-id <id>] [--tool-ref <id>] [--kind <cli|builtin|script|workflow|service>] [--command <token>] [--tag <tag>]"
    );
    println!("hc-context-cli skill seed <rg|cargo-test>");
    println!("hc-context-cli skill show <skill-id> [--json]");
    Ok(())
}

fn handle_skill(args: &[String]) -> Result<()> {
    match args {
        [action] if action == "list" => handle_skill_list(&[]),
        [action, rest @ ..] if action == "list" => handle_skill_list(rest),
        [action, skill_id, skill_name] if action == "create" => {
            handle_skill_create(skill_id, skill_name, &[])
        }
        [action, skill_id, skill_name, rest @ ..] if action == "create" => {
            handle_skill_create(skill_id, skill_name, rest)
        }
        [action, seed_name] if action == "seed" => handle_skill_seed(seed_name),
        [action, skill_id] if action == "show" => handle_skill_show(skill_id, &[]),
        [action, skill_id, rest @ ..] if action == "show" => handle_skill_show(skill_id, rest),
        _ => bail!(
            "usage: hc-context-cli skill list [--json] | hc-context-cli skill show <skill-id> [--json] | hc-context-cli skill seed <rg|cargo-test> | hc-context-cli skill create <skill-id> <name> --description <text> --instructions <text> [--tool-id <id>] [--tool-ref <id>] [--kind <cli|builtin|script|workflow|service>] [--command <token>] [--tag <tag>]"
        ),
    }
}

fn handle_skill_list(args: &[String]) -> Result<()> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => bail!("unexpected argument for skill list: {other}"),
        }
    }

    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let repository = SkillRepository::with_namespace(default_workspace_root(), workspace_namespace);
    let skills = repository.list_profiles()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&skills).context("failed to serialize skills")?
        );
    } else if skills.is_empty() {
        println!("skill> none");
    } else {
        for skill in skills {
            let command = if skill.default_command.is_empty() {
                "-".to_owned()
            } else {
                skill.default_command.join(" ")
            };
            println!(
                "skill> id={} tool={} kind={:?} command={}",
                skill.id,
                skill.resolved_tool_id(),
                skill.execution_kind,
                command
            );
            if !skill.description.is_empty() {
                println!("summary> {}", skill.description);
            }
        }
    }

    Ok(())
}

fn handle_skill_show(skill_id: &str, args: &[String]) -> Result<()> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => bail!("unexpected argument for skill show: {other}"),
        }
    }

    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let repository = SkillRepository::with_namespace(default_workspace_root(), workspace_namespace);
    let relative_path = format!("skills/{skill_id}.md");
    let skill = repository
        .read_profile(&relative_path)
        .with_context(|| format!("failed to load skill {skill_id}"))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&skill).context("failed to serialize skill")?
        );
    } else {
        println!("skill> {}", skill.id);
        println!("name> {}", skill.name);
        println!("tool> {}", skill.resolved_tool_id());
        println!(
            "command> {}",
            if skill.default_command.is_empty() {
                "-".to_owned()
            } else {
                skill.default_command.join(" ")
            }
        );
        if !skill.tool_refs.is_empty() {
            println!("refs> {}", skill.tool_refs.join(", "));
        }
        if !skill.tags.is_empty() {
            println!("tags> {}", skill.tags.join(", "));
        }
        if !skill.description.is_empty() {
            println!("summary> {}", skill.description);
        }
        if !skill.instructions.is_empty() {
            println!("instructions>");
            for line in skill.instructions.lines() {
                println!("{line}");
            }
        }
    }

    Ok(())
}

fn handle_skill_create(skill_id: &str, skill_name: &str, args: &[String]) -> Result<()> {
    let mut description: Option<String> = None;
    let mut instructions: Option<String> = None;
    let mut tool_id: Option<String> = None;
    let mut tool_refs = Vec::new();
    let mut tags = Vec::new();
    let mut default_command = Vec::new();
    let mut execution_kind = ToolExecutionKind::Builtin;

    let mut index = 0usize;
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
            "--instructions" => {
                instructions = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --instructions")?,
                );
                index += 2;
            }
            "--tool-id" => {
                tool_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tool-id")?,
                );
                index += 2;
            }
            "--tool-ref" => {
                tool_refs.push(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tool-ref")?,
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
                default_command.push(
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
            other => bail!("unexpected argument for skill create: {other}"),
        }
    }

    let description = description.context("skill create requires --description <text>")?;
    let instructions = instructions.context("skill create requires --instructions <text>")?;

    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let repository = SkillRepository::with_namespace(default_workspace_root(), workspace_namespace);

    let mut skill = SkillProfile::new(skill_id, skill_name)
        .with_description(description)
        .with_instructions(instructions)
        .with_execution_kind(execution_kind);

    if let Some(tool_id) = tool_id {
        skill = skill.with_tool_id(tool_id);
    }
    if !default_command.is_empty() {
        skill = skill.with_default_command(default_command);
    }
    for tool_ref in tool_refs {
        skill = skill.with_tool_ref(tool_ref);
    }
    for tag in tags {
        skill = skill.with_tag(tag);
    }

    let path = repository.write_profile(&skill)?;
    println!("skill> created {}", skill.id);
    println!("tool> {}", skill.resolved_tool_id());
    println!("persisted skill: {}", path.display());
    Ok(())
}

fn handle_skill_seed(seed_name: &str) -> Result<()> {
    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let repository = SkillRepository::with_namespace(default_workspace_root(), workspace_namespace);
    let skill = match seed_name {
        "rg" => seed_rg_skill(),
        "cargo-test" => seed_cargo_test_skill(),
        other => bail!("unsupported skill seed: {other}"),
    };
    let path = repository.write_profile(&skill)?;
    println!("skill> seeded {} as {}", seed_name, skill.id);
    println!("persisted skill: {}", path.display());
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

fn seed_rg_skill() -> SkillProfile {
    SkillProfile::new("skill.workspace.search", "Workspace Search")
        .with_description("Search workspace files with ripgrep before answering codebase questions.")
        .with_instructions(
            "Use rg first to locate candidate files or symbols. Prefer --files when the goal is to find the right file, and use -n searches when the goal is to inspect matching content.",
        )
        .with_tool_id("tool.rg")
        .with_execution_kind(ToolExecutionKind::Cli)
        .with_default_command(["rg", "-n"])
        .with_tool_ref("tool.rg")
        .with_tag("search")
        .with_tag("workspace")
}

fn seed_cargo_test_skill() -> SkillProfile {
    SkillProfile::new("skill.rust.test", "Rust Test Runner")
        .with_description("Run focused Rust test targets to validate changes before broader sweeps.")
        .with_instructions(
            "Prefer a narrow cargo test invocation first, using a filter or package when possible. Escalate to a broader test sweep only after the focused check passes or if scope is unclear.",
        )
        .with_tool_id("tool.cargo-test")
        .with_execution_kind(ToolExecutionKind::Cli)
        .with_default_command(["cargo", "test"])
        .with_tool_ref("tool.cargo-test")
        .with_tag("rust")
        .with_tag("testing")
}

fn load_cli_tool_catalog() -> Result<ToolCatalog> {
    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let skill_repository =
        SkillRepository::with_namespace(default_workspace_root(), workspace_namespace.clone());
    let tool_repository =
        ToolRepository::with_namespace(default_workspace_root(), workspace_namespace);
    let mut catalog = default_tool_catalog();
    if let Ok(custom_catalog) = tool_repository.load_catalog() {
        catalog.register_provider(&custom_catalog);
    }
    if let Ok(skill_catalog) = skill_repository.load_catalog() {
        catalog.register_provider(&skill_catalog);
    }
    Ok(catalog)
}

struct ResolvedToolTarget {
    tool: hc_context::ToolSpec,
    delegated_tool: Option<hc_context::ToolSpec>,
    skill: Option<SkillProfile>,
}

fn resolve_tool_selector(selector: &str) -> Result<ResolvedToolTarget> {
    let normalized = match selector {
        "rg" => "tool.rg",
        "cargo-test" => "tool.cargo-test",
        other => other,
    };

    if normalized.starts_with("skill.") {
        let memory_namespace = runtime_memory_namespace();
        let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
        let repository =
            SkillRepository::with_namespace(default_workspace_root(), workspace_namespace);
        let relative_path = format!("skills/{normalized}.md");
        let skill = repository
            .read_profile(&relative_path)
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
    if let Some(tool) = catalog.get(normalized) {
        if tool.composition == ToolComposition::Composite {
            let memory_namespace = runtime_memory_namespace();
            let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
            let repository =
                SkillRepository::with_namespace(default_workspace_root(), workspace_namespace);
            if let Some(skill) = repository
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

fn auto_select_tool(
    goal: &str,
) -> Result<(hc_context::ToolSpec, Vec<(hc_context::ToolSpec, i32)>)> {
    let catalog = load_cli_tool_catalog()?;
    let mut scored = catalog
        .list()
        .into_iter()
        .cloned()
        .map(|tool| {
            let score = score_tool_for_goal(&tool, goal);
            (tool, score)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.id.cmp(&right.0.id))
    });
    let selected = scored
        .first()
        .cloned()
        .context("no tools or skills are registered")?;
    Ok((selected.0, scored))
}

fn score_tool_for_goal(tool: &hc_context::ToolSpec, goal: &str) -> i32 {
    let lowered_goal = goal.to_ascii_lowercase();
    let mut score = 0i32;

    for token in lowered_goal.split(|character: char| !character.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        if tool.id.to_ascii_lowercase().contains(token) {
            score += 5;
        }
        if tool.name.to_ascii_lowercase().contains(token) {
            score += 6;
        }
        if tool.description.to_ascii_lowercase().contains(token) {
            score += 3;
        }
        if tool
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(token))
        {
            score += 8;
        }
    }

    if goal_contains_any(
        &lowered_goal,
        &["search", "find", "grep", "file", "path", "symbol"],
    ) {
        if tool.id == "tool.rg" {
            score += 20;
        }
        if tool
            .tags
            .iter()
            .any(|tag| tag == "search" || tag == "workspace")
        {
            score += 10;
        }
    }

    if goal_contains_any(
        &lowered_goal,
        &["test", "testing", "cargo", "assert", "spec"],
    ) {
        if tool.id == "tool.cargo-test" {
            score += 20;
        }
        if tool
            .tags
            .iter()
            .any(|tag| tag == "testing" || tag == "rust")
        {
            score += 10;
        }
    }

    if tool.tags.iter().any(|tag| tag == "skill") {
        score += 1;
    }

    score
}

fn goal_contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn handle_tool_plan(_registry: &ProviderRegistry, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "usage: hc-context-cli tool-plan <auto|rg|cargo-test|tool-id|skill-id> <goal...> [--json]"
        );
    }

    let tool_name = args.first().cloned().unwrap_or_default();
    let mut json = false;
    let mut goal_parts = Vec::new();

    for arg in &args[1..] {
        if arg == "--json" {
            json = true;
        } else {
            goal_parts.push(arg.clone());
        }
    }

    if goal_parts.is_empty() {
        bail!("missing goal for tool-plan");
    }

    let goal = goal_parts.join(" ");
    let (tool, candidates) = if tool_name == "auto" {
        auto_select_tool(&goal)?
    } else {
        (resolve_tool_selector(&tool_name)?.tool, Vec::new())
    };

    let memory_namespace = runtime_memory_namespace();
    let retriever = WorkspaceMemoryRetriever::new(
        default_workspace_root(),
        workspace_namespace_from_memory_namespace(&memory_namespace),
    );
    let resolved = if tool_name == "auto" {
        ResolvedToolTarget {
            tool: tool.clone(),
            delegated_tool: None,
            skill: None,
        }
    } else {
        resolve_tool_selector(&tool_name)?
    };
    let planning_tool = resolved.delegated_tool.as_ref().unwrap_or(&resolved.tool);
    let assets = load_tool_assets(&retriever, memory_namespace, planning_tool)?;
    let mut plan = build_tool_execution_plan_from_assets(
        &DefaultToolExecutionBinder,
        goal,
        planning_tool,
        &assets,
    )?;
    plan.tool_id = resolved.tool.id.clone();
    if let Some(skill) = &resolved.skill
        && !skill.instructions.trim().is_empty()
    {
        plan.guidance.insert(0, skill.instructions.clone());
    }

    if json {
        let payload = if tool_name == "auto" {
            serde_json::json!({
                "selected_tool": tool,
                "plan": plan,
                "candidates": candidates.into_iter().map(|(tool, score)| {
                    serde_json::json!({
                        "tool_id": tool.id,
                        "name": tool.name,
                        "score": score,
                    })
                }).collect::<Vec<_>>(),
            })
        } else {
            serde_json::json!(plan)
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).context("failed to serialize tool plan")?
        );
    } else {
        if tool_name == "auto" {
            println!("selector> auto");
            println!("selected> {}", tool.id);
            for (candidate, score) in candidates.iter().take(5) {
                println!(
                    "candidate> {} score={} composition={:?}",
                    candidate.id, score, candidate.composition
                );
            }
        }
        println!("tool> {}", plan.tool_id);
        println!("command> {}", plan.suggested_command.join(" "));
        for line in &plan.guidance {
            println!("guidance> {line}");
        }
        for line in &plan.validation_steps {
            println!("validation> {line}");
        }
        for line in &plan.recovery_steps {
            println!("recovery> {line}");
        }
    }

    Ok(())
}

fn handle_tool_seed(args: &[String]) -> Result<()> {
    let tool_name = args
        .first()
        .context("usage: hc-context-cli tool-seed <rg|cargo-test>")?;
    match tool_name.as_str() {
        "rg" => seed_rg_tool_assets(),
        "cargo-test" => seed_cargo_test_tool_assets(),
        other => bail!("unsupported tool for tool-seed: {other}"),
    }
}

fn handle_tool_export(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!(
            "usage: hc-context-cli tool-export <rg|cargo-test|tool-id|skill-id> [--out <dir>] [--json]"
        );
    }

    let selector = args.first().cloned().unwrap_or_default();
    let mut output_dir: Option<PathBuf> = None;
    let mut json = false;
    let mut index = 1usize;
    while index < args.len() {
        match args[index].as_str() {
            "--out" => {
                output_dir = Some(PathBuf::from(
                    args.get(index + 1).context("missing value for --out")?,
                ));
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            other => bail!("unexpected argument for tool-export: {other}"),
        }
    }

    let resolved = resolve_tool_selector(&selector)?;
    let export_tool = resolved
        .delegated_tool
        .as_ref()
        .unwrap_or(&resolved.tool)
        .clone();
    let export_slug = export_tool.id.trim_start_matches("tool.");
    let output_dir = output_dir.unwrap_or_else(|| {
        default_workspace_root()
            .join("exports")
            .join("capabilities")
            .join(export_slug)
    });
    let memory_namespace = runtime_memory_namespace();
    let retriever = WorkspaceMemoryRetriever::new(
        default_workspace_root(),
        workspace_namespace_from_memory_namespace(&memory_namespace),
    );
    let assets = load_tool_assets(&retriever, memory_namespace, &export_tool)?;
    let package = export_tool_capability_package(&output_dir, &export_tool, &assets)?;

    let skill_path = if let Some(skill) = &resolved.skill {
        let path = output_dir.join("portable").join("skill.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(skill).context("failed to serialize skill")?,
        )?;
        Some(path)
    } else {
        None
    };

    if json {
        let payload = serde_json::json!({
            "selector": selector,
            "exported_tool_id": export_tool.id,
            "package_dir": output_dir.display().to_string(),
            "asset_count": package.manifest.assets.len(),
            "portable_manifest_path": output_dir.join("portable").join("manifest.json").display().to_string(),
            "runnable_entrypoint": output_dir.join("runnable").join("run.sh").display().to_string(),
            "skill_path": skill_path.as_ref().map(|path| path.display().to_string()),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).context("failed to serialize tool export")?
        );
    } else {
        println!("export> {}", output_dir.display());
        println!("tool> {}", package.manifest.tool.id);
        println!("assets> {}", package.manifest.assets.len());
        println!(
            "portable> {}",
            output_dir.join("portable").join("manifest.json").display()
        );
        println!(
            "runnable> {}",
            output_dir.join("runnable").join("run.sh").display()
        );
        println!("readme> {}", output_dir.join("README.md").display());
        if let Some(path) = skill_path {
            println!("skill> {}", path.display());
        }
        for asset in &package.manifest.assets {
            println!(
                "asset> {} {}",
                asset.role,
                output_dir.join("portable").join(&asset.file).display()
            );
        }
    }

    Ok(())
}

fn handle_tool_run(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: hc-context-cli tool-run <rg|cargo-test|tool-id|skill-id> ...");
    }

    let tool_name = args.first().cloned().unwrap_or_default();
    let mut pattern: Option<String> = None;
    let mut goal: Option<String> = None;
    let mut search_path: Option<String> = None;
    let mut package: Option<String> = None;
    let mut json = false;
    let mut persist_outcome = false;
    let mut persist_evaluation = false;
    let mut persist_promotions = false;
    let mut persist_revisions = false;
    let mut persist_retirements = false;
    let mut index = 1usize;
    while index < args.len() {
        match args[index].as_str() {
            "--goal" => {
                goal = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --goal")?,
                );
                index += 2;
            }
            "--path" => {
                search_path = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --path")?,
                );
                index += 2;
            }
            "--package" => {
                package = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --package")?,
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--persist-outcome" => {
                persist_outcome = true;
                index += 1;
            }
            "--persist-evaluation" => {
                persist_evaluation = true;
                index += 1;
            }
            "--persist-promotions" => {
                persist_promotions = true;
                index += 1;
            }
            "--persist-revisions" => {
                persist_revisions = true;
                index += 1;
            }
            "--persist-retirements" => {
                persist_retirements = true;
                index += 1;
            }
            value => {
                if pattern.is_none() {
                    pattern = Some(value.to_owned());
                    index += 1;
                } else {
                    bail!("unexpected extra argument for tool-run: {value}");
                }
            }
        }
    }

    let resolved = resolve_tool_selector(&tool_name)?;
    let tool = resolved.tool.clone();
    let delegated_tool = resolved
        .delegated_tool
        .clone()
        .unwrap_or_else(|| tool.clone());
    let pattern = if delegated_tool.id == "tool.rg" {
        Some(pattern.context("missing search pattern for tool-run rg")?)
    } else {
        pattern
    };
    let goal = goal.unwrap_or_else(|| {
        pattern
            .clone()
            .unwrap_or_else(|| "run cargo test".to_owned())
    });

    let memory_namespace = runtime_memory_namespace();
    let retriever = WorkspaceMemoryRetriever::new(
        default_workspace_root(),
        workspace_namespace_from_memory_namespace(&memory_namespace),
    );
    let assets = load_tool_assets(&retriever, memory_namespace.clone(), &delegated_tool)?;
    let delegated_plan = build_tool_execution_plan_from_assets(
        &DefaultToolExecutionBinder,
        goal.clone(),
        &delegated_tool,
        &assets,
    )?;
    let mut plan = delegated_plan.clone();
    plan.tool_id = tool.id.clone();
    if let Some(skill) = &resolved.skill
        && !skill.instructions.trim().is_empty()
    {
        plan.guidance.insert(0, skill.instructions.clone());
    }
    let atomic_outcome = match delegated_tool.id.as_str() {
        "tool.rg" => run_rg_with_plan(
            &goal,
            pattern.as_deref().expect("rg pattern should exist"),
            search_path.as_deref(),
            &delegated_plan,
        )?,
        "tool.cargo-test" => run_cargo_test_with_plan(
            &goal,
            pattern.as_deref(),
            package.as_deref(),
            &delegated_plan,
        )?,
        _ => run_generic_tool_with_plan(
            &goal,
            pattern.as_deref(),
            search_path.as_deref(),
            &delegated_plan,
        )?,
    };
    let delegated_outcome = if delegated_tool.id != tool.id {
        Some(atomic_outcome.clone().with_parent_tool_id(tool.id.clone()))
    } else {
        None
    };
    let outcome = if delegated_tool.id != tool.id {
        atomic_outcome.wrapped_by(tool.id.clone())
    } else {
        atomic_outcome
    };
    let evaluation = evaluate_tool_execution(
        &tool,
        &plan,
        &outcome,
        &assets,
        &GeneralizationPolicy::default(),
        &default_tool_promotion_rule(&tool),
        &RetirementRule::default(),
    );

    let persisted_path = if persist_outcome {
        let request = room_memory_write_request_from_tool_outcome(&tool, &outcome);
        Some(persist_room_memory(
            default_workspace_root(),
            workspace_namespace_from_memory_namespace(&memory_namespace),
            &request,
        )?)
    } else {
        None
    };
    let persisted_delegated_path = if persist_outcome {
        if let Some(delegated_outcome) = &delegated_outcome {
            let request =
                room_memory_write_request_from_tool_outcome(&delegated_tool, delegated_outcome);
            Some(persist_room_memory(
                default_workspace_root(),
                workspace_namespace_from_memory_namespace(&memory_namespace),
                &request,
            )?)
        } else {
            None
        }
    } else {
        None
    };
    let persisted_evaluation_paths = if persist_evaluation {
        let mut paths = room_memory_write_requests_from_tool_evaluation(&tool, &evaluation)
            .into_iter()
            .map(|request| {
                persist_room_memory(
                    default_workspace_root(),
                    workspace_namespace_from_memory_namespace(&memory_namespace),
                    &request,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        paths.extend(persist_tool_evolution_events(
            default_workspace_root(),
            workspace_namespace_from_memory_namespace(&memory_namespace),
            &tool,
            &evaluation,
        )?);
        paths
    } else {
        Vec::new()
    };
    let persisted_promotion_paths = if persist_promotions {
        persist_compiled_tool_assets(
            default_workspace_root(),
            workspace_namespace_from_memory_namespace(&memory_namespace),
            &tool,
            &assets,
            &evaluation,
        )?
    } else {
        Vec::new()
    };
    let persisted_revision_paths = if persist_revisions {
        persist_revised_tool_assets(
            default_workspace_root(),
            workspace_namespace_from_memory_namespace(&memory_namespace),
            &tool,
            &assets,
            &evaluation,
            &outcome,
        )?
    } else {
        Vec::new()
    };
    let persisted_retirement_paths = if persist_retirements {
        persist_retired_tool_assets(
            default_workspace_root(),
            workspace_namespace_from_memory_namespace(&memory_namespace),
            &tool,
            &assets,
            &evaluation,
        )?
    } else {
        Vec::new()
    };

    if json {
        let payload = serde_json::json!({
            "plan": plan,
            "outcome": outcome,
            "delegated_outcome": delegated_outcome,
            "evaluation": evaluation,
            "persisted_path": persisted_path.as_ref().map(|path| path.display().to_string()),
            "persisted_delegated_path": persisted_delegated_path
                .as_ref()
                .map(|path| path.display().to_string()),
            "persisted_evaluation_paths": persisted_evaluation_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "persisted_promotion_paths": persisted_promotion_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "persisted_revision_paths": persisted_revision_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
            "persisted_retirement_paths": persisted_retirement_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).context("failed to serialize tool run")?
        );
    } else {
        println!("tool> {}", plan.tool_id);
        if let Some(parent_tool_id) = &outcome.parent_tool_id {
            println!("parent> {}", parent_tool_id);
        }
        if !outcome.invoked_tool_ids.is_empty() {
            println!("invoked> {}", outcome.invoked_tool_ids.join(", "));
        }
        println!("command> {}", outcome.command.join(" "));
        println!("success> {}", outcome.success);
        println!("summary> {}", outcome.summary);
        for line in &plan.guidance {
            println!("guidance> {line}");
        }
        for line in &plan.validation_steps {
            println!("validation> {line}");
        }
        for line in &plan.recovery_steps {
            println!("recovery> {line}");
        }
        for observation in &outcome.observations {
            println!("observation> {observation}");
        }
        for signal in &evaluation.signals {
            println!("signal> {}", signal_label(signal));
        }
        if !evaluation.promote_candidate_ids.is_empty() {
            println!("promote> {}", evaluation.promote_candidate_ids.join(", "));
        }
        if !evaluation.retire_candidate_ids.is_empty() {
            println!("retire> {}", evaluation.retire_candidate_ids.join(", "));
        }
        if let Some(path) = persisted_path {
            println!("persisted tool outcome: {}", path.display());
        }
        if let Some(path) = persisted_delegated_path {
            println!("persisted delegated outcome: {}", path.display());
        }
        for path in &persisted_evaluation_paths {
            println!("persisted evaluation: {}", path.display());
        }
        for path in &persisted_promotion_paths {
            println!("persisted promotion: {}", path.display());
        }
        for path in &persisted_revision_paths {
            println!("persisted revision: {}", path.display());
        }
        for path in &persisted_retirement_paths {
            println!("persisted retirement: {}", path.display());
        }
    }

    Ok(())
}

fn seed_rg_tool_assets() -> Result<()> {
    let namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
    let repository =
        MemoryRoomRepository::with_namespace(default_workspace_root(), workspace_namespace);
    let room = MemoryRoom::new(
        "room.tool.rg",
        MemoryLayer::Project,
        "RG Tool Room",
        "Reusable rg search guidance.",
    )
    .with_namespace(namespace.clone())
    .with_visibility(MemoryVisibility::Private)
    .with_tag("tool")
    .with_tag("rg")
    .with_tag("project");
    repository.write_room(&room)?;

    let assets = vec![
        (
            "asset.room.tool.rg.recipe.search-narrow-first",
            "workflow.search-narrow-first.md",
            MemoryKind::WorkflowMemory,
            "RG Narrow Search First",
            "Prefer narrowing search scope before broad content search.",
            vec!["tool", "rg", "recipe"],
        ),
        (
            "asset.room.tool.rg.validation.refine-broad-results",
            "validation.refine-broad-results.md",
            MemoryKind::Knowledge,
            "RG Refine Broad Results",
            "If results are too broad, refine by path, extension, or keyword before answering.",
            vec!["tool", "rg", "validation"],
        ),
        (
            "asset.room.tool.rg.recovery.retry-strategy",
            "recovery.retry-strategy.md",
            MemoryKind::Decision,
            "RG Retry Strategy",
            "If no matches are found, retry with alternate keywords or a narrower path guess.",
            vec!["tool", "rg", "recovery"],
        ),
    ];

    for (id, file_name, kind, title, summary, tags) in assets {
        let mut asset = MemoryRoomAsset::new(
            id,
            room.id.clone(),
            file_name,
            MemoryLayer::Project,
            MemoryRoomAssetKind::Compressed,
            title,
            summary,
        )
        .with_namespace(namespace.clone())
        .with_visibility(MemoryVisibility::Private)
        .with_memory_kind(kind)
        .with_owner(MemoryOwnerRef::project(room.id.clone()));
        for tag in tags {
            asset = asset.with_tag(tag);
        }
        repository.write_asset(&room, &asset)?;
    }

    println!("tool> seeded rg assets into {}", room.id);
    Ok(())
}

fn seed_cargo_test_tool_assets() -> Result<()> {
    let namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
    let repository =
        MemoryRoomRepository::with_namespace(default_workspace_root(), workspace_namespace);
    let room = MemoryRoom::new(
        "room.tool.cargo-test",
        MemoryLayer::Project,
        "Cargo Test Tool Room",
        "Reusable cargo test guidance.",
    )
    .with_namespace(namespace.clone())
    .with_visibility(MemoryVisibility::Private)
    .with_tag("tool")
    .with_tag("cargo-test")
    .with_tag("project");
    repository.write_room(&room)?;

    let assets = vec![
        (
            "asset.room.tool.cargo-test.recipe.targeted-first",
            "workflow.targeted-first.md",
            MemoryKind::WorkflowMemory,
            "Cargo Test Targeted First",
            "Start with a targeted test filter before wider test runs.",
            vec!["tool", "cargo-test", "recipe"],
        ),
        (
            "asset.room.tool.cargo-test.validation.check-ran-tests",
            "validation.check-ran-tests.md",
            MemoryKind::Knowledge,
            "Cargo Test Check Ran Tests",
            "Check whether the intended tests actually ran before trusting the result.",
            vec!["tool", "cargo-test", "validation"],
        ),
        (
            "asset.room.tool.cargo-test.recovery.retry-broader",
            "recovery.retry-broader.md",
            MemoryKind::Decision,
            "Cargo Test Retry Broader",
            "If no tests matched the filter, retry with a broader filter or no filter.",
            vec!["tool", "cargo-test", "recovery"],
        ),
    ];

    for (id, file_name, kind, title, summary, tags) in assets {
        let mut asset = MemoryRoomAsset::new(
            id,
            room.id.clone(),
            file_name,
            MemoryLayer::Project,
            MemoryRoomAssetKind::Compressed,
            title,
            summary,
        )
        .with_namespace(namespace.clone())
        .with_visibility(MemoryVisibility::Private)
        .with_memory_kind(kind)
        .with_owner(MemoryOwnerRef::project(room.id.clone()));
        for tag in tags {
            asset = asset.with_tag(tag);
        }
        repository.write_asset(&room, &asset)?;
    }

    println!("tool> seeded cargo-test assets into {}", room.id);
    Ok(())
}

fn run_rg_with_plan(
    goal: &str,
    pattern: &str,
    search_path: Option<&str>,
    plan: &hc_context::ToolExecutionPlan,
) -> Result<ToolExecutionOutcome> {
    let scope = search_path.unwrap_or(".");
    let mut command = plan.suggested_command.clone();
    let output = if plan.suggested_command.iter().any(|arg| arg == "--files") {
        if scope != "." {
            command.push(scope.to_owned());
        }
        let output = Command::new(&command[0])
            .args(&command[1..])
            .output()
            .with_context(|| format!("failed to run {}", command.join(" ")))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let matched = stdout
            .lines()
            .filter(|line| line.contains(pattern))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let success = output.status.success() && !matched.is_empty();
        ToolExecutionOutcome {
            tool_id: plan.tool_id.clone(),
            parent_tool_id: None,
            invoked_tool_ids: Vec::new(),
            goal: goal.to_owned(),
            command,
            success,
            summary: if success {
                format!(
                    "Filtered {} matching file candidates for pattern `{pattern}`.",
                    matched.len()
                )
            } else {
                format!("No file candidates matched pattern `{pattern}`.")
            },
            observations: matched.into_iter().take(10).collect(),
        }
    } else {
        command.push(pattern.to_owned());
        command.push(scope.to_owned());
        let output = Command::new(&command[0])
            .args(&command[1..])
            .output()
            .with_context(|| format!("failed to run {}", command.join(" ")))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let matches = stdout.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
        let success = output.status.success() && !matches.is_empty();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let mut observations = matches.into_iter().take(10).collect::<Vec<_>>();
        if !stderr.is_empty() {
            observations.push(format!("stderr: {stderr}"));
        }
        ToolExecutionOutcome {
            tool_id: plan.tool_id.clone(),
            parent_tool_id: None,
            invoked_tool_ids: Vec::new(),
            goal: goal.to_owned(),
            command,
            success,
            summary: if success {
                format!(
                    "Found {} rg match lines for pattern `{pattern}`.",
                    observations.len()
                )
            } else {
                format!("No rg matches found for pattern `{pattern}`.")
            },
            observations,
        }
    };

    Ok(output)
}

fn run_cargo_test_with_plan(
    goal: &str,
    filter: Option<&str>,
    package: Option<&str>,
    plan: &hc_context::ToolExecutionPlan,
) -> Result<ToolExecutionOutcome> {
    let mut command = plan.suggested_command.clone();
    if let Some(package) = package {
        command.push("-p".to_owned());
        command.push(package.to_owned());
    }
    if let Some(filter) = filter
        && !filter.trim().is_empty()
    {
        command.push(filter.to_owned());
    }

    let output = Command::new(&command[0])
        .args(&command[1..])
        .output()
        .with_context(|| format!("failed to run {}", command.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");
    let observations = combined
        .lines()
        .filter(|line| {
            line.contains("test result")
                || line.contains("running ")
                || line.contains("FAILED")
                || line.contains("ok")
                || line.contains("error:")
                || line.contains("0 tests")
        })
        .map(ToOwned::to_owned)
        .take(12)
        .collect::<Vec<_>>();
    let success = output.status.success();
    let summary = if success {
        format!("cargo test succeeded for goal `{goal}`.")
    } else {
        format!("cargo test failed for goal `{goal}`.")
    };

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command,
        success,
        summary,
        observations,
    })
}

fn run_generic_tool_with_plan(
    goal: &str,
    argument: Option<&str>,
    working_dir: Option<&str>,
    plan: &hc_context::ToolExecutionPlan,
) -> Result<ToolExecutionOutcome> {
    let mut command = plan.suggested_command.clone();
    if command.is_empty() {
        bail!("tool {} has no command to execute", plan.tool_id);
    }
    if let Some(argument) = argument
        && !argument.trim().is_empty()
    {
        command.push(argument.to_owned());
    }

    let mut process = Command::new(&command[0]);
    process.args(&command[1..]);
    if let Some(working_dir) = working_dir {
        process.current_dir(working_dir);
    }
    let output = process
        .output()
        .with_context(|| format!("failed to run {}", command.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let observations = stdout
        .lines()
        .map(|line| format!("stdout: {line}"))
        .chain(stderr.lines().map(|line| format!("stderr: {line}")))
        .take(12)
        .collect::<Vec<_>>();
    let success = output.status.success();

    Ok(ToolExecutionOutcome {
        tool_id: plan.tool_id.clone(),
        parent_tool_id: None,
        invoked_tool_ids: Vec::new(),
        goal: goal.to_owned(),
        command,
        success,
        summary: if success {
            format!("tool command succeeded for goal `{goal}`.")
        } else {
            format!("tool command failed for goal `{goal}`.")
        },
        observations,
    })
}

fn default_tool_promotion_rule(tool: &hc_context::ToolSpec) -> PromotionRule {
    PromotionRule {
        from_stage: hc_memory::MemoryAssetStage::Generalized,
        to_stage: hc_memory::MemoryAssetStage::Compiled,
        min_confidence_milli: 800,
        required_tags: vec![
            "tool".to_owned(),
            tool.id.trim_start_matches("tool.").to_owned(),
        ],
        required_consumers: vec![hc_context::AssetConsumer::Executor],
    }
}

fn signal_label(signal: &EvaluationSignal) -> &'static str {
    match signal {
        EvaluationSignal::HumanConfirmed => "human_confirmed",
        EvaluationSignal::HumanRejected => "human_rejected",
        EvaluationSignal::ExecutionSucceeded => "execution_succeeded",
        EvaluationSignal::ExecutionFailed => "execution_failed",
        EvaluationSignal::ValidationPassed => "validation_passed",
        EvaluationSignal::ValidationFailed => "validation_failed",
        EvaluationSignal::RepeatedReuse => "repeated_reuse",
        EvaluationSignal::SupersededByNewerAsset => "superseded_by_newer_asset",
    }
}

fn handle_chat(registry: &ProviderRegistry, args: &[String]) -> Result<()> {
    let chat_flow = new_trace_id("flow.chat");
    let _chat_flow_guard = enter_flow_context(chat_flow.clone());
    let mut provider = default_provider();
    let mut model = default_model();
    let mut system_message = env::var("HC_LLM_SYSTEM").ok();
    let mut request_mode = default_request_mode();
    let mut output_style = OutputStyle {
        typewriter: true,
        typewriter_delay_ms: default_typewriter_delay_ms(),
    };
    let mut show_memory = false;
    let mut persist_chat_memory = default_chat_memory_enabled();
    let mut persist_literary_memory = default_literary_memory_enabled();
    let mut chat_room_id: Option<String> = None;
    let mut organizer_mode = default_organizer_mode();
    let mut prompt_asset_mode = default_prompt_asset_mode();
    let mut preference_summary_mode = default_preference_summary_mode();
    let mut promotion_trigger = default_promotion_trigger_mode();
    let mut promotion_window_size = default_promotion_window_size();
    let mut literary_trigger = default_literary_trigger_mode();
    let mut literary_window_size = default_literary_window_size();
    let mut chat_room_window_size = default_chat_room_window_size();
    let mut memory_query = ContextMemoryQuery::default().for_namespace(runtime_memory_namespace());

    let mut owner_kind: Option<MemoryOwnerKind> = None;
    let mut owner_id: Option<String> = None;

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
            "--scope" => {
                memory_query = memory_query.with_scope(parse_memory_scope(
                    args.get(index + 1).context("missing value for --scope")?,
                )?);
                index += 2;
            }
            "--owner-kind" => {
                owner_kind = Some(parse_memory_owner_kind(
                    args.get(index + 1)
                        .context("missing value for --owner-kind")?,
                )?);
                index += 2;
            }
            "--owner-id" => {
                owner_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --owner-id")?,
                );
                index += 2;
            }
            "--memory-kind" => {
                memory_query.memory_query.kind = Some(parse_memory_kind(
                    args.get(index + 1)
                        .context("missing value for --memory-kind")?,
                )?);
                index += 2;
            }
            "--tag" => {
                memory_query = memory_query.with_tag(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --tag")?,
                );
                index += 2;
            }
            "--memory-limit" => {
                memory_query = memory_query.with_limit(
                    args.get(index + 1)
                        .context("missing value for --memory-limit")?
                        .parse::<usize>()
                        .context("invalid value for --memory-limit")?,
                );
                index += 2;
            }
            "--request-mode" => {
                request_mode = parse_request_mode(
                    args.get(index + 1)
                        .context("missing value for --request-mode")?,
                )?;
                index += 2;
            }
            "--stream" => {
                request_mode = RequestMode::Stream;
                index += 1;
            }
            "--direct" => {
                request_mode = RequestMode::Direct;
                index += 1;
            }
            "--typewriter" => {
                output_style.typewriter = true;
                index += 1;
            }
            "--no-typewriter" => {
                output_style.typewriter = false;
                index += 1;
            }
            "--typewriter-delay-ms" => {
                output_style.typewriter_delay_ms = args
                    .get(index + 1)
                    .context("missing value for --typewriter-delay-ms")?
                    .parse::<u64>()
                    .context("invalid value for --typewriter-delay-ms")?;
                index += 2;
            }
            "--show-memory" => {
                show_memory = true;
                index += 1;
            }
            "--chat-memory" => {
                persist_chat_memory = true;
                index += 1;
            }
            "--no-chat-memory" => {
                persist_chat_memory = false;
                index += 1;
            }
            "--literary-memory" => {
                persist_literary_memory = true;
                index += 1;
            }
            "--no-literary-memory" => {
                persist_literary_memory = false;
                index += 1;
            }
            "--chat-room-id" => {
                chat_room_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .context("missing value for --chat-room-id")?,
                );
                index += 2;
            }
            "--organizer-mode" => {
                organizer_mode = parse_strategy_mode(
                    args.get(index + 1)
                        .context("missing value for --organizer-mode")?,
                )?;
                index += 2;
            }
            "--prompt-asset-mode" => {
                prompt_asset_mode = parse_strategy_mode(
                    args.get(index + 1)
                        .context("missing value for --prompt-asset-mode")?,
                )?;
                index += 2;
            }
            "--preference-summary-mode" => {
                preference_summary_mode = parse_strategy_mode(
                    args.get(index + 1)
                        .context("missing value for --preference-summary-mode")?,
                )?;
                index += 2;
            }
            "--promotion-trigger" => {
                promotion_trigger = parse_promotion_trigger_mode(
                    args.get(index + 1)
                        .context("missing value for --promotion-trigger")?,
                )?;
                index += 2;
            }
            "--promotion-window-size" => {
                promotion_window_size = args
                    .get(index + 1)
                    .context("missing value for --promotion-window-size")?
                    .parse::<usize>()
                    .context("invalid value for --promotion-window-size")?;
                if promotion_window_size == 0 {
                    bail!("--promotion-window-size must be greater than 0");
                }
                index += 2;
            }
            "--literary-trigger" => {
                literary_trigger = parse_promotion_trigger_mode(
                    args.get(index + 1)
                        .context("missing value for --literary-trigger")?,
                )?;
                index += 2;
            }
            "--literary-window-size" => {
                literary_window_size = args
                    .get(index + 1)
                    .context("missing value for --literary-window-size")?
                    .parse::<usize>()
                    .context("invalid value for --literary-window-size")?;
                if literary_window_size == 0 {
                    bail!("--literary-window-size must be greater than 0");
                }
                index += 2;
            }
            "--chat-room-window-size" => {
                chat_room_window_size = args
                    .get(index + 1)
                    .context("missing value for --chat-room-window-size")?
                    .parse::<usize>()
                    .context("invalid value for --chat-room-window-size")?;
                if chat_room_window_size == 0 {
                    bail!("--chat-room-window-size must be greater than 0");
                }
                index += 2;
            }
            other => bail!("unknown chat option: {other}"),
        }
    }

    if let (Some(owner_kind), Some(owner_id)) = (owner_kind.clone(), owner_id.clone()) {
        memory_query.memory_query.owner = Some(MemoryOwnerRef::new(owner_kind, owner_id));
    } else if owner_kind.is_some() || owner_id.is_some() {
        bail!("--owner-kind and --owner-id must be used together");
    }

    emit_cli_trace_with_fields(
        "chat",
        "start",
        Some("started"),
        "starting interactive chat session",
        BTreeMap::from([
            ("flow_id".to_owned(), chat_flow.clone()),
            ("provider".to_owned(), provider.clone()),
            ("model".to_owned(), model.clone()),
            (
                "request_mode".to_owned(),
                request_mode_label(request_mode).to_owned(),
            ),
            (
                "organizer_mode".to_owned(),
                strategy_mode_label(organizer_mode).to_owned(),
            ),
            (
                "prompt_asset_mode".to_owned(),
                strategy_mode_label(prompt_asset_mode).to_owned(),
            ),
            (
                "promotion_trigger".to_owned(),
                promotion_trigger_mode_label(promotion_trigger).to_owned(),
            ),
        ]),
    );

    println!("hc-context chat");
    print_organize_status(
        "ready",
        format!(
            "provider={provider} model={model} request_mode={} memory_scope={} organizer={} prompt_assets={} preference_summary={} promotion_trigger={} promotion_window_size={} literary_memory={} literary_trigger={} literary_window_size={} chat_room_window_size={}",
            request_mode_label(request_mode),
            memory_scope_label(memory_query.memory_query.scope.as_ref()),
            strategy_mode_label(organizer_mode),
            strategy_mode_label(prompt_asset_mode),
            strategy_mode_label(preference_summary_mode),
            promotion_trigger_mode_label(promotion_trigger),
            promotion_window_size,
            if persist_literary_memory { "on" } else { "off" },
            promotion_trigger_mode_label(literary_trigger),
            literary_window_size,
            chat_room_window_size,
        ),
    );
    println!("Type /help for commands, /quit to exit.");

    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let retriever =
        WorkspaceMemoryRetriever::new(default_workspace_root(), workspace_namespace.clone());
    let composer = DefaultContextComposer;
    let organizer_model = ModelRef::new(provider.clone(), model.clone());
    let llm_priority_gate = Arc::new(LlmPriorityGate::default());
    let organizer = build_memory_organizer(
        registry,
        &organizer_model,
        workspace_namespace.clone(),
        organizer_mode,
    );
    let prompt_asset_synthesizer = build_prompt_asset_synthesizer(
        registry,
        &organizer_model,
        workspace_namespace.clone(),
        prompt_asset_mode,
    );
    let mut history = Vec::new();
    let mut chat_room = if persist_chat_memory {
        let room = resolve_chat_room(
            default_workspace_root(),
            &workspace_namespace,
            &memory_namespace,
            chat_room_id.clone(),
        )?;
        ensure_chat_room(default_workspace_root(), &workspace_namespace, &room)?;
        print_organize_status("capture", format!("status=ready room={}", room.id));
        Some(room)
    } else {
        print_organize_status("capture", "status=disabled");
        None
    };
    let mut turn_index = 0usize;
    let mut pending_global_promotions = VecDeque::new();
    let mut pending_literary_memory = VecDeque::new();
    let mut background_worker = if let Some(room) = chat_room.clone() {
        if promotion_trigger == PromotionTriggerMode::Background
            || literary_trigger == PromotionTriggerMode::Background
        {
            Some(start_background_memory_worker(
                default_workspace_root().to_path_buf(),
                workspace_namespace.clone(),
                memory_namespace.clone(),
                room,
                provider.clone(),
                model.clone(),
                organizer_mode,
                preference_summary_mode,
                llm_priority_gate.clone(),
            ))
        } else {
            None
        }
    } else {
        None
    };

    loop {
        let input = prompt_raw("you> ")?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed {
            "/quit" | "/exit" => {
                flush_global_promotion_queue(
                    &mut pending_global_promotions,
                    organizer.as_ref(),
                    default_workspace_root(),
                    &workspace_namespace,
                    &memory_namespace,
                    chat_room.as_ref(),
                    registry,
                    &organizer_model,
                    preference_summary_mode,
                )?;
                flush_literary_memory_queue(
                    &mut pending_literary_memory,
                    registry,
                    &organizer_model,
                    default_workspace_root(),
                    &workspace_namespace,
                    chat_room.as_ref(),
                )?;
                shutdown_background_memory_worker(background_worker.take());
                break;
            }
            "/help" => {
                println!("/help");
                println!("/clear");
                println!("/prompts [filter]");
                println!("/promote");
                println!("/wenyan");
                println!("/system <text>");
                println!("/quit");
                continue;
            }
            "/clear" => {
                history.clear();
                println!("history cleared");
                continue;
            }
            _ if trimmed.starts_with("/prompts") => {
                let filter = trimmed
                    .strip_prefix("/prompts")
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                print_managed_prompt_history(
                    default_workspace_root(),
                    &workspace_namespace,
                    filter,
                )?;
                continue;
            }
            "/promote" => {
                if promotion_trigger == PromotionTriggerMode::Background {
                    print_organize_status("promote", "status=background");
                    continue;
                }
                let flushed = flush_global_promotion_queue(
                    &mut pending_global_promotions,
                    organizer.as_ref(),
                    default_workspace_root(),
                    &workspace_namespace,
                    &memory_namespace,
                    chat_room.as_ref(),
                    registry,
                    &organizer_model,
                    preference_summary_mode,
                )?;
                print_organize_status("promote", format!("status=drained items={flushed}"));
                continue;
            }
            "/wenyan" => {
                if literary_trigger == PromotionTriggerMode::Background {
                    print_organize_status("literary", "status=background");
                    continue;
                }
                let flushed = flush_literary_memory_queue(
                    &mut pending_literary_memory,
                    registry,
                    &organizer_model,
                    default_workspace_root(),
                    &workspace_namespace,
                    chat_room.as_ref(),
                )?;
                print_organize_status("literary", format!("status=drained items={flushed}"));
                continue;
            }
            _ if trimmed.starts_with("/system ") => {
                let value = trimmed
                    .strip_prefix("/system ")
                    .map(str::trim)
                    .unwrap_or_default();
                system_message = if value.is_empty() {
                    None
                } else {
                    Some(value.to_owned())
                };
                history.clear();
                println!("system prompt updated");
                continue;
            }
            _ => {}
        }

        turn_index += 1;
        let turn_flow = format!("{chat_flow}.turn.{turn_index}");
        let _turn_flow_guard = enter_flow_context(turn_flow.clone());
        emit_cli_trace_with_fields(
            "chat_turn",
            "receive_input",
            Some("started"),
            "processing chat turn",
            BTreeMap::from([
                ("turn_index".to_owned(), turn_index.to_string()),
                (
                    "input_chars".to_owned(),
                    trimmed.chars().count().to_string(),
                ),
                (
                    "lightweight_candidate".to_owned(),
                    should_use_lightweight_chat_path(trimmed, turn_index).to_string(),
                ),
            ]),
        );
        let should_enqueue_background_promotion =
            chat_room.is_some() && promotion_trigger == PromotionTriggerMode::Background;
        if let Some(room) = &chat_room {
            persist_chat_turn_user_message(
                default_workspace_root(),
                &workspace_namespace,
                room,
                turn_index,
                trimmed,
            )?;
            if !should_enqueue_background_promotion {
                pending_global_promotions.push_back(trimmed.to_owned());
                if promotion_trigger == PromotionTriggerMode::Immediate {
                    flush_global_promotion_queue(
                        &mut pending_global_promotions,
                        organizer.as_ref(),
                        default_workspace_root(),
                        &workspace_namespace,
                        &memory_namespace,
                        Some(room),
                        registry,
                        &organizer_model,
                        preference_summary_mode,
                    )?;
                    print_organize_status("promote", "status=drained trigger=immediate");
                }
            }
        }

        history.push(ChatMessage::new(MessageRole::User, trimmed.to_owned()));
        let mut effective_memory_query = if memory_query.memory_query.text.is_some() {
            memory_query.clone()
        } else {
            memory_query.clone().with_text(trimmed)
        };
        if let Some(room) = &chat_room {
            effective_memory_query = effective_memory_query.with_room_anchor(room.id.clone());
        }
        match ensure_context_room_for_input(
            default_workspace_root(),
            &workspace_namespace,
            &memory_namespace,
            &retriever,
            &effective_memory_query,
            trimmed,
            chat_room.as_ref(),
        )? {
            ContextRoomResolution::Created(room) => {
                emit_cli_trace_with_fields(
                    "context_room",
                    "resolve",
                    Some("created"),
                    "created context room for input",
                    BTreeMap::from([
                        ("room_id".to_owned(), room.id.clone()),
                        (
                            "layer".to_owned(),
                            format!("{:?}", room.layer).to_ascii_lowercase(),
                        ),
                    ]),
                );
                println!(
                    "room> created {} room: {}",
                    format!("{:?}", room.layer).to_ascii_lowercase(),
                    room.id
                );
            }
            ContextRoomResolution::Reused(room) if show_memory => {
                emit_cli_trace_with_fields(
                    "context_room",
                    "resolve",
                    Some("reused"),
                    "reused context room for input",
                    BTreeMap::from([
                        ("room_id".to_owned(), room.id.clone()),
                        (
                            "layer".to_owned(),
                            format!("{:?}", room.layer).to_ascii_lowercase(),
                        ),
                    ]),
                );
                println!(
                    "room> reused {} room: {}",
                    format!("{:?}", room.layer).to_ascii_lowercase(),
                    room.id
                );
            }
            ContextRoomResolution::Reused(room) => {
                emit_cli_trace_with_fields(
                    "context_room",
                    "resolve",
                    Some("reused"),
                    "reused context room for input",
                    BTreeMap::from([("room_id".to_owned(), room.id.clone())]),
                );
            }
            ContextRoomResolution::None => {
                emit_cli_trace(
                    "context_room",
                    "resolve",
                    Some("skipped"),
                    "no context room materialized for input",
                );
            }
        }
        let generation = GenerateRequest::new(
            ModelRef::new(provider.clone(), model.clone()),
            history.clone(),
        );
        let context_namespace = workspace_namespace_from_memory_namespace(
            &effective_memory_query
                .memory_query
                .namespace
                .clone()
                .unwrap_or_else(runtime_memory_namespace),
        );
        let system_prompt = match system_message.clone() {
            Some(system_message) => system_message,
            None => load_context_memory_system_prompt(&context_namespace)?,
        };
        let request = ContextRequest::new(generation)
            .with_memory_query(effective_memory_query.clone())
            .with_system_prompt(system_prompt)
            .with_prompt_policy(PromptPolicy::new(
                "Memory Usage Policy",
                load_context_memory_usage_policy_prompt(&context_namespace)?,
            ));
        let use_lightweight_chat_path = should_use_lightweight_chat_path(trimmed, turn_index);

        let _permit = llm_priority_gate.acquire_foreground();
        if should_enqueue_background_promotion {
            if let Some(worker) = &background_worker {
                enqueue_background_task(
                    &worker.sender,
                    BackgroundMemoryTask::GlobalPromotion {
                        content: trimmed.to_owned(),
                    },
                    "promotion",
                );
                print_organize_status("promote", "status=queued mode=background");
            }
        }
        print!("assistant> ");
        io::stdout().flush().context("failed to flush stdout")?;
        let response_result = match request_mode {
            RequestMode::Direct => {
                let response = if use_lightweight_chat_path {
                    generate_lightweight_chat_response(registry, &request)
                } else {
                    generate_with_context_using_synthesizer(
                        registry,
                        &retriever,
                        &composer,
                        prompt_asset_synthesizer.as_ref(),
                        &request,
                    )
                };
                if let Ok(response) = &response {
                    render_output(&response.response.message.content, output_style)?;
                }
                response
            }
            RequestMode::Stream => {
                let mut callback = |chunk: StreamChunk| -> Result<(), hc_llm::LlmError> {
                    render_output(&chunk.delta, output_style)
                        .map_err(|error| hc_llm::LlmError::ProviderFailure(error.to_string()))?;
                    Ok(())
                };
                if use_lightweight_chat_path {
                    generate_lightweight_chat_response_stream(registry, &request, &mut callback)
                } else {
                    generate_with_context_stream_using_synthesizer(
                        registry,
                        &retriever,
                        &composer,
                        prompt_asset_synthesizer.as_ref(),
                        &request,
                        &mut callback,
                    )
                }
            }
        };
        let response = match response_result {
            Ok(response) => response,
            Err(error) => {
                emit_cli_trace_with_fields(
                    "chat_turn",
                    "generate_response",
                    Some("failed"),
                    concise_error_message(&error),
                    BTreeMap::from([("turn_index".to_owned(), turn_index.to_string())]),
                );
                println!();
                history.pop();
                if is_retryable_provider_error(&error) {
                    println!(
                        "assistant> temporary provider error: {}",
                        concise_error_message(&error)
                    );
                    println!("assistant> please retry in a moment.");
                    continue;
                }
                return Err(error);
            }
        };
        println!();
        emit_cli_trace_with_fields(
            "chat_turn",
            "generate_response",
            Some("completed"),
            "generated assistant response",
            BTreeMap::from([
                ("turn_index".to_owned(), turn_index.to_string()),
                (
                    "response_chars".to_owned(),
                    response
                        .response
                        .message
                        .content
                        .chars()
                        .count()
                        .to_string(),
                ),
                (
                    "recalled_count".to_owned(),
                    response.recalled_memories.len().to_string(),
                ),
                (
                    "lightweight_path".to_owned(),
                    use_lightweight_chat_path.to_string(),
                ),
            ]),
        );
        history.push(response.response.message.clone());

        let persisted_prompt_assets = persist_synthesized_prompt_assets(
            default_workspace_root(),
            workspace_namespace.clone(),
            &response,
        )?;
        emit_cli_trace_with_fields(
            "prompt_assets",
            "persist",
            Some("completed"),
            "persisted synthesized prompt assets",
            BTreeMap::from([
                ("turn_index".to_owned(), turn_index.to_string()),
                (
                    "asset_count".to_owned(),
                    persisted_prompt_assets.len().to_string(),
                ),
            ]),
        );
        if !persisted_prompt_assets.is_empty() {
            print_organize_status(
                "prompt",
                format!(
                    "status=saved compiled_assets={}",
                    persisted_prompt_assets.len()
                ),
            );
        }

        if let Some(room) = &chat_room {
            persist_chat_turn_assistant_reply(
                default_workspace_root(),
                &workspace_namespace,
                room,
                turn_index,
                &response.response.message.content,
            )?;
            if persist_literary_memory {
                if literary_trigger == PromotionTriggerMode::Background {
                    if let Some(worker) = &background_worker {
                        enqueue_background_task(
                            &worker.sender,
                            BackgroundMemoryTask::Literary {
                                turn_index,
                                content: response.response.message.content.clone(),
                            },
                            "literary",
                        );
                        print_organize_status("literary", "status=queued mode=background");
                    }
                } else {
                    pending_literary_memory
                        .push_back((turn_index, response.response.message.content.clone()));
                    if literary_trigger == PromotionTriggerMode::Immediate {
                        let flushed = flush_literary_memory_queue(
                            &mut pending_literary_memory,
                            registry,
                            &organizer_model,
                            default_workspace_root(),
                            &workspace_namespace,
                            Some(room),
                        )?;
                        if flushed > 0 {
                            print_organize_status(
                                "literary",
                                format!("status=drained items={flushed} trigger=immediate"),
                            );
                        }
                    }
                }
            }
            if let Some(room) = &chat_room
                && should_flush_promotion_queue(
                    promotion_trigger,
                    pending_global_promotions.len(),
                    promotion_window_size,
                )
            {
                let flushed = flush_global_promotion_queue(
                    &mut pending_global_promotions,
                    organizer.as_ref(),
                    default_workspace_root(),
                    &workspace_namespace,
                    &memory_namespace,
                    Some(room),
                    registry,
                    &organizer_model,
                    preference_summary_mode,
                )?;
                if flushed > 0 {
                    print_organize_status("promote", format!("status=drained items={flushed}"));
                }
            }
            if persist_literary_memory
                && should_flush_promotion_queue(
                    literary_trigger,
                    pending_literary_memory.len(),
                    literary_window_size,
                )
            {
                let flushed = flush_literary_memory_queue(
                    &mut pending_literary_memory,
                    registry,
                    &organizer_model,
                    default_workspace_root(),
                    &workspace_namespace,
                    Some(room),
                )?;
                if flushed > 0 {
                    print_organize_status("literary", format!("status=drained items={flushed}"));
                }
            }
        }

        if should_roll_chat_room(chat_room.as_ref(), turn_index, chat_room_window_size) {
            flush_global_promotion_queue(
                &mut pending_global_promotions,
                organizer.as_ref(),
                default_workspace_root(),
                &workspace_namespace,
                &memory_namespace,
                chat_room.as_ref(),
                registry,
                &organizer_model,
                preference_summary_mode,
            )?;
            flush_literary_memory_queue(
                &mut pending_literary_memory,
                registry,
                &organizer_model,
                default_workspace_root(),
                &workspace_namespace,
                chat_room.as_ref(),
            )?;
            shutdown_background_memory_worker(background_worker.take());

            if let Some(room) = chat_room.as_ref() {
                archive_chat_room(
                    default_workspace_root(),
                    &workspace_namespace,
                    room,
                    turn_index,
                )?;
                print_organize_status(
                    "capture",
                    format!("status=archived room={} turns={turn_index}", room.id),
                );
            }

            let next_room =
                create_chat_room(&memory_namespace, default_chat_room_id(&memory_namespace));
            ensure_chat_room(default_workspace_root(), &workspace_namespace, &next_room)?;
            print_organize_status("capture", format!("status=ready room={}", next_room.id));
            chat_room = Some(next_room.clone());
            history.clear();
            turn_index = 0;

            background_worker = if promotion_trigger == PromotionTriggerMode::Background
                || literary_trigger == PromotionTriggerMode::Background
            {
                Some(start_background_memory_worker(
                    default_workspace_root().to_path_buf(),
                    workspace_namespace.clone(),
                    memory_namespace.clone(),
                    next_room,
                    provider.clone(),
                    model.clone(),
                    organizer_mode,
                    preference_summary_mode,
                    llm_priority_gate.clone(),
                ))
            } else {
                None
            };
        }

        if show_memory {
            let room_candidates = retriever.discover_room_candidates(&effective_memory_query)?;
            print_room_candidates_for_chat(&room_candidates);
            print_recalled_memories_for_chat(&response.recalled_memories);
        }
    }

    emit_cli_trace_with_fields(
        "chat",
        "finish",
        Some("completed"),
        "interactive chat session ended",
        BTreeMap::from([("flow_id".to_owned(), chat_flow)]),
    );

    Ok(())
}

fn should_use_lightweight_chat_path(input: &str, turn_index: usize) -> bool {
    if turn_index != 1 {
        return false;
    }

    let normalized = input.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "hi" | "hello"
            | "hey"
            | "你好"
            | "您好"
            | "嗨"
            | "哈喽"
            | "在吗"
            | "在嘛"
            | "早"
            | "早上好"
            | "中午好"
            | "下午好"
            | "晚上好"
    )
}

fn generate_lightweight_chat_response(
    registry: &ProviderRegistry,
    request: &ContextRequest,
) -> Result<ContextResponse> {
    let mut generation = request.generation.clone();
    generation.messages = lightweight_chat_messages(request)?;
    let response = registry
        .generate(&generation)
        .map_err(anyhow::Error::from)?;
    Ok(ContextResponse {
        response,
        recalled_memories: Vec::new(),
        synthesized_prompt_assets: Vec::new(),
    })
}

fn generate_lightweight_chat_response_stream(
    registry: &ProviderRegistry,
    request: &ContextRequest,
    on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), hc_llm::LlmError>,
) -> Result<ContextResponse> {
    let mut generation = request.generation.clone();
    generation.messages = lightweight_chat_messages(request)?;
    let response = registry
        .generate_stream(&generation, on_chunk)
        .map_err(anyhow::Error::from)?;
    Ok(ContextResponse {
        response,
        recalled_memories: Vec::new(),
        synthesized_prompt_assets: Vec::new(),
    })
}

fn lightweight_chat_messages(request: &ContextRequest) -> Result<Vec<ChatMessage>> {
    let mut messages = Vec::new();
    if let Some(system_prompt) = request.system_prompt.as_deref() {
        let namespace = request
            .memory_query
            .memory_query
            .namespace
            .clone()
            .unwrap_or_else(runtime_memory_namespace);
        let workspace_namespace = workspace_namespace_from_memory_namespace(&namespace);
        let lightweight_prompt = load_context_lightweight_chat_prompt(&workspace_namespace)?;
        messages.push(ChatMessage::new(
            MessageRole::System,
            format!("{system_prompt}\n\n{lightweight_prompt}"),
        ));
    }
    messages.extend(request.generation.messages.iter().cloned());
    Ok(messages)
}

fn print_room_candidates_for_generate(candidates: &[hc_context::RoomCandidate]) {
    println!("candidate rooms:");
    if candidates.is_empty() {
        println!("- none");
        return;
    }

    for candidate in candidates.iter().take(6) {
        println!(
            "- {} | kind={} | layer={:?} | score={} | {}",
            candidate.room_id,
            summarize_room_kind(candidate),
            candidate.layer,
            candidate.score_milli,
            summarize_room_signals(&candidate.reasons),
        );
        let summary = truncate_debug_text(&candidate.summary, 96);
        if !summary.is_empty() {
            println!("  summary: {}", summary);
        }
    }
}

fn print_recalled_memories_for_generate(memories: &[hc_context::RetrievedMemory]) {
    println!("recalled memories:");
    if memories.is_empty() {
        println!("- none");
        return;
    }

    for memory in memories {
        let room_suffix = memory
            .room_id
            .as_ref()
            .map(|room_id| format!(" | room={room_id}"))
            .unwrap_or_default();
        println!(
            "- {} | room_kind={} | kind={:?} | source={}{} | confidence={}",
            memory.title,
            summarize_retrieved_room_kind(memory),
            memory.kind,
            memory.source_kind,
            room_suffix,
            memory.confidence_milli,
        );
        let summary = truncate_debug_text(&memory.summary, 108);
        if !summary.is_empty() {
            println!("  summary: {}", summary);
        }
    }
}

fn print_room_candidates_for_chat(candidates: &[hc_context::RoomCandidate]) {
    if candidates.is_empty() {
        println!("room> none");
        return;
    }

    for candidate in candidates.iter().take(6) {
        println!(
            "room> {} | kind={} | layer={:?} | score={} | {}",
            candidate.room_id,
            summarize_room_kind(candidate),
            candidate.layer,
            candidate.score_milli,
            summarize_room_signals(&candidate.reasons),
        );
        let summary = truncate_debug_text(&candidate.summary, 96);
        if !summary.is_empty() {
            println!("room> summary={summary}");
        }
    }
}

fn print_recalled_memories_for_chat(memories: &[hc_context::RetrievedMemory]) {
    if memories.is_empty() {
        println!("memory> none");
        return;
    }

    for memory in memories {
        let room_suffix = memory
            .room_id
            .as_ref()
            .map(|room_id| format!(" | room={room_id}"))
            .unwrap_or_default();
        println!(
            "memory> {} | room_kind={} | kind={:?} | source={}{} | confidence={}",
            memory.title,
            summarize_retrieved_room_kind(memory),
            memory.kind,
            memory.source_kind,
            room_suffix,
            memory.confidence_milli,
        );
        let summary = truncate_debug_text(&memory.summary, 108);
        if !summary.is_empty() {
            println!("memory> summary={summary}");
        }
    }
}

fn print_managed_prompt_history(
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    filter: Option<&str>,
) -> Result<()> {
    let repository = MemoryRoomRepository::with_namespace(
        root.as_ref().to_path_buf(),
        workspace_namespace.clone(),
    );
    let prompt_dir = repository
        .root()
        .join(workspace_namespace.scoped_prefix())
        .join("memory/rooms/project/room.project.prompt-library/prompt");

    if !prompt_dir.exists() {
        println!("prompt> none");
        return Ok(());
    }

    let lowered_filter = filter.map(|value| value.to_ascii_lowercase());
    let mut assets = Vec::new();
    for entry in fs::read_dir(&prompt_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }

        let relative = path
            .strip_prefix(repository.root().join(workspace_namespace.scoped_prefix()))
            .expect("prompt asset should live under workspace namespace")
            .to_path_buf();
        let asset = repository.read_asset(relative)?;
        if let Some(filter) = &lowered_filter
            && !managed_prompt_asset_matches_filter(&asset, filter)
        {
            continue;
        }
        assets.push(asset);
    }

    assets.sort_by(|left, right| left.file_name.cmp(&right.file_name));

    if assets.is_empty() {
        println!("prompt> none");
        return Ok(());
    }

    for asset in assets {
        let status = if asset.tags.iter().any(|tag| tag == "revision") {
            "revision"
        } else {
            "current"
        };
        let summary = truncate_debug_text(&asset.summary, 88);
        let lineage = if asset.derived_from.is_empty() {
            String::new()
        } else {
            format!(" | derived_from={}", asset.derived_from.join(","))
        };
        println!(
            "prompt> {} | {} | {}{}",
            asset.file_name, status, asset.title, lineage
        );
        if !summary.is_empty() {
            println!("prompt> summary={summary}");
        }
    }

    Ok(())
}

fn managed_prompt_asset_matches_filter(asset: &MemoryRoomAsset, lowered_filter: &str) -> bool {
    asset
        .file_name
        .to_ascii_lowercase()
        .contains(lowered_filter)
        || asset.title.to_ascii_lowercase().contains(lowered_filter)
        || asset.summary.to_ascii_lowercase().contains(lowered_filter)
        || asset
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(lowered_filter))
}

fn summarize_room_kind(candidate: &hc_context::RoomCandidate) -> &'static str {
    if candidate.room_id.starts_with("room.agent.")
        || candidate.tags.iter().any(|tag| tag == "agent")
    {
        "agent"
    } else if candidate.room_id.starts_with("room.tool.")
        || candidate.tags.iter().any(|tag| tag == "tool")
    {
        "tool"
    } else if candidate.room_id.starts_with("room.project.")
        || candidate.tags.iter().any(|tag| tag == "project")
    {
        "project"
    } else if candidate.room_id.starts_with("room.task.")
        || candidate.tags.iter().any(|tag| tag == "task")
    {
        "task"
    } else if candidate.room_id.starts_with("room.topic.")
        || candidate.tags.iter().any(|tag| tag == "topic")
    {
        "topic"
    } else if candidate.room_id.starts_with("room.chat.")
        || candidate.tags.iter().any(|tag| tag == "chat")
    {
        "chat"
    } else if candidate.room_id.starts_with("room.global.")
        || candidate.tags.iter().any(|tag| tag == "global")
    {
        "global"
    } else {
        "other"
    }
}

fn summarize_retrieved_room_kind(memory: &hc_context::RetrievedMemory) -> &'static str {
    if let Some(room_id) = &memory.room_id {
        if room_id.starts_with("room.agent.") || memory.tags.iter().any(|tag| tag == "agent") {
            "agent"
        } else if room_id.starts_with("room.tool.") || memory.tags.iter().any(|tag| tag == "tool") {
            "tool"
        } else if room_id.starts_with("room.project.")
            || memory.tags.iter().any(|tag| tag == "project")
        {
            "project"
        } else if room_id.starts_with("room.task.") || memory.tags.iter().any(|tag| tag == "task") {
            "task"
        } else if room_id.starts_with("room.topic.") || memory.tags.iter().any(|tag| tag == "topic")
        {
            "topic"
        } else if room_id.starts_with("room.chat.") || memory.tags.iter().any(|tag| tag == "chat") {
            "chat"
        } else if room_id.starts_with("room.global.")
            || memory.tags.iter().any(|tag| tag == "global")
        {
            "global"
        } else {
            "other"
        }
    } else {
        "other"
    }
}

fn summarize_room_signals(reasons: &[String]) -> String {
    let mut labels = Vec::new();
    for reason in reasons {
        let label = if reason == "anchor-room" {
            "anchor"
        } else if reason == "anchor-related" {
            "anchor-link"
        } else if reason == "active-cluster" {
            "cluster"
        } else if let Some(value) = reason.strip_prefix("recent-hit+") {
            labels.push(format!("hit={value}"));
            continue;
        } else if let Some(value) = reason.strip_prefix("recent+") {
            labels.push(format!("fs={value}"));
            continue;
        } else if reason == "related-room" {
            "linked"
        } else if reason == "text-match" {
            "text"
        } else if reason == "scope-match" {
            "scope"
        } else if reason == "active-room" {
            "active"
        } else if let Some(kind) = reason.strip_prefix("room-kind=") {
            labels.push(format!("kind={kind}"));
            continue;
        } else if let Some(tag) = reason.strip_prefix("tag=") {
            labels.push(format!("tag={tag}"));
            continue;
        } else {
            continue;
        };
        labels.push(label.to_owned());
    }

    if labels.is_empty() {
        "signals=none".to_owned()
    } else {
        format!("signals={}", labels.join(","))
    }
}

fn truncate_debug_text(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut truncated = trimmed.chars().take(limit).collect::<String>();
    if trimmed.chars().count() > limit {
        truncated.push_str("...");
    }
    truncated
}

fn runtime_memory_namespace() -> MemoryNamespace {
    let tenant_id = env::var("HC_TENANT_ID").unwrap_or_else(|_| "local".to_owned());
    let user_id = env::var("HC_USER_ID").unwrap_or_else(|_| "default".to_owned());
    MemoryNamespace::new(tenant_id, user_id)
}

fn default_provider() -> String {
    default_provider_from_env()
}

fn default_model() -> String {
    default_model_from_env()
}

fn default_request_mode() -> RequestMode {
    env::var("HC_LLM_REQUEST_MODE")
        .ok()
        .and_then(|value| parse_request_mode(&value).ok())
        .unwrap_or(RequestMode::Direct)
}

fn default_organizer_mode() -> StrategyMode {
    env::var("HC_CONTEXT_ORGANIZER_MODE")
        .ok()
        .and_then(|value| parse_strategy_mode(&value).ok())
        .unwrap_or(StrategyMode::Auto)
}

fn default_prompt_asset_mode() -> StrategyMode {
    env::var("HC_CONTEXT_PROMPT_ASSET_MODE")
        .ok()
        .and_then(|value| parse_strategy_mode(&value).ok())
        .unwrap_or(StrategyMode::Auto)
}

fn default_preference_summary_mode() -> StrategyMode {
    env::var("HC_CONTEXT_PREFERENCE_SUMMARY_MODE")
        .ok()
        .and_then(|value| parse_strategy_mode(&value).ok())
        .unwrap_or(StrategyMode::Auto)
}

fn default_promotion_trigger_mode() -> PromotionTriggerMode {
    env::var("HC_CONTEXT_PROMOTION_TRIGGER")
        .ok()
        .and_then(|value| parse_promotion_trigger_mode(&value).ok())
        .unwrap_or(PromotionTriggerMode::WindowFull)
}

fn default_promotion_window_size() -> usize {
    env::var("HC_CONTEXT_PROMOTION_WINDOW_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8)
}

fn default_literary_trigger_mode() -> PromotionTriggerMode {
    env::var("HC_CONTEXT_LITERARY_TRIGGER")
        .ok()
        .and_then(|value| parse_promotion_trigger_mode(&value).ok())
        .unwrap_or(PromotionTriggerMode::WindowFull)
}

fn default_literary_window_size() -> usize {
    env::var("HC_CONTEXT_LITERARY_WINDOW_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4)
}

fn default_chat_room_window_size() -> usize {
    env::var("HC_CONTEXT_CHAT_ROOM_WINDOW_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(12)
}

fn priority_debug_enabled() -> bool {
    env::var("HC_CONTEXT_DEBUG_PRIORITY")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn default_typewriter_delay_ms() -> u64 {
    env::var("HC_LLM_TYPEWRITER_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(12)
}

fn default_chat_memory_enabled() -> bool {
    env::var("HC_CONTEXT_CHAT_MEMORY")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

fn default_literary_memory_enabled() -> bool {
    env::var("HC_CONTEXT_LITERARY_MEMORY")
        .ok()
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true)
}

fn parse_request_mode(value: &str) -> Result<RequestMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "direct" | "sync" => Ok(RequestMode::Direct),
        "stream" | "streaming" => Ok(RequestMode::Stream),
        other => bail!("unsupported request mode: {other}. supported modes: direct, stream"),
    }
}

fn parse_strategy_mode(value: &str) -> Result<StrategyMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(StrategyMode::Auto),
        "llm" => Ok(StrategyMode::Llm),
        "rule" => Ok(StrategyMode::Rule),
        other => bail!("unsupported strategy mode: {other}. supported modes: auto, llm, rule"),
    }
}

fn parse_promotion_trigger_mode(value: &str) -> Result<PromotionTriggerMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "immediate" | "sync" => Ok(PromotionTriggerMode::Immediate),
        "deferred" | "post_turn" => Ok(PromotionTriggerMode::Deferred),
        "window_full" | "window" | "batch" => Ok(PromotionTriggerMode::WindowFull),
        "background" | "async" | "worker" => Ok(PromotionTriggerMode::Background),
        other => bail!(
            "unsupported promotion trigger: {other}. supported modes: immediate, deferred, window_full, background"
        ),
    }
}

fn request_mode_label(mode: RequestMode) -> &'static str {
    match mode {
        RequestMode::Direct => "direct",
        RequestMode::Stream => "stream",
    }
}

fn strategy_mode_label(mode: StrategyMode) -> &'static str {
    match mode {
        StrategyMode::Auto => "auto",
        StrategyMode::Llm => "llm",
        StrategyMode::Rule => "rule",
    }
}

fn promotion_trigger_mode_label(mode: PromotionTriggerMode) -> &'static str {
    match mode {
        PromotionTriggerMode::Immediate => "immediate",
        PromotionTriggerMode::Deferred => "deferred",
        PromotionTriggerMode::WindowFull => "window_full",
        PromotionTriggerMode::Background => "background",
    }
}

fn memory_scope_label(scope: Option<&MemoryScope>) -> &'static str {
    match scope {
        Some(MemoryScope::Global) => "global",
        Some(MemoryScope::Persona) => "persona",
        Some(MemoryScope::Session) => "session",
        Some(MemoryScope::Instance) => "instance",
        Some(MemoryScope::Project) => "project",
        Some(MemoryScope::Task) => "task",
        None => "auto",
    }
}

fn parse_memory_scope(value: &str) -> Result<MemoryScope> {
    match value.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(MemoryScope::Global),
        "persona" => Ok(MemoryScope::Persona),
        "session" => Ok(MemoryScope::Session),
        "instance" => Ok(MemoryScope::Instance),
        "project" => Ok(MemoryScope::Project),
        "task" => Ok(MemoryScope::Task),
        other => bail!("unsupported scope: {other}"),
    }
}

fn parse_memory_owner_kind(value: &str) -> Result<MemoryOwnerKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(MemoryOwnerKind::Global),
        "persona" => Ok(MemoryOwnerKind::Persona),
        "session" => Ok(MemoryOwnerKind::Session),
        "instance" => Ok(MemoryOwnerKind::Instance),
        "project" => Ok(MemoryOwnerKind::Project),
        "task" => Ok(MemoryOwnerKind::Task),
        other => bail!("unsupported owner kind: {other}"),
    }
}

fn parse_memory_kind(value: &str) -> Result<MemoryKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "summary" => Ok(MemoryKind::Summary),
        "decision" => Ok(MemoryKind::Decision),
        "preference" => Ok(MemoryKind::Preference),
        "knowledge" => Ok(MemoryKind::Knowledge),
        "workflow_memory" | "workflow-memory" => Ok(MemoryKind::WorkflowMemory),
        other => bail!("unsupported memory kind: {other}"),
    }
}

fn parse_memory_layer(value: &str) -> Result<MemoryLayer> {
    match value.trim().to_ascii_lowercase().as_str() {
        "chat" => Ok(MemoryLayer::Chat),
        "topic" => Ok(MemoryLayer::Topic),
        "task" => Ok(MemoryLayer::Task),
        "project" => Ok(MemoryLayer::Project),
        "global" => Ok(MemoryLayer::Global),
        other => bail!("unsupported memory layer: {other}"),
    }
}

fn parse_memory_visibility(value: &str) -> Result<MemoryVisibility> {
    match value.trim().to_ascii_lowercase().as_str() {
        "private" => Ok(MemoryVisibility::Private),
        "tenant_shared" | "tenant-shared" => Ok(MemoryVisibility::TenantShared),
        "cross_tenant_shared" | "cross-tenant-shared" => Ok(MemoryVisibility::CrossTenantShared),
        other => bail!("unsupported memory visibility: {other}"),
    }
}

fn summarize_title_from_prompt(prompt_parts: &[String]) -> String {
    let joined = prompt_parts.join(" ").trim().to_owned();
    if joined.is_empty() {
        return "Context Memory".to_owned();
    }
    joined.chars().take(64).collect()
}

fn default_chat_room_id(namespace: &MemoryNamespace) -> String {
    format!(
        "room.chat.{}.{}.{}",
        slugify_chat_segment(&namespace.tenant_id),
        slugify_chat_segment(&namespace.user_id),
        current_timestamp_ms()
    )
}

fn create_chat_room(namespace: &MemoryNamespace, room_id: String) -> MemoryRoom {
    MemoryRoom::new(
        room_id,
        MemoryLayer::Chat,
        format!(
            "Chat Room | {} / {}",
            namespace.tenant_id, namespace.user_id
        ),
        "Interactive chat transcript and compressed reply memory.",
    )
    .with_namespace(namespace.clone())
    .with_visibility(MemoryVisibility::Private)
    .with_tag("chat")
    .with_tag("interactive")
}

fn create_context_room(
    namespace: &MemoryNamespace,
    kind: ContextRoomKind,
    room_slug: &str,
    title: &str,
) -> MemoryRoom {
    let layer = context_room_layer(kind);
    let kind_name = context_room_kind_name(kind);
    MemoryRoom::new(
        format!(
            "room.{}.{}.{}.{}",
            kind_name,
            slugify_chat_segment(&namespace.tenant_id),
            slugify_chat_segment(&namespace.user_id),
            room_slug
        ),
        layer,
        title,
        format!("Derived context room for {kind_name} memory."),
    )
    .with_namespace(namespace.clone())
    .with_visibility(MemoryVisibility::Private)
    .with_tag(kind_name)
    .with_tag("derived")
}

fn resolve_chat_room(
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    memory_namespace: &MemoryNamespace,
    explicit_room_id: Option<String>,
) -> Result<MemoryRoom> {
    if let Some(room_id) = explicit_room_id {
        return Ok(create_chat_room(memory_namespace, room_id));
    }

    if let Some(room) = find_latest_active_chat_room(root.as_ref(), workspace_namespace)? {
        return Ok(room);
    }

    Ok(create_chat_room(
        memory_namespace,
        default_chat_room_id(memory_namespace),
    ))
}

fn find_latest_active_chat_room(
    root: &Path,
    workspace_namespace: &WorkspaceNamespace,
) -> Result<Option<MemoryRoom>> {
    let chat_root = root
        .join(workspace_namespace.scoped_prefix())
        .join("memory")
        .join("rooms")
        .join("chat");
    if !chat_root.exists() {
        return Ok(None);
    }

    let repository =
        MemoryRoomRepository::with_namespace(root.to_path_buf(), workspace_namespace.clone());
    let mut latest_room: Option<(std::time::SystemTime, MemoryRoom)> = None;

    for entry in fs::read_dir(&chat_root)
        .with_context(|| format!("failed to read chat rooms under {}", chat_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let room_doc = path.join("room.md");
        if !room_doc.exists() {
            continue;
        }

        let modified_at = room_doc
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let relative = room_doc
            .strip_prefix(root.join(workspace_namespace.scoped_prefix()))
            .context("chat room path should live under namespace root")?
            .to_path_buf();
        let room = repository.read_room(relative)?;
        if room.status != "active" {
            continue;
        }

        match &latest_room {
            Some((current_modified_at, _)) if modified_at <= *current_modified_at => {}
            _ => latest_room = Some((modified_at, room)),
        }
    }

    Ok(latest_room.map(|(_, room)| room))
}

fn ensure_context_room_for_input(
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    memory_namespace: &MemoryNamespace,
    retriever: &WorkspaceMemoryRetriever,
    query: &ContextMemoryQuery,
    input: &str,
    chat_room: Option<&MemoryRoom>,
) -> Result<ContextRoomResolution> {
    if !should_materialize_context_room(input) {
        return Ok(ContextRoomResolution::None);
    }

    let candidates = retriever.discover_room_candidates(query)?;
    if let Some(candidate) = strongest_context_room_match(&candidates)
        && let Some(existing_room) =
            read_room_by_id(root.as_ref(), workspace_namespace, &candidate.room_id)?
    {
        let kind = context_room_kind_for_room(&existing_room);
        seed_context_room_with_input(
            root.as_ref(),
            workspace_namespace,
            &existing_room,
            kind,
            input,
        )?;
        let refreshed_room = refresh_context_room_metadata(
            root.as_ref(),
            workspace_namespace,
            &existing_room,
            input,
        )?;
        if let Some(chat_room) = chat_room {
            link_rooms(
                root.as_ref(),
                workspace_namespace,
                chat_room,
                &refreshed_room,
            )?;
        }
        return Ok(ContextRoomResolution::Reused(refreshed_room));
    }

    let kind = infer_context_room_kind(input);
    let slug = summarize_context_room_slug(input);
    if slug.is_empty() {
        return Ok(ContextRoomResolution::None);
    }

    if let Some(existing_room) =
        find_existing_context_room(root.as_ref(), workspace_namespace, kind, &slug)?
    {
        seed_context_room_with_input(
            root.as_ref(),
            workspace_namespace,
            &existing_room,
            kind,
            input,
        )?;
        let refreshed_room = refresh_context_room_metadata(
            root.as_ref(),
            workspace_namespace,
            &existing_room,
            input,
        )?;
        if let Some(chat_room) = chat_room {
            link_rooms(
                root.as_ref(),
                workspace_namespace,
                chat_room,
                &refreshed_room,
            )?;
        }
        return Ok(ContextRoomResolution::Reused(refreshed_room));
    }

    let room = create_context_room(
        memory_namespace,
        kind,
        &slug,
        &summarize_context_room_title(input, kind),
    );
    ensure_chat_room(root.as_ref(), workspace_namespace, &room)?;
    seed_context_room_with_input(root.as_ref(), workspace_namespace, &room, kind, input)?;
    if let Some(chat_room) = chat_room {
        link_rooms(root.as_ref(), workspace_namespace, chat_room, &room)?;
    }
    Ok(ContextRoomResolution::Created(room))
}

fn should_materialize_context_room(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }

    let lowered = trimmed.to_ascii_lowercase();
    if matches!(lowered.as_str(), "hi" | "hello" | "你好" | "嗨" | "在吗") {
        return false;
    }

    trimmed.chars().count() >= 8 || looks_like_task_input(&lowered)
}

fn strongest_context_room_match(
    candidates: &[hc_context::RoomCandidate],
) -> Option<&hc_context::RoomCandidate> {
    candidates.iter().find(|candidate| {
        candidate.layer != MemoryLayer::Chat
            && candidate.layer != MemoryLayer::Global
            && candidate.score_milli >= 720
    })
}

fn infer_context_room_kind(input: &str) -> ContextRoomKind {
    let lowered = input.to_ascii_lowercase();
    if looks_like_agent_input(&lowered) {
        ContextRoomKind::Agent
    } else if looks_like_tool_input(&lowered) {
        ContextRoomKind::Tool
    } else if looks_like_project_input(&lowered) {
        ContextRoomKind::Project
    } else if looks_like_task_input(&lowered) {
        ContextRoomKind::Task
    } else {
        ContextRoomKind::Topic
    }
}

fn looks_like_task_input(lowered: &str) -> bool {
    [
        "implement",
        "fix",
        "debug",
        "refactor",
        "test",
        "review",
        "设计",
        "实现",
        "修复",
        "调试",
        "重构",
        "测试",
        "任务",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword))
}

fn looks_like_agent_input(lowered: &str) -> bool {
    [
        "agent",
        "persona",
        "reviewer",
        "planner",
        "coder",
        "助手",
        "智能体",
        "人格",
        "角色",
        "审查者",
        "规划者",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword))
}

fn looks_like_tool_input(lowered: &str) -> bool {
    [
        "tool", "api", "git", "cargo", "minimax", "openai", "工具", "命令", "接口", "sdk",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword))
}

fn looks_like_project_input(lowered: &str) -> bool {
    [
        "project",
        "architecture",
        "convention",
        "workspace",
        "repo",
        "项目",
        "架构",
        "约定",
        "仓库",
        "工作区",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword))
}

fn summarize_context_room_slug(input: &str) -> String {
    let text = input
        .trim()
        .chars()
        .take(32)
        .collect::<String>()
        .replace('\n', " ");
    slugify_chat_segment(&text)
}

fn summarize_context_room_title(input: &str, kind: ContextRoomKind) -> String {
    let prefix = match kind {
        ContextRoomKind::Task => "Task Room",
        ContextRoomKind::Topic => "Topic Room",
        ContextRoomKind::Agent => "Agent Room",
        ContextRoomKind::Tool => "Tool Room",
        ContextRoomKind::Project => "Project Room",
    };
    format!(
        "{prefix} | {}",
        input.trim().chars().take(48).collect::<String>()
    )
}

fn context_room_layer(kind: ContextRoomKind) -> MemoryLayer {
    match kind {
        ContextRoomKind::Topic => MemoryLayer::Topic,
        ContextRoomKind::Task => MemoryLayer::Task,
        ContextRoomKind::Agent | ContextRoomKind::Tool | ContextRoomKind::Project => {
            MemoryLayer::Project
        }
    }
}

fn context_room_kind_name(kind: ContextRoomKind) -> &'static str {
    match kind {
        ContextRoomKind::Topic => "topic",
        ContextRoomKind::Task => "task",
        ContextRoomKind::Agent => "agent",
        ContextRoomKind::Tool => "tool",
        ContextRoomKind::Project => "project",
    }
}

fn context_room_kind_for_room(room: &MemoryRoom) -> ContextRoomKind {
    if room.tags.iter().any(|tag| tag == "agent") || room.id.starts_with("room.agent.") {
        ContextRoomKind::Agent
    } else if room.tags.iter().any(|tag| tag == "tool") || room.id.starts_with("room.tool.") {
        ContextRoomKind::Tool
    } else if room.tags.iter().any(|tag| tag == "project") || room.id.starts_with("room.project.") {
        ContextRoomKind::Project
    } else if room.tags.iter().any(|tag| tag == "task") || room.id.starts_with("room.task.") {
        ContextRoomKind::Task
    } else {
        ContextRoomKind::Topic
    }
}

fn read_room_by_id(
    root: &Path,
    workspace_namespace: &WorkspaceNamespace,
    room_id: &str,
) -> Result<Option<MemoryRoom>> {
    let store = WorkspaceStore::new(root.to_path_buf());
    let repository =
        MemoryRoomRepository::with_namespace(root.to_path_buf(), workspace_namespace.clone());
    let query = MarkdownQuery::default()
        .with_path_prefix("memory/rooms")
        .with_doc_type("memory_room")
        .with_id(room_id.to_owned())
        .with_limit(1);
    let Some(entry) = store
        .query_markdown_index_in_namespace(workspace_namespace, &query)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    Ok(Some(repository.read_room(entry.relative_path)?))
}

fn refresh_context_room_metadata(
    root: &Path,
    workspace_namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    input: &str,
) -> Result<MemoryRoom> {
    let repository =
        MemoryRoomRepository::with_namespace(root.to_path_buf(), workspace_namespace.clone());
    let mut updated_room = room.clone();
    updated_room.status = "active".to_owned();

    let input_snippet = input.trim().chars().take(96).collect::<String>();
    if !input_snippet.is_empty() {
        updated_room.summary = merge_context_room_summary(&updated_room.summary, &input_snippet);

        let refresh_marker = format!("refreshed:{}", slugify_chat_segment(&input_snippet));
        let activity_marker = format!(
            "last-active:{}",
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        );
        updated_room
            .derived_docs
            .retain(|item| !item.starts_with("last-active:"));
        if !refresh_marker.ends_with(':')
            && !updated_room
                .derived_docs
                .iter()
                .any(|item| item == &refresh_marker)
        {
            updated_room.derived_docs.push(refresh_marker);
            if updated_room.derived_docs.len() > 8 {
                let keep_from = updated_room.derived_docs.len().saturating_sub(8);
                updated_room.derived_docs.drain(0..keep_from);
            }
        }
        updated_room.derived_docs.push(activity_marker);
        if updated_room.derived_docs.len() > 8 {
            let keep_from = updated_room.derived_docs.len().saturating_sub(8);
            updated_room.derived_docs.drain(0..keep_from);
        }
    }

    repository.write_room(&updated_room)?;
    Ok(updated_room)
}

fn merge_context_room_summary(existing_summary: &str, input_snippet: &str) -> String {
    let existing = existing_summary.trim();
    let candidate = input_snippet.trim();
    if existing.is_empty() {
        return candidate.to_owned();
    }
    if candidate.is_empty() {
        return existing.to_owned();
    }

    let existing_norm = normalize_seed_text(existing);
    let candidate_norm = normalize_seed_text(candidate);
    if existing_norm.is_empty()
        || existing_norm == candidate_norm
        || existing_norm.contains(&candidate_norm)
    {
        return existing.chars().take(160).collect();
    }
    if candidate_norm.contains(&existing_norm) {
        return candidate.chars().take(160).collect();
    }

    let prefix = existing.chars().take(72).collect::<String>();
    let suffix = candidate.chars().take(72).collect::<String>();
    format!("{prefix} | latest: {suffix}")
        .chars()
        .take(160)
        .collect()
}

fn find_existing_context_room(
    root: &Path,
    workspace_namespace: &WorkspaceNamespace,
    kind: ContextRoomKind,
    slug: &str,
) -> Result<Option<MemoryRoom>> {
    let layer = context_room_layer(kind);
    let layer_dir = match layer {
        MemoryLayer::Chat => "chat",
        MemoryLayer::Topic => "topic",
        MemoryLayer::Task => "task",
        MemoryLayer::Project => "project",
        MemoryLayer::Global => "global",
    };
    let room_root = root
        .join(workspace_namespace.scoped_prefix())
        .join("memory")
        .join("rooms")
        .join(layer_dir);
    if !room_root.exists() {
        return Ok(None);
    }

    let kind_name = context_room_kind_name(kind);
    let repository =
        MemoryRoomRepository::with_namespace(root.to_path_buf(), workspace_namespace.clone());
    let mut best_match: Option<(u8, std::time::SystemTime, MemoryRoom)> = None;

    for entry in fs::read_dir(&room_root)
        .with_context(|| format!("failed to read context rooms under {}", room_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let room_doc = path.join("room.md");
        if !room_doc.exists() {
            continue;
        }

        let relative = room_doc
            .strip_prefix(root.join(workspace_namespace.scoped_prefix()))
            .context("context room path should live under namespace root")?
            .to_path_buf();
        let room = repository.read_room(relative)?;
        if room.status != "active" {
            continue;
        }
        if !room.tags.iter().any(|tag| tag == kind_name) {
            continue;
        }

        let room_slug = room.id.rsplit('.').next().unwrap_or_default();
        let match_rank = if room_slug == slug {
            3
        } else if room_slug.starts_with(slug) || slug.starts_with(room_slug) {
            2
        } else if room
            .title
            .to_ascii_lowercase()
            .contains(&slug.replace('.', " "))
        {
            1
        } else {
            0
        };
        if match_rank == 0 {
            continue;
        }

        let modified_at = room_doc
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match &best_match {
            Some((best_rank, best_modified_at, _))
                if match_rank < *best_rank
                    || (match_rank == *best_rank && modified_at <= *best_modified_at) => {}
            _ => best_match = Some((match_rank, modified_at, room)),
        }
    }

    Ok(best_match.map(|(_, _, room)| room))
}

fn seed_context_room_with_input(
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    kind: ContextRoomKind,
    input: &str,
) -> Result<PathBuf> {
    let summary = input.trim();
    let repository = MemoryRoomRepository::with_namespace(
        root.as_ref().to_path_buf(),
        workspace_namespace.clone(),
    );
    if should_skip_seed_write(&repository, room, summary)? {
        return Ok(MemoryRoomRepository::compressed_doc_relative_path(
            room,
            format!("seed.{}.md", slugify_chat_segment(summary)),
        ));
    }
    let memory_kind = match kind {
        ContextRoomKind::Task => MemoryKind::WorkflowMemory,
        ContextRoomKind::Topic => MemoryKind::Knowledge,
        ContextRoomKind::Agent => MemoryKind::WorkflowMemory,
        ContextRoomKind::Tool => MemoryKind::Knowledge,
        ContextRoomKind::Project => MemoryKind::Knowledge,
    };
    let owner = match kind {
        ContextRoomKind::Task => MemoryOwnerRef::task(room.id.clone()),
        ContextRoomKind::Agent => MemoryOwnerRef::persona(room.id.clone()),
        ContextRoomKind::Tool | ContextRoomKind::Project | ContextRoomKind::Topic => {
            MemoryOwnerRef::project(room.id.clone())
        }
    };
    let layer_tag = match kind {
        ContextRoomKind::Task => "task",
        ContextRoomKind::Topic => "topic",
        ContextRoomKind::Agent => "agent",
        ContextRoomKind::Tool => "tool",
        ContextRoomKind::Project => "project",
    };
    let specialization_tag = match kind {
        ContextRoomKind::Task => "workflow",
        ContextRoomKind::Topic => "knowledge",
        ContextRoomKind::Agent => "persona",
        ContextRoomKind::Tool => "integration",
        ContextRoomKind::Project => "architecture",
    };
    let file_slug = slugify_chat_segment(summary);
    let write_request = hc_context::RoomMemoryWriteRequest::new(
        room.id.clone(),
        room.layer.clone(),
        room.title.clone(),
        summary.to_owned(),
        memory_kind,
    )
    .with_visibility(MemoryVisibility::Private)
    .with_owner(owner)
    .with_tag(layer_tag)
    .with_tag(specialization_tag)
    .with_tag("seed")
    .with_file_name(format!("seed.{}.md", file_slug))
    .with_asset_id(format!("asset.{}.seed.{}", room.id, file_slug));
    persist_room_memory(root.as_ref(), workspace_namespace.clone(), &write_request)
}

fn should_skip_seed_write(
    repository: &MemoryRoomRepository,
    room: &MemoryRoom,
    summary: &str,
) -> Result<bool> {
    let target = normalize_seed_text(summary);
    if target.is_empty() {
        return Ok(true);
    }

    let assets = repository.read_compressed_assets(room)?;
    for asset in assets {
        let existing = normalize_seed_text(&asset.summary);
        if existing.is_empty() {
            continue;
        }

        if existing == target {
            return Ok(true);
        }

        if existing.contains(&target) || target.contains(&existing) {
            let shorter = existing.len().min(target.len());
            let longer = existing.len().max(target.len());
            if shorter * 10 >= longer * 8 {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn normalize_seed_text(text: &str) -> String {
    text.trim()
        .chars()
        .flat_map(|ch| ch.to_lowercase())
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .collect()
}

fn link_rooms(
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    chat_room: &MemoryRoom,
    context_room: &MemoryRoom,
) -> Result<()> {
    let repository = MemoryRoomRepository::with_namespace(
        root.as_ref().to_path_buf(),
        workspace_namespace.clone(),
    );

    let mut updated_chat_room = chat_room.clone();
    if !updated_chat_room
        .relations
        .iter()
        .any(|relation| relation.target == context_room.id)
    {
        updated_chat_room.relations.push(
            MemoryRelation::new(MemoryRelationKind::References, context_room.id.clone())
                .with_detail(format!(
                    "linked {} room",
                    format!("{:?}", context_room.layer).to_ascii_lowercase()
                )),
        );
    }
    if !updated_chat_room
        .related_entities
        .iter()
        .any(|entity| entity.id == context_room.id)
    {
        updated_chat_room
            .related_entities
            .push(room_entity_ref(context_room));
    }
    repository.write_room(&updated_chat_room)?;

    let mut updated_context_room = context_room.clone();
    if !updated_context_room
        .relations
        .iter()
        .any(|relation| relation.target == chat_room.id)
    {
        updated_context_room.relations.push(
            MemoryRelation::new(MemoryRelationKind::DerivedFrom, chat_room.id.clone())
                .with_detail("materialized from active chat room"),
        );
    }
    if !updated_context_room
        .related_entities
        .iter()
        .any(|entity| entity.id == chat_room.id)
    {
        updated_context_room
            .related_entities
            .push(room_entity_ref(chat_room));
    }
    repository.write_room(&updated_context_room)?;

    Ok(())
}

fn room_entity_ref(room: &MemoryRoom) -> MemoryEntityRef {
    let kind = match room.layer {
        MemoryLayer::Chat => MemoryEntityKind::Session,
        MemoryLayer::Topic => MemoryEntityKind::Topic,
        MemoryLayer::Task => MemoryEntityKind::Task,
        MemoryLayer::Project => MemoryEntityKind::Project,
        MemoryLayer::Global => MemoryEntityKind::Other,
    };
    MemoryEntityRef::new(kind, room.id.clone())
}

fn ensure_chat_room(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
) -> Result<()> {
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    repository.write_room(room)?;
    Ok(())
}

fn archive_chat_room(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turns: usize,
) -> Result<()> {
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let archived = room
        .clone()
        .with_status("archived")
        .with_derived_doc(format!("rolled-after-{turns}-turns"));
    repository.write_room(&archived)?;
    Ok(())
}

fn should_roll_chat_room(
    chat_room: Option<&MemoryRoom>,
    turn_index: usize,
    window_size: usize,
) -> bool {
    chat_room.is_some() && turn_index >= window_size
}

fn persist_chat_turn_user_message(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: &str,
) -> Result<()> {
    let persisted = content.trim();
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let asset_id = format!("asset.{}.turn.{}.user", room.id, turn_index);
    let asset = MemoryRoomAsset::new(
        asset_id.clone(),
        room.id.clone(),
        format!("{:04}.user-message.md", turn_index),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Raw,
        format!("User Message {}", turn_index),
        persisted,
    )
    .with_namespace(MemoryNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    ))
    .with_visibility(MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Knowledge)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_tag("chat")
    .with_tag("user");
    let _materialized = repository.materialize_asset(room, &asset)?;
    persist_chat_evolution_event(
        &repository,
        room,
        chat_event(
            &asset_id,
            &room.id,
            ArtifactEvolutionAction::Created,
            "persisted user message into chat room",
            vec!["chat", "user"],
            Vec::new(),
            Vec::new(),
        ),
    )?;
    Ok(())
}

fn persist_chat_turn_assistant_reply(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: &str,
) -> Result<()> {
    let persisted = strip_think_blocks(content).trim().to_owned();
    if persisted.is_empty() {
        return Ok(());
    }

    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let asset_id = format!("asset.{}.turn.{}.assistant", room.id, turn_index);
    let draft = ArtifactDraft::new(
        room.id.clone(),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Compressed,
        format!("Assistant Reply {}", turn_index),
        persisted,
    )
    .with_visibility(MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Summary)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_tag("chat")
    .with_tag("assistant")
    .with_file_name(format!("{:04}.assistant-reply.md", turn_index));
    let _materialized = repository.materialize_artifact_draft(room, asset_id.clone(), draft)?;
    persist_chat_evolution_event(
        &repository,
        room,
        chat_event(
            &asset_id,
            &room.id,
            ArtifactEvolutionAction::Created,
            "persisted assistant reply into chat room",
            vec!["chat", "assistant"],
            Vec::new(),
            Vec::new(),
        ),
    )?;
    Ok(())
}

fn persist_chat_turn_assistant_wenyan(
    registry: &ProviderRegistry,
    model: &ModelRef,
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: &str,
) -> Result<Option<PathBuf>> {
    let source = strip_think_blocks(content).trim().to_owned();
    if source.is_empty() {
        return Ok(None);
    }

    let generation = GenerateRequest::new(
        model.clone(),
        vec![
            ChatMessage::new(
                MessageRole::System,
                load_assistant_wenyan_prompt(namespace)?,
            ),
            ChatMessage::new(MessageRole::User, source),
        ],
    );
    let response = registry
        .generate(&generation)
        .map_err(anyhow::Error::from)?;
    let wenyan = strip_think_blocks(&response.message.content)
        .trim()
        .to_owned();
    if wenyan.is_empty() {
        return Ok(None);
    }

    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let asset_id = format!("asset.{}.turn.{}.assistant.wenyan", room.id, turn_index);
    let draft = ArtifactDraft::new(
        room.id.clone(),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Literary,
        format!("Assistant Wenyan {}", turn_index),
        wenyan,
    )
    .with_visibility(MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Summary)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_derived_from(format!("asset.{}.turn.{}.assistant", room.id, turn_index))
    .with_tag("chat")
    .with_tag("assistant")
    .with_tag("wenyan")
    .with_file_name(format!("{:04}.assistant-wenyan.md", turn_index));

    let materialized = repository.materialize_artifact_draft(room, asset_id.clone(), draft)?;
    persist_chat_evolution_event(
        &repository,
        room,
        chat_event(
            &asset_id,
            &room.id,
            ArtifactEvolutionAction::Derived,
            "derived wenyan rewrite from assistant reply",
            vec!["chat", "assistant", "wenyan"],
            vec![format!("turn:{turn_index}")],
            vec![
                "rewrite:wenyan".to_owned(),
                materialized.room_relative_path.clone(),
            ],
        ),
    )?;

    Ok(Some(materialized.path))
}

fn persist_chat_evolution_event(
    repository: &MemoryRoomRepository,
    room: &MemoryRoom,
    event: ArtifactEvolutionEvent,
) -> Result<()> {
    repository.materialize_evolution_event(room, &event)?;
    Ok(())
}

fn chat_event(
    asset_id: &str,
    room_id: &str,
    action: ArtifactEvolutionAction,
    reason: &str,
    tags: Vec<&str>,
    inputs: Vec<String>,
    outputs: Vec<String>,
) -> ArtifactEvolutionEvent {
    let event = ArtifactEvolutionEvent::new(
        format!(
            "event.{asset_id}.{}.{}",
            action_label(&action),
            current_timestamp_ms()
        ),
        asset_id.to_owned(),
        room_id.to_owned(),
        action,
        reason.to_owned(),
    )
    .with_created_at_ms(current_timestamp_ms());
    let event = tags
        .into_iter()
        .fold(event, |event, tag| event.with_tag(tag));
    let event = inputs
        .into_iter()
        .fold(event, |event, input| event.with_input(input));
    outputs
        .into_iter()
        .fold(event, |event, output| event.with_output(output))
}

fn action_label(action: &ArtifactEvolutionAction) -> &'static str {
    match action {
        ArtifactEvolutionAction::Created => "created",
        ArtifactEvolutionAction::Derived => "derived",
        ArtifactEvolutionAction::Evaluated => "evaluated",
        ArtifactEvolutionAction::Promoted => "promoted",
        ArtifactEvolutionAction::Revised => "revised",
        ArtifactEvolutionAction::Retired => "retired",
        ArtifactEvolutionAction::Superseded => "superseded",
    }
}

fn strip_think_blocks(content: &str) -> String {
    let mut output = String::new();
    let mut rest = content;
    loop {
        let Some(start) = rest.find("<think>") else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        let after_start = &rest[start + "<think>".len()..];
        let Some(end) = after_start.find("</think>") else {
            break;
        };
        rest = &after_start[end + "</think>".len()..];
    }
    output
}

fn concise_error_message(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .find(|message| !message.trim().is_empty())
        .unwrap_or_else(|| "unknown error".to_owned())
}

fn is_retryable_provider_error_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    ["http 429", "http 502", "http 503", "http 504", "http 529"]
        .iter()
        .any(|needle| lower.contains(needle))
        || lower.contains("overloaded_error")
        || lower.contains("rate limit")
        || lower.contains("please retry")
        || lower.contains("retry later")
        || lower.contains("稍后重试")
}

fn is_retryable_provider_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .map(ToString::to_string)
        .any(|message| is_retryable_provider_error_message(&message))
}

fn persist_global_promotions_for_chat_input(
    organizer: &(impl MemoryOrganizer + ?Sized),
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    memory_namespace: &MemoryNamespace,
    chat_room: &MemoryRoom,
    content: &str,
    registry: &ProviderRegistry,
    model: &ModelRef,
    summary_mode: StrategyMode,
) -> Result<()> {
    let input = MemoryOrganizationInput::new(memory_namespace.clone(), content)
        .with_room_hint(chat_room.id.clone(), MemoryLayer::Chat)
        .with_owner(MemoryOwnerRef::session(chat_room.id.clone()))
        .with_tag("chat");
    let decision = organizer.organize(&input)?;
    let summary = summarize_preference_for_promotion(registry, model, &input, summary_mode)?;
    let promotions = if decision.promotions.is_empty() {
        if summary.is_some() {
            vec![hc_context::MemoryPromotionSuggestion {
                target_layer: MemoryLayer::Global,
                target_room_id: Some(default_global_room_id(memory_namespace)),
                reason: "detected durable user preference".to_owned(),
            }]
        } else {
            Vec::new()
        }
    } else {
        decision.promotions.clone()
    };

    if promotions.is_empty() {
        return Ok(());
    }

    let Some((summary, memory_kind)) = summary else {
        return Ok(());
    };

    for promotion in &promotions {
        if promotion.target_layer != MemoryLayer::Global {
            continue;
        }
        let room_id = promotion
            .target_room_id
            .clone()
            .unwrap_or_else(|| default_global_room_id(memory_namespace));
        let file_slug = slugify_chat_segment(&summary);
        let write_request = hc_context::RoomMemoryWriteRequest::new(
            room_id.clone(),
            MemoryLayer::Global,
            "Global Preference",
            summary.clone(),
            memory_kind.clone(),
        )
        .with_visibility(MemoryVisibility::Private)
        .with_owner(MemoryOwnerRef::global())
        .with_tag("global")
        .with_tag("preference")
        .with_derived_from(chat_room.id.clone())
        .with_file_name(format!("pref.{}.md", file_slug))
        .with_asset_id(format!("asset.{}.{}", room_id, file_slug));
        let path = persist_room_memory(root.as_ref(), workspace_namespace.clone(), &write_request)?;
        print_organize_status(
            "promote",
            format!("status=saved target=global path={}", path.display()),
        );
    }

    Ok(())
}

fn flush_global_promotion_queue(
    pending: &mut VecDeque<String>,
    organizer: &(impl MemoryOrganizer + ?Sized),
    root: impl AsRef<Path>,
    workspace_namespace: &WorkspaceNamespace,
    memory_namespace: &MemoryNamespace,
    chat_room: Option<&MemoryRoom>,
    registry: &ProviderRegistry,
    model: &ModelRef,
    summary_mode: StrategyMode,
) -> Result<usize> {
    let Some(chat_room) = chat_room else {
        pending.clear();
        return Ok(0);
    };

    let mut flushed = 0usize;
    while let Some(content) = pending.front().cloned() {
        persist_global_promotions_for_chat_input(
            organizer,
            root.as_ref(),
            workspace_namespace,
            memory_namespace,
            chat_room,
            &content,
            registry,
            model,
            summary_mode,
        )?;
        pending.pop_front();
        flushed += 1;
    }

    Ok(flushed)
}

fn should_flush_promotion_queue(
    mode: PromotionTriggerMode,
    pending_len: usize,
    promotion_window_size: usize,
) -> bool {
    match mode {
        PromotionTriggerMode::Immediate => false,
        PromotionTriggerMode::Deferred => pending_len > 0,
        PromotionTriggerMode::WindowFull => pending_len >= promotion_window_size,
        PromotionTriggerMode::Background => false,
    }
}

fn start_background_memory_worker(
    root: PathBuf,
    workspace_namespace: WorkspaceNamespace,
    memory_namespace: MemoryNamespace,
    chat_room: MemoryRoom,
    provider: String,
    model: String,
    organizer_mode: StrategyMode,
    preference_summary_mode: StrategyMode,
    llm_priority_gate: Arc<LlmPriorityGate>,
) -> BackgroundMemoryWorker {
    let (sender, receiver) = mpsc::channel::<BackgroundMemoryTask>();
    emit_cli_trace(
        "background_worker",
        "start",
        Some("started"),
        "starting background memory worker",
    );
    let parent_flow_id = current_trace_context().flow_id;
    let run_id = current_run_id();
    let handle = thread::spawn(move || {
        let mut context = TraceContext::default().with_component(TRACE_COMPONENT);
        if let Some(run_id) = run_id {
            context = context.with_run_id(run_id);
        }
        if let Some(parent_flow_id) = parent_flow_id {
            context = context.with_parent_flow_id(parent_flow_id);
        }
        replace_trace_context(context);
        let registry = default_registry();
        let organizer_model = ModelRef::new(provider, model);

        while let Ok(task) = receiver.recv() {
            match task {
                BackgroundMemoryTask::GlobalPromotion { content } => {
                    let flow_id = new_trace_id("flow.background.global_promotion");
                    let _flow_guard = enter_flow_context(flow_id);
                    let _permit = llm_priority_gate.acquire_background();
                    emit_cli_trace_with_fields(
                        "background_worker",
                        "global_promotion",
                        Some("started"),
                        "running background global promotion",
                        BTreeMap::from([(
                            "content_chars".to_owned(),
                            content.chars().count().to_string(),
                        )]),
                    );
                    if priority_debug_enabled() {
                        eprint_organize_status(
                            "priority",
                            "class=background task=global_promotion action=started",
                        );
                    }
                    let organizer = build_memory_organizer(
                        &registry,
                        &organizer_model,
                        workspace_namespace.clone(),
                        organizer_mode,
                    );
                    if let Err(error) = persist_global_promotions_for_chat_input(
                        organizer.as_ref(),
                        &root,
                        &workspace_namespace,
                        &memory_namespace,
                        &chat_room,
                        &content,
                        &registry,
                        &organizer_model,
                        preference_summary_mode,
                    ) {
                        eprint_organize_status(
                            "promote",
                            format!(
                                "status=failed mode=background reason={}",
                                concise_error_message(&error)
                            ),
                        );
                    } else {
                        emit_cli_trace(
                            "background_worker",
                            "global_promotion",
                            Some("completed"),
                            "finished background global promotion",
                        );
                    }
                    if priority_debug_enabled() {
                        eprint_organize_status(
                            "priority",
                            "class=background task=global_promotion action=finished",
                        );
                    }
                }
                BackgroundMemoryTask::Literary {
                    turn_index,
                    content,
                } => {
                    let flow_id = format!("flow.background.literary.{turn_index}");
                    let _flow_guard = enter_flow_context(flow_id);
                    let _permit = llm_priority_gate.acquire_background();
                    emit_cli_trace_with_fields(
                        "background_worker",
                        "literary",
                        Some("started"),
                        "running background literary generation",
                        BTreeMap::from([("turn_index".to_owned(), turn_index.to_string())]),
                    );
                    if priority_debug_enabled() {
                        eprint_organize_status(
                            "priority",
                            "class=background task=literary action=started",
                        );
                    }
                    match persist_chat_turn_assistant_wenyan(
                        &registry,
                        &organizer_model,
                        &root,
                        &workspace_namespace,
                        &chat_room,
                        turn_index,
                        &content,
                    ) {
                        Ok(Some(_path)) => {}
                        Ok(None) => {}
                        Err(error) => eprint_organize_status(
                            "literary",
                            format!(
                                "status=failed mode=background reason={}",
                                concise_error_message(&error)
                            ),
                        ),
                    }
                    emit_cli_trace_with_fields(
                        "background_worker",
                        "literary",
                        Some("completed"),
                        "finished background literary generation",
                        BTreeMap::from([("turn_index".to_owned(), turn_index.to_string())]),
                    );
                    if priority_debug_enabled() {
                        eprint_organize_status(
                            "priority",
                            "class=background task=literary action=finished",
                        );
                    }
                }
                BackgroundMemoryTask::Shutdown => {
                    emit_cli_trace(
                        "background_worker",
                        "shutdown",
                        Some("completed"),
                        "background memory worker shutting down",
                    );
                    break;
                }
            }
        }
    });
    BackgroundMemoryWorker { sender, handle }
}

fn enqueue_background_task(
    sender: &mpsc::Sender<BackgroundMemoryTask>,
    task: BackgroundMemoryTask,
    label: &str,
) {
    emit_cli_trace(
        "background_worker",
        "enqueue",
        Some("queued"),
        format!("queued background task for {label}"),
    );
    if let Err(error) = sender.send(task) {
        eprint_organize_status(
            label,
            format!("status=failed action=enqueue reason={error}"),
        );
    }
}

fn shutdown_background_memory_worker(worker: Option<BackgroundMemoryWorker>) {
    if let Some(worker) = worker {
        emit_cli_trace(
            "background_worker",
            "shutdown",
            Some("started"),
            "requesting background worker shutdown",
        );
        let _ = worker.sender.send(BackgroundMemoryTask::Shutdown);
        let _ = worker.handle.join();
    }
}

fn flush_literary_memory_queue(
    pending: &mut VecDeque<(usize, String)>,
    registry: &ProviderRegistry,
    model: &ModelRef,
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    chat_room: Option<&MemoryRoom>,
) -> Result<usize> {
    let Some(chat_room) = chat_room else {
        pending.clear();
        return Ok(0);
    };

    let mut flushed = 0usize;
    while let Some((turn_index, content)) = pending.front().cloned() {
        match persist_chat_turn_assistant_wenyan(
            registry,
            model,
            root.as_ref(),
            namespace,
            chat_room,
            turn_index,
            &content,
        ) {
            Ok(Some(path)) => {
                print_organize_status("literary", format!("status=saved path={}", path.display()))
            }
            Ok(None) => {}
            Err(error) => eprint_organize_status(
                "literary",
                format!(
                    "status=skipped optional=true reason={}. Set HC_CONTEXT_LITERARY_MEMORY=false to disable this optional step.",
                    concise_error_message(&error)
                ),
            ),
        }
        pending.pop_front();
        flushed += 1;
    }

    Ok(flushed)
}

fn default_global_room_id(memory_namespace: &MemoryNamespace) -> String {
    format!(
        "room.global.{}.{}",
        slugify_chat_segment(&memory_namespace.tenant_id),
        slugify_chat_segment(&memory_namespace.user_id)
    )
}

fn build_memory_organizer<'a>(
    registry: &'a ProviderRegistry,
    model: &'a ModelRef,
    workspace_namespace: WorkspaceNamespace,
    mode: StrategyMode,
) -> Box<dyn MemoryOrganizer + 'a> {
    match mode {
        StrategyMode::Rule => Box::new(CompositeMemoryOrganizer::new(
            RuleBasedMemoryRoomRouter,
            RuleBasedMemoryKindResolver,
            KeywordMemoryTagSuggester,
            RuleBasedMemoryPromotionAdvisor,
        )),
        StrategyMode::Auto => {
            let tag_suggester = LlmMemoryTagSuggester::new(
                registry,
                model.clone(),
                workspace_namespace.clone(),
                KeywordMemoryTagSuggester,
            );
            let rule_organizer = CompositeMemoryOrganizer::new(
                RuleBasedMemoryRoomRouter,
                RuleBasedMemoryKindResolver,
                tag_suggester,
                RuleBasedMemoryPromotionAdvisor,
            );
            Box::new(LlmMemoryOrganizer::new(
                registry,
                model.clone(),
                workspace_namespace,
                rule_organizer,
            ))
        }
        StrategyMode::Llm => {
            let tag_suggester = LlmMemoryTagSuggester::strict(
                registry,
                model.clone(),
                workspace_namespace.clone(),
                KeywordMemoryTagSuggester,
            );
            let rule_organizer = CompositeMemoryOrganizer::new(
                RuleBasedMemoryRoomRouter,
                RuleBasedMemoryKindResolver,
                tag_suggester,
                RuleBasedMemoryPromotionAdvisor,
            );
            Box::new(LlmMemoryOrganizer::strict(
                registry,
                model.clone(),
                workspace_namespace,
                rule_organizer,
            ))
        }
    }
}

fn build_prompt_asset_synthesizer<'a>(
    registry: &'a ProviderRegistry,
    model: &'a ModelRef,
    workspace_namespace: WorkspaceNamespace,
    mode: StrategyMode,
) -> Box<dyn PromptAssetSynthesizer + 'a> {
    match mode {
        StrategyMode::Rule => Box::new(DefaultPromptAssetSynthesizer),
        StrategyMode::Auto => Box::new(LlmPromptAssetSynthesizer::new(
            registry,
            model.clone(),
            workspace_namespace.clone(),
            DefaultPromptAssetSynthesizer,
        )),
        StrategyMode::Llm => Box::new(LlmPromptAssetSynthesizer::strict(
            registry,
            model.clone(),
            workspace_namespace,
            DefaultPromptAssetSynthesizer,
        )),
    }
}

fn summarize_preference_for_promotion(
    registry: &ProviderRegistry,
    model: &ModelRef,
    input: &MemoryOrganizationInput,
    mode: StrategyMode,
) -> Result<Option<(String, MemoryKind)>> {
    match mode {
        StrategyMode::Rule => Ok(summarize_global_preference(input)),
        StrategyMode::Llm => summarize_global_preference_with_llm(registry, model, input),
        StrategyMode::Auto => match summarize_global_preference_with_llm(registry, model, input) {
            Ok(summary) => Ok(summary.or_else(|| summarize_global_preference(input))),
            Err(_) => Ok(summarize_global_preference(input)),
        },
    }
}

fn current_timestamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis()
}

fn slugify_chat_segment(value: &str) -> String {
    let mut slug = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('.') {
            slug.push('.');
        }
    }
    slug.trim_matches('.').to_owned()
}

fn render_output(text: &str, output_style: OutputStyle) -> Result<()> {
    if output_style.typewriter {
        for character in text.chars() {
            print!("{character}");
            io::stdout().flush().context("failed to flush stdout")?;
            if output_style.typewriter_delay_ms > 0 {
                thread::sleep(Duration::from_millis(output_style.typewriter_delay_ms));
            }
        }
    } else {
        print!("{text}");
        io::stdout().flush().context("failed to flush stdout")?;
    }
    Ok(())
}

fn prompt_raw(prompt: &str) -> Result<String> {
    set_active_prompt(prompt);
    print!("{prompt}");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut input = String::new();
    let result = io::stdin()
        .read_line(&mut input)
        .context("failed to read stdin");
    clear_active_prompt();
    result?;
    Ok(input)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_prompt_asset_filter_matches_title_tags_and_summary() {
        let asset = MemoryRoomAsset::new(
            "prompt.extract.semantic-tags",
            "room.project.prompt-library",
            "semantic-tags.md",
            MemoryLayer::Project,
            MemoryRoomAssetKind::Compressed,
            "Semantic Tag Suggester",
            "Infer semantic tags for reviewer and rg.",
        )
        .with_tag("managed_prompt")
        .with_tag("extract");

        assert!(managed_prompt_asset_matches_filter(&asset, "semantic"));
        assert!(managed_prompt_asset_matches_filter(&asset, "reviewer"));
        assert!(managed_prompt_asset_matches_filter(&asset, "extract"));
        assert!(!managed_prompt_asset_matches_filter(&asset, "wenyan"));
    }
}
