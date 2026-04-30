use std::{env, fs, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use hc_protocol::{
    AgentListResponse, AgentRouteRequest, AgentRouteResponse, ApiNamespace, ChatRequest,
    ChatResponse, DomainListResponse, ErrorResponse, HealthResponse, McpServerListResponse,
};
use hc_service::{
    ServiceConfig,
    agent::{list_agents, list_domains, route_agent},
    chat::handle_chat_request,
    tool::list_mcp_servers,
};
use serde::Deserialize;
use serde_json::{Value, json};

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
}

#[tokio::main]
async fn main() -> Result<()> {
    load_local_env_file()?;
    let state = AppState {
        service: ServiceConfig::new(workspace_root()),
    };
    let bind_addr = bind_addr()?;
    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/swagger-ui", get(swagger_ui))
        .route("/swagger-ui/", get(swagger_ui))
        .route("/v1/agents", get(agents))
        .route("/v1/domains", get(domains))
        .route("/v1/mcp/servers", get(mcp_servers))
        .route("/v1/agents/route", post(agent_route))
        .route("/v1/chat", post(chat))
        .with_state(state);

    println!("hc-api listening on http://{bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;
    axum::serve(listener, app)
        .await
        .context("api server failed")
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
        tokio::task::spawn_blocking(move || handle_chat_request(&state.service, request))
            .await
            .map_err(|error| ApiError(anyhow!("chat worker failed: {error}")))?
            .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn agents(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<AgentListResponse>, ApiError> {
    let namespace = ApiNamespace {
        tenant_id: query.tenant_id,
        user_id: query.user_id,
    };
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
    let namespace = ApiNamespace {
        tenant_id: query.tenant_id,
        user_id: query.user_id,
    };
    let response = tokio::task::spawn_blocking(move || list_domains(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("domain worker failed: {error}")))?
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

async fn mcp_servers(
    State(state): State<AppState>,
    Query(query): Query<NamespaceQuery>,
) -> Result<Json<McpServerListResponse>, ApiError> {
    let namespace = ApiNamespace {
        tenant_id: query.tenant_id,
        user_id: query.user_id,
    };
    let response = tokio::task::spawn_blocking(move || list_mcp_servers(&state.service, namespace))
        .await
        .map_err(|error| ApiError(anyhow!("mcp server worker failed: {error}")))?
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
