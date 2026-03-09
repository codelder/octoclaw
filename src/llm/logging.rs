//! Debug logging wrapper for LLM providers.
//!
//! Logs serialized request/response payloads at `DEBUG` level so all providers
//! share the same visibility into prompt/response traffic.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::json;

use crate::bootstrap::ironclaw_base_dir;
use crate::error::LlmError;
use crate::llm::provider::{
    CompletionRequest, CompletionResponse, LlmProvider, ModelMetadata, ToolCompletionRequest,
    ToolCompletionResponse,
};

const LLM_PAYLOAD_LOG_ENV: &str = "IRONCLAW_LLM_PAYLOAD_LOG";
const LLM_PAYLOAD_LOG_PATH_ENV: &str = "IRONCLAW_LLM_PAYLOAD_LOG_PATH";

pub struct LoggingProvider {
    inner: Arc<dyn LlmProvider>,
}

impl LoggingProvider {
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }

    fn log_payload<T: Serialize>(direction: &str, kind: &str, provider: &str, payload: &T) {
        match serde_json::to_string(payload) {
            Ok(json_text) => {
                if tracing::enabled!(tracing::Level::DEBUG) {
                    tracing::debug!(
                        provider,
                        kind,
                        direction,
                        body = %json_text,
                        "llm payload"
                    );
                }
                if let Some(path) = llm_payload_log_path() {
                    if let Err(error) = append_json_line(
                        &path,
                        json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "provider": provider,
                            "kind": kind,
                            "direction": direction,
                            "payload": serde_json::from_str::<serde_json::Value>(&json_text)
                                .unwrap_or_else(|_| json!({ "raw": json_text })),
                        }),
                    ) {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "failed to append llm payload log"
                        );
                    }
                }
            }
            Err(error) => tracing::debug!(
                provider,
                kind,
                direction,
                %error,
                "failed to serialize llm payload"
            ),
        }
    }
}

fn llm_payload_log_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(LLM_PAYLOAD_LOG_PATH_ENV) {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }

    let enabled = std::env::var(LLM_PAYLOAD_LOG_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);

    if enabled {
        Some(ironclaw_base_dir().join("logs").join("llm-payload.ndjson"))
    } else {
        None
    }
}

fn append_json_line(path: &Path, value: serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, &value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

#[async_trait]
impl LlmProvider for LoggingProvider {
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn cost_per_token(&self) -> (Decimal, Decimal) {
        self.inner.cost_per_token()
    }

    fn cache_write_multiplier(&self) -> Decimal {
        self.inner.cache_write_multiplier()
    }

    fn cache_read_discount(&self) -> Decimal {
        self.inner.cache_read_discount()
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let provider = self.inner.model_name().to_string();
        Self::log_payload("request", "complete", &provider, &request);

        match self.inner.complete(request).await {
            Ok(response) => {
                Self::log_payload("response", "complete", &provider, &response);
                Ok(response)
            }
            Err(error) => {
                tracing::debug!(
                    provider = %provider,
                    kind = "complete",
                    %error,
                    "llm request failed"
                );
                Err(error)
            }
        }
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, LlmError> {
        let provider = self.inner.model_name().to_string();
        Self::log_payload("request", "complete_with_tools", &provider, &request);

        match self.inner.complete_with_tools(request).await {
            Ok(response) => {
                Self::log_payload("response", "complete_with_tools", &provider, &response);
                Ok(response)
            }
            Err(error) => {
                tracing::debug!(
                    provider = %provider,
                    kind = "complete_with_tools",
                    %error,
                    "llm request failed"
                );
                Err(error)
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        self.inner.list_models().await
    }

    async fn model_metadata(&self) -> Result<ModelMetadata, LlmError> {
        self.inner.model_metadata().await
    }

    fn effective_model_name(&self, requested_model: Option<&str>) -> String {
        self.inner.effective_model_name(requested_model)
    }

    fn active_model_name(&self) -> String {
        self.inner.active_model_name()
    }

    fn set_model(&self, model: &str) -> Result<(), LlmError> {
        self.inner.set_model(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::tempdir;

    use crate::llm::provider::{ChatMessage, FinishReason, ToolCall, ToolDefinition};

    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_name(&self) -> &str {
            "stub-model"
        }

        fn cost_per_token(&self) -> (Decimal, Decimal) {
            (Decimal::ZERO, Decimal::ZERO)
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, LlmError> {
            Ok(CompletionResponse {
                content: "ok".to_string(),
                input_tokens: 1,
                output_tokens: 2,
                finish_reason: FinishReason::Stop,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, LlmError> {
            Ok(ToolCompletionResponse {
                content: Some("tool-ok".to_string()),
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"cmd": "pwd"}),
                }],
                input_tokens: 3,
                output_tokens: 4,
                finish_reason: FinishReason::ToolUse,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        }
    }

    #[tokio::test]
    async fn logging_provider_delegates_complete() {
        let provider = LoggingProvider::new(Arc::new(StubProvider));
        let response = provider
            .complete(CompletionRequest::new(vec![ChatMessage::user("hi")]))
            .await
            .unwrap();
        assert_eq!(response.content, "ok");
    }

    #[tokio::test]
    async fn logging_provider_delegates_complete_with_tools() {
        let provider = LoggingProvider::new(Arc::new(StubProvider));
        let response = provider
            .complete_with_tools(ToolCompletionRequest::new(
                vec![ChatMessage::user("hi")],
                vec![ToolDefinition {
                    name: "shell".to_string(),
                    description: "run shell".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }],
            ))
            .await
            .unwrap();
        assert_eq!(response.content.as_deref(), Some("tool-ok"));
        assert_eq!(response.tool_calls.len(), 1);
    }

    #[test]
    fn append_json_line_writes_ndjson_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("logs").join("llm-payload.ndjson");

        append_json_line(&path, json!({"provider": "test", "direction": "request"})).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"provider\":\"test\""));
        assert!(text.ends_with('\n'));
    }
}
