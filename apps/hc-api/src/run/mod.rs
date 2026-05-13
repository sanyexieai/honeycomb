use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    env,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Extension, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use hc_protocol::{
    AgentListResponse, AgentRouteRequest, AgentRouteResponse, ApiChatMessage, ApiMemoryQuery,
    ApiNamespace, ChatRequest, ChatResponse, DomainListResponse, ErrorResponse, HealthResponse,
    McpServerListResponse,
};
use hc_service::{
    ServiceConfig,
    agent::{list_agents, list_domains, route_agent},
    chat::ChatStreamEvent,
    conversation::{
        conversation_inbox_snapshot, dismiss_agent_turn_proposal, draft_agent_turn_proposal,
        mark_agent_turn_proposal_sent, process_conversation_inbox, publish_conversation_event,
    },
    human_inbox::{complete_human_inbox_item, list_human_inbox_pending},
    index::{IndexRebuildRequest, IndexSearchRequest, rebuild_index, search_index},
    room_routing::{RoomRoutingContext, resolve_room_routing_context},
    scheduler::{
        ApiDispatchDueWorkerWallMillisecondsHistogram, ScheduleRequest, ScheduleStatusRequest,
        SchedulerOperationalStats, SchedulerRunRequest, TimedFollowupFiredEventRow,
        dispatch_due_scheduled_runs, dispatch_fired_followup_messages_headless,
        dispatch_queued_scheduled_runs, fired_followup_messages_from_receipts, list_scheduled_runs,
        list_schedules, list_timed_followup_fired_events_since_created,
        merge_scheduler_operational_stats_with_dispatch_slip_histogram, queue_due_scheduled_runs,
        scheduler_operational_stats, scheduler_operational_stats_openmetrics_text,
        set_schedule_status, write_schedule,
    },
    tool::{
        McpToolCallRequest, McpToolCallResponse, McpToolListRequest, McpToolListResponse,
        ToolListResponse, ToolWriteRequest, ToolWriteResponse, call_configured_mcp_tool,
        list_mcp_servers, list_mcp_tools, list_tools, write_tool,
    },
    turn::{TurnStreamEvent, handle_turn_request, handle_turn_stream_request},
};
use hc_service::transport::{
    AgentTurnProposal, BehaviorConfig, BehaviorContext, BehaviorEngine, BehaviorPattern,
    ConversationEvent, ConversationRepository, DecisionOption, DecisionRecord, DecisionType,
    HumanInboxItem, MemoryRoom, ResolvedRoomCapabilities, ScheduledRun, ScheduledTask,
    WorkspaceNamespace, default_tenant_id, default_user_id, init_console_tracing,
    load_local_env_file, now_unix, tenant_id_from_env, user_id_from_env, wall_clock_ms,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::{
    Stream,
    wrappers::{IntervalStream, ReceiverStream},
};
use tracing::{error, info, warn};

mod openapi;

use openapi::{openapi_document, swagger_ui_html};

mod memory;

use memory::{
    memory_room_capabilities, memory_room_create, memory_room_get, memory_room_inherit_capability,
    memory_room_inherit_schedule, memory_room_inherit_skill, memory_room_inherit_tool,
    memory_room_routing, memory_room_update, memory_rooms, room_lookup_request,
};

mod behavior;

use behavior::{behavior_pattern_get, behavior_pattern_test, behavior_patterns};

#[derive(Clone)]
pub struct AppState {
    pub service: ServiceConfig,
    /// Per `tenant_id` + `user_id`: count of headless follow-up messages delivered inside this hc-api process
    /// (`dispatch_*` paths + scheduler loop when headless delivery is enabled).
    pub followup_headless_delivered_messages_total: Arc<Mutex<HashMap<(String, String), u64>>>,
    /// Per-namespace dispatch path outcomes (API `dispatch-*` + optional scheduler loop).
    pub api_scheduler_dispatch_totals:
        Arc<Mutex<HashMap<(String, String), ApiSchedulerDispatchTotals>>>,
}

#[derive(Clone, Copy, Default)]
pub struct ApiSchedulerDispatchTotals {
    dispatch_due_completed: u64,
    dispatch_due_failed: u64,
    dispatch_queued_completed: u64,
    dispatch_queued_failed: u64,
    scheduler_loop_tick_completed: u64,
    scheduler_loop_tick_failed: u64,
    last_dispatch_due_worker_wall_ms: u64,
    last_dispatch_queued_worker_wall_ms: u64,
    last_scheduler_tick_worker_wall_ms: u64,
    dispatch_due_worker_wall_ms_histogram: ApiDispatchDueWorkerWallMillisecondsHistogram,
    dispatch_queued_worker_wall_ms_histogram: ApiDispatchDueWorkerWallMillisecondsHistogram,
    scheduler_loop_tick_worker_wall_ms_histogram: ApiDispatchDueWorkerWallMillisecondsHistogram,
}

fn scheduler_worker_wall_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

const DEFAULT_API_BIND: &str = "127.0.0.1:8787";
const DEFAULT_SCHEDULER_TICK_SECONDS: u64 = 30;
const DEFAULT_SWAGGER_UI_DIST_BASE_URL: &str = "https://unpkg.com/swagger-ui-dist@5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerFollowupDeliveryMode {
    Headless,
    Off,
    /// POST JSON payloads to `HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL` (see `followup_webhook_url_from_env`);
    /// per-request timeout from `HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS` (default 30, clamped 1–300).
    Webhook,
}

#[derive(Debug, Clone)]
pub struct ApiRuntimeConfig {
    pub bind_addr: SocketAddr,
    pub scheduler_tick_seconds: u64,
    scheduler_followup_delivery_mode: SchedulerFollowupDeliveryMode,
    pub swagger_ui_dist_base_url: String,
}

impl ApiRuntimeConfig {
    pub fn from_env() -> Result<Self> {
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
            scheduler_followup_delivery_mode: scheduler_followup_delivery_mode_from_env(),
            swagger_ui_dist_base_url: env::var("HC_SWAGGER_UI_DIST_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_SWAGGER_UI_DIST_BASE_URL.to_owned()),
        })
    }

    /// Logs **`HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE`**, webhook URL / bearer presence (no secrets),
    /// and resolved **`webhook_timeout_secs`** from **`HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS`**.
    pub fn log_scheduler_followup_delivery(&self) {
        match self.scheduler_followup_delivery_mode {
            SchedulerFollowupDeliveryMode::Headless => {
                info!(
                    followup_delivery_mode = "headless",
                    "hc-api timed follow-up delivery"
                );
            }
            SchedulerFollowupDeliveryMode::Off => {
                info!(
                    followup_delivery_mode = "off",
                    "hc-api timed follow-up delivery"
                );
            }
            SchedulerFollowupDeliveryMode::Webhook => {
                let url_ok = followup_webhook_url_from_env().is_some();
                let bearer_ok = followup_webhook_bearer_token_from_env().is_some();
                let webhook_timeout_secs = followup_webhook_timeout_secs();
                info!(
                    followup_delivery_mode = "webhook",
                    webhook_url_configured = url_ok,
                    webhook_bearer_configured = bearer_ok,
                    webhook_timeout_secs,
                    "hc-api timed follow-up delivery"
                );
                if !url_ok {
                    warn!(
                        "HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL is unset or empty: webhook delivery will error when fired follow-ups need delivery"
                    );
                }
            }
        }
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
struct ScheduleFollowupFiredEventsQuery {
    #[serde(flatten)]
    namespace: NamespaceQuery,
    #[serde(default)]
    since_created_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ScheduleOperationalStatsQuery {
    #[serde(flatten)]
    namespace: NamespaceQuery,
    #[serde(default)]
    now_unix: Option<u64>,
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

/// Lightweight body for **`POST /v1/messages`** and **`POST /v1/messages/stream`** (vs full [`ChatRequest`] on `/v1/chat`).
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
    room_id: Option<String>,
    #[serde(default)]
    behavior_pattern: Option<String>,
    #[serde(default)]
    thinking_depth: Option<u8>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    domain_id: Option<String>,
    #[serde(default)]
    active_agent_id: Option<String>,
    #[serde(default)]
    active_task_id: Option<String>,
    #[serde(default)]
    active_work_item_id: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default)]
    messages: Vec<ApiChatMessage>,
    #[serde(default)]
    memory: Option<ApiMemoryQuery>,
}

fn chat_request_from_user_message(request: UserMessageRequest) -> ChatRequest {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        request.tenant_id.clone(),
        request.user_id.clone(),
    );
    let mut memory = ApiMemoryQuery {
        namespace,
        scope: None,
        kind: None,
        tag: None,
        text: None,
        limit: None,
    };
    if let Some(overlay) = &request.memory {
        if overlay.scope.is_some() {
            memory.scope = overlay.scope.clone();
        }
        if overlay.kind.is_some() {
            memory.kind = overlay.kind.clone();
        }
        if overlay.tag.is_some() {
            memory.tag = overlay.tag.clone();
        }
        if overlay.text.is_some() {
            memory.text = overlay.text.clone();
        }
        if overlay.limit.is_some() {
            memory.limit = overlay.limit;
        }
        let defaults = ApiNamespace::default();
        if overlay.namespace.tenant_id != defaults.tenant_id
            || overlay.namespace.user_id != defaults.user_id
        {
            memory.namespace = normalized_request_namespace(overlay.namespace.clone(), None, None);
        }
    }
    ChatRequest {
        tenant_id: Some(memory.namespace.tenant_id.clone()),
        user_id: Some(memory.namespace.user_id.clone()),
        session_id: normalized_optional_string(request.session_id),
        room_id: normalized_optional_string(request.room_id),
        behavior_pattern: normalized_optional_string(request.behavior_pattern),
        thinking_depth: request.thinking_depth,
        input: Some(request.text),
        messages: request.messages,
        provider: normalized_optional_string(request.provider),
        model: normalized_optional_string(request.model),
        system_prompt: normalized_optional_string(request.system_prompt),
        agent_id: normalized_optional_string(request.agent_id),
        domain_id: normalized_optional_string(request.domain_id),
        active_agent_id: normalized_optional_string(request.active_agent_id),
        active_task_id: normalized_optional_string(request.active_task_id),
        active_work_item_id: normalized_optional_string(request.active_work_item_id),
        memory,
        temperature: request.temperature,
        max_output_tokens: request.max_output_tokens,
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

    let (enhanced_request, _decision, room_ctx) =
        match enhance_chat_request_with_room_capabilities(&state, &request).await {
            Ok(triple) => triple,
            Err(error) => {
                let _ = socket
                    .send(Message::Text(
                        json!({
                            "event": "chat.error",
                            "id": format!("chat.error.{}", now_unix()),
                            "data": {
                                "type": "chat_error",
                                "error": format!(
                                    "Room capability enhancement failed: {}",
                                    error.0
                                ),
                            },
                        })
                        .to_string(),
                    ))
                    .await;
                return;
            }
        };

    let room_caps_data =
        match room_capabilities_stream_data(&state, &request, room_ctx.as_ref()).await {
            Ok(data) => data,
            Err(error) => {
                let _ = socket
                    .send(Message::Text(
                        json!({
                            "event": "chat.error",
                            "id": format!("chat.error.{}", now_unix()),
                            "data": {
                                "type": "chat_error",
                                "error": format!("Room capabilities metadata failed: {}", error.0),
                            },
                        })
                        .to_string(),
                    ))
                    .await;
                return;
            }
        };
    if let Some(data) = room_caps_data {
        let _ = socket
            .send(Message::Text(
                json!({
                    "event": "chat.room_capabilities",
                    "id": format!("chat.room_capabilities.{}", now_unix()),
                    "data": data,
                })
                .to_string(),
            ))
            .await;
    }

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
            handle_turn_stream_request(&service, enhanced_request, room_ctx, &mut on_event)
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

/// Axum router for tests and custom embedding (no background scheduler).
pub fn build_router(state: AppState, swagger_ui_dist_base_url: String) -> Router {
    Router::new()
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
        .route(
            "/v1/schedules/followup-fired-events",
            get(schedule_followup_fired_events),
        )
        .route(
            "/v1/schedules/operational-stats",
            get(schedule_operational_stats),
        )
        .route(
            "/v1/schedules/metrics/prometheus",
            get(schedule_metrics_prometheus),
        )
        .route("/v1/conversation/inbox", get(conversation_inbox))
        .route("/v1/conversation/stream", get(conversation_stream))
        .route("/v1/conversation/events", post(conversation_event))
        .route("/v1/conversation/process", post(conversation_process))
        .route("/v1/human-inbox", get(human_inbox_list))
        .route("/v1/human-inbox/complete", post(human_inbox_complete))
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
        .route("/v1/messages", post(messages))
        .route("/v1/messages/stream", post(messages_stream))
        .route("/v1/chat/ws", get(chat_ws))
        .route(
            "/v1/memory/rooms",
            get(memory_rooms).post(memory_room_create),
        )
        .route(
            "/v1/memory/rooms/:room_id",
            get(memory_room_get).put(memory_room_update),
        )
        .route(
            "/v1/memory/rooms/:room_id/capabilities",
            get(memory_room_capabilities),
        )
        .route(
            "/v1/memory/rooms/:room_id/routing",
            get(memory_room_routing),
        )
        .route(
            "/v1/memory/rooms/:room_id/capabilities/inherit",
            post(memory_room_inherit_capability),
        )
        .route(
            "/v1/memory/rooms/:room_id/tools/inherit",
            post(memory_room_inherit_tool),
        )
        .route(
            "/v1/memory/rooms/:room_id/skills/inherit",
            post(memory_room_inherit_skill),
        )
        .route(
            "/v1/memory/rooms/:room_id/schedules/inherit",
            post(memory_room_inherit_schedule),
        )
        .route("/v1/behavior/patterns", get(behavior_patterns))
        .route("/v1/behavior/patterns/:pattern", get(behavior_pattern_get))
        .route(
            "/v1/behavior/patterns/:pattern/test",
            post(behavior_pattern_test),
        )
        .layer(Extension(swagger_ui_dist_base_url))
        .with_state(state)
}

pub async fn serve() -> Result<()> {
    load_local_env_file()?;
    init_console_tracing();
    let runtime_config = ApiRuntimeConfig::from_env()?;
    runtime_config.log_scheduler_followup_delivery();
    let followup_headless_delivered_messages_total = Arc::new(Mutex::new(HashMap::new()));
    let api_scheduler_dispatch_totals = Arc::new(Mutex::new(HashMap::new()));
    let state = AppState {
        service: ServiceConfig::from_env(),
        followup_headless_delivered_messages_total: followup_headless_delivered_messages_total
            .clone(),
        api_scheduler_dispatch_totals: api_scheduler_dispatch_totals.clone(),
    };
    start_scheduler_loop_if_enabled(
        state.service.clone(),
        followup_headless_delivered_messages_total,
        api_scheduler_dispatch_totals,
        runtime_config.scheduler_tick_seconds,
        runtime_config.scheduler_followup_delivery_mode,
    );
    let bind_addr = runtime_config.bind_addr;
    let app = build_router(state, runtime_config.swagger_ui_dist_base_url.clone());
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
        WorkspaceNamespace::new(
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
) -> Result<Json<ConversationEvent>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let mut event = ConversationEvent::new(request.kind);
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

async fn human_inbox_list(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<Vec<HumanInboxItem>>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let items =
        tokio::task::spawn_blocking(move || list_human_inbox_pending(&state.service, namespace))
            .await
            .map_err(|error| ApiError(anyhow!("human inbox list worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(items))
}

#[derive(Debug, Deserialize)]
struct HumanInboxCompleteBody {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    item_id: String,
    response_body: String,
}

async fn human_inbox_complete(
    State(state): State<AppState>,
    Json(body): Json<HumanInboxCompleteBody>,
) -> Result<Json<Value>, ApiError> {
    let namespace =
        normalized_request_namespace(ApiNamespace::default(), body.tenant_id, body.user_id);
    let item_id = body.item_id;
    let response_body = body.response_body;
    let answered_at_ms = wall_clock_ms();
    let path = tokio::task::spawn_blocking(move || {
        complete_human_inbox_item(
            &state.service,
            namespace,
            &item_id,
            &response_body,
            answered_at_ms,
        )
    })
    .await
    .map_err(|error| ApiError(anyhow!("human inbox complete worker failed: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(json!({ "path": path })))
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
) -> Result<Json<AgentTurnProposal>, ApiError> {
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
) -> Result<Json<AgentTurnProposal>, ApiError> {
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

fn start_scheduler_loop_if_enabled(
    service: ServiceConfig,
    followup_headless_counters: Arc<Mutex<HashMap<(String, String), u64>>>,
    api_dispatch_totals: Arc<Mutex<HashMap<(String, String), ApiSchedulerDispatchTotals>>>,
    tick_seconds: u64,
    delivery_mode: SchedulerFollowupDeliveryMode,
) {
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
            let ns_for_counters = namespace.clone();
            let headless = followup_headless_counters.clone();
            let dispatch_totals_in_block = api_dispatch_totals.clone();
            let result = tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let report = dispatch_due_scheduled_runs(&service, ns_for_counters.clone(), None)?;
                let delivered = deliver_scheduler_followup_messages(
                    &service,
                    ns_for_counters.clone(),
                    &report.receipts,
                    delivery_mode,
                )?;
                let delivered_followups = delivered.len();
                record_followup_headless_delivered(
                    &headless,
                    &ns_for_counters,
                    delivered_followups,
                );
                let wall_ms = scheduler_worker_wall_ms(start);
                record_api_dispatch_totals(&dispatch_totals_in_block, &ns_for_counters, |t| {
                    t.scheduler_loop_tick_completed += 1;
                    t.last_scheduler_tick_worker_wall_ms = wall_ms;
                    t.scheduler_loop_tick_worker_wall_ms_histogram
                        .observe(wall_ms);
                });
                Ok::<_, anyhow::Error>((report, delivered_followups))
            })
            .await;
            match result {
                Ok(Ok((report, delivered_followups))) => {
                    if !report.receipts.is_empty() {
                        info!(
                            dispatched = report.receipts.len(),
                            queued = report.queued_count,
                            delivered_followups,
                            "scheduler dispatched due runs"
                        );
                    }
                }
                Ok(Err(error)) => {
                    record_api_dispatch_totals(&api_dispatch_totals, &namespace, |t| {
                        t.scheduler_loop_tick_failed += 1;
                    });
                    warn!(%error, "scheduler tick failed");
                }
                Err(error) => {
                    record_api_dispatch_totals(&api_dispatch_totals, &namespace, |t| {
                        t.scheduler_loop_tick_failed += 1;
                    });
                    warn!(%error, "scheduler worker failed");
                }
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
    let (enhanced_request, decision, room_ctx) =
        enhance_chat_request_with_room_capabilities(&state, &request).await?;

    let chat_namespace = normalized_request_namespace(
        ApiNamespace::default(),
        request.tenant_id.clone(),
        request.user_id.clone(),
    );
    let capabilities_info_opt: Option<RoomCapabilitiesInfo> = match request.room_id.as_ref() {
        None => None,
        Some(room_id) => Some(if let Some(ref ctx) = room_ctx {
            room_capabilities_info_from_routing(ctx)
        } else {
            get_room_capabilities_info(&state, room_id, &chat_namespace).await?
        }),
    };

    let service = state.service.clone();
    let mut response = tokio::task::spawn_blocking(move || {
        handle_turn_request(&service, enhanced_request, room_ctx)
    })
    .await
    .map_err(|error| ApiError(anyhow!("chat worker failed: {error}")))?
    .map_err(ApiError::from)?;

    // 添加房间信息到响应
    response.room_id = request.room_id.clone();
    if let Some(capabilities_info) = capabilities_info_opt {
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

/// Same JSON body as **`POST /v1/chat`**, accepting [`UserMessageRequest`] (OpenAPI **`UserMessageBody`**).
async fn messages(
    State(state): State<AppState>,
    Json(body): Json<UserMessageRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    let request = chat_request_from_user_message(body);
    chat(State(state), Json(request)).await
}

async fn chat_stream(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(16);
    let service = state.service.clone();

    // 增强请求以包含房间能力
    let (enhanced_request, _decision, room_ctx) =
        match enhance_chat_request_with_room_capabilities(&state, &request).await {
            Ok(triple) => triple,
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

    let room_caps_data =
        match room_capabilities_stream_data(&state, &request, room_ctx.as_ref()).await {
            Ok(data) => data,
            Err(error) => {
                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    let _ = tx_clone
                        .send(Ok(Event::default().event("chat.error").data(
                            json!({
                                "type": "chat_error",
                                "error": format!(
                                    "Room capabilities metadata failed: {}",
                                    error.0
                                ),
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
        if let Some(payload) = room_caps_data {
            let id = format!("chat.room_capabilities.{}", now_unix());
            if let Ok(event) = sse_json_event("chat.room_capabilities", id, payload) {
                let _ = tx_for_events.send(Ok(event)).await;
            }
        }
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
            handle_turn_stream_request(&service, enhanced_request, room_ctx, &mut on_event)
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

/// Same SSE payload as **`/v1/chat/stream`**, with a minimal JSON body (OpenAPI schema **`UserMessageBody`**).
async fn messages_stream(
    State(state): State<AppState>,
    Json(body): Json<UserMessageRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let request = chat_request_from_user_message(body);
    chat_stream(State(state), Json(request)).await
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
) -> Result<Json<Vec<ScheduledTask>>, ApiError> {
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
) -> Result<Json<ScheduledTask>, ApiError> {
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
) -> Result<Json<Vec<ScheduledRun>>, ApiError> {
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
) -> Result<Json<Vec<ScheduledRun>>, ApiError> {
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
    let ns_counters = namespace.clone();
    let service = state.service.clone();
    let counters = state.followup_headless_delivered_messages_total.clone();
    let dispatch_totals = state.api_scheduler_dispatch_totals.clone();
    let delivery_mode = scheduler_followup_delivery_mode_from_env();
    let join = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let report = dispatch_due_scheduled_runs(&service, namespace.clone(), request.now_unix)?;
        let delivered_followups = deliver_scheduler_followup_messages(
            &service,
            namespace.clone(),
            &report.receipts,
            delivery_mode,
        )?;
        let delivered_count = delivered_followups.len();
        record_followup_headless_delivered(&counters, &namespace, delivered_count);
        tracing::debug!(
            dispatched = report.receipts.len(),
            queued = report.queued_count,
            delivered_followups = delivered_count,
            "api schedule dispatch-due completed"
        );
        let wall_ms = scheduler_worker_wall_ms(start);
        Ok::<_, anyhow::Error>((report, wall_ms))
    })
    .await;

    let report = match join {
        Ok(Ok((report, wall_ms))) => {
            record_api_dispatch_totals(&dispatch_totals, &ns_counters, |t| {
                t.dispatch_due_completed += 1;
                t.last_dispatch_due_worker_wall_ms = wall_ms;
                t.dispatch_due_worker_wall_ms_histogram.observe(wall_ms);
            });
            report
        }
        Ok(Err(e)) => {
            record_api_dispatch_totals(&dispatch_totals, &ns_counters, |t| {
                t.dispatch_due_failed += 1;
            });
            return Err(ApiError(e));
        }
        Err(e) => {
            record_api_dispatch_totals(&dispatch_totals, &ns_counters, |t| {
                t.dispatch_due_failed += 1;
            });
            return Err(ApiError(anyhow!(
                "schedule dispatch-due worker failed: {e}"
            )));
        }
    };
    Ok(Json(report))
}

async fn schedule_dispatch_queued(
    State(state): State<AppState>,
    Json(request): Json<SchedulerRunRequest>,
) -> Result<Json<hc_service::scheduler::SchedulerDispatchReport>, ApiError> {
    let namespace =
        normalized_request_namespace(request.namespace, request.tenant_id, request.user_id);
    let ns_counters = namespace.clone();
    let service = state.service.clone();
    let counters = state.followup_headless_delivered_messages_total.clone();
    let dispatch_totals = state.api_scheduler_dispatch_totals.clone();
    let delivery_mode = scheduler_followup_delivery_mode_from_env();
    let join = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let report = dispatch_queued_scheduled_runs(&service, namespace.clone(), request.now_unix)?;
        let delivered_followups = deliver_scheduler_followup_messages(
            &service,
            namespace.clone(),
            &report.receipts,
            delivery_mode,
        )?;
        let delivered_count = delivered_followups.len();
        record_followup_headless_delivered(&counters, &namespace, delivered_count);
        tracing::debug!(
            dispatched = report.receipts.len(),
            queued = report.queued_count,
            delivered_followups = delivered_count,
            "api schedule dispatch-queued completed"
        );
        let wall_ms = scheduler_worker_wall_ms(start);
        Ok::<_, anyhow::Error>((report, wall_ms))
    })
    .await;

    let report = match join {
        Ok(Ok((report, wall_ms))) => {
            record_api_dispatch_totals(&dispatch_totals, &ns_counters, |t| {
                t.dispatch_queued_completed += 1;
                t.last_dispatch_queued_worker_wall_ms = wall_ms;
                t.dispatch_queued_worker_wall_ms_histogram.observe(wall_ms);
            });
            report
        }
        Ok(Err(e)) => {
            record_api_dispatch_totals(&dispatch_totals, &ns_counters, |t| {
                t.dispatch_queued_failed += 1;
            });
            return Err(ApiError(e));
        }
        Err(e) => {
            record_api_dispatch_totals(&dispatch_totals, &ns_counters, |t| {
                t.dispatch_queued_failed += 1;
            });
            return Err(ApiError(anyhow!(
                "schedule dispatch-queued worker failed: {e}"
            )));
        }
    };
    Ok(Json(report))
}

async fn schedule_followup_fired_events(
    State(state): State<AppState>,
    Query(query): Query<ScheduleFollowupFiredEventsQuery>,
) -> Result<Json<Vec<TimedFollowupFiredEventRow>>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.namespace.tenant_id.clone()),
        Some(query.namespace.user_id.clone()),
    );
    let workspace_ns = WorkspaceNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    );
    let since_created_at_unix = query.since_created_at_unix.unwrap_or(0);
    let service = state.service.clone();
    let rows = tokio::task::spawn_blocking(move || {
        list_timed_followup_fired_events_since_created(
            &service,
            &workspace_ns,
            since_created_at_unix,
        )
    })
    .await
    .map_err(|error| {
        ApiError(anyhow!(
            "schedule followup-fired-events blocking worker failed: {error}"
        ))
    })?
    .map_err(ApiError::from)?;
    Ok(Json(rows))
}

async fn schedule_operational_stats(
    State(state): State<AppState>,
    Query(query): Query<ScheduleOperationalStatsQuery>,
) -> Result<Json<SchedulerOperationalStats>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.namespace.tenant_id.clone()),
        Some(query.namespace.user_id.clone()),
    );
    let workspace_ns = WorkspaceNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    );
    let service = state.service.clone();
    let stats = tokio::task::spawn_blocking(move || {
        scheduler_operational_stats(&service, &workspace_ns, query.now_unix)
    })
    .await
    .map_err(|error| {
        ApiError(anyhow!(
            "schedule operational-stats blocking worker failed: {error}"
        ))
    })?
    .map_err(ApiError::from)?;
    let stats = merge_scheduler_operational_stats_with_api_counters(
        stats,
        &state.followup_headless_delivered_messages_total,
        &state.api_scheduler_dispatch_totals,
        &namespace.tenant_id,
        &namespace.user_id,
    );
    let stats = merge_scheduler_operational_stats_with_dispatch_slip_histogram(
        stats,
        &namespace.tenant_id,
        &namespace.user_id,
    );
    Ok(Json(stats))
}

async fn schedule_metrics_prometheus(
    State(state): State<AppState>,
    Query(query): Query<ScheduleOperationalStatsQuery>,
) -> Result<Response, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.namespace.tenant_id.clone()),
        Some(query.namespace.user_id.clone()),
    );
    let tenant_label = namespace.tenant_id.clone();
    let user_label = namespace.user_id.clone();
    let workspace_ns = WorkspaceNamespace::new(
        namespace.tenant_id.clone(),
        namespace.user_id.clone(),
    );
    let service = state.service.clone();
    let stats = tokio::task::spawn_blocking(move || {
        scheduler_operational_stats(&service, &workspace_ns, query.now_unix)
    })
    .await
    .map_err(|error| {
        ApiError(anyhow!(
            "schedule metrics prometheus blocking worker failed: {error}"
        ))
    })?
    .map_err(ApiError::from)?;
    let stats = merge_scheduler_operational_stats_with_api_counters(
        stats,
        &state.followup_headless_delivered_messages_total,
        &state.api_scheduler_dispatch_totals,
        &namespace.tenant_id,
        &namespace.user_id,
    );
    let stats = merge_scheduler_operational_stats_with_dispatch_slip_histogram(
        stats,
        &namespace.tenant_id,
        &namespace.user_id,
    );
    let text = scheduler_operational_stats_openmetrics_text(&stats, &tenant_label, &user_label);

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )
        .body(Body::from(text))
        .map_err(|e| ApiError(anyhow!("metrics response build failed: {e}")))
}

fn scheduler_followup_delivery_mode_from_env() -> SchedulerFollowupDeliveryMode {
    let raw = env::var("HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE")
        .unwrap_or_else(|_| "headless".to_owned())
        .to_ascii_lowercase();
    scheduler_followup_delivery_mode_parse(&raw)
}

fn scheduler_followup_delivery_mode_parse(raw: &str) -> SchedulerFollowupDeliveryMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" => SchedulerFollowupDeliveryMode::Off,
        "webhook" => SchedulerFollowupDeliveryMode::Webhook,
        _ => SchedulerFollowupDeliveryMode::Headless,
    }
}

fn followup_webhook_url_from_env() -> Option<String> {
    env::var("HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn followup_webhook_bearer_token_from_env() -> Option<String> {
    env::var("HC_SCHEDULER_FOLLOWUP_WEBHOOK_BEARER_TOKEN")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn parse_followup_webhook_timeout_secs(raw: Option<&str>) -> u64 {
    const DEFAULT_SECS: u64 = 30;
    const MIN_SECS: u64 = 1;
    const MAX_SECS: u64 = 300;
    let secs = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    secs.clamp(MIN_SECS, MAX_SECS)
}

fn followup_webhook_timeout_secs() -> u64 {
    parse_followup_webhook_timeout_secs(
        env::var("HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

fn deliver_followup_webhook(
    service: &ServiceConfig,
    namespace: ApiNamespace,
    receipts: &[hc_service::scheduler::SchedulerDispatchReceipt],
) -> Result<Vec<String>> {
    let url = followup_webhook_url_from_env().context(
        "HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL is required when HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE=webhook",
    )?;
    let tenant_id = namespace.tenant_id.clone();
    let user_id = namespace.user_id.clone();
    let messages = fired_followup_messages_from_receipts(service, namespace, receipts)?;
    if messages.is_empty() {
        return Ok(Vec::new());
    }
    let timeout = Duration::from_secs(followup_webhook_timeout_secs());
    let user_agent = concat!("honeycomb-hc-api/", env!("CARGO_PKG_VERSION"));
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .context("build reqwest client for follow-up webhook")?;
    let bearer = followup_webhook_bearer_token_from_env();
    let mut delivered = Vec::with_capacity(messages.len());
    for m in &messages {
        let body = json!({
            "tenant_id": tenant_id,
            "user_id": user_id,
            "followup_id": m.followup_id,
            "message": m.message,
        });
        let mut req = client
            .post(&url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .json(&body);
        if let Some(ref tok) = bearer {
            req = req.bearer_auth(tok);
        }
        let response = req
            .send()
            .with_context(|| format!("webhook POST for follow-up {}", m.followup_id))?;
        if !response.status().is_success() {
            let status = response.status();
            let err_body = response.text().unwrap_or_default();
            bail!(
                "follow-up webhook returned {status} for follow-up {}: {}",
                m.followup_id,
                err_body.trim()
            );
        }
        delivered.push(m.followup_id.clone());
    }
    Ok(delivered)
}

fn deliver_scheduler_followup_messages(
    service: &ServiceConfig,
    namespace: ApiNamespace,
    receipts: &[hc_service::scheduler::SchedulerDispatchReceipt],
    mode: SchedulerFollowupDeliveryMode,
) -> Result<Vec<String>> {
    match mode {
        SchedulerFollowupDeliveryMode::Headless => {
            dispatch_fired_followup_messages_headless(service, namespace, receipts)
        }
        SchedulerFollowupDeliveryMode::Off => Ok(Vec::new()),
        SchedulerFollowupDeliveryMode::Webhook => {
            deliver_followup_webhook(service, namespace, receipts)
        }
    }
}

fn record_followup_headless_delivered(
    map: &Arc<Mutex<HashMap<(String, String), u64>>>,
    namespace: &ApiNamespace,
    delivered_message_count: usize,
) {
    if delivered_message_count == 0 {
        return;
    }
    let key = (namespace.tenant_id.clone(), namespace.user_id.clone());
    if let Ok(mut guard) = map.lock() {
        *guard.entry(key).or_insert(0) += delivered_message_count as u64;
    }
}

fn record_api_dispatch_totals(
    map: &Arc<Mutex<HashMap<(String, String), ApiSchedulerDispatchTotals>>>,
    namespace: &ApiNamespace,
    mutate: impl FnOnce(&mut ApiSchedulerDispatchTotals),
) {
    let key = (namespace.tenant_id.clone(), namespace.user_id.clone());
    if let Ok(mut guard) = map.lock() {
        mutate(guard.entry(key).or_default());
    }
}

fn read_api_dispatch_totals(
    map: &Arc<Mutex<HashMap<(String, String), ApiSchedulerDispatchTotals>>>,
    tenant_id: &str,
    user_id: &str,
) -> ApiSchedulerDispatchTotals {
    let guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get(&(tenant_id.to_owned(), user_id.to_owned()))
        .copied()
        .unwrap_or_default()
}

fn read_followup_headless_delivered_total(
    map: &Arc<Mutex<HashMap<(String, String), u64>>>,
    tenant_id: &str,
    user_id: &str,
) -> u64 {
    let guard = map.lock().unwrap_or_else(|e| e.into_inner());
    *guard
        .get(&(tenant_id.to_owned(), user_id.to_owned()))
        .unwrap_or(&0)
}

#[cfg(test)]
mod followup_webhook_timeout_parse_tests {
    use super::parse_followup_webhook_timeout_secs;

    #[test]
    fn clamps_and_defaults_webhook_timeout_secs() {
        assert_eq!(parse_followup_webhook_timeout_secs(None), 30);
        assert_eq!(parse_followup_webhook_timeout_secs(Some("")), 30);
        assert_eq!(parse_followup_webhook_timeout_secs(Some("15")), 15);
        assert_eq!(parse_followup_webhook_timeout_secs(Some("0")), 1);
        assert_eq!(parse_followup_webhook_timeout_secs(Some("9999")), 300);
        assert_eq!(
            parse_followup_webhook_timeout_secs(Some("not-a-number")),
            30
        );
    }
}

#[cfg(test)]
mod scheduler_followup_delivery_mode_tests {
    use super::SchedulerFollowupDeliveryMode;
    use super::scheduler_followup_delivery_mode_parse;

    #[test]
    fn parses_delivery_mode_strings() {
        assert_eq!(
            scheduler_followup_delivery_mode_parse("headless"),
            SchedulerFollowupDeliveryMode::Headless
        );
        assert_eq!(
            scheduler_followup_delivery_mode_parse("off"),
            SchedulerFollowupDeliveryMode::Off
        );
        assert_eq!(
            scheduler_followup_delivery_mode_parse("webhook"),
            SchedulerFollowupDeliveryMode::Webhook
        );
        assert_eq!(
            scheduler_followup_delivery_mode_parse("WEBHOOK"),
            SchedulerFollowupDeliveryMode::Webhook
        );
    }
}

fn merge_scheduler_operational_stats_with_api_counters(
    mut stats: SchedulerOperationalStats,
    headless_messages: &Arc<Mutex<HashMap<(String, String), u64>>>,
    dispatch_totals: &Arc<Mutex<HashMap<(String, String), ApiSchedulerDispatchTotals>>>,
    tenant_id: &str,
    user_id: &str,
) -> SchedulerOperationalStats {
    let followup_delivered =
        read_followup_headless_delivered_total(headless_messages, tenant_id, user_id);
    stats.api_followup_messages_delivered_total = followup_delivered;
    stats.api_followup_headless_messages_delivered_total = followup_delivered;
    let d = read_api_dispatch_totals(dispatch_totals, tenant_id, user_id);
    stats.api_dispatch_due_completed_total = d.dispatch_due_completed;
    stats.api_dispatch_due_failed_total = d.dispatch_due_failed;
    stats.api_dispatch_queued_completed_total = d.dispatch_queued_completed;
    stats.api_dispatch_queued_failed_total = d.dispatch_queued_failed;
    stats.api_scheduler_loop_tick_completed_total = d.scheduler_loop_tick_completed;
    stats.api_scheduler_loop_tick_failed_total = d.scheduler_loop_tick_failed;
    stats.api_dispatch_due_last_worker_wall_ms = d.last_dispatch_due_worker_wall_ms;
    stats.api_dispatch_queued_last_worker_wall_ms = d.last_dispatch_queued_worker_wall_ms;
    stats.api_scheduler_loop_tick_last_worker_wall_ms = d.last_scheduler_tick_worker_wall_ms;
    stats.api_dispatch_due_worker_wall_ms_histogram = d.dispatch_due_worker_wall_ms_histogram;
    stats.api_dispatch_queued_worker_wall_ms_histogram = d.dispatch_queued_worker_wall_ms_histogram;
    stats.api_scheduler_loop_tick_worker_wall_ms_histogram =
        d.scheduler_loop_tick_worker_wall_ms_histogram;
    stats
}

#[cfg(test)]
mod api_process_counter_merge_tests {
    use super::*;
    use hc_service::scheduler::ScheduledRunDispatchSlipMillisecondsHistogram;

    #[test]
    fn merge_overwrites_placeholder_api_counters_from_maps() {
        let stats = SchedulerOperationalStats {
            now_unix: 9,
            followup_total: 1,
            followup_pending: 0,
            followup_pending_due: 0,
            followup_fired: 0,
            followup_cancelled: 0,
            followup_failed: 0,
            schedule_total: 0,
            schedule_active: 0,
            schedule_paused: 0,
            schedule_cancelled: 0,
            schedule_timed_mirror_active: 0,
            run_queued: 0,
            run_running: 0,
            run_succeeded: 0,
            run_failed: 0,
            run_cancelled: 0,
            api_followup_messages_delivered_total: 999,
            api_followup_headless_messages_delivered_total: 999,
            api_dispatch_due_completed_total: 999,
            api_dispatch_due_failed_total: 999,
            api_dispatch_queued_completed_total: 999,
            api_dispatch_queued_failed_total: 999,
            api_scheduler_loop_tick_completed_total: 999,
            api_scheduler_loop_tick_failed_total: 999,
            api_dispatch_due_last_worker_wall_ms: 999,
            api_dispatch_queued_last_worker_wall_ms: 999,
            api_scheduler_loop_tick_last_worker_wall_ms: 999,
            api_dispatch_due_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram {
                    count: 777,
                    sum_ms: 777,
                    bucket_le_ms_10: 1,
                    bucket_le_ms_50: 2,
                    bucket_le_ms_100: 3,
                    bucket_le_ms_500: 4,
                },
            api_dispatch_queued_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram {
                    count: 888,
                    sum_ms: 888,
                    bucket_le_ms_10: 8,
                    bucket_le_ms_50: 8,
                    bucket_le_ms_100: 8,
                    bucket_le_ms_500: 8,
                },
            api_scheduler_loop_tick_worker_wall_ms_histogram:
                ApiDispatchDueWorkerWallMillisecondsHistogram {
                    count: 666,
                    sum_ms: 666,
                    bucket_le_ms_10: 6,
                    bucket_le_ms_50: 6,
                    bucket_le_ms_100: 6,
                    bucket_le_ms_500: 6,
                },
            scheduled_run_dispatch_slip_ms_histogram:
                ScheduledRunDispatchSlipMillisecondsHistogram {
                    count: 888,
                    sum_ms: 8,
                    bucket_le_ms_1000: 8,
                    bucket_le_ms_5000: 8,
                    bucket_le_ms_30000: 8,
                    bucket_le_ms_60000: 8,
                    bucket_le_ms_300000: 8,
                    bucket_le_ms_3600000: 8,
                },
        };

        let headless = Arc::new(Mutex::new(HashMap::new()));
        let dispatch = Arc::new(Mutex::new(HashMap::new()));

        let ns = ApiNamespace {
            tenant_id: "tenant-a".into(),
            user_id: "user-b".into(),
        };

        record_followup_headless_delivered(&headless, &ns, 4);
        record_api_dispatch_totals(&dispatch, &ns, |t| {
            t.dispatch_due_completed = 5;
            t.dispatch_due_failed = 1;
            t.dispatch_queued_completed = 2;
            t.scheduler_loop_tick_failed = 7;
            t.last_dispatch_due_worker_wall_ms = 42;
            t.last_dispatch_queued_worker_wall_ms = 43;
            t.last_scheduler_tick_worker_wall_ms = 44;
            t.dispatch_due_worker_wall_ms_histogram.observe(10);
            t.dispatch_due_worker_wall_ms_histogram.observe(350);
            t.dispatch_queued_worker_wall_ms_histogram.observe(25);
            t.scheduler_loop_tick_worker_wall_ms_histogram.observe(40);
        });

        let merged = merge_scheduler_operational_stats_with_api_counters(
            stats, &headless, &dispatch, "tenant-a", "user-b",
        );

        assert_eq!(merged.now_unix, 9);
        assert_eq!(merged.followup_total, 1);
        assert_eq!(merged.api_followup_messages_delivered_total, 4);
        assert_eq!(merged.api_followup_headless_messages_delivered_total, 4);
        assert_eq!(merged.api_dispatch_due_completed_total, 5);
        assert_eq!(merged.api_dispatch_due_failed_total, 1);
        assert_eq!(merged.api_dispatch_queued_completed_total, 2);
        assert_eq!(merged.api_dispatch_queued_failed_total, 0);
        assert_eq!(merged.api_scheduler_loop_tick_completed_total, 0);
        assert_eq!(merged.api_scheduler_loop_tick_failed_total, 7);
        assert_eq!(merged.api_dispatch_due_last_worker_wall_ms, 42);
        assert_eq!(merged.api_dispatch_queued_last_worker_wall_ms, 43);
        assert_eq!(merged.api_scheduler_loop_tick_last_worker_wall_ms, 44);
        let ph = merged.api_dispatch_due_worker_wall_ms_histogram;
        assert_eq!(ph.count, 2);
        assert_eq!(ph.sum_ms, 360);
        assert_eq!(ph.bucket_le_ms_10, 1);
        assert_eq!(ph.bucket_le_ms_50, 1);
        assert_eq!(ph.bucket_le_ms_100, 1);
        assert_eq!(ph.bucket_le_ms_500, 2);
        let qh = merged.api_dispatch_queued_worker_wall_ms_histogram;
        assert_eq!(qh.count, 1);
        assert_eq!(qh.sum_ms, 25);
        assert_eq!(qh.bucket_le_ms_50, 1);
        let th = merged.api_scheduler_loop_tick_worker_wall_ms_histogram;
        assert_eq!(th.count, 1);
        assert_eq!(th.sum_ms, 40);
        assert_eq!(th.bucket_le_ms_50, 1);
        assert_eq!(merged.scheduled_run_dispatch_slip_ms_histogram.count, 888);
    }
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

// 房间能力增强相关函数
#[derive(Debug)]
struct RoomCapabilitiesInfo {
    capabilities: Vec<String>,
    tools: Vec<String>,
    skills: Vec<String>,
}

fn room_capabilities_info_from_routing(ctx: &RoomRoutingContext) -> RoomCapabilitiesInfo {
    let lists = ctx.response_capability_lists();
    RoomCapabilitiesInfo {
        capabilities: lists.capabilities,
        tools: lists.tools,
        skills: lists.skills,
    }
}

async fn enhance_chat_request_with_room_capabilities(
    state: &AppState,
    request: &ChatRequest,
) -> Result<
    (
        ChatRequest,
        Option<DecisionRecord>,
        Option<RoomRoutingContext>,
    ),
    ApiError,
> {
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

        let decision = behavior_engine
            .make_decision(DecisionType::ResponseStyle, options)
            .map_err(|e| ApiError(anyhow::anyhow!("Behavior decision failed: {}", e)))?;

        return Ok((enhanced_request, Some(decision), None));
    };

    let room_routing = match resolve_room_routing_context(&state.service, request) {
        Ok(Some(context)) => context,
        Ok(None) => return Ok((enhanced_request, None, None)),
        Err(err) => {
            error!(%room_id, error = %err, "failed to resolve room routing context");
            return Ok((enhanced_request, None, None));
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

    let decision = behavior_engine
        .make_decision(DecisionType::CapabilityCreation, capability_options)
        .map_err(|e| ApiError(anyhow::anyhow!("Room capability decision failed: {}", e)))?;

    // 根据决策结果增强系统提示
    enhanced_request.system_prompt = enhance_system_prompt_with_behavior_and_room_capabilities(
        &room_routing.resolved,
        &room_routing.room,
        &behavior_config,
        &decision,
        &enhanced_request.system_prompt,
    );

    Ok((enhanced_request, Some(decision), Some(room_routing)))
}

async fn get_room_capabilities_info(
    state: &AppState,
    room_id: &str,
    namespace: &ApiNamespace,
) -> Result<RoomCapabilitiesInfo, ApiError> {
    let request = room_lookup_request(room_id, namespace);
    let Some(context) = resolve_room_routing_context(&state.service, &request)
        .map_err(|e| ApiError(anyhow!("Failed to resolve room routing: {}", e)))?
    else {
        return Ok(RoomCapabilitiesInfo {
            capabilities: Vec::new(),
            tools: Vec::new(),
            skills: Vec::new(),
        });
    };

    Ok(room_capabilities_info_from_routing(&context))
}

/// Payload for SSE/WS event `chat.room_capabilities` (matches REST `ChatResponse` `room_*_used`).
async fn room_capabilities_stream_data(
    state: &AppState,
    request: &ChatRequest,
    room_ctx: Option<&RoomRoutingContext>,
) -> Result<Option<Value>, ApiError> {
    let Some(room_id) = request
        .room_id
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let chat_namespace = normalized_request_namespace(
        ApiNamespace::default(),
        request.tenant_id.clone(),
        request.user_id.clone(),
    );
    let info = if let Some(ctx) = room_ctx {
        room_capabilities_info_from_routing(ctx)
    } else {
        get_room_capabilities_info(state, room_id, &chat_namespace).await?
    };
    Ok(Some(json!({
        "type": "room_capabilities",
        "room_id": room_id,
        "room_capabilities_used": info.capabilities,
        "room_tools_used": info.tools,
        "room_skills_used": info.skills,
    })))
}

fn enhance_system_prompt_with_behavior_and_room_capabilities(
    room_capabilities: &ResolvedRoomCapabilities,
    room: &MemoryRoom,
    behavior_config: &BehaviorConfig,
    decision: &DecisionRecord,
    base_prompt: &Option<String>,
) -> Option<String> {
    let base = base_prompt
        .as_deref()
        .unwrap_or("You are a helpful assistant.");

    // 构建行为模式指导
    let behavior_guidance = match behavior_config.pattern {
        BehaviorPattern::Passive => {
            "Behavior Mode: PASSIVE - Execute instructions directly and precisely. Minimize interpretation and stick to explicit requests."
        }
        BehaviorPattern::Stable => {
            "Behavior Mode: STABLE - Use proven, reliable approaches. Prioritize accuracy and consistency over innovation."
        }
        BehaviorPattern::Learning => {
            "Behavior Mode: LEARNING - Balance reliability with careful exploration. Learn from interactions and suggest improvements when appropriate."
        }
        BehaviorPattern::Creative => {
            "Behavior Mode: CREATIVE - Pursue optimal solutions through innovative approaches. Be willing to explore new methods and possibilities."
        }
        BehaviorPattern::Adaptive => {
            "Behavior Mode: ADAPTIVE - Dynamically adjust approach based on context. Use the most appropriate strategy for each situation."
        }
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
        let tools_list: Vec<String> = room_capabilities
            .tools
            .iter()
            .map(|tool| {
                format!(
                    "  - {}: Tool from {:?}",
                    tool.tool_ref.id, tool.tool_ref.inheritance_type
                )
            })
            .collect();
        capability_sections.push(format!("Available Tools:\n{}", tools_list.join("\n")));
    }

    // 添加可用技能
    if !room_capabilities.skills.is_empty() {
        let skills_list: Vec<String> = room_capabilities
            .skills
            .iter()
            .map(|skill| {
                format!(
                    "  - {}: Skill from {:?}",
                    skill.skill_ref.id, skill.skill_ref.inheritance_type
                )
            })
            .collect();
        capability_sections.push(format!("Available Skills:\n{}", skills_list.join("\n")));
    }

    // 添加执行上下文信息
    let exec_ctx = &room.room_config.execution_context;
    if exec_ctx.working_directory.is_some()
        || exec_ctx.default_namespace.is_some()
        || !exec_ctx.environment.is_empty()
    {
        let mut context_info = Vec::new();

        if let Some(wd) = &exec_ctx.working_directory {
            context_info.push(format!("Working Directory: {}", wd));
        }

        if let Some(ns) = &exec_ctx.default_namespace {
            context_info.push(format!("Default Namespace: {}", ns));
        }

        if !exec_ctx.environment.is_empty() {
            let env_vars: Vec<String> = exec_ctx
                .environment
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

#[cfg(test)]
mod user_message_to_chat_request_tests {
    use super::{UserMessageRequest, chat_request_from_user_message};
    use hc_protocol::{ApiMessageRole, ChatRequest};

    fn chat_from_json(value: serde_json::Value) -> ChatRequest {
        let req: UserMessageRequest =
            serde_json::from_value(value).expect("UserMessageBody JSON -> UserMessageRequest");
        chat_request_from_user_message(req)
    }

    #[test]
    fn minimal_body_maps_text_default_memory_namespace() {
        let chat = chat_from_json(serde_json::json!({
            "text": "你好",
        }));
        assert_eq!(chat.input.as_deref(), Some("你好"));
        assert!(chat.messages.is_empty());
        assert_eq!(chat.memory.namespace.tenant_id, "local");
        assert_eq!(chat.memory.namespace.user_id, "default");
    }

    #[test]
    fn top_level_tenant_user_merge_into_chat_and_memory_namespace() {
        let chat = chat_from_json(serde_json::json!({
            "text": "hello",
            "tenant_id": "acme",
            "user_id": "alice",
        }));
        assert_eq!(chat.memory.namespace.tenant_id, "acme");
        assert_eq!(chat.memory.namespace.user_id, "alice");
        assert_eq!(chat.tenant_id.as_deref(), Some("acme"));
        assert_eq!(chat.user_id.as_deref(), Some("alice"));
    }

    #[test]
    fn memory_overlay_preserves_explicit_memory_fields() {
        let chat = chat_from_json(serde_json::json!({
            "text": "x",
            "memory": {
                "scope": "session",
                "limit": 4,
                "text": "refine",
                "kind": "knowledge",
                "tag": "t1",
            }
        }));
        assert_eq!(chat.memory.scope.as_deref(), Some("session"));
        assert_eq!(chat.memory.limit, Some(4));
        assert_eq!(chat.memory.text.as_deref(), Some("refine"));
        assert_eq!(chat.memory.kind.as_deref(), Some("knowledge"));
        assert_eq!(chat.memory.tag.as_deref(), Some("t1"));
    }

    #[test]
    fn memory_namespace_when_non_defaults_overrides_identity() {
        let chat = chat_from_json(serde_json::json!({
            "text": "x",
            "tenant_id": "zzz",
            "memory": {
                "namespace": { "tenant_id": "corp", "user_id": "u9" },
            }
        }));
        assert_eq!(chat.memory.namespace.tenant_id, "corp");
        assert_eq!(chat.memory.namespace.user_id, "u9");
        assert_eq!(chat.tenant_id.as_deref(), Some("corp"));
    }

    #[test]
    fn swarm_and_llm_hints_round_trip() {
        let chat = chat_from_json(serde_json::json!({
            "text": "t",
            "active_task_id": "task.coord.x",
            "active_work_item_id": "work-item.0003",
            "provider": "p",
            "model": "m",
            "temperature": 0.2,
            "max_output_tokens": 999,
            "thinking_depth": 2,
            "behavior_pattern": "stable",
            "system_prompt": "Be brief.",
        }));
        assert_eq!(chat.active_task_id.as_deref(), Some("task.coord.x"));
        assert_eq!(chat.active_work_item_id.as_deref(), Some("work-item.0003"));
        assert_eq!(chat.provider.as_deref(), Some("p"));
        assert_eq!(chat.model.as_deref(), Some("m"));
        assert_eq!(chat.temperature, Some(0.2));
        assert_eq!(chat.max_output_tokens, Some(999));
        assert_eq!(chat.thinking_depth, Some(2u8));
        assert_eq!(chat.behavior_pattern.as_deref(), Some("stable"));
        assert_eq!(chat.system_prompt.as_deref(), Some("Be brief."));
    }

    #[test]
    fn optional_messages_carry_over() {
        let chat = chat_from_json(serde_json::json!({
            "text": "last",
            "messages": [
                { "role": "user", "content": "first" },
                { "role": "assistant", "content": "second" }
            ]
        }));
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, ApiMessageRole::User);
        assert_eq!(chat.messages[0].content, "first");
        assert_eq!(chat.messages[1].role, ApiMessageRole::Assistant);
        assert_eq!(chat.messages[1].content, "second");
    }
}
