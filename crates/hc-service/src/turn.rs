use anyhow::Result;
use hc_protocol::{ChatRequest, ChatResponse};
use serde::{Deserialize, Serialize};

use crate::{
    ServiceConfig,
    chat::{ChatStreamEvent, handle_chat_request, handle_chat_stream_request},
    tool_turn::try_handle_configured_mcp_turn,
};

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
    if let Some(tool_result) = try_handle_configured_mcp_turn(config, &request)? {
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
    if let Some(tool_result) = try_handle_configured_mcp_turn(config, &request)? {
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
