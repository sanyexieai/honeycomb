use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use hc_context::{
    CompositeMemoryOrganizer, ContextMemoryQuery, ContextRequest, DefaultContextComposer,
    DefaultPromptAssetSynthesizer, KeywordMemoryTagSuggester, LlmMemoryOrganizer,
    LlmPromptAssetSynthesizer, MemoryOrganizationInput, MemoryOrganizer, PromptAssetSynthesizer,
    PromptPolicy, RuleBasedMemoryKindResolver, RuleBasedMemoryPromotionAdvisor,
    RuleBasedMemoryRoomRouter, WorkspaceMemoryRetriever, default_workspace_root,
    generate_with_context_using_synthesizer, generate_with_context_stream_using_synthesizer,
    persist_room_memory, room_memory_write_request_from_response, summarize_global_preference,
    summarize_global_preference_with_llm, workspace_namespace_from_memory_namespace,
};
use hc_llm::{
    ChatMessage, GenerateRequest, MessageRole, ModelRef, OpenAiCompatibleProvider,
    ProviderRegistry, StreamChunk,
};
use hc_memory::{
    MemoryEntityKind, MemoryEntityRef, MemoryKind, MemoryLayer, MemoryNamespace, MemoryOwnerKind,
    MemoryOwnerRef, MemoryRelation, MemoryRelationKind, MemoryRoom, MemoryRoomAsset,
    MemoryRoomAssetKind, MemoryRoomRepository, MemoryScope, MemoryVisibility,
};
use hc_store::store::{MarkdownQuery, WorkspaceNamespace, WorkspaceStore};

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

#[derive(Debug, Clone, Copy)]
struct OutputStyle {
    typewriter: bool,
    typewriter_delay_ms: u64,
}

fn main() -> Result<()> {
    load_local_env_file()?;
    let args: Vec<String> = env::args().skip(1).collect();
    let registry = default_registry();

    if args.is_empty() {
        return handle_chat(&registry, &[]);
    }

    match args.first().map(String::as_str) {
        Some("generate") => handle_generate(&registry, &args[1..]),
        Some("chat") => handle_chat(&registry, &args[1..]),
        Some("help") | Some("--help") | Some("-h") => print_help(),
        Some(other) => bail!("unknown command: {other}"),
        None => unreachable!("args emptiness handled above"),
    }
}

fn handle_generate(registry: &ProviderRegistry, args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: hc-context-cli generate <prompt> [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--show-memory] [--json] [--prompt-asset-mode <auto|llm|rule>] [--write-room-id <id> --write-room-layer <layer>]");
    }

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
                provider = args.get(index + 1).cloned().context("missing value for --provider")?;
                index += 2;
            }
            "--model" => {
                model = args.get(index + 1).cloned().context("missing value for --model")?;
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
                    args.get(index + 1).cloned().context("missing value for --tag")?,
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
    let request = ContextRequest::new(generation)
        .with_memory_query(effective_memory_query.clone())
        .with_system_prompt(system_message.unwrap_or_else(|| {
            "Use recalled memory when it is relevant, but do not invent facts from memory that are not present.".to_owned()
        }))
        .with_prompt_policy(PromptPolicy::new(
            "Memory Usage Policy",
            "Treat recalled memory as supporting context. Prefer direct user intent when they conflict, and do not invent missing facts.",
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

    let persisted_path = if let (Some(room_id), Some(room_layer)) = (write_room_id, write_room_layer) {
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

    if !json && let Some(path) = persisted_path {
        println!("persisted room memory: {}", path.display());
    }

    Ok(())
}

fn default_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let provider_id = default_provider();
    let api_key = provider_api_key(&provider_id);
    let base_url = provider_base_url(&provider_id);

    if let Some(api_key) = api_key {
        if let Ok(provider) = OpenAiCompatibleProvider::new(
            provider_id.clone(),
            format!("{provider_id} compatible"),
            base_url,
            api_key,
        ) {
            registry.register(provider);
        }
    }

    registry
}

fn provider_api_key(provider_id: &str) -> Option<String> {
    env::var("HC_LLM_API_KEY")
        .ok()
        .or_else(|| env::var(provider_api_key_var_name(provider_id)).ok())
}

fn provider_base_url(provider_id: &str) -> String {
    env::var("HC_LLM_BASE_URL")
        .ok()
        .or_else(|| env::var(provider_base_url_var_name(provider_id)).ok())
        .unwrap_or_else(|| default_base_url_for_provider(provider_id))
}

fn provider_api_key_var_name(provider_id: &str) -> &'static str {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "minimax" => "MINIMAX_API_KEY",
        _ => "OPENAI_API_KEY",
    }
}

fn provider_base_url_var_name(provider_id: &str) -> &'static str {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "minimax" => "MINIMAX_BASE_URL",
        _ => "OPENAI_BASE_URL",
    }
}

fn default_base_url_for_provider(provider_id: &str) -> String {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "minimax" => "https://api.minimaxi.com/v1".to_owned(),
        _ => "https://api.openai.com/v1".to_owned(),
    }
}

fn print_help() -> Result<()> {
    println!("hc-context-cli");
    println!("hc-context-cli                    # start chat");
    println!("hc-context-cli chat [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--no-typewriter] [--typewriter-delay-ms <n>] [--show-memory] [--chat-memory] [--no-chat-memory] [--literary-memory] [--no-literary-memory] [--chat-room-id <id>] [--organizer-mode <auto|llm|rule>] [--prompt-asset-mode <auto|llm|rule>] [--preference-summary-mode <auto|llm|rule>] [--promotion-trigger <immediate|deferred|window_full|background>] [--promotion-window-size <n>] [--literary-trigger <immediate|deferred|window_full|background>] [--literary-window-size <n>] [--chat-room-window-size <n>]");
    println!("hc-context-cli generate <prompt> [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--typewriter-delay-ms <n>] [--show-memory] [--json] [--prompt-asset-mode <auto|llm|rule>] [--write-room-id <id> --write-room-layer <layer>]");
    Ok(())
}

fn handle_chat(registry: &ProviderRegistry, args: &[String]) -> Result<()> {
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
                    args.get(index + 1).cloned().context("missing value for --tag")?,
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

    println!("hc-context chat");
    println!(
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
    );
    println!("Type /help for commands, /quit to exit.");

    let memory_namespace = runtime_memory_namespace();
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let retriever = WorkspaceMemoryRetriever::new(
        default_workspace_root(),
        workspace_namespace.clone(),
    );
    let composer = DefaultContextComposer;
    let rule_organizer = CompositeMemoryOrganizer::new(
        RuleBasedMemoryRoomRouter,
        RuleBasedMemoryKindResolver,
        KeywordMemoryTagSuggester,
        RuleBasedMemoryPromotionAdvisor,
    );
    let organizer_model = ModelRef::new(provider.clone(), model.clone());
    let organizer = build_memory_organizer(
        registry,
        &organizer_model,
        organizer_mode,
        rule_organizer,
    );
    let prompt_asset_synthesizer =
        build_prompt_asset_synthesizer(registry, &organizer_model, prompt_asset_mode);
    let mut history = Vec::new();
    let mut chat_room = if persist_chat_memory {
        let room = resolve_chat_room(
            default_workspace_root(),
            &workspace_namespace,
            &memory_namespace,
            chat_room_id.clone(),
        )?;
        ensure_chat_room(default_workspace_root(), &workspace_namespace, &room)?;
        println!("chat_memory=on room={}", room.id);
        Some(room)
    } else {
        println!("chat_memory=off");
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
            ))
        } else {
            None
        }
    } else {
        None
    };

    loop {
        print!("you> ");
        io::stdout().flush().context("failed to flush stdout")?;
        let input = prompt_raw("")?;
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
            "/promote" => {
                if promotion_trigger == PromotionTriggerMode::Background {
                    println!("promotion> background worker is enabled; queued work drains asynchronously");
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
                println!("promotion> flushed {} pending item(s)", flushed);
                continue;
            }
            "/wenyan" => {
                if literary_trigger == PromotionTriggerMode::Background {
                    println!("literary> background worker is enabled; queued work drains asynchronously");
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
                println!("literary> flushed {} pending item(s)", flushed);
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
        if let Some(room) = &chat_room {
            persist_chat_turn_user_message(
                default_workspace_root(),
                &workspace_namespace,
                room,
                turn_index,
                trimmed,
            )?;
            if promotion_trigger == PromotionTriggerMode::Background {
                if let Some(worker) = &background_worker {
                    enqueue_background_task(
                        &worker.sender,
                        BackgroundMemoryTask::GlobalPromotion {
                            content: trimmed.to_owned(),
                        },
                        "promotion",
                    );
                }
            } else {
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
                println!(
                    "room> created {} room: {}",
                    format!("{:?}", room.layer).to_ascii_lowercase(),
                    room.id
                );
            }
            ContextRoomResolution::Reused(room) if show_memory => {
                println!(
                    "room> reused {} room: {}",
                    format!("{:?}", room.layer).to_ascii_lowercase(),
                    room.id
                );
            }
            ContextRoomResolution::Reused(_) | ContextRoomResolution::None => {}
        }
        let generation = GenerateRequest::new(ModelRef::new(provider.clone(), model.clone()), history.clone());
        let request = ContextRequest::new(generation)
            .with_memory_query(effective_memory_query.clone())
            .with_system_prompt(system_message.clone().unwrap_or_else(|| {
                "Use recalled memory when it is relevant, but do not invent facts from memory that are not present.".to_owned()
            }))
            .with_prompt_policy(PromptPolicy::new(
                "Memory Usage Policy",
                "Treat recalled memory as supporting context. Prefer direct user intent when they conflict, and do not invent missing facts.",
            ));

        print!("assistant> ");
        io::stdout().flush().context("failed to flush stdout")?;
        let response = match request_mode {
            RequestMode::Direct => {
                let response = generate_with_context_using_synthesizer(
                    registry,
                    &retriever,
                    &composer,
                    prompt_asset_synthesizer.as_ref(),
                    &request,
                )?;
                render_output(&response.response.message.content, output_style)?;
                response
            }
            RequestMode::Stream => {
                let mut callback = |chunk: StreamChunk| -> Result<(), hc_llm::LlmError> {
                    render_output(&chunk.delta, output_style)
                        .map_err(|error| hc_llm::LlmError::ProviderFailure(error.to_string()))?;
                    Ok(())
                };
                generate_with_context_stream_using_synthesizer(
                    registry,
                    &retriever,
                    &composer,
                    prompt_asset_synthesizer.as_ref(),
                    &request,
                    &mut callback,
                )?
            }
        };
        println!();
        history.push(response.response.message.clone());

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
                            println!("literary> flushed {} pending item(s)", flushed);
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
                    println!("promotion> flushed {} pending item(s)", flushed);
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
                    println!("literary> flushed {} pending item(s)", flushed);
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
                println!("chat_memory> archived room={} turns={}", room.id, turn_index);
            }

            let next_room = create_chat_room(&memory_namespace, default_chat_room_id(&memory_namespace));
            ensure_chat_room(default_workspace_root(), &workspace_namespace, &next_room)?;
            println!("chat_memory> rolled to room={}", next_room.id);
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

    Ok(())
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

fn summarize_room_kind(candidate: &hc_context::RoomCandidate) -> &'static str {
    if candidate.room_id.starts_with("room.agent.") || candidate.tags.iter().any(|tag| tag == "agent") {
        "agent"
    } else if candidate.room_id.starts_with("room.tool.") || candidate.tags.iter().any(|tag| tag == "tool") {
        "tool"
    } else if candidate.room_id.starts_with("room.project.")
        || candidate.tags.iter().any(|tag| tag == "project")
    {
        "project"
    } else if candidate.room_id.starts_with("room.task.") || candidate.tags.iter().any(|tag| tag == "task") {
        "task"
    } else if candidate.room_id.starts_with("room.topic.") || candidate.tags.iter().any(|tag| tag == "topic") {
        "topic"
    } else if candidate.room_id.starts_with("room.chat.") || candidate.tags.iter().any(|tag| tag == "chat") {
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
        } else if room_id.starts_with("room.topic.") || memory.tags.iter().any(|tag| tag == "topic") {
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
    env::var("HC_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_owned())
}

fn default_model() -> String {
    env::var("HC_LLM_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_owned())
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
        format!("Chat Room | {} / {}", namespace.tenant_id, namespace.user_id),
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

    let repository = MemoryRoomRepository::with_namespace(root.to_path_buf(), workspace_namespace.clone());
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
            link_rooms(root.as_ref(), workspace_namespace, chat_room, &refreshed_room)?;
        }
        return Ok(ContextRoomResolution::Reused(refreshed_room));
    }

    let kind = infer_context_room_kind(input);
    let slug = summarize_context_room_slug(input);
    if slug.is_empty() {
        return Ok(ContextRoomResolution::None);
    }

    if let Some(existing_room) = find_existing_context_room(
        root.as_ref(),
        workspace_namespace,
        kind,
        &slug,
    )? {
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
            link_rooms(root.as_ref(), workspace_namespace, chat_room, &refreshed_room)?;
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
    seed_context_room_with_input(
        root.as_ref(),
        workspace_namespace,
        &room,
        kind,
        input,
    )?;
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
        "tool",
        "api",
        "git",
        "cargo",
        "minimax",
        "openai",
        "工具",
        "命令",
        "接口",
        "sdk",
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
    format!("{prefix} | {}", input.trim().chars().take(48).collect::<String>())
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
    } else if room.tags.iter().any(|tag| tag == "project")
        || room.id.starts_with("room.project.")
    {
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
    if existing_norm.is_empty() || existing_norm == candidate_norm || existing_norm.contains(&candidate_norm) {
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
    let repository = MemoryRoomRepository::with_namespace(root.to_path_buf(), workspace_namespace.clone());
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
        } else if room.title.to_ascii_lowercase().contains(&slug.replace('.', " ")) {
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
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), workspace_namespace.clone());
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
    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), workspace_namespace.clone());

    let mut updated_chat_room = chat_room.clone();
    if !updated_chat_room
        .relations
        .iter()
        .any(|relation| relation.target == context_room.id)
    {
        updated_chat_room.relations.push(
            MemoryRelation::new(MemoryRelationKind::References, context_room.id.clone())
                .with_detail(format!("linked {} room", format!("{:?}", context_room.layer).to_ascii_lowercase())),
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
    let repository = MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    repository.write_room(room)?;
    Ok(())
}

fn archive_chat_room(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turns: usize,
) -> Result<()> {
    let repository = MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let archived = room
        .clone()
        .with_status("archived")
        .with_derived_doc(format!("rolled-after-{turns}-turns"));
    repository.write_room(&archived)?;
    Ok(())
}

fn should_roll_chat_room(chat_room: Option<&MemoryRoom>, turn_index: usize, window_size: usize) -> bool {
    chat_room.is_some() && turn_index >= window_size
}

fn persist_chat_turn_user_message(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: &str,
) -> Result<()> {
    let repository = MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let asset = MemoryRoomAsset::new(
        format!("asset.{}.turn.{}.user", room.id, turn_index),
        room.id.clone(),
        format!("turn.{:04}.user.md", turn_index),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Raw,
        format!("User Turn {}", turn_index),
        content.trim(),
    )
    .with_namespace(MemoryNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()))
    .with_visibility(MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Knowledge)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_tag("chat")
    .with_tag("user");
    repository.write_asset(room, &asset)?;
    Ok(())
}

fn persist_chat_turn_assistant_reply(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
    turn_index: usize,
    content: &str,
) -> Result<()> {
    let repository = MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let asset = MemoryRoomAsset::new(
        format!("asset.{}.turn.{}.assistant", room.id, turn_index),
        room.id.clone(),
        format!("turn.{:04}.assistant.md", turn_index),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Compressed,
        format!("Assistant Turn {}", turn_index),
        content.trim(),
    )
    .with_namespace(MemoryNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone()))
    .with_visibility(MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Summary)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_tag("chat")
    .with_tag("assistant");
    repository.write_asset(room, &asset)?;
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
                "Translate the assistant answer into concise classical Chinese. Return only the classical Chinese text, with no explanation.",
            ),
            ChatMessage::new(MessageRole::User, source),
        ],
    );
    let response = registry.generate(&generation).map_err(anyhow::Error::from)?;
    let wenyan = response.message.content.trim();
    if wenyan.is_empty() {
        return Ok(None);
    }

    let repository =
        MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    let asset = MemoryRoomAsset::new(
        format!("asset.{}.turn.{}.assistant.wenyan", room.id, turn_index),
        room.id.clone(),
        format!("turn.{:04}.assistant.wenyan.md", turn_index),
        MemoryLayer::Chat,
        MemoryRoomAssetKind::Literary,
        format!("Assistant Turn {} Wenyan", turn_index),
        wenyan,
    )
    .with_namespace(MemoryNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    ))
    .with_visibility(MemoryVisibility::Private)
    .with_memory_kind(MemoryKind::Summary)
    .with_owner(MemoryOwnerRef::session(room.id.clone()))
    .with_derived_from(format!("asset.{}.turn.{}.assistant", room.id, turn_index))
    .with_tag("chat")
    .with_tag("assistant")
    .with_tag("wenyan");

    Ok(Some(repository.write_asset(room, &asset)?))
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
        println!("promotion> persisted global memory: {}", path.display());
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
) -> BackgroundMemoryWorker {
    let (sender, receiver) = mpsc::channel::<BackgroundMemoryTask>();
    let handle = thread::spawn(move || {
        let registry = default_registry();
        let organizer_model = ModelRef::new(provider, model);

        while let Ok(task) = receiver.recv() {
            match task {
                BackgroundMemoryTask::GlobalPromotion { content } => {
                    let organizer = build_memory_organizer(
                        &registry,
                        &organizer_model,
                        organizer_mode,
                        CompositeMemoryOrganizer::new(
                            RuleBasedMemoryRoomRouter,
                            RuleBasedMemoryKindResolver,
                            KeywordMemoryTagSuggester,
                            RuleBasedMemoryPromotionAdvisor,
                        ),
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
                        eprintln!(
                            "promotion> background task failed: {}",
                            concise_error_message(&error)
                        );
                    }
                }
                BackgroundMemoryTask::Literary { turn_index, content } => {
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
                        Err(error) => eprintln!(
                            "literary> background task failed: {}",
                            concise_error_message(&error)
                        ),
                    }
                }
                BackgroundMemoryTask::Shutdown => break,
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
    if let Err(error) = sender.send(task) {
        eprintln!("{label}> failed to enqueue background task: {error}");
    }
}

fn shutdown_background_memory_worker(worker: Option<BackgroundMemoryWorker>) {
    if let Some(worker) = worker {
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
            Ok(Some(path)) => println!("literary> persisted wenyan memory: {}", path.display()),
            Ok(None) => {}
            Err(error) => eprintln!(
                "literary> skipped wenyan memory: {}. Set HC_CONTEXT_LITERARY_MEMORY=false to disable this optional step.",
                concise_error_message(&error)
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
    mode: StrategyMode,
    rule_organizer: CompositeMemoryOrganizer<
        RuleBasedMemoryRoomRouter,
        RuleBasedMemoryKindResolver,
        KeywordMemoryTagSuggester,
        RuleBasedMemoryPromotionAdvisor,
    >,
) -> Box<dyn MemoryOrganizer + 'a> {
    match mode {
        StrategyMode::Rule => Box::new(rule_organizer),
        StrategyMode::Auto => {
            Box::new(LlmMemoryOrganizer::new(registry, model.clone(), rule_organizer))
        }
        StrategyMode::Llm => Box::new(LlmMemoryOrganizer::strict(
            registry,
            model.clone(),
            rule_organizer,
        )),
    }
}

fn build_prompt_asset_synthesizer<'a>(
    registry: &'a ProviderRegistry,
    model: &'a ModelRef,
    mode: StrategyMode,
) -> Box<dyn PromptAssetSynthesizer + 'a> {
    match mode {
        StrategyMode::Rule => Box::new(DefaultPromptAssetSynthesizer),
        StrategyMode::Auto => Box::new(LlmPromptAssetSynthesizer::new(
            registry,
            model.clone(),
            DefaultPromptAssetSynthesizer,
        )),
        StrategyMode::Llm => Box::new(LlmPromptAssetSynthesizer::strict(
            registry,
            model.clone(),
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
    print!("{prompt}");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read stdin")?;
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

    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
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
