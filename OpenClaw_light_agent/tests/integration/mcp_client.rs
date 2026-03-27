//! Integration tests for the MCP client against a real (mock) MCP server.

use openclaw_light::config::McpServerConfig;
use openclaw_light::tools::mcp::McpClient;
use serde_json::json;
use std::collections::HashMap;

fn mock_server_config() -> McpServerConfig {
    McpServerConfig {
        command: "python3".into(),
        args: vec![format!(
            "{}/tests/fixtures/mock_mcp_server.py",
            env!("CARGO_MANIFEST_DIR")
        )],
        env: HashMap::new(),
        timeout_secs: 10,
        max_output_bytes: 65536,
    }
}

#[tokio::test]
async fn mcp_initialize_and_list_tools() {
    let cfg = mock_server_config();
    let mut client = McpClient::start(&cfg).await.unwrap();

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 2);

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"add"));

    client.shutdown().await;
}

#[tokio::test]
async fn mcp_call_tool_echo() {
    let cfg = mock_server_config();
    let mut client = McpClient::start(&cfg).await.unwrap();

    let result = client
        .call_tool("echo", json!({"text": "hello world"}))
        .await
        .unwrap();
    assert_eq!(result, "hello world");

    client.shutdown().await;
}

#[tokio::test]
async fn mcp_call_tool_add() {
    let cfg = mock_server_config();
    let mut client = McpClient::start(&cfg).await.unwrap();

    let result = client
        .call_tool("add", json!({"a": 3, "b": 7}))
        .await
        .unwrap();
    assert_eq!(result, "10");

    client.shutdown().await;
}

#[tokio::test]
async fn mcp_shutdown_graceful() {
    let cfg = mock_server_config();
    let mut client = McpClient::start(&cfg).await.unwrap();

    // Verify it works first
    let _ = client.list_tools().await.unwrap();

    // Shutdown should complete without hanging
    client.shutdown().await;
}
