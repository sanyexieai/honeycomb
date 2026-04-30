#![recursion_limit = "256"]

use std::{
    collections::BTreeSet, convert::Infallible, env, fs, net::SocketAddr, path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{
        Html, IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::StreamExt;
use hc_conversation::{ConversationRepository, now_unix};
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

#[derive(Debug, Clone)]
struct AppState {
    service: ServiceConfig,
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

#[tokio::main]
async fn main() -> Result<()> {
    load_local_env_file()?;
    let state = AppState {
        service: ServiceConfig::new(workspace_root()),
    };
    start_scheduler_loop_if_enabled(state.service.clone());
    let bind_addr = bind_addr()?;
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
        .route("/v1/turn", post(turn))
        .route("/v1/turn/stream", post(turn_stream))
        .route("/v1/messages", post(message))
        .route("/v1/messages/stream", post(message_stream))
        .with_state(state);

    println!("hc-api listening on http://{bind_addr}");
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

fn start_scheduler_loop_if_enabled(service: ServiceConfig) {
    if !env_flag("HC_SCHEDULER_ENABLED") {
        return;
    }
    let tick_seconds = env::var("HC_SCHEDULER_TICK_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(30);
    let namespace = ApiNamespace {
        tenant_id: env::var("HC_TENANT_ID").unwrap_or_else(|_| default_tenant_id()),
        user_id: env::var("HC_USER_ID").unwrap_or_else(|_| default_user_id()),
    };
    println!(
        "hc-api scheduler enabled namespace={}/{} tick_seconds={}",
        namespace.tenant_id, namespace.user_id, tick_seconds
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
                        println!(
                            "scheduler> dispatched={} queued={}",
                            report.receipts.len(),
                            report.queued_count
                        );
                    }
                }
                Ok(Err(error)) => eprintln!("warning> scheduler tick failed: {error}"),
                Err(error) => eprintln!("warning> scheduler worker failed: {error}"),
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

async fn swagger_ui() -> Html<String> {
    Html(swagger_ui_html())
}

async fn chat(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    let response =
        tokio::task::spawn_blocking(move || handle_turn_request(&state.service, request))
            .await
            .map_err(|error| ApiError(anyhow!("chat worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn turn(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    let response =
        tokio::task::spawn_blocking(move || handle_turn_request(&state.service, request))
            .await
            .map_err(|error| ApiError(anyhow!("turn worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn message(
    State(state): State<AppState>,
    Json(request): Json<UserMessageRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    turn(State(state), Json(chat_request_from_user_message(request))).await
}

async fn chat_stream(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    turn_stream(State(state), Json(request)).await
}

async fn message_stream(
    State(state): State<AppState>,
    Json(request): Json<UserMessageRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    turn_stream(State(state), Json(chat_request_from_user_message(request))).await
}

async fn turn_stream(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(16);
    let service = state.service.clone();
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
            handle_turn_stream_request(&service, request, &mut on_event)
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
                    "description": "Streams chat lifecycle events and model deltas. Events include chat.started, chat.delta, chat.completed, and chat.error.",
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
            "/v1/turn": {
                "post": {
                    "summary": "Run one conversational turn",
                    "operationId": "runTurn",
                    "description": "Canonical non-streaming turn endpoint. It currently delegates to chat generation and is the stable API surface for moving CLI turn nodes into the service layer.",
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
                            "description": "Completed turn response",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ChatResponse" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/turn/stream": {
                "post": {
                    "summary": "Run one conversational turn over Server-Sent Events",
                    "operationId": "streamTurn",
                    "description": "Canonical streaming turn endpoint. Events include turn.started, chat.started, chat.delta, chat.completed, turn.completed, and chat.error.",
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
                            "description": "SSE stream of turn and chat events",
                            "content": {
                                "text/event-stream": {
                                    "schema": { "type": "string" }
                                }
                            }
                        }
                    }
                }
            },
            "/v1/messages": {
                "post": {
                    "summary": "Send a user message",
                    "operationId": "sendMessage",
                    "description": "Lightweight user-facing message endpoint. Server-side runtime variables and routing decide provider, model, memory, and agent behavior.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/UserMessageRequest" },
                                "examples": {
                                    "simple": {
                                        "value": {
                                            "text": "中午推荐我吃什么"
                                        }
                                    }
                                }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Completed message response",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ChatResponse" }
                                }
                            }
                        },
                        "400": { "$ref": "#/components/responses/BadRequest" },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/messages/stream": {
                "post": {
                    "summary": "Send a user message over Server-Sent Events",
                    "operationId": "streamMessage",
                    "description": "Lightweight streaming message endpoint. Events are the same as /v1/turn/stream.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/UserMessageRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "SSE stream of turn and chat events",
                            "content": {
                                "text/event-stream": {
                                    "schema": { "type": "string" }
                                }
                            }
                        }
                    }
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
                "UserMessageRequest": {
                    "type": "object",
                    "required": ["text"],
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "User-visible message text."
                        },
                        "tenant_id": {
                            "type": "string",
                            "default": "local"
                        },
                        "user_id": {
                            "type": "string",
                            "default": "default"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional conversation/session id. Empty values use a namespace-scoped default session."
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Optional explicit agent id."
                        },
                        "domain_id": {
                            "type": "string",
                            "description": "Optional domain hint."
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
                        "selected_agent_id": { "type": "string" },
                        "selected_domain_id": { "type": "string" },
                        "recalled_memories": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/MemoryRef" }
                        },
                        "synthesized_prompt_asset_count": {
                            "type": "integer",
                            "minimum": 0
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

fn swagger_ui_html() -> String {
    let spec = openapi_document();
    let spec_json = serde_json::to_string(&spec).expect("openapi document should serialize");
    SWAGGER_UI_HTML.replace("__HONEYCOMB_OPENAPI_SPEC__", &spec_json)
}

fn bind_addr() -> Result<SocketAddr> {
    env::var("HC_API_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_owned())
        .parse()
        .context("invalid HC_API_BIND")
}

fn workspace_root() -> PathBuf {
    env::var("HC_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("workspace"))
}

fn default_tenant_id() -> String {
    "local".to_owned()
}

fn default_user_id() -> String {
    "default".to_owned()
}

fn load_local_env_file() -> Result<()> {
    let env_path = env::current_dir()
        .context("failed to read current directory")?
        .join(".env");
    if !env_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&env_path)
        .with_context(|| format!("failed to read {}", env_path.display()))?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || env::var_os(key).is_some() {
            continue;
        }
        unsafe {
            env::set_var(key, clean_env_value(value));
        }
    }
    Ok(())
}

fn clean_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

const SWAGGER_UI_HTML: &str = r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Honeycomb API Swagger</title>
    <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css" />
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
    <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
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
