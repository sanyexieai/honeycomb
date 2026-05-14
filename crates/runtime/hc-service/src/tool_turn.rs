use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use anyhow::{Context, Result};
use hc_agent::routing::phrase_match_score_with_stop_terms;
use hc_context::runtime::default_session_id;
use hc_protocol::{ApiChatMessage, ApiMessageRole, ApiNamespace, ChatRequest, ChatResponse};
use hc_toolchain::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    ServiceConfig,
    timed_turn::{ReminderRule, TimedSequenceRule},
    tool::{McpToolListRequest, list_mcp_tools},
    tool_execution::{execute_tool_invocation, mcp_invocation_plan, require_mcp_metadata},
};

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ToolRoutingTags {
    #[serde(default)]
    intent_rules: Vec<ToolIntentRoutingRule>,
    #[serde(default)]
    tool_weights: Vec<ToolWeightRule>,
    #[serde(default)]
    confirmation_hints: Vec<String>,
    #[serde(default)]
    routing_stop_terms: Vec<String>,
    #[serde(default)]
    argument_rules: Vec<ToolArgumentRule>,
    #[serde(default)]
    tool_argument_rules: Vec<ToolScopedArgumentRule>,
    #[serde(default)]
    confirmation_flows: Vec<ToolConfirmationFlowRule>,
    #[serde(default)]
    pub(crate) timed_sequence_rules: Vec<TimedSequenceRule>,
    #[serde(default)]
    pub(crate) reminder_rules: Vec<ReminderRule>,
}

impl ToolRoutingTags {
    pub(crate) fn ensure_builtin_timed_sequences(&mut self) {
        if self.timed_sequence_rules.is_empty() {
            self.timed_sequence_rules = crate::timed_turn::builtin_timed_sequence_rules();
        }
        if self.reminder_rules.is_empty() {
            self.reminder_rules = crate::timed_turn::builtin_reminder_rules();
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolIntentRoutingRule {
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default)]
    examples: Vec<String>,
    #[serde(default)]
    negative_examples: Vec<String>,
    #[serde(default)]
    preferred_selectors: Vec<String>,
    #[serde(default)]
    weight: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolWeightRule {
    #[serde(default)]
    selectors: Vec<String>,
    #[serde(default)]
    weight: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolArgumentRule {
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default)]
    args: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolScopedArgumentRule {
    #[serde(default)]
    selectors: Vec<String>,
    #[serde(default)]
    args: BTreeMap<String, Value>,
    #[serde(default)]
    include_matched_argument_rules: bool,
    #[serde(default)]
    context_calls: Vec<ToolContextCallRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolContextCallRule {
    arg: String,
    server_id: String,
    #[serde(default)]
    tool_names: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolResponseRenderingConfig {
    #[serde(default)]
    renderers: Vec<ToolResponseRenderer>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolResponseRenderer {
    #[allow(dead_code)]
    #[serde(default)]
    id: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    selectors: Vec<String>,
    #[serde(default)]
    context_arg: Option<String>,
    #[serde(default)]
    item_paths: Vec<String>,
    #[serde(default)]
    name_keys: Vec<String>,
    #[serde(default)]
    primary_fields: Vec<ToolResponseField>,
    #[serde(default)]
    alternative_fields: Vec<ToolResponseField>,
    #[serde(default)]
    reason_array_keys: Vec<String>,
    #[serde(default)]
    reason_keys: Vec<String>,
    #[serde(default)]
    header: Option<String>,
    #[serde(default)]
    header_with_context: Option<String>,
    #[serde(default)]
    primary_heading: Option<String>,
    #[serde(default)]
    alternatives_heading: Option<String>,
    #[serde(default)]
    confirmation_prompt: Option<String>,
    order_success: Option<String>,
    #[serde(default)]
    status_labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ToolResponseField {
    label: String,
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    max_len: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolConfirmationFlowRule {
    #[serde(default)]
    pub source_selectors: Vec<String>,
    #[serde(default)]
    pub target_selectors: Vec<String>,
    #[serde(default)]
    pub fallback_target_selectors: Vec<Vec<String>>,
    #[serde(default)]
    pub item_paths: Vec<String>,
    #[serde(default)]
    pub target_args: BTreeMap<String, Value>,
    #[serde(default)]
    pub item_arg_mappings: Vec<ToolItemArgMapping>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolItemArgMapping {
    pub arg: String,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolConfirmation {
    pub tool_id: String,
    pub fallback_tool_ids: Vec<String>,
    pub items: Vec<Value>,
    pub flow: ToolConfirmationFlowRule,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolTurnSessionState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_confirmation: Option<PendingToolConfirmation>,
}

#[derive(Debug, Clone)]
pub struct ConfirmedPendingToolRoute {
    pub tool_id: String,
    pub fallback_tool_ids: Vec<String>,
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ConfiguredMcpRoute {
    pub namespace: ApiNamespace,
    pub input: String,
    pub tool: ToolSpec,
    pub server_id: String,
    pub tool_name: String,
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct ToolTurnResult {
    pub response: ChatResponse,
    pub tool_id: String,
    pub server_id: String,
    pub tool_name: String,
    pub result: Value,
}

#[derive(Debug, Clone)]
pub struct PendingToolExecutionPlan {
    pub namespace: ApiNamespace,
    pub session_id: String,
    pub route: ConfirmedPendingToolRoute,
}

pub fn try_handle_configured_mcp_turn(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<ToolTurnResult>> {
    if let Some(tool_result) = try_handle_persisted_pending_confirmation(config, request)? {
        return Ok(Some(tool_result));
    }
    try_handle_configured_mcp_route_turn(config, request)
}

/// Configured MCP route only (no pending-confirmation branch). Use when confirmation was handled earlier.
pub fn try_handle_configured_mcp_route_turn(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<ToolTurnResult>> {
    try_handle_configured_mcp_route_turn_with_filter(config, request, None)
}

pub fn try_handle_configured_mcp_route_turn_with_filter(
    config: &ServiceConfig,
    request: &ChatRequest,
    allowed_tool_ids: Option<&BTreeSet<String>>,
) -> Result<Option<ToolTurnResult>> {
    let Some(route) =
        resolve_configured_mcp_route_with_policy(config, request, allowed_tool_ids, None, None)?
    else {
        return Ok(None);
    };
    let result = execute_configured_mcp_route(config, request, route)?;
    if let Some(tool_result) = &result {
        persist_pending_confirmation_for_result(config, request, tool_result)?;
    }
    Ok(result)
}

pub fn try_handle_persisted_pending_confirmation(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<ToolTurnResult>> {
    let Some(plan) = resolve_persisted_pending_confirmation_plan(config, request)? else {
        return Ok(None);
    };
    let result = execute_persisted_pending_confirmation_plan(config, request, plan)?;
    Ok(Some(result))
}

pub fn resolve_persisted_pending_confirmation_plan(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<PendingToolExecutionPlan>> {
    let input = request_input(request)?;
    let namespace = request_namespace(request);
    let session_id = request_session_id(request, &namespace);
    let state = read_tool_turn_session_state(config, &namespace, &session_id)?;
    let Some(pending) = state.pending_confirmation.as_ref() else {
        return Ok(None);
    };
    let Some(route) =
        resolve_confirmed_pending_tool_route(config, &namespace, &input, Some(pending))?
    else {
        return Ok(None);
    };
    Ok(Some(PendingToolExecutionPlan {
        namespace,
        session_id,
        route,
    }))
}

pub fn execute_persisted_pending_confirmation_plan(
    config: &ServiceConfig,
    request: &ChatRequest,
    plan: PendingToolExecutionPlan,
) -> Result<ToolTurnResult> {
    let tools = list_mcp_tools(
        config,
        McpToolListRequest {
            namespace: plan.namespace.clone(),
            tenant_id: None,
            user_id: None,
            refresh: false,
            server_id: None,
        },
    )?;
    let result = execute_confirmed_pending_tool_route(
        config,
        request,
        &plan.namespace,
        plan.route,
        &tools.tools,
    )?;
    clear_tool_turn_session_state(config, &plan.namespace, &plan.session_id)?;
    Ok(result)
}

fn persist_pending_confirmation_for_result(
    config: &ServiceConfig,
    request: &ChatRequest,
    tool_result: &ToolTurnResult,
) -> Result<()> {
    let namespace = request_namespace(request);
    let session_id = request_session_id(request, &namespace);
    let tools = list_mcp_tools(
        config,
        McpToolListRequest {
            namespace: namespace.clone(),
            tenant_id: None,
            user_id: None,
            refresh: false,
            server_id: None,
        },
    )?;
    let Some(source_tool) = tools
        .tools
        .iter()
        .find(|tool| tool.tool.id == tool_result.tool_id)
        .map(|tool| tool.tool.clone())
    else {
        clear_tool_turn_session_state(config, &namespace, &session_id)?;
        return Ok(());
    };
    let tool_specs = tools
        .tools
        .iter()
        .map(|tool| tool.tool.clone())
        .collect::<Vec<_>>();
    let pending = pending_confirmation_from_tool_result(
        config,
        &namespace,
        &source_tool,
        &tool_result.result,
        &tool_specs,
    )?;
    write_tool_turn_session_state(
        config,
        &namespace,
        &session_id,
        &ToolTurnSessionState {
            pending_confirmation: pending,
        },
    )
}

pub fn resolve_configured_mcp_route(
    config: &ServiceConfig,
    request: &ChatRequest,
) -> Result<Option<ConfiguredMcpRoute>> {
    resolve_configured_mcp_route_with_filter(config, request, None)
}

pub fn resolve_configured_mcp_route_with_filter(
    config: &ServiceConfig,
    request: &ChatRequest,
    allowed_tool_ids: Option<&BTreeSet<String>>,
) -> Result<Option<ConfiguredMcpRoute>> {
    resolve_configured_mcp_route_with_policy(config, request, allowed_tool_ids, None, None)
}

pub fn resolve_configured_mcp_route_with_policy(
    config: &ServiceConfig,
    request: &ChatRequest,
    allowed_tool_ids: Option<&BTreeSet<String>>,
    provider_argument_override: Option<&Map<String, Value>>,
    tool_argument_override: Option<&BTreeMap<String, Map<String, Value>>>,
) -> Result<Option<ConfiguredMcpRoute>> {
    let input = request_input(request)?;
    let namespace = request_namespace(request);
    let routing = load_tool_routing_tags(config, &namespace).unwrap_or_default();
    let tools = list_mcp_tools(
        config,
        McpToolListRequest {
            namespace: namespace.clone(),
            tenant_id: None,
            user_id: None,
            refresh: false,
            server_id: None,
        },
    )?;
    let Some(tool) = select_mcp_tool(&tools.tools, &routing, &input, allowed_tool_ids) else {
        return Ok(None);
    };
    let Some(server_id) = tool.tool.default_command.get(1).cloned() else {
        return Ok(None);
    };
    let Some(tool_name) = tool.tool.default_command.get(2).cloned() else {
        return Ok(None);
    };
    let mut arguments = Map::new();
    arguments.insert("query".to_owned(), Value::String(input.clone()));
    for (key, value) in
        route_arguments_for_tool(config, &namespace, request, &tool.tool, &routing, &input)
    {
        arguments.insert(key, value);
    }
    if let Some(overrides) =
        tool_argument_override.and_then(|overrides| overrides.get(&tool.tool.id))
    {
        for (key, value) in overrides {
            arguments.insert(key.clone(), value.clone());
        }
    }
    if let Some(overrides) = provider_argument_override {
        for (key, value) in overrides {
            arguments.insert(key.clone(), value.clone());
        }
    }
    Ok(Some(ConfiguredMcpRoute {
        namespace,
        input,
        tool: tool.tool,
        server_id,
        tool_name,
        arguments,
    }))
}

pub fn execute_configured_mcp_route(
    config: &ServiceConfig,
    request: &ChatRequest,
    route: ConfiguredMcpRoute,
) -> Result<Option<ToolTurnResult>> {
    let rendering = load_tool_response_rendering(config, &route.namespace).unwrap_or_default();
    let invocation = mcp_invocation_plan(
        route.tool.id.clone(),
        route.input.clone(),
        route.tool.default_command.clone(),
        route.namespace.clone(),
        request.session_id.clone(),
        route.server_id.clone(),
        route.tool_name.clone(),
        route.arguments,
    );
    let outcome = execute_tool_invocation(config, &invocation)?;
    let (server_id, tool_name, raw_result) = require_mcp_metadata(&outcome)?;
    let Some(content) = render_tool_result(&rendering, &route.tool, raw_result) else {
        return Ok(None);
    };
    Ok(Some(ToolTurnResult {
        response: ChatResponse {
            message: ApiChatMessage {
                role: ApiMessageRole::Assistant,
                content,
                name: None,
            },
            model: request.model.clone().unwrap_or_default(),
            provider: request.provider.clone().unwrap_or_default(),
            tenant_id: Some(route.namespace.tenant_id),
            user_id: Some(route.namespace.user_id),
            session_id: request.session_id.clone().or_else(|| {
                Some(default_session_id(
                    request
                        .tenant_id
                        .as_deref()
                        .unwrap_or(hc_context::runtime::DEFAULT_TENANT_ID),
                    request
                        .user_id
                        .as_deref()
                        .unwrap_or(hc_context::runtime::DEFAULT_USER_ID),
                ))
            }),
            room_id: request.room_id.clone(),
            selected_agent_id: request.agent_id.clone(),
            selected_domain_id: request.domain_id.clone(),
            selected_provider: None,
            recalled_memories: Vec::new(),
            synthesized_prompt_asset_count: 0,
            room_capabilities_used: Vec::new(),
            room_tools_used: Vec::new(),
            room_skills_used: Vec::new(),
            behavior_pattern_used: None,
            decision_reasoning: None,
            decision_confidence: None,
            active_task_id: None,
        },
        tool_id: route.tool.id,
        server_id: server_id.to_owned(),
        tool_name: tool_name.to_owned(),
        result: raw_result.clone(),
    }))
}

fn execute_confirmed_pending_tool_route(
    config: &ServiceConfig,
    request: &ChatRequest,
    namespace: &ApiNamespace,
    route: ConfirmedPendingToolRoute,
    tools: &[crate::tool::McpToolSummary],
) -> Result<ToolTurnResult> {
    let rendering = load_tool_response_rendering(config, namespace).unwrap_or_default();
    let selectors = std::iter::once(route.tool_id.clone())
        .chain(route.fallback_tool_ids.clone())
        .collect::<Vec<_>>();
    let mut last_error = None;
    for selector in selectors {
        let Some(tool) = tools.iter().find(|tool| tool.tool.id == selector) else {
            continue;
        };
        let Some(server_id) = tool.tool.default_command.get(1).cloned() else {
            continue;
        };
        let Some(tool_name) = tool.tool.default_command.get(2).cloned() else {
            continue;
        };
        let invocation = mcp_invocation_plan(
            tool.tool.id.clone(),
            route.tool_id.clone(),
            tool.tool.default_command.clone(),
            namespace.clone(),
            request.session_id.clone(),
            server_id.clone(),
            tool_name.clone(),
            route.arguments.clone(),
        );
        let outcome = execute_tool_invocation(config, &invocation)?;
        let (_, _, raw_result) = require_mcp_metadata(&outcome)?;
        if !outcome.success {
            last_error = Some(raw_result.clone());
            continue;
        }
        if let Some(content) = render_tool_result(&rendering, &tool.tool, raw_result) {
            return Ok(ToolTurnResult {
                response: ChatResponse {
                    message: ApiChatMessage {
                        role: ApiMessageRole::Assistant,
                        content,
                        name: None,
                    },
                    model: request.model.clone().unwrap_or_default(),
                    provider: request.provider.clone().unwrap_or_default(),
                    tenant_id: Some(namespace.tenant_id.clone()),
                    user_id: Some(namespace.user_id.clone()),
                    session_id: Some(request_session_id(request, namespace)),
                    room_id: request.room_id.clone(),
                    selected_agent_id: request.agent_id.clone(),
                    selected_domain_id: request.domain_id.clone(),
                    selected_provider: None,
                    recalled_memories: Vec::new(),
                    synthesized_prompt_asset_count: 0,
                    room_capabilities_used: Vec::new(),
                    room_tools_used: Vec::new(),
                    room_skills_used: Vec::new(),
                    behavior_pattern_used: None,
                    decision_reasoning: None,
                    decision_confidence: None,
                    active_task_id: None,
                },
                tool_id: tool.tool.id.clone(),
                server_id,
                tool_name,
                result: raw_result.clone(),
            });
        }
        last_error = Some(raw_result.clone());
    }
    let detail = last_error
        .map(|value| value.to_string())
        .unwrap_or_else(|| "no pending confirmation target tool was available".to_owned());
    anyhow::bail!("confirmed pending tool route failed: {detail}");
}

pub fn pending_confirmation_from_tool_result(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    source_tool: &ToolSpec,
    result: &Value,
    tools: &[ToolSpec],
) -> Result<Option<PendingToolConfirmation>> {
    let routing = load_tool_routing_tags(config, namespace).unwrap_or_default();
    let Some(flow) = routing
        .confirmation_flows
        .iter()
        .find(|flow| tool_matches_any_selector(source_tool, &flow.source_selectors))
    else {
        return Ok(None);
    };
    let Some(order_tool) = tools
        .iter()
        .find(|tool| tool_matches_all_selectors(tool, &flow.target_selectors))
    else {
        return Ok(None);
    };
    let fallback_tool_ids = confirmation_fallback_tool_ids(tools, flow, &order_tool.id);
    let items = extract_items_for_paths(result, &flow.item_paths)
        .into_iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Ok(None);
    }
    Ok(Some(PendingToolConfirmation {
        tool_id: order_tool.id.clone(),
        fallback_tool_ids,
        items,
        flow: flow.clone(),
    }))
}

pub fn confirmed_pending_arguments(
    pending: &PendingToolConfirmation,
    item: &Value,
    user_turn: &str,
) -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert(
        "query".to_owned(),
        Value::String(user_turn.trim().to_owned()),
    );
    for (key, value) in &pending.flow.target_args {
        arguments.insert(key.clone(), value.clone());
    }
    for mapping in &pending.flow.item_arg_mappings {
        if let Some(value) = value_for_keys(item, &mapping.keys)
            && let Some(rendered) = render_item_arg_mapping(value, mapping)
        {
            arguments.insert(mapping.arg.clone(), rendered);
        }
    }
    arguments
}

pub fn resolve_confirmed_pending_tool_route(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    input: &str,
    pending: Option<&PendingToolConfirmation>,
) -> Result<Option<ConfirmedPendingToolRoute>> {
    let Some(pending) = pending else {
        return Ok(None);
    };
    if !is_confirmation_turn(config, namespace, input) {
        return Ok(None);
    }
    let Some(item) = pending.items.first() else {
        return Ok(None);
    };
    Ok(Some(ConfirmedPendingToolRoute {
        tool_id: pending.tool_id.clone(),
        fallback_tool_ids: pending.fallback_tool_ids.clone(),
        arguments: confirmed_pending_arguments(pending, item, input),
    }))
}

pub fn is_confirmation_turn(config: &ServiceConfig, namespace: &ApiNamespace, input: &str) -> bool {
    let routing = load_tool_routing_tags(config, namespace).unwrap_or_default();
    routing.confirmation_hints.iter().any(|hint| {
        phrase_match_score_with_stop_terms(input, hint, &routing.routing_stop_terms) > 0
    })
}

pub(crate) fn request_input(request: &ChatRequest) -> Result<String> {
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
        .context("turn request requires input or a user message")
}

pub(crate) fn request_namespace(request: &ChatRequest) -> ApiNamespace {
    let mut namespace = request.memory.namespace.clone();
    if let Some(tenant_id) = request
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        namespace.tenant_id = tenant_id.to_owned();
    }
    if let Some(user_id) = request
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        namespace.user_id = user_id.to_owned();
    }
    if namespace.tenant_id.trim().is_empty() {
        namespace.tenant_id = hc_context::runtime::DEFAULT_TENANT_ID.to_owned();
    }
    if namespace.user_id.trim().is_empty() {
        namespace.user_id = hc_context::runtime::DEFAULT_USER_ID.to_owned();
    }
    namespace
}

fn request_session_id(request: &ChatRequest, namespace: &ApiNamespace) -> String {
    request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_session_id(&namespace.tenant_id, &namespace.user_id))
}

fn read_tool_turn_session_state(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
) -> Result<ToolTurnSessionState> {
    load_tool_turn_session_state(config, namespace, session_id)
}

pub fn load_tool_turn_session_state(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
) -> Result<ToolTurnSessionState> {
    let path = tool_turn_session_state_path(config, namespace, session_id);
    if !path.exists() {
        return Ok(ToolTurnSessionState::default());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_tool_turn_session_state(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
    state: &ToolTurnSessionState,
) -> Result<()> {
    save_tool_turn_session_state(config, namespace, session_id, state)
}

pub fn save_tool_turn_session_state(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
    state: &ToolTurnSessionState,
) -> Result<()> {
    let path = tool_turn_session_state_path(config, namespace, session_id);
    if state.pending_confirmation.is_none() {
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&path, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn clear_tool_turn_session_state(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
) -> Result<()> {
    clear_persisted_tool_turn_session_state(config, namespace, session_id)
}

pub fn clear_persisted_tool_turn_session_state(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
) -> Result<()> {
    write_tool_turn_session_state(
        config,
        namespace,
        session_id,
        &ToolTurnSessionState::default(),
    )
}

fn tool_turn_session_state_path(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    session_id: &str,
) -> PathBuf {
    config
        .workspace_root
        .join("tenants")
        .join(&namespace.tenant_id)
        .join("users")
        .join(&namespace.user_id)
        .join("sessions")
        .join(format!(
            "{}.tool-turn-state.json",
            safe_file_name(session_id)
        ))
}

fn safe_file_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn select_mcp_tool(
    tools: &[crate::tool::McpToolSummary],
    routing: &ToolRoutingTags,
    input: &str,
    allowed_tool_ids: Option<&BTreeSet<String>>,
) -> Option<crate::tool::McpToolSummary> {
    tools
        .iter()
        .filter(|tool| allowed_tool_ids.is_none_or(|allowed| allowed.contains(&tool.tool.id)))
        .filter_map(|tool| {
            let score = score_tool_for_input(&tool.tool, routing, input);
            (score > 0).then_some((tool.clone(), score))
        })
        .max_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| right.0.tool.id.cmp(&left.0.tool.id))
        })
        .map(|(tool, _)| tool)
}

fn score_tool_for_input(tool: &ToolSpec, routing: &ToolRoutingTags, input: &str) -> i32 {
    let mut score = score_tool_for_goal(tool, input, &routing.routing_stop_terms);
    for rule in &routing.intent_rules {
        let intent_score =
            best_phrase_match_score(input, &rule.hints, &routing.routing_stop_terms).max(
                best_phrase_match_score(input, &rule.examples, &routing.routing_stop_terms),
            );
        let negative_score =
            best_phrase_match_score(input, &rule.negative_examples, &routing.routing_stop_terms);
        if intent_score > 0 && tool_matches_any_selector(tool, &rule.preferred_selectors) {
            score += rule.weight * intent_score / 100;
        }
        if negative_score > 0 && tool_matches_any_selector(tool, &rule.preferred_selectors) {
            score -= rule.weight.abs() * negative_score / 100;
        }
    }
    for rule in &routing.tool_weights {
        if tool_matches_any_selector(tool, &rule.selectors) {
            score += rule.weight;
        }
    }
    score
}

fn route_arguments_for_tool(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    request: &ChatRequest,
    tool: &ToolSpec,
    routing: &ToolRoutingTags,
    input: &str,
) -> BTreeMap<String, Value> {
    let mut args = BTreeMap::new();
    for rule in &routing.tool_argument_rules {
        if !tool_matches_any_selector(tool, &rule.selectors) {
            continue;
        }
        args.extend(rule.args.clone());
        if rule.include_matched_argument_rules {
            for argument_rule in &routing.argument_rules {
                if text_matches_any(input, &argument_rule.hints) {
                    args.extend(argument_rule.args.clone());
                }
            }
        }
        for context_call in &rule.context_calls {
            if let Some(context) = fetch_tool_context(config, namespace, request, context_call) {
                args.insert(context_call.arg.clone(), context);
            }
        }
    }
    args
}

fn fetch_tool_context(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    request: &ChatRequest,
    rule: &ToolContextCallRule,
) -> Option<Value> {
    for tool_name in &rule.tool_names {
        let invocation = mcp_invocation_plan(
            format!("mcp.context.{}.{}", rule.server_id, tool_name),
            format!("context fetch for {}", rule.arg),
            vec![
                "hc.mcp.call".to_owned(),
                rule.server_id.clone(),
                tool_name.clone(),
            ],
            namespace.clone(),
            request.session_id.clone(),
            rule.server_id.clone(),
            tool_name.clone(),
            Map::new(),
        );
        let outcome = execute_tool_invocation(config, &invocation).ok()?;
        if outcome.success {
            return outcome.raw_result.clone();
        }
    }
    None
}

fn render_tool_result(
    rendering: &ToolResponseRenderingConfig,
    tool: &ToolSpec,
    result: &Value,
) -> Option<String> {
    if is_mcp_error(result) {
        return None;
    }
    let renderer = rendering
        .renderers
        .iter()
        .find(|renderer| tool_matches_any_selector(tool, &renderer.selectors))?;
    if renderer.kind == "order" {
        return render_order_result(result, renderer);
    }
    if renderer.kind != "ranked_items" {
        return None;
    }
    let items = extract_ranked_items(result, renderer);
    if items.is_empty() {
        return None;
    }
    let has_context = renderer
        .context_arg
        .as_deref()
        .is_some_and(|arg| result.to_string().contains(arg));
    let mut lines = vec![if has_context {
        renderer
            .header_with_context
            .as_deref()
            .or(renderer.header.as_deref())
            .unwrap_or("I found these available options:")
            .to_owned()
    } else {
        renderer
            .header
            .as_deref()
            .unwrap_or("I found these available options:")
            .to_owned()
    }];
    let mut items = items.into_iter().take(3);
    if let Some(primary) = items.next() {
        if let Some(heading) = &renderer.primary_heading {
            lines.push(heading.clone());
        }
        lines.extend(render_primary_item(primary, renderer));
    }
    let alternatives = items.collect::<Vec<_>>();
    if !alternatives.is_empty() {
        if let Some(heading) = &renderer.alternatives_heading {
            lines.push(heading.clone());
        }
        for (index, item) in alternatives.into_iter().enumerate() {
            lines.push(render_alternative_item(index + 2, item, renderer));
        }
    }
    if let Some(prompt) = &renderer.confirmation_prompt {
        lines.push(prompt.clone());
    }
    Some(lines.join("\n"))
}

fn render_order_result(result: &Value, renderer: &ToolResponseRenderer) -> Option<String> {
    let mut lines = vec![
        renderer
            .order_success
            .as_deref()
            .unwrap_or("The order has been submitted.")
            .to_owned(),
    ];
    for field in &renderer.primary_fields {
        if let Some(value) = render_field_with_renderer(result, field, Some(renderer)) {
            lines.push(format!("{}: {}", field.label, value));
        }
    }
    Some(lines.join("\n"))
}

fn render_primary_item(item: &Value, renderer: &ToolResponseRenderer) -> Vec<String> {
    let mut lines = vec![format!("1. {}", item_name(item, renderer))];
    for field in &renderer.primary_fields {
        if let Some(value) = render_field_with_renderer(item, field, Some(renderer)) {
            lines.push(format!("   {}: {}", field.label, value));
        }
    }
    if let Some(reason) = item_reason(item, renderer) {
        lines.push(format!("   推荐理由: {reason}"));
    }
    lines
}

fn render_alternative_item(index: usize, item: &Value, renderer: &ToolResponseRenderer) -> String {
    let details = renderer
        .alternative_fields
        .iter()
        .filter_map(|field| render_field_with_renderer(item, field, Some(renderer)))
        .collect::<Vec<_>>();
    let suffix = if details.is_empty() {
        String::new()
    } else {
        format!(" - {}", details.join("; "))
    };
    format!("{index}. {}{suffix}", item_name(item, renderer))
}

fn confirmation_fallback_tool_ids(
    tools: &[ToolSpec],
    flow: &ToolConfirmationFlowRule,
    primary_tool_id: &str,
) -> Vec<String> {
    let mut ids = Vec::new();
    for selectors in &flow.fallback_target_selectors {
        if selectors.is_empty() {
            continue;
        }
        if let Some(tool) = tools
            .iter()
            .find(|tool| tool_matches_all_selectors(tool, selectors))
            && tool.id != primary_tool_id
            && !ids.iter().any(|id| id == &tool.id)
        {
            ids.push(tool.id.clone());
        }
    }
    ids
}

fn extract_items_for_paths<'a>(value: &'a Value, paths: &[String]) -> Vec<&'a Value> {
    if let Some(array) = value.as_array() {
        return array.iter().filter(|item| item.is_object()).collect();
    }
    for path in paths {
        if let Some(array) = value_for_key(value, path).and_then(Value::as_array) {
            return array.iter().filter(|item| item.is_object()).collect();
        }
    }
    Vec::new()
}

fn render_item_arg_mapping(value: &Value, mapping: &ToolItemArgMapping) -> Option<Value> {
    match mapping.format.as_deref() {
        Some("single_menu_item") => value
            .as_i64()
            .map(|menu_id| serde_json::json!([{ "menu_id": menu_id, "quantity": 1 }])),
        _ => Some(value.clone()),
    }
}

fn item_name(item: &Value, renderer: &ToolResponseRenderer) -> String {
    value_for_keys(item, &renderer.name_keys)
        .and_then(display_json_value)
        .unwrap_or_else(|| "Unnamed".to_owned())
}

fn item_reason(item: &Value, renderer: &ToolResponseRenderer) -> Option<String> {
    for key in &renderer.reason_array_keys {
        if let Some(values) = value_for_key(item, key).and_then(Value::as_array) {
            let rendered = values
                .iter()
                .filter_map(Value::as_str)
                .take(2)
                .map(|value| compact(value, 80))
                .collect::<Vec<_>>()
                .join("; ");
            if !rendered.trim().is_empty() {
                return Some(rendered);
            }
        }
    }
    for key in &renderer.reason_keys {
        if let Some(value) = value_for_key(item, key).and_then(Value::as_str) {
            let value = compact(value, 110);
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn render_field_with_renderer(
    item: &Value,
    field: &ToolResponseField,
    renderer: Option<&ToolResponseRenderer>,
) -> Option<String> {
    let value = value_for_keys(item, &field.keys)?;
    let rendered = match field.format.as_deref() {
        Some("cents_cny") => value
            .as_i64()
            .map(|cents| format!("约 {:.2} 元", cents as f64 / 100.0))
            .or_else(|| {
                value
                    .as_f64()
                    .map(|cents| format!("约 {:.2} 元", cents / 100.0))
            }),
        Some("yuan_cny") => value
            .as_f64()
            .map(|yuan| format!("约 {:.2} 元", yuan))
            .or_else(|| {
                value
                    .as_i64()
                    .map(|yuan| format!("约 {:.2} 元", yuan as f64))
            }),
        Some("status_label") => value.as_str().map(|status| {
            renderer
                .and_then(|renderer| renderer.status_labels.get(status).cloned())
                .unwrap_or_else(|| status.to_owned())
        }),
        _ => display_json_value(value),
    }?;
    Some(compact(&rendered, field.max_len.unwrap_or(120)))
}

fn extract_ranked_items<'a>(value: &'a Value, renderer: &ToolResponseRenderer) -> Vec<&'a Value> {
    if let Some(array) = value.as_array() {
        return array.iter().filter(|item| item.is_object()).collect();
    }
    for key in &renderer.item_paths {
        if let Some(array) = value_for_key(value, key).and_then(Value::as_array) {
            return array.iter().filter(|item| item.is_object()).collect();
        }
    }
    Vec::new()
}

fn value_for_keys<'a>(value: &'a Value, keys: &[String]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value_for_key(value, key))
}

fn value_for_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    if key == "." {
        return Some(value);
    }
    if key.starts_with('/') {
        return value.pointer(key);
    }
    let mut current = value;
    for segment in key.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn display_json_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn is_mcp_error(value: &Value) -> bool {
    value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn tool_matches_any_selector(tool: &ToolSpec, selectors: &[String]) -> bool {
    selectors
        .iter()
        .any(|selector| tool_matches_selector(tool, selector))
}

fn tool_matches_all_selectors(tool: &ToolSpec, selectors: &[String]) -> bool {
    !selectors.is_empty()
        && selectors
            .iter()
            .all(|selector| tool_matches_selector(tool, selector))
}

fn tool_matches_selector(tool: &ToolSpec, selector: &str) -> bool {
    let selector = selector.trim();
    !selector.is_empty()
        && (contains_ci(&tool.id, selector)
            || contains_ci(&tool.name, selector)
            || contains_ci(&tool.description, selector)
            || tool.tags.iter().any(|tag| contains_ci(tag, selector))
            || tool
                .default_command
                .iter()
                .any(|part| contains_ci(part, selector)))
}

fn text_matches_any(text: &str, selectors: &[String]) -> bool {
    selectors
        .iter()
        .any(|selector| phrase_match_score_with_stop_terms(text, selector, &[]) > 0)
}

fn best_phrase_match_score(text: &str, phrases: &[String], stop_terms: &[String]) -> i32 {
    phrases
        .iter()
        .map(|phrase| phrase_match_score_with_stop_terms(text, phrase, stop_terms))
        .max()
        .unwrap_or(0)
}

fn score_tool_for_goal(tool: &ToolSpec, goal: &str, stop_terms: &[String]) -> i32 {
    let mut score = 0;
    score += phrase_match_score_with_stop_terms(goal, &tool.id, stop_terms) / 25;
    score += phrase_match_score_with_stop_terms(goal, &tool.name, stop_terms) / 20;
    score += phrase_match_score_with_stop_terms(goal, &tool.description, stop_terms) / 35;
    score += tool
        .tags
        .iter()
        .map(|tag| phrase_match_score_with_stop_terms(goal, tag, stop_terms) / 35)
        .sum::<i32>();
    for token in goal_match_terms(goal) {
        if token.is_empty() {
            continue;
        }
        let token_lowered = token.to_ascii_lowercase();
        if tool.id.to_ascii_lowercase().contains(&token_lowered) {
            score += 4;
        }
        if tool.name.to_ascii_lowercase().contains(&token_lowered) || tool.name.contains(&token) {
            score += 3;
        }
        if tool
            .description
            .to_ascii_lowercase()
            .contains(&token_lowered)
            || tool.description.contains(&token)
        {
            score += 2;
        }
        if tool
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(&token_lowered) || tag.contains(&token))
        {
            score += 2;
        }
    }
    score
}

fn goal_match_terms(goal: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lowered = goal.to_ascii_lowercase();
    for token in lowered.split(|ch: char| !ch.is_alphanumeric()) {
        if !token.is_empty() {
            terms.push(token.to_owned());
        }
    }

    let cjk_runs = goal
        .split(|ch: char| ch.is_ascii() || ch.is_whitespace() || ch.is_ascii_punctuation())
        .filter(|part| !part.is_empty());
    for run in cjk_runs {
        terms.push(run.to_owned());
        let chars: Vec<char> = run.chars().collect();
        for window in chars.windows(2) {
            terms.push(window.iter().collect());
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn contains_ci(text: &str, needle: &str) -> bool {
    text.contains(needle)
        || text
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
}

fn compact(value: &str, max_len: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= max_len {
        return value;
    }
    value
        .chars()
        .take(max_len.saturating_sub(1))
        .collect::<String>()
        + "…"
}

pub(crate) fn load_tool_routing_tags(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
) -> Result<ToolRoutingTags> {
    let mut tags: ToolRoutingTags =
        load_frontmatter(config, namespace, "routing/tool-routing-tags.md")?;
    tags.ensure_builtin_timed_sequences();
    Ok(tags)
}

fn load_tool_response_rendering(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
) -> Result<ToolResponseRenderingConfig> {
    load_frontmatter(config, namespace, "rendering/tool-response-rendering.md")
}

fn load_frontmatter<T: for<'de> Deserialize<'de>>(
    config: &ServiceConfig,
    namespace: &ApiNamespace,
    relative: &str,
) -> Result<T> {
    let path = config
        .workspace_root
        .join("tenants")
        .join(&namespace.tenant_id)
        .join("users")
        .join(&namespace.user_id)
        .join(relative);
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let frontmatter = markdown_frontmatter(&content)
        .with_context(|| format!("missing frontmatter in {}", path.display()))?;
    serde_yaml::from_str(frontmatter).with_context(|| format!("failed to parse {}", path.display()))
}

fn markdown_frontmatter(content: &str) -> Option<&str> {
    let content = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;
    let (frontmatter, _) = content
        .split_once("\r\n---")
        .or_else(|| content.split_once("\n---"))?;
    Some(frontmatter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hc_capability::ModelDependence;
    use hc_toolchain::{ToolComposition, ToolExecutionKind, ToolStability};

    #[test]
    fn lunch_recommendation_prefers_food_combo_over_sms() {
        let routing = ToolRoutingTags {
            intent_rules: vec![ToolIntentRoutingRule {
                hints: vec!["推荐".to_owned(), "吃什么".to_owned()],
                preferred_selectors: vec!["recommend-combo".to_owned(), "recommend".to_owned()],
                weight: 10,
                ..ToolIntentRoutingRule::default()
            }],
            tool_weights: vec![ToolWeightRule {
                selectors: vec!["recommend-combo".to_owned()],
                weight: 8,
            }],
            ..ToolRoutingTags::default()
        };
        let combo = tool(
            "tool.mcp.careos-food-delivery.recommend-combo",
            "recommend_combo",
        );
        let sms = tool("tool.mcp.careos-phone-sms.send-sms", "send_sms");

        assert!(
            score_tool_for_input(&combo, &routing, "中午推荐我吃什么")
                > score_tool_for_input(&sms, &routing, "中午推荐我吃什么")
        );
    }

    fn tool(id: &str, tool_name: &str) -> ToolSpec {
        ToolSpec {
            id: id.to_owned(),
            name: tool_name.to_owned(),
            description: tool_name.to_owned(),
            execution_kind: ToolExecutionKind::Service,
            composition: ToolComposition::Atomic,
            stability: ToolStability::Managed,
            model_dependence: ModelDependence::Optional,
            default_command: vec![
                "hc.mcp.call".to_owned(),
                "mcp.demo".to_owned(),
                tool_name.to_owned(),
            ],
            tags: vec![tool_name.replace('_', "-")],
        }
    }
}
