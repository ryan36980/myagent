//! Context assembly for the agent.
//!
//! Builds the system prompt by combining the base prompt with
//! memory context, tool descriptions, and compaction warnings.

use crate::channel::types::ToolDefinition;

/// Build the full system prompt from components.
///
/// - `memory_context`: output of `MemoryStore::build_context()`, or empty.
/// - `compaction_warning`: non-empty when conversation history is near its limit.
/// - `runtime_info`: model/provider/channel info string, or empty.
/// - `context_files`: pre-loaded context file contents (SOUL.md etc), or empty.
pub fn build_system_prompt(
    base_prompt: &str,
    tools: &[ToolDefinition],
    memory_context: &str,
    compaction_warning: &str,
    runtime_info: &str,
    context_files: &str,
) -> String {
    let mut prompt = base_prompt.to_string();

    // Date/time section
    let now = chrono::Local::now();
    prompt.push_str(&format!(
        "\n\n## Current Date & Time\n{}\n",
        now.format("%A, %B %e, %Y — %H:%M")
    ));

    // Runtime info section
    if !runtime_info.is_empty() {
        prompt.push_str("\n\n## Runtime\n");
        prompt.push_str(runtime_info);
    }

    // Project context files section
    if !context_files.is_empty() {
        prompt.push_str("\n\n## Project Context\n");
        prompt.push_str(context_files);
    }

    // Memory section
    if !memory_context.is_empty() {
        prompt.push_str("\n\n## Memory\n");
        prompt.push_str(memory_context);
    }

    // Tools section
    if !tools.is_empty() {
        prompt.push_str("\n\n## Available Tools\n");
        for tool in tools {
            prompt.push_str(&format!("- **{}**: {}\n", tool.name, tool.description));
        }
    }

    // Compaction warning (appended at the very end for salience)
    if !compaction_warning.is_empty() {
        prompt.push('\n');
        prompt.push_str(compaction_warning);
        prompt.push('\n');
    }

    prompt
}

/// Warning text injected when conversation history approaches its limit.
pub const COMPACTION_WARNING: &str = "\
\u{26a0} Conversation history is approaching its limit and older messages will be \
lost. Store any important context, decisions, or user preferences to memory \
now (action: \"append\" or \"append_log\"). Reply with the user's answer \
afterward. If nothing to store, just reply normally.";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_includes_memory_section() {
        let tools = vec![ToolDefinition {
            name: "memory".into(),
            description: "manage memory".into(),
            input_schema: serde_json::json!({}),
        }];
        let prompt = build_system_prompt(
            "You are an assistant.",
            &tools,
            "### MEMORY.md\nUser likes cats\n",
            "",
            "",
            "",
        );
        assert!(prompt.contains("## Memory"));
        assert!(prompt.contains("User likes cats"));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("memory"));
    }

    #[test]
    fn prompt_empty_memory() {
        let prompt = build_system_prompt("Base.", &[], "", "", "", "");
        assert!(!prompt.contains("## Memory"));
        assert!(!prompt.contains("## Available Tools"));
    }

    #[test]
    fn prompt_with_compaction_warning() {
        let prompt = build_system_prompt("Base.", &[], "", COMPACTION_WARNING, "", "");
        assert!(prompt.contains("\u{26a0}"));
        assert!(prompt.contains("approaching its limit"));
    }

    #[test]
    fn prompt_with_runtime_info() {
        let prompt = build_system_prompt(
            "Base.",
            &[],
            "",
            "",
            "Model: claude | Provider: anthropic | Channel: telegram | Thinking: off",
            "",
        );
        assert!(prompt.contains("## Runtime"));
        assert!(prompt.contains("Model: claude"));
    }

    #[test]
    fn prompt_with_context_files() {
        let prompt = build_system_prompt(
            "Base.",
            &[],
            "",
            "",
            "",
            "### SOUL.md\nBe helpful and kind.\n",
        );
        assert!(prompt.contains("## Project Context"));
        assert!(prompt.contains("Be helpful and kind"));
    }
}
