//! HTML-to-text conversion utilities (zero external dependencies).
//!
//! Shared by `web_fetch` and `web_search` tools.

/// Convert HTML to plain text.
///
/// 1. Remove `<script>`, `<style>`, `<noscript>` blocks entirely
/// 2. Replace block-level closing tags with newlines
/// 3. Strip remaining HTML tags
/// 4. Decode common HTML entities
/// 5. Compress excessive whitespace
pub fn html_to_text(html: &str) -> String {
    let mut s = remove_tag_blocks(html, "script");
    s = remove_tag_blocks(&s, "style");
    s = remove_tag_blocks(&s, "noscript");

    // Block-level tags → newlines (before stripping tags)
    for tag in &[
        "</p>", "</div>", "</li>", "</tr>", "</table>",
        "</h1>", "</h2>", "</h3>", "</h4>", "</h5>", "</h6>",
        "</blockquote>", "</pre>", "</section>", "</article>",
        "</header>", "</footer>", "</nav>", "</main>",
    ] {
        s = replace_case_insensitive(&s, tag, "\n");
    }
    // <br> and <br/> variants
    s = replace_br(&s);

    s = strip_html_tags(&s);
    s = decode_html_entities(&s);
    compress_whitespace(&s)
}

/// Remove HTML tags from a string, preserving text content.
pub fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

/// Decode common HTML entities.
pub fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Remove entire `<tag>...</tag>` blocks (case-insensitive).
fn remove_tag_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut result = String::with_capacity(html.len());
    let mut pos = 0;

    while pos < html.len() {
        if let Some(start) = lower[pos..].find(&open) {
            let abs_start = pos + start;
            // Make sure it's a real tag (followed by space, >, or end)
            let after_open = abs_start + open.len();
            if after_open < html.len() {
                let next_ch = html.as_bytes()[after_open];
                if next_ch != b' ' && next_ch != b'>' && next_ch != b'\t'
                    && next_ch != b'\n' && next_ch != b'\r'
                {
                    // Not a real tag, copy up to after the match and continue
                    result.push_str(&html[pos..after_open]);
                    pos = after_open;
                    continue;
                }
            }
            // Copy text before the tag
            result.push_str(&html[pos..abs_start]);
            // Find the closing tag
            if let Some(end) = lower[abs_start..].find(&close) {
                pos = abs_start + end + close.len();
            } else {
                // No closing tag — skip to end
                break;
            }
        } else {
            result.push_str(&html[pos..]);
            break;
        }
    }

    result
}

/// Compress multiple blank lines into one, multiple spaces into one.
fn compress_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_newline_count = 0u32;
    let mut prev_space = false;

    for ch in s.chars() {
        match ch {
            '\n' => {
                prev_newline_count += 1;
                prev_space = false;
                if prev_newline_count <= 2 {
                    result.push('\n');
                }
            }
            ' ' | '\t' => {
                prev_newline_count = 0;
                if !prev_space {
                    result.push(' ');
                    prev_space = true;
                }
            }
            '\r' => {} // skip CR
            _ => {
                prev_newline_count = 0;
                prev_space = false;
                result.push(ch);
            }
        }
    }

    result.trim().to_string()
}

/// Case-insensitive replacement for short tag strings.
fn replace_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower_haystack = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut result = String::with_capacity(haystack.len());
    let mut pos = 0;

    while let Some(idx) = lower_haystack[pos..].find(&lower_needle) {
        let abs_idx = pos + idx;
        result.push_str(&haystack[pos..abs_idx]);
        result.push_str(replacement);
        pos = abs_idx + needle.len();
    }
    result.push_str(&haystack[pos..]);
    result
}

/// Replace `<br>`, `<br/>`, `<br />` variants with newlines (case-insensitive).
fn replace_br(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut result = String::with_capacity(s.len());
    let mut pos = 0;

    while pos < s.len() {
        if let Some(idx) = lower[pos..].find("<br") {
            let abs_idx = pos + idx;
            result.push_str(&s[pos..abs_idx]);
            // Find the closing >
            if let Some(close) = s[abs_idx..].find('>') {
                result.push('\n');
                pos = abs_idx + close + 1;
            } else {
                result.push_str(&s[abs_idx..]);
                break;
            }
        } else {
            result.push_str(&s[pos..]);
            break;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_scripts_and_styles() {
        let html = r#"<html><head><style>body{color:red}</style></head>
            <body><script>alert('xss')</script><p>Hello world</p>
            <SCRIPT type="text/javascript">var x=1;</SCRIPT></body></html>"#;
        let text = html_to_text(html);
        assert!(!text.contains("alert"));
        assert!(!text.contains("color:red"));
        assert!(!text.contains("var x"));
        assert!(text.contains("Hello world"));
    }

    #[test]
    fn html_to_text_preserves_plain_text() {
        let input = "Just plain text, no HTML here.";
        assert_eq!(html_to_text(input), input);
    }

    #[test]
    fn html_to_text_compresses_whitespace() {
        let html = "<p>Line 1</p>\n\n\n\n\n<p>Line 2</p>";
        let text = html_to_text(html);
        // Should not have more than 2 consecutive newlines
        assert!(!text.contains("\n\n\n"));
        assert!(text.contains("Line 1"));
        assert!(text.contains("Line 2"));
    }

    #[test]
    fn html_to_text_decodes_entities() {
        let html = "<p>A &amp; B &lt; C &gt; D</p>";
        let text = html_to_text(html);
        assert!(text.contains("A & B < C > D"));
    }

    #[test]
    fn html_to_text_block_tags_to_newlines() {
        let html = "<div>First</div><div>Second</div><br><p>Third</p>";
        let text = html_to_text(html);
        assert!(text.contains("First\n"));
        assert!(text.contains("Second\n"));
        assert!(text.contains("Third"));
    }

    #[test]
    fn strip_html_tags_basic() {
        assert_eq!(strip_html_tags("hello <b>world</b>!"), "hello world!");
    }

    #[test]
    fn decode_html_entities_basic() {
        assert_eq!(
            decode_html_entities("A &amp; B &lt; C &gt; D &quot;E&quot;"),
            "A & B < C > D \"E\""
        );
    }

    #[test]
    fn remove_tag_blocks_nested() {
        let html = "before<script>inner</script>after";
        assert_eq!(remove_tag_blocks(html, "script"), "beforeafter");
    }

    #[test]
    fn remove_tag_blocks_with_attributes() {
        let html = r#"before<script type="text/javascript">code</script>after"#;
        assert_eq!(remove_tag_blocks(html, "script"), "beforeafter");
    }

    #[test]
    fn compress_whitespace_basic() {
        assert_eq!(compress_whitespace("a  b   c"), "a b c");
        assert_eq!(compress_whitespace("a\n\n\n\nb"), "a\n\nb");
    }
}
