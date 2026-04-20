//! Minimal pluggable LLM core for Honeycomb.

use std::collections::BTreeMap;
use std::error::Error as _;
use std::io::{BufRead, BufReader};

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
pub struct StreamChunk {
    pub delta: String,
    pub finish_reason: Option<FinishReason>,
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
    fn generate_stream(
        &self,
        request: &GenerateRequest,
        on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
    ) -> Result<GenerateResponse, LlmError> {
        let response = self.generate(request)?;
        if !response.message.content.is_empty() {
            on_chunk(StreamChunk {
                delta: response.message.content.clone(),
                finish_reason: Some(response.finish_reason.clone()),
            })?;
        }
        Ok(response)
    }
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

    pub fn generate_stream(
        &self,
        request: &GenerateRequest,
        on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
    ) -> Result<GenerateResponse, LlmError> {
        let provider = self
            .provider(&request.model.provider)
            .ok_or_else(|| LlmError::ProviderNotFound(request.model.provider.clone()))?;
        provider.generate_stream(request, on_chunk)
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
                supports_streaming: true,
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
        let body = build_openai_chat_request(request, false);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|error| LlmError::ProviderFailure(format_transport_error(&error)))?;

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

    fn generate_stream(
        &self,
        request: &GenerateRequest,
        on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
    ) -> Result<GenerateResponse, LlmError> {
        let body = build_openai_chat_request(request, true);
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|error| LlmError::ProviderFailure(format_transport_error(&error)))?;

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

        let mut assistant_role = MessageRole::Assistant;
        let mut accumulated = String::new();
        let mut finish_reason = FinishReason::Stop;
        let mut raw_chunks = Vec::new();
        let mut reader = BufReader::new(response);
        let mut line = String::new();

        loop {
            line.clear();
            let read = reader
                .read_line(&mut line)
                .map_err(|error| LlmError::ProviderFailure(error.to_string()))?;
            if read == 0 {
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some(payload) = trimmed.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                break;
            }

            let chunk: OpenAiChatStreamChunk = serde_json::from_str(payload)
                .map_err(|error| LlmError::ProviderFailure(error.to_string()))?;
            raw_chunks.push(
                serde_json::to_value(&chunk)
                    .map_err(|error| LlmError::ProviderFailure(error.to_string()))?,
            );

            for choice in chunk.choices {
                if let Some(role) = choice.delta.role.as_deref() {
                    assistant_role = parse_openai_role(role);
                }
                if let Some(content) = choice.delta.content {
                    accumulated.push_str(&content);
                    on_chunk(StreamChunk {
                        delta: content,
                        finish_reason: None,
                    })?;
                }
                if let Some(reason) = choice.finish_reason.as_deref() {
                    finish_reason = parse_finish_reason(Some(reason));
                }
            }
        }

        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage {
                role: assistant_role,
                content: accumulated,
                name: None,
            },
            finish_reason,
            usage: None,
            raw: Some(serde_json::Value::Array(raw_chunks)),
        })
    }
}

fn format_transport_error(error: &reqwest::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(current) = source {
        let message = current.to_string();
        if !message.is_empty() && !parts.iter().any(|part| part == &message) {
            parts.push(message);
        }
        source = current.source();
    }
    parts.join(": ")
}

fn build_openai_chat_request(request: &GenerateRequest, stream: bool) -> OpenAiChatRequest {
    OpenAiChatRequest {
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
        stream,
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

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiChatStreamChunk {
    choices: Vec<OpenAiStreamChoice>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAiStreamDelta {
    role: Option<String>,
    content: Option<String>,
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
    fn registry_returns_error_for_missing_provider() {
        let registry = ProviderRegistry::new();
        let request = GenerateRequest::new(
            ModelRef::new("openai", "gpt-4.1-mini"),
            vec![ChatMessage::new(MessageRole::User, "route this")],
        );

        let error = registry
            .generate(&request)
            .expect_err("missing provider should fail");
        assert!(matches!(error, LlmError::ProviderNotFound(provider) if provider == "openai"));
    }
}
