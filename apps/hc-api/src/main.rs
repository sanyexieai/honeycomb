#![recursion_limit = "256"]

use std::{
    collections::BTreeSet, convert::Infallible, env, net::SocketAddr, time::Duration,
};

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{
        Extension, Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use hc_bootstrap::{
    default_tenant_id, default_user_id, load_local_env_file, tenant_id_from_env, user_id_from_env,
    workspace_root,
};
use hc_conversation::{ConversationRepository, now_unix};
use hc_memory::{
    MemoryLayer, MemoryNamespace, MemoryRoom, MemoryRoomRepository, RoomCapabilityResolver,
    ResolvedRoomCapabilities, CapabilityRef, ToolRef, SkillRef, ScheduleRef, RoomConfig,
};
use hc_behavior::{
    BehaviorPattern, BehaviorConfig, BehaviorContext, BehaviorEngine,
    DecisionType, DecisionOption, DecisionRecord,
};
use hc_protocol::{
    AgentListResponse, AgentRouteRequest, AgentRouteResponse, ApiNamespace, ChatRequest,
    ChatResponse, DomainListResponse, ErrorResponse, HealthResponse, McpServerListResponse,
};
use hc_service::{
    ServiceConfig,
    agent::{list_agents, list_domains, route_agent},
    chat::ChatStreamEvent,
    conversation::{
        conversation_inbox_snapshot, dismiss_agent_turn_proposal, draft_agent_turn_proposal,
        mark_agent_turn_proposal_sent, process_conversation_inbox, publish_conversation_event,
    },
    index::{IndexRebuildRequest, IndexSearchRequest, rebuild_index, search_index},
    scheduler::{
        ScheduleRequest, ScheduleStatusRequest, SchedulerRunRequest, dispatch_due_scheduled_runs,
        dispatch_queued_scheduled_runs, list_scheduled_runs, list_schedules,
        queue_due_scheduled_runs, set_schedule_status, write_schedule,
    },
    tool::{
        McpToolCallRequest, McpToolCallResponse, McpToolListRequest, McpToolListResponse,
        ToolListResponse, ToolWriteRequest, ToolWriteResponse, call_configured_mcp_tool,
        list_mcp_servers, list_mcp_tools, list_tools, write_tool,
    },
    turn::{TurnStreamEvent, handle_turn_request, handle_turn_stream_request},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::{
    Stream,
    wrappers::{IntervalStream, ReceiverStream},
};
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
struct AppState {
    service: ServiceConfig,
}

const DEFAULT_API_BIND: &str = "127.0.0.1:8787";
const DEFAULT_SCHEDULER_TICK_SECONDS: u64 = 30;
const DEFAULT_SWAGGER_UI_DIST_BASE_URL: &str = "https://unpkg.com/swagger-ui-dist@5";

#[derive(Debug, Clone)]
struct ApiRuntimeConfig {
    bind_addr: SocketAddr,
    scheduler_tick_seconds: u64,
    swagger_ui_dist_base_url: String,
}

impl ApiRuntimeConfig {
    fn from_env() -> Result<Self> {
        Ok(Self {
            bind_addr: env::var("HC_API_BIND")
                .unwrap_or_else(|_| DEFAULT_API_BIND.to_owned())
                .parse()
                .context("invalid HC_API_BIND")?,
            scheduler_tick_seconds: env::var("HC_SCHEDULER_TICK_SECONDS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(DEFAULT_SCHEDULER_TICK_SECONDS),
            swagger_ui_dist_base_url: env::var("HC_SWAGGER_UI_DIST_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_SWAGGER_UI_DIST_BASE_URL.to_owned()),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct NamespaceQuery {
    #[serde(default = "default_tenant_id")]
    tenant_id: String,
    #[serde(default = "default_user_id")]
    user_id: String,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamQuery {
    #[serde(default = "default_tenant_id")]
    tenant_id: String,
    #[serde(default = "default_user_id")]
    user_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    poll_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationEventRequest {
    #[serde(default)]
    namespace: ApiNamespace,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    kind: String,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    payload: serde_json::Map<String, Value>,
    #[serde(default)]
    due_at_unix: Option<u64>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ProposalActionRequest {
    #[serde(default)]
    namespace: ApiNamespace,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    proposal_id: String,
}

/// Minimal `{ "text": "..." }` body for lightweight clients (legacy `/v1/messages` compatibility).
#[derive(Debug, Clone, Deserialize)]
struct UserMessageRequest {
    text: String,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    domain_id: Option<String>,
}

#[allow(dead_code)]
fn chat_request_from_user_message(request: UserMessageRequest) -> ChatRequest {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        request.tenant_id.clone(),
        request.user_id.clone(),
    );
    ChatRequest {
        tenant_id: Some(namespace.tenant_id.clone()),
        user_id: Some(namespace.user_id.clone()),
        session_id: normalized_optional_string(request.session_id),
        room_id: None,
        behavior_pattern: None,
        thinking_depth: None,
        input: Some(request.text),
        messages: Vec::new(),
        provider: None,
        model: None,
        system_prompt: None,
        agent_id: normalized_optional_string(request.agent_id),
        domain_id: normalized_optional_string(request.domain_id),
        active_agent_id: None,
        active_task_id: None,
        memory: hc_protocol::ApiMemoryQuery {
            namespace,
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

async fn chat_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| chat_ws_session(socket, state))
}

async fn chat_ws_session(mut socket: WebSocket, state: AppState) {
    let first_text = loop {
        match socket.recv().await {
            None => return,
            Some(Ok(Message::Text(t))) => break t,
            Some(Ok(Message::Ping(p))) => {
                let _ = socket.send(Message::Pong(p)).await;
            }
            Some(Ok(Message::Close(_))) | Some(Err(_)) => return,
            Some(Ok(_)) => {}
        }
    };

    let request: ChatRequest = match serde_json::from_str(&first_text) {
        Ok(request) => request,
        Err(error) => {
            let _ = socket
                .send(Message::Text(
                    json!({
                        "event": "chat.error",
                        "id": format!("chat.error.{}", now_unix()),
                        "data": {
                            "type": "chat_error",
                            "error": format!("invalid ChatRequest JSON: {error}"),
                        },
                    })
                    .to_string(),
                ))
                .await;
            return;
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);
    let tx_blocking = tx.clone();
    let service = state.service.clone();
    tokio::spawn(async move {
        let response = tokio::task::spawn_blocking(move || {
            let mut on_event = |event: TurnStreamEvent| -> Result<()> {
                let event_name = turn_stream_event_name(&event);
                let id = format!("{event_name}.{}", now_unix());
                let payload = serde_json::to_value(&event)?;
                let message = json!({
                    "event": event_name,
                    "id": id,
                    "data": payload,
                })
                .to_string();
                tx_blocking
                    .blocking_send(message)
                    .map_err(|_| anyhow!("chat ws client disconnected"))?;
                Ok(())
            };
            handle_turn_stream_request(&service, request, &mut on_event)
        })
        .await
        .map_err(|error| anyhow!("chat ws worker failed: {error}"))
        .and_then(|result| result);

        if let Err(error) = response {
            let _ = tx
                .send(
                    json!({
                        "event": "chat.error",
                        "id": format!("chat.error.{}", now_unix()),
                        "data": {
                            "type": "chat_error",
                            "error": error.to_string(),
                        },
                    })
                    .to_string(),
                )
                .await;
        }
    });

    while let Some(text) = rx.recv().await {
        if socket.send(Message::Text(text)).await.is_err() {
            break;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    load_local_env_file()?;
    let runtime_config = ApiRuntimeConfig::from_env()?;
    let state = AppState {
        service: ServiceConfig::new(workspace_root()),
    };
    start_scheduler_loop_if_enabled(state.service.clone(), runtime_config.scheduler_tick_seconds);
    let bind_addr = runtime_config.bind_addr;
    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/swagger-ui", get(swagger_ui))
        .route("/swagger-ui/", get(swagger_ui))
        .route("/v1/agents", get(agents))
        .route("/v1/domains", get(domains))
        .route("/v1/tools", get(tools).post(tool_upsert))
        .route("/v1/mcp/servers", get(mcp_servers))
        .route("/v1/mcp/tools", get(mcp_tools_get).post(mcp_tools_post))
        .route("/v1/mcp/call", post(mcp_call))
        .route("/v1/index/rebuild", post(index_rebuild))
        .route("/v1/index/search", post(index_search))
        .route("/v1/schedules", get(schedules).post(schedule_upsert))
        .route("/v1/schedules/status", post(schedule_status))
        .route("/v1/schedules/runs", get(schedule_runs))
        .route("/v1/schedules/run-due", post(schedule_run_due))
        .route("/v1/schedules/dispatch-due", post(schedule_dispatch_due))
        .route(
            "/v1/schedules/dispatch-queued",
            post(schedule_dispatch_queued),
        )
        .route("/v1/conversation/inbox", get(conversation_inbox))
        .route("/v1/conversation/stream", get(conversation_stream))
        .route("/v1/conversation/events", post(conversation_event))
        .route("/v1/conversation/process", post(conversation_process))
        .route(
            "/v1/conversation/proposals/draft",
            post(conversation_proposal_draft),
        )
        .route(
            "/v1/conversation/proposals/sent",
            post(conversation_proposal_sent),
        )
        .route(
            "/v1/conversation/proposals/dismiss",
            post(conversation_proposal_dismiss),
        )
        .route("/v1/agents/route", post(agent_route))
        .route("/v1/chat", post(chat))
        .route("/v1/chat/stream", post(chat_stream))
        .route("/v1/chat/ws", get(chat_ws))
        .route("/v1/memory/rooms", get(memory_rooms).post(memory_room_create))
        .route("/v1/memory/rooms/:room_id", get(memory_room_get).put(memory_room_update))
        .route("/v1/memory/rooms/:room_id/capabilities", get(memory_room_capabilities))
        .route("/v1/memory/rooms/:room_id/capabilities/inherit", post(memory_room_inherit_capability))
        .route("/v1/memory/rooms/:room_id/tools/inherit", post(memory_room_inherit_tool))
        .route("/v1/memory/rooms/:room_id/skills/inherit", post(memory_room_inherit_skill))
        .route("/v1/memory/rooms/:room_id/schedules/inherit", post(memory_room_inherit_schedule))
        .route("/v1/behavior/patterns", get(behavior_patterns))
        .route("/v1/behavior/patterns/:pattern", get(behavior_pattern_get))
        .route("/v1/behavior/patterns/:pattern/test", post(behavior_pattern_test))
        .layer(Extension(runtime_config.swagger_ui_dist_base_url))
        .with_state(state);

    info!(%bind_addr, "hc-api listening");
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;
    axum::serve(listener, app)
        .await
        .context("api server failed")
}

async fn conversation_inbox(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<hc_service::conversation::ConversationInboxSnapshot>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let _session_id = normalized_optional_string(query.session_id)
        .unwrap_or_else(|| default_session_id(&namespace));
    let response = tokio::task::spawn_blocking(move || {
        conversation_inbox_snapshot(&state.service, namespace, None)
    })
    .await
    .map_err(|error| ApiError(anyhow!("conversation inbox worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn conversation_stream(
    State(state): State<AppState>,
    Query(query): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let session_id = normalized_optional_string(query.session_id)
        .unwrap_or_else(|| default_session_id(&namespace));
    let poll_ms = query.poll_ms.unwrap_or(1000).clamp(250, 30_000);
    let repository = ConversationRepository::with_namespace(
        state.service.workspace_root.clone(),
        hc_store::store::WorkspaceNamespace::new(
            namespace.tenant_id.clone(),
            namespace.user_id.clone(),
        ),
    );
    let mut seen = BTreeSet::<String>::new();
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_millis(poll_ms)))
        .flat_map(move |_| {
            let repository = repository.clone();
            let session_id = session_id.clone();
            let mut events = Vec::new();
            match conversation_stream_items(&repository, &session_id, &mut seen) {
                Ok(items) => {
                    for item in items {
                        if let Ok(event) = sse_json_event(&item.event, item.id, item.payload) {
                            events.push(Ok(event));
                        }
                    }
                }
                Err(error) => {
                    events.push(Ok(Event::default().event("error").data(
                        json!({
                            "message": error.to_string(),
                        })
                        .to_string(),
                    )));
                }
            }
            tokio_stream::iter(events)
        });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}

async fn conversation_event(
    State(state): State<AppState>,
    Json(request): Json<ConversationEventRequest>,
) -> Result<Json<hc_conversation::ConversationEvent>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let mut event = hc_conversation::ConversationEvent::new(request.kind);
    event.room_id = normalized_optional_string(request.room_id)
        .or_else(|| normalized_optional_string(request.session_id))
        .or_else(|| Some(default_session_id(&namespace)));
    event.agent_id = request.agent_id;
    event.payload = request.payload;
    event.due_at_unix = request.due_at_unix;
    if !request.tags.is_empty() {
        event.tags = request.tags;
    }
    event.notes = request.notes;
    let response = tokio::task::spawn_blocking(move || {
        publish_conversation_event(&state.service, namespace, event)
    })
    .await
    .map_err(|error| ApiError(anyhow!("conversation event worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn conversation_process(
    State(state): State<AppState>,
    Json(mut namespace): Json<ApiNamespace>,
) -> Result<Json<hc_service::conversation::ConversationProcessReport>, ApiError> {
    namespace = normalized_request_namespace(namespace, None, None);
    let response = tokio::task::spawn_blocking(move || {
        process_conversation_inbox(&state.service, namespace, None)
    })
    .await
    .map_err(|error| ApiError(anyhow!("conversation process worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn conversation_proposal_draft(
    State(state): State<AppState>,
    Json(request): Json<ProposalActionRequest>,
) -> Result<Json<hc_service::conversation::AgentTurnDraft>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let _session_id = normalized_optional_string(request.session_id)
        .unwrap_or_else(|| default_session_id(&namespace));
    let response = tokio::task::spawn_blocking(move || {
        draft_agent_turn_proposal(&state.service, namespace, &request.proposal_id)
    })
    .await
    .map_err(|error| {
        ApiError(anyhow!(
            "conversation proposal draft worker failed: {error}"
        ))
    })?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn conversation_proposal_sent(
    State(state): State<AppState>,
    Json(request): Json<ProposalActionRequest>,
) -> Result<Json<hc_conversation::AgentTurnProposal>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let _session_id = normalized_optional_string(request.session_id)
        .unwrap_or_else(|| default_session_id(&namespace));
    let response = tokio::task::spawn_blocking(move || {
        mark_agent_turn_proposal_sent(&state.service, namespace, &request.proposal_id)
    })
    .await
    .map_err(|error| ApiError(anyhow!("conversation proposal sent worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn conversation_proposal_dismiss(
    State(state): State<AppState>,
    Json(request): Json<ProposalActionRequest>,
) -> Result<Json<hc_conversation::AgentTurnProposal>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let _session_id = normalized_optional_string(request.session_id)
        .unwrap_or_else(|| default_session_id(&namespace));
    let response = tokio::task::spawn_blocking(move || {
        dismiss_agent_turn_proposal(&state.service, namespace, &request.proposal_id)
    })
    .await
    .map_err(|error| {
        ApiError(anyhow!(
            "conversation proposal dismiss worker failed: {error}"
        ))
    })?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

fn start_scheduler_loop_if_enabled(service: ServiceConfig, tick_seconds: u64) {
    if !env_flag("HC_SCHEDULER_ENABLED") {
        return;
    }
    let namespace = ApiNamespace {
        tenant_id: tenant_id_from_env(),
        user_id: user_id_from_env(),
    };
    info!(
        tenant_id = %namespace.tenant_id,
        user_id = %namespace.user_id,
        tick_seconds,
        "hc-api scheduler enabled"
    );
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(tick_seconds));
        loop {
            interval.tick().await;
            let service = service.clone();
            let namespace = namespace.clone();
            let result = tokio::task::spawn_blocking(move || {
                dispatch_due_scheduled_runs(&service, namespace, None)
            })
            .await;
            match result {
                Ok(Ok(report)) => {
                    if !report.receipts.is_empty() {
                        info!(
                            dispatched = report.receipts.len(),
                            queued = report.queued_count,
                            "scheduler dispatched due runs"
                        );
                    }
                }
                Ok(Err(error)) => warn!(%error, "scheduler tick failed"),
                Err(error) => warn!(%error, "scheduler worker failed"),
            }
        }
    });
}

fn normalized_request_namespace(
    mut namespace: ApiNamespace,
    tenant_id: Option<String>,
    user_id: Option<String>,
) -> ApiNamespace {
    if let Some(tenant_id) = normalized_optional_string(tenant_id) {
        namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = normalized_optional_string(user_id) {
        namespace.user_id = user_id;
    }
    if namespace.tenant_id.trim().is_empty() {
        namespace.tenant_id = default_tenant_id();
    }
    if namespace.user_id.trim().is_empty() {
        namespace.user_id = default_user_id();
    }
    namespace
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn default_session_id(namespace: &ApiNamespace) -> String {
    format!(
        "session.{}.{}.default",
        namespace.tenant_id, namespace.user_id
    )
}

#[derive(Debug)]
struct ConversationStreamItem {
    event: String,
    id: String,
    payload: Value,
}

fn conversation_stream_items(
    repository: &ConversationRepository,
    session_id: &str,
    seen: &mut BTreeSet<String>,
) -> Result<Vec<ConversationStreamItem>> {
    let mut items = Vec::new();
    for event in repository.list_events()? {
        if !room_matches(event.room_id.as_deref(), session_id) {
            continue;
        }
        let key = format!(
            "event:{}:{:?}:{}",
            event.id, event.status, event.relative_path
        );
        if seen.insert(key.clone()) {
            items.push(ConversationStreamItem {
                event: format!("conversation.event.{}", event.kind),
                id: key,
                payload: json!({
                    "type": "conversation_event",
                    "event": event,
                }),
            });
        }
    }
    for followup in repository.list_followups()? {
        if !room_matches(followup.room_id.as_deref(), session_id) {
            continue;
        }
        let key = format!(
            "followup:{}:{:?}:{}",
            followup.id, followup.status, followup.relative_path
        );
        if seen.insert(key.clone()) {
            items.push(ConversationStreamItem {
                event: "conversation.followup".to_owned(),
                id: key,
                payload: json!({
                    "type": "pending_followup",
                    "followup": followup,
                }),
            });
        }
    }
    for proposal in repository.list_proposals()? {
        if !room_matches(proposal.room_id.as_deref(), session_id) {
            continue;
        }
        let key = format!(
            "proposal:{}:{:?}:{}",
            proposal.id, proposal.status, proposal.relative_path
        );
        if seen.insert(key.clone()) {
            items.push(ConversationStreamItem {
                event: "conversation.proposal".to_owned(),
                id: key,
                payload: json!({
                    "type": "agent_turn_proposal",
                    "proposal": proposal,
                }),
            });
        }
    }
    Ok(items)
}

fn room_matches(room_id: Option<&str>, session_id: &str) -> bool {
    room_id.is_none_or(|room_id| room_id == session_id)
}

fn sse_json_event(event: &str, id: String, payload: Value) -> Result<Event> {
    Ok(Event::default()
        .event(event.to_owned())
        .id(id)
        .data(payload.to_string()))
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        service: "hc-api".to_owned(),
    })
}

async fn openapi() -> Json<Value> {
    Json(openapi_document())
}

async fn swagger_ui(Extension(swagger_ui_dist_base_url): Extension<String>) -> Html<String> {
    Html(swagger_ui_html(&swagger_ui_dist_base_url))
}

async fn chat(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    let (enhanced_request, decision) = enhance_chat_request_with_room_capabilities(&state, &request).await?;
    let service = state.service.clone();
    let mut response =
        tokio::task::spawn_blocking(move || handle_turn_request(&service, enhanced_request))
            .await
            .map_err(|error| ApiError(anyhow!("chat worker failed: {error}")))?
            .map_err(ApiError::from)?;
    
    // 添加房间信息到响应
    response.room_id = request.room_id.clone();
    if let Some(room_id) = &request.room_id {
        let capabilities_info = get_room_capabilities_info(&state, room_id).await?;
        response.room_capabilities_used = capabilities_info.capabilities;
        response.room_tools_used = capabilities_info.tools;
        response.room_skills_used = capabilities_info.skills;
    }
    
    // 添加行为模式信息到响应
    if let Some(decision) = decision {
        response.behavior_pattern_used = Some(format!("{:?}", decision.behavior_pattern));
        response.decision_reasoning = Some(decision.reasoning);
        response.decision_confidence = Some(decision.confidence);
    }
    
    Ok(Json(response))
}

async fn chat_stream(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(16);
    let service = state.service.clone();
    
    // 增强请求以包含房间能力
    let (enhanced_request, _decision) = match enhance_chat_request_with_room_capabilities(&state, &request).await {
        Ok((req, dec)) => (req, dec),
        Err(error) => {
            let tx_clone = tx.clone();
            tokio::spawn(async move {
                let _ = tx_clone
                    .send(Ok(Event::default().event("chat.error").data(
                        json!({
                            "type": "chat_error",
                            "error": format!("Room capability enhancement failed: {}", error.0),
                        })
                        .to_string(),
                    )))
                    .await;
            });
            return Sse::new(ReceiverStream::new(rx)).keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(15))
                    .text("keep-alive"),
            );
        }
    };
    
    tokio::spawn(async move {
        let tx_for_events = tx.clone();
        let response = tokio::task::spawn_blocking(move || {
            let mut on_event = |event: TurnStreamEvent| -> Result<()> {
                let event_name = turn_stream_event_name(&event);
                let id = format!("{event_name}.{}", now_unix());
                let payload = serde_json::to_value(event)?;
                tx_for_events
                    .blocking_send(Ok(sse_json_event(event_name, id, payload)?))
                    .map_err(|error| anyhow!("chat stream client disconnected: {error}"))?;
                Ok(())
            };
            handle_turn_stream_request(&service, enhanced_request, &mut on_event)
        })
        .await
        .map_err(|error| anyhow!("chat stream worker failed: {error}"))
        .and_then(|result| result);

        if let Err(error) = response {
            let _ = tx
                .send(Ok(Event::default().event("chat.error").data(
                    json!({
                        "type": "chat_error",
                        "error": error.to_string(),
                    })
                    .to_string(),
                )))
                .await;
        }
    });

    Sse::new(ReceiverStream::new(rx)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

fn chat_stream_event_name(event: &ChatStreamEvent) -> &'static str {
    match event {
        ChatStreamEvent::Started { .. } => "chat.started",
        ChatStreamEvent::Delta { .. } => "chat.delta",
        ChatStreamEvent::Completed { .. } => "chat.completed",
    }
}

fn turn_stream_event_name(event: &TurnStreamEvent) -> &'static str {
    match event {
        TurnStreamEvent::Started { .. } => "turn.started",
        TurnStreamEvent::Tool { .. } => "turn.tool",
        TurnStreamEvent::Completed => "turn.completed",
        TurnStreamEvent::Chat { event } => chat_stream_event_name(event),
    }
}

async fn agents(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<AgentListResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let response = tokio::task::spawn_blocking(move || list_agents(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("agent worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn domains(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<DomainListResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let response = tokio::task::spawn_blocking(move || list_domains(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("domain worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn tools(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<ToolListResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let response = tokio::task::spawn_blocking(move || list_tools(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("tool list worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn tool_upsert(
    State(state): State<AppState>,
    Json(request): Json<ToolWriteRequest>,
) -> Result<Json<ToolWriteResponse>, ApiError> {
    let response = tokio::task::spawn_blocking(move || write_tool(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("tool write worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn mcp_servers(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<McpServerListResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let response = tokio::task::spawn_blocking(move || list_mcp_servers(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("mcp server worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn mcp_tools_get(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<McpToolListResponse>, ApiError> {
    let request = McpToolListRequest {
        namespace: ApiNamespace::default(),
        tenant_id: Some(query.tenant_id),
        user_id: Some(query.user_id),
        refresh: false,
        server_id: None,
    };
    let response = tokio::task::spawn_blocking(move || list_mcp_tools(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("mcp tool list worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn mcp_tools_post(
    State(state): State<AppState>,
    Json(request): Json<McpToolListRequest>,
) -> Result<Json<McpToolListResponse>, ApiError> {
    let response = tokio::task::spawn_blocking(move || list_mcp_tools(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("mcp tool list worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn mcp_call(
    State(state): State<AppState>,
    Json(request): Json<McpToolCallRequest>,
) -> Result<Json<McpToolCallResponse>, ApiError> {
    let response =
        tokio::task::spawn_blocking(move || call_configured_mcp_tool(&state.service, request))
            .await
            .map_err(|error| ApiError(anyhow!("mcp call worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn index_rebuild(
    State(state): State<AppState>,
    Json(request): Json<IndexRebuildRequest>,
) -> Result<Json<hc_service::index::IndexRebuildResponse>, ApiError> {
    let response = tokio::task::spawn_blocking(move || rebuild_index(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("index rebuild worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn index_search(
    State(state): State<AppState>,
    Json(request): Json<IndexSearchRequest>,
) -> Result<Json<hc_service::index::IndexSearchResponse>, ApiError> {
    let response = tokio::task::spawn_blocking(move || search_index(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("index search worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedules(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<Vec<hc_scheduler::ScheduledTask>>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let response = tokio::task::spawn_blocking(move || list_schedules(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("schedule list worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedule_upsert(
    State(state): State<AppState>,
    Json(request): Json<ScheduleRequest>,
) -> Result<Json<hc_service::scheduler::ScheduleWriteResponse>, ApiError> {
    let response = tokio::task::spawn_blocking(move || write_schedule(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("schedule write worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedule_status(
    State(state): State<AppState>,
    Json(request): Json<ScheduleStatusRequest>,
) -> Result<Json<hc_scheduler::ScheduledTask>, ApiError> {
    let response =
        tokio::task::spawn_blocking(move || set_schedule_status(&state.service, request))
            .await
            .map_err(|error| ApiError(anyhow!("schedule status worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedule_runs(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<Vec<hc_scheduler::ScheduledRun>>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let response =
        tokio::task::spawn_blocking(move || list_scheduled_runs(&state.service, namespace))
            .await
            .map_err(|error| ApiError(anyhow!("schedule run list worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedule_run_due(
    State(state): State<AppState>,
    Json(request): Json<SchedulerRunRequest>,
) -> Result<Json<Vec<hc_scheduler::ScheduledRun>>, ApiError> {
    let response =
        tokio::task::spawn_blocking(move || queue_due_scheduled_runs(&state.service, request))
            .await
            .map_err(|error| ApiError(anyhow!("schedule run-due worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedule_dispatch_due(
    State(state): State<AppState>,
    Json(request): Json<SchedulerRunRequest>,
) -> Result<Json<hc_service::scheduler::SchedulerDispatchReport>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let response = tokio::task::spawn_blocking(move || {
        dispatch_due_scheduled_runs(&state.service, namespace, request.now_unix)
    })
    .await
    .map_err(|error| ApiError(anyhow!("schedule dispatch-due worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn schedule_dispatch_queued(
    State(state): State<AppState>,
    Json(request): Json<SchedulerRunRequest>,
) -> Result<Json<hc_service::scheduler::SchedulerDispatchReport>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let response = tokio::task::spawn_blocking(move || {
        dispatch_queued_scheduled_runs(&state.service, namespace, request.now_unix)
    })
    .await
    .map_err(|error| ApiError(anyhow!("schedule dispatch-queued worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn agent_route(
    State(state): State<AppState>,
    Json(request): Json<AgentRouteRequest>,
) -> Result<Json<AgentRouteResponse>, ApiError> {
    let response = tokio::task::spawn_blocking(move || route_agent(&state.service, request))
        .await
        .map_err(|error| ApiError(anyhow!("agent route worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

fn openapi_document() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Honeycomb API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "HTTP API for Honeycomb chat, memory recall, and tool-aware workflows."
        },
        "servers": [
            {
                "url": "/",
                "description": "Same-origin Honeycomb API server"
            }
        ],
        "paths": {
            "/health": {
                "get": {
                    "summary": "Health check",
                    "operationId": "health",
                    "responses": {
                        "200": {
                            "description": "Service is healthy",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/HealthResponse" }
                                }
                            }
                        }
                    }
                }
            },
            "/v1/chat": {
                "post": {
                    "summary": "Generate a context-aware chat response",
                    "operationId": "chat",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ChatRequest" },
                                "examples": {
                                    "simple": {
                                        "summary": "Single-turn Chinese identity question",
                                        "value": {
                                            "input": "你叫什么",
                                            "memory": {
                                                "namespace": {
                                                    "tenant_id": "local",
                                                    "user_id": "default"
                                                },
                                                "limit": 8
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Generated chat response",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ChatResponse" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "401": { "$ref": "#/components/responses/Unauthorized" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/chat/stream": {
                "post": {
                    "summary": "Generate a chat response over Server-Sent Events",
                    "operationId": "streamChat",
                    "description": "Streams chat lifecycle events and model deltas. Events include turn.started, turn.tool, turn.completed, chat.started, chat.delta, chat.completed, and chat.error.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/ChatRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "SSE stream of chat lifecycle events",
                            "content": {
                                "text/event-stream": {
                                    "schema": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            },
            "/v1/chat/ws": {
                "get": {
                    "summary": "WebSocket chat stream",
                    "operationId": "streamChatWebSocket",
                    "description": "Open WebSocket upgrade on GET. After the handshake, send one text frame containing a ChatRequest JSON body; the server streams JSON event messages with the same payload shape as SSE /v1/chat/stream (fields event, id, data). OpenAPI cannot fully describe WebSocket frames; use this entry as a capability marker."
                }
            },
            "/v1/agents": {
                "get": {
                    "summary": "List workspace agent profiles",
                    "operationId": "listAgents",
                    "parameters": [
                        {
                            "name": "tenant_id",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "local"
                            }
                        },
                        {
                            "name": "user_id",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "default"
                            }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Agent profiles available in the workspace namespace",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/AgentListResponse" }
                                }
                            }
                        },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/domains": {
                "get": {
                    "summary": "List workspace domain profiles",
                    "operationId": "listDomains",
                    "parameters": [
                        {
                            "name": "tenant_id",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "local"
                            }
                        },
                        {
                            "name": "user_id",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "default"
                            }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Domain profiles available in the workspace namespace",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/DomainListResponse" }
                                }
                            }
                        },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/tools": {
                "get": {
                    "summary": "List workspace tool definitions",
                    "operationId": "listTools",
                    "responses": {
                        "200": { "description": "Tool definitions", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                },
                "post": {
                    "summary": "Create or update a workspace tool definition",
                    "operationId": "upsertTool",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Written tool", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/mcp/servers": {
                "get": {
                    "summary": "List workspace MCP server definitions",
                    "operationId": "listMcpServers",
                    "parameters": [
                        {
                            "name": "tenant_id",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "local"
                            }
                        },
                        {
                            "name": "user_id",
                            "in": "query",
                            "required": false,
                            "schema": {
                                "type": "string",
                                "default": "default"
                            }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "MCP servers available in the workspace namespace",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/McpServerListResponse" }
                                }
                            }
                        },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/mcp/tools": {
                "get": {
                    "summary": "List cached MCP tools",
                    "operationId": "listMcpTools",
                    "responses": {
                        "200": { "description": "MCP tools", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                },
                "post": {
                    "summary": "List or refresh MCP tools",
                    "operationId": "listOrRefreshMcpTools",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "MCP tools", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/mcp/call": {
                "post": {
                    "summary": "Call a configured MCP tool with runtime context",
                    "operationId": "callMcpTool",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "MCP call result", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/index/rebuild": {
                "post": {
                    "summary": "Rebuild markdown and optional vector indexes",
                    "operationId": "rebuildIndex",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Index rebuild report", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/index/search": {
                "post": {
                    "summary": "Search markdown or vector indexes",
                    "operationId": "searchIndex",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Index search results", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules": {
                "get": {
                    "summary": "List schedules",
                    "operationId": "listSchedules",
                    "responses": {
                        "200": { "description": "Schedules", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                },
                "post": {
                    "summary": "Create or update a schedule",
                    "operationId": "upsertSchedule",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Written schedule", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/status": {
                "post": {
                    "summary": "Set schedule status",
                    "operationId": "setScheduleStatus",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Updated schedule", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/runs": {
                "get": {
                    "summary": "List scheduled runs",
                    "operationId": "listScheduledRuns",
                    "responses": {
                        "200": { "description": "Scheduled runs", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/run-due": {
                "post": {
                    "summary": "Queue due scheduled runs",
                    "operationId": "queueDueScheduledRuns",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Queued runs", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/dispatch-due": {
                "post": {
                    "summary": "Queue and dispatch due scheduled runs",
                    "operationId": "dispatchDueScheduledRuns",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Dispatch report", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/dispatch-queued": {
                "post": {
                    "summary": "Dispatch already queued scheduled runs",
                    "operationId": "dispatchQueuedScheduledRuns",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } },
                    "responses": {
                        "200": { "description": "Dispatch report", "content": { "application/json": { "schema": { "type": "object" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/conversation/stream": {
                "get": {
                    "summary": "Subscribe to conversation events with Server-Sent Events",
                    "operationId": "streamConversation",
                    "parameters": [
                        { "name": "tenant_id", "in": "query", "required": false, "schema": { "type": "string", "default": "local" } },
                        { "name": "user_id", "in": "query", "required": false, "schema": { "type": "string", "default": "default" } },
                        { "name": "session_id", "in": "query", "required": false, "schema": { "type": "string" } },
                        { "name": "poll_ms", "in": "query", "required": false, "schema": { "type": "integer", "default": 1000, "minimum": 250 } }
                    ],
                    "responses": {
                        "200": {
                            "description": "SSE stream of conversation events, followups, and proposals",
                            "content": {
                                "text/event-stream": {
                                    "schema": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            },
            "/v1/agents/route": {
                "post": {
                    "summary": "Route input to the best matching agent profile",
                    "operationId": "routeAgent",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/AgentRouteRequest" },
                                "examples": {
                                    "identity": {
                                        "value": {
                                            "input": "你叫什么",
                                            "namespace": {
                                                "tenant_id": "local",
                                                "user_id": "default"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Ranked routing candidates",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/AgentRouteResponse" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/behavior/patterns": {
                "get": {
                    "summary": "List available behavior patterns",
                    "operationId": "listBehaviorPatterns",
                    "responses": {
                        "200": {
                            "description": "List of behavior patterns",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {
                                            "patterns": {
                                                "type": "array",
                                                "items": { "$ref": "#/components/schemas/BehaviorPattern" }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/behavior/patterns/{pattern}": {
                "get": {
                    "summary": "Get details of a specific behavior pattern",
                    "operationId": "getBehaviorPattern",
                    "parameters": [
                        {
                            "name": "pattern",
                            "in": "path",
                            "required": true,
                            "schema": {
                                "type": "string",
                                "enum": ["passive", "stable", "learning", "creative", "adaptive"]
                            }
                        }
                    ],
                    "responses": {
                        "200": {
                            "description": "Behavior pattern details",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/BehaviorPatternDetails" }
                                }
                            }
                        },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/behavior/patterns/{pattern}/test": {
                "post": {
                    "summary": "Test behavior pattern decision-making",
                    "operationId": "testBehaviorPattern",
                    "parameters": [
                        {
                            "name": "pattern",
                            "in": "path",
                            "required": true,
                            "schema": {
                                "type": "string",
                                "enum": ["passive", "stable", "learning", "creative", "adaptive"]
                            }
                        }
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/BehaviorTestRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Test decision result",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/DecisionRecord" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "404": { "$ref": "#/components/responses/NotFound" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "ApiMessageRole": {
                    "type": "string",
                    "enum": ["system", "user", "assistant", "tool"]
                },
                "ApiChatMessage": {
                    "type": "object",
                    "required": ["role", "content"],
                    "properties": {
                        "role": { "$ref": "#/components/schemas/ApiMessageRole" },
                        "content": { "type": "string" },
                        "name": { "type": "string" }
                    }
                },
                "ApiNamespace": {
                    "type": "object",
                    "properties": {
                        "tenant_id": {
                            "type": "string",
                            "default": "local"
                        },
                        "user_id": {
                            "type": "string",
                            "default": "default"
                        }
                    }
                },
                "ApiMemoryQuery": {
                    "type": "object",
                    "properties": {
                        "namespace": { "$ref": "#/components/schemas/ApiNamespace" },
                        "scope": {
                            "type": "string",
                            "enum": ["global", "persona", "session", "instance", "project", "task"]
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["summary", "decision", "preference", "knowledge", "workflow_memory"]
                        },
                        "tag": { "type": "string" },
                        "text": { "type": "string" },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "default": 8
                        }
                    }
                },
                "ChatRequest": {
                    "type": "object",
                    "properties": {
                        "tenant_id": {
                            "type": "string",
                            "default": "local",
                            "description": "Optional tenant id. Empty values use the default tenant."
                        },
                        "user_id": {
                            "type": "string",
                            "default": "default",
                            "description": "Optional user id. Empty values use the default user."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional conversation/session id. Empty values use a namespace-scoped default session."
                        },
                        "room_id": {
                            "type": "string",
                            "description": "Optional memory room id. When provided, the chat will use room-specific capabilities and context."
                        },
                        "input": {
                            "type": "string",
                            "description": "Convenience single-turn user message. Appended after messages when both are present."
                        },
                        "messages": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ApiChatMessage" }
                        },
                        "provider": { "type": "string" },
                        "model": { "type": "string" },
                        "system_prompt": { "type": "string" },
                        "agent_id": {
                            "type": "string",
                            "description": "Explicit agent profile id. When omitted, the service routes input to an agent."
                        },
                        "domain_id": {
                            "type": "string",
                            "description": "Optional domain hint for routing."
                        },
                        "active_agent_id": {
                            "type": "string",
                            "description": "Agent id from the active task/session context."
                        },
                        "active_task_id": {
                            "type": "string",
                            "description": "Active task id used by future task-aware routing."
                        },
                        "memory": { "$ref": "#/components/schemas/ApiMemoryQuery" },
                        "temperature": {
                            "type": "number",
                            "format": "float"
                        },
                        "max_output_tokens": {
                            "type": "integer",
                            "minimum": 1
                        }
                    }
                },
                "MemoryRef": {
                    "type": "object",
                    "required": [
                        "id",
                        "title",
                        "summary",
                        "scope",
                        "kind",
                        "source_kind",
                        "confidence_milli",
                        "tags"
                    ],
                    "properties": {
                        "id": { "type": "string" },
                        "title": { "type": "string" },
                        "summary": { "type": "string" },
                        "scope": { "type": "string" },
                        "kind": { "type": "string" },
                        "source_kind": { "type": "string" },
                        "confidence_milli": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 1000
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "room_id": { "type": "string" }
                    }
                },
                "ChatResponse": {
                    "type": "object",
                    "required": [
                        "message",
                        "model",
                        "provider",
                        "recalled_memories",
                        "synthesized_prompt_asset_count"
                    ],
                    "properties": {
                        "message": { "$ref": "#/components/schemas/ApiChatMessage" },
                        "model": { "type": "string" },
                        "provider": { "type": "string" },
                        "tenant_id": { "type": "string" },
                        "user_id": { "type": "string" },
                        "session_id": { "type": "string" },
                        "room_id": { "type": "string" },
                        "selected_agent_id": { "type": "string" },
                        "selected_domain_id": { "type": "string" },
                        "recalled_memories": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/MemoryRef" }
                        },
                        "synthesized_prompt_asset_count": {
                            "type": "integer",
                            "minimum": 0
                        },
                        "room_capabilities_used": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "List of room capabilities that were used in this response"
                        },
                        "room_tools_used": {
                            "type": "array", 
                            "items": { "type": "string" },
                            "description": "List of room tools that were available for this response"
                        },
                        "room_skills_used": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "List of room skills that were used in this response"
                        }
                    }
                },
                "HealthResponse": {
                    "type": "object",
                    "required": ["status", "service"],
                    "properties": {
                        "status": { "type": "string", "example": "ok" },
                        "service": { "type": "string", "example": "hc-api" }
                    }
                },
                "AgentProfileSummary": {
                    "type": "object",
                    "required": [
                        "id",
                        "name",
                        "kind",
                        "priority",
                        "intent_hints",
                        "tool_refs",
                        "memory_scope_refs",
                        "tags"
                    ],
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": ["domain_service", "task_role", "router", "guard", "other"]
                        },
                        "project_id": { "type": "string" },
                        "domain_id": { "type": "string" },
                        "priority": { "type": "integer" },
                        "intent_hints": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "tool_refs": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "memory_scope_refs": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                },
                "AgentListResponse": {
                    "type": "object",
                    "required": ["agents"],
                    "properties": {
                        "agents": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/AgentProfileSummary" }
                        }
                    }
                },
                "DomainProfileSummary": {
                    "type": "object",
                    "required": [
                        "id",
                        "name",
                        "kind",
                        "priority",
                        "intent_hints",
                        "tool_refs",
                        "memory_scope_refs",
                        "tags"
                    ],
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": ["service", "project_area", "safety", "other"]
                        },
                        "project_id": { "type": "string" },
                        "priority": { "type": "integer" },
                        "intent_hints": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "default_agent_id": { "type": "string" },
                        "tool_refs": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "memory_scope_refs": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                },
                "DomainListResponse": {
                    "type": "object",
                    "required": ["domains"],
                    "properties": {
                        "domains": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/DomainProfileSummary" }
                        }
                    }
                },
                "McpServerSummary": {
                    "type": "object",
                    "required": ["id", "name", "description", "transport", "command", "tags"],
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "transport": { "type": "string" },
                        "url": { "type": "string" },
                        "command": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                },
                "McpServerListResponse": {
                    "type": "object",
                    "required": ["servers"],
                    "properties": {
                        "servers": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/McpServerSummary" }
                        }
                    }
                },
                "AgentRouteRequest": {
                    "type": "object",
                    "required": ["input"],
                    "properties": {
                        "input": { "type": "string" },
                        "namespace": { "$ref": "#/components/schemas/ApiNamespace" },
                        "project_id": { "type": "string" },
                        "domain_id": { "type": "string" },
                        "active_agent_id": { "type": "string" },
                        "active_task_id": { "type": "string" },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 20,
                            "default": 5
                        }
                    }
                },
                "AgentRouteCandidate": {
                    "type": "object",
                    "required": ["agent_id", "score", "reasons"],
                    "properties": {
                        "agent_id": { "type": "string" },
                        "domain_id": { "type": "string" },
                        "score": { "type": "integer" },
                        "reasons": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                },
                "AgentRouteResponse": {
                    "type": "object",
                    "required": ["candidates"],
                    "properties": {
                        "selected_agent_id": { "type": "string" },
                        "selected_domain_id": { "type": "string" },
                        "candidates": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/AgentRouteCandidate" }
                        }
                    }
                },
                "ErrorResponse": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {
                        "error": { "type": "string" }
                    }
                },
                "BehaviorPattern": {
                    "type": "object",
                    "required": ["name", "description", "risk_tolerance", "innovation_tendency", "proactivity"],
                    "properties": {
                        "name": { "type": "string", "enum": ["passive", "stable", "learning", "creative", "adaptive"] },
                        "description": { "type": "string" },
                        "risk_tolerance": { "type": "number" },
                        "innovation_tendency": { "type": "number" },
                        "proactivity": { "type": "number" }
                    }
                },
                "BehaviorPatternDetails": {
                    "type": "object",
                    "required": ["pattern", "description", "attributes", "config"],
                    "properties": {
                        "pattern": { "type": "string" },
                        "description": { "type": "string" },
                        "attributes": {
                            "type": "object",
                            "properties": {
                                "risk_tolerance": { "type": "number" },
                                "innovation_tendency": { "type": "number" },
                                "proactivity": { "type": "number" }
                            }
                        },
                        "config": {
                            "type": "object",
                            "properties": {
                                "thinking_depth": { "type": "integer" },
                                "enable_metacognition": { "type": "boolean" },
                                "learning_rate": { "type": "number" }
                            }
                        }
                    }
                },
                "BehaviorTestRequest": {
                    "type": "object",
                    "properties": {
                        "context": {
                            "type": "object",
                            "properties": {
                                "user_id": { "type": "string" },
                                "room_id": { "type": "string" },
                                "task_type": { "type": "string" },
                                "complexity": { "type": "number" },
                                "success_rate": { "type": "number" },
                                "time_pressure": { "type": "number" },
                                "available_tools_count": { "type": "integer" }
                            }
                        },
                        "config": {
                            "type": "object",
                            "properties": {
                                "thinking_depth": { "type": "integer" },
                                "enable_metacognition": { "type": "boolean" },
                                "learning_rate": { "type": "number" }
                            }
                        }
                    }
                },
                "DecisionRecord": {
                    "type": "object",
                    "required": ["context", "behavior_pattern", "decision_type", "options_considered", "chosen_option", "reasoning", "confidence"],
                    "properties": {
                        "context": { "type": "object" },
                        "behavior_pattern": { "type": "string" },
                        "decision_type": { "type": "string" },
                        "options_considered": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/DecisionOption" }
                        },
                        "chosen_option": { "type": "string" },
                        "reasoning": { "type": "string" },
                        "confidence": { "type": "number" },
                        "execution_result": { "type": "object" }
                    }
                },
                "DecisionOption": {
                    "type": "object",
                    "required": ["id", "description"],
                    "properties": {
                        "id": { "type": "string" },
                        "description": { "type": "string" },
                        "pros": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "cons": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "estimated_effort": { "type": "number" },
                        "success_probability": { "type": "number" },
                        "innovation_level": { "type": "number" },
                        "risk_level": { "type": "number" }
                    }
                }
            },
            "responses": {
                "BadRequest": {
                    "description": "Invalid request",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/ErrorResponse" }
                        }
                    }
                },
                "Unauthorized": {
                    "description": "Provider credentials are missing or invalid",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/ErrorResponse" }
                        }
                    }
                },
                "NotFound": {
                    "description": "Resource not found",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/ErrorResponse" }
                        }
                    }
                },
                "InternalError": {
                    "description": "Unexpected server error",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/ErrorResponse" }
                        }
                    }
                }
            }
        }
    })
}

fn swagger_ui_html(swagger_ui_dist_base_url: &str) -> String {
    let spec = openapi_document();
    let spec_json = serde_json::to_string(&spec).expect("openapi document should serialize");
    SWAGGER_UI_HTML
        .replace("__HONEYCOMB_OPENAPI_SPEC__", &spec_json)
        .replace("__SWAGGER_UI_DIST_BASE_URL__", swagger_ui_dist_base_url.trim_end_matches('/'))
}

const SWAGGER_UI_HTML: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Honeycomb API Swagger</title>
    <link rel="stylesheet" href="__SWAGGER_UI_DIST_BASE_URL__/swagger-ui.css" />
    <style>
      body {
        margin: 0;
        background: #f7f8fa;
      }
      .swagger-ui .topbar {
        display: none;
      }
    </style>
  </head>
  <body>
    <div id="swagger-ui"></div>
    <script src="__SWAGGER_UI_DIST_BASE_URL__/swagger-ui-bundle.js"></script>
    <script>
      window.onload = () => {
        const spec = __HONEYCOMB_OPENAPI_SPEC__;
        window.ui = SwaggerUIBundle({
          spec,
          dom_id: "#swagger-ui",
          deepLinking: true,
          persistAuthorization: true,
          displayRequestDuration: true
        });
      };
    </script>
  </body>
</html>
"##;

// Memory Room API 结构
#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryRoomListResponse {
    rooms: Vec<MemoryRoomSummary>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryRoomSummary {
    id: String,
    layer: String,
    title: String,
    summary: String,
    tags: Vec<String>,
    capability_count: usize,
    tool_count: usize,
    skill_count: usize,
    schedule_count: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryRoomResponse {
    room: MemoryRoomDetail,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryRoomDetail {
    id: String,
    namespace: MemoryNamespaceResponse,
    layer: String,
    title: String,
    summary: String,
    status: String,
    tags: Vec<String>,
    inherited_capabilities: Vec<CapabilityRef>,
    inherited_tools: Vec<ToolRef>,
    inherited_skills: Vec<SkillRef>,
    inherited_schedules: Vec<ScheduleRef>,
    room_config: serde_json::Value,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryNamespaceResponse {
    tenant_id: String,
    user_id: String,
}

#[derive(serde::Deserialize)]
struct MemoryRoomWriteRequest {
    id: Option<String>,
    layer: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    status: Option<String>,
    tags: Option<Vec<String>>,
    room_config: Option<RoomConfig>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RoomCapabilitiesResponse {
    room_id: String,
    resolved_capabilities: Vec<ResolvedCapabilityResponse>,
    resolved_tools: Vec<ResolvedToolResponse>,
    resolved_skills: Vec<ResolvedSkillResponse>,
    resolved_schedules: Vec<ResolvedScheduleResponse>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedCapabilityResponse {
    capability_ref: CapabilityRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedToolResponse {
    tool_ref: ToolRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedSkillResponse {
    skill_ref: SkillRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedScheduleResponse {
    schedule_ref: ScheduleRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InheritCapabilityRequest {
    capability_ref: CapabilityRef,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InheritToolRequest {
    tool_ref: ToolRef,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InheritSkillRequest {
    skill_ref: SkillRef,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InheritScheduleRequest {
    schedule_ref: ScheduleRef,
}

// Memory Room API 处理函数
async fn memory_rooms(
    State(_state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<MemoryRoomListResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );

    let rooms = repository.list_rooms()
        .map_err(|e| ApiError(anyhow::anyhow!("Failed to list memory rooms: {}", e)))?
        .into_iter()
        .map(|room| MemoryRoomSummary {
            id: room.id,
            layer: format!("{:?}", room.layer).to_lowercase(),
            title: room.title,
            summary: room.summary,
            tags: room.tags,
            capability_count: room.inherited_capabilities.len(),
            tool_count: room.inherited_tools.len(),
            skill_count: room.inherited_skills.len(),
            schedule_count: room.inherited_schedules.len(),
        })
        .collect();
    
    Ok(Json(MemoryRoomListResponse { rooms }))
}

async fn memory_room_get(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<MemoryRoomResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );

    match repository.get_room_by_id(&room_id)? {
        Some(room) => {
            let room_detail = MemoryRoomDetail {
                id: room.id,
                namespace: MemoryNamespaceResponse {
                    tenant_id: room.namespace.tenant_id,
                    user_id: room.namespace.user_id,
                },
                layer: format!("{:?}", room.layer).to_lowercase(),
                title: room.title,
                summary: room.summary,
                status: room.status,
                tags: room.tags,
                inherited_capabilities: room.inherited_capabilities,
                inherited_tools: room.inherited_tools,
                inherited_skills: room.inherited_skills,
                inherited_schedules: room.inherited_schedules,
                room_config: serde_json::to_value(&room.room_config)
                    .map_err(|e| ApiError(anyhow::anyhow!("Failed to serialize room config: {}", e)))?,
            };
            Ok(Json(MemoryRoomResponse { room: room_detail }))
        },
        None => Err(ApiError(anyhow!("Room not found: {}", room_id))),
    }
}

fn memory_room_detail(room: MemoryRoom) -> Result<MemoryRoomDetail, ApiError> {
    Ok(MemoryRoomDetail {
        id: room.id,
        namespace: MemoryNamespaceResponse {
            tenant_id: room.namespace.tenant_id,
            user_id: room.namespace.user_id,
        },
        layer: format!("{:?}", room.layer).to_lowercase(),
        title: room.title,
        summary: room.summary,
        status: room.status,
        tags: room.tags,
        inherited_capabilities: room.inherited_capabilities,
        inherited_tools: room.inherited_tools,
        inherited_skills: room.inherited_skills,
        inherited_schedules: room.inherited_schedules,
        room_config: serde_json::to_value(&room.room_config)
            .map_err(|e| ApiError(anyhow!("Failed to serialize room config: {}", e)))?,
    })
}

fn parse_memory_layer(value: Option<&str>) -> Result<MemoryLayer, ApiError> {
    match value.unwrap_or("chat").trim().to_ascii_lowercase().as_str() {
        "chat" => Ok(MemoryLayer::Chat),
        "topic" => Ok(MemoryLayer::Topic),
        "task" => Ok(MemoryLayer::Task),
        "project" => Ok(MemoryLayer::Project),
        "global" => Ok(MemoryLayer::Global),
        layer => Err(ApiError(anyhow!(
            "Invalid room layer: {} (must be chat, topic, task, project, or global)",
            layer
        ))),
    }
}

async fn memory_room_create(
    State(_state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
    Json(request): Json<MemoryRoomWriteRequest>,
) -> Result<Json<MemoryRoomResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let memory_namespace = MemoryNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );

    let id = request
        .id
        .filter(|value| !value.trim().is_empty())
        .context("missing room id")
        .map_err(ApiError)?;
    if repository.get_room_by_id(&id)?.is_some() {
        return Err(ApiError(anyhow!("Room already exists: {}", id)));
    }

    let title = request
        .title
        .filter(|value| !value.trim().is_empty())
        .context("missing room title")
        .map_err(ApiError)?;
    let summary = request.summary.unwrap_or_default();
    let layer = parse_memory_layer(request.layer.as_deref())?;

    let mut room = MemoryRoom::new(id, layer, title, summary).with_namespace(memory_namespace);
    if let Some(status) = request.status {
        room.status = status;
    }
    if let Some(tags) = request.tags {
        room.tags = tags;
    }
    if let Some(room_config) = request.room_config {
        room.room_config = room_config;
    }

    repository.write_room(&room)?;
    Ok(Json(MemoryRoomResponse {
        room: memory_room_detail(room)?,
    }))
}

async fn memory_room_update(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
    Json(request): Json<MemoryRoomWriteRequest>,
) -> Result<Json<MemoryRoomResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;

    if let Some(layer) = request.layer {
        let requested_layer = parse_memory_layer(Some(&layer))?;
        if requested_layer != room.layer {
            return Err(ApiError(anyhow!(
                "Changing room layer is not supported by this endpoint"
            )));
        }
    }
    if let Some(title) = request.title {
        room.title = title;
    }
    if let Some(summary) = request.summary {
        room.summary = summary;
    }
    if let Some(status) = request.status {
        room.status = status;
    }
    if let Some(tags) = request.tags {
        room.tags = tags;
    }
    if let Some(room_config) = request.room_config {
        room.room_config = room_config;
    }

    repository.write_room(&room)?;
    Ok(Json(MemoryRoomResponse {
        room: memory_room_detail(room)?,
    }))
}

async fn memory_room_capabilities(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<RoomCapabilitiesResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    
    let memory_namespace = MemoryNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    match repository.get_room_by_id(&room_id)? {
        Some(room) => {
            match resolver.resolve_room_capabilities(&room) {
                Ok(capabilities) => {
                    let response = RoomCapabilitiesResponse {
                        room_id: room_id.clone(),
                        resolved_capabilities: capabilities.capabilities.into_iter().map(|cap| ResolvedCapabilityResponse {
                            capability_ref: cap.capability_ref,
                            source_data: None,
                        }).collect(),
                        resolved_tools: capabilities.tools.into_iter().map(|tool| ResolvedToolResponse {
                            tool_ref: tool.tool_ref,
                            source_data: None,
                        }).collect(),
                        resolved_skills: capabilities.skills.into_iter().map(|skill| ResolvedSkillResponse {
                            skill_ref: skill.skill_ref,
                            source_data: None,
                        }).collect(),
                        resolved_schedules: capabilities.schedules.into_iter().map(|schedule| ResolvedScheduleResponse {
                            schedule_ref: schedule.schedule_ref,
                            source_data: None,
                        }).collect(),
                    };
                    Ok(Json(response))
                }
                Err(err) => Err(ApiError(anyhow!("Failed to resolve room capabilities: {}", err))),
            }
        }
        None => Err(ApiError(anyhow!("Room not found: {}", room_id))),
    }
}

async fn memory_room_inherit_capability(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
    Json(request): Json<InheritCapabilityRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let memory_namespace = MemoryNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_capability_to_room(&mut room, request.capability_ref)?;
    repository.write_room(&room)?;

    Ok(Json(serde_json::json!({"success": true, "message": "Capability inheritance added"})))
}

async fn memory_room_inherit_tool(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
    Json(request): Json<InheritToolRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let memory_namespace = MemoryNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_tool_to_room(&mut room, request.tool_ref)?;
    repository.write_room(&room)?;

    Ok(Json(serde_json::json!({"success": true, "message": "Tool inheritance added"})))
}

async fn memory_room_inherit_skill(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
    Json(request): Json<InheritSkillRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let memory_namespace = MemoryNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_skill_to_room(&mut room, request.skill_ref)?;
    repository.write_room(&room)?;

    Ok(Json(serde_json::json!({"success": true, "message": "Skill inheritance added"})))
}

async fn memory_room_inherit_schedule(
    State(_state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
    Json(request): Json<InheritScheduleRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let memory_namespace = MemoryNamespace::new(&namespace.tenant_id, &namespace.user_id);
    let repository = MemoryRoomRepository::with_namespace(
        workspace_root(),
        hc_store::store::WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_schedule_to_room(&mut room, request.schedule_ref)?;
    repository.write_room(&room)?;

    Ok(Json(serde_json::json!({"success": true, "message": "Schedule inheritance added"})))
}

// 房间能力增强相关函数
#[derive(Debug)]
struct RoomCapabilitiesInfo {
    capabilities: Vec<String>,
    tools: Vec<String>,
    skills: Vec<String>,
}

async fn enhance_chat_request_with_room_capabilities(
    _state: &AppState,
    request: &ChatRequest,
) -> Result<(ChatRequest, Option<DecisionRecord>), ApiError> {
    let mut enhanced_request = request.clone();
    
    // 构建行为上下文
    let behavior_context = BehaviorContext {
        user_id: request.user_id.clone(),
        session_id: request.session_id.clone(),
        room_id: request.room_id.clone(),
        task_type: Some("chat".to_string()),
        ..Default::default()
    };
    
    // 确定行为模式
    let behavior_pattern = if let Some(pattern_str) = &request.behavior_pattern {
        BehaviorPattern::from_str_or_default(pattern_str)
    } else {
        BehaviorPattern::default()
    };
    
    // 创建行为配置
    let mut behavior_config = BehaviorConfig::new(behavior_pattern);
    if let Some(depth) = request.thinking_depth {
        behavior_config = behavior_config.with_thinking_depth(depth);
    }
    
    // 创建行为引擎
    let mut behavior_engine = BehaviorEngine::new(behavior_config.clone(), behavior_context);
    
    // 如果没有指定房间ID，进行基础的行为决策
    let Some(room_id) = &request.room_id else {
        // 即使没有房间，也可以根据行为模式调整响应风格
        let options = vec![
            DecisionOption::new("direct", "Direct response without room context")
                .with_metrics(0.8, 0.9, 0.2, 0.1),
            DecisionOption::new("enhanced", "Enhanced response with general capabilities")
                .with_metrics(0.6, 0.8, 0.5, 0.3),
        ];
        
        let decision = behavior_engine.make_decision(DecisionType::ResponseStyle, options)
            .map_err(|e| ApiError(anyhow::anyhow!("Behavior decision failed: {}", e)))?;
        
        return Ok((enhanced_request, Some(decision)));
    };

    // 构建命名空间
    let tenant_id = request.tenant_id.clone().unwrap_or_else(default_tenant_id);
    let user_id = request.user_id.clone().unwrap_or_else(default_user_id);
    let namespace = hc_store::store::WorkspaceNamespace::new(&tenant_id, &user_id);
    
    // 获取房间
    let repository = MemoryRoomRepository::with_namespace(workspace_root(), namespace.clone());
    
    // 查找房间
    let room = match repository.get_room_by_id(room_id) {
        Ok(Some(room)) => room,
        Ok(None) => {
            // 房间不存在，返回原请求（可选择记录日志或返回错误）
            return Ok((enhanced_request, None));
        }
        Err(err) => {
            error!(%room_id, error = %err, "failed to load room");
            return Ok((enhanced_request, None));
        }
    };
    
    // 构建内存命名空间
    let memory_namespace = MemoryNamespace::new(&tenant_id, &user_id);
    
    // 解析房间能力
    let resolver = RoomCapabilityResolver::new(memory_namespace);
    let capabilities = match resolver.resolve_room_capabilities(&room) {
        Ok(caps) => caps,
        Err(err) => {
            error!(%room_id, error = %err, "failed to resolve room capabilities");
            return Ok((enhanced_request, None));
        }
    };
    
    // 基于房间能力做出决策
    let capability_options = vec![
        DecisionOption::new("minimal", "Use minimal room capabilities")
            .with_metrics(0.9, 0.95, 0.1, 0.05),
        DecisionOption::new("standard", "Use standard room capabilities")
            .with_metrics(0.7, 0.85, 0.4, 0.2),
        DecisionOption::new("enhanced", "Use all available room capabilities")
            .with_metrics(0.5, 0.75, 0.8, 0.4),
    ];
    
    let decision = behavior_engine.make_decision(DecisionType::CapabilityCreation, capability_options)
        .map_err(|e| ApiError(anyhow::anyhow!("Room capability decision failed: {}", e)))?;
    
    // 根据决策结果增强系统提示
    enhanced_request.system_prompt = enhance_system_prompt_with_behavior_and_room_capabilities(
        &capabilities,
        &room,
        &behavior_config,
        &decision,
        &enhanced_request.system_prompt,
    );
    
    Ok((enhanced_request, Some(decision)))
}

async fn get_room_capabilities_info(
    state: &AppState,
    room_id: &str,
) -> Result<RoomCapabilitiesInfo, ApiError> {
    // 使用默认命名空间（可以考虑从请求上下文获取）
    let namespace = hc_store::store::WorkspaceNamespace::local_default();
    let repository = MemoryRoomRepository::with_namespace(workspace_root(), namespace);
    
    // 查找房间
    let room = match repository.get_room_by_id(room_id)
        .map_err(|e| ApiError(anyhow!("Failed to load room: {}", e)))? 
    {
        Some(room) => room,
        None => {
            return Ok(RoomCapabilitiesInfo {
                capabilities: Vec::new(),
                tools: Vec::new(),
                skills: Vec::new(),
            });
        }
    };
    
    // 构建内存命名空间
    let memory_namespace = MemoryNamespace::new("local", "default");
    
    // 解析房间能力
    let resolver = RoomCapabilityResolver::new(memory_namespace);
    let capabilities = resolver.resolve_room_capabilities(&room)
        .map_err(|e| ApiError(anyhow!("Failed to resolve room capabilities: {}", e)))?;
    
    // 提取实际的能力列表
    Ok(RoomCapabilitiesInfo {
        capabilities: capabilities.capabilities.iter()
            .map(|c| c.capability_ref.id.clone())
            .collect(),
        tools: capabilities.tools.iter()
            .map(|t| t.tool_ref.id.clone())
            .collect(),
        skills: capabilities.skills.iter()
            .map(|s| s.skill_ref.id.clone())
            .collect(),
    })
}


fn enhance_system_prompt_with_behavior_and_room_capabilities(
    room_capabilities: &ResolvedRoomCapabilities,
    room: &MemoryRoom,
    behavior_config: &BehaviorConfig,
    decision: &DecisionRecord,
    base_prompt: &Option<String>,
) -> Option<String> {
    let base = base_prompt.as_deref().unwrap_or("You are a helpful assistant.");
    
    // 构建行为模式指导
    let behavior_guidance = match behavior_config.pattern {
        BehaviorPattern::Passive => {
            "Behavior Mode: PASSIVE - Execute instructions directly and precisely. Minimize interpretation and stick to explicit requests."
        },
        BehaviorPattern::Stable => {
            "Behavior Mode: STABLE - Use proven, reliable approaches. Prioritize accuracy and consistency over innovation."
        },
        BehaviorPattern::Learning => {
            "Behavior Mode: LEARNING - Balance reliability with careful exploration. Learn from interactions and suggest improvements when appropriate."
        },
        BehaviorPattern::Creative => {
            "Behavior Mode: CREATIVE - Pursue optimal solutions through innovative approaches. Be willing to explore new methods and possibilities."
        },
        BehaviorPattern::Adaptive => {
            "Behavior Mode: ADAPTIVE - Dynamically adjust approach based on context. Use the most appropriate strategy for each situation."
        },
    };
    
    let thinking_guidance = if behavior_config.thinking_depth > 3 {
        "Think deeply and systematically. Break down complex problems into steps and explain your reasoning process."
    } else if behavior_config.thinking_depth > 1 {
        "Think through the problem systematically and provide clear reasoning for your approach."
    } else {
        "Respond directly and efficiently."
    };
    
    // 构建房间能力描述
    let mut capability_sections = Vec::new();
    
    // 添加行为模式信息
    capability_sections.push(format!(
        "{}\n\nThinking Approach: {}\n\nDecision Context: {} (Confidence: {:.1}%)",
        behavior_guidance,
        thinking_guidance,
        decision.reasoning,
        decision.confidence * 100.0
    ));
    
    // 添加房间信息
    capability_sections.push(format!(
        "Room Context: You are operating in room '{}' with specialized capabilities.",
        room_capabilities.room_id
    ));
    
    // 添加可用工具
    if !room_capabilities.tools.is_empty() {
        let tools_list: Vec<String> = room_capabilities.tools
            .iter()
            .map(|tool| format!("  - {}: Tool from {:?}", tool.tool_ref.id, tool.tool_ref.inheritance_type))
            .collect();
        capability_sections.push(format!(
            "Available Tools:\n{}",
            tools_list.join("\n")
        ));
    }
    
    // 添加可用技能
    if !room_capabilities.skills.is_empty() {
        let skills_list: Vec<String> = room_capabilities.skills
            .iter()
            .map(|skill| format!("  - {}: Skill from {:?}", skill.skill_ref.id, skill.skill_ref.inheritance_type))
            .collect();
        capability_sections.push(format!(
            "Available Skills:\n{}",
            skills_list.join("\n")
        ));
    }
    
    // 添加执行上下文信息
    let exec_ctx = &room.room_config.execution_context;
    if exec_ctx.working_directory.is_some() 
        || exec_ctx.default_namespace.is_some() 
        || !exec_ctx.environment.is_empty() {
        let mut context_info = Vec::new();
        
        if let Some(wd) = &exec_ctx.working_directory {
            context_info.push(format!("Working Directory: {}", wd));
        }
        
        if let Some(ns) = &exec_ctx.default_namespace {
            context_info.push(format!("Default Namespace: {}", ns));
        }
        
        if !exec_ctx.environment.is_empty() {
            let env_vars: Vec<String> = exec_ctx.environment
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            context_info.push(format!("Environment: {}", env_vars.join(", ")));
        }
        
        if !context_info.is_empty() {
            capability_sections.push(format!(
                "Execution Context:\n  - {}",
                context_info.join("\n  - ")
            ));
        }
    }
    
    // 组合最终的系统提示
    if capability_sections.is_empty() {
        base_prompt.clone()
    } else {
        let enhanced = format!(
            "{}\n\n--- Room Capabilities ---\n{}\n\nUse these capabilities appropriately to provide the best assistance in this room context.",
            base,
            capability_sections.join("\n\n")
        );
        Some(enhanced)
    }
}

async fn behavior_patterns() -> Result<Json<Value>, ApiError> {
    let patterns = vec![
        BehaviorPattern::Passive,
        BehaviorPattern::Stable,
        BehaviorPattern::Learning,
        BehaviorPattern::Creative,
        BehaviorPattern::Adaptive,
    ];
    
    let pattern_data: Vec<_> = patterns
        .iter()
        .map(|pattern| {
            serde_json::json!({
                "name": format!("{:?}", pattern).to_lowercase(),
                "description": match pattern {
                    BehaviorPattern::Passive => "被动执行模式 - 严格按照指令执行",
                    BehaviorPattern::Stable => "稳定模式 - 保守且可靠的决策",
                    BehaviorPattern::Learning => "学习模式 - 保守新建功能，注重学习",
                    BehaviorPattern::Creative => "创造模式 - 注重创新和探索",
                    BehaviorPattern::Adaptive => "自适应模式 - 根据情况动态调整",
                },
                "risk_tolerance": pattern.risk_tolerance(),
                "innovation_tendency": pattern.innovation_tendency(),
                "proactivity": pattern.proactivity(),
            })
        })
        .collect();
    
    Ok(Json(serde_json::json!({
        "patterns": pattern_data
    })))
}

async fn behavior_pattern_get(Path(pattern_name): Path<String>) -> Result<Json<Value>, ApiError> {
    let pattern = BehaviorPattern::from_str(&pattern_name)
        .map_err(|e| ApiError(e))?;
    
    let config = BehaviorConfig::new(pattern.clone());
    
    Ok(Json(serde_json::json!({
        "pattern": format!("{:?}", pattern).to_lowercase(),
        "description": match pattern {
            BehaviorPattern::Passive => "被动执行模式 - 严格按照指令执行",
            BehaviorPattern::Stable => "稳定模式 - 保守且可靠的决策",
            BehaviorPattern::Learning => "学习模式 - 保守新建功能，注重学习",
            BehaviorPattern::Creative => "创造模式 - 注重创新和探索",
            BehaviorPattern::Adaptive => "自适应模式 - 根据情况动态调整",
        },
        "attributes": {
            "risk_tolerance": pattern.risk_tolerance(),
            "innovation_tendency": pattern.innovation_tendency(),
            "proactivity": pattern.proactivity(),
        },
        "config": {
            "thinking_depth": config.thinking_depth,
            "enable_metacognition": config.enable_metacognition,
            "learning_rate": config.learning_rate,
        }
    })))
}

async fn behavior_pattern_test(
    Path(pattern_name): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let pattern = BehaviorPattern::from_str(&pattern_name)
        .map_err(|e| ApiError(e))?;
    
    // 解析测试上下文
    let mut context = BehaviorContext::default();
    if let Some(ctx) = payload.get("context").and_then(|c| c.as_object()) {
        if let Some(user_id) = ctx.get("user_id").and_then(|v| v.as_str()) {
            context.user_id = Some(user_id.to_string());
        }
        if let Some(room_id) = ctx.get("room_id").and_then(|v| v.as_str()) {
            context.room_id = Some(room_id.to_string());
        }
        if let Some(task_type) = ctx.get("task_type").and_then(|v| v.as_str()) {
            context.task_type = Some(task_type.to_string());
        }
        if let Some(complexity) = ctx.get("complexity").and_then(|v| v.as_f64()) {
            context.estimated_complexity = Some(complexity as f32);
        }
        if let Some(success_rate) = ctx.get("success_rate").and_then(|v| v.as_f64()) {
            context.historical_success_rate = Some(success_rate as f32);
        }
        if let Some(time_pressure) = ctx.get("time_pressure").and_then(|v| v.as_f64()) {
            context.time_pressure = Some(time_pressure as f32);
        }
        if let Some(tools_count) = ctx.get("available_tools_count").and_then(|v| v.as_u64()) {
            context.available_tools_count = Some(tools_count as u32);
        }
    }
    
    // 解析配置
    let mut config = BehaviorConfig::new(pattern);
    if let Some(cfg) = payload.get("config").and_then(|c| c.as_object()) {
        if let Some(depth) = cfg.get("thinking_depth").and_then(|v| v.as_u64()) {
            config = config.with_thinking_depth(depth as u8);
        }
        if let Some(metacognition) = cfg.get("enable_metacognition").and_then(|v| v.as_bool()) {
            config = config.with_metacognition(metacognition);
        }
        if let Some(learning_rate) = cfg.get("learning_rate").and_then(|v| v.as_f64()) {
            config = config.with_learning_rate(learning_rate as f32);
        }
    }
    
    // 创建行为引擎
    let mut engine = BehaviorEngine::new(config, context);
    
    // 创建一些测试选项
    let options = vec![
        DecisionOption::new("conservative", "保守方案 - 使用已知可靠的方法")
            .with_pros(vec!["风险低".to_string(), "成功率高".to_string()])
            .with_cons(vec!["创新性不足".to_string()])
            .with_estimated_effort(Some(3.0))
            .with_success_probability(Some(0.9))
            .with_innovation_level(Some(0.2))
            .with_risk_level(Some(0.1)),
        DecisionOption::new("balanced", "平衡方案 - 在安全和创新之间取平衡")
            .with_pros(vec!["平衡性好".to_string(), "适应性强".to_string()])
            .with_cons(vec!["可能不够大胆".to_string()])
            .with_estimated_effort(Some(5.0))
            .with_success_probability(Some(0.7))
            .with_innovation_level(Some(0.5))
            .with_risk_level(Some(0.3)),
        DecisionOption::new("innovative", "创新方案 - 尝试新的方法和技术")
            .with_pros(vec!["创新性高".to_string(), "学习价值大".to_string()])
            .with_cons(vec!["风险高".to_string(), "不确定性大".to_string()])
            .with_estimated_effort(Some(8.0))
            .with_success_probability(Some(0.5))
            .with_innovation_level(Some(0.9))
            .with_risk_level(Some(0.7)),
    ];
    
    let decision_record = engine.make_decision(DecisionType::StrategySelection, options)
        .map_err(|e| ApiError(anyhow::anyhow!("Decision making failed: {}", e)))?;
    
    Ok(Json(serde_json::to_value(&decision_record).unwrap()))
}


struct ApiError(anyhow::Error);

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let message = self.0.to_string();
        let status = if message.contains("requires input")
            || message.contains("unsupported memory")
            || message.contains("invalid")
        {
            StatusCode::BAD_REQUEST
        } else if message.contains("missing api key") {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (
            status,
            Json(ErrorResponse {
                error: concise_error(&self.0),
            }),
        )
            .into_response()
    }
}

fn concise_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .next()
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| anyhow!("unknown error").to_string())
}
