//! OpenAPI 规范与 Swagger UI 壳页（`run/openapi/` 目录，与 HTTP 契约对齐）。

use serde_json::{Value, json};

pub(crate) fn openapi_document() -> Value {
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
            "/v1/messages": {
                "post": {
                    "summary": "Generate a chat response from a minimal user-message body",
                    "operationId": "postMessages",
                    "description": "Same JSON response as `POST /v1/chat` after mapping `UserMessageBody` into a full `ChatRequest`. Prefer this for lightweight clients; use `POST /v1/messages/stream` for SSE.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/UserMessageBody" }
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
                    "description": "Streams chat lifecycle events and model deltas. SSE `event` names include `chat.room_capabilities` (optional, emitted first when `ChatRequest.room_id` is set; `data` matches `#/components/schemas/RoomCapabilitiesStreamData`), `turn.started`, `turn.tool`, `turn.completed`, `chat.started`, `chat.delta`, `chat.completed`, and `chat.error`.",
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
            "/v1/messages/stream": {
                "post": {
                    "summary": "Stream chat from a minimal user-message body",
                    "operationId": "streamMessages",
                    "description": "Same SSE `event` / `data` sequence as `POST /v1/chat/stream` after mapping this body into a full `ChatRequest`. Supports optional `messages`, `memory`, `room_id`, behavior knobs, and LLM fields. Use `/v1/chat/stream` only when you need request fields not mirrored here.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/UserMessageBody" }
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
                    "description": "Open WebSocket upgrade on GET. After the handshake, send one text frame containing a ChatRequest JSON body; the server streams JSON event messages with the same payload shape as SSE /v1/chat/stream (fields `event`, `id`, `data`), including optional leading `chat.room_capabilities` when `room_id` is set. OpenAPI cannot fully describe WebSocket frames; use this entry as a capability marker."
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
            "/v1/schedules/followup-fired-events": {
                "get": {
                    "summary": "List timed follow-up fire events for replay recovery",
                    "operationId": "listFollowupFiredEvents",
                    "parameters": [
                        { "name": "tenant_id", "in": "query", "required": false, "schema": { "type": "string", "default": "local" } },
                        { "name": "user_id", "in": "query", "required": false, "schema": { "type": "string", "default": "default" } },
                        { "name": "since_created_at_unix", "in": "query", "required": false, "schema": { "type": "integer", "format": "int64" } }
                    ],
                    "responses": {
                        "200": { "description": "Fired replay rows", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/operational-stats": {
                "get": {
                    "summary": "Operational snapshot for schedules and follow-ups",
                    "operationId": "schedulesOperationalStats",
                    "parameters": [
                        { "name": "tenant_id", "in": "query", "required": false, "schema": { "type": "string", "default": "local" } },
                        { "name": "user_id", "in": "query", "required": false, "schema": { "type": "string", "default": "default" } },
                        { "name": "now_unix", "in": "query", "required": false, "schema": { "type": "integer", "format": "int64" } }
                    ],
                    "responses": {
                        "200": {
                            "description": "Scheduler operational snapshot (JSON). Disk-backed counters are always present. hc-api process fields (api_dispatch_*, api_followup_messages_delivered_total, legacy api_followup_headless_messages_delivered_total same value, histograms, etc.) are omitted when zero. CLI `hc-cli schedule stats` omits api_* entirely. Webhook follow-up delivery is configured with HC_SCHEDULER_FOLLOWUP_DELIVERY_MODE=webhook, HC_SCHEDULER_FOLLOWUP_WEBHOOK_URL, optional HC_SCHEDULER_FOLLOWUP_WEBHOOK_BEARER_TOKEN, optional HC_SCHEDULER_FOLLOWUP_WEBHOOK_TIMEOUT_SECS (default 30, clamped 1–300); see docs/scheduled-tasks.md.",
                            "content": { "application/json": { "schema": { "type": "object" } } }
                        },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/schedules/metrics/prometheus": {
                "get": {
                    "summary": "Operational stats as Prometheus / OpenMetrics text (gauges, tenant scoped)",
                    "operationId": "schedulesOperationalStatsPrometheus",
                    "parameters": [
                        { "name": "tenant_id", "in": "query", "required": false, "schema": { "type": "string", "default": "local" } },
                        { "name": "user_id", "in": "query", "required": false, "schema": { "type": "string", "default": "default" } },
                        { "name": "now_unix", "in": "query", "required": false, "schema": { "type": "integer", "format": "int64" } }
                    ],
                    "responses": {
                        "200": { "description": "OpenMetrics exposition", "content": { "text/plain": { "schema": { "type": "string" } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/human-inbox": {
                "get": {
                    "summary": "List pending human responder inbox items",
                    "operationId": "listHumanInbox",
                    "parameters": [
                        { "name": "tenant_id", "in": "query", "required": false, "schema": { "type": "string", "default": "local" } },
                        { "name": "user_id", "in": "query", "required": false, "schema": { "type": "string", "default": "default" } }
                    ],
                    "responses": {
                        "200": { "description": "Pending inbox items", "content": { "application/json": { "schema": { "type": "array", "items": { "type": "object" } } } } },
                        "500": { "$ref": "#/components/responses/InternalError" }
                    }
                }
            },
            "/v1/human-inbox/complete": {
                "post": {
                    "summary": "Complete a pending human inbox item",
                    "operationId": "completeHumanInboxItem",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["item_id", "response_body"], "properties": { "tenant_id": { "type": "string" }, "user_id": { "type": "string" }, "item_id": { "type": "string" }, "response_body": { "type": "string" } } } } } },
                    "responses": {
                        "200": { "description": "Result with storage path", "content": { "application/json": { "schema": { "type": "object" } } } },
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
                        "behavior_pattern": {
                            "type": "string",
                            "description": "Optional behavior pattern name; see `GET /v1/behavior/patterns`."
                        },
                        "thinking_depth": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 255,
                            "description": "Optional thinking-depth hint for the behavior engine."
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
                        "active_work_item_id": {
                            "type": "string",
                            "description": "Optional `work-item.*` id; scopes HTTP L2/L3 degenerate coordination when multiple planner work items are open."
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
                "UserMessageBody": {
                    "type": "object",
                    "description": "Request body for `POST /v1/messages` (JSON) and `POST /v1/messages/stream` (SSE); maps to `ChatRequest` server-side.",
                    "required": ["text"],
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "User message for this turn (`ChatRequest.input`)."
                        },
                        "messages": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/ApiChatMessage" },
                            "description": "Optional preceding turns (`ChatRequest.messages`). Combined with `input` per the same rules as full `ChatRequest`."
                        },
                        "memory": {
                            "$ref": "#/components/schemas/ApiMemoryQuery"
                        },
                        "tenant_id": {
                            "type": "string",
                            "description": "Optional tenant id."
                        },
                        "user_id": {
                            "type": "string",
                            "description": "Optional user id."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session / conversation id (`ChatRequest.session_id`)."
                        },
                        "room_id": {
                            "type": "string",
                            "description": "Optional memory room id; same semantics as `ChatRequest.room_id` (capabilities, routing context, leading `chat.room_capabilities` SSE when configured)."
                        },
                        "behavior_pattern": {
                            "type": "string",
                            "description": "Optional behavior pattern name (`ChatRequest.behavior_pattern`); see `GET /v1/behavior/patterns`."
                        },
                        "thinking_depth": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 255,
                            "description": "Optional thinking-depth override (`ChatRequest.thinking_depth`)."
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Explicit agent profile id when routing should skip discovery."
                        },
                        "domain_id": {
                            "type": "string",
                            "description": "Optional domain routing hint."
                        },
                        "active_agent_id": {
                            "type": "string",
                            "description": "Agent instance id from active task/session context (`ChatRequest.active_agent_id`; distinct from routing `agent_id`)."
                        },
                        "active_task_id": {
                            "type": "string",
                            "description": "Optional active task id for swarm / task-scoped memory defaults."
                        },
                        "active_work_item_id": {
                            "type": "string",
                            "description": "Optional `work-item.*` id; scopes HTTP L2/L3 degenerate coordination when multiple planner work items are open."
                        },
                        "provider": {
                            "type": "string",
                            "description": "LLM provider id (`ChatRequest.provider`)."
                        },
                        "model": {
                            "type": "string",
                            "description": "Model name (`ChatRequest.model`)."
                        },
                        "system_prompt": {
                            "type": "string",
                            "description": "Overrides default system prompt for this turn (`ChatRequest.system_prompt`)."
                        },
                        "temperature": {
                            "type": "number",
                            "format": "float",
                            "description": "Sampling temperature (`ChatRequest.temperature`)."
                        },
                        "max_output_tokens": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Maximum completion tokens (`ChatRequest.max_output_tokens`)."
                        }
                    }
                },
                "UserMessageStreamBody": {
                    "deprecated": true,
                    "description": "Alias of `#/components/schemas/UserMessageBody` for backwards-compatible OpenAPI `$ref` tooling; prefer `UserMessageBody`.",
                    "allOf": [{ "$ref": "#/components/schemas/UserMessageBody" }]
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
                        "selected_provider": { "type": "string" },
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
                        },
                        "behavior_pattern_used": { "type": "string" },
                        "decision_reasoning": { "type": "string" },
                        "decision_confidence": {
                            "type": "number",
                            "format": "float"
                        },
                        "active_task_id": {
                            "type": "string",
                            "description": "ADR-004 task binding: canonical active task for this session after this turn (client may persist as next request ChatRequest.active_task_id)."
                        }
                    }
                },
                "RoomCapabilitiesStreamData": {
                    "type": "object",
                    "required": [
                        "type",
                        "room_id",
                        "room_capabilities_used",
                        "room_tools_used",
                        "room_skills_used"
                    ],
                    "description": "SSE/WS `chat.room_capabilities` payload (`data` field). Aligns with non-stream `ChatResponse` room list fields.",
                    "properties": {
                        "type": {
                            "type": "string",
                            "enum": ["room_capabilities"]
                        },
                        "room_id": { "type": "string" },
                        "room_capabilities_used": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "room_tools_used": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "room_skills_used": {
                            "type": "array",
                            "items": { "type": "string" }
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
                        },
                        "definition_layer": {
                            "type": "string",
                            "description": "user_runtime | workspace_capability | session_runtime"
                        },
                        "extends_workspace_agent": { "type": "string" },
                        "status": {
                            "type": "string",
                            "description": "Optional lifecycle, e.g. temporary placeholder agent."
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
                        "active_work_item_id": {
                            "type": "string",
                            "description": "Optional `work-item.*` id; symmetry with ChatRequest / future task-aware routing signals."
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session key; when set, routing includes the session-scoped agent under agent-runtime/sessions/."
                        },
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

pub(crate) fn swagger_ui_html(swagger_ui_dist_base_url: &str) -> String {
    let spec = openapi_document();
    let spec_json = serde_json::to_string(&spec).expect("openapi document should serialize");
    SWAGGER_UI_HTML
        .replace("__HONEYCOMB_OPENAPI_SPEC__", &spec_json)
        .replace(
            "__SWAGGER_UI_DIST_BASE_URL__",
            swagger_ui_dist_base_url.trim_end_matches('/'),
        )
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
