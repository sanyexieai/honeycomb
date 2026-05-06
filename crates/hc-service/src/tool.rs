use anyhow::{Context, Result};
use hc_context::runtime::{RuntimeIdentity, RuntimeVariableRepository};
use hc_protocol::{ApiNamespace, McpServerListResponse, McpServerSummary};
use hc_store::store::WorkspaceNamespace;
use hc_toolchain::{McpServerRepository, ToolRepository, ToolSpec, call_mcp_tool};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ServiceConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolListRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub refresh: bool,
    #[serde(default)]
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolListResponse {
    pub tools: Vec<McpToolSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolSummary {
    pub server_id: String,
    pub refreshed_at_unix: Option<u64>,
    pub tool: ToolSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub server_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolCallResponse {
    pub server_id: String,
    pub tool_name: String,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolListResponse {
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolWriteRequest {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    pub tool: ToolSpec,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolWriteResponse {
    pub tool: ToolSpec,
    pub path: String,
}

pub fn list_mcp_servers(
    config: &ServiceConfig,
    namespace: ApiNamespace,
) -> Result<McpServerListResponse> {
    let repository = McpServerRepository::with_namespace(
        config.workspace_root.clone(),
        WorkspaceNamespace::new(namespace.tenant_id, namespace.user_id),
    );
    let servers = repository
        .list_servers()?
        .into_iter()
        .map(|server| McpServerSummary {
            id: server.id,
            name: server.name,
            description: server.description,
            enabled: server.enabled,
            transport: format!("{:?}", server.transport),
            url: server.url,
            command: server.command,
            tags: server.tags,
        })
        .collect();
    Ok(McpServerListResponse { servers })
}

pub fn list_tools(config: &ServiceConfig, namespace: ApiNamespace) -> Result<ToolListResponse> {
    let repository = ToolRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace(&namespace),
    );
    Ok(ToolListResponse {
        tools: repository.list_tools()?,
    })
}

pub fn write_tool(config: &ServiceConfig, request: ToolWriteRequest) -> Result<ToolWriteResponse> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let repository = ToolRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace(&namespace),
    );
    let path = repository.write_tool(&request.tool)?;
    Ok(ToolWriteResponse {
        tool: request.tool,
        path: path.to_string_lossy().replace('\\', "/"),
    })
}

pub fn list_mcp_tools(
    config: &ServiceConfig,
    request: McpToolListRequest,
) -> Result<McpToolListResponse> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let repository = McpServerRepository::with_namespace(
        config.workspace_root.clone(),
        workspace_namespace(&namespace),
    );
    let servers = repository.list_servers()?;
    let mut tools = Vec::new();
    for server in servers {
        if !server.enabled {
            continue;
        }
        if let Some(server_id) = request.server_id.as_deref()
            && server.id != server_id
        {
            continue;
        }
        let cache_result = if request.refresh {
            repository.refresh_tool_cache(&server)
        } else {
            match repository.read_tool_cache(&server.id) {
                Ok(cache) => Ok(cache),
                Err(read_error) => match repository.refresh_tool_cache(&server) {
                    Ok(cache) => Ok(cache),
                    Err(refresh_error) => {
                        let _ = repository.quarantine_tool_cache(
                            &server.id,
                            &format!(
                                "cache read failed ({read_error}); refresh failed ({refresh_error})"
                            ),
                        );
                        Err(refresh_error)
                    }
                },
            }
        };
        let cache = match cache_result {
            Ok(cache) => cache,
            Err(error) => {
                if request.server_id.is_some() {
                    return Err(error)
                        .with_context(|| format!("failed to load MCP tools for {}", server.id));
                }
                continue;
            }
        };
        tools.extend(cache.tools.into_iter().map(|tool| McpToolSummary {
            server_id: cache.server_id.clone(),
            refreshed_at_unix: Some(cache.refreshed_at_unix),
            tool,
        }));
    }
    tools.sort_by(|left, right| {
        left.server_id
            .cmp(&right.server_id)
            .then_with(|| left.tool.id.cmp(&right.tool.id))
    });
    Ok(McpToolListResponse { tools })
}

pub fn call_configured_mcp_tool(
    config: &ServiceConfig,
    request: McpToolCallRequest,
) -> Result<McpToolCallResponse> {
    let namespace = normalized_namespace(request.namespace, request.tenant_id, request.user_id);
    let workspace_namespace = workspace_namespace(&namespace);
    let repository =
        McpServerRepository::with_namespace(config.workspace_root.clone(), workspace_namespace);
    let server = repository.get_server(&request.server_id)?;
    let mut arguments = Map::new();
    for (key, value) in &server.default_args {
        arguments.insert(key.clone(), value.clone());
    }
    for (key, value) in request.arguments {
        arguments.insert(key, value);
    }
    let runtime_identity = RuntimeIdentity::from_optional(
        Some(namespace.tenant_id.clone()),
        Some(namespace.user_id.clone()),
        request.session_id,
    );
    let runtime = RuntimeVariableRepository::new(&config.workspace_root)
        .load(runtime_identity.clone(), None)
        .unwrap_or_else(|_| hc_context::runtime::RuntimeVariables::new(runtime_identity));
    runtime.inject_mcp_arguments(&mut arguments);
    let result = call_mcp_tool(&server, &request.tool_name, Value::Object(arguments))
        .with_context(|| format!("mcp tool call failed: {}/{}", server.id, request.tool_name))?;
    Ok(McpToolCallResponse {
        server_id: server.id,
        tool_name: request.tool_name,
        result,
    })
}

fn normalized_namespace(
    mut namespace: ApiNamespace,
    tenant_id: Option<String>,
    user_id: Option<String>,
) -> ApiNamespace {
    if let Some(tenant_id) = tenant_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        namespace.tenant_id = tenant_id;
    }
    if let Some(user_id) = user_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        namespace.user_id = user_id;
    }
    if namespace.tenant_id.trim().is_empty() {
        namespace.tenant_id = hc_context::runtime::DEFAULT_TENANT_ID.to_owned();
    }
    if namespace.user_id.trim().is_empty() {
        namespace.user_id = hc_context::runtime::DEFAULT_USER_ID.to_owned();
    }
    namespace
}

fn workspace_namespace(namespace: &ApiNamespace) -> WorkspaceNamespace {
    WorkspaceNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}
