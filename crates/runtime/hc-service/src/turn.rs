use anyhow::Result;
use hc_intent::{IntentInput, IntentRouter};
use hc_protocol::{ChatRequest, ChatResponse};
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, OnceLock};

use crate::{
    ServiceConfig,
    chat::{
        ChatStreamEvent, handle_turn_router_chat_fallback_request,
        handle_turn_router_chat_fallback_stream_request,
    },
    room_routing::{RoomRoutingContext, resolve_room_routing_context},
    timed_turn::{TimedDeliverMode, execute_timed_turn_plan, timed_stream_plan_from_plan},
    tool_turn::{
        ToolTurnResult, execute_configured_mcp_route, execute_persisted_pending_confirmation_plan,
    },
    turn_router::{
        ChatFallbackPlan, TurnDecision, TurnProviderRegistry, TurnRoute, TurnRouterInput,
    },
};

fn intent_router() -> &'static IntentRouter {
    static ROUTER: OnceLock<IntentRouter> = OnceLock::new();
    ROUTER.get_or_init(IntentRouter::with_builtin_defaults)
}

fn resolve_turn_intent(request: &ChatRequest) -> hc_intent::IntentResolution {
    let text = crate::tool_turn::request_input(request).unwrap_or_default();
    intent_router().resolve(&IntentInput {
        user_text: text.trim(),
    })
}

fn provider_registry() -> &'static TurnProviderRegistry {
    static REGISTRY: LazyLock<TurnProviderRegistry> =
        LazyLock::new(TurnProviderRegistry::with_builtin_defaults);
    &REGISTRY
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnStreamEvent {
    Started {
        tenant_id: Option<String>,
        user_id: Option<String>,
        session_id: Option<String>,
    },
    Tool {
        tool_id: String,
        server_id: String,
        tool_name: String,
    },
    Chat {
        event: ChatStreamEvent,
    },
    Completed,
}

fn annotate_turn_decision(
    mut response: ChatResponse,
    provider_id: &str,
    reason: &str,
) -> ChatResponse {
    response.selected_provider = Some(provider_id.to_owned());
    if response.decision_reasoning.is_none() {
        response.decision_reasoning = Some(format!("[{provider_id}] {reason}"));
    }
    response
}

#[derive(Debug, Clone)]
pub enum ServiceTurnOutcome {
    PendingConfirmation(ToolTurnResult),
    Timed(ChatResponse),
    McpTool(ToolTurnResult),
    ChatFallback { request: ChatRequest },
}

fn apply_chat_fallback_plan(mut request: ChatRequest, plan: ChatFallbackPlan) -> ChatRequest {
    if request.agent_id.is_none() {
        request.agent_id = plan.selected_agent_id.clone();
    }
    if request.domain_id.is_none() {
        request.domain_id = plan.selected_domain_id.clone();
    }
    if request.active_agent_id.is_none() {
        request.active_agent_id = request.agent_id.clone();
    }
    request
}

/// When the caller did not pass a cached [`RoomRoutingContext`], load it once (same as historical turn handlers).
fn resolve_turn_fallback_room_attachment(
    config: &ServiceConfig,
    request: &ChatRequest,
    room_routing_cache: Option<&RoomRoutingContext>,
) -> Result<Option<RoomRoutingContext>> {
    if room_routing_cache.is_none() {
        Ok(resolve_room_routing_context(config, request)?)
    } else {
        Ok(None)
    }
}

/// Single [`TurnProviderRegistry::decide`] for resolved `room_routing` (sync + stream share this **after** room attach).
fn classify_turn_route_decision(
    config: &ServiceConfig,
    request: &ChatRequest,
    room_routing: Option<&RoomRoutingContext>,
    history_for_match: &[hc_protocol::ApiChatMessage],
) -> Result<TurnDecision> {
    let intent = resolve_turn_intent(request);
    provider_registry().decide(&TurnRouterInput {
        config,
        request,
        intent: &intent,
        room_routing,
        history_for_match,
    })
}

fn emit_turn_route_swarm_observability_non_chat_fallback(
    config: &ServiceConfig,
    request: &ChatRequest,
    route: &TurnRoute,
    room_routing: Option<&RoomRoutingContext>,
) {
    if matches!(route, TurnRoute::ChatFallback(_)) {
        return;
    }
    let ns = crate::chat::workspace_namespace_from_chat_request(request);
    crate::chat::emit_swarm_observability_for_chat_like_request(
        config,
        &ns,
        request,
        "turn.api",
        room_routing,
    );
}

/// `room_routing_cache`: reuse a [`resolve_room_routing_context`](resolve_room_routing_context) result
/// when the caller already loaded it for the same `request`.
pub fn try_handle_service_turn(
    config: &ServiceConfig,
    request: &ChatRequest,
    timed_deliver_mode: TimedDeliverMode,
    history_for_match: &[hc_protocol::ApiChatMessage],
    room_routing_cache: Option<&RoomRoutingContext>,
) -> Result<ServiceTurnOutcome> {
    let fallback_owned =
        resolve_turn_fallback_room_attachment(config, request, room_routing_cache)?;
    let room_routing = room_routing_cache.or_else(|| fallback_owned.as_ref());
    let decision = classify_turn_route_decision(config, request, room_routing, history_for_match)?;

    let selected = decision.selected;
    let provider_id = selected.provider_id;
    let reason = selected.reason.clone();

    emit_turn_route_swarm_observability_non_chat_fallback(
        config,
        request,
        &selected.route,
        room_routing,
    );

    match selected.route {
        TurnRoute::PendingConfirmation(plan) => {
            let mut tool_result =
                execute_persisted_pending_confirmation_plan(config, request, plan)?;
            tool_result.response =
                annotate_turn_decision(tool_result.response, provider_id, &reason);
            Ok(ServiceTurnOutcome::PendingConfirmation(tool_result))
        }
        TurnRoute::Timed(plan) => {
            let response = execute_timed_turn_plan(config, request, timed_deliver_mode, plan)?
                .ok_or_else(|| anyhow::anyhow!("timed turn plan produced no response"))?;
            Ok(ServiceTurnOutcome::Timed(annotate_turn_decision(
                response,
                provider_id,
                &reason,
            )))
        }
        TurnRoute::McpTool(route) => {
            let Some(mut tool_result) = execute_configured_mcp_route(config, request, route)?
            else {
                return Err(anyhow::anyhow!("configured MCP route produced no response"));
            };
            tool_result.response =
                annotate_turn_decision(tool_result.response, provider_id, &reason);
            Ok(ServiceTurnOutcome::McpTool(tool_result))
        }
        TurnRoute::ChatFallback(plan) => Ok(ServiceTurnOutcome::ChatFallback {
            request: apply_chat_fallback_plan(request.clone(), plan),
        }),
    }
}

pub fn handle_turn_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
) -> Result<ChatResponse> {
    match try_handle_service_turn(
        config,
        &request,
        TimedDeliverMode::Headless,
        &request.messages,
        room_routing_cache.as_ref(),
    )? {
        ServiceTurnOutcome::PendingConfirmation(tool_result) => Ok(tool_result.response),
        ServiceTurnOutcome::Timed(response) => Ok(response),
        ServiceTurnOutcome::McpTool(tool_result) => Ok(tool_result.response),
        ServiceTurnOutcome::ChatFallback { request } => {
            handle_turn_router_chat_fallback_request(config, request, room_routing_cache)
        }
    }
}

pub fn handle_turn_stream_request(
    config: &ServiceConfig,
    request: ChatRequest,
    room_routing_cache: Option<RoomRoutingContext>,
    on_event: &mut dyn FnMut(TurnStreamEvent) -> Result<()>,
) -> Result<ChatResponse> {
    let fallback_owned =
        resolve_turn_fallback_room_attachment(config, &request, room_routing_cache.as_ref())?;
    let room_routing = room_routing_cache
        .as_ref()
        .or_else(|| fallback_owned.as_ref());
    on_event(TurnStreamEvent::Started {
        tenant_id: request.tenant_id.clone(),
        user_id: request.user_id.clone(),
        session_id: request.session_id.clone(),
    })?;
    let decision = classify_turn_route_decision(config, &request, room_routing, &request.messages)?;

    let selected = decision.selected;
    let provider_id = selected.provider_id;
    let reason = selected.reason.clone();

    emit_turn_route_swarm_observability_non_chat_fallback(
        config,
        &request,
        &selected.route,
        room_routing,
    );

    match selected.route {
        TurnRoute::PendingConfirmation(plan) => {
            let tool_result = execute_persisted_pending_confirmation_plan(config, &request, plan)?;
            on_event(TurnStreamEvent::Tool {
                tool_id: tool_result.tool_id,
                server_id: tool_result.server_id,
                tool_name: tool_result.tool_name,
            })?;
            on_event(TurnStreamEvent::Completed)?;
            Ok(annotate_turn_decision(
                tool_result.response,
                provider_id,
                &reason,
            ))
        }
        TurnRoute::Timed(plan) => {
            let Some(stream_plan) = timed_stream_plan_from_plan(config, &request, plan)? else {
                return Err(anyhow::anyhow!("timed stream plan produced no response"));
            };
            use std::thread;
            use std::time::Duration;
            for (index, chunk) in stream_plan.chunks.iter().enumerate() {
                if index > 0 && stream_plan.pause_between_chunks_ms > 0 {
                    thread::sleep(Duration::from_millis(stream_plan.pause_between_chunks_ms));
                }
                on_event(TurnStreamEvent::Chat {
                    event: ChatStreamEvent::Delta {
                        delta: chunk.clone(),
                        finish_reason: None,
                    },
                })?;
            }
            on_event(TurnStreamEvent::Chat {
                event: ChatStreamEvent::Completed {
                    response: annotate_turn_decision(
                        stream_plan.final_response.clone(),
                        provider_id,
                        &reason,
                    ),
                },
            })?;
            on_event(TurnStreamEvent::Completed)?;
            Ok(annotate_turn_decision(
                stream_plan.final_response,
                provider_id,
                &reason,
            ))
        }
        TurnRoute::McpTool(route) => {
            let Some(tool_result) = execute_configured_mcp_route(config, &request, route)? else {
                return Err(anyhow::anyhow!("configured MCP route produced no response"));
            };
            on_event(TurnStreamEvent::Tool {
                tool_id: tool_result.tool_id,
                server_id: tool_result.server_id,
                tool_name: tool_result.tool_name,
            })?;
            on_event(TurnStreamEvent::Completed)?;
            Ok(annotate_turn_decision(
                tool_result.response,
                provider_id,
                &reason,
            ))
        }
        TurnRoute::ChatFallback(plan) => {
            let request = apply_chat_fallback_plan(request, plan);
            let response = handle_turn_router_chat_fallback_stream_request(
                config,
                request,
                room_routing_cache,
                &mut |event| on_event(TurnStreamEvent::Chat { event }),
            )?;
            on_event(TurnStreamEvent::Completed)?;
            Ok(annotate_turn_decision(response, provider_id, &reason))
        }
    }
}

#[cfg(test)]
mod turn_classify_tests {
    use super::*;
    use hc_bootstrap::wall_clock_ms;
    use hc_context::runtime::{DEFAULT_TENANT_ID, DEFAULT_USER_ID};
    use hc_protocol::{ApiChatMessage, ApiMemoryQuery, ApiMessageRole, ApiNamespace, ChatRequest};
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    use crate::{
        room_routing::{
            PROVIDER_CHAT_FALLBACK, PROVIDER_MCP_TOOL, PROVIDER_PENDING_CONFIRMATION,
            PROVIDER_TIMED, RoomRoutingContext, default_enabled_providers,
        },
        timed_turn::TimedTurnPlan,
        tool_turn::{
            PendingToolConfirmation, ToolConfirmationFlowRule, ToolTurnSessionState,
            request_namespace, save_tool_turn_session_state,
        },
    };
    use hc_memory::{MemoryLayer, MemoryRoom, ResolvedRoomCapabilities};

    fn chat_request_fixture(content: impl Into<String>) -> ChatRequest {
        ChatRequest {
            tenant_id: Some(DEFAULT_TENANT_ID.to_owned()),
            user_id: Some(DEFAULT_USER_ID.to_owned()),
            session_id: None,
            room_id: None,
            behavior_pattern: None,
            thinking_depth: None,
            input: None,
            messages: vec![ApiChatMessage {
                role: ApiMessageRole::User,
                content: content.into(),
                name: None,
            }],
            model: None,
            provider: None,
            system_prompt: None,
            agent_id: None,
            domain_id: None,
            active_agent_id: None,
            active_task_id: None,
            active_work_item_id: None,
            memory: ApiMemoryQuery {
                namespace: ApiNamespace {
                    tenant_id: DEFAULT_TENANT_ID.to_owned(),
                    user_id: DEFAULT_USER_ID.to_owned(),
                },
                ..Default::default()
            },
            temperature: None,
            max_output_tokens: None,
        }
    }

    fn chat_request_fixture_with_session(
        session_id: impl Into<String>,
        content: impl Into<String>,
    ) -> ChatRequest {
        let mut r = chat_request_fixture(content);
        r.session_id = Some(session_id.into());
        r
    }

    /// Minimal `routing/tool-routing-tags.md` so [`crate::tool_turn::load_tool_routing_tags`] succeeds.
    /// **`id` / `type`** satisfy workspace markdown index invariants (plain `confirmation_hints`‑only YAML triggered `mcp` index errors in tests).
    fn write_minimal_confirmation_routing_tags(config: &ServiceConfig) {
        let path = config
            .workspace_root
            .join("tenants")
            .join(DEFAULT_TENANT_ID)
            .join("users")
            .join(DEFAULT_USER_ID)
            .join("routing")
            .join("tool-routing-tags.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            r#"---
id: turn-classify-pending-fixture-routing-tags
type: routing_tags
confirmation_hints:
  - "确认"
routing_stop_terms: []
---
"#,
        )
        .unwrap();
    }

    fn stub_room_routing_without_timed_provider() -> RoomRoutingContext {
        let mut enabled = default_enabled_providers();
        enabled.remove(PROVIDER_TIMED);
        assert!(!enabled.contains(PROVIDER_TIMED));
        assert!(enabled.contains(PROVIDER_CHAT_FALLBACK));

        RoomRoutingContext {
            room: MemoryRoom::new(
                "room.turn.test.timed-disabled",
                MemoryLayer::Chat,
                "stub-title",
                "stub-summary",
            ),
            resolved: ResolvedRoomCapabilities::new("room.turn.test.timed-disabled"),
            enabled_providers: enabled,
            allowed_tool_ids: None,
            provider_weights: BTreeMap::new(),
            provider_argument_overrides: BTreeMap::new(),
            tool_argument_overrides: BTreeMap::new(),
            capability_ids: BTreeSet::new(),
            skill_ids: BTreeSet::new(),
        }
    }

    fn stub_room_routing_without_mcp_tool_provider() -> RoomRoutingContext {
        let mut enabled = default_enabled_providers();
        enabled.remove(PROVIDER_MCP_TOOL);
        assert!(!enabled.contains(PROVIDER_MCP_TOOL));
        assert!(enabled.contains(PROVIDER_PENDING_CONFIRMATION));

        RoomRoutingContext {
            room: MemoryRoom::new(
                "room.turn.test.mcp-disabled",
                MemoryLayer::Chat,
                "stub-title",
                "stub-summary",
            ),
            resolved: ResolvedRoomCapabilities::new("room.turn.test.mcp-disabled"),
            enabled_providers: enabled,
            allowed_tool_ids: None,
            provider_weights: BTreeMap::new(),
            provider_argument_overrides: BTreeMap::new(),
            tool_argument_overrides: BTreeMap::new(),
            capability_ids: BTreeSet::new(),
            skill_ids: BTreeSet::new(),
        }
    }

    /// `pending_confirmation` + `mcp_tool` off; **`timed`** still on (harmless for confirmation-only user text).
    fn stub_room_routing_without_pending_and_mcp_providers() -> RoomRoutingContext {
        let mut enabled = default_enabled_providers();
        enabled.remove(PROVIDER_PENDING_CONFIRMATION);
        enabled.remove(PROVIDER_MCP_TOOL);
        assert!(!enabled.contains(PROVIDER_PENDING_CONFIRMATION));
        assert!(enabled.contains(PROVIDER_CHAT_FALLBACK));

        RoomRoutingContext {
            room: MemoryRoom::new(
                "room.turn.test.pending-mcp-disabled",
                MemoryLayer::Chat,
                "stub-title",
                "stub-summary",
            ),
            resolved: ResolvedRoomCapabilities::new("room.turn.test.pending-mcp-disabled"),
            enabled_providers: enabled,
            allowed_tool_ids: None,
            provider_weights: BTreeMap::new(),
            provider_argument_overrides: BTreeMap::new(),
            tool_argument_overrides: BTreeMap::new(),
            capability_ids: BTreeSet::new(),
            skill_ids: BTreeSet::new(),
        }
    }

    #[test]
    fn shared_classifier_plain_message_chat_fallback_then_try_handle_aligned() {
        let dir = std::env::temp_dir().join(format!("hc-turn-shared-classify-{}", wall_clock_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir);
        let req = chat_request_fixture("hello from turn shared classify regression");

        let attachment = resolve_turn_fallback_room_attachment(&config, &req, None).unwrap();
        let room_routing = attachment.as_ref();
        let decision =
            classify_turn_route_decision(&config, &req, room_routing, &req.messages).unwrap();
        assert!(matches!(
            decision.selected.route,
            TurnRoute::ChatFallback(_)
        ));

        let outcome = try_handle_service_turn(
            &config,
            &req,
            TimedDeliverMode::Headless,
            &req.messages,
            None,
        )
        .unwrap();
        assert!(matches!(outcome, ServiceTurnOutcome::ChatFallback { .. }));

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    #[test]
    fn classify_turn_cn_reminder_selects_timed_provider() {
        let dir = std::env::temp_dir().join(format!("hc-turn-timed-classify-{}", wall_clock_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir);
        let req = chat_request_fixture("十分钟后提醒我喝水");

        let attachment = resolve_turn_fallback_room_attachment(&config, &req, None).unwrap();
        let room = attachment.as_ref();
        let decision = classify_turn_route_decision(&config, &req, room, &req.messages).unwrap();

        assert_eq!(decision.selected.provider_id, PROVIDER_TIMED);
        match &decision.selected.route {
            TurnRoute::Timed(TimedTurnPlan::Reminder {
                rule,
                delay_seconds,
            }) => {
                assert_eq!(rule.id, "builtin.reminder.cn");
                assert!(*delay_seconds >= 60);
            }
            route => panic!("expected Timed reminder route, got {:?}", route),
        }

        let outcome = try_handle_service_turn(
            &config,
            &req,
            TimedDeliverMode::Headless,
            &req.messages,
            None,
        )
        .unwrap();
        match outcome {
            ServiceTurnOutcome::Timed(response) => {
                assert!(response.message.content.contains("提醒您"));
            }
            other => panic!("expected ServiceTurnOutcome::Timed, got {:?}", other),
        }

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    /// Reminder-shaped user text but room disables `timed`; must not select [`TurnRoute::Timed`].
    #[test]
    fn classify_turn_cn_reminder_room_disables_timed_selects_chat_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "hc-turn-reminder-room-no-timed-{}",
            wall_clock_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir);
        let req = chat_request_fixture("十分钟后提醒我喝水");

        let room_ctx = stub_room_routing_without_timed_provider();
        assert!(!room_ctx.allows_provider(PROVIDER_TIMED));

        let decision =
            classify_turn_route_decision(&config, &req, Some(&room_ctx), &req.messages).unwrap();

        assert_eq!(
            decision.selected.provider_id, PROVIDER_CHAT_FALLBACK,
            "timed provider should be skipped when room.routing disables it"
        );
        assert!(matches!(
            decision.selected.route,
            TurnRoute::ChatFallback(_)
        ));

        let outcome = try_handle_service_turn(
            &config,
            &req,
            TimedDeliverMode::Headless,
            &req.messages,
            Some(&room_ctx),
        )
        .unwrap();
        assert!(matches!(outcome, ServiceTurnOutcome::ChatFallback { .. }));

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    /// Pending-confirmation session + confirmation-shaped user text → router classifies **`pending_confirmation`** (score beats Timed / chat fallback).
    /// Does not call [`try_handle_service_turn`] execute branch (would require MCP toolchain).
    #[test]
    fn classify_turn_pending_confirmation_wins_when_session_awaits_confirmation() {
        let dir = std::env::temp_dir().join(format!(
            "hc-turn-pending-confirm-classify-{}",
            wall_clock_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir);
        write_minimal_confirmation_routing_tags(&config);

        let room_ctx = stub_room_routing_without_mcp_tool_provider();

        let session_slug = "turn-pending-confirm-classify";
        let req = chat_request_fixture_with_session(session_slug, "确认");

        let namespace = request_namespace(&req);
        save_tool_turn_session_state(
            &config,
            &namespace,
            session_slug,
            &ToolTurnSessionState {
                pending_confirmation: Some(PendingToolConfirmation {
                    tool_id: "tool.mcp.fixture.order".into(),
                    fallback_tool_ids: Vec::new(),
                    items: vec![json!({ "sku": "demo-item" })],
                    flow: ToolConfirmationFlowRule::default(),
                }),
            },
        )
        .unwrap();

        let decision =
            classify_turn_route_decision(&config, &req, Some(&room_ctx), &req.messages).unwrap();

        assert_eq!(decision.selected.provider_id, PROVIDER_PENDING_CONFIRMATION);
        assert!(matches!(
            decision.selected.route,
            TurnRoute::PendingConfirmation(_)
        ));

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }

    /// Same pending session + **「确认」** as [`classify_turn_pending_confirmation_wins_when_session_awaits_confirmation`], but room disables **`pending_confirmation`** → must not hydrate the persisted slot.
    #[test]
    fn classify_turn_cn_confirm_room_disables_pending_selects_chat_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "hc-turn-room-no-pending-confirm-{}",
            wall_clock_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ServiceConfig::new(dir);
        write_minimal_confirmation_routing_tags(&config);

        let room_ctx = stub_room_routing_without_pending_and_mcp_providers();
        assert!(!room_ctx.allows_provider(PROVIDER_PENDING_CONFIRMATION));

        let session_slug = "turn-room-disables-pending";
        let req = chat_request_fixture_with_session(session_slug, "确认");

        let namespace = request_namespace(&req);
        save_tool_turn_session_state(
            &config,
            &namespace,
            session_slug,
            &ToolTurnSessionState {
                pending_confirmation: Some(PendingToolConfirmation {
                    tool_id: "tool.mcp.fixture.order".into(),
                    fallback_tool_ids: Vec::new(),
                    items: vec![json!({ "sku": "demo-item" })],
                    flow: ToolConfirmationFlowRule::default(),
                }),
            },
        )
        .unwrap();

        let decision =
            classify_turn_route_decision(&config, &req, Some(&room_ctx), &req.messages).unwrap();

        assert_eq!(
            decision.selected.provider_id, PROVIDER_CHAT_FALLBACK,
            "pending_confirmation provider gated off despite persisted session pending state"
        );
        assert!(matches!(
            decision.selected.route,
            TurnRoute::ChatFallback(_)
        ));

        let _ = std::fs::remove_dir_all(&config.workspace_root);
    }
}
