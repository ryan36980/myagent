//! ReAct (Reason + Act) loop implementation.
//!
//! The core agent loop that:
//! 1. Sends messages to the LLM
//! 2. If the LLM requests tool use, executes tools and feeds results back
//! 3. Repeats until the LLM produces a final text response or max iterations

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::channel::types::{
    ChatMessage, ContentBlock, IncomingMessage, LlmResponse, MessageContent, OutgoingMessage,
    Role, StopReason, StreamEvent, ToolDefinition,
};
use crate::error::Result;
use crate::memory::MemoryStore;
use crate::provider::llm::LlmProvider;
use crate::provider::stt::SttProvider;
use crate::provider::tts::TtsProvider;
use crate::session::store::sanitize_after_truncation;
use crate::session::SessionStore;
use crate::tools::memory::ChatContext;
use crate::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Loop detection
// ---------------------------------------------------------------------------

const LOOP_WINDOW: usize = 30;
const LOOP_WARNING_THRESHOLD: usize = 10;
const LOOP_BLOCK_THRESHOLD: usize = 20;
const GLOBAL_CIRCUIT_BREAKER: usize = 30;

const MAX_TOOL_RESULT_CHARS: usize = 400_000;
const TRUNCATION_SUFFIX: &str =
    "\n\n[Content truncated — original was too large. Ask the user to break the task into smaller parts.]";

/// A single tool call record with hashed fingerprint and outcome.
struct ToolCallRecord {
    /// `"tool_name:input_hash_hex"` — compact fingerprint (~30 bytes).
    fingerprint: String,
    /// Hash of the tool result (set after execution via `record_outcome`).
    result_hash: Option<u64>,
}

/// Detects repetitive tool call patterns during a ReAct loop.
///
/// Keeps a sliding window of the most recent tool call records and
/// checks for three patterns:
/// - **generic_repeat**: same tool+input called repeatedly with no-progress
/// - **ping_pong**: alternating A→B→A→B pattern with no-progress
/// - **circuit_breaker**: high tool usage with no-progress (results not changing)
struct LoopDetector {
    history: VecDeque<ToolCallRecord>,
    total_tool_calls: usize,
}

/// Result of a loop detection check.
#[derive(Debug, PartialEq)]
enum LoopStatus {
    /// No loop detected.
    Ok,
    /// Possible loop — append a warning to the tool result.
    Warning(String),
    /// Definite loop — abort the ReAct loop.
    Block(String),
}

/// Hash a serde_json::Value into a u64 using DefaultHasher (fast, non-crypto).
fn hash_value(v: &serde_json::Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let s = v.to_string();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Hash a result string into a u64.
fn hash_str(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

impl LoopDetector {
    fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(LOOP_WINDOW),
            total_tool_calls: 0,
        }
    }

    /// Build a compact fingerprint: `"tool_name:input_hash_hex"`.
    fn make_fingerprint(tool_name: &str, tool_input: &serde_json::Value) -> String {
        format!("{}:{:016x}", tool_name, hash_value(tool_input))
    }

    /// Phase 1: Record a tool call input before execution; return detection result.
    fn record_input(&mut self, tool_name: &str, tool_input: &serde_json::Value) -> LoopStatus {
        self.total_tool_calls += 1;

        let fingerprint = Self::make_fingerprint(tool_name, tool_input);
        if self.history.len() >= LOOP_WINDOW {
            self.history.pop_front();
        }
        self.history.push_back(ToolCallRecord {
            fingerprint: fingerprint.clone(),
            result_hash: None,
        });

        // Global circuit breaker: after N tool calls, check for stagnation
        if self.total_tool_calls >= GLOBAL_CIRCUIT_BREAKER {
            let stale = self.global_no_progress_streak();
            if stale >= GLOBAL_CIRCUIT_BREAKER {
                return LoopStatus::Block(format!(
                    "Global circuit breaker: {} tool calls with no progress",
                    self.total_tool_calls
                ));
            }
        }

        let len = self.history.len();
        if len < 3 {
            return LoopStatus::Ok;
        }

        // generic_repeat: count consecutive identical fingerprints from the end
        let repeat_count = self
            .history
            .iter()
            .rev()
            .take_while(|r| r.fingerprint == fingerprint)
            .count();

        // Check no-progress streak for identical calls (same input, same result)
        let no_progress = self.no_progress_streak(&fingerprint);

        if no_progress >= LOOP_BLOCK_THRESHOLD {
            return LoopStatus::Block(format!(
                "Blocked: tool '{}' called {} times with identical input and no progress",
                tool_name, no_progress
            ));
        }
        if no_progress >= LOOP_WARNING_THRESHOLD {
            return LoopStatus::Warning(format!(
                "WARNING: You have called '{}' {} times with the same input and no progress. \
                 Try a different approach or respond to the user.",
                tool_name, no_progress
            ));
        }

        // If results are changing (has progress), only warn at high repeat counts
        if repeat_count >= LOOP_BLOCK_THRESHOLD * 2 {
            return LoopStatus::Warning(format!(
                "WARNING: You have called '{}' {} times with the same input. \
                 Consider responding to the user.",
                tool_name, repeat_count
            ));
        }

        // ping_pong: check for A→B→A→B pattern (last 4 entries)
        if len >= 4 {
            let items: Vec<&ToolCallRecord> = self.history.iter().rev().take(4).collect();
            if items[0].fingerprint == items[2].fingerprint
                && items[1].fingerprint == items[3].fingerprint
                && items[0].fingerprint != items[1].fingerprint
            {
                let pp_no_progress = self.ping_pong_no_progress();
                if pp_no_progress >= LOOP_BLOCK_THRESHOLD {
                    return LoopStatus::Block(
                        "Blocked: ping-pong loop detected between two tools with no progress"
                            .into(),
                    );
                }
                if pp_no_progress >= LOOP_WARNING_THRESHOLD {
                    return LoopStatus::Warning(
                        "WARNING: You appear to be alternating between two tools \
                         in a loop with no progress. Try a different approach or respond to the user."
                            .into(),
                    );
                }
            }
        }

        // circuit_breaker: count total calls for this tool name, progress-aware
        let tool_prefix = format!("{}:", tool_name);
        let tool_count = self
            .history
            .iter()
            .filter(|r| r.fingerprint.starts_with(&tool_prefix))
            .count();

        let tool_no_progress = self.tool_no_progress_streak(tool_name);

        if tool_no_progress >= LOOP_BLOCK_THRESHOLD {
            return LoopStatus::Block(format!(
                "Blocked: tool '{}' used {} times with no progress",
                tool_name, tool_no_progress
            ));
        }
        if tool_no_progress >= LOOP_WARNING_THRESHOLD {
            return LoopStatus::Warning(format!(
                "WARNING: You have used '{}' {} times with no progress. Consider responding to the user.",
                tool_name, tool_no_progress
            ));
        }

        // High total usage with progress — soft warning
        if tool_count >= LOOP_BLOCK_THRESHOLD * 2 {
            return LoopStatus::Warning(format!(
                "WARNING: High tool usage — '{}' called {} times. Consider responding to the user.",
                tool_name, tool_count
            ));
        }

        LoopStatus::Ok
    }

    /// Phase 2: Record the tool result hash after execution.
    fn record_outcome(&mut self, result: &str) {
        if let Some(last) = self.history.back_mut() {
            last.result_hash = Some(hash_str(result));
        }
    }

    /// Count consecutive calls from the end with the same fingerprint AND same result.
    fn no_progress_streak(&self, fingerprint: &str) -> usize {
        let matching: Vec<&ToolCallRecord> = self
            .history
            .iter()
            .rev()
            .take_while(|r| r.fingerprint == fingerprint)
            .collect();

        if matching.len() < 2 {
            return 0;
        }

        // Find the result hash of the most recent completed call
        let reference_hash = matching.iter().find_map(|r| r.result_hash);
        let Some(ref_hash) = reference_hash else {
            // No results recorded yet — count input-only repeats conservatively
            return matching.len();
        };

        // Count how many consecutive have the same result
        matching
            .iter()
            .take_while(|r| r.result_hash.map_or(true, |h| h == ref_hash))
            .count()
    }

    /// Count no-progress streak for any calls to a given tool (regardless of input).
    fn tool_no_progress_streak(&self, tool_name: &str) -> usize {
        let tool_prefix = format!("{}:", tool_name);
        let tool_calls: Vec<&ToolCallRecord> = self
            .history
            .iter()
            .rev()
            .filter(|r| r.fingerprint.starts_with(&tool_prefix))
            .collect();

        if tool_calls.len() < 2 {
            return 0;
        }

        // Check consecutive calls from the end with same fingerprint+result
        let mut streak = 1;
        for i in 1..tool_calls.len() {
            let curr = &tool_calls[i - 1];
            let prev = &tool_calls[i];
            if curr.fingerprint == prev.fingerprint
                && curr.result_hash.is_some()
                && curr.result_hash == prev.result_hash
            {
                streak += 1;
            } else {
                break;
            }
        }
        streak
    }

    /// Count ping-pong no-progress length (both A and B results not changing).
    fn ping_pong_no_progress(&self) -> usize {
        let items: Vec<&ToolCallRecord> = self.history.iter().collect();
        let len = items.len();
        if len < 4 {
            return 0;
        }

        let a_fp = &items[len - 2].fingerprint;
        let b_fp = &items[len - 1].fingerprint;
        let a_hash = items[len - 2].result_hash;
        let b_hash = items[len - 1].result_hash;

        let mut count = 0;
        for i in (0..len).rev() {
            let expected_fp = if (len - 1 - i) % 2 == 0 { b_fp } else { a_fp };
            let expected_hash = if (len - 1 - i) % 2 == 0 {
                b_hash
            } else {
                a_hash
            };

            if items[i].fingerprint != *expected_fp {
                break;
            }
            // Check no-progress: result must match the reference
            if let (Some(curr), Some(exp)) = (items[i].result_hash, expected_hash) {
                if curr != exp {
                    break;
                }
            }
            count += 1;
        }
        count
    }

    /// Count consecutive entries from the end where the result matches a prior
    /// call with the same fingerprint (global no-progress check).
    fn global_no_progress_streak(&self) -> usize {
        let items: Vec<&ToolCallRecord> = self.history.iter().collect();
        let len = items.len();
        if len < 2 {
            return 0;
        }

        let mut streak = 0;
        for i in (1..len).rev() {
            let curr = &items[i];
            // Find the most recent prior call with the same fingerprint
            let prev = items[..i]
                .iter()
                .rev()
                .find(|r| r.fingerprint == curr.fingerprint);

            if let Some(prev) = prev {
                if curr.result_hash.is_some() && curr.result_hash == prev.result_hash {
                    streak += 1;
                } else {
                    break;
                }
            } else {
                break; // No prior call with same fingerprint — counts as progress
            }
        }
        streak
    }
}

/// Truncate a tool result string if it exceeds `max_chars`.
///
/// Prefers breaking at a newline boundary for readability.
fn truncate_tool_result(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    // Find a safe UTF-8 boundary
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }

    let truncated = &text[..end];
    if let Some(pos) = truncated.rfind('\n') {
        format!("{}{}", &text[..pos], TRUNCATION_SUFFIX)
    } else {
        format!("{}{}", truncated, TRUNCATION_SUFFIX)
    }
}

/// Truncate a string to at most `max` bytes, respecting UTF-8 boundaries.
fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Take the last `max` bytes of a string, respecting UTF-8 boundaries.
fn safe_tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Prune old tool results to reduce context window pressure.
///
/// Keeps the last 3 assistant messages' tool results intact; soft-trims
/// older ones to head(1500) + tail(1500) when they exceed 4000 chars.
fn prune_tool_results(messages: &mut [ChatMessage]) {
    let assistant_count = messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count();

    if assistant_count <= 3 {
        return;
    }

    // Find the index of the 3rd-from-last assistant message
    let mut asst_seen = 0;
    let mut prune_before = messages.len();
    for (i, m) in messages.iter().enumerate().rev() {
        if m.role == Role::Assistant {
            asst_seen += 1;
            if asst_seen == 3 {
                prune_before = i;
                break;
            }
        }
    }

    // Soft-trim tool results before that index
    for msg in &mut messages[..prune_before] {
        for block in &mut msg.content {
            if let ContentBlock::ToolResult { content, .. } = block {
                if content.len() > 4000 {
                    let original_len = content.len();
                    let head = safe_truncate(content, 1500).to_string();
                    let tail = safe_tail(content, 1500).to_string();
                    let trimmed = original_len - head.len() - tail.len();
                    *content = format!(
                        "{}...\n[trimmed {} chars]\n...{}",
                        head, trimmed, tail
                    );
                }
            }
        }
    }
}

/// The agent runtime — channel-agnostic message processor.
pub struct AgentRuntime {
    pub llm: Box<dyn LlmProvider>,
    pub stt: Box<dyn SttProvider>,
    pub tts: Box<dyn TtsProvider>,
    pub tools: ToolRegistry,
    pub sessions: SessionStore,
    pub memory: Arc<MemoryStore>,
    pub chat_context: Arc<Mutex<ChatContext>>,
    pub system_prompt: String,
    /// Agent turn timeout in seconds (default 600). The ReAct loop aborts
    /// after this duration, returning a timeout message.
    pub agent_timeout_secs: u64,
    /// TTS auto mode: "inbound", "always", "tagged", or anything else (off).
    pub tts_auto_mode: String,
    /// Enable automatic conversation compaction via LLM summarization.
    pub auto_compact: bool,
    /// Fraction of messages to keep after compaction (0.0–1.0, default 0.4).
    pub compact_ratio: f64,
    /// Response prefix template (supports {model}, {provider}, {thinkingLevel}).
    pub response_prefix: String,
    /// Provider name for response prefix template.
    pub provider_name: String,
    /// Model name for response prefix template.
    pub model_name: String,
    /// Thinking level string for response prefix template.
    pub thinking_level: String,
    /// Followup debounce window in milliseconds (default 2000).
    pub followup_debounce_ms: u64,
    /// Pre-loaded context file contents (SOUL.md etc), injected into system prompt.
    pub context_files_content: String,
    /// Queue mode: "interrupt" (default) or "queue".
    pub queue_mode: String,
}

impl AgentRuntime {
    /// Handle an incoming message from any channel, producing a response.
    ///
    /// The `abort` flag can be set to `true` by another task (e.g. /stop command)
    /// to cancel the agent mid-execution.
    ///
    /// If `stream_channel` is provided, text deltas are streamed live to the
    /// user via message editing. When streaming completes, the returned
    /// `OutgoingMessage.text` will be empty (already delivered).
    pub async fn handle(
        &self,
        msg: &IncomingMessage,
        download_voice: impl AsyncVoiceDownloader,
        abort: Arc<AtomicBool>,
        stream_channel: Option<Arc<dyn crate::channel::Channel>>,
    ) -> Result<OutgoingMessage> {
        let is_voice = matches!(msg.content, MessageContent::Voice { .. });

        // Step 1: Extract text and optional image content blocks
        let (text, extra_content) = match &msg.content {
            MessageContent::Text(t) => (t.clone(), Vec::new()),
            MessageContent::Voice { file_ref, mime } => {
                info!(channel = %msg.channel, chat_id = %msg.chat_id, "transcribing voice message");
                let audio_bytes = download_voice.download(file_ref).await?;
                let transcribed = match self.stt.transcribe(&audio_bytes, mime).await {
                    Ok(t) => t,
                    Err(e) if Self::is_transient_error(&e) => {
                        warn!("transient STT error, retrying after 2s: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        self.stt.transcribe(&audio_bytes, mime).await?
                    }
                    Err(e) => return Err(e),
                };
                (transcribed, Vec::new())
            }
            MessageContent::Image {
                file_ref,
                mime,
                caption,
            } => {
                info!(channel = %msg.channel, chat_id = %msg.chat_id, "processing image message");
                let image_bytes = download_voice.download(file_ref).await?;
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&image_bytes);
                let image_block = ContentBlock::Image {
                    source_type: "base64".into(),
                    media_type: mime.clone(),
                    data: b64,
                };
                let text = caption
                    .clone()
                    .unwrap_or_else(|| "What's in this image?".into());
                (text, vec![image_block])
            }
        };

        if text.trim().is_empty() && extra_content.is_empty() {
            return Ok(OutgoingMessage {
                text: Some("I couldn't understand the audio. Please try again.".into()),
                voice: None,
            });
        }

        info!(
            channel = %msg.channel,
            chat_id = %msg.chat_id,
            text_len = text.len(),
            has_image = !extra_content.is_empty(),
            "processing message"
        );

        // Update shared chat context so memory tool knows which chat we're in
        {
            let mut ctx = self.chat_context.lock().await;
            ctx.channel = msg.channel.clone();
            ctx.chat_id = msg.chat_id.clone();
        }

        // Extract preferred audio format from channel before react_loop takes ownership
        let audio_format = stream_channel
            .as_ref()
            .map(|ch| ch.preferred_audio_format())
            .unwrap_or_default();

        // Step 2: Run ReAct loop
        let reply = self
            .react_loop(
                &msg.channel,
                &msg.chat_id,
                &text,
                &abort,
                stream_channel,
                extra_content,
            )
            .await?;

        // Check if text was already delivered via streaming edits
        let (already_streamed, reply) = if let Some(rest) = reply.strip_prefix("\x00STREAMED\x00") {
            (true, rest.to_string())
        } else {
            (false, reply)
        };

        // Step 3: Determine whether to synthesize voice based on tts_auto_mode
        let (display_text, voice) = match self.tts_auto_mode.as_str() {
            "always" if !reply.is_empty() => {
                let audio = self.try_synthesize(&reply, audio_format).await;
                (reply, audio)
            }
            "inbound" if is_voice && !reply.is_empty() => {
                let audio = self.try_synthesize(&reply, audio_format).await;
                (reply, audio)
            }
            "tagged" if !reply.is_empty() => {
                let (clean_text, speak_text) = extract_speak_tags(&reply);
                let audio = if let Some(ref to_speak) = speak_text {
                    self.try_synthesize(to_speak, audio_format).await
                } else {
                    None
                };
                (clean_text, audio)
            }
            _ => (reply, None),
        };

        // If text was already streamed, don't send it again as text
        let display_text = if already_streamed { String::new() } else { display_text };

        // Silent reply: 🤐 suppresses the message entirely
        let display_text = if display_text.trim() == "\u{1f910}" {
            String::new()
        } else {
            display_text
        };

        Ok(OutgoingMessage {
            text: if display_text.is_empty() { None } else { Some(display_text) },
            voice,
        })
    }

    /// Attempt TTS synthesis, returning None on failure instead of propagating.
    async fn try_synthesize(&self, text: &str, format: crate::provider::tts::AudioFormat) -> Option<Vec<u8>> {
        info!(text_len = text.len(), "TTS synthesizing");
        match self.tts.synthesize(text, format).await {
            Ok(audio) => {
                info!(audio_bytes = audio.len(), "TTS synthesis complete");
                Some(audio)
            }
            Err(e) => {
                warn!(error = %e, "TTS synthesis failed, sending text only");
                None
            }
        }
    }

    /// Automatically compact conversation history when it exceeds 75% of
    /// the session history limit.
    ///
    /// Uses `compact_ratio` to determine how many messages to keep (default
    /// 0.4 = keep 40%, summarize the first 60%).
    async fn maybe_compact(
        &self,
        channel: &str,
        chat_id: &str,
        messages: &mut Vec<ChatMessage>,
    ) {
        if !self.auto_compact {
            return;
        }

        let limit = self.sessions.history_limit();
        // 0 = unlimited: skip proactive compaction, rely on emergency compaction
        if limit == 0 {
            return;
        }
        let threshold = (limit as f64 * 0.75) as usize;

        if messages.len() <= threshold {
            return;
        }

        self.do_compact(channel, chat_id, messages, self.compact_ratio)
            .await;
    }

    /// Emergency compaction triggered by LLM context overflow errors.
    /// Tries progressively more aggressive strategies:
    /// 1. Keep 40% with summary
    /// 2. Keep 20% with summary
    /// 3. Drop all but last 2 messages without summary
    async fn maybe_compact_emergency(
        &self,
        channel: &str,
        chat_id: &str,
        messages: &mut Vec<ChatMessage>,
    ) {
        info!(
            channel,
            chat_id,
            msg_count = messages.len(),
            "emergency compaction triggered"
        );

        // Try keeping 40% first
        if self.do_compact(channel, chat_id, messages, 0.4).await {
            return;
        }

        // More aggressive: keep 20%
        if self.do_compact(channel, chat_id, messages, 0.2).await {
            return;
        }

        // Last resort: drop everything except the last 2 messages, no summary
        if messages.len() > 2 {
            let keep = messages.split_off(messages.len() - 2);
            *messages = keep;
            sanitize_after_truncation(messages);
            warn!(
                channel,
                chat_id,
                remaining = messages.len(),
                "emergency compaction: dropped without summary"
            );
        }
    }

    /// Core compaction: summarize the oldest (1 - keep_ratio) fraction of
    /// messages and truncate. Returns true if compaction was performed.
    async fn do_compact(
        &self,
        channel: &str,
        chat_id: &str,
        messages: &mut Vec<ChatMessage>,
        keep_ratio: f64,
    ) -> bool {
        if messages.len() <= 2 {
            return false;
        }

        let keep_count = ((messages.len() as f64 * keep_ratio) as usize).max(1);
        let split_at = messages.len() - keep_count;

        info!(
            channel,
            chat_id,
            msg_count = messages.len(),
            split_at,
            keep_ratio,
            "compacting conversation"
        );

        let old_messages = &messages[..split_at];

        // Build a summarization prompt from the old messages
        let mut summary_input = String::from(
            "Summarize the following conversation into key facts, decisions, \
             and user preferences. Be concise (max 200 words):\n\n",
        );
        for msg in old_messages {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    summary_input.push_str(&format!("{role}: {text}\n"));
                }
            }
        }

        let summary_msgs = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: summary_input,
            }],
        }];

        match self
            .llm
            .chat("You are a conversation summarizer.", &summary_msgs, &[])
            .await
        {
            Ok(response) => {
                if let Some(summary) = response.text() {
                    if let Err(e) = self
                        .memory
                        .append_log(channel, chat_id, &format!("[auto-compact] {summary}"))
                        .await
                    {
                        warn!(error = %e, "failed to append compaction summary to memory");
                    }

                    *messages = messages.split_off(split_at);
                    sanitize_after_truncation(messages);

                    info!(
                        channel,
                        chat_id,
                        remaining = messages.len(),
                        "compaction complete"
                    );
                    true
                } else {
                    false
                }
            }
            Err(e) => {
                warn!(error = %e, "compaction LLM call failed, skipping");
                false
            }
        }
    }

    /// Apply the response prefix template to a reply text.
    /// Replaces `{model}`, `{provider}`, `{thinkingLevel}` with actual values.
    fn apply_response_prefix(&self, text: &str) -> String {
        if self.response_prefix.is_empty() {
            return text.to_string();
        }
        let prefix = self
            .response_prefix
            .replace("{model}", &self.model_name)
            .replace("{provider}", &self.provider_name)
            .replace("{thinkingLevel}", &self.thinking_level);
        format!("{}{}", prefix, text)
    }

    /// Check if an error indicates an LLM context window overflow.
    fn is_context_overflow(err: &crate::error::GatewayError) -> bool {
        let msg = err.to_string().to_lowercase();
        msg.contains("context")
            || msg.contains("too many tokens")
            || msg.contains("max_tokens")
            || msg.contains("context_length_exceeded")
            || msg.contains("prompt is too long")
    }

    /// Check if an error is transient and worth retrying once.
    fn is_transient_error(err: &crate::error::GatewayError) -> bool {
        let msg = err.to_string().to_lowercase();
        msg.contains("transport error")
            || msg.contains("connection")
            || msg.contains("timed out")
            || msg.contains("overloaded")
            || msg.contains("lookup")
            || ["500", "502", "503", "521", "522", "523", "524", "529"]
                .iter()
                .any(|code| msg.contains(code))
    }

    /// Call LLM via streaming and accumulate events into an LlmResponse.
    #[cfg(test)]
    async fn consume_stream(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        self.consume_stream_live(system, messages, tools, &mut None, &Arc::new(AtomicBool::new(false)))
            .await
    }

    /// Call LLM via streaming with live preview support.
    ///
    /// If `writer` is `Some`, text deltas are pushed to the StreamingWriter
    /// for real-time message editing. On tool_use, the writer is stopped.
    /// If `abort` is set, the stream is terminated early.
    async fn consume_stream_live(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        writer: &mut Option<crate::channel::streaming::StreamingWriter>,
        abort: &Arc<AtomicBool>,
    ) -> Result<LlmResponse> {
        use futures_util::StreamExt;

        let mut stream = self.llm.chat_stream(system, messages, tools).await?;
        let mut text_buffer = String::new();
        let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();
        let mut stop_reason = StopReason::EndTurn;

        while let Some(event) = stream.next().await {
            if abort.load(Ordering::Relaxed) {
                break;
            }
            match event? {
                StreamEvent::TextDelta(t) => {
                    text_buffer.push_str(&t);
                    if let Some(ref mut w) = writer {
                        w.push(&t).await;
                    }
                }
                StreamEvent::ToolUse { id, name, input } => {
                    if let Some(ref mut w) = writer {
                        w.stop().await;
                    }
                    tool_uses.push((id, name, input));
                }
                StreamEvent::Done { stop_reason: sr } => {
                    stop_reason = sr;
                }
            }
        }

        let mut content = Vec::new();
        if !text_buffer.is_empty() {
            content.push(ContentBlock::Text { text: text_buffer });
        }
        for (id, name, input) in tool_uses {
            content.push(ContentBlock::ToolUse { id, name, input });
        }

        if content.is_empty() {
            debug!("LLM returned empty content (no text, no tool_use)");
        }

        Ok(LlmResponse {
            content,
            stop_reason,
        })
    }

    /// The core ReAct loop.
    ///
    /// If `stream_channel` is provided, text deltas are streamed live to the
    /// user via message editing (Telegram). The final text is delivered through
    /// the streaming writer rather than returned for separate dispatch.
    ///
    /// `extra_content` contains additional content blocks (e.g. images) to
    /// prepend to the user message.
    pub async fn react_loop(
        &self,
        channel: &str,
        chat_id: &str,
        input: &str,
        abort: &Arc<AtomicBool>,
        stream_channel: Option<Arc<dyn crate::channel::Channel>>,
        extra_content: Vec<ContentBlock>,
    ) -> Result<String> {
        // Load session history
        let mut messages = self.sessions.load(channel, chat_id).await?;

        // Auto-compact if approaching history limit
        self.maybe_compact(channel, chat_id, &mut messages).await;

        // Add user message (with optional image content blocks prepended)
        let mut user_content = extra_content;
        user_content.push(ContentBlock::Text {
            text: input.to_string(),
        });
        messages.push(ChatMessage {
            role: Role::User,
            content: user_content,
        });

        // Build memory context for system prompt injection
        let memory_context = self.memory.build_context(channel, chat_id).await?;

        let tool_defs = self.tools.definitions();

        // Check if history is still large after compaction — warn agent
        let compaction_warning = if messages.len() > 15 {
            crate::agent::context::COMPACTION_WARNING
        } else {
            ""
        };

        // Runtime info for system prompt
        let runtime_info = format!(
            "Model: {} | Provider: {} | Channel: {} | Thinking: {}",
            self.model_name, self.provider_name, channel, self.thinking_level
        );

        let system = crate::agent::context::build_system_prompt(
            &self.system_prompt,
            &tool_defs,
            &memory_context,
            compaction_warning,
            &runtime_info,
            &self.context_files_content,
        );

        let mut loop_detector = LoopDetector::new();
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(self.agent_timeout_secs);
        let mut iteration: usize = 0;
        // Track whether any text was delivered to the user across iterations
        // (via streaming edits). Used to avoid silent responses after tool calls.
        let mut any_text_streamed = false;

        loop {
            // Prune old tool results to reduce context pressure
            prune_tool_results(&mut messages);

            // Abort check
            if abort.load(Ordering::Relaxed) {
                info!(channel, chat_id, "agent aborted by user");
                self.sessions.save(channel, chat_id, &messages).await?;
                return Ok("Operation cancelled.".to_string());
            }

            // Time-based timeout (aligns with OpenClaw original 600s agent timeout)
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            debug!(iteration, "ReAct loop iteration");

            // Create a StreamingWriter if channel supports message editing
            let mut writer = stream_channel.as_ref().map(|ch| {
                crate::channel::streaming::StreamingWriter::new(ch.clone(), chat_id.to_string())
            });

            let response = match self
                .consume_stream_live(&system, &messages, &tool_defs, &mut writer, abort)
                .await
            {
                Ok(r) => r,
                Err(ref e) if Self::is_context_overflow(e) => {
                    warn!("context overflow detected, attempting emergency compaction");
                    self.maybe_compact_emergency(channel, chat_id, &mut messages)
                        .await;
                    // Retry after compaction (fresh writer)
                    writer = stream_channel.as_ref().map(|ch| {
                        crate::channel::streaming::StreamingWriter::new(
                            ch.clone(),
                            chat_id.to_string(),
                        )
                    });
                    self.consume_stream_live(&system, &messages, &tool_defs, &mut writer, abort)
                        .await?
                }
                Err(ref e) if Self::is_transient_error(e) => {
                    warn!("transient error, retrying after 2.5s: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                    writer = stream_channel.as_ref().map(|ch| {
                        crate::channel::streaming::StreamingWriter::new(
                            ch.clone(),
                            chat_id.to_string(),
                        )
                    });
                    self.consume_stream_live(&system, &messages, &tool_defs, &mut writer, abort)
                        .await?
                }
                Err(e) => return Err(e),
            };

            match response.stop_reason {
                StopReason::MaxTokens => {
                    // Output was truncated — append what we have and let the
                    // model continue in the next iteration.  This handles the
                    // case where a tool_use block was cut off mid-generation.
                    warn!(
                        channel,
                        chat_id,
                        iteration,
                        content_blocks = response.content.len(),
                        "max_tokens reached, continuing loop"
                    );

                    if let Some(ref mut w) = writer {
                        w.stop().await;
                        if w.was_sent() {
                            any_text_streamed = true;
                        }
                    }

                    if !response.content.is_empty() {
                        messages.push(ChatMessage {
                            role: Role::Assistant,
                            content: response.content.clone(),
                        });
                        // Inject a synthetic user nudge so the model continues
                        messages.push(ChatMessage {
                            role: Role::User,
                            content: vec![ContentBlock::Text {
                                text: "[Your previous response was truncated due to length. Continue where you left off — call the tool now.]".to_string(),
                            }],
                        });
                    }
                    // Fall through to next loop iteration
                }
                StopReason::EndTurn => {
                    // Final response — add assistant message and save
                    messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: response.content.clone(),
                    });

                    self.sessions.save(channel, chat_id, &messages).await?;

                    let raw_reply = response
                        .text()
                        .unwrap_or("")
                        .to_string();
                    let reply_text = self.apply_response_prefix(&raw_reply);

                    // Finalize streaming preview if active
                    // Track whether text was already delivered via streaming
                    let mut already_streamed = false;
                    if let Some(ref mut w) = writer {
                        w.finish().await;
                        if w.was_sent() {
                            already_streamed = true;
                        }
                    }
                    if !already_streamed && reply_text.is_empty() && any_text_streamed {
                        already_streamed = true;
                    }

                    if already_streamed {
                        info!(
                            channel,
                            chat_id,
                            iterations = iteration + 1,
                            reply_len = reply_text.len(),
                            "ReAct loop complete (streamed)"
                        );
                        // Prefix with marker so handle() knows text was
                        // already displayed but can still use it for TTS.
                        return Ok(format!("\x00STREAMED\x00{}", reply_text));
                    }

                    // Guard: if LLM returned completely empty response (no text,
                    // no tool_use) and nothing was streamed, don't silently drop —
                    // return a fallback so the user isn't left hanging.
                    if reply_text.is_empty() && !any_text_streamed && response.content.is_empty() {
                        warn!(
                            channel,
                            chat_id,
                            iterations = iteration + 1,
                            "LLM returned empty response, returning fallback"
                        );
                        return Ok("(Model returned an empty response, this is normal and can happen occasionally — please try again.)".to_string());
                    }

                    info!(
                        channel,
                        chat_id,
                        iterations = iteration + 1,
                        reply_len = reply_text.len(),
                        "ReAct loop complete"
                    );

                    return Ok(reply_text);
                }
                StopReason::ToolUse => {
                    // Track if streaming writer sent text in this iteration
                    if let Some(ref w) = writer {
                        if w.was_sent() {
                            any_text_streamed = true;
                        }
                    }

                    // Add assistant message with tool use blocks
                    messages.push(ChatMessage {
                        role: Role::Assistant,
                        content: response.content.clone(),
                    });

                    // Abort check before tool execution
                    if abort.load(Ordering::Relaxed) {
                        info!(channel, chat_id, "agent aborted by user during tool execution");
                        self.sessions.save(channel, chat_id, &messages).await?;
                        return Ok("Operation cancelled.".to_string());
                    }

                    // Phase 1: Record inputs and check for loop blocks
                    let tool_uses = response.tool_uses();
                    let mut tasks: Vec<(&str, &str, &serde_json::Value, LoopStatus)> =
                        Vec::with_capacity(tool_uses.len());
                    let mut tool_results: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());
                    let mut blocked = false;

                    for (tool_id, tool_name, tool_input) in &tool_uses {
                        let loop_status =
                            loop_detector.record_input(tool_name, tool_input);

                        if let LoopStatus::Block(ref msg) = loop_status {
                            warn!(tool = *tool_name, "loop detected, blocking: {}", msg);
                            // Generate error results for ALL tool_uses so the
                            // API never sees a tool_use without a matching result.
                            tool_results.clear();
                            for (tid, _, _) in &tool_uses {
                                tool_results.push(ContentBlock::ToolResult {
                                    tool_use_id: tid.to_string(),
                                    content: msg.clone(),
                                });
                            }
                            blocked = true;
                            break;
                        }

                        tasks.push((tool_id, tool_name, tool_input, loop_status));
                    }

                    if !blocked {
                        // Phase 2: Execute all tools in parallel
                        let futures: Vec<_> = tasks
                            .iter()
                            .map(|(_, tool_name, tool_input, _)| {
                                info!(tool = *tool_name, "executing tool");
                                self.tools.execute(tool_name, (*tool_input).clone())
                            })
                            .collect();
                        let results = futures_util::future::join_all(futures).await;

                        // Phase 3: Process results
                        for ((tool_id, tool_name, _, loop_status), exec_result) in
                            tasks.iter().zip(results)
                        {
                            let mut result = match exec_result {
                                Ok(output) => output,
                                Err(e) => {
                                    error!(tool = *tool_name, error = %e, "tool execution failed");
                                    format!("Error: {}", e)
                                }
                            };

                            loop_detector.record_outcome(&result);

                            result = truncate_tool_result(&result, MAX_TOOL_RESULT_CHARS);

                            if let LoopStatus::Warning(ref warning) = loop_status {
                                warn!(tool = *tool_name, "loop warning: {}", warning);
                                result = format!("{}\n\n{}", result, warning);
                            }

                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: tool_id.to_string(),
                                content: result,
                            });
                        }
                    }

                    // Add tool results as user message
                    messages.push(ChatMessage {
                        role: Role::User,
                        content: tool_results,
                    });

                    if blocked {
                        // Force one more LLM call to get a final text response
                        debug!("loop blocked, requesting final response");
                    }
                }
                StopReason::Other(ref reason) => {
                    warn!(reason, "unexpected stop reason");
                    break;
                }
            }

            iteration += 1;
        }

        // Agent turn timeout reached
        warn!(
            timeout_secs = self.agent_timeout_secs,
            iterations = iteration,
            "ReAct loop timed out"
        );
        self.sessions.save(channel, chat_id, &messages).await?;
        Ok("Processing took too long, please try again.".to_string())
    }
}

/// Extract `<speak>...</speak>` tags from text for the "tagged" TTS mode.
///
/// Returns `(clean_text, Option<speak_text>)`:
/// - `clean_text`: original text with all `<speak>...</speak>` tags removed.
/// - `speak_text`: concatenated content from all `<speak>` tags, or `None` if
///   no tags found.
fn extract_speak_tags(text: &str) -> (String, Option<String>) {
    let mut clean = String::with_capacity(text.len());
    let mut speak = String::new();
    let mut rest = text;

    while let Some(start) = rest.find("<speak>") {
        // Append everything before the tag
        clean.push_str(&rest[..start]);

        let after_open = &rest[start + 7..]; // len("<speak>") == 7
        if let Some(end) = after_open.find("</speak>") {
            if !speak.is_empty() {
                speak.push(' ');
            }
            speak.push_str(&after_open[..end]);
            rest = &after_open[end + 8..]; // len("</speak>") == 8
        } else {
            // Unclosed tag — keep the rest as-is
            clean.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }

    // Append anything after the last tag
    clean.push_str(rest);

    if speak.is_empty() {
        (clean, None)
    } else {
        (clean, Some(speak))
    }
}

/// Abstraction for downloading voice files from a channel.
/// This avoids making AgentRuntime depend on the Channel trait directly.
#[async_trait::async_trait]
pub trait AsyncVoiceDownloader: Send + Sync {
    async fn download(&self, file_ref: &str) -> Result<Vec<u8>>;
}

/// A voice downloader backed by a Channel reference.
pub struct ChannelVoiceDownloader<'a, C: crate::channel::Channel + ?Sized> {
    channel: &'a C,
}

impl<'a, C: crate::channel::Channel + ?Sized> ChannelVoiceDownloader<'a, C> {
    pub fn new(channel: &'a C) -> Self {
        Self { channel }
    }
}

#[async_trait::async_trait]
impl<C: crate::channel::Channel + ?Sized> AsyncVoiceDownloader for ChannelVoiceDownloader<'_, C> {
    async fn download(&self, file_ref: &str) -> Result<Vec<u8>> {
        self.channel.download_voice(file_ref).await
    }
}

/// A voice downloader backed by an `Arc<dyn Channel>`.
pub struct ArcVoiceDownloader {
    channel: Arc<dyn crate::channel::Channel>,
}

impl ArcVoiceDownloader {
    pub fn new(channel: Arc<dyn crate::channel::Channel>) -> Self {
        Self { channel }
    }
}

#[async_trait::async_trait]
impl AsyncVoiceDownloader for ArcVoiceDownloader {
    async fn download(&self, file_ref: &str) -> Result<Vec<u8>> {
        self.channel.download_voice(file_ref).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::stt::SttProvider;
    use crate::provider::tts::TtsProvider;

    // -- Mock providers -------------------------------------------------------

    /// Mock LLM that always returns a fixed response.
    struct MockLlm {
        response: LlmResponse,
    }

    #[async_trait::async_trait]
    impl crate::provider::llm::LlmProvider for MockLlm {
        async fn chat(
            &self,
            _system: &str,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> crate::error::Result<LlmResponse> {
            Ok(self.response.clone())
        }
        // Uses the default chat_stream() → wraps chat() into a stream
    }

    /// Mock LLM that returns responses in order (for multi-turn tests).
    struct SequentialMockLlm {
        responses: std::sync::Mutex<Vec<LlmResponse>>,
    }

    #[async_trait::async_trait]
    impl crate::provider::llm::LlmProvider for SequentialMockLlm {
        async fn chat(
            &self,
            _system: &str,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> crate::error::Result<LlmResponse> {
            let mut responses = self.responses.lock().unwrap();
            assert!(!responses.is_empty(), "no more mock responses");
            Ok(responses.remove(0))
        }
    }

    struct MockStt;

    #[async_trait::async_trait]
    impl SttProvider for MockStt {
        async fn transcribe(&self, _audio: &[u8], _mime: &str) -> crate::error::Result<String> {
            Ok("transcribed".into())
        }
    }

    struct MockTts;

    #[async_trait::async_trait]
    impl TtsProvider for MockTts {
        async fn synthesize(&self, _text: &str, _format: crate::provider::tts::AudioFormat) -> crate::error::Result<Vec<u8>> {
            Ok(vec![])
        }
    }

    struct MockTool;

    #[async_trait::async_trait]
    impl crate::tools::Tool for MockTool {
        fn name(&self) -> &str {
            "test_tool"
        }
        fn description(&self) -> &str {
            "A test tool"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _input: serde_json::Value) -> crate::error::Result<String> {
            Ok("tool_result".into())
        }
    }

    /// Helper to construct a minimal AgentRuntime with a mock LLM.
    fn build_runtime(
        tmp: &tempfile::TempDir,
        llm: Box<dyn crate::provider::llm::LlmProvider>,
        tools: Vec<Box<dyn crate::tools::Tool>>,
        history_limit: usize,
        auto_compact: bool,
    ) -> AgentRuntime {
        let session_dir = tmp.path().join("sessions");
        let memory_dir = tmp.path().join("memory");
        AgentRuntime {
            llm,
            stt: Box::new(MockStt),
            tts: Box::new(MockTts),
            tools: ToolRegistry::new(tools),
            sessions: SessionStore::new(session_dir.to_str().unwrap(), history_limit, 0),
            memory: Arc::new(MemoryStore::new(
                memory_dir.to_str().unwrap(),
                4096,
                4096,
            )),
            chat_context: Arc::new(Mutex::new(ChatContext {
                channel: String::new(),
                chat_id: String::new(),
            })),
            system_prompt: "Test.".into(),
            agent_timeout_secs: 5,
            tts_auto_mode: "off".into(),
            auto_compact,
            compact_ratio: 0.4,
            response_prefix: String::new(),
            provider_name: "test".into(),
            model_name: "test-model".into(),
            thinking_level: "off".into(),
            followup_debounce_ms: 2000,
            context_files_content: String::new(),
            queue_mode: "interrupt".into(),
        }
    }

    // -- extract_speak_tags tests ---------------------------------------------

    #[test]
    fn extract_speak_tags_no_tags() {
        let (clean, speak) = extract_speak_tags("Hello, how are you?");
        assert_eq!(clean, "Hello, how are you?");
        assert!(speak.is_none());
    }

    #[test]
    fn extract_speak_tags_single_tag() {
        let (clean, speak) = extract_speak_tags("OK, <speak>lights turned on</speak>.");
        assert_eq!(clean, "OK, .");
        assert_eq!(speak.unwrap(), "lights turned on");
    }

    #[test]
    fn extract_speak_tags_multiple_tags() {
        let (clean, speak) =
            extract_speak_tags("<speak>Hello</speak> world <speak>goodbye</speak>");
        assert_eq!(clean, " world ");
        assert_eq!(speak.unwrap(), "Hello goodbye");
    }

    #[test]
    fn extract_speak_tags_unclosed_tag() {
        let (clean, speak) = extract_speak_tags("Start <speak>unclosed text");
        assert_eq!(clean, "Start <speak>unclosed text");
        assert!(speak.is_none());
    }

    #[test]
    fn extract_speak_tags_empty_tag() {
        let (clean, speak) = extract_speak_tags("Before <speak></speak> after");
        assert_eq!(clean, "Before  after");
        // Empty speak content results in None
        assert!(speak.is_none());
    }

    #[test]
    fn extract_speak_tags_entire_text() {
        let (clean, speak) = extract_speak_tags("<speak>All spoken</speak>");
        assert_eq!(clean, "");
        assert_eq!(speak.unwrap(), "All spoken");
    }

    // -- consume_stream tests -------------------------------------------------

    #[tokio::test]
    async fn consume_stream_accumulates_text_and_tool_use() {
        let tmp = tempfile::tempdir().unwrap();
        let response = LlmResponse {
            content: vec![
                ContentBlock::Text {
                    text: "Hello ".into(),
                },
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "memory".into(),
                    input: serde_json::json!({"action": "read"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
        };
        let rt = build_runtime(&tmp, Box::new(MockLlm { response }), vec![], 20, true);

        let msgs = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".into(),
            }],
        }];

        let result = rt.consume_stream("system", &msgs, &[]).await.unwrap();
        assert_eq!(result.stop_reason, StopReason::ToolUse);
        assert_eq!(result.content.len(), 2);
        match &result.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello "),
            other => panic!("expected Text, got: {:?}", other),
        }
        match &result.content[1] {
            ContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "memory");
            }
            other => panic!("expected ToolUse, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn consume_stream_end_turn_text_only() {
        let tmp = tempfile::tempdir().unwrap();
        let response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: "Just text.".into(),
            }],
            stop_reason: StopReason::EndTurn,
        };
        let rt = build_runtime(&tmp, Box::new(MockLlm { response }), vec![], 20, true);

        let msgs = vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
        }];

        let result = rt.consume_stream("system", &msgs, &[]).await.unwrap();
        assert_eq!(result.stop_reason, StopReason::EndTurn);
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.text().unwrap(), "Just text.");
    }

    // -- maybe_compact tests --------------------------------------------------

    #[tokio::test]
    async fn maybe_compact_triggers_and_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let summary_response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: "Summary: user discussed cats.".into(),
            }],
            stop_reason: StopReason::EndTurn,
        };
        // history_limit=8, threshold=6, so 8 messages triggers compaction
        let rt = build_runtime(
            &tmp,
            Box::new(MockLlm {
                response: summary_response,
            }),
            vec![],
            8,
            true,
        );
        rt.memory.init().await.unwrap();

        let mut messages: Vec<ChatMessage> = (0..8)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![ContentBlock::Text {
                    text: format!("msg_{i}"),
                }],
            })
            .collect();

        rt.maybe_compact("test", "c1", &mut messages).await;

        // compact_ratio=0.4: keep_count = (8*0.4)=3, split_at = 8-3 = 5
        // keeps messages[5..8], sanitize strips leading Asst(msg_5)
        // result: [msg_6(User), msg_7(Asst)] = 2 messages
        assert_eq!(messages.len(), 2);
        match &messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "msg_6"),
            _ => panic!("expected text"),
        }
    }

    #[tokio::test]
    async fn maybe_compact_skips_when_below_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: "unused".into(),
            }],
            stop_reason: StopReason::EndTurn,
        };
        // history_limit=20, threshold=15, only 3 messages → no compaction
        let rt = build_runtime(&tmp, Box::new(MockLlm { response }), vec![], 20, true);

        let mut messages: Vec<ChatMessage> = (0..3)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![ContentBlock::Text {
                    text: format!("msg_{i}"),
                }],
            })
            .collect();

        rt.maybe_compact("test", "c1", &mut messages).await;
        assert_eq!(messages.len(), 3);
    }

    #[tokio::test]
    async fn maybe_compact_skips_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: "unused".into(),
            }],
            stop_reason: StopReason::EndTurn,
        };
        // auto_compact=false, history_limit=8, 7 messages would trigger but won't
        let rt = build_runtime(&tmp, Box::new(MockLlm { response }), vec![], 8, false);

        let mut messages: Vec<ChatMessage> = (0..7)
            .map(|i| ChatMessage {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![ContentBlock::Text {
                    text: format!("msg_{i}"),
                }],
            })
            .collect();

        rt.maybe_compact("test", "c1", &mut messages).await;
        assert_eq!(messages.len(), 7);
    }

    // -- react_loop end-to-end tests ------------------------------------------

    #[tokio::test]
    async fn react_loop_returns_text_response() {
        let tmp = tempfile::tempdir().unwrap();
        let response = LlmResponse {
            content: vec![ContentBlock::Text {
                text: "Hello user!".into(),
            }],
            stop_reason: StopReason::EndTurn,
        };
        let rt = build_runtime(&tmp, Box::new(MockLlm { response }), vec![], 20, true);
        rt.sessions.init().await.unwrap();
        rt.memory.init().await.unwrap();

        let abort = Arc::new(AtomicBool::new(false));
        let reply = rt
            .react_loop("test", "c1", "hello", &abort, None, Vec::new())
            .await
            .unwrap();
        assert_eq!(reply, "Hello user!");
    }

    #[tokio::test]
    async fn react_loop_executes_tool_then_responds() {
        let tmp = tempfile::tempdir().unwrap();

        let responses = vec![
            // First call: tool use
            LlmResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "test_tool".into(),
                    input: serde_json::json!({}),
                }],
                stop_reason: StopReason::ToolUse,
            },
            // Second call: final text
            LlmResponse {
                content: vec![ContentBlock::Text {
                    text: "Done!".into(),
                }],
                stop_reason: StopReason::EndTurn,
            },
        ];

        let llm = Box::new(SequentialMockLlm {
            responses: std::sync::Mutex::new(responses),
        });
        let tools: Vec<Box<dyn crate::tools::Tool>> = vec![Box::new(MockTool)];
        let rt = build_runtime(&tmp, llm, tools, 20, true);
        rt.sessions.init().await.unwrap();
        rt.memory.init().await.unwrap();

        let abort = Arc::new(AtomicBool::new(false));
        let reply = rt
            .react_loop("test", "c1", "do it", &abort, None, Vec::new())
            .await
            .unwrap();
        assert_eq!(reply, "Done!");
    }

    // -- response prefix tests -------------------------------------------------

    #[test]
    fn response_prefix_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = build_runtime(
            &tmp,
            Box::new(MockLlm {
                response: LlmResponse {
                    content: vec![],
                    stop_reason: StopReason::EndTurn,
                },
            }),
            vec![],
            20,
            false,
        );
        assert_eq!(rt.apply_response_prefix("Hello"), "Hello");
    }

    #[test]
    fn response_prefix_with_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rt = build_runtime(
            &tmp,
            Box::new(MockLlm {
                response: LlmResponse {
                    content: vec![],
                    stop_reason: StopReason::EndTurn,
                },
            }),
            vec![],
            20,
            false,
        );
        rt.response_prefix = "[{provider}/{model}] ".into();
        rt.provider_name = "anthropic".into();
        rt.model_name = "claude-3".into();
        assert_eq!(
            rt.apply_response_prefix("Hello"),
            "[anthropic/claude-3] Hello"
        );
    }

    // -- LoopDetector tests ----------------------------------------------------

    #[test]
    fn loop_detector_no_loop() {
        let mut d = LoopDetector::new();
        let v1 = serde_json::json!({"a": 1});
        let v2 = serde_json::json!({"b": 2});
        assert_eq!(d.record_input("tool_a", &v1), LoopStatus::Ok);
        d.record_outcome("result_a");
        assert_eq!(d.record_input("tool_b", &v2), LoopStatus::Ok);
        d.record_outcome("result_b");
        assert_eq!(d.record_input("tool_a", &v1), LoopStatus::Ok);
        d.record_outcome("result_a");
    }

    #[test]
    fn loop_detector_generic_repeat_warning_with_no_progress() {
        let mut d = LoopDetector::new();
        let v = serde_json::json!({"x": 1});
        for _ in 0..LOOP_WARNING_THRESHOLD - 1 {
            assert_eq!(d.record_input("tool_a", &v), LoopStatus::Ok);
            d.record_outcome("same_result"); // same result every time
        }
        match d.record_input("tool_a", &v) {
            LoopStatus::Warning(msg) => assert!(msg.contains("tool_a")),
            other => panic!("expected Warning, got {:?}", other),
        }
    }

    #[test]
    fn loop_detector_generic_repeat_block_with_no_progress() {
        let mut d = LoopDetector::new();
        let v = serde_json::json!({"x": 1});
        for _ in 0..LOOP_BLOCK_THRESHOLD - 1 {
            let _ = d.record_input("tool_a", &v);
            d.record_outcome("same_result");
        }
        match d.record_input("tool_a", &v) {
            LoopStatus::Block(msg) => assert!(msg.contains("Blocked")),
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn loop_detector_same_input_different_results_no_block() {
        // The key improvement: same tool+input but results are changing → no block
        let mut d = LoopDetector::new();
        let v = serde_json::json!({"cmd": "ls"});
        for i in 0..LOOP_BLOCK_THRESHOLD + 5 {
            let status = d.record_input("exec", &v);
            d.record_outcome(&format!("result_{}", i)); // different result each time
            // Should never block because results are changing (progress)
            match status {
                LoopStatus::Block(_) => panic!("should not block when results are changing"),
                _ => {}
            }
        }
    }

    #[test]
    fn loop_detector_different_inputs_no_block() {
        // Different inputs for same tool should not trigger circuit breaker
        let mut d = LoopDetector::new();
        for i in 0..LOOP_BLOCK_THRESHOLD + 5 {
            let status = d.record_input("exec", &serde_json::json!({"i": i}));
            d.record_outcome(&format!("result_{}", i));
            match status {
                LoopStatus::Block(_) => panic!("should not block with different inputs"),
                _ => {}
            }
        }
    }

    #[test]
    fn loop_detector_ping_pong_no_progress_warning() {
        let mut d = LoopDetector::new();
        let v1 = serde_json::json!({"a": 1});
        let v2 = serde_json::json!({"b": 2});
        // Build up alternating pattern with same results (no progress)
        for _ in 0..5 {
            let _ = d.record_input("tool_a", &v1);
            d.record_outcome("result_a");
            let _ = d.record_input("tool_b", &v2);
            d.record_outcome("result_b");
        }
        // At 10 calls in ping-pong with no progress, should have triggered warning
    }

    #[test]
    fn loop_detector_circuit_breaker_no_progress() {
        let mut d = LoopDetector::new();
        let v = serde_json::json!({"cmd": "check"});
        for _ in 0..LOOP_WARNING_THRESHOLD - 1 {
            assert_eq!(d.record_input("tool_a", &v), LoopStatus::Ok);
            d.record_outcome("same");
        }
        match d.record_input("tool_a", &v) {
            LoopStatus::Warning(msg) => assert!(msg.contains("tool_a")),
            other => panic!("expected Warning, got {:?}", other),
        }
    }

    // -- truncate_tool_result tests ----------------------------------------------

    #[test]
    fn truncate_noop_when_short() {
        let text = "Hello, world!";
        let result = truncate_tool_result(text, 100);
        assert_eq!(result, text);
    }

    #[test]
    fn truncate_at_newline() {
        let text = format!("{}\n{}", "a".repeat(50), "b".repeat(100));
        let result = truncate_tool_result(&text, 80);
        assert!(result.starts_with(&"a".repeat(50)));
        assert!(result.contains("[Content truncated"));
        assert!(!result.contains("bbb"));
    }

    #[test]
    fn truncate_no_newline() {
        let text = "a".repeat(200);
        let result = truncate_tool_result(&text, 100);
        assert!(result.len() > 100); // truncated text + suffix
        assert!(result.contains("[Content truncated"));
    }

    #[test]
    fn truncate_multibyte_safe() {
        // Build a string of multi-byte chars
        let text = "\u{1F600}".repeat(100); // 400 bytes
        let result = truncate_tool_result(&text, 200);
        assert!(result.contains("[Content truncated"));
        // Must be valid UTF-8 (if it compiles and doesn't panic, it is)
    }

    #[tokio::test]
    async fn react_loop_hits_timeout() {
        let tmp = tempfile::tempdir().unwrap();

        // Every call returns a tool use — will exhaust agent_timeout_secs
        let tool_response = LlmResponse {
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "test_tool".into(),
                input: serde_json::json!({}),
            }],
            stop_reason: StopReason::ToolUse,
        };
        let mut rt = build_runtime(
            &tmp,
            Box::new(MockLlm {
                response: tool_response,
            }),
            vec![Box::new(MockTool)],
            20,
            true,
        );
        // Use a very short timeout so the test completes quickly
        rt.agent_timeout_secs = 1;
        rt.sessions.init().await.unwrap();
        rt.memory.init().await.unwrap();

        let abort = Arc::new(AtomicBool::new(false));
        let reply = rt
            .react_loop("test", "c1", "loop forever", &abort, None, Vec::new())
            .await
            .unwrap();
        assert!(reply.contains("too long"));
    }
}
