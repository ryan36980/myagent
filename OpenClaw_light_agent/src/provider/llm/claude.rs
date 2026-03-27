//! Anthropic Claude Messages API v1 implementation.

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::LlmProvider;
use crate::auth::AuthMode;
use crate::channel::types::{
    ChatMessage, ContentBlock, LlmResponse, Role, StopReason, StreamEvent, ToolDefinition,
};
use crate::config::ProviderConfig;
use crate::error::{GatewayError, Result};

/// Claude LLM provider using Anthropic Messages API.
pub struct ClaudeProvider {
    client: reqwest::Client,
    auth: AuthMode,
    model: String,
    base_url: String,
    max_tokens: u32,
    /// Extended thinking budget in tokens, or None if thinking is off.
    thinking_budget: Option<u32>,
}

impl ClaudeProvider {
    /// Create a new provider using the traditional API key authentication.
    pub fn new(client: reqwest::Client, config: &ProviderConfig) -> Self {
        Self::with_auth(client, config, AuthMode::ApiKey(config.api_key.clone()))
    }

    /// Create a new provider with the specified authentication mode.
    pub fn with_auth(client: reqwest::Client, config: &ProviderConfig, auth: AuthMode) -> Self {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.anthropic.com".into());
        Self {
            client,
            auth,
            model: config.model.clone(),
            base_url,
            max_tokens: config.max_tokens.unwrap_or(16384),
            thinking_budget: None,
        }
    }

    /// Set the extended thinking budget (in tokens).
    pub fn with_thinking_budget(mut self, budget: Option<u32>) -> Self {
        self.thinking_budget = budget;
        self
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url.trim_end_matches('/'))
    }

    /// API version header: use newer version when thinking is enabled.
    fn api_version(&self) -> &str {
        if self.thinking_budget.is_some() {
            "2025-04-14"
        } else {
            "2023-06-01"
        }
    }

    /// Apply authentication headers to a request builder.
    /// - `ApiKey` → `x-api-key: <key>`
    /// - `OAuth`  → `Authorization: Bearer <token>` + `anthropic-beta: oauth-2025-04-20`
    async fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        match &self.auth {
            AuthMode::ApiKey(key) => Ok(builder.header("x-api-key", key)),
            AuthMode::OAuth(store) => {
                let token = store.lock().await.get_token().await?;
                Ok(builder
                    .header("Authorization", format!("Bearer {}", token))
                    .header("anthropic-beta", crate::auth::OAUTH_BETA_HEADER))
            }
        }
    }
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    async fn chat(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let api_messages: Vec<ApiMessage> = messages.iter().map(|m| m.into()).collect();

        let api_tools: Vec<ApiTool> = tools
            .iter()
            .map(|t| ApiTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();

        let thinking = self.thinking_budget.map(|budget| ApiThinking {
            thinking_type: "enabled".into(),
            budget_tokens: budget,
        });

        let request = ApiRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages: &api_messages,
            tools: if api_tools.is_empty() {
                None
            } else {
                Some(&api_tools)
            },
            thinking,
        };

        debug!(model = %self.model, msg_count = messages.len(), "calling Claude API");

        let builder = self
            .client
            .post(self.messages_url())
            .header("anthropic-version", self.api_version())
            .header("content-type", "application/json")
            .json(&request);
        let response = self.apply_auth(builder).await?.send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Claude API error");
            return Err(GatewayError::Agent(format!(
                "Claude API returned {}: {}",
                status, body
            )));
        }

        let api_resp: ApiResponse = response.json().await?;

        let content = api_resp
            .content
            .into_iter()
            .filter_map(|block| match block {
                ApiContentBlock::Text { text } => Some(ContentBlock::Text { text }),
                ApiContentBlock::ToolUse { id, name, input } => {
                    Some(ContentBlock::ToolUse { id, name, input })
                }
                ApiContentBlock::Thinking { thinking } => {
                    debug!(len = thinking.len(), "received thinking block (discarded)");
                    None
                }
            })
            .collect();

        let stop_reason = match api_resp.stop_reason.as_deref() {
            Some("end_turn") => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some(other) => StopReason::Other(other.to_string()),
            None => StopReason::EndTurn,
        };

        Ok(LlmResponse {
            content,
            stop_reason,
        })
    }

    async fn chat_stream(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let api_messages: Vec<ApiMessage> = messages.iter().map(|m| m.into()).collect();

        let api_tools: Vec<ApiTool> = tools
            .iter()
            .map(|t| ApiTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();

        let thinking = self.thinking_budget.map(|budget| ApiThinking {
            thinking_type: "enabled".into(),
            budget_tokens: budget,
        });

        let request = ApiStreamRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            system,
            messages: &api_messages,
            tools: if api_tools.is_empty() {
                None
            } else {
                Some(&api_tools)
            },
            stream: true,
            thinking,
        };

        tracing::info!(
            model = %self.model,
            msg_count = messages.len(),
            tool_count = api_tools.len(),
            max_tokens = self.max_tokens,
            "calling Claude API (stream)"
        );

        let builder = self
            .client
            .post(self.messages_url())
            .header("anthropic-version", self.api_version())
            .header("content-type", "application/json")
            .json(&request);
        let response = self.apply_auth(builder).await?.send().await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "Claude API stream error");
            return Err(GatewayError::Agent(format!(
                "Claude API returned {}: {}",
                status, body
            )));
        }

        let byte_stream: BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>> =
            Box::pin(response.bytes_stream());

        // Parse SSE events from the byte stream
        let stream = {
            let buffer = String::new();
            let current_tool_id = String::new();
            let current_tool_name = String::new();
            let current_tool_input = String::new();

            futures_util::stream::unfold(
                (byte_stream, buffer, current_tool_id, current_tool_name, current_tool_input),
                move |(mut byte_stream, mut buffer, mut tool_id, mut tool_name, mut tool_input)| async move {
                    loop {
                        // Try to extract a complete SSE event from the buffer
                        if let Some(event_end) = buffer.find("\n\n") {
                            let event_text = buffer[..event_end].to_string();
                            buffer = buffer[event_end + 2..].to_string();

                            // Parse SSE: extract event type and data
                            let mut event_type = String::new();
                            let mut data = String::new();
                            for line in event_text.lines() {
                                if let Some(rest) = line.strip_prefix("event: ") {
                                    event_type = rest.trim().to_string();
                                } else if let Some(rest) = line.strip_prefix("data: ") {
                                    data = rest.to_string();
                                }
                            }

                            if data.is_empty() {
                                continue;
                            }

                            match event_type.as_str() {
                                "content_block_start" => {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                                        let block_type = v["content_block"]["type"].as_str().unwrap_or("");
                                        tracing::debug!(block_type, "SSE content_block_start");
                                        if block_type == "tool_use" {
                                            tool_id = v["content_block"]["id"].as_str().unwrap_or("").to_string();
                                            tool_name = v["content_block"]["name"].as_str().unwrap_or("").to_string();
                                            tool_input.clear();
                                        }
                                        // "thinking" blocks are silently skipped
                                    }
                                    continue;
                                }
                                "content_block_delta" => {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                                        let delta_type = v["delta"]["type"].as_str().unwrap_or("");
                                        match delta_type {
                                            "text_delta" => {
                                                if let Some(text) = v["delta"]["text"].as_str() {
                                                    return Some((
                                                        Ok(StreamEvent::TextDelta(text.to_string())),
                                                        (byte_stream, buffer, tool_id, tool_name, tool_input),
                                                    ));
                                                }
                                            }
                                            "input_json_delta" => {
                                                if let Some(partial) = v["delta"]["partial_json"].as_str() {
                                                    tool_input.push_str(partial);
                                                }
                                            }
                                            // Skip thinking_delta events — discard thinking content
                                            "thinking_delta" => {}
                                            _ => {}
                                        }
                                    }
                                    continue;
                                }
                                "content_block_stop" => {
                                    if !tool_name.is_empty() {
                                        let input: serde_json::Value =
                                            serde_json::from_str(if tool_input.is_empty() { "{}" } else { &tool_input })
                                                .unwrap_or(serde_json::Value::Object(Default::default()));
                                        let event = StreamEvent::ToolUse {
                                            id: std::mem::take(&mut tool_id),
                                            name: std::mem::take(&mut tool_name),
                                            input,
                                        };
                                        tool_input.clear();
                                        return Some((
                                            Ok(event),
                                            (byte_stream, buffer, tool_id, tool_name, tool_input),
                                        ));
                                    }
                                    continue;
                                }
                                "message_delta" => {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                                        let raw_reason = v["delta"]["stop_reason"].as_str().unwrap_or("(none)");
                                        tracing::info!(stop_reason = raw_reason, "SSE message_delta");
                                        let reason = match v["delta"]["stop_reason"].as_str() {
                                            Some("end_turn") => StopReason::EndTurn,
                                            Some("tool_use") => StopReason::ToolUse,
                                            Some("max_tokens") => StopReason::MaxTokens,
                                            Some(other) => StopReason::Other(other.to_string()),
                                            None => StopReason::EndTurn,
                                        };
                                        return Some((
                                            Ok(StreamEvent::Done { stop_reason: reason }),
                                            (byte_stream, buffer, tool_id, tool_name, tool_input),
                                        ));
                                    }
                                    continue;
                                }
                                "message_stop" => {
                                    return None;
                                }
                                _ => continue,
                            }
                        }

                        // Need more data from the byte stream
                        match byte_stream.next().await {
                            Some(Ok(bytes)) => {
                                buffer.push_str(&String::from_utf8_lossy(&bytes));
                            }
                            Some(Err(e)) => {
                                return Some((
                                    Err(GatewayError::Transport(e)),
                                    (byte_stream, buffer, tool_id, tool_name, tool_input),
                                ));
                            }
                            None => {
                                // Stream ended without a Done event
                                if !buffer.trim().is_empty() {
                                    warn!("Claude stream ended with buffered data");
                                }
                                return None;
                            }
                        }
                    }
                },
            )
        };

        Ok(Box::pin(stream))
    }
}

// ========== Anthropic API types ==========

#[derive(Serialize)]
struct ApiThinking {
    #[serde(rename = "type")]
    thinking_type: String,
    budget_tokens: u32,
}

#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: &'a [ApiMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ApiTool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ApiThinking>,
}

#[derive(Serialize)]
struct ApiStreamRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: &'a [ApiMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ApiTool]>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ApiThinking>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: Vec<ApiContentBlockSer>,
}

#[derive(Serialize)]
struct ApiImageSource {
    #[serde(rename = "type")]
    source_type: String,
    media_type: String,
    data: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContentBlockSer {
    Text {
        text: String,
    },
    Image {
        source: ApiImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

impl From<&ChatMessage> for ApiMessage {
    fn from(msg: &ChatMessage) -> Self {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };

        let content = msg
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => ApiContentBlockSer::Text { text: text.clone() },
                ContentBlock::Image {
                    source_type,
                    media_type,
                    data,
                } => ApiContentBlockSer::Image {
                    source: ApiImageSource {
                        source_type: source_type.clone(),
                        media_type: media_type.clone(),
                        data: data.clone(),
                    },
                },
                ContentBlock::ToolUse { id, name, input } => ApiContentBlockSer::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                },
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } => ApiContentBlockSer::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                },
            })
            .collect();

        ApiMessage {
            role: role.to_string(),
            content,
        }
    }
}

#[derive(Serialize)]
struct ApiTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ApiContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ApiContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    Thinking { thinking: String },
}

// ========== Tests ==========

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;
    use futures_util::StreamExt;
    use serde_json::json;

    fn test_provider(base_url: &str) -> ClaudeProvider {
        let config = ProviderConfig {
            api_key: "test-key".into(),
            model: "test-model".into(),
            max_tokens: Some(256),
            base_url: Some(base_url.into()),
        };
        ClaudeProvider::new(reqwest::Client::new(), &config)
    }

    fn sse_text_response(text_chunks: &[&str]) -> String {
        let mut body = String::new();
        body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test\",\"stop_reason\":null}}\n\n");
        body.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
        for chunk in text_chunks {
            let escaped = chunk.replace('\\', "\\\\").replace('"', "\\\"");
            body.push_str(&format!(
                "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{escaped}\"}}}}\n\n"
            ));
        }
        body.push_str("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
        body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n");
        body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        body
    }

    fn sse_tool_use_response(tool_id: &str, tool_name: &str, args_json: &str) -> String {
        let mut body = String::new();
        body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test\",\"stop_reason\":null}}\n\n");
        body.push_str(&format!(
            "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{tool_id}\",\"name\":\"{tool_name}\",\"input\":{{}}}}}}\n\n"
        ));
        // Send args as one input_json_delta chunk
        let escaped = args_json.replace('\\', "\\\\").replace('"', "\\\"");
        body.push_str(&format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{escaped}\"}}}}\n\n"
        ));
        body.push_str("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
        body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n");
        body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        body
    }

    #[tokio::test]
    async fn stream_text_deltas() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_text_response(&["Hello", ", ", "world!"]), "text/event-stream"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        }];

        let mut stream = provider.chat_stream("sys", &messages, &[]).await.unwrap();

        let mut texts = Vec::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::TextDelta(t) => texts.push(t),
                StreamEvent::Done { stop_reason } => done = Some(stop_reason),
                _ => {}
            }
        }

        assert_eq!(texts, vec!["Hello", ", ", "world!"]);
        assert_eq!(done, Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn stream_tool_use() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        sse_tool_use_response("tool_01", "get_weather", r#"{"city":"Paris"}"#),
                        "text/event-stream",
                    ),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Weather?".into() }],
        }];

        let mut stream = provider.chat_stream("sys", &messages, &[]).await.unwrap();

        let mut tool_uses = Vec::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::ToolUse { id, name, input } => tool_uses.push((id, name, input)),
                StreamEvent::Done { stop_reason } => done = Some(stop_reason),
                _ => {}
            }
        }

        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].0, "tool_01");
        assert_eq!(tool_uses[0].1, "get_weather");
        assert_eq!(tool_uses[0].2, json!({"city": "Paris"}));
        assert_eq!(done, Some(StopReason::ToolUse));
    }

    #[tokio::test]
    async fn stream_api_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(500).set_body_raw(
                r#"{"error":"internal"}"#,
                "application/json",
            ))
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        }];

        let result = provider.chat_stream("sys", &messages, &[]).await;
        assert!(result.is_err(), "expected error for 500 response");
        match result.err().unwrap() {
            GatewayError::Agent(msg) => assert!(msg.contains("500")),
            other => panic!("expected Agent error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn stream_max_tokens_stop_reason() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mut body = String::new();
        body.push_str("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"test\",\"stop_reason\":null}}\n\n");
        body.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
        body.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"truncated\"}}\n\n");
        body.push_str("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
        body.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n");
        body.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "text/event-stream"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "Hi".into() }],
        }];

        let mut stream = provider.chat_stream("sys", &messages, &[]).await.unwrap();
        let mut done = None;
        while let Some(event) = stream.next().await {
            if let Ok(StreamEvent::Done { stop_reason }) = event {
                done = Some(stop_reason);
            }
        }
        assert_eq!(done, Some(StopReason::MaxTokens));
    }
}
