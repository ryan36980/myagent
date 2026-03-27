//! Integration tests for the HTTP API channel.

use openclaw_light::channel::http_api::HttpApiChannel;
use openclaw_light::channel::Channel;
use openclaw_light::config::HttpApiConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Find a free port by binding to port 0.
async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// Create a channel with no auth and no session_dir.
async fn make_channel(port: u16) -> HttpApiChannel {
    let config = HttpApiConfig {
        enabled: true,
        listen: format!("127.0.0.1:{port}"),
        auth_token: String::new(),
    };
    HttpApiChannel::new(&config, None).await.unwrap()
}

/// Create a channel with auth token.
async fn make_channel_with_auth(port: u16, token: &str) -> HttpApiChannel {
    let config = HttpApiConfig {
        enabled: true,
        listen: format!("127.0.0.1:{port}"),
        auth_token: token.to_string(),
    };
    HttpApiChannel::new(&config, None).await.unwrap()
}

#[tokio::test]
async fn post_chat_returns_message() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    // Send a POST /chat request in a background task
    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"test_chat","text":"hello"}"#;
        let request = format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read response
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    // Poll should return the incoming message
    let messages = channel.poll().await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].chat_id, "test_chat");
    match &messages[0].content {
        openclaw_light::channel::types::MessageContent::Text(t) => assert_eq!(t, "hello"),
        _ => panic!("expected text content"),
    }

    // Send response back
    channel.send_text("test_chat", "world").await.unwrap();

    // Wait for client to receive the response
    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("world"));
}

#[tokio::test]
async fn non_post_returns_404() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    // Poll returns empty (404 sent, no message produced)
    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("404"));
}

#[tokio::test]
async fn invalid_json_returns_400() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = "not valid json";
        let request = format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("400"));
}

#[tokio::test]
async fn channel_id_is_http_api() {
    let port = free_port().await;
    let channel = make_channel(port).await;
    assert_eq!(channel.id(), "http_api");
}

// ── New tests: HTML, SSE, CORS, History, Auth ─────────────────────────────

#[tokio::test]
async fn get_root_returns_html() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("text/html"));
    assert!(response.contains("OpenClaw"));
}

#[tokio::test]
async fn options_returns_cors_headers() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "OPTIONS /chat/stream HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("204 No Content"));
    assert!(response.contains("Access-Control-Allow-Origin: *"));
    assert!(response.contains("Access-Control-Allow-Methods:"));
}

#[tokio::test]
async fn post_chat_stream_returns_sse_headers() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"sse_test","text":"hi"}"#;
        let request = format!(
            "POST /chat/stream HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read SSE header + initial events
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    // Poll should return the incoming message
    let messages = channel.poll().await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].chat_id, "sse_test");

    // Send response through SSE channel
    let msg_id = channel.send_text("sse_test", "Hello from SSE").await.unwrap();
    assert!(msg_id.starts_with("sse_"));

    // Close the stream
    channel.close_stream("sse_test").await.unwrap();

    // Wait for client to receive
    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("text/event-stream"));
}

#[tokio::test]
async fn sse_receives_delta_and_done_events() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"sse_events","text":"test"}"#;
        let request = format!(
            "POST /chat/stream HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read all data until connection closes
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stream.read(&mut buf),
            )
            .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => all.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => break,
                Err(_) => break, // timeout
            }
        }
        String::from_utf8_lossy(&all).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert_eq!(messages.len(), 1);

    // Simulate agent response: send_text → edit_message → close_stream
    let msg_id = channel
        .send_text("sse_events", "Hello")
        .await
        .unwrap();
    assert!(msg_id.starts_with("sse_"));

    // Edit with more text (StreamingWriter sends full buffer)
    channel
        .edit_message("sse_events", &msg_id, "Hello world")
        .await
        .unwrap();

    // Close stream
    channel.close_stream("sse_events").await.unwrap();

    // Small delay for the background writer task to flush
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let response = client_handle.await.unwrap();
    assert!(response.contains("text/event-stream"), "should be SSE content type");
    assert!(response.contains(r#""type":"delta""#), "should contain delta event");
    assert!(response.contains(r#""type":"done""#), "should contain done event");
}

#[tokio::test]
async fn get_chat_history_empty() {
    // With no session_dir, history returns []
    let port = free_port().await;
    let config = HttpApiConfig {
        enabled: true,
        listen: format!("127.0.0.1:{port}"),
        auth_token: String::new(),
    };
    let channel = HttpApiChannel::new(&config, None).await.unwrap();

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "GET /chat/history?chat_id=test HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("[]"));
}

#[tokio::test]
async fn get_chat_history_with_session_data() {
    let tmp = tempfile::tempdir().unwrap();
    let session_dir = tmp.path().to_str().unwrap().to_string();

    // Write a session JSONL file
    let session_file = format!("{}/http_api_test_chat.jsonl", session_dir);
    let content = concat!(
        r#"{"role":"user","content":[{"type":"text","text":"hello"}]}"#, "\n",
        r#"{"role":"assistant","content":[{"type":"text","text":"hi there"}]}"#, "\n",
        r#"{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"test","input":{}}]}"#, "\n",
    );
    tokio::fs::write(&session_file, content).await.unwrap();

    let port = free_port().await;
    let config = HttpApiConfig {
        enabled: true,
        listen: format!("127.0.0.1:{port}"),
        auth_token: String::new(),
    };
    let channel = HttpApiChannel::new(&config, Some(session_dir)).await.unwrap();

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request =
            "GET /chat/history?chat_id=test_chat HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
    // Should contain user and assistant text entries, but NOT tool_use
    assert!(response.contains("hello"));
    assert!(response.contains("hi there"));
    assert!(!response.contains("tool_use"));
}

#[tokio::test]
async fn auth_no_token_passes() {
    // When auth_token is empty, requests should pass without Authorization header
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"auth_test","text":"hi"}"#;
        let request = format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert_eq!(messages.len(), 1); // Should pass through

    channel.send_text("auth_test", "ok").await.unwrap();
    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
}

#[tokio::test]
async fn auth_required_returns_401_without_token() {
    let port = free_port().await;
    let channel = make_channel_with_auth(port, "secret123").await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"auth_test","text":"hi"}"#;
        let request = format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty()); // Should be rejected

    let response = client_handle.await.unwrap();
    assert!(response.contains("401"));
}

#[tokio::test]
async fn auth_required_passes_with_correct_token() {
    let port = free_port().await;
    let channel = make_channel_with_auth(port, "secret123").await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"auth_test","text":"hi"}"#;
        let request = format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\n\
             Authorization: Bearer secret123\r\n\
             Content-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert_eq!(messages.len(), 1); // Should pass through

    channel.send_text("auth_test", "ok").await.unwrap();
    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
}

#[tokio::test]
async fn get_root_no_auth_required() {
    // GET / should work even when auth is configured
    let port = free_port().await;
    let channel = make_channel_with_auth(port, "secret123").await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("200 OK"));
    assert!(response.contains("text/html"));
}

#[tokio::test]
async fn responses_include_cors_headers() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "GET /nonexistent HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let _messages = channel.poll().await.unwrap();
    let response = client_handle.await.unwrap();
    assert!(
        response.contains("Access-Control-Allow-Origin: *"),
        "404 response should include CORS headers"
    );
}

#[tokio::test]
async fn auth_wrong_token_returns_401() {
    let port = free_port().await;
    let channel = make_channel_with_auth(port, "correct_token").await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"t","text":"hi"}"#;
        let request = format!(
            "POST /chat HTTP/1.1\r\nHost: localhost\r\n\
             Authorization: Bearer wrong_token\r\n\
             Content-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("401"), "wrong token should be rejected");
}

#[tokio::test]
async fn auth_protects_chat_stream() {
    let port = free_port().await;
    let channel = make_channel_with_auth(port, "secret").await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"t","text":"hi"}"#;
        let request = format!(
            "POST /chat/stream HTTP/1.1\r\nHost: localhost\r\n\
             Content-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("401"), "/chat/stream should require auth");
}

#[tokio::test]
async fn auth_protects_chat_history() {
    let port = free_port().await;
    let channel = make_channel_with_auth(port, "secret").await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request =
            "GET /chat/history?chat_id=test HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("401"), "/chat/history should require auth");
}

#[tokio::test]
async fn history_missing_chat_id_returns_400() {
    let port = free_port().await;
    let config = HttpApiConfig {
        enabled: true,
        listen: format!("127.0.0.1:{port}"),
        auth_token: String::new(),
    };
    let channel = HttpApiChannel::new(&config, Some("/tmp".into())).await.unwrap();

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let request = "GET /chat/history HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert!(messages.is_empty());

    let response = client_handle.await.unwrap();
    assert!(response.contains("400"), "missing chat_id should return 400");
}

#[tokio::test]
async fn sse_typing_event() {
    let port = free_port().await;
    let channel = make_channel(port).await;

    let addr = format!("127.0.0.1:{port}");
    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        let body = r#"{"chat_id":"typing_test","text":"test"}"#;
        let request = format!(
            "POST /chat/stream HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stream.read(&mut buf),
            )
            .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => all.extend_from_slice(&buf[..n]),
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&all).to_string()
    });

    let messages = channel.poll().await.unwrap();
    assert_eq!(messages.len(), 1);

    // Send typing → delta → done
    channel.send_typing("typing_test").await.unwrap();
    let msg_id = channel.send_text("typing_test", "hi").await.unwrap();
    assert!(msg_id.starts_with("sse_"));
    channel.close_stream("typing_test").await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let response = client_handle.await.unwrap();
    assert!(response.contains(r#""type":"typing""#), "should contain typing event");
    assert!(response.contains(r#""type":"delta""#), "should contain delta event");
    assert!(response.contains(r#""type":"done""#), "should contain done event");
}
