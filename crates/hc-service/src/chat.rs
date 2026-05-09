use anyhow::{Context, Result, anyhow, bail};
use hc_agent::{
    AgentKind, AgentProfile, AgentRepository, DomainKind, DomainProfile, DomainRepository,
    append_implicit_intent_dedupe_record, append_routing_binding_log_line,
    build_routing_binding_log_line_v1_headless_from_snapshot, ensure_http_implicit_task_plan_stub,
    format_execution_results_digest_for_http_l23, format_plan_notes_digest_for_http_l23,
    format_review_notes_digest_for_http_l23, http_l2l3_planner_steering_enabled_from_env,
    load_implicit_intent_dedupe_keys, load_task_execution_result_artifacts_v1,
    load_task_plan_note_artifacts_v1, load_task_review_note_artifacts_v1,
    maybe_apply_http_l2l3_planner_steering, persist_http_chat_l23_degenerate_claim_assign,
    read_task_artifact, swarm_routing,
};
use hc_bootstrap::wall_clock_ms;
use hc_context::{
    ContextMemoryQuery, ContextRequest, DefaultContextComposer, MemoryKind, MemoryNamespace,
    MemoryScope, PromptPolicy, WorkspaceMemoryRetriever, generate_with_context,
    generate_with_context_stream, load_context_memory_system_prompt,
    load_context_memory_usage_policy_prompt, memory_kind_label, memory_scope_label,
    runtime::{RuntimeIdentity, default_session_id, runtime_identity_prompt},
    workspace_namespace_from_memory_namespace,
};
use hc_llm::{
    ChatMessage, GenerateRequest, LlmError, MessageRole, ModelRef, StreamChunk,
    default_model_from_env, default_provider_from_env, default_registry_from_env,
};
use hc_protocol::{
    AgentRouteRequest, ApiChatMessage, ApiMemoryQuery, ApiMessageRole, ApiNamespace, ChatRequest,
    ChatResponse, MemoryRef,
    swarm::{
        ImplicitIntentDedupeKey, ImplicitIntentDedupeRecord, RoutingTier,
        SwarmRoutingBindingSnapshot, TaskBindingAction,
    },
};
use hc_store::{store::WorkspaceNamespace, task_coordination::task_plan_markdown_relative};
use serde::{Deserialize, Serialize};

use crate::{
    ServiceConfig,
    agent::route_agent,
    room_routing::{
        RoomRoutingContext, resolve_room_routing_context, task_id_hint_from_room_routing,
    },
    session_swarm_state::{
        persist_session_swarm_active_task_binding, persisted_conversation_active_task_hint,
        swarm_session_key,
    },
};

const TASK_PLAN_L23_PROMPT_EXCERPT_MAX_CHARS: usize = 12_000;

/// Short ADR-005 outward-speaker cue for **`hc-service` HTTP** L2/L3 turns (orchestration still owns full consolidate).
const ADR005_HTTP_L23_SPEAKER_APPENDIX: &str = r#"

---
## Outward speaker (ADR-005 P0, HTTP L2/L3)
- **Public voice**: answer as the **task planner** (same runtime consolidator identity in P0; no separate outward consolidator agent).
- **Consistency**: aim for **one coherent** user-visible reply per turn; when several internal execution threads or reviews apply, **summarize briefly** rather than dumping raw shards.
"#;

/// Swarm `routing_message_id` prefix for **`/chat`** → [`prepare_chat_request`] (`{prefix}.{wall_clock_ms}`).
pub(crate) const SWARM_MESSAGE_ID_CHAT_API: &str = "chat.api";
/// Same prepare path when **[`crate::turn::handle_turn_request`] chooses `ChatFallback`** (distinct ingress from [`SWARM_MESSAGE_ID_CHAT_API`]).
pub(crate) const SWARM_MESSAGE_ID_TURN_CHAT_FALLBACK: &str = "turn.chat_fallback";

fn truncate_utf8_chars(body: &str, max_chars: usize) -> String {
    let count = body.chars().count();
    if count <= max_chars {
        return body.to_owned();
    }
    let head: String = body.chars().take(max_chars).collect();
    format!("{head}\n\n[... truncated from {count} graphemes ...]")
}

fn try_task_plan_markdown_excerpt_for_l23_prompt(
    workspace_root: impl AsRef<std::path::Path>,
    namespace: &WorkspaceNamespace,
    task_id: &str,
) -> Option<String> {
    let rel = task_plan_markdown_relative(task_id);
    let doc = read_task_artifact(workspace_root, namespace, &rel).ok()?;
    let body = doc.body.trim();
    if body.is_empty() {
        return None;
    }
    let excerpt = truncate_utf8_chars(body, TASK_PLAN_L23_PROMPT_EXCERPT_MAX_CHARS);
    Some(format!(
        "\n\n---\n## Coordination task plan (read-only excerpt)\n\
        Task `{task_id}`. Persisted **`coordination/**/task_plan.md`** snapshot for this L2/L3 HTTP turn; runtime state may advance beyond this file.\n\n{excerpt}\n",
    ))
}

pub fn handle_chat_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
) -> Result<ChatResponse> {
    execute_handle_chat_generate(
        config,
        request,
        room_routing_cache,
        SWARM_MESSAGE_ID_CHAT_API,
    )
}

/// Turn router delegated to chat LLM (same prompt/memory stack as `/chat`; own swarm **`routing_message_id`** prefix).
pub(crate) fn handle_turn_router_chat_fallback_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
) -> Result<ChatResponse> {
    execute_handle_chat_generate(
        config,
        request,
        room_routing_cache,
        SWARM_MESSAGE_ID_TURN_CHAT_FALLBACK,
    )
}

fn execute_handle_chat_generate(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
    swarm_observability_message_id_prefix: &str,
) -> Result<ChatResponse> {
    let prepared = prepare_chat_request(
        config,
        request,
        room_routing_cache.as_ref(),
        swarm_observability_message_id_prefix,
    )?;
    let registry = default_registry_from_env();
    let retriever =
        WorkspaceMemoryRetriever::new(&config.workspace_root, prepared.workspace_namespace.clone());
    let composer = DefaultContextComposer;
    let response =
        generate_with_context(&registry, &retriever, &composer, &prepared.context_request)?;

    if let Err(error) = maybe_persist_http_chat_l23_degenerate_claim_assign(
        config,
        &prepared,
        room_routing_cache.as_ref(),
    ) {
        tracing::warn!(
            ?error,
            "HTTP chat: optional L2/L3 degenerate claim→assign coordination persist"
        );
    }

    Ok(chat_response_from_context_response(
        response,
        &prepared.request,
        prepared.agent_context.as_ref(),
        prepared.binding_active_task_id.clone(),
    ))
}

#[derive(Debug, Clone)]
pub struct ChatAgentSelection {
    pub selected_agent_id: Option<String>,
    pub selected_domain_id: Option<String>,
    pub reasoning: String,
}

pub fn resolve_chat_agent_selection(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<ChatAgentSelection> {
    if let Some(agent_id) = request.agent_id.as_deref() {
        return Ok(ChatAgentSelection {
            selected_agent_id: Some(agent_id.to_owned()),
            selected_domain_id: request.domain_id.clone(),
            reasoning: format!("chat request explicitly selected agent {agent_id}"),
        });
    }

    let input = routing_input(request)?;
    let route = route_agent(
        config,
        AgentRouteRequest {
            input,
            namespace: request.memory.namespace.clone(),
            project_id: None,
            domain_id: request.domain_id.clone(),
            active_agent_id: request.active_agent_id.clone(),
            active_task_id: request.active_task_id.clone(),
            active_work_item_id: request.active_work_item_id.clone(),
            limit: Some(1),
        },
    )?;

    let reasoning = if let Some(candidate) = route.candidates.first() {
        if candidate.reasons.is_empty() {
            format!("chat fallback selected agent {}", candidate.agent_id)
        } else {
            format!(
                "chat fallback selected agent {} via {}",
                candidate.agent_id,
                candidate.reasons.join(", ")
            )
        }
    } else {
        "chat fallback found no agent candidate".to_owned()
    };

    Ok(ChatAgentSelection {
        selected_agent_id: route.selected_agent_id,
        selected_domain_id: route.selected_domain_id,
        reasoning,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatStreamEvent {
    Started {
        tenant_id: String,
        user_id: String,
        session_id: String,
    },
    Delta {
        delta: String,
        finish_reason: Option<String>,
    },
    Completed {
        response: ChatResponse,
    },
}

pub fn handle_chat_stream_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
    on_event: &mut dyn FnMut(ChatStreamEvent) -> Result<()>,
) -> Result<ChatResponse> {
    execute_handle_chat_stream(
        config,
        request,
        room_routing_cache,
        SWARM_MESSAGE_ID_CHAT_API,
        on_event,
    )
}

pub(crate) fn handle_turn_router_chat_fallback_stream_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
    on_event: &mut dyn FnMut(ChatStreamEvent) -> Result<()>,
) -> Result<ChatResponse> {
    execute_handle_chat_stream(
        config,
        request,
        room_routing_cache,
        SWARM_MESSAGE_ID_TURN_CHAT_FALLBACK,
        on_event,
    )
}

fn execute_handle_chat_stream(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
    swarm_observability_message_id_prefix: &str,
    on_event: &mut dyn FnMut(ChatStreamEvent) -> Result<()>,
) -> Result<ChatResponse> {
    let prepared = prepare_chat_request(
        config,
        request,
        room_routing_cache.as_ref(),
        swarm_observability_message_id_prefix,
    )?;
    on_event(ChatStreamEvent::Started {
        tenant_id: prepared.request.memory.namespace.tenant_id.clone(),
        user_id: prepared.request.memory.namespace.user_id.clone(),
        session_id: prepared.request.session_id.clone().unwrap_or_else(|| {
            default_session_id(
                &prepared.request.memory.namespace.tenant_id,
                &prepared.request.memory.namespace.user_id,
            )
        }),
    })?;

    let registry = default_registry_from_env();
    let retriever =
        WorkspaceMemoryRetriever::new(&config.workspace_root, prepared.workspace_namespace.clone());
    let composer = DefaultContextComposer;
    let mut stream_chunk = |chunk: StreamChunk| -> std::result::Result<(), LlmError> {
        on_event(ChatStreamEvent::Delta {
            delta: chunk.delta,
            finish_reason: chunk.finish_reason.map(|reason| format!("{reason:?}")),
        })
        .map_err(|error| LlmError::ProviderFailure(error.to_string()))
    };
    let response = generate_with_context_stream(
        &registry,
        &retriever,
        &composer,
        &prepared.context_request,
        &mut stream_chunk,
    )?;
    let response = chat_response_from_context_response(
        response,
        &prepared.request,
        prepared.agent_context.as_ref(),
        prepared.binding_active_task_id.clone(),
    );
    if let Err(error) = maybe_persist_http_chat_l23_degenerate_claim_assign(
        config,
        &prepared,
        room_routing_cache.as_ref(),
    ) {
        tracing::warn!(
            ?error,
            "HTTP chat stream: optional L2/L3 degenerate claim→assign coordination persist"
        );
    }

    on_event(ChatStreamEvent::Completed {
        response: response.clone(),
    })?;
    Ok(response)
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedChatRequest {
    request: ChatRequest,
    workspace_namespace: WorkspaceNamespace,
    agent_context: Option<ResolvedAgentContext>,
    context_request: ContextRequest,
    binding_active_task_id: Option<String>,
}

fn maybe_persist_http_chat_l23_degenerate_claim_assign(
    config: &ServiceConfig,
    prepared: &PreparedChatRequest,
    room_routing_cache: Option<&RoomRoutingContext>,
) -> Result<()> {
    let Ok(user_text) = routing_input(&prepared.request) else {
        return Ok(());
    };
    let fall_back =
        swarm_task_room_fallback_for_chat(config, &prepared.request, room_routing_cache);
    let persisted_hint = persisted_conversation_active_task_hint(
        &config.workspace_root,
        &prepared.workspace_namespace,
        &prepared.request,
    );
    let conversation_active_for_swarm = prepared
        .binding_active_task_id
        .clone()
        .or_else(|| prepared.request.active_task_id.clone())
        .or(persisted_hint);
    let snapshot = swarm_routing::classify_swarm_snapshot_for_chat_input(
        user_text.as_str(),
        conversation_active_for_swarm.as_deref(),
        fall_back.as_deref(),
    );
    let tier = snapshot.routing.routing_tier;
    let Some(task_id) = prepared.binding_active_task_id.as_deref() else {
        return Ok(());
    };
    if task_id.trim().is_empty() {
        return Ok(());
    }
    let Some(agent) = prepared.agent_context.as_ref() else {
        return Ok(());
    };
    let name = agent.agent.name.trim();
    let display_name = if name.is_empty() {
        agent.agent.id.as_str()
    } else {
        name
    };
    persist_http_chat_l23_degenerate_claim_assign(
        &config.workspace_root,
        &prepared.workspace_namespace,
        tier,
        task_id,
        agent.agent.id.as_str(),
        display_name,
        prepared.request.active_work_item_id.as_deref(),
    )?;
    Ok(())
}

fn prepare_chat_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<&RoomRoutingContext>,
    swarm_observability_message_id_prefix: &str,
) -> Result<PreparedChatRequest> {
    prepare_chat_request_with_swarm_clock(
        config,
        request,
        room_routing_cache,
        swarm_observability_message_id_prefix,
        wall_clock_ms(),
    )
}

/// Like [`prepare_chat_request`], but uses a caller-supplied **`swarm_created_at_ms`** so HTTP implicit
/// task ids (`task.http.implicit.{ms}`), routing **`message_id`**, and JSONL timestamps stay deterministic (tests).
pub(crate) fn prepare_chat_request_with_swarm_clock(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<&RoomRoutingContext>,
    swarm_observability_message_id_prefix: &str,
    swarm_created_at_ms: u64,
) -> Result<PreparedChatRequest> {
    let request = normalize_chat_request(request);
    let memory_namespace = memory_namespace_from_api(&request.memory.namespace);
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let fallback_owned = if room_routing_cache.is_none() {
        resolve_room_routing_context(config, &request)
            .ok()
            .flatten()
    } else {
        None
    };
    let room_routing = room_routing_cache.or_else(|| fallback_owned.as_ref());
    let agent_context = resolve_agent_context(config, &request)?;
    let model = ModelRef::new(
        request
            .provider
            .clone()
            .unwrap_or_else(default_provider_from_env),
        request.model.clone().unwrap_or_else(default_model_from_env),
    );
    let messages = request_messages(&request)?;
    let user_text = routing_input(&request)?;
    let fall_back = swarm_task_room_fallback_for_chat(config, &request, room_routing);
    let persisted_hint = persisted_conversation_active_task_hint(
        &config.workspace_root,
        &workspace_namespace,
        &request,
    );
    let conversation_active_for_swarm = request.active_task_id.clone().or(persisted_hint);
    let snapshot = swarm_routing::classify_swarm_snapshot_for_chat_input(
        &user_text,
        conversation_active_for_swarm.as_deref(),
        fall_back.as_deref(),
    );
    let http_implicit_allocated = emit_swarm_observability_from_classified(
        config,
        &workspace_namespace,
        &request,
        swarm_observability_message_id_prefix,
        &user_text,
        &snapshot,
        fall_back.as_deref(),
        agent_context
            .as_ref()
            .map(|resolved| resolved.agent.id.as_str()),
        swarm_created_at_ms,
    )?;

    if http_l2l3_planner_steering_enabled_from_env()
        && matches!(
            snapshot.routing.routing_tier,
            RoutingTier::L2 | RoutingTier::L3
        )
    {
        let task_anchor = http_implicit_allocated
            .as_deref()
            .or(snapshot.task_binding.active_task_id.as_deref())
            .or(request.active_task_id.as_deref())
            .or(fall_back.as_deref());
        if let Some(tid) = task_anchor.map(str::trim).filter(|s| !s.is_empty()) {
            if let Err(error) = maybe_apply_http_l2l3_planner_steering(
                config.workspace_root.as_path(),
                &workspace_namespace,
                tid,
                user_text.as_str(),
            ) {
                tracing::warn!(
                    ?error,
                    task_id = %tid,
                    "HTTP L2/L3 planner steering failed; continuing with pre-steering task plan"
                );
            }
        }
    }

    let mut generation = GenerateRequest::new(model, messages);
    generation.temperature = request.temperature;
    generation.max_output_tokens = request.max_output_tokens;

    // Same-turn HTTP implicit task id must participate here; `snapshot` still has
    // `active_task_id: None` when `CreateImplicitTask` (ADR-004 Phase 2 memory default).
    let memory_active_task_id = http_implicit_allocated
        .as_deref()
        .or(conversation_active_for_swarm.as_deref());
    let swarm_memory_hint = request.memory.scope.is_none().then(|| {
        swarm_memory_scope_hint_from_snapshot(
            &snapshot,
            memory_active_task_id,
            fall_back.as_deref(),
        )
    });
    let memory_query = build_memory_query(
        memory_namespace,
        &request.memory,
        request.input.clone(),
        swarm_memory_hint,
    )?;
    let base_system_prompt = match request.system_prompt.clone() {
        Some(system_prompt) if !system_prompt.trim().is_empty() => system_prompt,
        _ => load_context_memory_system_prompt(&workspace_namespace)?,
    };
    let mut system_prompt = compose_agent_system_prompt(
        append_runtime_identity_prompt(base_system_prompt, &request),
        agent_context.as_ref(),
    );

    let task_anchor_for_plan = http_implicit_allocated
        .as_deref()
        .or(snapshot.task_binding.active_task_id.as_deref())
        .or(request.active_task_id.as_deref())
        .or(fall_back.as_deref());

    if matches!(
        snapshot.routing.routing_tier,
        RoutingTier::L2 | RoutingTier::L3
    ) {
        if let Some(task_id) = task_anchor_for_plan {
            if let Some(block) = try_task_plan_markdown_excerpt_for_l23_prompt(
                &config.workspace_root,
                &workspace_namespace,
                task_id,
            ) {
                tracing::debug!(
                    task_id = %task_id,
                    routing_tier = %snapshot.routing.routing_tier,
                    "HTTP chat L2/L3: appended persisted task_plan excerpt to system prompt"
                );
                system_prompt.push_str(&block);
            }

            match load_task_execution_result_artifacts_v1(
                config.workspace_root.as_path(),
                &workspace_namespace,
                task_id,
            ) {
                Ok(artifacts) if !artifacts.is_empty() => {
                    let digest = format_execution_results_digest_for_http_l23(&artifacts);
                    if !digest.is_empty() {
                        tracing::debug!(
                            task_id = %task_id,
                            count = artifacts.len(),
                            "HTTP chat L2/L3: appended execution_result digest to system prompt"
                        );
                        system_prompt.push_str(&digest);
                    }
                }
                Err(error) => tracing::debug!(
                    ?error,
                    task_id = %task_id,
                    "load task execution_result artifacts for L2/L3 prompt"
                ),
                _ => {}
            }

            match load_task_plan_note_artifacts_v1(
                config.workspace_root.as_path(),
                &workspace_namespace,
                task_id,
            ) {
                Ok(artifacts) if !artifacts.is_empty() => {
                    let digest = format_plan_notes_digest_for_http_l23(&artifacts);
                    if !digest.is_empty() {
                        tracing::debug!(
                            task_id = %task_id,
                            count = artifacts.len(),
                            "HTTP chat L2/L3: appended plan_note digest to system prompt"
                        );
                        system_prompt.push_str(&digest);
                    }
                }
                Err(error) => tracing::debug!(
                    ?error,
                    task_id = %task_id,
                    "load task plan_note artifacts for L2/L3 prompt"
                ),
                _ => {}
            }

            match load_task_review_note_artifacts_v1(
                config.workspace_root.as_path(),
                &workspace_namespace,
                task_id,
            ) {
                Ok(artifacts) if !artifacts.is_empty() => {
                    let digest = format_review_notes_digest_for_http_l23(&artifacts);
                    if !digest.is_empty() {
                        tracing::debug!(
                            task_id = %task_id,
                            count = artifacts.len(),
                            "HTTP chat L2/L3: appended review_note digest to system prompt"
                        );
                        system_prompt.push_str(&digest);
                    }
                }
                Err(error) => tracing::debug!(
                    ?error,
                    task_id = %task_id,
                    "load task review_note artifacts for L2/L3 prompt"
                ),
                _ => {}
            }
        }

        system_prompt.push_str(ADR005_HTTP_L23_SPEAKER_APPENDIX);
    }

    let context_request = ContextRequest::new(generation)
        .with_memory_query(memory_query)
        .with_system_prompt(system_prompt)
        .with_prompt_policy(PromptPolicy::new(
            "Memory Usage Policy",
            load_context_memory_usage_policy_prompt(&workspace_namespace)?,
        ));

    Ok(PreparedChatRequest {
        request,
        workspace_namespace,
        agent_context,
        context_request,
        binding_active_task_id: http_implicit_allocated
            .or_else(|| snapshot.task_binding.active_task_id.clone()),
    })
}

fn swarm_task_room_fallback_for_chat(
    config: &ServiceConfig,
    request: &ChatRequest,
    pre_resolved_room_routing: Option<&RoomRoutingContext>,
) -> Option<String> {
    match pre_resolved_room_routing {
        Some(ctx) => task_id_hint_from_room_routing(ctx),
        None => task_scope_fallback_from_task_layer_room(config, request),
    }
}

/// When `room_id` refers to a **task-layer** room, its id is used as [`decide_task_binding`] fall-back
/// (ADR-004), matching UI `task_scope` hints when the client omits `active_task_id`.
fn task_scope_fallback_from_task_layer_room(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Option<String> {
    let ctx = resolve_room_routing_context(config, request)
        .ok()
        .flatten()?;
    task_id_hint_from_room_routing(&ctx)
}

/// Same swarm routing/binding trace as UI; optional `coordination/<task>.routing.jsonl` when we can
/// resolve a task id (`active_task_id` or task-layer `room_id`, same path convention as UI).
///
/// `message_id_prefix`: e.g. `chat.api`, `turn.chat_fallback` (turn router ChatFallback), `turn.api`
/// ([`emit_swarm_observability_for_chat_like_request`]), `cli.chat` — final id is `{prefix}.{wall_clock_ms}`.
/// `pre_resolved_room_routing`: when the caller already called [`resolve_room_routing_context`],
/// pass it here so task binding / coordination do not load the room twice (non-task rooms imply no
/// task id hint without a second read).
pub fn emit_swarm_observability_for_chat_like_request(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
    request: &ChatRequest,
    message_id_prefix: &str,
    pre_resolved_room_routing: Option<&RoomRoutingContext>,
) {
    let Ok(user_text) = routing_input(request) else {
        return;
    };
    let fall_back = swarm_task_room_fallback_for_chat(config, request, pre_resolved_room_routing);
    let persisted_hint = persisted_conversation_active_task_hint(
        &config.workspace_root,
        workspace_namespace,
        request,
    );
    let conversation_active_for_swarm = request.active_task_id.clone().or(persisted_hint);
    let snapshot = swarm_routing::classify_swarm_snapshot_for_chat_input(
        &user_text,
        conversation_active_for_swarm.as_deref(),
        fall_back.as_deref(),
    );
    let created_at_ms = wall_clock_ms();
    if let Err(error) = emit_swarm_observability_from_classified(
        config,
        workspace_namespace,
        request,
        message_id_prefix,
        &user_text,
        &snapshot,
        fall_back.as_deref(),
        None,
        created_at_ms,
    ) {
        tracing::warn!(
            ?error,
            prefix = %message_id_prefix,
            "emit swarm observability from classified snapshot"
        );
    }
}

fn emit_swarm_observability_from_classified(
    config: &ServiceConfig,
    workspace_namespace: &WorkspaceNamespace,
    request: &ChatRequest,
    message_id_prefix: &str,
    user_text: &str,
    snapshot: &SwarmRoutingBindingSnapshot,
    fall_back: Option<&str>,
    selected_agent_id: Option<&str>,
    created_at_ms: u64,
) -> Result<Option<String>> {
    let tenant_raw = request.memory.namespace.tenant_id.trim();
    let user_raw = request.memory.namespace.user_id.trim();
    let tenant_eff = if tenant_raw.is_empty() {
        hc_context::runtime::DEFAULT_TENANT_ID
    } else {
        tenant_raw
    };
    let user_eff = if user_raw.is_empty() {
        hc_context::runtime::DEFAULT_USER_ID
    } else {
        user_raw
    };
    let session_id = request
        .session_id
        .as_ref()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_session_id(tenant_eff, user_eff));
    let session_key = swarm_session_key(request);
    let message_id = format!("{message_id_prefix}.{created_at_ms}");

    let l23_implicit = matches!(
        snapshot.routing.routing_tier,
        RoutingTier::L2 | RoutingTier::L3
    ) && snapshot.task_binding.task_binding_action
        == TaskBindingAction::CreateImplicitTask;
    let http_implicit_task_id: Option<String> = if l23_implicit {
        let provisional = format!("task.http.implicit.{created_at_ms}");
        let dedupe_key = ImplicitIntentDedupeKey::from_trigger(&session_id, &message_id, user_text);
        let existing = load_implicit_intent_dedupe_keys(
            &config.workspace_root,
            workspace_namespace,
            &provisional,
        )?;
        if !existing.contains(&dedupe_key) {
            let record = ImplicitIntentDedupeRecord::from_key(&dedupe_key, created_at_ms);
            append_implicit_intent_dedupe_record(
                &config.workspace_root,
                workspace_namespace,
                &provisional,
                &record,
            )?;
        }
        Some(provisional)
    } else {
        None
    };

    if let Some(ref provisional) = http_implicit_task_id {
        if let Err(error) = ensure_http_implicit_task_plan_stub(
            &config.workspace_root,
            workspace_namespace,
            provisional,
            user_text,
            message_id.as_str(),
            session_id.as_str(),
        ) {
            tracing::warn!(
                ?error,
                task_id = %provisional,
                prefix = %message_id_prefix,
                "materialize HTTP implicit task_plan stub"
            );
        }
    }

    let persist_binding = http_implicit_task_id
        .clone()
        .or_else(|| snapshot.task_binding.active_task_id.clone());

    if let Err(error) = persist_session_swarm_active_task_binding(
        &config.workspace_root,
        workspace_namespace,
        &session_key,
        persist_binding.as_deref(),
    ) {
        tracing::warn!(
            ?error,
            prefix = %message_id_prefix,
            session_key = %session_key,
            "persist session swarm active_task_id"
        );
    }

    let coordination_task_id = http_implicit_task_id
        .as_deref()
        .or(snapshot.task_binding.active_task_id.as_deref())
        .or(request.active_task_id.as_deref())
        .or(fall_back);

    let mut binding_for_emit = snapshot.task_binding.clone();
    if let Some(ref id) = http_implicit_task_id {
        binding_for_emit.active_task_id = Some(id.clone());
    }
    swarm_routing::emit_swarm_message_routing(
        &snapshot.routing,
        &binding_for_emit,
        &message_id,
        &session_id,
    );
    swarm_routing::emit_http_chat_single_agent_execute_degenerate_trace(
        &message_id,
        &session_id,
        &snapshot.routing,
        selected_agent_id,
    );
    swarm_routing::emit_http_chat_create_implicit_task_binding_trace(
        &message_id,
        &session_id,
        &snapshot.routing,
        &snapshot.task_binding,
        http_implicit_task_id.as_deref(),
    );
    if let Some(task_id) = coordination_task_id {
        let coord_snap = if http_implicit_task_id.is_some() {
            SwarmRoutingBindingSnapshot::new(snapshot.routing.clone(), binding_for_emit.clone())
        } else {
            snapshot.clone()
        };
        let line = build_routing_binding_log_line_v1_headless_from_snapshot(
            created_at_ms,
            message_id.clone(),
            session_id,
            task_id,
            user_text,
            coord_snap,
        );
        if let Err(error) = append_routing_binding_log_line(
            &config.workspace_root,
            workspace_namespace,
            task_id,
            &line,
        ) {
            tracing::warn!(
                ?error,
                task_id = %task_id,
                prefix = %message_id_prefix,
                "append routing binding coordination log (HTTP service)"
            );
        }
    }

    Ok(http_implicit_task_id)
}

/// Store namespace derived from [`ChatRequest`]; fills default tenant/user when absent (matches
/// [`normalize_chat_request`] defaults).
pub fn workspace_namespace_from_chat_request(request: &ChatRequest) -> WorkspaceNamespace {
    let tenant_id = if request.memory.namespace.tenant_id.trim().is_empty() {
        hc_context::runtime::DEFAULT_TENANT_ID.to_owned()
    } else {
        request.memory.namespace.tenant_id.clone()
    };
    let user_id = if request.memory.namespace.user_id.trim().is_empty() {
        hc_context::runtime::DEFAULT_USER_ID.to_owned()
    } else {
        request.memory.namespace.user_id.clone()
    };
    let memory_namespace = MemoryNamespace::new(tenant_id, user_id);
    workspace_namespace_from_memory_namespace(&memory_namespace)
}

fn chat_response_from_context_response(
    response: hc_context::ContextResponse,
    request: &ChatRequest,
    agent_context: Option<&ResolvedAgentContext>,
    binding_active_task_id: Option<String>,
) -> ChatResponse {
    ChatResponse {
        message: api_message_from_llm(response.response.message),
        model: response.response.model.model,
        provider: response.response.model.provider,
        tenant_id: Some(request.memory.namespace.tenant_id.clone()),
        user_id: Some(request.memory.namespace.user_id.clone()),
        session_id: request.session_id.clone(),
        room_id: request.room_id.clone(),
        selected_agent_id: agent_context.map(|context| context.agent.id.clone()),
        selected_domain_id: agent_context.and_then(|context| context.agent.domain_id.clone()),
        selected_provider: None,
        recalled_memories: response
            .recalled_memories
            .into_iter()
            .map(memory_ref_from_retrieved)
            .collect(),
        synthesized_prompt_asset_count: response.synthesized_prompt_assets.len(),
        room_capabilities_used: Vec::new(),
        room_tools_used: Vec::new(),
        room_skills_used: Vec::new(),
        behavior_pattern_used: None,
        decision_reasoning: None,
        decision_confidence: None,
        active_task_id: binding_active_task_id,
    }
}

fn normalize_chat_request(mut request: ChatRequest) -> ChatRequest {
    if let Some(tenant_id) = normalized_optional_string(request.tenant_id.take()) {
        request.memory.namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = normalized_optional_string(request.user_id.take()) {
        request.memory.namespace.user_id = user_id;
    }
    if request.memory.namespace.tenant_id.trim().is_empty() {
        request.memory.namespace.tenant_id = hc_context::runtime::DEFAULT_TENANT_ID.to_owned();
    }
    if request.memory.namespace.user_id.trim().is_empty() {
        request.memory.namespace.user_id = hc_context::runtime::DEFAULT_USER_ID.to_owned();
    }
    request.tenant_id = Some(request.memory.namespace.tenant_id.clone());
    request.user_id = Some(request.memory.namespace.user_id.clone());
    request.session_id = normalized_optional_string(request.session_id.take()).or_else(|| {
        Some(default_session_id(
            &request.memory.namespace.tenant_id,
            &request.memory.namespace.user_id,
        ))
    });
    request.active_work_item_id = normalized_optional_string(request.active_work_item_id.take());
    request
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn append_runtime_identity_prompt(base_system_prompt: String, request: &ChatRequest) -> String {
    let identity = RuntimeIdentity::from_optional(
        Some(request.memory.namespace.tenant_id.clone()),
        Some(request.memory.namespace.user_id.clone()),
        request.session_id.clone(),
    );
    format!(
        "{base_system_prompt}\n\n{}",
        runtime_identity_prompt(&identity)
    )
}

#[derive(Debug, Clone)]
struct ResolvedAgentContext {
    agent: AgentProfile,
    domain: Option<DomainProfile>,
}

fn resolve_agent_context(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<ResolvedAgentContext>> {
    let namespace = request.memory.namespace.clone();
    let workspace_namespace =
        WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone());
    let agent_repository =
        AgentRepository::with_namespace(config.workspace_root.clone(), workspace_namespace.clone());
    let domain_repository =
        DomainRepository::with_namespace(config.workspace_root.clone(), workspace_namespace);

    let agents = agent_repository.list_profiles()?;
    if agents.is_empty() {
        return Ok(None);
    }
    let domains = domain_repository.list_profiles()?;

    let selection = resolve_chat_agent_selection(config, request)?;
    let selected_agent_id = selection.selected_agent_id;

    let Some(selected_agent_id) = selected_agent_id else {
        return Ok(None);
    };
    let agent = agents
        .into_iter()
        .find(|agent| agent.id == selected_agent_id)
        .with_context(|| format!("selected agent profile not found: {selected_agent_id}"))?;
    let domain = agent
        .domain_id
        .as_deref()
        .and_then(|domain_id| domains.into_iter().find(|domain| domain.id == domain_id));

    Ok(Some(ResolvedAgentContext { agent, domain }))
}

fn routing_input(request: &ChatRequest) -> Result<String> {
    if let Some(input) = request.input.as_deref()
        && !input.trim().is_empty()
    {
        return Ok(input.trim().to_owned());
    }
    request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == ApiMessageRole::User && !message.content.trim().is_empty())
        .map(|message| message.content.trim().to_owned())
        .ok_or_else(|| anyhow!("chat request requires input or messages"))
}

fn compose_agent_system_prompt(
    base_system_prompt: String,
    context: Option<&ResolvedAgentContext>,
) -> String {
    let Some(context) = context else {
        return base_system_prompt;
    };

    let mut sections = vec![
        base_system_prompt.trim().to_owned(),
        format!(
            "[Selected Agent]\nid: {}\nname: {}\nkind: {}\nproject_id: {}\ndomain_id: {}\npriority: {}",
            context.agent.id,
            context.agent.name,
            agent_kind_label(&context.agent.kind),
            context.agent.project_id.as_deref().unwrap_or(""),
            context.agent.domain_id.as_deref().unwrap_or(""),
            context.agent.priority
        ),
    ];

    if let Some(domain) = &context.domain {
        sections.push(format!(
            "[Selected Domain]\nid: {}\nname: {}\nkind: {}\npriority: {}\n{}",
            domain.id,
            domain.name,
            domain_kind_label(&domain.kind),
            domain.priority,
            domain.description
        ));
    }

    if !context.agent.tool_refs.is_empty() {
        sections.push(format!(
            "[Available Tool References]\n{}",
            context.agent.tool_refs.join("\n")
        ));
    }
    if !context.agent.memory_scope_refs.is_empty() {
        sections.push(format!(
            "[Memory Scope References]\n{}",
            context.agent.memory_scope_refs.join("\n")
        ));
    }
    if !context.agent.instructions.trim().is_empty() {
        sections.push(format!(
            "[Agent Instructions]\n{}",
            context.agent.instructions.trim()
        ));
    }

    sections.join("\n\n")
}

fn agent_kind_label(kind: &AgentKind) -> &'static str {
    match kind {
        AgentKind::DomainService => "domain_service",
        AgentKind::TaskRole => "task_role",
        AgentKind::Router => "router",
        AgentKind::Guard => "guard",
        AgentKind::Other => "other",
    }
}

fn domain_kind_label(kind: &DomainKind) -> &'static str {
    match kind {
        DomainKind::Service => "service",
        DomainKind::ProjectArea => "project_area",
        DomainKind::Safety => "safety",
        DomainKind::Other => "other",
    }
}

fn request_messages(request: &ChatRequest) -> Result<Vec<ChatMessage>> {
    let mut messages = request
        .messages
        .iter()
        .map(llm_message_from_api)
        .collect::<Vec<_>>();
    if let Some(input) = request.input.as_deref()
        && !input.trim().is_empty()
    {
        messages.push(ChatMessage::new(MessageRole::User, input.trim().to_owned()));
    }
    if messages.is_empty() {
        bail!("chat request requires input or messages");
    }
    Ok(messages)
}

/// When [`ApiMemoryQuery::scope`] is omitted, align memory retrieval with swarm routing tier +
/// task scope id (same inputs as routing/binding observability; ADR-004 Phase 2 rollout).
#[derive(Debug, Clone)]
struct SwarmMemoryScopeHint {
    tier: RoutingTier,
    task_scope_id: Option<String>,
}

fn normalized_task_scope_id(id: Option<&str>) -> Option<String> {
    id.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn swarm_memory_scope_hint_from_snapshot(
    snapshot: &SwarmRoutingBindingSnapshot,
    active_task_id: Option<&str>,
    fall_back_task_room_id: Option<&str>,
) -> SwarmMemoryScopeHint {
    let task_scope_id = normalized_task_scope_id(active_task_id)
        .or_else(|| normalized_task_scope_id(fall_back_task_room_id));
    SwarmMemoryScopeHint {
        tier: snapshot.routing.routing_tier,
        task_scope_id,
    }
}

fn apply_swarm_default_memory_scope(
    mut query: ContextMemoryQuery,
    hint: &SwarmMemoryScopeHint,
) -> ContextMemoryQuery {
    match hint.tier {
        RoutingTier::L1 => query.with_scope(MemoryScope::Session),
        RoutingTier::L2 | RoutingTier::L3 => {
            if let Some(id) = normalized_task_scope_id(hint.task_scope_id.as_deref()) {
                query = query.with_scope(MemoryScope::Task).with_room_anchor(id);
            } else {
                query = query.with_scope(MemoryScope::Session);
            }
            query
        }
    }
}

fn build_memory_query(
    namespace: MemoryNamespace,
    memory: &ApiMemoryQuery,
    fallback_text: Option<String>,
    swarm_scope_hint: Option<SwarmMemoryScopeHint>,
) -> Result<ContextMemoryQuery> {
    let mut query = ContextMemoryQuery::default().for_namespace(namespace);
    let text = memory
        .text
        .clone()
        .or(fallback_text)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(text) = text {
        query = query.with_text(text);
    }
    query = query.with_limit(memory.limit.unwrap_or(8).max(1));
    if let Some(scope) = memory.scope.as_deref() {
        query = query.with_scope(parse_scope(scope)?);
    } else if let Some(ref hint) = swarm_scope_hint {
        query = apply_swarm_default_memory_scope(query, hint);
    }
    if let Some(kind) = memory.kind.as_deref() {
        query.memory_query.kind = Some(parse_kind(kind)?);
    }
    if let Some(tag) = memory.tag.as_deref()
        && !tag.trim().is_empty()
    {
        query = query.with_tag(tag.trim().to_owned());
    }
    Ok(query)
}

fn llm_message_from_api(message: &ApiChatMessage) -> ChatMessage {
    let role = match message.role {
        ApiMessageRole::System => MessageRole::System,
        ApiMessageRole::User => MessageRole::User,
        ApiMessageRole::Assistant => MessageRole::Assistant,
        ApiMessageRole::Tool => MessageRole::Tool,
    };
    let mut chat_message = ChatMessage::new(role, message.content.clone());
    if let Some(name) = &message.name {
        chat_message = chat_message.named(name.clone());
    }
    chat_message
}

fn api_message_from_llm(message: ChatMessage) -> ApiChatMessage {
    let role = match message.role {
        MessageRole::System => ApiMessageRole::System,
        MessageRole::User => ApiMessageRole::User,
        MessageRole::Assistant => ApiMessageRole::Assistant,
        MessageRole::Tool => ApiMessageRole::Tool,
    };
    ApiChatMessage {
        role,
        content: message.content,
        name: message.name,
    }
}

fn memory_ref_from_retrieved(memory: hc_context::RetrievedMemory) -> MemoryRef {
    MemoryRef {
        id: memory.id,
        title: memory.title,
        summary: memory.summary,
        scope: memory_scope_label(&memory.scope).to_owned(),
        kind: memory_kind_label(&memory.kind).to_owned(),
        source_kind: memory.source_kind,
        confidence_milli: memory.confidence_milli,
        tags: memory.tags,
        room_id: memory.room_id,
    }
}

fn memory_namespace_from_api(namespace: &ApiNamespace) -> MemoryNamespace {
    MemoryNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

fn parse_scope(value: &str) -> Result<MemoryScope> {
    match value.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(MemoryScope::Global),
        "persona" => Ok(MemoryScope::Persona),
        "session" => Ok(MemoryScope::Session),
        "instance" => Ok(MemoryScope::Instance),
        "project" => Ok(MemoryScope::Project),
        "task" => Ok(MemoryScope::Task),
        other => bail!("unsupported memory scope: {other}"),
    }
}

fn parse_kind(value: &str) -> Result<MemoryKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "summary" => Ok(MemoryKind::Summary),
        "decision" => Ok(MemoryKind::Decision),
        "preference" => Ok(MemoryKind::Preference),
        "knowledge" => Ok(MemoryKind::Knowledge),
        "workflow_memory" | "workflow-memory" => Ok(MemoryKind::WorkflowMemory),
        other => bail!("unsupported memory kind: {other}"),
    }
}

#[cfg(test)]
mod memory_query_swarm_tests {
    use super::*;

    #[test]
    fn explicit_api_scope_skips_swarm_defaults() {
        let ns = MemoryNamespace::new("t", "u");
        let mut memory = ApiMemoryQuery::default();
        memory.scope = Some("global".to_owned());
        let hint = SwarmMemoryScopeHint {
            tier: RoutingTier::L3,
            task_scope_id: Some("task-room".to_owned()),
        };
        let q = build_memory_query(ns, &memory, None, Some(hint)).expect("query");
        assert_eq!(q.memory_query.scope, Some(MemoryScope::Global));
        assert!(q.room_anchor_ids.is_empty());
    }

    #[test]
    fn l1_swarm_defaults_to_session_scope() {
        let ns = MemoryNamespace::new("t", "u");
        let memory = ApiMemoryQuery::default();
        let hint = SwarmMemoryScopeHint {
            tier: RoutingTier::L1,
            task_scope_id: Some("ignored-for-l1-retrieval-default".to_owned()),
        };
        let q = build_memory_query(ns, &memory, None, Some(hint)).expect("query");
        assert_eq!(q.memory_query.scope, Some(MemoryScope::Session));
        assert!(q.room_anchor_ids.is_empty());
    }

    #[test]
    fn l2_with_task_id_defaults_to_task_scope_and_anchor() {
        let ns = MemoryNamespace::new("t", "u");
        let memory = ApiMemoryQuery::default();
        let hint = SwarmMemoryScopeHint {
            tier: RoutingTier::L2,
            task_scope_id: Some("task.http.implicit.999".to_owned()),
        };
        let q = build_memory_query(ns, &memory, None, Some(hint)).expect("query");
        assert_eq!(q.memory_query.scope, Some(MemoryScope::Task));
        assert_eq!(q.room_anchor_ids, vec!["task.http.implicit.999".to_owned()]);
    }

    #[test]
    fn l3_with_task_defaults_to_task_scope_and_anchor() {
        let ns = MemoryNamespace::new("t", "u");
        let memory = ApiMemoryQuery::default();
        let hint = SwarmMemoryScopeHint {
            tier: RoutingTier::L3,
            task_scope_id: Some("tid-42".to_owned()),
        };
        let q = build_memory_query(ns, &memory, None, Some(hint)).expect("query");
        assert_eq!(q.memory_query.scope, Some(MemoryScope::Task));
        assert_eq!(q.room_anchor_ids, vec!["tid-42".to_owned()]);
    }

    #[test]
    fn l3_without_task_falls_back_to_session_scope() {
        let ns = MemoryNamespace::new("t", "u");
        let memory = ApiMemoryQuery::default();
        let hint = SwarmMemoryScopeHint {
            tier: RoutingTier::L3,
            task_scope_id: None,
        };
        let q = build_memory_query(ns, &memory, None, Some(hint)).expect("query");
        assert_eq!(q.memory_query.scope, Some(MemoryScope::Session));
        assert!(q.room_anchor_ids.is_empty());
    }
}

#[cfg(test)]
mod prepare_chat_l23_task_plan_excerpt_tests {
    use super::*;
    use hc_agent::{
        HTTP_L23_EXECUTION_DIGEST_HEADING, HTTP_L23_PLAN_DIGEST_HEADING,
        HTTP_L23_REVIEW_DIGEST_HEADING,
    };
    use hc_agent::{TaskNamespace, TaskRequest};
    use hc_protocol::ApiNamespace;
    use hc_store::{store::WorkspaceStore, task_coordination::implicit_intent_journal_relative};
    use serde::Serialize;

    #[derive(Serialize)]
    struct TaskPlanFrontmatterFixture {
        id: String,
        #[serde(rename = "type")]
        doc_type: String,
        title: String,
        tenant_id: String,
        user_id: String,
        status: String,
        tags: Vec<String>,
        created_at: String,
        updated_at: String,
    }

    fn write_minimal_task_plan_fixture(
        root: &std::path::Path,
        workspace_ns: &WorkspaceNamespace,
        task_id: &str,
        body: &str,
    ) {
        let rel = task_plan_markdown_relative(task_id);
        let store = WorkspaceStore::new(root.to_path_buf());
        let fm = TaskPlanFrontmatterFixture {
            id: format!("task-plan.{task_id}"),
            doc_type: "task_plan".to_owned(),
            title: "fixture plan".to_owned(),
            tenant_id: workspace_ns.tenant_id.clone(),
            user_id: workspace_ns.user_id.clone(),
            status: "draft".to_owned(),
            tags: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        store
            .write_markdown_in_namespace(workspace_ns, &rel, &fm, body)
            .expect("write task_plan.md fixture");
    }

    fn build_l23_chat_request(
        tenant: &str,
        user: &str,
        session_id: &str,
        input: &str,
        task_id: &str,
    ) -> ChatRequest {
        ChatRequest {
            tenant_id: None,
            user_id: None,
            session_id: Some(session_id.to_owned()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some(input.to_owned()),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: Some(task_id.to_owned()),
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace::from_tenant_user(tenant, user),
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        }
    }

    fn create_l23_test_config(prefix: &str) -> ServiceConfig {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", wall_clock_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        ServiceConfig::new(dir)
    }

    fn l23_workspace_ns(tenant: &str, user: &str) -> WorkspaceNamespace {
        WorkspaceNamespace::new(tenant.to_owned(), user.to_owned())
    }

    fn prepare_l23_fixture(
        prefix: &str,
        tenant: &str,
        user: &str,
        task_id: &str,
        body: &str,
    ) -> (ServiceConfig, WorkspaceNamespace, TaskRequest) {
        let config = create_l23_test_config(prefix);
        let workspace_ns = l23_workspace_ns(tenant, user);
        write_minimal_task_plan_fixture(&config.workspace_root, &workspace_ns, task_id, body);
        let task = TaskRequest::new(task_id, "fixture", "goal")
            .with_namespace(TaskNamespace::new(tenant, user));
        (config, workspace_ns, task)
    }

    #[test]
    fn l23_reuse_active_task_appends_task_plan_excerpt_and_speaker_appendix() {
        let config = create_l23_test_config("hc-prepare-l23-task-plan");

        let tenant = "tenant_l23_excerpt";
        let user = "user_l23_excerpt";
        let task_id = "task.l23.excerpt.fixture";
        let workspace_ns = l23_workspace_ns(tenant, user);
        const PROBE: &str = "### EXCERPT_PROBE_PREPARE_CHAT_L23";
        write_minimal_task_plan_fixture(
            &config.workspace_root,
            &workspace_ns,
            task_id,
            &format!("{PROBE}\n\nPlanned work for HTTP excerpt path."),
        );

        let req = ChatRequest {
            tenant_id: None,
            user_id: None,
            session_id: Some("sess-l23-excerpt".into()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some("refactor the legacy checkout module for clarity".into()),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: Some(task_id.to_owned()),
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace::from_tenant_user(tenant, user),
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        };

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");

        assert!(
            prompt.contains("## Coordination task plan (read-only excerpt)"),
            "expected task plan excerpt header in system prompt"
        );
        assert!(
            prompt.contains(&format!("Task `{task_id}`.")),
            "expected task id label in excerpt block"
        );
        assert!(
            prompt.contains(PROBE),
            "expected persisted task_plan body in excerpt"
        );
        assert!(
            prompt.contains("Outward speaker (ADR-005 P0, HTTP L2/L3)"),
            "expected ADR-005 HTTP speaker appendix after L2/L3 classify"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l23_appends_review_note_digest_when_task_review_json_present() {
        use hc_agent::persist_review_note_artifact_v1;

        let tenant = "tenant_l23_review_digest";
        let user = "user_l23_review_digest";
        let task_id = "task.l23.review.digest.fixture";
        const PROBE: &str = "### EXCERPT_PROBE_REVIEW_DIGEST_L23";
        let (config, workspace_ns, task) = prepare_l23_fixture(
            "hc-prepare-l23-review-digest",
            tenant,
            user,
            task_id,
            &format!("{PROBE}\n\nPlanned work for HTTP review digest path."),
        );
        persist_review_note_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            "wi-review-probe",
            "Summary unique REVIEW_DIGEST_HTTP_L23_XY9",
            Some("needs_revision".into()),
            None,
            "reviewer:fixture",
        )
        .expect("persist review note");

        let req = build_l23_chat_request(
            tenant,
            user,
            "sess-l23-review-digest",
            "refactor the legacy checkout module for clarity",
            task_id,
        );

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");

        assert!(
            prompt.contains(HTTP_L23_REVIEW_DIGEST_HEADING),
            "expected review_note digest header in system prompt"
        );
        assert!(
            prompt.contains("REVIEW_DIGEST_HTTP_L23_XY9"),
            "expected persisted review summary in digest"
        );
        assert!(
            prompt.contains("`needs_revision`"),
            "expected verdict in digest line"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l23_execution_digest_keeps_truncated_marker_for_oversized_summary() {
        use hc_agent::persist_execution_result_artifact_v1;

        let tenant = "tenant_l23_exec_trunc";
        let user = "user_l23_exec_trunc";
        let task_id = "task.l23.exec.trunc.fixture";
        let (config, workspace_ns, task) = prepare_l23_fixture(
            "hc-prepare-l23-exec-trunc",
            tenant,
            user,
            task_id,
            "### EXCERPT_PROBE_EXEC_TRUNC_L23",
        );
        let huge = "x".repeat(12_000);
        persist_execution_result_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            "wi-exec-trunc",
            huge,
            None,
            "worker:fixture",
        )
        .expect("persist oversized execution result");

        let req = build_l23_chat_request(
            tenant,
            user,
            "sess-l23-exec-trunc",
            "refactor the legacy checkout module for clarity",
            task_id,
        );

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");
        assert!(
            prompt.contains("truncated; 1 execution_result record(s) on disk"),
            "oversized first execution result should still leave truncation marker"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l23_appends_plan_note_digest_when_task_plan_json_present() {
        use hc_agent::persist_plan_note_artifact_v1;

        let tenant = "tenant_l23_plan_digest";
        let user = "user_l23_plan_digest";
        let task_id = "task.l23.plan.digest.fixture";
        const PROBE: &str = "### EXCERPT_PROBE_PLAN_DIGEST_L23";
        let (config, workspace_ns, task) = prepare_l23_fixture(
            "hc-prepare-l23-plan-digest",
            tenant,
            user,
            task_id,
            &format!("{PROBE}\n\nPlanned work for HTTP plan digest path."),
        );
        persist_plan_note_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            None,
            "Summary unique PLAN_DIGEST_HTTP_L23_QK7",
            Some("planner split the rollout into two phases".into()),
            "planner:fixture",
        )
        .expect("persist plan note");

        let req = build_l23_chat_request(
            tenant,
            user,
            "sess-l23-plan-digest",
            "refactor the legacy checkout module for clarity",
            task_id,
        );

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");

        assert!(
            prompt.contains(HTTP_L23_PLAN_DIGEST_HEADING),
            "expected plan_note digest header in system prompt"
        );
        assert!(
            prompt.contains("PLAN_DIGEST_HTTP_L23_QK7"),
            "expected persisted plan summary in digest"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l23_review_note_digest_keeps_truncated_marker_for_oversized_summary() {
        use hc_agent::persist_review_note_artifact_v1;

        let tenant = "tenant_l23_review_trunc";
        let user = "user_l23_review_trunc";
        let task_id = "task.l23.review.trunc.fixture";
        let (config, workspace_ns, task) = prepare_l23_fixture(
            "hc-prepare-l23-review-trunc",
            tenant,
            user,
            task_id,
            "### EXCERPT_PROBE_REVIEW_TRUNC_L23",
        );
        let huge = "x".repeat(12_000);
        persist_review_note_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            "wi-review-trunc",
            huge,
            Some("needs_revision".into()),
            None,
            "reviewer:fixture",
        )
        .expect("persist oversized review note");

        let req = build_l23_chat_request(
            tenant,
            user,
            "sess-l23-review-trunc",
            "refactor the legacy checkout module for clarity",
            task_id,
        );

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");
        assert!(
            prompt.contains("truncated; 1 review_note record(s) on disk"),
            "oversized first review note should still leave truncation marker"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l23_plan_note_digest_keeps_truncated_marker_for_oversized_summary() {
        use hc_agent::persist_plan_note_artifact_v1;

        let tenant = "tenant_l23_plan_trunc";
        let user = "user_l23_plan_trunc";
        let task_id = "task.l23.plan.trunc.fixture";
        let (config, workspace_ns, task) = prepare_l23_fixture(
            "hc-prepare-l23-plan-trunc",
            tenant,
            user,
            task_id,
            "### EXCERPT_PROBE_PLAN_TRUNC_L23",
        );
        let huge = "x".repeat(12_000);
        persist_plan_note_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            None,
            huge,
            None,
            "planner:fixture",
        )
        .expect("persist oversized plan note");

        let req = build_l23_chat_request(
            tenant,
            user,
            "sess-l23-plan-trunc",
            "refactor the legacy checkout module for clarity",
            task_id,
        );

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");
        assert!(
            prompt.contains("truncated; 1 plan_note record(s) on disk"),
            "oversized first plan note should still leave truncation marker"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l23_appends_all_artifact_digests_in_stable_order() {
        use hc_agent::{
            TaskNamespace, TaskRequest, persist_execution_result_artifact_v1,
            persist_plan_note_artifact_v1, persist_review_note_artifact_v1,
        };

        let config = create_l23_test_config("hc-prepare-l23-all-digests");

        let tenant = "tenant_l23_all_digests";
        let user = "user_l23_all_digests";
        let task_id = "task.l23.all.digests.fixture";
        let workspace_ns = l23_workspace_ns(tenant, user);
        write_minimal_task_plan_fixture(
            &config.workspace_root,
            &workspace_ns,
            task_id,
            "### EXCERPT_PROBE_ALL_DIGESTS",
        );
        let task = TaskRequest::new(task_id, "fixture", "goal")
            .with_namespace(TaskNamespace::new(tenant, user));
        persist_execution_result_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            "wi-all",
            "EXEC_ALL_DIGESTS_UNIQUE",
            None,
            "worker:fixture",
        )
        .expect("persist execution");
        persist_plan_note_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            None,
            "PLAN_ALL_DIGESTS_UNIQUE",
            None,
            "planner:fixture",
        )
        .expect("persist plan");
        persist_review_note_artifact_v1(
            &config.workspace_root,
            &workspace_ns,
            &task,
            "wi-all",
            "REVIEW_ALL_DIGESTS_UNIQUE",
            Some("approve".into()),
            None,
            "reviewer:fixture",
        )
        .expect("persist review");

        let req = ChatRequest {
            tenant_id: None,
            user_id: None,
            session_id: Some("sess-l23-all-digests".into()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some("refactor for rollout safety".into()),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: Some(task_id.to_owned()),
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace::from_tenant_user(tenant, user),
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        };

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");
        let execution_idx = prompt
            .find(HTTP_L23_EXECUTION_DIGEST_HEADING)
            .expect("execution digest header");
        let plan_idx = prompt
            .find(HTTP_L23_PLAN_DIGEST_HEADING)
            .expect("plan digest header");
        let review_idx = prompt
            .find(HTTP_L23_REVIEW_DIGEST_HEADING)
            .expect("review digest header");
        assert!(execution_idx < plan_idx && plan_idx < review_idx);
        assert!(prompt.contains("EXEC_ALL_DIGESTS_UNIQUE"));
        assert!(prompt.contains("PLAN_ALL_DIGESTS_UNIQUE"));
        assert!(prompt.contains("REVIEW_ALL_DIGESTS_UNIQUE"));

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    /// **`CreateImplicitTask`** uses **`task.http.implicit.{created_at_ms}`**; [`prepare_chat_request_with_swarm_clock`]
    /// pins **`swarm_created_at_ms`** so the same slug can **`task_plan.md`** preseed before prepare.
    #[test]
    fn l23_http_implicit_task_anchor_appends_task_plan_excerpt_when_file_preseeded() {
        const FIXED_MS: u64 = 9_424_242;
        let implicit_id = format!("task.http.implicit.{FIXED_MS}");

        let dir =
            std::env::temp_dir().join(format!("hc-prepare-l23-implicit-tp-{}", wall_clock_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir.clone());

        let tenant = "tenant_l23_implicit_tp";
        let user = "user_l23_implicit_tp";
        let workspace_ns = WorkspaceNamespace::new(tenant.to_owned(), user.to_owned());

        const PROBE: &str = "### EXCERPT_PROBE_HTTP_IMPLICIT_TASK_PLAN";
        write_minimal_task_plan_fixture(
            &config.workspace_root,
            &workspace_ns,
            &implicit_id,
            &format!("{PROBE}\n\nImplicit HTTP task slug matches swarm clock."),
        );

        let req = ChatRequest {
            tenant_id: None,
            user_id: None,
            session_id: Some("sess-l23-http-implicit-tp".into()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some("refactor the notification pipeline end to end".into()),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: None,
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace::from_tenant_user(tenant, user),
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        };

        let prepared =
            prepare_chat_request_with_swarm_clock(&config, req, None, "chat.api", FIXED_MS)
                .unwrap();

        assert_eq!(
            prepared.binding_active_task_id.as_deref(),
            Some(implicit_id.as_str()),
            "implicit L2/L3 must bind outbound active_task_id to task.http.implicit.{{ms}}"
        );

        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");
        assert!(
            prompt.contains("## Coordination task plan (read-only excerpt)"),
            "implicit task anchor still loads persisted task_plan excerpt"
        );
        assert!(
            prompt.contains(&format!("Task `{implicit_id}`.")),
            "excerpt banner should cite the implicit task id"
        );
        assert!(prompt.contains(PROBE));
        assert!(prompt.contains("Outward speaker (ADR-005 P0, HTTP L2/L3)"));

        let journal_abs = config
            .workspace_root
            .join(workspace_ns.scoped_prefix())
            .join(implicit_intent_journal_relative(&implicit_id));
        assert!(
            journal_abs.is_file(),
            "expected coordination/<slug>.implicit-intent.jsonl after CreateImplicitTask path ({})",
            journal_abs.display()
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    /// No prewritten markdown: **`emit_swarm_observability_from_classified`** allocates implicit id →
    /// **`ensure_http_implicit_task_plan_stub`** → same-turn **`prepare`** excerpt sees persisted plan body.
    #[test]
    fn l23_http_implicit_prepare_materializes_task_plan_stub_for_prompt_excerpt() {
        use hc_agent::read_task_artifact;

        const FIXED_MS: u64 = 7_771_771;
        let implicit_id = format!("task.http.implicit.{FIXED_MS}");
        let user_goal = "refactor the payment adapters layer for rollout safety";

        let dir =
            std::env::temp_dir().join(format!("hc-prepare-l23-implicit-stub-{}", wall_clock_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir.clone());

        let tenant = "tenant_l23_implicit_stub";
        let user = "user_l23_implicit_stub";
        let workspace_ns = WorkspaceNamespace::new(tenant.to_owned(), user.to_owned());

        let req = ChatRequest {
            tenant_id: None,
            user_id: None,
            session_id: Some("sess-l23-http-implicit-stub".into()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some(user_goal.into()),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: None,
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace::from_tenant_user(tenant, user),
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        };

        let prepared =
            prepare_chat_request_with_swarm_clock(&config, req, None, "chat.api", FIXED_MS)
                .unwrap();

        assert_eq!(
            prepared.binding_active_task_id.as_deref(),
            Some(implicit_id.as_str())
        );

        let plan_rel = task_plan_markdown_relative(&implicit_id);
        let persisted =
            read_task_artifact(&config.workspace_root, &workspace_ns, &plan_rel).unwrap();
        assert!(
            persisted.body.contains("- goal:"),
            "stub render_task_plan body should expose goal field"
        );
        assert!(
            persisted.body.contains("refactor the payment adapters"),
            "persisted stub should retain the user-triggering message as task goal ({})",
            implicit_id,
        );
        assert!(
            persisted.body.contains("work-item.http.implicit.holder"),
            "implicit stub should seed a Planned placeholder work item row"
        );
        assert!(
            persisted.body.contains("lifecycle=planned"),
            "seed work item should render with lifecycle=planned"
        );
        let expected_routing_id = format!("chat.api.{FIXED_MS}");
        assert!(
            persisted
                .body
                .contains(&format!("routing_message_id={expected_routing_id}")),
            "planning_notes should embed swarm routing_message_id for same-turn trace join"
        );
        assert!(
            persisted
                .body
                .contains("session_id=sess-l23-http-implicit-stub"),
            "planning_notes should embed conversation session_id"
        );

        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");
        assert!(
            prompt.contains("## Coordination task plan (read-only excerpt)"),
            "same-turn excerpt should hydrate from materialized implicit task_plan"
        );
        assert!(
            prompt.contains("refactor the payment adapters"),
            "system prompt excerpt should include goal text copied from persisted stub",
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn l1_with_task_on_disk_does_not_append_task_plan_excerpt() {
        let dir =
            std::env::temp_dir().join(format!("hc-prepare-l1-no-excerpt-{}", wall_clock_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir.clone());

        let tenant = "tenant_l1_no_excerpt";
        let user = "user_l1_no_excerpt";
        let task_id = "task.l1.no_excerpt.fixture";
        let workspace_ns = WorkspaceNamespace::new(tenant.to_owned(), user.to_owned());
        const PROBE: &str = "### SHOULD_NOT_APPEAR_IN_SYSTEM_PROMPT_L1";
        write_minimal_task_plan_fixture(&config.workspace_root, &workspace_ns, task_id, PROBE);

        let req = ChatRequest {
            tenant_id: None,
            user_id: None,
            session_id: Some("sess-l1".into()),
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: Some("hello".into()),
            messages: Vec::new(),
            provider: None,
            model: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: Some(task_id.to_owned()),
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace::from_tenant_user(tenant, user),
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        };

        let prepared = prepare_chat_request(&config, req, None, "test.prepare.prefix").unwrap();
        let prompt = prepared
            .context_request
            .system_prompt
            .as_deref()
            .expect("system prompt");

        assert!(
            !prompt.contains("Coordination task plan (read-only excerpt)"),
            "L1 must not inject read-only task_plan excerpt"
        );
        assert!(
            !prompt.contains(PROBE),
            "task plan body must not leak into L1 system prompt"
        );

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }
}

pub fn concise_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .next()
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| anyhow!("unknown error").to_string())
}
