use std::{env, fs, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use hc_context::{
    ContextMemoryQuery, ContextRequest, DefaultContextComposer, MemoryKind, MemoryNamespace,
    MemoryScope, PromptPolicy, WorkspaceMemoryRetriever, generate_with_context,
    load_context_memory_system_prompt, load_context_memory_usage_policy_prompt, memory_kind_label,
    memory_scope_label, workspace_namespace_from_memory_namespace,
};
use hc_llm::{
    ChatMessage, GenerateRequest, MessageRole, ModelRef, default_model_from_env,
    default_provider_from_env, default_registry_from_env,
};
use hc_protocol::{
    ApiChatMessage, ApiMemoryQuery, ApiMessageRole, ApiNamespace, ChatRequest, ChatResponse,
    ErrorResponse, HealthResponse, MemoryRef,
};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct AppState {
    workspace_root: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    load_local_env_file()?;
    let state = AppState {
        workspace_root: workspace_root(),
    };
    let bind_addr = bind_addr()?;
    let app = Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi))
        .route("/swagger-ui", get(swagger_ui))
        .route("/swagger-ui/", get(swagger_ui))
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
    let response = tokio::task::spawn_blocking(move || handle_chat_request(&state, request))
        .await
        .map_err(|error| ApiError(anyhow!("chat worker failed: {error}")))?
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

fn handle_chat_request(state: &AppState, request: ChatRequest) -> Result<ChatResponse> {
    let memory_namespace = memory_namespace_from_api(&request.memory.namespace);
    let workspace_namespace = workspace_namespace_from_memory_namespace(&memory_namespace);
    let model = ModelRef::new(
        request
            .provider
            .clone()
            .unwrap_or_else(default_provider_from_env),
        request.model.clone().unwrap_or_else(default_model_from_env),
    );
    let messages = request_messages(&request)?;
    let mut generation = GenerateRequest::new(model.clone(), messages);
    generation.temperature = request.temperature;
    generation.max_output_tokens = request.max_output_tokens;

    let memory_query =
        build_memory_query(memory_namespace, &request.memory, request.input.clone())?;
    let system_prompt = match request.system_prompt {
        Some(system_prompt) if !system_prompt.trim().is_empty() => system_prompt,
        _ => load_context_memory_system_prompt(&workspace_namespace)?,
    };
    let context_request = ContextRequest::new(generation)
        .with_memory_query(memory_query)
        .with_system_prompt(system_prompt)
        .with_prompt_policy(PromptPolicy::new(
            "Memory Usage Policy",
            load_context_memory_usage_policy_prompt(&workspace_namespace)?,
        ));

    let registry = default_registry_from_env();
    let retriever = WorkspaceMemoryRetriever::new(&state.workspace_root, workspace_namespace);
    let composer = DefaultContextComposer;
    let response = generate_with_context(&registry, &retriever, &composer, &context_request)?;

    Ok(ChatResponse {
        message: api_message_from_llm(response.response.message),
        model: response.response.model.model,
        provider: response.response.model.provider,
        recalled_memories: response
            .recalled_memories
            .into_iter()
            .map(memory_ref_from_retrieved)
            .collect(),
        synthesized_prompt_asset_count: response.synthesized_prompt_assets.len(),
    })
}

fn request_messages(request: &ChatRequest) -> Result<Vec<ChatMessage>> {
    let mut messages = request
        .messages
        .iter()
        .map(llm_message_from_api)
        .collect::<Vec<_>>();
    if let Some(input) = request.input.as_deref()
        && !input.trim().is_empty()
    {
        messages.push(ChatMessage::new(MessageRole::User, input.trim().to_owned()));
    }
    if messages.is_empty() {
        bail!("chat request requires input or messages");
    }
    Ok(messages)
}

fn build_memory_query(
    namespace: MemoryNamespace,
    memory: &ApiMemoryQuery,
    fallback_text: Option<String>,
) -> Result<ContextMemoryQuery> {
    let mut query = ContextMemoryQuery::default().for_namespace(namespace);
    let text = memory
        .text
        .clone()
        .or(fallback_text)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(text) = text {
        query = query.with_text(text);
    }
    query = query.with_limit(memory.limit.unwrap_or(8).max(1));
    if let Some(scope) = memory.scope.as_deref() {
        query = query.with_scope(parse_scope(scope)?);
    }
    if let Some(kind) = memory.kind.as_deref() {
        query.memory_query.kind = Some(parse_kind(kind)?);
    }
    if let Some(tag) = memory.tag.as_deref()
        && !tag.trim().is_empty()
    {
        query = query.with_tag(tag.trim().to_owned());
    }
    Ok(query)
}

fn llm_message_from_api(message: &ApiChatMessage) -> ChatMessage {
    let role = match message.role {
        ApiMessageRole::System => MessageRole::System,
        ApiMessageRole::User => MessageRole::User,
        ApiMessageRole::Assistant => MessageRole::Assistant,
        ApiMessageRole::Tool => MessageRole::Tool,
    };
    let mut chat_message = ChatMessage::new(role, message.content.clone());
    if let Some(name) = &message.name {
        chat_message = chat_message.named(name.clone());
    }
    chat_message
}

fn api_message_from_llm(message: ChatMessage) -> ApiChatMessage {
    let role = match message.role {
        MessageRole::System => ApiMessageRole::System,
        MessageRole::User => ApiMessageRole::User,
        MessageRole::Assistant => ApiMessageRole::Assistant,
        MessageRole::Tool => ApiMessageRole::Tool,
    };
    ApiChatMessage {
        role,
        content: message.content,
        name: message.name,
    }
}

fn memory_ref_from_retrieved(memory: hc_context::RetrievedMemory) -> MemoryRef {
    MemoryRef {
        id: memory.id,
        title: memory.title,
        summary: memory.summary,
        scope: memory_scope_label(&memory.scope).to_owned(),
        kind: memory_kind_label(&memory.kind).to_owned(),
        source_kind: memory.source_kind,
        confidence_milli: memory.confidence_milli,
        tags: memory.tags,
        room_id: memory.room_id,
    }
}

fn memory_namespace_from_api(namespace: &ApiNamespace) -> MemoryNamespace {
    MemoryNamespace::new(namespace.tenant_id.clone(), namespace.user_id.clone())
}

fn parse_scope(value: &str) -> Result<MemoryScope> {
    match value.trim().to_ascii_lowercase().as_str() {
        "global" => Ok(MemoryScope::Global),
        "persona" => Ok(MemoryScope::Persona),
        "session" => Ok(MemoryScope::Session),
        "instance" => Ok(MemoryScope::Instance),
        "project" => Ok(MemoryScope::Project),
        "task" => Ok(MemoryScope::Task),
        other => bail!("unsupported memory scope: {other}"),
    }
}

fn parse_kind(value: &str) -> Result<MemoryKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "summary" => Ok(MemoryKind::Summary),
        "decision" => Ok(MemoryKind::Decision),
        "preference" => Ok(MemoryKind::Preference),
        "knowledge" => Ok(MemoryKind::Knowledge),
        "workflow_memory" | "workflow-memory" => Ok(MemoryKind::WorkflowMemory),
        other => bail!("unsupported memory kind: {other}"),
    }
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
