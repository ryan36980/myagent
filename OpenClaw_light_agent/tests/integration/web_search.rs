//! Integration tests for the web search tool using wiremock.

use openclaw_light::tools::web_search::{format_results, parse_ddg_html};

#[test]
fn web_search_parse_full_html() {
    // Simulate a DuckDuckGo HTML response with results
    let html = r#"
    <html><body>
    <div class="results">
      <div class="result">
        <a rel="nofollow" class="result__a" href="https://www.rust-lang.org/">Rust Programming Language</a>
        <a class="result__snippet" href="https://www.rust-lang.org/">
          A language empowering everyone to build reliable software.
        </a>
      </div>
      <div class="result">
        <a rel="nofollow" class="result__a" href="https://doc.rust-lang.org/book/">The Rust Book</a>
        <a class="result__snippet" href="https://doc.rust-lang.org/book/">
          The official Rust programming language book.
        </a>
      </div>
    </div>
    </body></html>
    "#;

    let results = parse_ddg_html(html, 5);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "Rust Programming Language");
    assert_eq!(results[0].url, "https://www.rust-lang.org/");
    assert!(results[0].snippet.contains("reliable software"));
    assert_eq!(results[1].title, "The Rust Book");
}

#[test]
fn web_search_no_results() {
    let html = r#"<html><body><div class="no-results">No results</div></body></html>"#;
    let results = parse_ddg_html(html, 5);
    assert!(results.is_empty());
    let formatted = format_results("nonexistent query", &results);
    assert!(formatted.contains("No results found"));
}
