//! OpenAI Chat Completions compatible LLM provider.
//!
//! Works with any service exposing the OpenAI `/v1/chat/completions` endpoint:
//! OpenAI, Groq, DeepSeek, GLM, local vLLM, etc.

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use super::LlmProvider;
use crate::channel::types::{
    ChatMessage, ContentBlock, LlmResponse, Role, StopReason, StreamEvent, ToolDefinition,
};
use crate::config::ProviderConfig;
use crate::error::{GatewayError, Result};

/// OpenAI-compatible LLM provider.
pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    max_tokens: u32,
}

impl OpenAiCompatProvider {
    pub fn new(client: reqwest::Client, config: &ProviderConfig) -> Self {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".into());
        Self {
            client,
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            base_url,
            max_tokens: config.max_tokens.unwrap_or(4096),
        }
    }
}

/// Build the full OaiMessage array from system prompt + internal messages.
fn build_oai_messages(system: &str, messages: &[ChatMessage]) -> Vec<OaiMessage> {
    let mut oai_messages: Vec<OaiMessage> = Vec::with_capacity(messages.len() + 1);

    if !system.is_empty() {
        oai_messages.push(OaiMessage {
            role: "system".into(),
            content: Some(serde_json::Value::String(system.into())),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in messages {
        match msg.role {
            Role::User => {
                let has_tool_results = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }));

                if has_tool_results {
                    for block in &msg.content {
                        match block {
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                            } => {
                                oai_messages.push(OaiMessage {
                                    role: "tool".into(),
                                    content: Some(serde_json::Value::String(content.clone())),
                                    tool_calls: None,
                                    tool_call_id: Some(tool_use_id.clone()),
                                });
                            }
                            ContentBlock::Text { text } => {
                                oai_messages.push(OaiMessage {
                                    role: "user".into(),
                                    content: Some(serde_json::Value::String(text.clone())),
                                    tool_calls: None,
                                    tool_call_id: None,
                                });
                            }
                            _ => {}
                        }
                    }
                } else {
                    oai_messages.push(OaiMessage {
                        role: "user".into(),
                        content: build_user_content(&msg.content),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
            Role::Assistant => {
                let has_tool_use = msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

                if has_tool_use {
                    let text: String = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let tool_calls: Vec<OaiToolCall> = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } => Some(OaiToolCall {
                                id: id.clone(),
                                r#type: "function".into(),
                                function: OaiFunction {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            }),
                            _ => None,
                        })
                        .collect();

                    oai_messages.push(OaiMessage {
                        role: "assistant".into(),
                        content: if text.is_empty() {
                            None
                        } else {
                            Some(serde_json::Value::String(text))
                        },
                        tool_calls: Some(tool_calls),
                        tool_call_id: None,
                    });
                } else {
                    let text: String = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    oai_messages.push(OaiMessage {
                        role: "assistant".into(),
                        content: Some(serde_json::Value::String(text)),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    oai_messages
}

/// Build OaiMessage content value from a list of ContentBlocks.
///
/// - Text-only: `Value::String(text)` (backward compatible).
/// - With images: `Value::Array([{"type":"text",...}, {"type":"image_url",...}])`.
fn build_user_content(blocks: &[ContentBlock]) -> Option<serde_json::Value> {
    let mut texts: Vec<&str> = Vec::new();
    let mut images: Vec<&ContentBlock> = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => texts.push(text),
            ContentBlock::Image { .. } => images.push(block),
            _ => {}
        }
    }

    if images.is_empty() {
        // Text-only: backward compatible string content
        let joined = texts.join("\n");
        if joined.is_empty() {
            None
        } else {
            Some(serde_json::Value::String(joined))
        }
    } else {
        // Multimodal: array of content parts
        let mut parts = Vec::new();
        let joined = texts.join("\n");
        if !joined.is_empty() {
            parts.push(serde_json::json!({"type": "text", "text": joined}));
        }
        for img in images {
            if let ContentBlock::Image {
                media_type, data, ..
            } = img
            {
                parts.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", media_type, data)
                    }
                }));
            }
        }
        Some(serde_json::Value::Array(parts))
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    async fn chat(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let oai_messages = build_oai_messages(system, messages);

        // Convert tool definitions
        let oai_tools: Vec<OaiToolDef> = tools
            .iter()
            .map(|t| OaiToolDef {
                r#type: "function".into(),
                function: OaiFunctionDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect();

        let request = OaiRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            messages: &oai_messages,
            tools: if oai_tools.is_empty() {
                None
            } else {
                Some(&oai_tools)
            },
        };

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        debug!(model = %self.model, url = %url, msg_count = messages.len(), "calling OpenAI-compat API");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "OpenAI-compat API error");
            return Err(GatewayError::Agent(format!(
                "OpenAI-compat API returned {}: {}",
                status, body
            )));
        }

        let api_resp: OaiResponse = response.json().await?;

        let choice = api_resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| GatewayError::Agent("empty choices array".into()))?;

        // Build content blocks from the response
        let mut content = Vec::new();

        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text });
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let input: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                content.push(ContentBlock::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input,
                });
            }
        }

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("tool_calls") => StopReason::ToolUse,
            Some("stop") => StopReason::EndTurn,
            Some("length") => StopReason::MaxTokens,
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
        let oai_messages = build_oai_messages(system, messages);

        let oai_tools: Vec<OaiToolDef> = tools
            .iter()
            .map(|t| OaiToolDef {
                r#type: "function".into(),
                function: OaiFunctionDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect();

        let request = OaiStreamRequest {
            model: &self.model,
            max_tokens: self.max_tokens,
            messages: &oai_messages,
            tools: if oai_tools.is_empty() {
                None
            } else {
                Some(&oai_tools)
            },
            stream: true,
        };

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        debug!(model = %self.model, url = %url, msg_count = messages.len(), "calling OpenAI-compat API (stream)");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(status = %status, body = %body, "OpenAI-compat API stream error");
            return Err(GatewayError::Agent(format!(
                "OpenAI-compat API returned {}: {}",
                status, body
            )));
        }

        let byte_stream: futures_util::stream::BoxStream<'static, std::result::Result<bytes::Bytes, reqwest::Error>> =
            Box::pin(response.bytes_stream());

        // Parse SSE lines from the byte stream
        let stream = {
            let buffer = String::new();
            // Track in-progress tool calls by index
            let tool_calls: std::collections::HashMap<u32, (String, String, String)> =
                std::collections::HashMap::new();

            futures_util::stream::unfold(
                (byte_stream, buffer, tool_calls),
                move |(mut byte_stream, mut buffer, mut tool_calls)| async move {
                    loop {
                        // Extract complete lines from buffer
                        if let Some(nl_pos) = buffer.find('\n') {
                            let line = buffer[..nl_pos].trim_end_matches('\r').to_string();
                            buffer = buffer[nl_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            let data = if let Some(rest) = line.strip_prefix("data: ") {
                                rest
                            } else {
                                continue;
                            };

                            if data == "[DONE]" {
                                // Emit any pending tool calls
                                // The Done event should have been emitted via finish_reason
                                return None;
                            }

                            let v: serde_json::Value = match serde_json::from_str(data) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };

                            let choice = &v["choices"][0];
                            let delta = &choice["delta"];

                            // Check for text delta
                            if let Some(content) = delta["content"].as_str() {
                                if !content.is_empty() {
                                    return Some((
                                        Ok(StreamEvent::TextDelta(content.to_string())),
                                        (byte_stream, buffer, tool_calls),
                                    ));
                                }
                            }

                            // Check for tool call deltas
                            if let Some(tcs) = delta["tool_calls"].as_array() {
                                for tc in tcs {
                                    let idx = tc["index"].as_u64().unwrap_or(0) as u32;
                                    let entry = tool_calls.entry(idx).or_insert_with(|| {
                                        (String::new(), String::new(), String::new())
                                    });
                                    if let Some(id) = tc["id"].as_str() {
                                        entry.0 = id.to_string();
                                    }
                                    if let Some(name) = tc["function"]["name"].as_str() {
                                        entry.1 = name.to_string();
                                    }
                                    if let Some(args) = tc["function"]["arguments"].as_str() {
                                        entry.2.push_str(args);
                                    }
                                }
                            }

                            // Check for finish_reason
                            if let Some(reason) = choice["finish_reason"].as_str() {
                                // Before emitting Done, emit any accumulated tool calls
                                // one at a time (unfold yields one item per iteration)
                                if !tool_calls.is_empty() {
                                    let mut keys: Vec<u32> = tool_calls.keys().copied().collect();
                                    keys.sort();
                                    let first_key = keys[0];
                                    if let Some((id, name, args)) = tool_calls.remove(&first_key) {
                                        let input: serde_json::Value =
                                            serde_json::from_str(&args).unwrap_or_default();
                                        // Re-inject the line so finish_reason is seen again
                                        buffer = format!("data: {}\n{}", data, buffer);
                                        return Some((
                                            Ok(StreamEvent::ToolUse { id, name, input }),
                                            (byte_stream, buffer, tool_calls),
                                        ));
                                    }
                                }

                                let stop_reason = match reason {
                                    "stop" => StopReason::EndTurn,
                                    "tool_calls" => StopReason::ToolUse,
                                    "length" => StopReason::MaxTokens,
                                    other => StopReason::Other(other.to_string()),
                                };

                                return Some((
                                    Ok(StreamEvent::Done { stop_reason }),
                                    (byte_stream, buffer, tool_calls),
                                ));
                            }

                            continue;
                        }

                        // Need more data
                        match byte_stream.next().await {
                            Some(Ok(bytes)) => {
                                buffer.push_str(&String::from_utf8_lossy(&bytes));
                            }
                            Some(Err(e)) => {
                                return Some((
                                    Err(GatewayError::Transport(e)),
                                    (byte_stream, buffer, tool_calls),
                                ));
                            }
                            None => {
                                if !buffer.trim().is_empty() {
                                    warn!("OpenAI stream ended with buffered data");
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

// ========== OpenAI API types ==========

#[derive(Serialize)]
struct OaiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: &'a [OaiMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [OaiToolDef]>,
}

#[derive(Serialize)]
struct OaiStreamRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: &'a [OaiMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [OaiToolDef]>,
    stream: bool,
}

#[derive(Serialize, Clone)]
struct OaiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize, Clone)]
struct OaiToolCall {
    id: String,
    r#type: String,
    function: OaiFunction,
}

#[derive(Serialize, Clone)]
struct OaiFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct OaiToolDef {
    r#type: String,
    function: OaiFunctionDef,
}

#[derive(Serialize)]
struct OaiFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Deserialize)]
struct OaiResponse {
    choices: Vec<OaiChoice>,
}

#[derive(Deserialize)]
struct OaiChoice {
    message: OaiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OaiResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OaiResponseToolCall>>,
}

#[derive(Deserialize)]
struct OaiResponseToolCall {
    id: String,
    function: OaiResponseFunction,
}

#[derive(Deserialize)]
struct OaiResponseFunction {
    name: String,
    arguments: String,
}

// ========== Tests ==========

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::types::{ChatMessage, ContentBlock, Role, StopReason, ToolDefinition};
    use crate::config::ProviderConfig;
    use serde_json::json;

    // Helper: load a fixture file relative to project root
    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/openai_compat/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {path}: {e}"))
    }

    // Helper: build a minimal provider pointing at the given base_url
    fn test_provider(base_url: &str) -> OpenAiCompatProvider {
        let config = ProviderConfig {
            api_key: "test-key".into(),
            model: "test-model".into(),
            max_tokens: Some(256),
            base_url: Some(base_url.into()),
        };
        OpenAiCompatProvider::new(reqwest::Client::new(), &config)
    }

    // -----------------------------------------------------------------------
    // A. Request serialization
    // -----------------------------------------------------------------------

    #[test]
    fn ser_system_message() {
        let msg = OaiMessage {
            role: "system".into(),
            content: Some(serde_json::Value::String("You are a helpful assistant.".into())),
            tool_calls: None,
            tool_call_id: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["role"], "system");
        assert_eq!(v["content"], "You are a helpful assistant.");
        assert!(v.get("tool_calls").is_none());
        assert!(v.get("tool_call_id").is_none());
    }

    #[test]
    fn ser_user_message() {
        let msg = OaiMessage {
            role: "user".into(),
            content: Some(serde_json::Value::String("Hi".into())),
            tool_calls: None,
            tool_call_id: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "Hi");
    }

    #[test]
    fn ser_assistant_with_tool_calls() {
        let msg = OaiMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![OaiToolCall {
                id: "call_001".into(),
                r#type: "function".into(),
                function: OaiFunction {
                    name: "ha_control".into(),
                    arguments: r#"{"entity_id":"light.room"}"#.into(),
                },
            }]),
            tool_call_id: None,
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["role"], "assistant");
        // content is None → skip_serializing_if means key absent
        assert!(v.get("content").is_none());
        let tc = &v["tool_calls"];
        assert!(tc.is_array());
        assert_eq!(tc[0]["id"], "call_001");
        assert_eq!(tc[0]["type"], "function");
        assert_eq!(tc[0]["function"]["name"], "ha_control");
    }

    #[test]
    fn ser_tool_result_message() {
        let msg = OaiMessage {
            role: "tool".into(),
            content: Some(serde_json::Value::String("OK".into())),
            tool_calls: None,
            tool_call_id: Some("call_001".into()),
        };
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "call_001");
        assert_eq!(v["content"], "OK");
    }

    #[test]
    fn ser_tool_def() {
        let def = OaiToolDef {
            r#type: "function".into(),
            function: OaiFunctionDef {
                name: "get_weather".into(),
                description: "Get current weather".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    },
                    "required": ["city"]
                }),
            },
        };
        let v = serde_json::to_value(&def).unwrap();
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "get_weather");
        assert_eq!(v["function"]["description"], "Get current weather");
        assert_eq!(v["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn ser_request_without_tools() {
        let msgs = vec![OaiMessage {
            role: "user".into(),
            content: Some(serde_json::Value::String("Hello".into())),
            tool_calls: None,
            tool_call_id: None,
        }];
        let req = OaiRequest {
            model: "test-model",
            max_tokens: 256,
            messages: &msgs,
            tools: None,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("tools").is_none());
        assert_eq!(v["model"], "test-model");
        assert_eq!(v["max_tokens"], 256);
    }

    #[test]
    fn ser_request_with_tools() {
        let msgs = vec![OaiMessage {
            role: "user".into(),
            content: Some(serde_json::Value::String("Hello".into())),
            tool_calls: None,
            tool_call_id: None,
        }];
        let tools = vec![OaiToolDef {
            r#type: "function".into(),
            function: OaiFunctionDef {
                name: "test".into(),
                description: "test tool".into(),
                parameters: json!({"type": "object"}),
            },
        }];
        let req = OaiRequest {
            model: "test-model",
            max_tokens: 256,
            messages: &msgs,
            tools: Some(&tools),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v["tools"].is_array());
        assert_eq!(v["tools"].as_array().unwrap().len(), 1);
    }

    // -----------------------------------------------------------------------
    // B. Response deserialization (fixtures)
    // -----------------------------------------------------------------------

    #[test]
    fn deser_basic_chat() {
        let json = fixture("basic_chat_response.json");
        let resp: OaiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Hello! How can I help you today?")
        );
        assert!(resp.choices[0].message.tool_calls.is_none());
    }

    #[test]
    fn deser_tool_call() {
        let json = fixture("tool_call_response.json");
        let resp: OaiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(
            resp.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        let tc = resp.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("should have tool_calls");
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_abc123");
        assert_eq!(tc[0].function.name, "ha_control");
        // arguments is a JSON string — verify it parses
        let args: serde_json::Value =
            serde_json::from_str(&tc[0].function.arguments).unwrap();
        assert_eq!(args["entity_id"], "light.living_room");
    }

    #[test]
    fn deser_multi_turn() {
        let json = fixture("multi_turn_response.json");
        let resp: OaiResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        let content = resp.choices[0].message.content.as_deref().unwrap();
        assert!(content.contains("turned on"));
    }

    #[test]
    fn deser_empty_choices() {
        let json = fixture("empty_choices_response.json");
        let resp: OaiResponse = serde_json::from_str(&json).unwrap();
        assert!(resp.choices.is_empty());
    }

    #[test]
    fn deser_malformed_args() {
        let json = fixture("malformed_tool_args.json");
        let resp: OaiResponse = serde_json::from_str(&json).unwrap();
        let tc = resp.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("should have tool_calls");
        assert_eq!(tc[0].function.arguments, "{not valid json");
        // Verify it does NOT parse as valid JSON
        assert!(serde_json::from_str::<serde_json::Value>(&tc[0].function.arguments).is_err());
    }

    // -----------------------------------------------------------------------
    // C. End-to-end integration (wiremock)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn chat_basic_roundtrip() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("basic_chat_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".into(),
            }],
        }];

        let resp = provider.chat("You are helpful.", &messages, &[]).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(resp.text(), Some("Hello! How can I help you today?"));
    }

    #[tokio::test]
    async fn chat_tool_call_roundtrip() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("tool_call_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let tools = vec![ToolDefinition {
            name: "ha_control".into(),
            description: "Control HA".into(),
            input_schema: json!({"type": "object"}),
        }];
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Turn on the light".into(),
            }],
        }];

        let resp = provider.chat("You are helpful.", &messages, &tools).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        let tool_uses = resp.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].0, "call_abc123");
        assert_eq!(tool_uses[0].1, "ha_control");
        assert_eq!(tool_uses[0].2["entity_id"], "light.living_room");
    }

    #[tokio::test]
    async fn chat_multi_turn() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("multi_turn_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        // Simulate a multi-turn conversation with tool result
        let messages = vec![
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Turn on the light".into(),
                }],
            },
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_abc123".into(),
                    name: "ha_control".into(),
                    input: json!({"entity_id": "light.living_room", "action": "turn_on"}),
                }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_abc123".into(),
                    content: "success".into(),
                }],
            },
        ];

        let resp = provider.chat("You are helpful.", &messages, &[]).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert!(resp.text().unwrap().contains("turned on"));
    }

    #[tokio::test]
    async fn chat_auth_header() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("basic_chat_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        provider.chat("sys", &messages, &[]).await.unwrap();
        // If the header didn't match, wiremock would have returned 404
        // and the expect(1) ensures exactly one matching request was received
    }

    #[tokio::test]
    async fn chat_trailing_slash() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("basic_chat_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        // base_url with trailing slash
        let url = format!("{}/", mock_server.uri());
        let provider = test_provider(&url);
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        provider.chat("sys", &messages, &[]).await.unwrap();
    }

    #[tokio::test]
    async fn chat_api_error_4xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("error_response_429.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        let err = provider.chat("sys", &messages, &[]).await.unwrap_err();
        match err {
            GatewayError::Agent(msg) => {
                assert!(msg.contains("429"), "should mention 429: {msg}");
            }
            other => panic!("expected Agent error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_api_error_5xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("error_response_500.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        let err = provider.chat("sys", &messages, &[]).await.unwrap_err();
        match err {
            GatewayError::Agent(msg) => {
                assert!(msg.contains("500"), "should mention 500: {msg}");
            }
            other => panic!("expected Agent error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_empty_choices() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("empty_choices_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        let err = provider.chat("sys", &messages, &[]).await.unwrap_err();
        match err {
            GatewayError::Agent(msg) => {
                assert!(
                    msg.contains("empty choices"),
                    "should mention empty choices: {msg}"
                );
            }
            other => panic!("expected Agent error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_malformed_args() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("malformed_tool_args.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        let resp = provider.chat("sys", &messages, &[]).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        let tool_uses = resp.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        // Malformed JSON args → unwrap_or_default() → Value::Null
        assert!(tool_uses[0].2.is_null());
    }

    #[tokio::test]
    async fn chat_empty_system_prompt() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("basic_chat_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        // Empty system prompt → no system message in request
        let resp = provider.chat("", &messages, &[]).await.unwrap();
        assert!(resp.text().is_some());

        // Verify the request body did not contain a system message
        // by checking wiremock received requests
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let req_body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).unwrap();
        let msgs = req_body["messages"].as_array().unwrap();
        assert!(
            !msgs.iter().any(|m| m["role"] == "system"),
            "should not contain system message when system prompt is empty"
        );
    }

    #[tokio::test]
    async fn chat_tools_omitted_when_empty() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("basic_chat_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .mount(&mock_server)
            .await;

        let provider = test_provider(&mock_server.uri());
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hi".into(),
            }],
        }];

        // Empty tools slice → no "tools" key in request body
        provider.chat("sys", &messages, &[]).await.unwrap();

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let req_body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).unwrap();
        assert!(
            req_body.get("tools").is_none(),
            "should not contain 'tools' key when tools slice is empty"
        );
    }

    // -----------------------------------------------------------------------
    // D. Streaming tests (wiremock SSE)
    // -----------------------------------------------------------------------

    fn sse_text_response(text_chunks: &[&str]) -> String {
        let mut body = String::new();
        for chunk in text_chunks {
            let escaped = chunk.replace('\\', "\\\\").replace('"', "\\\"");
            body.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{escaped}\"}},\"finish_reason\":null}}]}}\n\n"
            ));
        }
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn sse_tool_call_response(tool_id: &str, tool_name: &str, args_chunks: &[&str]) -> String {
        let mut body = String::new();
        // First chunk: tool call id + name
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{tool_id}\",\"function\":{{\"name\":\"{tool_name}\",\"arguments\":\"\"}}}}]}},\"finish_reason\":null}}]}}\n\n"
        ));
        // Subsequent chunks: arguments fragments
        for chunk in args_chunks {
            let escaped = chunk.replace('\\', "\\\\").replace('"', "\\\"");
            body.push_str(&format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"{escaped}\"}}}}]}},\"finish_reason\":null}}]}}\n\n"
            ));
        }
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n");
        body.push_str("data: [DONE]\n\n");
        body
    }

    #[tokio::test]
    async fn stream_text_deltas() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(sse_text_response(&["Hello", " world"]), "text/event-stream"),
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

        assert_eq!(texts, vec!["Hello", " world"]);
        assert_eq!(done, Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn stream_tool_call() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        sse_tool_call_response("call_01", "get_weather", &[r#"{"city""#, r#":"Paris"}"#]),
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
        assert_eq!(tool_uses[0].0, "call_01");
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
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_raw(
                r#"{"error":"rate limited"}"#,
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
        assert!(result.is_err(), "expected error for 429 response");
        match result.err().unwrap() {
            GatewayError::Agent(msg) => assert!(msg.contains("429")),
            other => panic!("expected Agent error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn stream_length_finish_reason() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mut body = String::new();
        body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"trunc\"},\"finish_reason\":null}]}\n\n");
        body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n");
        body.push_str("data: [DONE]\n\n");

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
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

    // -----------------------------------------------------------------------
    // E. Local LLM integration tests (wiremock)
    // -----------------------------------------------------------------------

    /// Helper: build a provider simulating a local LLM with a dummy API key.
    fn local_llm_provider(base_url: &str, api_key: &str, model: &str) -> OpenAiCompatProvider {
        let config = ProviderConfig {
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: Some(2048),
            base_url: Some(base_url.into()),
        };
        OpenAiCompatProvider::new(reqwest::Client::new(), &config)
    }

    #[tokio::test]
    async fn local_llm_chat_roundtrip() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        let body = fixture("local_llm_response.json");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer ollama"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(body, "application/json"),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let provider = local_llm_provider(&mock_server.uri(), "ollama", "qwen2.5:14b");
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "你好".into(),
            }],
        }];

        let resp = provider.chat("You are helpful.", &messages, &[]).await.unwrap();
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert_eq!(
            resp.text(),
            Some("你好！我是本地运行的 Qwen 模型，有什么可以帮你的？")
        );
    }

    #[tokio::test]
    async fn local_llm_stream_roundtrip() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(
                    sse_text_response(&["你好", "！有什么", "可以帮你的？"]),
                    "text/event-stream",
                ),
            )
            .mount(&mock_server)
            .await;

        let provider = local_llm_provider(&mock_server.uri(), "ollama", "qwen2.5:14b");
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "你好".into(),
            }],
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

        assert_eq!(texts, vec!["你好", "！有什么", "可以帮你的？"]);
        assert_eq!(done, Some(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn local_llm_tool_call_roundtrip() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(
                    sse_tool_call_response(
                        "call_local_01",
                        "ha_control",
                        &[r#"{"entity_id""#, r#":"light.room","action":"turn_on"}"#],
                    ),
                    "text/event-stream",
                ),
            )
            .mount(&mock_server)
            .await;

        let provider = local_llm_provider(&mock_server.uri(), "not-needed", "qwen2.5:14b");
        let messages = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "开灯".into(),
            }],
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
        assert_eq!(tool_uses[0].0, "call_local_01");
        assert_eq!(tool_uses[0].1, "ha_control");
        assert_eq!(tool_uses[0].2["entity_id"], "light.room");
        assert_eq!(tool_uses[0].2["action"], "turn_on");
        assert_eq!(done, Some(StopReason::ToolUse));
    }
}
