//! File-backed memory store.
//!
//! Storage layout:
//! ```text
//! {memory_dir}/
//!   SHARED/                    ← cross-chat shared memory
//!     MEMORY.md
//!   {channel}_{chat_id}/       ← per-chat isolated memory
//!     MEMORY.md                ← long-term curated memory (agent can rewrite)
//!     YYYY-MM-DD.md            ← daily append-only log
//! ```

use std::path::PathBuf;

use tracing::debug;

use crate::error::Result;

const SHARED_DIR: &str = "SHARED";

/// File-backed memory store with per-chat directories and a shared layer.
pub struct MemoryStore {
    dir: PathBuf,
    max_memory_bytes: usize,
    max_context_bytes: usize,
}

impl MemoryStore {
    pub fn new(dir: &str, max_memory_bytes: usize, max_context_bytes: usize) -> Self {
        Self {
            dir: PathBuf::from(dir),
            max_memory_bytes,
            max_context_bytes,
        }
    }

    /// Ensure the base memory directory exists.
    pub async fn init(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        Ok(())
    }

    /// Per-chat directory path.
    fn chat_dir(&self, channel: &str, chat_id: &str) -> PathBuf {
        self.dir.join(format!("{}_{}", channel, chat_id))
    }

    fn memory_path(&self, channel: &str, chat_id: &str) -> PathBuf {
        self.chat_dir(channel, chat_id).join("MEMORY.md")
    }

    fn log_path(&self, channel: &str, chat_id: &str, date: &str) -> PathBuf {
        self.chat_dir(channel, chat_id).join(format!("{}.md", date))
    }

    /// Shared memory directory (cross-chat).
    fn shared_dir(&self) -> PathBuf {
        self.dir.join(SHARED_DIR)
    }

    fn shared_memory_path(&self) -> PathBuf {
        self.shared_dir().join("MEMORY.md")
    }

    // -----------------------------------------------------------------------
    // MEMORY.md operations (per-chat)
    // -----------------------------------------------------------------------

    /// Read MEMORY.md contents. Returns empty string if not found.
    pub async fn read(&self, channel: &str, chat_id: &str) -> Result<String> {
        let path = self.memory_path(channel, chat_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Append text to MEMORY.md. Returns a warning if size exceeds max_memory_bytes.
    pub async fn append(
        &self,
        channel: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<String> {
        let dir = self.chat_dir(channel, chat_id);
        tokio::fs::create_dir_all(&dir).await?;

        let path = self.memory_path(channel, chat_id);

        // Read existing content
        let existing = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };

        let mut new_content = existing;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(text);
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }

        tokio::fs::write(&path, &new_content).await?;
        debug!(channel, chat_id, bytes = new_content.len(), "memory appended");

        if new_content.len() > self.max_memory_bytes {
            Ok(format!(
                "Saved. Warning: MEMORY.md is now {} bytes (limit {}). \
                 Consider using 'rewrite' to reorganize and trim it.",
                new_content.len(),
                self.max_memory_bytes
            ))
        } else {
            Ok("Saved.".to_string())
        }
    }

    /// Replace MEMORY.md entirely. Used by agent to reorganize/compact.
    pub async fn rewrite(
        &self,
        channel: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<String> {
        let dir = self.chat_dir(channel, chat_id);
        tokio::fs::create_dir_all(&dir).await?;

        let path = self.memory_path(channel, chat_id);
        tokio::fs::write(&path, text).await?;
        debug!(channel, chat_id, bytes = text.len(), "memory rewritten");

        Ok(format!("MEMORY.md rewritten ({} bytes).", text.len()))
    }

    // -----------------------------------------------------------------------
    // SHARED/MEMORY.md operations (cross-chat)
    // -----------------------------------------------------------------------

    /// Read SHARED/MEMORY.md. Returns empty string if not found.
    pub async fn read_shared(&self) -> Result<String> {
        let path = self.shared_memory_path();
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Append text to SHARED/MEMORY.md. Returns a warning if size exceeds max_memory_bytes.
    pub async fn append_shared(&self, text: &str) -> Result<String> {
        let dir = self.shared_dir();
        tokio::fs::create_dir_all(&dir).await?;

        let path = self.shared_memory_path();

        let existing = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };

        let mut new_content = existing;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(text);
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }

        tokio::fs::write(&path, &new_content).await?;
        debug!(bytes = new_content.len(), "shared memory appended");

        if new_content.len() > self.max_memory_bytes {
            Ok(format!(
                "Saved. Warning: shared MEMORY.md is now {} bytes (limit {}). \
                 Consider using 'rewrite' with scope 'shared' to reorganize and trim it.",
                new_content.len(),
                self.max_memory_bytes
            ))
        } else {
            Ok("Saved.".to_string())
        }
    }

    /// Replace SHARED/MEMORY.md entirely.
    pub async fn rewrite_shared(&self, text: &str) -> Result<String> {
        let dir = self.shared_dir();
        tokio::fs::create_dir_all(&dir).await?;

        let path = self.shared_memory_path();
        tokio::fs::write(&path, text).await?;
        debug!(bytes = text.len(), "shared memory rewritten");

        Ok(format!("Shared MEMORY.md rewritten ({} bytes).", text.len()))
    }

    // -----------------------------------------------------------------------
    // Daily log operations
    // -----------------------------------------------------------------------

    /// Read a daily log. `date` should be YYYY-MM-DD. Returns empty if not found.
    pub async fn read_log(
        &self,
        channel: &str,
        chat_id: &str,
        date: &str,
    ) -> Result<String> {
        let path = self.log_path(channel, chat_id, date);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Append to today's daily log.
    pub async fn append_log(
        &self,
        channel: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<String> {
        let dir = self.chat_dir(channel, chat_id);
        tokio::fs::create_dir_all(&dir).await?;

        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let path = self.log_path(channel, chat_id, &date);

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        file.write_all(text.as_bytes()).await?;
        if !text.ends_with('\n') {
            file.write_all(b"\n").await?;
        }

        debug!(channel, chat_id, date = %date, "log appended");
        Ok(format!("Logged to {}.md.", date))
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// Substring search across MEMORY.md and all daily logs.
    /// Returns matching lines with ±2 lines of context, grouped by file (newest first).
    pub async fn search(
        &self,
        channel: &str,
        chat_id: &str,
        query: &str,
        max_results: usize,
    ) -> Result<String> {
        let chat_dir = self.chat_dir(channel, chat_id);

        // Collect files: MEMORY.md first, then log files sorted newest-first
        let mut files: Vec<(String, PathBuf)> = Vec::new();

        let memory_path = self.memory_path(channel, chat_id);
        if memory_path.exists() {
            files.push(("MEMORY.md".to_string(), memory_path));
        }

        // List log files
        if let Ok(mut entries) = tokio::fs::read_dir(&chat_dir).await {
            let mut log_names: Vec<(String, PathBuf)> = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".md") && name != "MEMORY.md" {
                    log_names.push((name, entry.path()));
                }
            }
            // Sort log files newest-first (YYYY-MM-DD.md sorts lexicographically)
            log_names.sort_by(|a, b| b.0.cmp(&a.0));
            files.extend(log_names);
        }

        let query_lower = query.to_lowercase();
        let mut output = String::new();
        let mut total_matches = 0usize;

        for (label, path) in &files {
            if total_matches >= max_results {
                break;
            }

            let content = match tokio::fs::read_to_string(path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let lines: Vec<&str> = content.lines().collect();
            let mut file_matches = Vec::new();

            for (i, line) in lines.iter().enumerate() {
                if total_matches >= max_results {
                    break;
                }
                if line.to_lowercase().contains(&query_lower) {
                    // Gather context: ±2 lines
                    let start = i.saturating_sub(2);
                    let end = (i + 3).min(lines.len());
                    let ctx: Vec<String> = lines[start..end]
                        .iter()
                        .enumerate()
                        .map(|(j, l)| {
                            let line_num = start + j + 1;
                            if start + j == i {
                                format!(">{:4}: {}", line_num, l)
                            } else {
                                format!(" {:4}: {}", line_num, l)
                            }
                        })
                        .collect();
                    file_matches.push(ctx.join("\n"));
                    total_matches += 1;
                }
            }

            if !file_matches.is_empty() {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&format!("### {}\n", label));
                output.push_str(&file_matches.join("\n---\n"));
                output.push('\n');
            }
        }

        if output.is_empty() {
            Ok(format!("No results found for {:?}.", query))
        } else {
            Ok(output)
        }
    }

    /// Substring search in SHARED/MEMORY.md only.
    pub async fn search_shared(
        &self,
        query: &str,
        max_results: usize,
    ) -> Result<String> {
        let path = self.shared_memory_path();
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(format!("No results found for {:?}.", query));
            }
            Err(e) => return Err(e.into()),
        };

        let query_lower = query.to_lowercase();
        let lines: Vec<&str> = content.lines().collect();
        let mut output = String::new();
        let mut total_matches = 0usize;

        for (i, line) in lines.iter().enumerate() {
            if total_matches >= max_results {
                break;
            }
            if line.to_lowercase().contains(&query_lower) {
                let start = i.saturating_sub(2);
                let end = (i + 3).min(lines.len());
                let ctx: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(j, l)| {
                        let line_num = start + j + 1;
                        if start + j == i {
                            format!(">{:4}: {}", line_num, l)
                        } else {
                            format!(" {:4}: {}", line_num, l)
                        }
                    })
                    .collect();
                if !output.is_empty() {
                    output.push_str("\n---\n");
                }
                output.push_str(&ctx.join("\n"));
                total_matches += 1;
            }
        }

        if output.is_empty() {
            Ok(format!("No results found for {:?}.", query))
        } else {
            Ok(format!("### Shared MEMORY.md\n{}\n", output))
        }
    }

    // -----------------------------------------------------------------------
    // Context building (adaptive token budget)
    // -----------------------------------------------------------------------

    /// Build memory context for system prompt injection.
    ///
    /// Algorithm:
    /// 1. SHARED/MEMORY.md loaded first (shared knowledge, omitted if empty)
    /// 2. Per-chat MEMORY.md always included (truncated to budget if too large)
    /// 3. Remaining budget is filled with recent daily logs (newest first)
    /// 4. Each log is included whole or skipped (no mid-file truncation)
    pub async fn build_context(&self, channel: &str, chat_id: &str) -> Result<String> {
        let mut output = String::new();
        let mut budget = self.max_context_bytes;

        // 1. SHARED/MEMORY.md — only if non-empty
        let shared = self.read_shared().await?;
        if !shared.is_empty() {
            let header = "### Shared MEMORY.md\n";
            if shared.len() + header.len() + 1 <= budget {
                output.push_str(header);
                output.push_str(&shared);
                if !shared.ends_with('\n') {
                    output.push('\n');
                }
                output.push('\n');
                budget = budget.saturating_sub(output.len());
            } else {
                // Truncate shared to fit budget
                output.push_str(header);
                let avail = budget.saturating_sub(header.len() + 20);
                let truncated = safe_truncate(&shared, avail);
                output.push_str(truncated);
                output.push_str("\n(truncated)\n\n");
                budget = 0;
            }

            if budget == 0 {
                return Ok(output);
            }
        }

        // 2. Per-chat MEMORY.md — always included
        let memory = self.read(channel, chat_id).await?;
        output.push_str("### MEMORY.md\n");
        if memory.is_empty() {
            let placeholder = "No saved memories yet.\n";
            output.push_str(placeholder);
            budget = budget.saturating_sub(output.len());
        } else if memory.len() <= budget.saturating_sub(output.len()) {
            output.push_str(&memory);
            if !memory.ends_with('\n') {
                output.push('\n');
            }
            budget = budget.saturating_sub(output.len());
        } else {
            // Truncate memory to fit budget
            let avail = budget.saturating_sub(output.len() + 20); // room for truncation note
            let truncated = safe_truncate(&memory, avail);
            output.push_str(truncated);
            output.push_str("\n(truncated)\n");
            budget = 0;
        }

        if budget == 0 {
            return Ok(output);
        }

        // 2. Collect log files, sorted newest-first
        let chat_dir = self.chat_dir(channel, chat_id);
        let mut log_names: Vec<String> = Vec::new();

        if let Ok(mut entries) = tokio::fs::read_dir(&chat_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".md") && name != "MEMORY.md" {
                    log_names.push(name);
                }
            }
        }
        log_names.sort_by(|a, b| b.cmp(a)); // newest first

        if !log_names.is_empty() {
            let header = "\n### Recent Notes (auto-loaded within token budget)\n";
            if header.len() <= budget {
                output.push_str(header);
                budget = budget.saturating_sub(header.len());
            }
        }

        for name in &log_names {
            if budget == 0 {
                break;
            }
            let date = name.trim_end_matches(".md");
            let path = self.log_path(channel, chat_id, date);

            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Section header + content
            let section_header = format!("#### {}\n", date);
            let section_len = section_header.len() + content.len() + 1; // +1 for trailing \n

            if section_len <= budget {
                output.push_str(&section_header);
                output.push_str(&content);
                if !content.ends_with('\n') {
                    output.push('\n');
                }
                budget = budget.saturating_sub(section_len);
            }
            // Skip if doesn't fit (no mid-file truncation for logs)
        }

        if !log_names.is_empty() {
            let footer = "(older logs available via search or read_log tool)\n";
            if footer.len() <= budget {
                output.push_str(footer);
            }
        }

        Ok(output)
    }
}

/// Truncate a string to at most `max_bytes` without splitting a UTF-8 char.
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(
            dir.path().to_str().unwrap(),
            4096,
            4096,
        );
        (dir, store)
    }

    #[tokio::test]
    async fn read_nonexistent_returns_empty() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        let content = store.read("tg", "123").await.unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn append_creates_file_and_dir() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        let result = store.append("tg", "123", "hello world").await.unwrap();
        assert_eq!(result, "Saved.");

        // Directory and file should exist
        let path = store.memory_path("tg", "123");
        assert!(path.exists());
    }

    #[tokio::test]
    async fn append_and_read() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append("tg", "1", "line one").await.unwrap();
        store.append("tg", "1", "line two").await.unwrap();
        let content = store.read("tg", "1").await.unwrap();
        assert!(content.contains("line one"));
        assert!(content.contains("line two"));
    }

    #[tokio::test]
    async fn rewrite_replaces_content() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append("tg", "1", "old stuff").await.unwrap();
        store.rewrite("tg", "1", "new stuff\n").await.unwrap();
        let content = store.read("tg", "1").await.unwrap();
        assert_eq!(content, "new stuff\n");
        assert!(!content.contains("old stuff"));
    }

    #[tokio::test]
    async fn append_warns_on_max_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(dir.path().to_str().unwrap(), 20, 4096);
        store.init().await.unwrap();
        let result = store
            .append("tg", "1", "this is a long string that exceeds 20 bytes")
            .await
            .unwrap();
        assert!(result.contains("Warning"));
        assert!(result.contains("rewrite"));
    }

    #[tokio::test]
    async fn append_log_creates_daily_file() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        let result = store.append_log("tg", "1", "event happened").await.unwrap();
        assert!(result.contains(".md"));
    }

    #[tokio::test]
    async fn append_log_and_read_log() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append_log("tg", "1", "event A").await.unwrap();
        store.append_log("tg", "1", "event B").await.unwrap();

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let content = store.read_log("tg", "1", &today).await.unwrap();
        assert!(content.contains("event A"));
        assert!(content.contains("event B"));
    }

    #[tokio::test]
    async fn init_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub").join("memory");
        let store = MemoryStore::new(sub.to_str().unwrap(), 4096, 4096);
        store.init().await.unwrap();
        assert!(sub.exists());
    }

    #[tokio::test]
    async fn search_finds_keyword_in_memory() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append("tg", "1", "The cat sat on the mat").await.unwrap();
        let result = store.search("tg", "1", "cat", 10).await.unwrap();
        assert!(result.contains("cat"));
        assert!(result.contains("MEMORY.md"));
    }

    #[tokio::test]
    async fn search_finds_keyword_in_logs() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append_log("tg", "1", "deployed v2.0").await.unwrap();
        let result = store.search("tg", "1", "deployed", 10).await.unwrap();
        assert!(result.contains("deployed"));
    }

    #[tokio::test]
    async fn search_no_results() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append("tg", "1", "hello").await.unwrap();
        let result = store.search("tg", "1", "zzzzz", 10).await.unwrap();
        assert!(result.contains("No results"));
    }

    #[tokio::test]
    async fn build_context_respects_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Very small budget
        let store = MemoryStore::new(dir.path().to_str().unwrap(), 4096, 200);
        store.init().await.unwrap();
        store.append("tg", "1", "important fact").await.unwrap();

        // Create a log file manually for a known date
        let chat_dir = store.chat_dir("tg", "1");
        tokio::fs::write(
            chat_dir.join("2026-02-12.md"),
            "lots of log content here that should fit within budget",
        )
        .await
        .unwrap();

        let ctx = store.build_context("tg", "1").await.unwrap();
        assert!(ctx.contains("MEMORY.md"));
        assert!(ctx.contains("important fact"));
        assert!(ctx.len() <= 250); // some slack for headers
    }

    // -------------------------------------------------------------------
    // Shared memory tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn shared_read_nonexistent_returns_empty() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        let content = store.read_shared().await.unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn shared_append_and_read() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        let r = store.append_shared("device IP: 10.0.0.116").await.unwrap();
        assert_eq!(r, "Saved.");

        store.append_shared("router: 10.0.0.1").await.unwrap();

        let content = store.read_shared().await.unwrap();
        assert!(content.contains("10.0.0.116"));
        assert!(content.contains("10.0.0.1"));
    }

    #[tokio::test]
    async fn shared_rewrite_replaces_content() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append_shared("old info").await.unwrap();
        store.rewrite_shared("new info\n").await.unwrap();

        let content = store.read_shared().await.unwrap();
        assert_eq!(content, "new info\n");
        assert!(!content.contains("old info"));
    }

    #[tokio::test]
    async fn shared_append_warns_on_max_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(dir.path().to_str().unwrap(), 20, 4096);
        store.init().await.unwrap();
        let result = store
            .append_shared("this is a long string that exceeds 20 bytes")
            .await
            .unwrap();
        assert!(result.contains("Warning"));
        assert!(result.contains("rewrite"));
    }

    #[tokio::test]
    async fn search_shared_finds_keyword() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append_shared("TV IP: 10.0.0.116").await.unwrap();
        let result = store.search_shared("TV", 10).await.unwrap();
        assert!(result.contains("10.0.0.116"));
        assert!(result.contains("Shared MEMORY.md"));
    }

    #[tokio::test]
    async fn search_shared_no_results() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append_shared("hello").await.unwrap();
        let result = store.search_shared("zzzzz", 10).await.unwrap();
        assert!(result.contains("No results"));
    }

    #[tokio::test]
    async fn build_context_includes_shared() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append_shared("TV: 10.0.0.116").await.unwrap();
        store.append("tg", "1", "user prefers dark mode").await.unwrap();

        let ctx = store.build_context("tg", "1").await.unwrap();
        assert!(ctx.contains("### Shared MEMORY.md"));
        assert!(ctx.contains("10.0.0.116"));
        assert!(ctx.contains("### MEMORY.md"));
        assert!(ctx.contains("dark mode"));
        // Shared should come before per-chat
        let shared_pos = ctx.find("Shared MEMORY.md").unwrap();
        let chat_pos = ctx.find("### MEMORY.md").unwrap();
        assert!(shared_pos < chat_pos);
    }

    #[tokio::test]
    async fn build_context_omits_shared_header_when_empty() {
        let (_dir, store) = temp_store();
        store.init().await.unwrap();
        store.append("tg", "1", "per-chat only").await.unwrap();

        let ctx = store.build_context("tg", "1").await.unwrap();
        assert!(!ctx.contains("Shared MEMORY.md"));
        assert!(ctx.contains("### MEMORY.md"));
        assert!(ctx.contains("per-chat only"));
    }
}
