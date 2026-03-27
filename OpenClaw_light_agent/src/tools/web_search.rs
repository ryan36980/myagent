//! Web search tool using DuckDuckGo HTML search.
//!
//! Parses search results from `https://html.duckduckgo.com/html/` using simple
//! string matching — no HTML parser dependency required.

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::html_utils::{decode_html_entities, strip_html_tags};
use super::Tool;
use crate::error::{GatewayError, Result};

/// Maximum results the tool will return.
const MAX_RESULTS_CAP: usize = 10;

pub struct WebSearchTool {
    client: reqwest::Client,
    default_max_results: usize,
    search_url: String,
    /// "brave" or "duckduckgo"
    provider: String,
    /// Brave Search API key (empty if not configured)
    brave_api_key: String,
}

impl WebSearchTool {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            default_max_results: 5,
            search_url: "https://html.duckduckgo.com/html/".into(),
            provider: "duckduckgo".into(),
            brave_api_key: String::new(),
        }
    }

    /// Create from config with provider selection.
    pub fn from_config(
        client: reqwest::Client,
        config: &crate::config::WebSearchConfig,
    ) -> Self {
        let brave_api_key = if config.provider == "brave" {
            std::env::var(&config.api_key_env).unwrap_or_default()
        } else {
            String::new()
        };
        Self {
            client,
            default_max_results: config.max_results.min(MAX_RESULTS_CAP),
            search_url: "https://html.duckduckgo.com/html/".into(),
            provider: config.provider.clone(),
            brave_api_key,
        }
    }

    /// Create a WebSearchTool with a custom search URL (for testing with wiremock).
    pub fn new_with_url(client: reqwest::Client, url: String) -> Self {
        Self {
            client,
            default_max_results: 5,
            search_url: url,
            provider: "duckduckgo".into(),
            brave_api_key: String::new(),
        }
    }

    /// Search using DuckDuckGo HTML scraping.
    async fn ddg_search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        debug!(query, max_results, "searching DuckDuckGo");

        let resp = self
            .client
            .post(&self.search_url)
            .form(&[("q", query)])
            .header("User-Agent", "OpenClaw-Light/0.1")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "web_search".into(),
                message: format!("search request failed: {e}"),
            })?;

        let html = resp.text().await.map_err(|e| GatewayError::Tool {
            tool: "web_search".into(),
            message: format!("failed to read response: {e}"),
        })?;

        Ok(parse_ddg_html(&html, max_results))
    }

    /// Search using Brave Search API (JSON).
    async fn brave_search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>> {
        debug!(query, max_results, "searching Brave");

        let resp = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .query(&[("q", query), ("count", &max_results.to_string())])
            .header("Accept", "application/json")
            .header("Accept-Encoding", "gzip")
            .header("X-Subscription-Token", &self.brave_api_key)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "web_search".into(),
                message: format!("Brave search request failed: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Tool {
                tool: "web_search".into(),
                message: format!("Brave API returned {}: {}", status, body),
            });
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| GatewayError::Tool {
            tool: "web_search".into(),
            message: format!("failed to parse Brave response: {e}"),
        })?;

        let mut results = Vec::new();
        if let Some(web_results) = json["web"]["results"].as_array() {
            for item in web_results.iter().take(max_results) {
                let title = item["title"].as_str().unwrap_or("").to_string();
                let url = item["url"].as_str().unwrap_or("").to_string();
                let snippet = item["description"].as_str().unwrap_or("").to_string();
                if !url.is_empty() {
                    results.push(SearchResult {
                        title,
                        url,
                        snippet,
                    });
                }
            }
        }

        Ok(results)
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo. Returns titles, URLs, and snippets. \
         Useful for finding current information, documentation, news, or answers."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 5, max: 10)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "web_search".into(),
                message: "query is required".into(),
            })?;

        if query.trim().is_empty() {
            return Err(GatewayError::Tool {
                tool: "web_search".into(),
                message: "query must not be empty".into(),
            });
        }

        let max_results = input["max_results"]
            .as_u64()
            .map(|n| (n as usize).min(MAX_RESULTS_CAP))
            .unwrap_or(self.default_max_results);

        // Dispatch to the configured provider
        let results = if self.provider == "brave" && !self.brave_api_key.is_empty() {
            match self.brave_search(query, max_results).await {
                Ok(r) => r,
                Err(e) => {
                    // Fallback to DDG on Brave failure
                    debug!(error = %e, "Brave search failed, falling back to DuckDuckGo");
                    self.ddg_search(query, max_results).await?
                }
            }
        } else {
            self.ddg_search(query, max_results).await?
        };

        Ok(format_results(query, &results))
    }
}

/// A single search result.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Parse DuckDuckGo HTML search results using string matching.
///
/// Looks for `<a class="result__a"` for title+URL and
/// `<a class="result__snippet"` for the snippet text.
pub fn parse_ddg_html(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Split on result blocks — each result lives inside a div with class "result"
    // We look for result__a links (title + URL) and result__snippet (description)
    let mut pos = 0;

    while results.len() < max_results {
        // Find the next result link
        let anchor_marker = "class=\"result__a\"";
        let anchor_pos = match html[pos..].find(anchor_marker) {
            Some(p) => pos + p,
            None => break,
        };

        // Find the full <a ...> tag that contains this class attribute.
        // tag_start = the '<' before the marker; tag_close = the '>' after the marker.
        let tag_start = html[..anchor_pos].rfind('<').unwrap_or(anchor_pos);
        let after_marker = anchor_pos + anchor_marker.len();
        let tag_close = html[after_marker..]
            .find('>')
            .map(|p| after_marker + p + 1)
            .unwrap_or(after_marker);

        // Extract href from the full opening tag
        let tag = &html[tag_start..tag_close];
        let url = extract_href_from_tag(tag);

        // Extract title text (content between tag_close and the next </a>)
        let title = match html[tag_close..].find("</") {
            Some(end) => {
                let raw = &html[tag_close..tag_close + end];
                strip_html_tags(raw).trim().to_string()
            }
            None => String::new(),
        };

        // Look for snippet near this result
        let snippet_marker = "class=\"result__snippet\"";
        let mut search_end = (anchor_pos + 2000).min(html.len());
        // Ensure we don't slice in the middle of a multi-byte UTF-8 character
        while search_end < html.len() && !html.is_char_boundary(search_end) {
            search_end += 1;
        }
        let snippet = if let Some(sp) = html[anchor_pos..search_end].find(snippet_marker) {
            let snippet_text = extract_tag_text(&html[anchor_pos + sp..]);
            decode_html_entities(&snippet_text)
        } else {
            String::new()
        };

        let url = decode_html_entities(&url);
        let title = decode_html_entities(&title);

        // Only include results with valid URLs
        if !url.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        pos = tag_close;
    }

    results
}

/// Extract the href value from an anchor tag string like `<a ... href="URL" ...>`.
fn extract_href_from_tag(tag: &str) -> String {
    if let Some(href_pos) = tag.find("href=\"") {
        let url_start = href_pos + 6;
        if let Some(url_end) = tag[url_start..].find('"') {
            return tag[url_start..url_start + url_end].to_string();
        }
    }
    String::new()
}

/// Extract text content from the first `>...</` sequence.
fn extract_tag_text(html: &str) -> String {
    // Find the closing > of the opening tag
    let start = match html.find('>') {
        Some(p) => p + 1,
        None => return String::new(),
    };

    // Find the next closing tag
    let end = match html[start..].find("</") {
        Some(p) => start + p,
        None => return String::new(),
    };

    let text = &html[start..end];
    // Strip any inner HTML tags
    strip_html_tags(text).trim().to_string()
}

/// Format search results into a human-readable string.
pub fn format_results(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!("Search: \"{query}\"\n\nNo results found.");
    }

    let mut out = format!("Search: \"{query}\"\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("\n{}. {}", i + 1, r.title));
        if !r.url.is_empty() {
            out.push_str(&format!(" — {}", r.url));
        }
        if !r.snippet.is_empty() {
            out.push_str(&format!("\n   {}", r.snippet));
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ddg_html_results() {
        let html = r#"
        <div class="result">
          <a rel="nofollow" class="result__a" href="https://example.com/page1">Example Page 1</a>
          <a class="result__snippet" href="https://example.com/page1">This is the first result snippet</a>
        </div>
        <div class="result">
          <a rel="nofollow" class="result__a" href="https://example.com/page2">Example Page 2</a>
          <a class="result__snippet" href="https://example.com/page2">Second result snippet here</a>
        </div>
        "#;

        let results = parse_ddg_html(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Example Page 1");
        assert_eq!(results[0].url, "https://example.com/page1");
        assert!(results[0].snippet.contains("first result"));
        assert_eq!(results[1].title, "Example Page 2");
    }

    #[test]
    fn parse_ddg_html_max_results() {
        let html = r#"
        <a rel="nofollow" class="result__a" href="https://a.com">A</a>
        <a class="result__snippet" href="https://a.com">snip a</a>
        <a rel="nofollow" class="result__a" href="https://b.com">B</a>
        <a class="result__snippet" href="https://b.com">snip b</a>
        <a rel="nofollow" class="result__a" href="https://c.com">C</a>
        <a class="result__snippet" href="https://c.com">snip c</a>
        "#;

        let results = parse_ddg_html(html, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn format_results_empty() {
        let out = format_results("test", &[]);
        assert!(out.contains("No results found"));
    }

    #[test]
    fn format_results_with_items() {
        let results = vec![SearchResult {
            title: "Rust Lang".into(),
            url: "https://rust-lang.org".into(),
            snippet: "A systems language".into(),
        }];
        let out = format_results("rust", &results);
        assert!(out.contains("Rust Lang"));
        assert!(out.contains("https://rust-lang.org"));
        assert!(out.contains("A systems language"));
        assert!(out.starts_with("Search: \"rust\""));
    }

    #[test]
    fn decode_entities() {
        assert_eq!(
            decode_html_entities("A &amp; B &lt; C &gt; D"),
            "A & B < C > D"
        );
    }

    #[test]
    fn strip_tags() {
        assert_eq!(strip_html_tags("hello <b>world</b>!"), "hello world!");
    }

    #[test]
    fn empty_query_error() {
        // Test is async but we test the sync validation path
        let tool = WebSearchTool::new(reqwest::Client::new());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(tool.execute(json!({"query": "  "})));
        assert!(result.is_err());
    }
}
