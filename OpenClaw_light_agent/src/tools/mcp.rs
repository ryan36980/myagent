//! MCP (Model Context Protocol) client and proxy tool.
//!
//! Spawns MCP server processes, communicates over stdio using newline-delimited
//! JSON-RPC 2.0, and bridges each remote tool into the local [`Tool`] trait.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::Tool;
use crate::config::McpServerConfig;
use crate::error::{GatewayError, Result};

// ---------------------------------------------------------------------------
// JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[allow(dead_code)]
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// MCP tool definition (from tools/list)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

// ---------------------------------------------------------------------------
// McpClient
// ---------------------------------------------------------------------------

/// Client for a single MCP server process, communicating over stdin/stdout.
pub struct McpClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    timeout: Duration,
    max_output_bytes: usize,
}

impl McpClient {
    /// Spawn the MCP server and perform the initialize handshake.
    pub async fn start(cfg: &McpServerConfig) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| GatewayError::Tool {
            tool: "mcp".into(),
            message: format!("failed to spawn MCP server '{}': {e}", cfg.command),
        })?;

        let stdin = child.stdin.take().ok_or_else(|| GatewayError::Tool {
            tool: "mcp".into(),
            message: "MCP server stdin not available".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| GatewayError::Tool {
            tool: "mcp".into(),
            message: "MCP server stdout not available".into(),
        })?;

        let mut client = Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            next_id: 1,
            timeout: Duration::from_secs(cfg.timeout_secs),
            max_output_bytes: cfg.max_output_bytes,
        };

        // Initialize handshake
        client.initialize().await?;

        Ok(client)
    }

    /// Send the `initialize` request and `notifications/initialized`.
    async fn initialize(&mut self) -> Result<()> {
        let resp = self
            .request(
                "initialize",
                Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "openclaw-light",
                        "version": "0.1.0"
                    }
                })),
            )
            .await?;

        debug!(response = %resp, "MCP initialize response");

        // Send initialized notification (no id, no response expected)
        self.notify("notifications/initialized", None).await?;

        Ok(())
    }

    /// List tools provided by this MCP server.
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>> {
        let resp = self.request("tools/list", None).await?;

        let tools: Vec<McpToolDef> = resp["tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        Ok(tools)
    }

    /// Call a tool on the MCP server and return the text result.
    ///
    /// Output is truncated to `max_output_bytes` at a safe UTF-8 boundary.
    pub async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<String> {
        let resp = self
            .request(
                "tools/call",
                Some(json!({
                    "name": tool_name,
                    "arguments": arguments,
                })),
            )
            .await?;

        // Extract text from content array
        let text = resp["content"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        if item["type"].as_str() == Some("text") {
                            item["text"].as_str().map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| resp.to_string());

        // Truncate output to max_output_bytes at safe UTF-8 boundary
        Ok(truncate_mcp_output(&text, self.max_output_bytes))
    }

    /// Send a JSON-RPC request (with id) and wait for the response.
    ///
    /// The response read is wrapped in a timeout (configured via
    /// `McpServerConfig::timeout_secs`).
    async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(id),
            method: method.into(),
            params,
        };

        self.send(&req).await?;

        let resp = tokio::time::timeout(self.timeout, self.recv())
            .await
            .map_err(|_| {
                warn!(method, timeout_secs = self.timeout.as_secs(), "MCP request timed out");
                GatewayError::Tool {
                    tool: "mcp".into(),
                    message: format!(
                        "MCP request '{}' timed out after {}s",
                        method,
                        self.timeout.as_secs()
                    ),
                }
            })??;

        if let Some(err) = resp.error {
            return Err(GatewayError::Tool {
                tool: "mcp".into(),
                message: format!("JSON-RPC error {}: {}", err.code, err.message),
            });
        }

        Ok(resp.result.unwrap_or(Value::Null))
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: None,
            method: method.into(),
            params,
        };

        self.send(&req).await
    }

    /// Write a JSON-RPC message to stdin.
    async fn send(&mut self, req: &JsonRpcRequest) -> Result<()> {
        let mut line = serde_json::to_string(req)?;
        line.push('\n');

        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "mcp".into(),
                message: format!("failed to write to MCP stdin: {e}"),
            })?;

        self.stdin.flush().await.map_err(|e| GatewayError::Tool {
            tool: "mcp".into(),
            message: format!("failed to flush MCP stdin: {e}"),
        })?;

        Ok(())
    }

    /// Read a JSON-RPC response from stdout, skipping notifications.
    async fn recv(&mut self) -> Result<JsonRpcResponse> {
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|e| GatewayError::Tool {
                    tool: "mcp".into(),
                    message: format!("failed to read from MCP stdout: {e}"),
                })?;

            if n == 0 {
                return Err(GatewayError::Tool {
                    tool: "mcp".into(),
                    message: "MCP server closed stdout (EOF)".into(),
                });
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Try to parse as a response (has id field)
            let resp: JsonRpcResponse =
                serde_json::from_str(line).map_err(|e| GatewayError::Tool {
                    tool: "mcp".into(),
                    message: format!("invalid JSON-RPC from MCP: {e}"),
                })?;

            // Skip notifications (no id)
            if resp.id.is_some() {
                return Ok(resp);
            }
            // Otherwise it's a notification from the server — skip it
        }
    }

    /// Gracefully shut down the MCP server: close stdin, wait briefly, then
    /// kill if still alive.
    pub async fn shutdown(&mut self) {
        // Flush and close stdin to signal the server to exit
        let _ = self.stdin.flush().await;

        // Give the server 2 seconds to exit
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.child.wait(),
        )
        .await
        {
            Ok(Ok(status)) => {
                debug!(status = %status, "MCP server exited");
            }
            _ => {
                info!("MCP server did not exit in time, killing");
                let _ = self.child.kill().await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// McpProxyTool — bridges one MCP tool to the local Tool trait
// ---------------------------------------------------------------------------

/// Wraps a single MCP server tool as a local [`Tool`].
pub struct McpProxyTool {
    client: Arc<Mutex<McpClient>>,
    /// Full tool name: `mcp__{server}__{tool}`
    full_name: String,
    /// Original tool name on the MCP server.
    remote_name: String,
    description: String,
    schema: Value,
}

impl McpProxyTool {
    pub fn new(
        client: Arc<Mutex<McpClient>>,
        server_name: &str,
        tool_def: &McpToolDef,
    ) -> Self {
        let full_name = format!("mcp__{}__{}", server_name, tool_def.name);
        Self {
            client,
            full_name,
            remote_name: tool_def.name.clone(),
            description: tool_def
                .description
                .clone()
                .unwrap_or_else(|| format!("MCP tool: {}", tool_def.name)),
            schema: tool_def.input_schema.clone(),
        }
    }
}

#[async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        if self.schema.is_null() {
            json!({ "type": "object", "properties": {} })
        } else {
            self.schema.clone()
        }
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let mut client = self.client.lock().await;
        client.call_tool(&self.remote_name, input).await
    }
}

/// Format the proxy tool name for a given server and tool.
pub fn proxy_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// Truncate a string to at most `max_bytes`, cutting at a safe UTF-8 char boundary.
///
/// If truncation occurs a notice is appended.
fn truncate_mcp_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let pos = safe_truncate_pos(s, max_bytes);
    format!(
        "{}... [truncated, {} total bytes]",
        &s[..pos],
        s.len()
    )
}

/// Find the largest byte position <= `max` that lies on a UTF-8 character
/// boundary in `s`.
fn safe_truncate_pos(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut pos = max;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jsonrpc_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn parse_jsonrpc_error() {
        let json = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"method not found"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(2));
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "method not found");
    }

    #[test]
    fn format_tool_call_request() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(3),
            method: "tools/call".into(),
            params: Some(json!({
                "name": "get_weather",
                "arguments": { "city": "Tokyo" }
            })),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("tools/call"));
        assert!(s.contains("get_weather"));
        assert!(s.contains("Tokyo"));
    }

    #[test]
    fn mcp_config_serde() {
        let json5 = r#"{
            "servers": {
                "weather": {
                    "command": "python3",
                    "args": ["server.py"],
                    "env": { "API_KEY": "test123" }
                }
            }
        }"#;
        let cfg: crate::config::McpConfig = serde_json::from_str(json5).unwrap();
        assert_eq!(cfg.servers.len(), 1);
        let weather = &cfg.servers["weather"];
        assert_eq!(weather.command, "python3");
        assert_eq!(weather.args, vec!["server.py"]);
        assert_eq!(weather.env.get("API_KEY").unwrap(), "test123");
    }

    #[test]
    fn mcp_config_empty_default() {
        let cfg = crate::config::McpConfig::default();
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn proxy_tool_name_format() {
        assert_eq!(proxy_tool_name("weather", "get_forecast"), "mcp__weather__get_forecast");
        assert_eq!(proxy_tool_name("ha", "toggle_light"), "mcp__ha__toggle_light");
    }

    #[test]
    fn truncate_mcp_output_within_limit() {
        let s = "hello world";
        assert_eq!(truncate_mcp_output(s, 100), "hello world");
    }

    #[test]
    fn truncate_mcp_output_exact_limit() {
        let s = "hello";
        assert_eq!(truncate_mcp_output(s, 5), "hello");
    }

    #[test]
    fn truncate_mcp_output_exceeds_limit() {
        let s = "hello world, this is a long string";
        let result = truncate_mcp_output(s, 11);
        assert!(result.starts_with("hello world"));
        assert!(result.contains("truncated"));
        assert!(result.contains(&format!("{}", s.len())));
    }

    #[test]
    fn truncate_mcp_output_multibyte_utf8() {
        // "こんにちは" — each char is 3 bytes, total 15 bytes
        let s = "こんにちは";
        assert_eq!(s.len(), 15);
        // Truncate at 7 bytes — should cut to 6 (2 full chars)
        let result = truncate_mcp_output(s, 7);
        assert!(result.starts_with("こん"));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn safe_truncate_pos_on_boundary() {
        let s = "abc";
        assert_eq!(safe_truncate_pos(s, 2), 2);
    }

    #[test]
    fn safe_truncate_pos_mid_multibyte() {
        let s = "aé"; // 'a' = 1 byte, 'é' = 2 bytes, total 3
        assert_eq!(safe_truncate_pos(s, 2), 1); // back up to char boundary
    }
}
