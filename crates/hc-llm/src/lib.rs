//! Minimal pluggable LLM core for Honeycomb.

use std::collections::BTreeMap;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            name: None,
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerateRequest {
    pub model: ModelRef,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub metadata: BTreeMap<String, String>,
}

impl GenerateRequest {
    pub fn new(model: ModelRef, messages: Vec<ChatMessage>) -> Self {
        Self {
            model,
            messages,
            temperature: None,
            max_output_tokens: None,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerateResponse {
    pub model: ModelRef,
    pub message: ChatMessage,
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
    pub raw: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCall,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub supports_chat: bool,
    pub supports_streaming: bool,
}

pub trait LlmProvider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, LlmError>;
}

#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn LlmProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<P>(&mut self, provider: P)
    where
        P: LlmProvider + 'static,
    {
        let id = provider.info().id;
        self.providers.insert(id, Box::new(provider));
    }

    pub fn provider(&self, id: &str) -> Option<&dyn LlmProvider> {
        self.providers.get(id).map(|provider| provider.as_ref())
    }

    pub fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let provider = self
            .provider(&request.model.provider)
            .ok_or_else(|| LlmError::ProviderNotFound(request.model.provider.clone()))?;
        provider.generate(request)
    }

    pub fn provider_infos(&self) -> Vec<ProviderInfo> {
        self.providers
            .values()
            .map(|provider| provider.info())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("provider failure: {0}")]
    ProviderFailure(String),
}

#[derive(Debug, Clone, Default)]
pub struct MockProvider;

impl MockProvider {
    pub fn new() -> Self {
        Self
    }
}

impl LlmProvider for MockProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "mock".to_owned(),
            display_name: "Mock Provider".to_owned(),
            supports_chat: true,
            supports_streaming: false,
        }
    }

    fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let last_user_message = request
            .messages
            .iter()
            .rev()
            .find(|message| matches!(message.role, MessageRole::User))
            .ok_or_else(|| LlmError::InvalidRequest("missing user message".to_owned()))?;

        let content = format!(
            "mock:{}:{}",
            request.model.model,
            last_user_message.content.trim()
        );

        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage::new(MessageRole::Assistant, content),
            finish_reason: FinishReason::Stop,
            usage: Some(TokenUsage {
                input_tokens: request.messages.len() as u32,
                output_tokens: 1,
            }),
            raw: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleProvider {
    info: ProviderInfo,
    base_url: String,
    api_key: String,
    client: Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(LlmError::InvalidRequest("missing api key".to_owned()));
        }

        let client = Client::builder()
            .build()
            .map_err(|error| LlmError::ProviderFailure(error.to_string()))?;

        Ok(Self {
            info: ProviderInfo {
                id: id.into(),
                display_name: display_name.into(),
                supports_chat: true,
                supports_streaming: false,
            },
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            client,
        })
    }
}

impl LlmProvider for OpenAiCompatibleProvider {
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }

    fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let body = OpenAiChatRequest {
            model: request.model.model.clone(),
            messages: request
                .messages
                .iter()
                .map(|message| OpenAiMessage {
                    role: openai_role(&message.role).to_owned(),
                    content: Some(message.content.clone()),
                    name: message.name.clone(),
                })
                .collect(),
            temperature: request.temperature,
            max_tokens: request.max_output_tokens,
            stream: false,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|error| LlmError::ProviderFailure(error.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response
                .text()
                .unwrap_or_else(|_| "<failed to read error body>".to_owned());
            return Err(LlmError::ProviderFailure(format!(
                "http {}: {}",
                status.as_u16(),
                text
            )));
        }

        let raw: OpenAiChatResponse = response
            .json()
            .map_err(|error| LlmError::ProviderFailure(error.to_string()))?;

        let choice = raw
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::ProviderFailure("missing choice".to_owned()))?;

        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage {
                role: parse_openai_role(&choice.message.role),
                content: choice.message.content.unwrap_or_default(),
                name: choice.message.name,
            },
            finish_reason: parse_finish_reason(choice.finish_reason.as_deref()),
            usage: raw.usage.map(|usage| TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            }),
            raw: raw.raw,
        })
    }
}

fn openai_role(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn parse_openai_role(role: &str) -> MessageRole {
    match role {
        "system" => MessageRole::System,
        "assistant" => MessageRole::Assistant,
        "tool" => MessageRole::Tool,
        _ => MessageRole::User,
    }
}

fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("length") => FinishReason::Length,
        Some("tool_calls") => FinishReason::ToolCall,
        Some("stop") | None => FinishReason::Stop,
        _ => FinishReason::Error,
    }
}

#[derive(Debug, Serialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
    #[serde(flatten)]
    raw: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_provider_echoes_last_user_message() {
        let provider = MockProvider::new();
        let request = GenerateRequest::new(
            ModelRef::new("mock", "demo"),
            vec![
                ChatMessage::new(MessageRole::System, "You are concise."),
                ChatMessage::new(MessageRole::User, "hello world"),
            ],
        );

        let response = provider.generate(&request).expect("mock should respond");

        assert_eq!(response.model.provider, "mock");
        assert_eq!(response.model.model, "demo");
        assert_eq!(response.message.role, MessageRole::Assistant);
        assert_eq!(response.message.content, "mock:demo:hello world");
        assert_eq!(response.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn registry_routes_request_to_matching_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register(MockProvider::new());

        let request = GenerateRequest::new(
            ModelRef::new("mock", "demo"),
            vec![ChatMessage::new(MessageRole::User, "route this")],
        );

        let response = registry.generate(&request).expect("registry should route");
        assert_eq!(response.message.content, "mock:demo:route this");
    }
}
