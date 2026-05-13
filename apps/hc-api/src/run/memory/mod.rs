//! Memory Room HTTP 路由（/v1/memory/...）。

use anyhow::{Context, Result, anyhow};
use axum::{
    Json,
    extract::{Path, Query, State},
};
use hc_protocol::{ApiNamespace, ChatRequest};
use hc_service::room_routing::{RoomRoutingExplain, resolve_room_routing_explain};
use hc_service::transport::{
    CapabilityRef, MemoryLayer, MemoryNamespace, MemoryRoom, MemoryRoomRepository,
    RoomCapabilityResolver, RoomConfig, ScheduleRef, SkillRef, ToolRef, WorkspaceNamespace,
    workspace_root,
};

use super::{ApiError, AppState, NamespaceQuery, normalized_request_namespace};

// Memory Room API 结构
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct MemoryRoomListResponse {
    rooms: Vec<MemoryRoomSummary>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct MemoryRoomSummary {
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
pub(super) struct MemoryRoomResponse {
    room: MemoryRoomDetail,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct MemoryRoomDetail {
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
pub(super) struct MemoryNamespaceResponse {
    tenant_id: String,
    user_id: String,
}

#[derive(serde::Deserialize)]
pub(super) struct MemoryRoomWriteRequest {
    id: Option<String>,
    layer: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    status: Option<String>,
    tags: Option<Vec<String>>,
    room_config: Option<RoomConfig>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct RoomCapabilitiesResponse {
    room_id: String,
    resolved_capabilities: Vec<ResolvedCapabilityResponse>,
    resolved_tools: Vec<ResolvedToolResponse>,
    resolved_skills: Vec<ResolvedSkillResponse>,
    resolved_schedules: Vec<ResolvedScheduleResponse>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct RoomRoutingResponse {
    room_id: String,
    enabled_providers: Vec<String>,
    provider_weights: std::collections::BTreeMap<String, i32>,
    capability_ids: Vec<String>,
    skill_ids: Vec<String>,
    allowed_tool_ids: Vec<String>,
    provider_argument_override_keys: std::collections::BTreeMap<String, Vec<String>>,
    tool_argument_override_keys: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ResolvedCapabilityResponse {
    capability_ref: CapabilityRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ResolvedToolResponse {
    tool_ref: ToolRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ResolvedSkillResponse {
    skill_ref: SkillRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ResolvedScheduleResponse {
    schedule_ref: ScheduleRef,
    source_data: Option<serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct InheritCapabilityRequest {
    capability_ref: CapabilityRef,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct InheritToolRequest {
    tool_ref: ToolRef,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct InheritSkillRequest {
    skill_ref: SkillRef,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct InheritScheduleRequest {
    schedule_ref: ScheduleRef,
}

pub(super) fn room_lookup_request(room_id: &str, namespace: &ApiNamespace) -> ChatRequest {
    ChatRequest {
        tenant_id: Some(namespace.tenant_id.clone()),
        user_id: Some(namespace.user_id.clone()),
        session_id: None,
        room_id: Some(room_id.to_owned()),
        behavior_pattern: None,
        thinking_depth: None,
        input: None,
        messages: Vec::new(),
        provider: None,
        model: None,
        system_prompt: None,
        agent_id: None,
        domain_id: None,
        active_agent_id: None,
        active_task_id: None,
        active_work_item_id: None,
        memory: hc_protocol::ApiMemoryQuery {
            namespace: namespace.clone(),
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

pub(super) fn room_routing_response(explain: RoomRoutingExplain) -> RoomRoutingResponse {
    RoomRoutingResponse {
        room_id: explain.room_id,
        enabled_providers: explain.enabled_providers,
        provider_weights: explain.provider_weights,
        capability_ids: explain.capability_ids,
        skill_ids: explain.skill_ids,
        allowed_tool_ids: explain.allowed_tool_ids,
        provider_argument_override_keys: explain.provider_argument_override_keys,
        tool_argument_override_keys: explain.tool_argument_override_keys,
    }
}

// Memory Room API 处理函数
pub(super) async fn memory_rooms(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );

    let rooms = repository
        .list_rooms()
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

pub(super) async fn memory_room_get(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
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
                room_config: serde_json::to_value(&room.room_config).map_err(|e| {
                    ApiError(anyhow::anyhow!("Failed to serialize room config: {}", e))
                })?,
            };
            Ok(Json(MemoryRoomResponse { room: room_detail }))
        }
        None => Err(ApiError(anyhow!("Room not found: {}", room_id))),
    }
}

pub(super) fn memory_room_detail(room: MemoryRoom) -> Result<MemoryRoomDetail, ApiError> {
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

pub(super) fn parse_memory_layer(value: Option<&str>) -> Result<MemoryLayer, ApiError> {
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

pub(super) async fn memory_room_create(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
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

pub(super) async fn memory_room_update(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
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

pub(super) async fn memory_room_capabilities(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    match repository.get_room_by_id(&room_id)? {
        Some(room) => match resolver.resolve_room_capabilities(&room) {
            Ok(capabilities) => {
                let response = RoomCapabilitiesResponse {
                    room_id: room_id.clone(),
                    resolved_capabilities: capabilities
                        .capabilities
                        .into_iter()
                        .map(|cap| ResolvedCapabilityResponse {
                            capability_ref: cap.capability_ref,
                            source_data: None,
                        })
                        .collect(),
                    resolved_tools: capabilities
                        .tools
                        .into_iter()
                        .map(|tool| ResolvedToolResponse {
                            tool_ref: tool.tool_ref,
                            source_data: None,
                        })
                        .collect(),
                    resolved_skills: capabilities
                        .skills
                        .into_iter()
                        .map(|skill| ResolvedSkillResponse {
                            skill_ref: skill.skill_ref,
                            source_data: None,
                        })
                        .collect(),
                    resolved_schedules: capabilities
                        .schedules
                        .into_iter()
                        .map(|schedule| ResolvedScheduleResponse {
                            schedule_ref: schedule.schedule_ref,
                            source_data: None,
                        })
                        .collect(),
                };
                Ok(Json(response))
            }
            Err(err) => Err(ApiError(anyhow!(
                "Failed to resolve room capabilities: {}",
                err
            ))),
        },
        None => Err(ApiError(anyhow!("Room not found: {}", room_id))),
    }
}

pub(super) async fn memory_room_routing(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<RoomRoutingResponse>, ApiError> {
    let namespace = normalized_request_namespace(
        ApiNamespace::default(),
        Some(query.tenant_id),
        Some(query.user_id),
    );
    let request = room_lookup_request(&room_id, &namespace);
    let explain = resolve_room_routing_explain(&state.service, &request)
        .map_err(|e| ApiError(anyhow!("Failed to resolve room routing: {}", e)))?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    Ok(Json(room_routing_response(explain)))
}

pub(super) async fn memory_room_inherit_capability(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_capability_to_room(&mut room, request.capability_ref)?;
    repository.write_room(&room)?;

    Ok(Json(
        serde_json::json!({"success": true, "message": "Capability inheritance added"}),
    ))
}

pub(super) async fn memory_room_inherit_tool(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_tool_to_room(&mut room, request.tool_ref)?;
    repository.write_room(&room)?;

    Ok(Json(
        serde_json::json!({"success": true, "message": "Tool inheritance added"}),
    ))
}

pub(super) async fn memory_room_inherit_skill(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_skill_to_room(&mut room, request.skill_ref)?;
    repository.write_room(&room)?;

    Ok(Json(
        serde_json::json!({"success": true, "message": "Skill inheritance added"}),
    ))
}

pub(super) async fn memory_room_inherit_schedule(
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
        WorkspaceNamespace::new(&namespace.tenant_id, &namespace.user_id),
    );
    let resolver = RoomCapabilityResolver::new(memory_namespace);

    let mut room = repository
        .get_room_by_id(&room_id)?
        .ok_or_else(|| ApiError(anyhow!("Room not found: {}", room_id)))?;
    resolver.add_schedule_to_room(&mut room, request.schedule_ref)?;
    repository.write_room(&room)?;

    Ok(Json(
        serde_json::json!({"success": true, "message": "Schedule inheritance added"}),
    ))
}
