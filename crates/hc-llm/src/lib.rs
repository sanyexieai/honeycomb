//! Minimal pluggable LLM core for Honeycomb.

use std::collections::BTreeMap;
use std::env;
use std::error::Error as _;
use std::io::{BufRead, BufReader};
use std::thread;
use std::time::Duration;

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

pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn LlmProvider>>,
    retry_policy: LlmRetryPolicy,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            retry_policy: LlmRetryPolicy::from_env(),
        }
    }
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
        let policy = self.retry_policy;
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            match provider.generate(request) {
                Ok(response) => return Ok(response),
                Err(error) if policy.should_retry(attempt, &error) => {
                    let delay = policy.backoff_for_attempt(attempt);
                    policy.log_retry(attempt, &error, delay);
                    thread::sleep(delay);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn generate_stream(
        &self,
        request: &GenerateRequest,
        on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
    ) -> Result<GenerateResponse, LlmError> {
        let provider = self
            .provider(&request.model.provider)
            .ok_or_else(|| LlmError::ProviderNotFound(request.model.provider.clone()))?;
        let policy = self.retry_policy;
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let mut saw_chunk = false;
            let mut wrapped = |chunk: StreamChunk| -> Result<(), LlmError> {
                saw_chunk = true;
                on_chunk(chunk)
            };
            match provider.generate_stream(request, &mut wrapped) {
                Ok(response) => return Ok(response),
                Err(error) if !saw_chunk && policy.should_retry(attempt, &error) => {
                    let delay = policy.backoff_for_attempt(attempt);
                    policy.log_retry(attempt, &error, delay);
                    thread::sleep(delay);
                }
                Err(error) => return Err(error),
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub display_name: &'static str,
    pub default_base_url: &'static str,
    pub balanced_model: &'static str,
    pub fast_model: &'static str,
    pub coding_model: &'static str,
}

pub fn default_registry_from_env() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let provider_id = default_provider_from_env();
    let api_key = provider_api_key_from_env(&provider_id);
    let base_url = provider_base_url_from_env(&provider_id);

    if let Some(api_key) = api_key
        && let Ok(provider) = OpenAiCompatibleProvider::new(
            provider_id.clone(),
            provider_preset(&provider_id)
                .map(|preset| preset.display_name.to_owned())
                .unwrap_or_else(|| format!("{provider_id} compatible")),
            base_url,
            api_key,
        )
    {
        registry.register(provider);
    }

    registry
}

pub fn default_provider_from_env() -> String {
    if let Ok(provider) = env::var("HC_LLM_PROVIDER")
        && !provider.trim().eq_ignore_ascii_case("mock")
    {
        return provider.trim().to_owned();
    }

    if env::var("HC_LLM_API_KEY").is_ok() || env::var("OPENAI_API_KEY").is_ok() {
        return "openai".to_owned();
    }

    if env::var("MINIMAX_API_KEY").is_ok() {
        return "minimax".to_owned();
    }

    if env::var("DEEPSEEK_API_KEY").is_ok() {
        return "deepseek".to_owned();
    }

    "openai".to_owned()
}

pub fn default_model_from_env() -> String {
    let provider = default_provider_from_env();
    if let Ok(model) = env::var("HC_LLM_MODEL")
        && !using_legacy_mock_config()
    {
        return model;
    }

    let model_type = env::var("HC_LLM_MODEL_TYPE").unwrap_or_else(|_| "balanced".to_owned());
    default_model_for_provider(&provider, &model_type)
}

pub fn provider_api_key_from_env(provider_id: &str) -> Option<String> {
    env::var("HC_LLM_API_KEY")
        .ok()
        .or_else(|| env::var(provider_api_key_var_name(provider_id)).ok())
}

pub fn provider_base_url_from_env(provider_id: &str) -> String {
    env::var("HC_LLM_BASE_URL")
        .ok()
        .or_else(|| env::var(provider_base_url_var_name(provider_id)).ok())
        .unwrap_or_else(|| default_base_url_for_provider(provider_id))
}

pub fn default_base_url_for_provider(provider: &str) -> String {
    provider_preset(provider)
        .map(|preset| preset.default_base_url.to_owned())
        .unwrap_or_else(|| "https://api.openai.com/v1".to_owned())
}

pub fn default_model_for_provider(provider: &str, model_type: &str) -> String {
    if let Some(preset) = provider_preset(provider) {
        return match model_type {
            "fast" => preset.fast_model.to_owned(),
            "coding" => preset.coding_model.to_owned(),
            _ => preset.balanced_model.to_owned(),
        };
    }

    match model_type {
        "fast" => "gpt-4.1-mini".to_owned(),
        "coding" => "gpt-4.1".to_owned(),
        _ => "gpt-4.1-mini".to_owned(),
    }
}

pub fn provider_presets() -> &'static [ProviderPreset] {
    &[
        ProviderPreset {
            id: "openai",
            display_name: "OpenAI Compatible",
            default_base_url: "https://api.openai.com/v1",
            balanced_model: "gpt-4.1-mini",
            fast_model: "gpt-4.1-mini",
            coding_model: "gpt-4.1",
        },
        ProviderPreset {
            id: "minimax",
            display_name: "MiniMax Compatible",
            default_base_url: "https://api.minimaxi.com/v1",
            balanced_model: "MiniMax-M2.5",
            fast_model: "MiniMax-M2.5-HighSpeed",
            coding_model: "MiniMax-M2.1",
        },
        ProviderPreset {
            id: "deepseek",
            display_name: "DeepSeek Compatible",
            default_base_url: "https://api.deepseek.com",
            balanced_model: "deepseek-v4-flash",
            fast_model: "deepseek-v4-flash",
            coding_model: "deepseek-v4-pro",
        },
    ]
}

pub fn provider_preset(provider: &str) -> Option<&'static ProviderPreset> {
    provider_presets()
        .iter()
        .find(|preset| preset.id.eq_ignore_ascii_case(provider.trim()))
}

pub fn is_timeout_error(error: &LlmError) -> bool {
    match error {
        LlmError::ProviderFailure(message) => {
            let lowered = message.to_ascii_lowercase();
            lowered.contains("timed out") || lowered.contains("timeout")
        }
        LlmError::ProviderNotFound(_) | LlmError::InvalidRequest(_) => false,
    }
}

pub fn sanitize_assistant_text(content: &str) -> String {
    let without_hidden_blocks = strip_assistant_hidden_blocks(content);
    without_hidden_blocks
        .lines()
        .filter(|line| !is_assistant_control_line(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub fn strip_assistant_hidden_blocks(content: &str) -> String {
    let mut remaining = content;
    let mut output = String::new();

    while let Some((start, tag_name, open_end)) = find_hidden_block_open(remaining) {
        output.push_str(&remaining[..start]);
        let close = format!("</{tag_name}>");
        let after_open = &remaining[open_end..];
        let Some(close_start) = after_open.find(&close) else {
            return output.trim().to_owned();
        };
        remaining = &after_open[close_start + close.len()..];
    }

    output.push_str(remaining);
    output
}

pub fn is_retryable_provider_failure_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "http 408", "http 409", "http 425", "http 429", "http 500", "http 502", "http 503",
        "http 504", "http 529",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || lower.contains("overloaded_error")
        || lower.contains("rate limit")
        || lower.contains("please retry")
        || lower.contains("retry later")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("temporary failure")
}

fn find_hidden_block_open(content: &str) -> Option<(usize, String, usize)> {
    let mut search_from = 0usize;
    while let Some(relative_start) = content[search_from..].find('<') {
        let start = search_from + relative_start;
        let after_lt = &content[start + 1..];
        if after_lt.starts_with('/') || after_lt.starts_with('!') || after_lt.starts_with('?') {
            search_from = start + 1;
            continue;
        }
        let Some(open_end_relative) = after_lt.find('>') else {
            return None;
        };
        let tag_header = &after_lt[..open_end_relative];
        let Some(tag_name) = tag_header.split_whitespace().next() else {
            search_from = start + 1;
            continue;
        };
        if is_hidden_assistant_tag(tag_name) {
            return Some((
                start,
                tag_name.to_owned(),
                start + 1 + open_end_relative + 1,
            ));
        }
        search_from = start + 1;
    }
    None
}

fn is_hidden_assistant_tag(tag_name: &str) -> bool {
    let local_name = tag_name
        .rsplit_once(':')
        .map(|(_, local)| local)
        .unwrap_or(tag_name)
        .to_ascii_lowercase();
    local_name == "think" || local_name == "tool_call" || local_name == "invoke"
}

fn is_assistant_control_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed
        .strip_prefix('$')
        .and_then(|rest| rest.split_whitespace().next())
        .is_some_and(|command| {
            !command.is_empty()
                && command
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch == '_')
        })
    {
        return true;
    }
    if let Some(tag_name) = xml_like_single_line_tag_name(trimmed) {
        let local_name = tag_name
            .rsplit_once(':')
            .map(|(_, local)| local)
            .unwrap_or(tag_name);
        return matches!(local_name, "parameter");
    }
    false
}

fn xml_like_single_line_tag_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('<')?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let end = rest.find([' ', '>'])?;
    Some(&rest[..end])
}

fn using_legacy_mock_config() -> bool {
    env::var("HC_LLM_PROVIDER")
        .map(|provider| provider.trim().eq_ignore_ascii_case("mock"))
        .unwrap_or(false)
}

pub fn provider_api_key_var_name(provider_id: &str) -> &'static str {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "minimax" => "MINIMAX_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        _ => "OPENAI_API_KEY",
    }
}

pub fn provider_base_url_var_name(provider_id: &str) -> &'static str {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "minimax" => "MINIMAX_BASE_URL",
        "deepseek" => "DEEPSEEK_BASE_URL",
        _ => "OPENAI_BASE_URL",
    }
}

#[derive(Debug, Clone, Copy)]
struct LlmRetryPolicy {
    max_attempts: usize,
    base_delay_ms: u64,
    log_retries: bool,
}

impl Default for LlmRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 250,
            log_retries: false,
        }
    }
}

impl LlmRetryPolicy {
    fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            max_attempts: env_usize("HC_LLM_RETRY_MAX_ATTEMPTS")
                .filter(|value| *value > 0)
                .unwrap_or(defaults.max_attempts),
            base_delay_ms: env_u64("HC_LLM_RETRY_BASE_DELAY_MS").unwrap_or(defaults.base_delay_ms),
            log_retries: env_bool("HC_LLM_RETRY_LOG").unwrap_or(defaults.log_retries),
        }
    }

    fn should_retry(&self, attempt: usize, error: &LlmError) -> bool {
        attempt < self.max_attempts && is_retryable_llm_error(error)
    }

    fn backoff_for_attempt(&self, attempt: usize) -> Duration {
        let multiplier = 1u64
            .checked_shl(attempt.saturating_sub(1).min(63) as u32)
            .unwrap_or(u64::MAX);
        Duration::from_millis(self.base_delay_ms.saturating_mul(multiplier))
    }

    fn log_retry(&self, attempt: usize, error: &LlmError, delay: Duration) {
        if !self.log_retries {
            return;
        }
        eprintln!(
            "llm retry> attempt {attempt}/{} failed: {}; retrying in {}ms",
            self.max_attempts,
            error,
            delay.as_millis()
        );
    }
}

fn env_usize(key: &str) -> Option<usize> {
    env::var(key).ok()?.trim().parse().ok()
}

fn env_u64(key: &str) -> Option<u64> {
    env::var(key).ok()?.trim().parse().ok()
}

fn env_bool(key: &str) -> Option<bool> {
    let value = env::var(key).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn is_retryable_llm_error(error: &LlmError) -> bool {
    match error {
        LlmError::ProviderFailure(message) => is_retryable_provider_failure_message(message),
        LlmError::ProviderNotFound(_) | LlmError::InvalidRequest(_) => false,
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
            .timeout(request_timeout_from_env())
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

        let response_text = response
            .text()
            .map_err(|error| LlmError::ProviderFailure(format_transport_error(&error)))?;
        let raw: OpenAiChatResponse = serde_json::from_str(&response_text).map_err(|error| {
            LlmError::ProviderFailure(format!(
                "failed to decode chat response: {}; body: {}",
                error,
                compact_error_body(&response_text)
            ))
        })?;

        let choice = raw
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::ProviderFailure("missing choice".to_owned()))?;

        let message = choice.message;
        let name = message.name.clone();

        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage {
                role: parse_openai_role(&message.role),
                content: message_content_to_string(message),
                name,
            },
            finish_reason: parse_finish_reason(choice.finish_reason.as_deref()),
            usage: raw.usage.map(|usage| TokenUsage {
                input_tokens: usage.prompt_tokens.unwrap_or_default(),
                output_tokens: usage.completion_tokens.unwrap_or_default(),
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
                if let Some(content) = choice.delta.content.map(content_to_string) {
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

fn request_timeout_from_env() -> Duration {
    env_u64("HC_LLM_REQUEST_TIMEOUT_SECS")
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(180))
}

fn compact_error_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > 800 {
        let mut truncated = compact.chars().take(800).collect::<String>();
        truncated.push_str("...");
        truncated
    } else {
        compact
    }
}

fn build_openai_chat_request(request: &GenerateRequest, stream: bool) -> OpenAiChatRequest {
    OpenAiChatRequest {
        model: request.model.model.clone(),
        messages: request
            .messages
            .iter()
            .map(|message| OpenAiMessage {
                role: openai_role(&message.role).to_owned(),
                content: Some(OpenAiMessageContent::Text(message.content.clone())),
                reasoning_content: None,
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
    content: Option<OpenAiMessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<OpenAiMessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum OpenAiMessageContent {
    Text(String),
    Parts(Vec<OpenAiMessageContentPart>),
    Other(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiMessageContentPart {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

fn content_to_string(content: OpenAiMessageContent) -> String {
    match content {
        OpenAiMessageContent::Text(text) => text,
        OpenAiMessageContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|part| part.text)
            .collect::<Vec<_>>()
            .join(""),
        OpenAiMessageContent::Other(value) => match value {
            serde_json::Value::String(text) => text,
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        },
    }
}

fn message_content_to_string(message: OpenAiMessage) -> String {
    let content = message.content.map(content_to_string).unwrap_or_default();
    if !content.trim().is_empty() {
        return content;
    }
    message
        .reasoning_content
        .map(content_to_string)
        .unwrap_or_default()
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
    content: Option<OpenAiMessageContent>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
