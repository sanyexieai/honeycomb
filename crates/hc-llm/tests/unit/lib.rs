use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct RetryOnceProvider {
    calls: Arc<AtomicUsize>,
}

impl LlmProvider for RetryOnceProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "retry-once".to_owned(),
            display_name: "Retry Once".to_owned(),
            supports_chat: true,
            supports_streaming: true,
        }
    }

    fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Err(LlmError::ProviderFailure("http 503: overloaded".to_owned()));
        }

        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        })
    }
}

struct RetryStreamOnceProvider {
    calls: Arc<AtomicUsize>,
}

impl LlmProvider for RetryStreamOnceProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "retry-stream-once".to_owned(),
            display_name: "Retry Stream Once".to_owned(),
            supports_chat: true,
            supports_streaming: true,
        }
    }

    fn generate(&self, _request: &GenerateRequest) -> Result<GenerateResponse, LlmError> {
        unreachable!("streaming test should not call non-streaming method")
    }

    fn generate_stream(
        &self,
        request: &GenerateRequest,
        on_chunk: &mut dyn FnMut(StreamChunk) -> Result<(), LlmError>,
    ) -> Result<GenerateResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Err(LlmError::ProviderFailure("http 429: rate limit".to_owned()));
        }

        on_chunk(StreamChunk {
            delta: "ok".to_owned(),
            finish_reason: None,
        })?;
        Ok(GenerateResponse {
            model: request.model.clone(),
            message: ChatMessage::new(MessageRole::Assistant, "ok"),
            finish_reason: FinishReason::Stop,
            usage: None,
            raw: None,
        })
    }
}

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

#[test]
fn registry_retries_retryable_generate_failures() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ProviderRegistry::new();
    registry.retry_policy = LlmRetryPolicy {
        max_attempts: 2,
        base_delay_ms: 0,
        log_retries: false,
    };
    registry.register(RetryOnceProvider {
        calls: calls.clone(),
    });
    let request = GenerateRequest::new(
        ModelRef::new("retry-once", "mock"),
        vec![ChatMessage::new(MessageRole::User, "hello")],
    );

    let response = registry.generate(&request).expect("retry should succeed");
    assert_eq!(response.message.content, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn registry_retries_stream_failures_before_first_chunk() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ProviderRegistry::new();
    registry.retry_policy = LlmRetryPolicy {
        max_attempts: 2,
        base_delay_ms: 0,
        log_retries: false,
    };
    registry.register(RetryStreamOnceProvider {
        calls: calls.clone(),
    });
    let request = GenerateRequest::new(
        ModelRef::new("retry-stream-once", "mock"),
        vec![ChatMessage::new(MessageRole::User, "hello")],
    );
    let mut output = String::new();

    let response = registry
        .generate_stream(&request, &mut |chunk| {
            output.push_str(&chunk.delta);
            Ok(())
        })
        .expect("retry should succeed");

    assert_eq!(response.message.content, "ok");
    assert_eq!(output, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn registry_stops_after_configured_retry_attempts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ProviderRegistry::new();
    registry.retry_policy = LlmRetryPolicy {
        max_attempts: 1,
        base_delay_ms: 0,
        log_retries: false,
    };
    registry.register(RetryOnceProvider {
        calls: calls.clone(),
    });
    let request = GenerateRequest::new(
        ModelRef::new("retry-once", "mock"),
        vec![ChatMessage::new(MessageRole::User, "hello")],
    );

    let error = registry
        .generate(&request)
        .expect_err("single attempt should return first error");

    assert!(matches!(error, LlmError::ProviderFailure(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn retry_policy_backoff_is_exponential() {
    let policy = LlmRetryPolicy {
        max_attempts: 3,
        base_delay_ms: 100,
        log_retries: false,
    };

    assert_eq!(policy.backoff_for_attempt(1), Duration::from_millis(100));
    assert_eq!(policy.backoff_for_attempt(2), Duration::from_millis(200));
}

#[test]
fn sanitizer_removes_structural_hidden_assistant_markup() {
    let sanitized = sanitize_assistant_text(
        r#"visible
<think>hidden reasoning</think>
$SKILL tool.frontend
<minimax:tool_call>
<parameter name="command">run</parameter>
</minimax:tool_call>
done"#,
    );

    assert!(sanitized.contains("visible"));
    assert!(sanitized.contains("done"));
    assert!(!sanitized.contains("hidden reasoning"));
    assert!(!sanitized.contains("$SKILL"));
    assert!(!sanitized.contains("tool_call"));
    assert!(!sanitized.contains("parameter"));
}

#[test]
fn deepseek_provider_preset_uses_openai_compatible_endpoint() {
    let preset = provider_preset("deepseek").expect("deepseek preset should exist");

    assert_eq!(preset.default_base_url, "https://api.deepseek.com");
    assert_eq!(preset.balanced_model, "deepseek-v4-flash");
    assert_eq!(preset.coding_model, "deepseek-v4-pro");
    assert_eq!(provider_api_key_var_name("deepseek"), "DEEPSEEK_API_KEY");
    assert_eq!(provider_base_url_var_name("deepseek"), "DEEPSEEK_BASE_URL");
}

#[test]
fn openai_compatible_response_accepts_extra_content_shapes() {
    let response: OpenAiChatResponse = serde_json::from_str(
        r#"{
          "id": "chatcmpl-test",
          "choices": [{
            "message": {
              "role": "assistant",
              "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": " world"}
              ],
              "reasoning_content": "hidden"
            },
            "finish_reason": "stop"
          }],
          "usage": {"total_tokens": 12}
        }"#,
    )
    .expect("response should decode");
    let choice = response.choices.into_iter().next().unwrap();

    assert_eq!(
        choice.message.content.map(content_to_string).unwrap(),
        "hello world"
    );
    assert_eq!(response.usage.unwrap().prompt_tokens.unwrap_or_default(), 0);
}

#[test]
fn openai_compatible_response_falls_back_to_reasoning_content() {
    let response: OpenAiChatResponse = serde_json::from_str(
        r#"{
          "choices": [{
            "message": {
              "role": "assistant",
              "content": null,
              "reasoning_content": "reasoned answer"
            },
            "finish_reason": "stop"
          }]
        }"#,
    )
    .expect("response should decode");
    let choice = response.choices.into_iter().next().unwrap();

    assert_eq!(message_content_to_string(choice.message), "reasoned answer");
}
