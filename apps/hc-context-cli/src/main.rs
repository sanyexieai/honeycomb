use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
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
    MemoryKind, MemoryLayer, MemoryNamespace, MemoryOwnerKind, MemoryOwnerRef, MemoryRoom,
    MemoryRoomAsset, MemoryRoomAssetKind, MemoryRoomRepository, MemoryScope, MemoryVisibility,
};
use hc_store::store::WorkspaceNamespace;

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
    let request = ContextRequest::new(generation)
        .with_memory_query(memory_query)
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
        println!("recalled memories:");
        if response.recalled_memories.is_empty() {
            println!("- none");
        } else {
            for memory in &response.recalled_memories {
                let room_suffix = memory
                    .room_id
                    .as_ref()
                    .map(|room_id| format!(" | room={room_id}"))
                    .unwrap_or_default();
                println!(
                    "- {} | kind={:?} | source={}{} | confidence={} | {}",
                    memory.title,
                    memory.kind,
                    memory.source_kind,
                    room_suffix,
                    memory.confidence_milli,
                    memory.summary
                );
            }
        }
    }

    if !json && let Some(path) = persisted_path {
        println!("persisted room memory: {}", path.display());
    }

    Ok(())
}

fn default_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let provider_id = default_provider();
    let api_key = env::var("HC_LLM_API_KEY")
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .ok();
    let base_url = env::var("HC_LLM_BASE_URL")
        .or_else(|_| env::var("OPENAI_BASE_URL"))
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());

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

fn print_help() -> Result<()> {
    println!("hc-context-cli");
    println!("hc-context-cli                    # start chat");
    println!("hc-context-cli chat [--provider <id>] [--model <name>] [--system <text>] [--scope <scope>] [--owner-kind <kind>] [--owner-id <id>] [--memory-kind <kind>] [--tag <tag>] [--memory-limit <n>] [--request-mode <direct|stream>] [--stream] [--direct] [--typewriter] [--no-typewriter] [--typewriter-delay-ms <n>] [--show-memory] [--chat-memory] [--no-chat-memory] [--literary-memory] [--no-literary-memory] [--chat-room-id <id>] [--organizer-mode <auto|llm|rule>] [--prompt-asset-mode <auto|llm|rule>] [--preference-summary-mode <auto|llm|rule>]");
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
        "provider={provider} model={model} request_mode={} memory_scope={} organizer={} prompt_assets={} preference_summary={} literary_memory={}",
        request_mode_label(request_mode),
        memory_scope_label(memory_query.memory_query.scope.as_ref()),
        strategy_mode_label(organizer_mode),
        strategy_mode_label(prompt_asset_mode),
        strategy_mode_label(preference_summary_mode),
        if persist_literary_memory { "on" } else { "off" },
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
    let chat_room = if persist_chat_memory {
        let room_id = chat_room_id.unwrap_or_else(|| default_chat_room_id(&memory_namespace));
        let room = MemoryRoom::new(
            room_id,
            MemoryLayer::Chat,
            format!("Chat Room | {} / {}", memory_namespace.tenant_id, memory_namespace.user_id),
            "Interactive chat transcript and compressed reply memory.",
        )
        .with_namespace(memory_namespace.clone())
        .with_visibility(MemoryVisibility::Private)
        .with_tag("chat")
        .with_tag("interactive");
        ensure_chat_room(default_workspace_root(), &workspace_namespace, &room)?;
        println!("chat_memory=on room={}", room.id);
        Some(room)
    } else {
        println!("chat_memory=off");
        None
    };
    let mut turn_index = 0usize;

    loop {
        print!("you> ");
        io::stdout().flush().context("failed to flush stdout")?;
        let input = prompt_raw("")?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed {
            "/quit" | "/exit" => break,
            "/help" => {
                println!("/help");
                println!("/clear");
                println!("/system <text>");
                println!("/quit");
                continue;
            }
            "/clear" => {
                history.clear();
                println!("history cleared");
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
        }

        if let Some(room) = &chat_room {
            persist_global_promotions_for_chat_input(
                organizer.as_ref(),
                default_workspace_root(),
                &workspace_namespace,
                &memory_namespace,
                room,
                trimmed,
                registry,
                &organizer_model,
                preference_summary_mode,
            )?;
        }

        history.push(ChatMessage::new(MessageRole::User, trimmed.to_owned()));
        let generation = GenerateRequest::new(ModelRef::new(provider.clone(), model.clone()), history.clone());
        let request = ContextRequest::new(generation)
            .with_memory_query(memory_query.clone())
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
                match persist_chat_turn_assistant_wenyan(
                    registry,
                    &organizer_model,
                    default_workspace_root(),
                    &workspace_namespace,
                    room,
                    turn_index,
                    &response.response.message.content,
                ) {
                    Ok(Some(path)) => println!("literary> persisted wenyan memory: {}", path.display()),
                    Ok(None) => {}
                    Err(error) => eprintln!("literary> skipped wenyan memory: {error}"),
                }
            }
        }

        if show_memory {
            if response.recalled_memories.is_empty() {
                println!("memory> none");
            } else {
                for memory in &response.recalled_memories {
                    println!(
                        "memory> {} | kind={:?} | source={} | {}",
                        memory.title, memory.kind, memory.source_kind, memory.summary
                    );
                }
            }
        }
    }

    Ok(())
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
        .unwrap_or(StrategyMode::Llm)
}

fn default_prompt_asset_mode() -> StrategyMode {
    env::var("HC_CONTEXT_PROMPT_ASSET_MODE")
        .ok()
        .and_then(|value| parse_strategy_mode(&value).ok())
        .unwrap_or(StrategyMode::Llm)
}

fn default_preference_summary_mode() -> StrategyMode {
    env::var("HC_CONTEXT_PREFERENCE_SUMMARY_MODE")
        .ok()
        .and_then(|value| parse_strategy_mode(&value).ok())
        .unwrap_or(StrategyMode::Llm)
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

fn ensure_chat_room(
    root: impl AsRef<Path>,
    namespace: &WorkspaceNamespace,
    room: &MemoryRoom,
) -> Result<()> {
    let repository = MemoryRoomRepository::with_namespace(root.as_ref().to_path_buf(), namespace.clone());
    repository.write_room(room)?;
    Ok(())
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

    if decision.promotions.is_empty() {
        return Ok(());
    }

    let Some((summary, memory_kind)) =
        summarize_preference_for_promotion(registry, model, &input, summary_mode)?
    else {
        return Ok(());
    };

    for promotion in &decision.promotions {
        if promotion.target_layer != MemoryLayer::Global {
            continue;
        }
        let room_id = promotion.target_room_id.clone().unwrap_or_else(|| {
            format!(
                "room.global.{}.{}",
                slugify_chat_segment(&memory_namespace.tenant_id),
                slugify_chat_segment(&memory_namespace.user_id)
            )
        });
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
