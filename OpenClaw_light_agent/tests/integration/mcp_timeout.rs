//! Integration test for MCP client timeout behavior.

use openclaw_light::config::McpServerConfig;
use openclaw_light::tools::mcp::McpClient;
use std::collections::HashMap;
use std::time::Instant;

#[tokio::test]
async fn mcp_call_tool_timeout() {
    // Use the slow MCP server that sleeps 5 seconds on tools/call
    let fixture_path = format!(
        "{}/tests/fixtures/slow_mcp_server.py",
        env!("CARGO_MANIFEST_DIR")
    );

    let config = McpServerConfig {
        command: "python3".into(),
        args: vec![fixture_path],
        env: HashMap::new(),
        timeout_secs: 2, // 2 second timeout, server sleeps 5
        max_output_bytes: 65536,
    };

    let mut client = McpClient::start(&config)
        .await
        .expect("failed to start slow MCP server");

    // list_tools should work (no delay in the slow server)
    let tools = client.list_tools().await.expect("list_tools failed");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "slow_echo");

    // call_tool should timeout because the server sleeps 5s but timeout is 2s
    let start = Instant::now();
    let result = client
        .call_tool("slow_echo", serde_json::json!({"text": "hello"}))
        .await;
    let elapsed = start.elapsed();

    // Should have timed out in ~2 seconds, not waited the full 5
    assert!(
        elapsed.as_secs() < 4,
        "should timeout before 4s, took {:?}",
        elapsed
    );
    assert!(result.is_err(), "expected timeout error");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("timed out") || err_msg.contains("timeout"),
        "error should mention timeout: {err_msg}"
    );
}
