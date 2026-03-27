//! HTTP GET tool for fetching web content.
//!
//! HTML pages are automatically converted to plain text (scripts/styles/tags
//! removed). Supports pagination via `offset` and `max_chars` for long pages.
//! Download size is capped at a configurable limit (default 2 MB).

use std::net::IpAddr;

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::html_utils;
use super::Tool;
use crate::error::{GatewayError, Result};

/// Check if an IP address is in a blocked (private/reserved) range.
///
/// Blocks: loopback (127.x), private (10.x, 172.16-31.x, 192.168.x),
/// link-local (169.254.x), IPv6 loopback (::1), IPv6 link-local (fe80::/10),
/// IPv6 unique-local (fc00::/7), and IPv4-mapped IPv6.
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8
            octets[0] == 127
            // 10.0.0.0/8
            || octets[0] == 10
            // 172.16.0.0/12
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16
            || (octets[0] == 192 && octets[1] == 168)
            // 169.254.0.0/16 (link-local, includes AWS metadata 169.254.169.254)
            || (octets[0] == 169 && octets[1] == 254)
            // 0.0.0.0/8
            || octets[0] == 0
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            // ::1
            v6.is_loopback()
            // fe80::/10
            || (segments[0] & 0xffc0) == 0xfe80
            // fc00::/7
            || (segments[0] & 0xfe00) == 0xfc00
            // ::ffff:0:0/96 (IPv4-mapped) — check the mapped IPv4
            || if let Some(v4) = v6.to_ipv4_mapped() {
                is_blocked_ip(&IpAddr::V4(v4))
            } else {
                false
            }
            // :: (unspecified)
            || v6.is_unspecified()
        }
    }
}

/// Validate a URL for SSRF safety: parse, resolve DNS, check all resolved IPs.
///
/// Returns `Ok(())` if the URL is safe to fetch, or an error describing why
/// the request was blocked.
pub async fn validate_url_ssrf(url_str: &str) -> Result<()> {
    let parsed = url::Url::parse(url_str).map_err(|e| GatewayError::Tool {
        tool: "web_fetch".into(),
        message: format!("invalid URL: {e}"),
    })?;

    // Only allow http/https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(GatewayError::Tool {
                tool: "web_fetch".into(),
                message: format!("blocked scheme: {scheme}"),
            });
        }
    }

    let host = parsed.host_str().ok_or_else(|| GatewayError::Tool {
        tool: "web_fetch".into(),
        message: "URL has no host".into(),
    })?;

    // Try parsing host as IP directly
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(&ip) {
            return Err(GatewayError::Tool {
                tool: "web_fetch".into(),
                message: format!("blocked: {} resolves to private/reserved IP", host),
            });
        }
        return Ok(());
    }

    // DNS resolution — check all resolved addresses
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addr = format!("{}:{}", host, port);
    let resolved = tokio::net::lookup_host(&addr).await.map_err(|e| GatewayError::Tool {
        tool: "web_fetch".into(),
        message: format!("DNS resolution failed for {}: {e}", host),
    })?;

    let mut found_any = false;
    for socket_addr in resolved {
        found_any = true;
        if is_blocked_ip(&socket_addr.ip()) {
            return Err(GatewayError::Tool {
                tool: "web_fetch".into(),
                message: format!(
                    "blocked: {} resolves to private/reserved IP {}",
                    host,
                    socket_addr.ip()
                ),
            });
        }
    }

    if !found_any {
        return Err(GatewayError::Tool {
            tool: "web_fetch".into(),
            message: format!("DNS resolution returned no results for {}", host),
        });
    }

    Ok(())
}

/// UTF-8 safe character slicing: find the byte index of the Nth character.
fn char_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

pub struct WebFetchTool {
    client: reqwest::Client,
    max_download_bytes: usize,
}

impl WebFetchTool {
    pub fn new(client: reqwest::Client, max_download_bytes: usize) -> Self {
        Self {
            client,
            max_download_bytes,
        }
    }

    /// Inner fetch logic without SSRF validation (used by tests with localhost mock servers).
    async fn fetch_inner(&self, input: serde_json::Value) -> Result<String> {
        let url = input["url"].as_str().ok_or_else(|| GatewayError::Tool {
            tool: "web_fetch".into(),
            message: "url is required".into(),
        })?;

        debug!(url, "fetching URL");

        let mut req = self.client.get(url);

        // Add custom headers if provided
        if let Some(headers) = input.get("headers").and_then(|h| h.as_object()) {
            for (key, value) in headers {
                if let Some(v) = value.as_str() {
                    req = req.header(key.as_str(), v);
                }
            }
        }

        let resp = req
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "web_fetch".into(),
                message: e.to_string(),
            })?;

        let status = resp.status();

        // Read Content-Type before consuming the body
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        // Read body with download size limit
        let body_bytes = resp.bytes().await.map_err(|e| GatewayError::Tool {
            tool: "web_fetch".into(),
            message: e.to_string(),
        })?;

        if body_bytes.len() > self.max_download_bytes {
            return Ok(format!(
                "Error: Response too large ({} bytes, limit {} bytes). \
                 Try a more specific URL or use an API endpoint instead.",
                body_bytes.len(),
                self.max_download_bytes
            ));
        }

        let body = String::from_utf8_lossy(&body_bytes).into_owned();

        // Convert HTML to plain text; leave JSON/XML/plain text as-is
        let text = if content_type.contains("text/html") {
            html_utils::html_to_text(&body)
        } else {
            body
        };

        // Pagination: offset and max_chars
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let max_chars = input["max_chars"]
            .as_u64()
            .map(|v| (v as usize).min(256_000))
            .unwrap_or(128_000);

        let total_chars = text.chars().count();
        let start = offset.min(total_chars);
        let end = (start + max_chars).min(total_chars);

        // UTF-8 safe slicing
        let start_byte = char_byte_offset(&text, start);
        let end_byte = char_byte_offset(&text, end);
        let slice = &text[start_byte..end_byte];

        let result = if !status.is_success() {
            if end < total_chars {
                format!(
                    "HTTP {}: {}\n\n[Showing chars {}-{} of {}. Use offset={} to read more.]",
                    status, slice, start, end, total_chars, end
                )
            } else {
                format!("HTTP {}: {}", status, slice)
            }
        } else if end < total_chars {
            format!(
                "{}\n\n[Showing chars {}-{} of {}. Use offset={} to read more.]",
                slice, start, end, total_chars, end
            )
        } else if start > 0 {
            format!(
                "{}\n\n[Showing chars {}-{} of {} (end).]",
                slice, start, total_chars, total_chars
            )
        } else {
            slice.to_string()
        };

        Ok(result)
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL via HTTP GET. HTML pages are automatically converted to plain text \
         (scripts/styles/tags removed). Supports pagination for long pages via offset and max_chars. \
         Default: first 128000 chars. Use offset to read subsequent sections."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers"
                },
                "offset": {
                    "type": "integer",
                    "description": "Character offset to start reading from (default: 0)"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 128000, max: 256000)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let url = input["url"].as_str().ok_or_else(|| GatewayError::Tool {
            tool: "web_fetch".into(),
            message: "url is required".into(),
        })?;

        // SSRF protection: validate URL before fetching
        validate_url_ssrf(url).await?;

        self.fetch_inner(input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default max_download_bytes for tests (2 MB).
    const TEST_MAX_DOWNLOAD: usize = 2_000_000;

    #[test]
    fn tool_metadata() {
        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        assert_eq!(tool.name(), "web_fetch");
        assert!(!tool.description().is_empty());
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("url")));
        // Verify new params are in schema
        assert!(schema["properties"]["offset"].is_object());
        assert!(schema["properties"]["max_chars"].is_object());
    }

    #[tokio::test]
    async fn missing_url_returns_error() {
        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("url is required"));
    }

    #[tokio::test]
    async fn fetch_success() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/data"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("hello world"))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool
            .fetch_inner(json!({"url": format!("{}/data", server.uri())}))
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn fetch_error_status() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/fail"))
            .respond_with(wiremock::ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool
            .fetch_inner(json!({"url": format!("{}/fail", server.uri())}))
            .await
            .unwrap();
        assert!(result.contains("404"), "should contain status: {result}");
    }

    #[tokio::test]
    async fn fetch_truncates_large_response() {
        let server = wiremock::MockServer::start().await;
        let big_body = "x".repeat(200_000);
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/big"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(&big_body))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool
            .fetch_inner(json!({"url": format!("{}/big", server.uri())}))
            .await
            .unwrap();
        assert!(result.contains("Use offset="));
        assert!(result.len() < big_body.len());
    }

    #[tokio::test]
    async fn fetch_html_page_converts_to_text() {
        let server = wiremock::MockServer::start().await;
        let html = r#"<html><head><style>body{color:red}</style>
            <script>alert('xss')</script></head>
            <body><p>Hello world</p></body></html>"#;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/page"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(html, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool
            .fetch_inner(json!({"url": format!("{}/page", server.uri())}))
            .await
            .unwrap();
        assert!(result.contains("Hello world"));
        assert!(!result.contains("<script>"));
        assert!(!result.contains("alert"));
        assert!(!result.contains("color:red"));
    }

    #[tokio::test]
    async fn fetch_json_api_no_conversion() {
        let server = wiremock::MockServer::start().await;
        let json_body = r#"{"key": "<b>value</b>"}"#;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_raw(json_body, "application/json"),
            )
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool
            .fetch_inner(json!({"url": format!("{}/api", server.uri())}))
            .await
            .unwrap();
        // JSON should NOT have HTML tags stripped
        assert!(result.contains("<b>value</b>"));
    }

    #[tokio::test]
    async fn fetch_with_offset_pagination() {
        let server = wiremock::MockServer::start().await;
        let body = "abcdefghij"; // 10 chars
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/page"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);

        // First page: offset=0, max_chars=5 → "abcde" + pagination hint
        let result = tool
            .fetch_inner(json!({
                "url": format!("{}/page", server.uri()),
                "offset": 0,
                "max_chars": 5
            }))
            .await
            .unwrap();
        assert!(result.contains("abcde"));
        assert!(result.contains("Use offset=5"));

        // Second page: offset=5, max_chars=5 → "fghij" (end)
        let result = tool
            .fetch_inner(json!({
                "url": format!("{}/page", server.uri()),
                "offset": 5,
                "max_chars": 5
            }))
            .await
            .unwrap();
        assert!(result.contains("fghij"));
        assert!(result.contains("(end)"));
    }

    #[tokio::test]
    async fn fetch_rejects_oversized_response() {
        let server = wiremock::MockServer::start().await;
        // Use a small limit for testing
        let small_limit = 100;
        let big_body = "x".repeat(200);
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/huge"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(&big_body))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), small_limit);
        let result = tool
            .fetch_inner(json!({"url": format!("{}/huge", server.uri())}))
            .await
            .unwrap();
        assert!(result.contains("too large"));
    }

    #[test]
    fn ssrf_blocked_ips() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        // Loopback
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3))));
        // Private 10.x
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        // Private 172.16-31.x
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1))));
        // Private 192.168.x
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        // Link-local (AWS metadata)
        assert!(is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))));
        // Public
        assert!(!is_blocked_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        // IPv6 loopback
        assert!(is_blocked_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
        // IPv6 unspecified
        assert!(is_blocked_ip(&IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
    }

    #[tokio::test]
    async fn ssrf_blocks_private_url() {
        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool.execute(json!({"url": "http://127.0.0.1/admin"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn ssrf_blocks_metadata_endpoint() {
        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool.execute(json!({"url": "http://169.254.169.254/latest/meta-data/"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[tokio::test]
    async fn ssrf_blocks_bad_scheme() {
        let result = validate_url_ssrf("file:///etc/passwd").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("scheme"));
    }

    #[tokio::test]
    async fn fetch_with_custom_headers() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/auth"))
            .and(wiremock::matchers::header("X-Custom", "test-value"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let tool = WebFetchTool::new(reqwest::Client::new(), TEST_MAX_DOWNLOAD);
        let result = tool
            .fetch_inner(json!({
                "url": format!("{}/auth", server.uri()),
                "headers": {"X-Custom": "test-value"}
            }))
            .await
            .unwrap();
        assert_eq!(result, "ok");
    }
}
