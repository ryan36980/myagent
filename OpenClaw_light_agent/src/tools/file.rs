//! File operation tools: read, write, edit, find.
//!
//! Structured file tools for the agent — more reliable than shell `cat`/`sed`/`grep`
//! via the `exec` tool.  All structs are zero-sized (no config needed).

use std::collections::VecDeque;
use std::path::Path;

use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

use super::Tool;
use crate::error::{GatewayError, Result};

/// Maximum bytes to read from a file (64 KB).
const MAX_READ_BYTES: usize = 65_536;

/// Maximum number of results returned by file_find.
const MAX_FIND_RESULTS: usize = 50;

/// Default max directory traversal depth for file_find.
const DEFAULT_MAX_DEPTH: usize = 10;

/// Number of leading bytes to check for binary detection.
const BINARY_CHECK_BYTES: usize = 512;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if `bytes` looks like binary content (contains null bytes).
fn is_binary(bytes: &[u8]) -> bool {
    let check_len = bytes.len().min(BINARY_CHECK_BYTES);
    bytes[..check_len].contains(&0)
}

/// Format `content` with line numbers (cat -n style).
///
/// - `offset`: 1-based line number to start from (default 1).
/// - `limit`: maximum number of lines to include (0 = all).
fn format_with_line_numbers(content: &str, offset: usize, limit: usize) -> String {
    let offset = if offset == 0 { 1 } else { offset };
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if offset > total {
        return format!("(offset {offset} exceeds total {total} lines)");
    }

    let start = offset - 1; // to 0-based index
    let end = if limit == 0 {
        total
    } else {
        (start + limit).min(total)
    };

    let width = format!("{}", end).len();
    let mut buf = String::new();
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_no = start + i + 1;
        buf.push_str(&format!("{line_no:>width$}\t{line}\n"));
    }
    buf
}

// ---------------------------------------------------------------------------
// file_read
// ---------------------------------------------------------------------------

pub struct FileReadTool;

impl FileReadTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn description(&self) -> &str {
        "Read a file and return its contents with line numbers. \
         Supports offset (1-based line number) and limit (number of lines) \
         for partial reads. Binary files are detected and rejected. \
         Large files are truncated to 64 KB."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Start from this line number (1-based, default: 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: all)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "file_read".into(),
                message: "path is required".into(),
            })?;

        let offset = input["offset"].as_u64().unwrap_or(1) as usize;
        let limit = input["limit"].as_u64().unwrap_or(0) as usize;

        debug!(path, offset, limit, "file_read");

        let raw = tokio::fs::read(path).await.map_err(|e| GatewayError::Tool {
            tool: "file_read".into(),
            message: format!("{path}: {e}"),
        })?;

        if raw.is_empty() {
            return Ok("(empty file)".into());
        }

        if is_binary(&raw) {
            let len = raw.len();
            return Ok(format!("(binary file, {len} bytes)"));
        }

        let truncated = raw.len() > MAX_READ_BYTES;
        let bytes = if truncated { &raw[..MAX_READ_BYTES] } else { &raw[..] };

        let content = String::from_utf8_lossy(bytes);
        let mut result = format_with_line_numbers(&content, offset, limit);

        if truncated {
            result.push_str(&format!(
                "\n[truncated at {} bytes, file is {} bytes total]",
                MAX_READ_BYTES,
                raw.len()
            ));
        }

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// file_write
// ---------------------------------------------------------------------------

pub struct FileWriteTool;

impl FileWriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating it if it doesn't exist. \
         Parent directories are created automatically. \
         Overwrites the file if it already exists."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "file_write".into(),
                message: "path is required".into(),
            })?;

        let content = input["content"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "file_write".into(),
                message: "content is required".into(),
            })?;

        debug!(path, bytes = content.len(), "file_write");

        // Create parent directories
        if let Some(parent) = Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| GatewayError::Tool {
                        tool: "file_write".into(),
                        message: format!("failed to create directory: {e}"),
                    })?;
            }
        }

        let bytes = content.len();
        tokio::fs::write(path, content)
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "file_write".into(),
                message: format!("{path}: {e}"),
            })?;

        Ok(format!("Wrote {bytes} bytes to {path}"))
    }
}

// ---------------------------------------------------------------------------
// file_edit
// ---------------------------------------------------------------------------

pub struct FileEditTool;

impl FileEditTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing exact string matches. \
         By default replaces only one occurrence — fails if old_string \
         appears 0 or more than 1 times (set replace_all=true for multiple). \
         Supports multi-line old_string and new_string."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "Exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement string"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false, single replacement)"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let path = input["path"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "file_edit".into(),
                message: "path is required".into(),
            })?;

        let old_string = input["old_string"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "file_edit".into(),
                message: "old_string is required".into(),
            })?;

        let new_string = input["new_string"]
            .as_str()
            .ok_or_else(|| GatewayError::Tool {
                tool: "file_edit".into(),
                message: "new_string is required".into(),
            })?;

        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        debug!(path, replace_all, "file_edit");

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "file_edit".into(),
                message: format!("{path}: {e}"),
            })?;

        let count = content.matches(old_string).count();

        if count == 0 {
            return Err(GatewayError::Tool {
                tool: "file_edit".into(),
                message: format!("old_string not found in {path}"),
            });
        }

        if count > 1 && !replace_all {
            return Err(GatewayError::Tool {
                tool: "file_edit".into(),
                message: format!(
                    "old_string found {count} times in {path} — use replace_all=true \
                     or provide a more specific old_string"
                ),
            });
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        tokio::fs::write(path, &new_content)
            .await
            .map_err(|e| GatewayError::Tool {
                tool: "file_edit".into(),
                message: format!("{path}: {e}"),
            })?;

        Ok(format!(
            "Replaced {count} occurrence(s) in {path} ({} bytes)",
            new_content.len()
        ))
    }
}

// ---------------------------------------------------------------------------
// file_find
// ---------------------------------------------------------------------------

pub struct FileFindTool;

impl FileFindTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for FileFindTool {
    fn name(&self) -> &str {
        "file_find"
    }

    fn description(&self) -> &str {
        "Find files by name pattern and/or content. \
         Searches recursively from the given path (default \".\"). \
         pattern matches file name substrings; content searches inside files \
         (binary files are skipped). Returns up to 50 results."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: \".\")"
                },
                "pattern": {
                    "type": "string",
                    "description": "File name substring to match"
                },
                "content": {
                    "type": "string",
                    "description": "Search for this string inside files"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth (default: 10)"
                }
            }
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String> {
        let root = input["path"].as_str().unwrap_or(".");
        let pattern = input["pattern"].as_str();
        let content_query = input["content"].as_str();
        let max_depth = input["max_depth"].as_u64().unwrap_or(DEFAULT_MAX_DEPTH as u64) as usize;

        if pattern.is_none() && content_query.is_none() {
            return Err(GatewayError::Tool {
                tool: "file_find".into(),
                message: "at least one of pattern or content is required".into(),
            });
        }

        debug!(root, ?pattern, ?content_query, max_depth, "file_find");

        let mut results: Vec<String> = Vec::new();
        let mut queue: VecDeque<(std::path::PathBuf, usize)> = VecDeque::new();
        queue.push_back((std::path::PathBuf::from(root), 0));

        while let Some((dir, depth)) = queue.pop_front() {
            if results.len() >= MAX_FIND_RESULTS {
                break;
            }

            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                if results.len() >= MAX_FIND_RESULTS {
                    break;
                }

                let path = entry.path();
                let ft = match entry.file_type().await {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };

                if ft.is_dir() {
                    if depth < max_depth {
                        queue.push_back((path, depth + 1));
                    }
                    continue;
                }

                if !ft.is_file() {
                    continue;
                }

                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();

                // Pattern filter: match file name substring
                if let Some(pat) = pattern {
                    if !name_str.contains(pat) {
                        continue;
                    }
                }

                // Content filter: search inside the file
                if let Some(query) = content_query {
                    let bytes = match tokio::fs::read(&path).await {
                        Ok(b) => b,
                        Err(_) => continue,
                    };

                    if is_binary(&bytes) {
                        continue;
                    }

                    let text = String::from_utf8_lossy(&bytes);
                    if !text.contains(query) {
                        continue;
                    }

                    // Include matching line numbers
                    let mut matches: Vec<String> = Vec::new();
                    for (i, line) in text.lines().enumerate() {
                        if line.contains(query) {
                            let line_no = i + 1;
                            let preview = if line.len() > 120 {
                                format!("{}...", &line[..120])
                            } else {
                                line.to_string()
                            };
                            matches.push(format!("  L{line_no}: {preview}"));
                            if matches.len() >= 3 {
                                break;
                            }
                        }
                    }
                    results.push(format!("{}\n{}", path.display(), matches.join("\n")));
                } else {
                    results.push(path.display().to_string());
                }
            }
        }

        if results.is_empty() {
            return Ok("No files found.".into());
        }

        let total = results.len();
        let mut output = results.join("\n");
        if total >= MAX_FIND_RESULTS {
            output.push_str(&format!("\n\n[results capped at {MAX_FIND_RESULTS}]"));
        }
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- helper tests --------------------------------------------------------

    #[test]
    fn is_binary_with_null() {
        assert!(is_binary(b"hello\x00world"));
    }

    #[test]
    fn is_binary_plain_text() {
        assert!(!is_binary(b"hello world"));
    }

    #[test]
    fn is_binary_empty() {
        assert!(!is_binary(b""));
    }

    #[test]
    fn format_line_numbers_basic() {
        let content = "line1\nline2\nline3";
        let result = format_with_line_numbers(content, 1, 0);
        assert!(result.contains("1\tline1"));
        assert!(result.contains("2\tline2"));
        assert!(result.contains("3\tline3"));
    }

    #[test]
    fn format_line_numbers_with_offset() {
        let content = "a\nb\nc\nd\ne";
        let result = format_with_line_numbers(content, 3, 0);
        assert!(!result.contains("1\t"));
        assert!(!result.contains("2\t"));
        assert!(result.contains("3\tc"));
        assert!(result.contains("4\td"));
        assert!(result.contains("5\te"));
    }

    #[test]
    fn format_line_numbers_with_limit() {
        let content = "a\nb\nc\nd\ne";
        let result = format_with_line_numbers(content, 2, 2);
        assert!(!result.contains("1\t"));
        assert!(result.contains("2\tb"));
        assert!(result.contains("3\tc"));
        assert!(!result.contains("4\t"));
    }

    #[test]
    fn format_line_numbers_offset_beyond() {
        let content = "a\nb";
        let result = format_with_line_numbers(content, 10, 0);
        assert!(result.contains("exceeds total"));
    }

    // -- file_read tests -----------------------------------------------------

    #[test]
    fn file_read_metadata() {
        let tool = FileReadTool::new();
        assert_eq!(tool.name(), "file_read");
        assert!(!tool.description().is_empty());
        assert!(tool.input_schema().is_object());
    }

    #[tokio::test]
    async fn file_read_normal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, "hello\nworld\n").unwrap();

        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.contains("1\thello"));
        assert!(result.contains("2\tworld"));
    }

    #[tokio::test]
    async fn file_read_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, "").unwrap();

        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert_eq!(result, "(empty file)");
    }

    #[tokio::test]
    async fn file_read_not_found() {
        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": "/tmp/__nonexistent_file_12345__"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_read_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lines.txt");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();

        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap(), "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert!(result.contains("2\tb"));
        assert!(result.contains("3\tc"));
        assert!(!result.contains("1\ta"));
        assert!(!result.contains("4\td"));
    }

    #[tokio::test]
    async fn file_read_offset_beyond() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();

        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap(), "offset": 100}))
            .await
            .unwrap();
        assert!(result.contains("exceeds total"));
    }

    #[tokio::test]
    async fn file_read_binary_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        let mut data = vec![0u8; 100];
        data[50] = 0; // null byte
        std::fs::write(&path, &data).unwrap();

        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.contains("binary file"));
    }

    #[tokio::test]
    async fn file_read_large_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        // Create a file larger than MAX_READ_BYTES
        let content = "x".repeat(MAX_READ_BYTES + 1000);
        std::fs::write(&path, &content).unwrap();

        let tool = FileReadTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.contains("truncated"));
    }

    // -- file_write tests ----------------------------------------------------

    #[test]
    fn file_write_metadata() {
        let tool = FileWriteTool::new();
        assert_eq!(tool.name(), "file_write");
        assert!(!tool.description().is_empty());
        assert!(tool.input_schema().is_object());
    }

    #[tokio::test]
    async fn file_write_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");

        let tool = FileWriteTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap(), "content": "hello world"}))
            .await
            .unwrap();
        assert!(result.contains("11 bytes"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn file_write_create_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c.txt");

        let tool = FileWriteTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap(), "content": "nested"}))
            .await
            .unwrap();
        assert!(result.contains("bytes"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested");
    }

    #[tokio::test]
    async fn file_write_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("over.txt");
        std::fs::write(&path, "old content").unwrap();

        let tool = FileWriteTool::new();
        tool.execute(json!({"path": path.to_str().unwrap(), "content": "new content"}))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn file_write_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");

        let tool = FileWriteTool::new();
        let result = tool
            .execute(json!({"path": path.to_str().unwrap(), "content": ""}))
            .await
            .unwrap();
        assert!(result.contains("0 bytes"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    // -- file_edit tests -----------------------------------------------------

    #[test]
    fn file_edit_metadata() {
        let tool = FileEditTool::new();
        assert_eq!(tool.name(), "file_edit");
        assert!(!tool.description().is_empty());
        assert!(tool.input_schema().is_object());
    }

    #[tokio::test]
    async fn file_edit_single_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FileEditTool::new();
        let result = tool
            .execute(json!({
                "path": path.to_str().unwrap(),
                "old_string": "world",
                "new_string": "rust"
            }))
            .await
            .unwrap();
        assert!(result.contains("1 occurrence"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust");
    }

    #[tokio::test]
    async fn file_edit_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, "hello world").unwrap();

        let tool = FileEditTool::new();
        let result = tool
            .execute(json!({
                "path": path.to_str().unwrap(),
                "old_string": "missing",
                "new_string": "X"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[tokio::test]
    async fn file_edit_multiple_without_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, "aaa bbb aaa").unwrap();

        let tool = FileEditTool::new();
        let result = tool
            .execute(json!({
                "path": path.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "X"
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("2 times"));
    }

    #[tokio::test]
    async fn file_edit_replace_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, "aaa bbb aaa").unwrap();

        let tool = FileEditTool::new();
        let result = tool
            .execute(json!({
                "path": path.to_str().unwrap(),
                "old_string": "aaa",
                "new_string": "X",
                "replace_all": true
            }))
            .await
            .unwrap();
        assert!(result.contains("2 occurrence"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "X bbb X");
    }

    #[tokio::test]
    async fn file_edit_file_not_found() {
        let tool = FileEditTool::new();
        let result = tool
            .execute(json!({
                "path": "/tmp/__nonexistent_edit_12345__",
                "old_string": "x",
                "new_string": "y"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_edit_multiline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        std::fs::write(&path, "line1\nline2\nline3\n").unwrap();

        let tool = FileEditTool::new();
        let result = tool
            .execute(json!({
                "path": path.to_str().unwrap(),
                "old_string": "line1\nline2",
                "new_string": "replaced"
            }))
            .await
            .unwrap();
        assert!(result.contains("1 occurrence"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "replaced\nline3\n"
        );
    }

    // -- file_find tests -----------------------------------------------------

    #[test]
    fn file_find_metadata() {
        let tool = FileFindTool::new();
        assert_eq!(tool.name(), "file_find");
        assert!(!tool.description().is_empty());
        assert!(tool.input_schema().is_object());
    }

    #[tokio::test]
    async fn file_find_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("bar.rs"), "world").unwrap();

        let tool = FileFindTool::new();
        let result = tool
            .execute(json!({"path": dir.path().to_str().unwrap(), "pattern": ".txt"}))
            .await
            .unwrap();
        assert!(result.contains("foo.txt"));
        assert!(!result.contains("bar.rs"));
    }

    #[tokio::test]
    async fn file_find_by_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "needle in haystack").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here").unwrap();

        let tool = FileFindTool::new();
        let result = tool
            .execute(json!({"path": dir.path().to_str().unwrap(), "content": "needle"}))
            .await
            .unwrap();
        assert!(result.contains("a.txt"));
        assert!(!result.contains("b.txt"));
        assert!(result.contains("L1"));
    }

    #[tokio::test]
    async fn file_find_combined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("foo.txt"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("bar.rs"), "fn test() {}").unwrap();

        let tool = FileFindTool::new();
        let result = tool
            .execute(json!({
                "path": dir.path().to_str().unwrap(),
                "pattern": ".rs",
                "content": "fn main"
            }))
            .await
            .unwrap();
        assert!(result.contains("foo.rs"));
        assert!(!result.contains("foo.txt")); // wrong extension
        assert!(!result.contains("bar.rs")); // wrong content
    }

    #[tokio::test]
    async fn file_find_no_args_error() {
        let tool = FileFindTool::new();
        let result = tool.execute(json!({"path": "."})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn file_find_no_results() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let tool = FileFindTool::new();
        let result = tool
            .execute(json!({"path": dir.path().to_str().unwrap(), "pattern": ".zzz"}))
            .await
            .unwrap();
        assert_eq!(result, "No files found.");
    }

    #[tokio::test]
    async fn file_find_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("deep.txt"), "deep").unwrap();
        std::fs::write(dir.path().join("shallow.txt"), "shallow").unwrap();

        let tool = FileFindTool::new();
        // depth=1 should not find the deeply nested file
        let result = tool
            .execute(json!({
                "path": dir.path().to_str().unwrap(),
                "pattern": ".txt",
                "max_depth": 1
            }))
            .await
            .unwrap();
        assert!(result.contains("shallow.txt"));
        assert!(!result.contains("deep.txt"));
    }

    #[tokio::test]
    async fn file_find_skips_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("text.txt"), "needle").unwrap();
        std::fs::write(dir.path().join("bin.dat"), b"needle\x00binary").unwrap();

        let tool = FileFindTool::new();
        let result = tool
            .execute(json!({"path": dir.path().to_str().unwrap(), "content": "needle"}))
            .await
            .unwrap();
        assert!(result.contains("text.txt"));
        assert!(!result.contains("bin.dat"));
    }
}
