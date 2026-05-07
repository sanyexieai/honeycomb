use anyhow::Result;
use hc_intent::{IntentInput, IntentRouter};
use hc_protocol::{ChatRequest, ChatResponse};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

use crate::{
    ServiceConfig,
    chat::{ChatStreamEvent, handle_chat_request, handle_chat_stream_request},
    timed_turn::{TimedDeliverMode, try_handle_timed_chat_turn, try_timed_stream_plan},
    tool_turn::{try_handle_configured_mcp_route_turn, try_handle_persisted_pending_confirmation},
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

pub fn handle_turn_request(config: &ServiceConfig, request: ChatRequest) -> Result<ChatResponse> {
    if let Some(tool_result) = try_handle_persisted_pending_confirmation(config, &request)? {
        return Ok(tool_result.response);
    }
    let intent = resolve_turn_intent(&request);
    if let Some(response) = try_handle_timed_chat_turn(
        config,
        &request,
        &intent,
        TimedDeliverMode::Headless,
        &request.messages,
    )? {
        return Ok(response);
    }
    if let Some(tool_result) = try_handle_configured_mcp_route_turn(config, &request)? {
        return Ok(tool_result.response);
    }
    handle_chat_request(config, request)
}

pub fn handle_turn_stream_request(
    config: &ServiceConfig,
    request: ChatRequest,
    on_event: &mut dyn FnMut(TurnStreamEvent) -> Result<()>,
) -> Result<ChatResponse> {
    on_event(TurnStreamEvent::Started {
        tenant_id: request.tenant_id.clone(),
        user_id: request.user_id.clone(),
        session_id: request.session_id.clone(),
    })?;
    if let Some(tool_result) = try_handle_persisted_pending_confirmation(config, &request)? {
        on_event(TurnStreamEvent::Tool {
            tool_id: tool_result.tool_id,
            server_id: tool_result.server_id,
            tool_name: tool_result.tool_name,
        })?;
        on_event(TurnStreamEvent::Completed)?;
        return Ok(tool_result.response);
    }
    let intent = resolve_turn_intent(&request);
    if let Some(plan) = try_timed_stream_plan(config, &request, &intent, &request.messages)? {
        use std::thread;
        use std::time::Duration;
        for (index, chunk) in plan.chunks.iter().enumerate() {
            if index > 0 && plan.pause_between_chunks_ms > 0 {
                thread::sleep(Duration::from_millis(plan.pause_between_chunks_ms));
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
                response: plan.final_response.clone(),
            },
        })?;
        on_event(TurnStreamEvent::Completed)?;
        return Ok(plan.final_response);
    }
    if let Some(tool_result) = try_handle_configured_mcp_route_turn(config, &request)? {
        on_event(TurnStreamEvent::Tool {
            tool_id: tool_result.tool_id,
            server_id: tool_result.server_id,
            tool_name: tool_result.tool_name,
        })?;
        on_event(TurnStreamEvent::Completed)?;
        return Ok(tool_result.response);
    }
    let response = handle_chat_stream_request(config, request, &mut |event| {
        on_event(TurnStreamEvent::Chat { event })
    })?;
    on_event(TurnStreamEvent::Completed)?;
    Ok(response)
}
