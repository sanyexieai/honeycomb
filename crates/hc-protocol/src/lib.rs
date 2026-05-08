//! Shared protocol types and traits for Honeycomb.

use serde::{Deserialize, Serialize};

pub mod swarm;

pub const DEFAULT_TENANT_ID: &str = "local";
pub const DEFAULT_USER_ID: &str = "default";

pub mod protocol {
    //! Stable schemas shared across runtime, storage, and UI layers.

    /// Marker trait for records that carry a stable identifier.
    pub trait RecordId {
        fn id(&self) -> &str;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiChatMessage {
    pub role: ApiMessageRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiNamespace {
    #[serde(default = "default_tenant_id")]
    pub tenant_id: String,
    #[serde(default = "default_user_id")]
    pub user_id: String,
}

impl Default for ApiNamespace {
    fn default() -> Self {
        Self {
            tenant_id: default_tenant_id(),
            user_id: default_user_id(),
        }
    }
}

impl ApiNamespace {
    pub fn from_tenant_user(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ApiMemoryQuery {
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_depth: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default)]
    pub messages: Vec<ApiChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_task_id: Option<String>,
    #[serde(default)]
    pub memory: ApiMemoryQuery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRef {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub scope: String,
    pub kind: String,
    pub source_kind: String,
    pub confidence_milli: u16,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatResponse {
    pub message: ApiChatMessage,
    pub model: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_domain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_provider: Option<String>,
    pub recalled_memories: Vec<MemoryRef>,
    pub synthesized_prompt_asset_count: usize,
    #[serde(default)]
    pub room_capabilities_used: Vec<String>,
    #[serde(default)]
    pub room_tools_used: Vec<String>,
    #[serde(default)]
    pub room_skills_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_pattern_used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_confidence: Option<f32>,
    /// Task binding result for this turn (ADR-004); aligns with [`crate::swarm::TaskBindingDecisionRecord::active_task_id`].
    /// Clients may persist and resend as the next [`ChatRequest::active_task_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentProfileSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub domain_id: Option<String>,
    pub priority: i32,
    pub intent_hints: Vec<String>,
    #[serde(default)]
    pub routing_examples: Vec<String>,
    #[serde(default)]
    pub negative_routing_examples: Vec<String>,
    pub tool_refs: Vec<String>,
    pub memory_scope_refs: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentListResponse {
    pub agents: Vec<AgentProfileSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainProfileSummary {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub project_id: Option<String>,
    pub priority: i32,
    pub intent_hints: Vec<String>,
    #[serde(default)]
    pub routing_examples: Vec<String>,
    #[serde(default)]
    pub negative_routing_examples: Vec<String>,
    pub default_agent_id: Option<String>,
    pub tool_refs: Vec<String>,
    pub memory_scope_refs: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainListResponse {
    pub domains: Vec<DomainProfileSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub command: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerListResponse {
    pub servers: Vec<McpServerSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRouteRequest {
    pub input: String,
    #[serde(default)]
    pub namespace: ApiNamespace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRouteCandidate {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<String>,
    pub score: i32,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRouteResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_domain_id: Option<String>,
    pub candidates: Vec<AgentRouteCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorResponse {
    pub error: String,
}

fn default_tenant_id() -> String {
    DEFAULT_TENANT_ID.to_owned()
}

fn default_user_id() -> String {
    DEFAULT_USER_ID.to_owned()
}
