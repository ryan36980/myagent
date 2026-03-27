//! OpenClaw Light — entry point.
//!
//! Initializes the single-threaded tokio runtime, loads configuration,
//! creates provider/channel instances, and runs the main dispatch loop
//! with graceful shutdown support.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use openclaw_light::agent::react_loop::{AgentRuntime, ArcVoiceDownloader};
use openclaw_light::backup;
use openclaw_light::auth::{AuthMode, TokenStore};
use openclaw_light::channel::cli::CliChannel;
use openclaw_light::channel::feishu::FeishuChannel;
use openclaw_light::channel::http_api::HttpApiChannel;
use openclaw_light::channel::telegram::TelegramChannel;
use openclaw_light::channel::Channel;
use openclaw_light::channel::types::{IncomingMessage, MessageContent};
use openclaw_light::config::GatewayConfig;
use openclaw_light::memory::MemoryStore;
use openclaw_light::provider;
use openclaw_light::provider::llm::claude::ClaudeProvider;
use openclaw_light::provider::llm::failover::FailoverLlmProvider;
use openclaw_light::provider::llm::openai_compat::OpenAiCompatProvider;
use openclaw_light::provider::stt::groq::GroqSttProvider;
use openclaw_light::provider::stt::google::GoogleSttProvider;
use openclaw_light::provider::stt::volcengine::VolcengineSttProvider;
use openclaw_light::provider::tts::edge::EdgeTtsProvider;
use openclaw_light::provider::tts::elevenlabs::ElevenLabsTtsProvider;
use openclaw_light::provider::tts::openai::OpenAiTtsProvider;
use openclaw_light::provider::tts::volcengine::VolcengineTtsProvider;
use openclaw_light::session::SessionStore;
use openclaw_light::tools::cron::{
    cron_matches, load_all_tasks, save_all_tasks, schedule_at_matches, CronTool,
};
use openclaw_light::tools::exec::ExecTool;
use openclaw_light::tools::file::{FileEditTool, FileFindTool, FileReadTool, FileWriteTool};
use openclaw_light::tools::get_time::GetTimeTool;
use openclaw_light::tools::ha_control::HaControlTool;
use openclaw_light::tools::mcp::{McpClient, McpProxyTool};
use openclaw_light::tools::memory::{ChatContext, MemoryTool};
use openclaw_light::tools::agent_tool::{create_session_tools, SubAgentResult};
use openclaw_light::tools::web_fetch::WebFetchTool;
use openclaw_light::tools::backup::BackupTool;
use openclaw_light::tools::web_search::WebSearchTool;
use openclaw_light::tools::ToolRegistry;

/// Abort command keywords (aligned with OpenClaw original).
const ABORT_COMMANDS: &[&str] = &["/stop", "stop", "esc", "abort", "cancel"];

/// Check if a message is an abort command.
fn is_abort_command(msg: &IncomingMessage) -> bool {
    if let MessageContent::Text(ref text) = msg.content {
        let trimmed = text.trim().to_lowercase();
        ABORT_COMMANDS.contains(&trimmed.as_str())
    } else {
        false
    }
}

/// Coalesce multiple pending messages into a single `IncomingMessage`.
///
/// Filters out abort commands, extracts text from each message type, and wraps
/// multiple messages in a context header so the agent sees them as one turn.
/// Returns `None` if all messages were abort commands or empty.
fn coalesce_pending_messages(pending: &[IncomingMessage]) -> Option<IncomingMessage> {
    let texts: Vec<&str> = pending
        .iter()
        .filter(|m| !is_abort_command(m))
        .filter_map(|m| match &m.content {
            MessageContent::Text(t) if !t.trim().is_empty() => Some(t.as_str()),
            MessageContent::Voice { .. } => Some("[voice message]"),
            MessageContent::Image { caption, .. } => {
                caption.as_deref().or(Some("[image]"))
            }
            _ => None,
        })
        .collect();

    if texts.is_empty() {
        return None;
    }

    // Use the first non-abort message as the template
    let template = pending.iter().find(|m| !is_abort_command(m))?;

    let coalesced_text = if texts.len() == 1 {
        texts[0].to_string()
    } else {
        let mut buf = String::from(
            "[The user sent follow-up messages while you were working:]\n",
        );
        for t in &texts {
            buf.push_str(t);
            buf.push('\n');
        }
        buf
    };

    Some(IncomingMessage {
        channel: template.channel.clone(),
        chat_id: template.chat_id.clone(),
        sender_id: template.sender_id.clone(),
        content: MessageContent::Text(coalesced_text),
        timestamp: template.timestamp,
    })
}

/// Dispatch an agent response through the appropriate channel.
async fn dispatch_response(
    channel: &dyn Channel,
    chat_id: &str,
    response: openclaw_light::channel::types::OutgoingMessage,
) {
    if let Some(ref audio) = response.voice {
        if let Err(e) = channel.send_voice(chat_id, audio).await {
            error!(error = %e, chat_id, "failed to send voice");
            // Fall back to text
            if let Some(ref text) = response.text {
                let _ = channel.send_text(chat_id, text).await;
            }
        }
    } else if let Some(ref text) = response.text {
        if let Err(e) = channel.send_text(chat_id, text).await {
            error!(error = %e, chat_id, "failed to send text");
        }
    }

    // Close any streaming connection (SSE done event for HTTP API; no-op for others)
    if let Err(e) = channel.close_stream(chat_id).await {
        error!(error = %e, chat_id, "failed to close stream");
    }
}

/// Handle `/auth` commands for OAuth flow management.
/// Returns `Some(reply)` if the message was an auth command, `None` otherwise.
async fn handle_auth_command(
    text: &str,
    token_store: &Arc<Mutex<TokenStore>>,
) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with("/auth") {
        return None;
    }

    let args = trimmed.strip_prefix("/auth").unwrap_or("").trim();

    let reply = match args {
        "" => {
            // Start auth flow
            let url = token_store.lock().await.start_auth();
            // Escape underscores for Telegram Markdown (client_id contains _)
            let escaped_url = url.replace('_', "\\_");
            format!(
                "Open this link to authorize:\n{}\n\nAfter authorizing, \
                 send the code back: /auth YOUR\\_CODE",
                escaped_url
            )
        }
        "status" => token_store.lock().await.status(),
        "reset" => {
            token_store.lock().await.clear();
            "OAuth tokens cleared. Use /auth to start a new authorization.".into()
        }
        code => {
            // Strip URL fragment if user copied code#state from callback page
            let clean_code = code.split('#').next().unwrap_or(code).trim();
            // Exchange authorization code
            match token_store.lock().await.exchange_code(clean_code).await {
                Ok(()) => "Authentication successful! You can now use the bot.".into(),
                Err(e) => format!("Authentication failed: {e}"),
            }
        }
    };

    Some(reply)
}

/// Per-chat queue manager: routes messages to per-chat worker tasks.
///
/// Each chat gets its own mpsc channel + tokio::spawn'd worker task.
/// Different chats process concurrently; same-chat messages are sequential.
struct ChatQueueManager {
    queues: Mutex<HashMap<String, mpsc::Sender<IncomingMessage>>>,
}

impl ChatQueueManager {
    fn new() -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
        }
    }

    /// Enqueue a message for processing. Spawns a new worker if needed.
    async fn enqueue(
        &self,
        msg: IncomingMessage,
        agent: Arc<AgentRuntime>,
        channel: Arc<dyn Channel>,
        token_store: Option<Arc<Mutex<TokenStore>>>,
    ) {
        let chat_key = format!("{}:{}", msg.channel, msg.chat_id);
        let mut queues = self.queues.lock().await;

        // Clean up closed channels
        queues.retain(|_, tx| !tx.is_closed());

        let tx = queues
            .entry(chat_key.clone())
            .or_insert_with(|| {
                let (tx, rx) = mpsc::channel::<IncomingMessage>(16);
                let agent = agent.clone();
                let channel = channel.clone();
                let ts = token_store.clone();
                tokio::spawn(async move {
                    chat_worker(agent, channel, rx, ts).await;
                });
                tx
            });

        if let Err(e) = tx.send(msg).await {
            error!(chat_key, error = %e, "failed to enqueue message");
        }
    }
}

/// Per-chat worker: processes messages sequentially for one chat, supports abort.
///
/// Supports two queue modes:
/// - `"interrupt"` (default): new user messages abort the current agent turn
///   and are processed immediately (aligned with original OpenClaw).
/// - `"queue"`: messages are collected during processing and coalesced after.
async fn chat_worker(
    agent: Arc<AgentRuntime>,
    channel: Arc<dyn Channel>,
    mut rx: mpsc::Receiver<IncomingMessage>,
    token_store: Option<Arc<Mutex<TokenStore>>>,
) {
    let interrupt_mode = agent.queue_mode == "interrupt";
    let mut overflow_msg: Option<IncomingMessage> = None;

    loop {
        // Get next message: either from overflow (interrupt) or from channel
        let msg = if let Some(m) = overflow_msg.take() {
            m
        } else {
            match rx.recv().await {
                Some(m) => m,
                None => break,
            }
        };

        let chat_id = msg.chat_id.clone();

        // Intercept /auth commands when OAuth mode is active
        if let Some(ref store) = token_store {
            if let MessageContent::Text(ref text) = msg.content {
                if let Some(reply) = handle_auth_command(text, store).await {
                    let _ = channel.send_text(&chat_id, &reply).await;
                    continue;
                }
            }
        }

        // Check if this is an abort command (nothing running → ignore)
        if is_abort_command(&msg) {
            continue;
        }

        // Start agent processing with abort support
        let abort = Arc::new(AtomicBool::new(false));
        let downloader = ArcVoiceDownloader::new(channel.clone());
        let agent_clone = agent.clone();
        let abort_clone = abort.clone();

        let agent_fut = agent_clone.handle(&msg, downloader, abort_clone, Some(channel.clone()));
        tokio::pin!(agent_fut);

        let mut typing_interval =
            tokio::time::interval(std::time::Duration::from_secs(6));
        let mut pending: Vec<IncomingMessage> = Vec::new();

        let result = loop {
            tokio::select! {
                result = &mut agent_fut => break result,
                _ = typing_interval.tick() => {
                    let _ = channel.send_typing(&chat_id).await;
                }
                Some(next_msg) = rx.recv() => {
                    if is_abort_command(&next_msg) {
                        abort.store(true, Ordering::Relaxed);
                        let _ = channel.send_text(&chat_id, "Stopping...").await;
                    } else if interrupt_mode {
                        // Interrupt: abort current, process new message next
                        abort.store(true, Ordering::Relaxed);
                        pending.clear();
                        pending.push(next_msg);
                    } else {
                        // Queue mode: collect for later coalesce
                        pending.push(next_msg);
                    }
                }
            }
        };

        match result {
            Ok(response) => {
                dispatch_response(channel.as_ref(), &chat_id, response).await;
            }
            Err(e) => {
                error!(error = %e, chat_id, "agent error");
                let _ = channel
                    .send_text(&chat_id, &format!("Sorry, an error occurred: {}", e))
                    .await;
            }
        }

        // Process pending messages
        if !pending.is_empty() {
            if interrupt_mode {
                // Interrupt mode: process the interrupting message as next turn
                overflow_msg = Some(pending.remove(0));
                // Remaining pending (if any) are discarded — interrupt semantics
            } else {
                // Queue mode: debounce + coalesce
                let debounce_ms = agent.followup_debounce_ms;
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_millis(debounce_ms);
                let mut aborted = false;

                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline) => break,
                        Some(next_msg) = rx.recv() => {
                            if is_abort_command(&next_msg) {
                                pending.clear();
                                aborted = true;
                                break;
                            }
                            pending.push(next_msg);
                        }
                    }
                }

                if aborted || pending.is_empty() {
                    continue;
                }

                // Coalesce pending messages into one
                if let Some(coalesced) = coalesce_pending_messages(&pending) {
                    let abort = Arc::new(AtomicBool::new(false));
                    let downloader = ArcVoiceDownloader::new(channel.clone());
                    let coalesced_chat_id = coalesced.chat_id.clone();

                    let mut typing_interval =
                        tokio::time::interval(std::time::Duration::from_secs(6));

                    let agent_fut =
                        agent.handle(&coalesced, downloader, abort, Some(channel.clone()));
                    tokio::pin!(agent_fut);

                    let result = loop {
                        tokio::select! {
                            result = &mut agent_fut => break result,
                            _ = typing_interval.tick() => {
                                let _ = channel.send_typing(&coalesced_chat_id).await;
                            }
                        }
                    };

                    match result {
                        Ok(response) => {
                            dispatch_response(channel.as_ref(), &coalesced_chat_id, response).await;
                        }
                        Err(e) => {
                            error!(error = %e, chat_id = %coalesced_chat_id, "agent error (followup)");
                            let _ = channel
                                .send_text(
                                    &coalesced_chat_id,
                                    &format!("Sorry, an error occurred: {}", e),
                                )
                                .await;
                        }
                    }
                }
            }
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .compact()
        .init();

    info!("OpenClaw Light starting...");

    // Load configuration
    let config = GatewayConfig::load(None)?;
    info!(provider = %config.provider, model = %config.model, "config loaded");

    // Build shared HTTP client (connection pool reuse)
    let http_client = reqwest::Client::builder()
        .user_agent("OpenClaw-Light/0.1")
        .timeout(std::time::Duration::from_secs(600))
        .pool_max_idle_per_host(4)
        .build()?;

    // Build OAuth token store (if configured)
    let token_store: Option<Arc<Mutex<TokenStore>>> =
        if config.auth.mode == "oauth" && config.provider == "anthropic" {
            let mut store = TokenStore::new(
                config.auth.client_id.clone(),
                None,
                std::path::PathBuf::from(&config.auth.token_file),
                http_client.clone(),
            );
            store.load().await.ok(); // Load existing tokens if available
            let store = Arc::new(Mutex::new(store));
            info!("OAuth mode enabled (token file: {})", config.auth.token_file);
            Some(store)
        } else {
            None
        };

    // Build providers
    let provider_config = config.provider_config()?;
    let thinking_budget = config.agents.thinking_budget();

    let auth_mode = match &token_store {
        Some(store) => AuthMode::OAuth(store.clone()),
        None => AuthMode::ApiKey(provider_config.api_key.clone()),
    };

    let primary_llm: Box<dyn provider::llm::LlmProvider> = match config.provider.as_str() {
        "anthropic" => Box::new(
            ClaudeProvider::with_auth(http_client.clone(), &provider_config, auth_mode)
                .with_thinking_budget(thinking_budget),
        ),
        _ => Box::new(OpenAiCompatProvider::new(http_client.clone(), &provider_config)),
    };

    // Build failover provider chain if fallback models are configured
    let llm: Box<dyn provider::llm::LlmProvider> =
        if config.agents.fallback_models.is_empty() {
            primary_llm
        } else {
            let mut providers: Vec<Box<dyn provider::llm::LlmProvider>> = vec![primary_llm];
            for fb in &config.agents.fallback_models {
                let api_key = fb
                    .api_key_env
                    .as_deref()
                    .and_then(|env| std::env::var(env).ok())
                    .unwrap_or_default();
                let fb_config = openclaw_light::config::ProviderConfig {
                    api_key: api_key.clone(),
                    model: fb.model.clone(),
                    max_tokens: None,
                    base_url: fb.base_url.clone(),
                };
                let fb_provider: Box<dyn provider::llm::LlmProvider> = match fb.provider.as_str()
                {
                    "anthropic" => Box::new(
                        ClaudeProvider::with_auth(
                            http_client.clone(),
                            &fb_config,
                            openclaw_light::auth::AuthMode::ApiKey(api_key),
                        )
                        .with_thinking_budget(None),
                    ),
                    _ => Box::new(OpenAiCompatProvider::new(http_client.clone(), &fb_config)),
                };
                providers.push(fb_provider);
            }
            info!(
                fallback_count = providers.len() - 1,
                "failover chain configured"
            );
            Box::new(FailoverLlmProvider::new(providers))
        };

    let stt_config = config.stt_config()?;
    info!(stt_provider = %stt_config.provider, "STT provider selected");
    let stt: Box<dyn provider::stt::SttProvider> = match stt_config.provider.as_str() {
        "volcengine" | "doubao" => {
            let vc = stt_config.volcengine.clone().unwrap_or_default();
            Box::new(VolcengineSttProvider::new(&vc)?)
        }
        "google" => Box::new(GoogleSttProvider::new(http_client.clone(), &stt_config)),
        _ => Box::new(GroqSttProvider::new(http_client.clone(), &stt_config)),
    };

    let tts: Box<dyn provider::tts::TtsProvider> = match config.messages.tts.provider.as_str() {
        "openai" => {
            let openai_cfg = config.messages.tts.openai.clone().unwrap_or_default();
            Box::new(OpenAiTtsProvider::new(http_client.clone(), &openai_cfg)?)
        }
        "elevenlabs" => {
            let el_cfg = config.messages.tts.elevenlabs.clone().unwrap_or_default();
            Box::new(ElevenLabsTtsProvider::new(http_client.clone(), &el_cfg)?)
        }
        "volcengine" | "doubao" => {
            let vc_cfg = config.messages.tts.volcengine.clone().unwrap_or_default();
            Box::new(VolcengineTtsProvider::new(http_client.clone(), &vc_cfg)?)
        }
        _ => Box::new(EdgeTtsProvider::new(&config.messages.tts)),
    };

    // Build shared chat context (used by memory + cron tools)
    let chat_context = Arc::new(Mutex::new(ChatContext {
        channel: String::new(),
        chat_id: String::new(),
    }));

    // Build memory store
    let memory_store = Arc::new(MemoryStore::new(
        &config.memory.dir,
        config.memory.max_memory_bytes,
        config.memory.max_context_bytes,
    ));
    memory_store.init().await?;
    info!(dir = %config.memory.dir, "memory store initialized");

    // Build tools
    let mut tool_list: Vec<Box<dyn openclaw_light::tools::Tool>> = Vec::new();
    let allowed = &config.tools.allow;

    if allowed.contains(&"ha_control".to_string()) && !config.home_assistant.url.is_empty() {
        tool_list.push(Box::new(HaControlTool::new(
            http_client.clone(),
            &config.home_assistant,
        )));
    }
    if allowed.contains(&"web_fetch".to_string()) {
        tool_list.push(Box::new(WebFetchTool::new(
            http_client.clone(),
            config.web_fetch.max_download_bytes,
        )));
    }
    if allowed.contains(&"get_time".to_string()) {
        tool_list.push(Box::new(GetTimeTool::new()));
    }
    if allowed.contains(&"cron".to_string()) {
        tool_list.push(Box::new(CronTool::new(
            &config.session.dir,
            chat_context.clone(),
        )));
    }
    if allowed.contains(&"memory".to_string()) {
        let tool_memory_store = MemoryStore::new(
            &config.memory.dir,
            config.memory.max_memory_bytes,
            config.memory.max_context_bytes,
        );
        tool_list.push(Box::new(MemoryTool::new(
            tool_memory_store,
            chat_context.clone(),
        )));
    }
    if allowed.contains(&"exec".to_string()) {
        tool_list.push(Box::new(ExecTool::new(&config.exec)));
    }
    if allowed.contains(&"web_search".to_string()) {
        tool_list.push(Box::new(WebSearchTool::from_config(
            http_client.clone(),
            &config.web_search,
        )));
    }
    if allowed.contains(&"backup".to_string()) {
        tool_list.push(Box::new(BackupTool::new(&config.backup)));
    }
    if allowed.contains(&"file_read".to_string()) {
        tool_list.push(Box::new(FileReadTool::new()));
    }
    if allowed.contains(&"file_write".to_string()) {
        tool_list.push(Box::new(FileWriteTool::new()));
    }
    if allowed.contains(&"file_edit".to_string()) {
        tool_list.push(Box::new(FileEditTool::new()));
    }
    if allowed.contains(&"file_find".to_string()) {
        tool_list.push(Box::new(FileFindTool::new()));
    }

    // Session tools (sessions_spawn/list/history/send): 4 tools sharing one registry
    let (announce_tx, mut announce_rx) = mpsc::channel::<SubAgentResult>(16);
    let agent_cell = if allowed.iter().any(|t| t.starts_with("sessions_")) {
        let (session_tools, cell) = create_session_tools(announce_tx);
        for tool in session_tools {
            // Only register tools that are in the allow list
            if allowed.contains(&tool.name().to_string()) {
                tool_list.push(tool);
            }
        }
        Some(cell)
    } else {
        None
    };

    // MCP: start configured servers and register their tools
    let mut mcp_clients: Vec<Arc<Mutex<McpClient>>> = Vec::new();

    for (name, server_cfg) in &config.mcp.servers {
        match McpClient::start(server_cfg).await {
            Ok(client) => {
                let client = Arc::new(Mutex::new(client));
                let tools_result = client.lock().await.list_tools().await;
                match tools_result {
                    Ok(tools) => {
                        let tool_count = tools.len();
                        for tool_def in &tools {
                            tool_list.push(Box::new(McpProxyTool::new(
                                client.clone(),
                                name,
                                tool_def,
                            )));
                        }
                        mcp_clients.push(client);
                        info!(server = %name, tool_count, "MCP server connected");
                    }
                    Err(e) => {
                        warn!(server = %name, error = %e, "failed to list MCP tools");
                        client.lock().await.shutdown().await;
                    }
                }
            }
            Err(e) => warn!(server = %name, error = %e, "failed to start MCP server"),
        }
    }

    info!(tools = ?allowed, "tools registered");

    let tool_registry = ToolRegistry::new(tool_list);

    // Build session store
    let sessions = SessionStore::new(
        &config.session.dir,
        config.session.history_limit,
        config.session.dm_history_limit,
    );
    sessions.init().await?;

    // Load context files (SOUL.md equivalent) at startup
    let mut context_files_content = String::new();
    for path in &config.agents.context_files {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let truncated = &content[..content.len().min(20_000)];
                context_files_content.push_str(&format!("### {}\n{}\n\n", path, truncated));
            }
            Err(e) => warn!(path, error = %e, "failed to load context file"),
        }
    }
    context_files_content.truncate(150_000);
    if !context_files_content.is_empty() {
        info!(
            files = config.agents.context_files.len(),
            bytes = context_files_content.len(),
            "context files loaded"
        );
    }

    // Build agent runtime (Arc-wrapped for sharing across chat workers)
    let tts_auto_mode = config.messages.tts.auto.clone();
    let agent = Arc::new(AgentRuntime {
        llm,
        stt,
        tts,
        tools: tool_registry,
        sessions,
        memory: memory_store,
        chat_context: chat_context.clone(),
        system_prompt: config.agents.system_prompt.clone(),
        agent_timeout_secs: config.agents.agent_timeout_secs,
        tts_auto_mode,
        auto_compact: config.agents.auto_compact,
        compact_ratio: config.agents.compact_ratio,
        response_prefix: config.messages.response_prefix.clone(),
        provider_name: config.provider.clone(),
        model_name: config.model.clone(),
        thinking_level: config.agents.thinking.clone(),
        followup_debounce_ms: config.agents.followup_debounce_ms,
        context_files_content,
        queue_mode: config.agents.queue_mode.clone(),
    });

    // Inject AgentRuntime into sub-agent tool's OnceCell
    if let Some(cell) = agent_cell {
        let _ = cell.set(agent.clone());
    }

    // Build channels based on configuration (Arc-wrapped for sharing)
    let telegram_enabled = !config.channels.telegram.bot_token.is_empty();
    let feishu_enabled = !config.channels.feishu.app_id.is_empty();
    let http_enabled = config.channels.http_api.enabled;
    let cli_enabled = config.channels.cli.enabled;

    let telegram: Arc<TelegramChannel> =
        Arc::new(TelegramChannel::new(http_client.clone(), &config.channels.telegram));
    if telegram_enabled {
        info!("Telegram channel enabled");
    }

    let feishu: Option<Arc<FeishuChannel>> = if feishu_enabled {
        info!("Feishu channel enabled");
        Some(Arc::new(FeishuChannel::new(
            http_client.clone(),
            &config.channels.feishu,
        )))
    } else {
        None
    };

    let http_api: Option<Arc<HttpApiChannel>> = if http_enabled {
        match HttpApiChannel::new(
            &config.channels.http_api,
            Some(config.session.dir.clone()),
        )
        .await
        {
            Ok(ch) => Some(Arc::new(ch)),
            Err(e) => {
                error!(error = %e, "failed to start HTTP API channel");
                None
            }
        }
    } else {
        None
    };

    let cli: Option<Arc<CliChannel>> = if cli_enabled {
        info!("CLI channel enabled");
        Some(Arc::new(CliChannel::new()))
    } else {
        None
    };

    // Per-chat queue manager for concurrent message processing
    let queue_manager = ChatQueueManager::new();

    // Cron file path
    let cron_file = format!("{}/cron.json", config.session.dir);

    // Cron check interval (60 seconds)
    let mut cron_interval = tokio::time::interval(std::time::Duration::from_secs(60));
    // First tick fires immediately; skip it so we don't run cron at startup
    cron_interval.tick().await;

    // Main dispatch loop with graceful shutdown
    info!("entering main dispatch loop (Ctrl+C to stop)");

    loop {
        tokio::select! {
            // Poll Telegram for new messages
            poll_result = telegram.poll(), if telegram_enabled => {
                match poll_result {
                    Ok(messages) => {
                        for msg in messages {
                            queue_manager.enqueue(
                                msg,
                                agent.clone(),
                                telegram.clone() as Arc<dyn Channel>,
                                token_store.clone(),
                            ).await;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Telegram poll error");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }

            // Poll Feishu for new messages (WebSocket-backed)
            poll_result = async { feishu.as_ref().unwrap().poll().await }, if feishu.is_some() => {
                match poll_result {
                    Ok(messages) => {
                        let ch = feishu.as_ref().unwrap().clone() as Arc<dyn Channel>;
                        for msg in messages {
                            queue_manager.enqueue(
                                msg,
                                agent.clone(),
                                ch.clone(),
                                token_store.clone(),
                            ).await;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Feishu poll error");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }

            // Poll HTTP API for new requests
            poll_result = async { http_api.as_ref().unwrap().poll().await }, if http_api.is_some() => {
                match poll_result {
                    Ok(messages) => {
                        let ch = http_api.as_ref().unwrap().clone() as Arc<dyn Channel>;
                        for msg in messages {
                            queue_manager.enqueue(
                                msg,
                                agent.clone(),
                                ch.clone(),
                                token_store.clone(),
                            ).await;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "HTTP API poll error");
                    }
                }
            }

            // Poll CLI for input
            poll_result = async { cli.as_ref().unwrap().poll().await }, if cli.is_some() => {
                match poll_result {
                    Ok(messages) => {
                        if messages.is_empty() && cli_enabled {
                            // EOF on stdin — exit cleanly
                            info!("CLI stdin closed, shutting down");
                            break;
                        }
                        let ch = cli.as_ref().unwrap().clone() as Arc<dyn Channel>;
                        for msg in messages {
                            queue_manager.enqueue(
                                msg,
                                agent.clone(),
                                ch.clone(),
                                token_store.clone(),
                            ).await;
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "CLI poll error");
                    }
                }
            }

            // Cron tick: check scheduled tasks every 60s
            _ = cron_interval.tick() => {
                let now = chrono::Local::now();
                let now_minute = now.timestamp() / 60;

                let mut tasks = load_all_tasks(&cron_file).await;
                let mut changed = false;
                let mut to_delete: Vec<String> = Vec::new();

                for task in tasks.iter_mut() {
                    if task.last_run.map_or(false, |lr| lr / 60 == now_minute) {
                        continue;
                    }

                    // Determine if this task should fire: schedule_at first, then cron_expr
                    let should_fire = if let Some(ref at) = task.schedule_at {
                        schedule_at_matches(at, &now)
                    } else if !task.cron_expr.is_empty() {
                        cron_matches(&task.cron_expr, &now)
                    } else {
                        false
                    };

                    if !should_fire {
                        continue;
                    }

                    info!(
                        task_id = %task.id,
                        desc = %task.description,
                        "cron task triggered"
                    );

                    // Feature 10: Isolated execution — use temp chat_id
                    let effective_chat_id = if task.isolated {
                        format!("_cron_isolated_{}_{}", task.id, now.timestamp())
                    } else {
                        task.chat_id.clone()
                    };

                    {
                        let mut ctx = chat_context.lock().await;
                        ctx.channel = task.channel.clone();
                        ctx.chat_id = effective_chat_id.clone();
                    }

                    let msg = IncomingMessage {
                        channel: task.channel.clone(),
                        chat_id: effective_chat_id,
                        sender_id: "cron".to_string(),
                        content: MessageContent::Text(task.command.clone()),
                        timestamp: now.timestamp(),
                    };

                    // Route cron responses to the appropriate channel
                    let downloader = ArcVoiceDownloader::new(telegram.clone() as Arc<dyn Channel>);
                    let cron_abort = Arc::new(AtomicBool::new(false));
                    match agent.handle(&msg, downloader, cron_abort, None).await {
                        Ok(response) => {
                            // Feature 9: Delivery mode dispatch
                            match task.delivery_mode.as_str() {
                                "none" => {
                                    debug!(task_id = %task.id, "cron result silenced (delivery_mode=none)");
                                }
                                "webhook" => {
                                    if let Some(ref url) = task.webhook_url {
                                        // SSRF check before posting
                                        match openclaw_light::tools::web_fetch::validate_url_ssrf(url).await {
                                            Ok(()) => {
                                                let payload = serde_json::json!({
                                                    "task_id": task.id,
                                                    "description": task.description,
                                                    "response": response.text,
                                                });
                                                let wh_result = http_client
                                                    .post(url)
                                                    .json(&payload)
                                                    .timeout(std::time::Duration::from_secs(5))
                                                    .send()
                                                    .await;
                                                if let Err(e) = wh_result {
                                                    warn!(error = %e, task_id = %task.id, "webhook delivery failed");
                                                }
                                            }
                                            Err(e) => {
                                                warn!(error = %e, task_id = %task.id, "webhook URL blocked by SSRF check");
                                            }
                                        }
                                    } else {
                                        warn!(task_id = %task.id, "delivery_mode=webhook but no webhook_url set");
                                    }
                                }
                                _ => {
                                    // "announce" — send to channel (existing behavior)
                                    if let Some(ref text) = response.text {
                                        if !task.chat_id.is_empty() {
                                            let send_result = match task.channel.as_str() {
                                                "feishu" if feishu.is_some() => {
                                                    feishu.as_ref().unwrap().send_text(&task.chat_id, text).await
                                                }
                                                "http_api" if http_api.is_some() => {
                                                    http_api.as_ref().unwrap().send_text(&task.chat_id, text).await
                                                }
                                                "cli" if cli.is_some() => {
                                                    cli.as_ref().unwrap().send_text(&task.chat_id, text).await
                                                }
                                                _ => telegram.send_text(&task.chat_id, text).await,
                                            };
                                            if let Err(e) = send_result {
                                                warn!(error = %e, task_id = %task.id, "failed to send cron response");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, task_id = %task.id, "cron task execution failed");
                        }
                    }

                    task.last_run = Some(now.timestamp());
                    changed = true;

                    // Feature 6: Delete after run
                    if task.delete_after_run {
                        to_delete.push(task.id.clone());
                        debug!(task_id = %task.id, "marking one-shot task for deletion");
                    }
                }

                // Remove tasks marked for deletion
                if !to_delete.is_empty() {
                    tasks.retain(|t| !to_delete.contains(&t.id));
                    changed = true;
                }

                if changed {
                    if let Err(e) = save_all_tasks(&cron_file, &tasks).await {
                        error!(error = %e, "failed to save cron tasks after execution");
                    }
                }

                // Automatic backup check
                match backup::maybe_run(&config.backup).await {
                    Ok(Some(msg)) => info!("{}", msg),
                    Ok(None) => {}
                    Err(e) => warn!("backup failed: {}", e),
                }
            }

            // Sub-agent completion: route result as synthetic message
            Some(result) = announce_rx.recv() => {
                let announce_text = format!(
                    "[Sub-agent {} completed]\n{}",
                    result.run_id,
                    result.text.chars().take(2000).collect::<String>()
                );
                let synthetic = IncomingMessage {
                    channel: result.channel.clone(),
                    chat_id: result.chat_id.clone(),
                    sender_id: "_system".into(),
                    content: MessageContent::Text(announce_text),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                let ch: Arc<dyn Channel> = match result.channel.as_str() {
                    "feishu" if feishu.is_some() => {
                        feishu.as_ref().unwrap().clone() as Arc<dyn Channel>
                    }
                    "http_api" if http_api.is_some() => {
                        http_api.as_ref().unwrap().clone() as Arc<dyn Channel>
                    }
                    "cli" if cli.is_some() => {
                        cli.as_ref().unwrap().clone() as Arc<dyn Channel>
                    }
                    _ => telegram.clone() as Arc<dyn Channel>,
                };
                queue_manager.enqueue(
                    synthetic,
                    agent.clone(),
                    ch,
                    token_store.clone(),
                ).await;
            }

            // Graceful shutdown on Ctrl+C
            _ = tokio::signal::ctrl_c() => {
                info!("received Ctrl+C, shutting down gracefully...");
                break;
            }
        }
    }

    // Shut down MCP servers
    for client in &mcp_clients {
        client.lock().await.shutdown().await;
    }

    info!("OpenClaw Light stopped.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_text_msg(text: &str) -> IncomingMessage {
        IncomingMessage {
            channel: "test".into(),
            chat_id: "c1".into(),
            sender_id: "u1".into(),
            content: MessageContent::Text(text.into()),
            timestamp: 1000,
        }
    }

    // -- is_abort_command tests -----------------------------------------------

    #[test]
    fn abort_command_recognized() {
        for cmd in ABORT_COMMANDS {
            let msg = make_text_msg(cmd);
            assert!(is_abort_command(&msg), "should recognize {:?}", cmd);
        }
    }

    #[test]
    fn abort_command_case_insensitive() {
        let msg = make_text_msg("  /Stop  ");
        assert!(is_abort_command(&msg));
    }

    #[test]
    fn abort_command_rejects_normal_text() {
        let msg = make_text_msg("hello");
        assert!(!is_abort_command(&msg));
    }

    #[test]
    fn abort_command_rejects_voice() {
        let msg = IncomingMessage {
            channel: "test".into(),
            chat_id: "c1".into(),
            sender_id: "u1".into(),
            content: MessageContent::Voice {
                file_ref: "f1".into(),
                mime: "audio/ogg".into(),
            },
            timestamp: 1000,
        };
        assert!(!is_abort_command(&msg));
    }

    // -- coalesce_pending_messages tests --------------------------------------

    #[test]
    fn coalesce_empty_returns_none() {
        assert!(coalesce_pending_messages(&[]).is_none());
    }

    #[test]
    fn coalesce_single_text_passthrough() {
        let msgs = vec![make_text_msg("好了没")];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => assert_eq!(t, "好了没"),
            _ => panic!("expected Text"),
        }
        assert_eq!(result.channel, "test");
        assert_eq!(result.chat_id, "c1");
    }

    #[test]
    fn coalesce_multiple_texts_wrapped() {
        let msgs = vec![make_text_msg("好了没"), make_text_msg("加油")];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => {
                assert!(t.contains("[The user sent follow-up messages"));
                assert!(t.contains("好了没"));
                assert!(t.contains("加油"));
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn coalesce_filters_abort_commands() {
        let msgs = vec![
            make_text_msg("/stop"),
            make_text_msg("好了没"),
            make_text_msg("abort"),
        ];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => {
                assert_eq!(t, "好了没");
                assert!(!t.contains("stop"));
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn coalesce_all_abort_returns_none() {
        let msgs = vec![make_text_msg("/stop"), make_text_msg("cancel")];
        assert!(coalesce_pending_messages(&msgs).is_none());
    }

    #[test]
    fn coalesce_voice_becomes_placeholder() {
        let msgs = vec![IncomingMessage {
            channel: "test".into(),
            chat_id: "c1".into(),
            sender_id: "u1".into(),
            content: MessageContent::Voice {
                file_ref: "f1".into(),
                mime: "audio/ogg".into(),
            },
            timestamp: 1000,
        }];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => assert_eq!(t, "[voice message]"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn coalesce_image_with_caption() {
        let msgs = vec![IncomingMessage {
            channel: "test".into(),
            chat_id: "c1".into(),
            sender_id: "u1".into(),
            content: MessageContent::Image {
                file_ref: "f1".into(),
                mime: "image/jpeg".into(),
                caption: Some("看这个".into()),
            },
            timestamp: 1000,
        }];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => assert_eq!(t, "看这个"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn coalesce_image_without_caption() {
        let msgs = vec![IncomingMessage {
            channel: "test".into(),
            chat_id: "c1".into(),
            sender_id: "u1".into(),
            content: MessageContent::Image {
                file_ref: "f1".into(),
                mime: "image/jpeg".into(),
                caption: None,
            },
            timestamp: 1000,
        }];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => assert_eq!(t, "[image]"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn coalesce_mixed_types() {
        let msgs = vec![
            make_text_msg("快点"),
            IncomingMessage {
                channel: "test".into(),
                chat_id: "c1".into(),
                sender_id: "u1".into(),
                content: MessageContent::Voice {
                    file_ref: "f1".into(),
                    mime: "audio/ogg".into(),
                },
                timestamp: 1001,
            },
            make_text_msg("好了吗"),
        ];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => {
                assert!(t.contains("[The user sent follow-up messages"));
                assert!(t.contains("快点"));
                assert!(t.contains("[voice message]"));
                assert!(t.contains("好了吗"));
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn coalesce_empty_text_skipped() {
        let msgs = vec![make_text_msg(""), make_text_msg("  "), make_text_msg("有用的")];
        let result = coalesce_pending_messages(&msgs).unwrap();
        match result.content {
            MessageContent::Text(t) => assert_eq!(t, "有用的"),
            _ => panic!("expected Text"),
        }
    }
}
