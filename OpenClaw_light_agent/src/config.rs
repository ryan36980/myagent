//! Configuration loading for the OpenClaw Rust Gateway.
//!
//! The configuration file uses **JSON5** syntax (comments, trailing commas,
//! unquoted keys) and is expected at one of the following paths, tried in
//! order:
//!
//! 1. An explicit path passed to [`GatewayConfig::load`].
//! 2. `./openclaw.json` (working directory).
//! 3. `~/.openclaw/openclaw.json` (user home directory).
//!
//! ## Environment variable substitution
//!
//! Before parsing, every occurrence of `${VAR_NAME}` in the raw file text is
//! replaced with the value of the corresponding environment variable.  If the
//! variable is not set the placeholder is replaced with an empty string, which
//! lets optional secrets remain blank during development.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::{GatewayError, Result};

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Root configuration for the gateway.
///
/// Every section is optional and will fall back to sensible defaults when
/// omitted from the file.  The JSON5 field names use camelCase to match the
/// canonical `openclaw.json` format.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GatewayConfig {
    /// LLM provider name (e.g. `"anthropic"`).
    pub provider: String,

    /// Model identifier sent to the LLM provider.
    pub model: String,

    /// Channel-specific configuration (Telegram, etc.).
    pub channels: ChannelsConfig,

    /// Message handling: TTS, STT / media understanding.
    pub messages: MessagesConfig,

    /// Tool allowlist and per-tool settings.
    pub tools: ToolsConfig,

    /// Home Assistant integration settings.
    pub home_assistant: HomeAssistantConfig,

    /// Agent loop settings (system prompt, iteration limits).
    pub agents: AgentConfig,

    /// Session persistence settings.
    pub session: SessionConfig,

    /// Memory persistence settings (long-term memory + daily logs).
    pub memory: MemoryConfig,

    /// Shell command execution settings.
    pub exec: ExecConfig,

    /// Web fetch tool settings (download limits, etc.).
    pub web_fetch: WebFetchConfig,

    /// Web search provider settings.
    pub web_search: WebSearchConfig,

    /// MCP (Model Context Protocol) client settings.
    pub mcp: McpConfig,

    /// Authentication settings (API key vs OAuth).
    pub auth: AuthConfig,

    /// Automatic backup settings.
    pub backup: BackupConfig,

    /// Optional overrides for the LLM provider (base URL, API key env var, etc.).
    #[serde(default, rename = "providerConfig")]
    pub provider_config_override: ProviderConfigOverride,
}

/// Optional per-provider overrides specified in the JSON5 config file.
///
/// These let users point at any OpenAI-compatible endpoint without changing
/// code — just set `providerConfig.baseUrl` and `providerConfig.apiKeyEnv`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProviderConfigOverride {
    /// Override the base URL for the LLM API.
    #[serde(rename = "baseUrl")]
    pub base_url: Option<String>,

    /// Name of the environment variable holding the API key.
    #[serde(rename = "apiKeyEnv")]
    pub api_key_env: Option<String>,

    /// Override max tokens.
    #[serde(rename = "maxTokens")]
    pub max_tokens: Option<u32>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-5-20250929".into(),
            channels: ChannelsConfig::default(),
            messages: MessagesConfig::default(),
            tools: ToolsConfig::default(),
            home_assistant: HomeAssistantConfig::default(),
            agents: AgentConfig::default(),
            session: SessionConfig::default(),
            memory: MemoryConfig::default(),
            exec: ExecConfig::default(),
            web_fetch: WebFetchConfig::default(),
            web_search: WebSearchConfig::default(),
            auth: AuthConfig::default(),
            backup: BackupConfig::default(),
            mcp: McpConfig::default(),
            provider_config_override: ProviderConfigOverride::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider (LLM)
// ---------------------------------------------------------------------------

/// LLM provider settings used to construct a concrete provider implementation
/// (e.g. [`crate::provider::llm::claude::ClaudeProvider`]).
///
/// This struct is **not** part of the JSON file directly; it is assembled by
/// the gateway startup code from [`GatewayConfig`] fields and environment
/// variables.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// API key for the LLM provider.
    pub api_key: String,

    /// Model identifier (e.g. `"claude-sonnet-4-5-20250929"`).
    pub model: String,

    /// Optional maximum token limit for completions.
    pub max_tokens: Option<u32>,

    /// Base URL for OpenAI-compatible providers (e.g. `"https://api.groq.com/openai/v1"`).
    pub base_url: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "claude-sonnet-4-5-20250929".into(),
            max_tokens: None,
            base_url: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// Per-channel configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChannelsConfig {
    /// Telegram channel settings.
    pub telegram: TelegramConfig,

    /// Feishu/Lark channel settings (WebSocket long connection).
    pub feishu: FeishuConfig,

    /// HTTP API channel settings.
    pub http_api: HttpApiConfig,

    /// CLI channel settings.
    pub cli: CliConfig,
}

/// HTTP API channel settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HttpApiConfig {
    /// Whether the HTTP API channel is enabled.
    pub enabled: bool,

    /// Address to listen on (e.g. `"127.0.0.1:8080"`).
    pub listen: String,

    /// Optional Bearer token for API authentication.
    /// When non-empty, all POST and GET /chat/* endpoints require
    /// `Authorization: Bearer <token>`. Empty/unset = no auth.
    pub auth_token: String,
}

impl Default for HttpApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:8080".into(),
            auth_token: String::new(),
        }
    }
}

/// CLI channel settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CliConfig {
    /// Whether the CLI channel is enabled.
    pub enabled: bool,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

/// Feishu/Lark channel settings (WebSocket long connection mode).
///
/// Uses the Feishu Open Platform WebSocket SDK protocol — no public URL needed.
/// The gateway connects outward to Feishu's WSS endpoint, similar to Telegram
/// long polling.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FeishuConfig {
    /// Feishu App ID (e.g. `"cli_xxx"`).
    pub app_id: String,

    /// Feishu App Secret.
    pub app_secret: String,

    /// Feishu API domain. Default: `"https://open.feishu.cn"`.
    /// For Lark (international): `"https://open.larksuite.com"`.
    pub domain: String,

    /// Optional allowlist of Feishu user open_ids. When non-empty only these
    /// users may interact with the bot.
    pub allowed_users: Vec<String>,
}

impl Default for FeishuConfig {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            app_secret: String::new(),
            domain: "https://open.feishu.cn".into(),
            allowed_users: Vec::new(),
        }
    }
}

/// Telegram Bot API settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TelegramConfig {
    /// Bot token obtained from `@BotFather`.
    ///
    /// Defaults to an empty string.  In the canonical config file the value is
    /// `${TELEGRAM_BOT_TOKEN}` so the actual token is injected through the
    /// environment.
    pub bot_token: String,

    /// Optional allowlist of Telegram user IDs.  When non-empty only these
    /// users may interact with the bot.
    pub allowed_users: Vec<i64>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            allowed_users: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Messages (TTS + STT / media understanding)
// ---------------------------------------------------------------------------

/// Settings that govern how inbound and outbound messages are processed
/// (text-to-speech, speech-to-text, media understanding).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MessagesConfig {
    /// Text-to-speech configuration.
    pub tts: TtsConfig,

    /// Media understanding / speech-to-text configuration.
    pub media_understanding: MediaUnderstandingConfig,

    /// Optional prefix template prepended to every outgoing reply.
    /// Supports template variables: `{model}`, `{provider}`, `{thinkingLevel}`.
    pub response_prefix: String,
}

/// Text-to-speech configuration.
///
/// The [`EdgeTtsConfig`] sub-section is wrapped in an `Option` so that
/// provider implementations can distinguish "not configured" from
/// "configured with defaults".
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TtsConfig {
    /// When to auto-generate TTS.
    /// `"inbound"` = reply with voice when the user sends voice,
    /// `"always"` = always attach voice, `"never"` = text only.
    pub auto: String,

    /// TTS provider name (e.g. `"edge"`).
    pub provider: String,

    /// Maximum text length (in characters) that will be sent for synthesis.
    /// Longer texts are truncated.
    pub max_text_length: usize,

    /// Microsoft Edge TTS specific settings.
    /// `None` when the `edge` key is absent from the config file.
    pub edge: Option<EdgeTtsConfig>,

    /// OpenAI TTS specific settings.
    /// `None` when the `openai` key is absent from the config file.
    pub openai: Option<OpenAiTtsConfig>,

    /// ElevenLabs TTS specific settings.
    /// `None` when the `elevenlabs` key is absent from the config file.
    pub elevenlabs: Option<ElevenLabsTtsConfig>,

    /// Volcengine (火山引擎/豆包) TTS specific settings.
    /// `None` when the `volcengine` key is absent from the config file.
    pub volcengine: Option<VolcengineTtsConfig>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            auto: "inbound".into(),
            provider: "edge".into(),
            max_text_length: 500,
            edge: None,
            openai: None,
            elevenlabs: None,
            volcengine: None,
        }
    }
}

/// Configuration for the Microsoft Edge TTS provider.
///
/// All fields are optional so that only the values the user wants to override
/// need to be specified in the config file.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EdgeTtsConfig {
    /// Voice name (e.g. `"zh-CN-XiaoxiaoNeural"`).
    pub voice: Option<String>,

    /// Speech rate adjustment (e.g. `"+10%"`).
    pub rate: Option<String>,

    /// Pitch adjustment (e.g. `"+0Hz"`).
    pub pitch: Option<String>,

    /// Volume adjustment (e.g. `"+0%"`).
    pub volume: Option<String>,

    /// Chromium version for DRM token generation.
    /// Update this when Edge TTS returns 403 errors.
    #[serde(rename = "chromiumVersion")]
    pub chromium_version: Option<String>,
}

impl Default for EdgeTtsConfig {
    fn default() -> Self {
        Self {
            voice: None,
            rate: None,
            pitch: None,
            volume: None,
            chromium_version: None,
        }
    }
}

/// Configuration for the OpenAI-compatible TTS provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenAiTtsConfig {
    /// Base URL for the TTS API (e.g. `"https://api.openai.com/v1"`).
    pub base_url: Option<String>,

    /// Name of the environment variable holding the API key.
    pub api_key_env: String,

    /// Model identifier (e.g. `"tts-1"`, `"tts-1-hd"`, `"gpt-4o-mini-tts"`).
    pub model: String,

    /// Voice name (e.g. `"alloy"`, `"echo"`, `"nova"`, `"shimmer"`).
    pub voice: String,
}

impl Default for OpenAiTtsConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            api_key_env: "OPENAI_API_KEY".into(),
            model: "tts-1".into(),
            voice: "alloy".into(),
        }
    }
}

/// Configuration for the ElevenLabs TTS provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ElevenLabsTtsConfig {
    /// Base URL for the TTS API (e.g. `"https://api.elevenlabs.io/v1"`).
    pub base_url: Option<String>,

    /// Name of the environment variable holding the API key.
    pub api_key_env: String,

    /// Model identifier (e.g. `"eleven_multilingual_v2"`).
    pub model_id: String,

    /// Voice ID (required, obtained from ElevenLabs dashboard).
    pub voice_id: String,
}

impl Default for ElevenLabsTtsConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            api_key_env: "ELEVENLABS_API_KEY".into(),
            model_id: "eleven_multilingual_v2".into(),
            voice_id: String::new(),
        }
    }
}

/// Configuration for the Volcengine (火山引擎/豆包) TTS provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VolcengineTtsConfig {
    /// Base URL for the TTS API.
    pub base_url: Option<String>,

    /// Application ID from Volcengine console.
    pub app_id: String,

    /// Access token for authentication (supports `${ENV_VAR}` substitution in config).
    pub access_token: String,

    /// Cluster / resource ID (e.g. `"volcano_tts"`).
    pub cluster: String,

    /// Voice type ID (e.g. `"BV001_streaming"`).
    pub voice_type: String,

    /// Speech speed ratio (1.0 = normal).
    pub speed_ratio: Option<f32>,

    /// Volume ratio (1.0 = normal).
    pub volume_ratio: Option<f32>,

    /// Pitch ratio (1.0 = normal).
    pub pitch_ratio: Option<f32>,
}

impl Default for VolcengineTtsConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            app_id: String::new(),
            access_token: String::new(),
            cluster: String::new(),
            voice_type: String::new(),
            speed_ratio: None,
            volume_ratio: None,
            pitch_ratio: None,
        }
    }
}

/// Media understanding / STT settings wrapper.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MediaUnderstandingConfig {
    /// Audio transcription (STT) settings.
    pub audio: SttConfig,
}

/// Speech-to-text provider configuration.
///
/// This struct doubles as the constructor input for concrete STT
/// implementations (e.g. [`crate::provider::stt::groq::GroqSttProvider`]).
/// Fields that are provider-specific (like `base_url`) are optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SttConfig {
    /// STT provider name (e.g. `"groq"`, `"volcengine"`, `"doubao"`).
    pub provider: String,

    /// Model identifier (e.g. `"whisper-large-v3-turbo"`).
    /// Optional because the provider implementation supplies a default.
    pub model: Option<String>,

    /// API key for the STT provider.
    pub api_key: String,

    /// Optional base URL override (e.g. for self-hosted Whisper).
    pub base_url: Option<String>,

    /// Volcengine (豆包) STT specific settings.
    /// `None` when the `volcengine` key is absent from the config file.
    pub volcengine: Option<VolcengineSttConfig>,

    /// Google Cloud STT specific settings.
    /// `None` when the `google` key is absent from the config file.
    pub google: Option<GoogleSttConfig>,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: "groq".into(),
            model: Some("whisper-large-v3-turbo".into()),
            api_key: String::new(),
            base_url: None,
            volcengine: None,
            google: None,
        }
    }
}

/// Configuration for the Volcengine (豆包) STT provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VolcengineSttConfig {
    /// Application ID from Volcengine console.
    pub app_id: String,

    /// Access token / access key for authentication.
    pub access_token: String,

    /// Cluster / resource ID (default: `"volc.bigasr.sauc.duration"`).
    pub cluster: String,

    /// WebSocket URL override (default: v3 BigModel endpoint).
    pub ws_url: String,
}

impl Default for VolcengineSttConfig {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            access_token: String::new(),
            cluster: String::new(),
            ws_url: String::new(),
        }
    }
}

/// Configuration for the Google Cloud STT provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GoogleSttConfig {
    /// BCP-47 language code (e.g. `"zh-CN"`, `"en-US"`).
    pub language_code: String,
}

impl Default for GoogleSttConfig {
    fn default() -> Self {
        Self {
            language_code: "zh-CN".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// Tool allowlist and settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Names of tools the agent is permitted to invoke.
    ///
    /// An empty list means *no* tools are available.
    pub allow: Vec<String>,
}

// ---------------------------------------------------------------------------
// Home Assistant
// ---------------------------------------------------------------------------

/// Home Assistant REST API integration settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HomeAssistantConfig {
    /// Base URL of the Home Assistant instance
    /// (e.g. `"http://192.168.1.100:8123"`).
    pub url: String,

    /// Long-lived access token.
    ///
    /// In the canonical config file the value is `${HA_TOKEN}` so the actual
    /// token is injected through the environment.
    pub token: String,
}

impl Default for HomeAssistantConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            token: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Settings that control the agentic tool-use loop.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentConfig {
    /// System prompt prepended to every LLM request.
    pub system_prompt: String,

    /// Agent turn timeout in seconds. The ReAct loop will abort after this
    /// duration, returning a timeout message. Default: 600 (10 minutes).
    /// Aligns with OpenClaw original's `agents.defaults.timeoutSeconds`.
    pub agent_timeout_secs: u64,

    /// Enable automatic conversation compaction via LLM summarization.
    pub auto_compact: bool,

    /// Extended thinking level: `"off"`, `"low"`, `"medium"`, or `"high"`.
    /// Maps to budget_tokens: low=2048, medium=8192, high=32768.
    pub thinking: String,

    /// Compaction ratio (0.0–1.0). Fraction of messages to keep after
    /// compaction. Default: 0.4 (keep 40%, summarize 60%).
    pub compact_ratio: f64,

    /// Followup debounce window in milliseconds. After the agent completes a
    /// turn, if there are pending messages, wait this long to collect more
    /// before coalescing them into a single turn. Default: 2000 (2 seconds).
    pub followup_debounce_ms: u64,

    /// Fallback models tried in order when the primary model fails.
    pub fallback_models: Vec<FallbackModel>,

    /// Paths to context files loaded at startup and injected into the system
    /// prompt (equivalent to SOUL.md in original OpenClaw).
    pub context_files: Vec<String>,

    /// Queue mode for incoming messages during agent processing.
    /// `"interrupt"` (default) — new messages abort the current turn and are
    ///   processed immediately (aligned with original OpenClaw).
    /// `"queue"` — messages are collected and coalesced after the current turn.
    pub queue_mode: String,
}

/// A fallback LLM provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackModel {
    /// Provider type: `"anthropic"`, `"openai"`, etc.
    pub provider: String,

    /// Model identifier.
    pub model: String,

    /// Environment variable name holding the API key.
    pub api_key_env: Option<String>,

    /// Optional base URL override.
    pub base_url: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: concat!(
                "You are a personal assistant running inside OpenClaw.\n",
                "\n",
                "## Tooling\n",
                "Tool names are case-sensitive. Call tools exactly as listed.\n",
                "No limit on tool calls per turn — use as many as needed.\n",
                "For multi-step tasks (writing files, generating content, etc.), ",
                "complete ALL steps in one turn. Do not stop to report progress.\n",
                "CRITICAL: When you decide to perform an action, CALL THE TOOL IMMEDIATELY ",
                "in the same response. NEVER reply with only text saying you will do something ",
                "— that wastes a turn and forces the user to prompt you again. ",
                "Text-only responses that promise future action are FORBIDDEN.\n",
                "User messages during execution are queued and do not interrupt you.\n",
                "If a task is complex or long-running, use sessions_spawn to create a sub-agent.\n",
                "\n",
                "## Tool Call Style\n",
                "Default: do not narrate routine, low-risk tool calls (just call the tool).\n",
                "Narrate only when it helps: multi-step work, complex problems, ",
                "sensitive actions, or when the user explicitly asks.\n",
                "Keep narration brief and value-dense; avoid repeating obvious steps.\n",
                "\n",
                "## Safety\n",
                "Prioritize safety and human oversight over completion; ",
                "if instructions conflict, pause and ask.\n",
                "Never fabricate system limitations or tool availability.\n",
                "\n",
                "## Memory\n",
                "Before answering questions about prior work, decisions, dates, people, or to-do items, ",
                "search your memory first using the memory tool (action: \"search\" or \"read\").\n",
                "Save important facts, preferences, and decisions to memory for future reference.\n",
                "Use scope: \"shared\" for cross-conversation knowledge (device IPs, network config, ",
                "household members, universal preferences). Default scope is per-conversation.\n",
                "\n",
                "## Silent Replies\n",
                "If you have nothing meaningful to say after completing an internal operation, ",
                "reply with exactly \u{1f910} (nothing else). This suppresses the message.",
            ).into(),
            agent_timeout_secs: 900,
            auto_compact: true,
            thinking: "off".into(),
            compact_ratio: 0.4,
            followup_debounce_ms: 2000,
            fallback_models: Vec::new(),
            context_files: Vec::new(),
            queue_mode: "interrupt".into(),
        }
    }
}

impl AgentConfig {
    /// Convert the thinking level string to a budget_tokens value.
    /// Returns `None` for "off" or unrecognized values.
    pub fn thinking_budget(&self) -> Option<u32> {
        match self.thinking.as_str() {
            "low" => Some(2048),
            "medium" => Some(8192),
            "high" => Some(32768),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Session persistence settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionConfig {
    /// Directory where session history files are stored.
    pub dir: String,

    /// Maximum number of raw messages to keep per session file (0 = unlimited).
    /// Hard backstop — prefer `dm_history_limit` for user-turn-based limiting.
    pub history_limit: usize,

    /// Maximum number of **user turns** to retain (matching OpenClaw original
    /// `dmHistoryLimit`).  A "user turn" is a User message containing at least
    /// one `Text` block (tool_result-only messages don't count).
    /// 0 = unlimited.  Default: 20.
    pub dm_history_limit: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dir: "./sessions".into(),
            history_limit: 0,
            dm_history_limit: 20,
        }
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// Memory persistence settings (long-term memory + daily logs).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MemoryConfig {
    /// Directory where per-chat memory files are stored.
    pub dir: String,

    /// Maximum size (bytes) for MEMORY.md before warning the agent to rewrite.
    pub max_memory_bytes: usize,

    /// Token budget (bytes) for context injection (MEMORY.md + recent logs).
    pub max_context_bytes: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            dir: "./memory".into(),
            max_memory_bytes: 4096,
            max_context_bytes: 4096,
        }
    }
}

// ---------------------------------------------------------------------------
// Exec
// ---------------------------------------------------------------------------

/// Shell command execution settings for the `exec` tool.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExecConfig {
    /// Default timeout in seconds (capped at 300).
    pub timeout_secs: u64,

    /// Maximum combined output bytes (stdout + stderr).
    pub max_output_bytes: usize,

    /// Working directory for commands.
    pub work_dir: String,

    /// Directory for reusable skill scripts (auto-added to PATH).
    pub skills_dir: String,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_output_bytes: 8192,
            work_dir: ".".into(),
            skills_dir: "./skills".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Web Fetch
// ---------------------------------------------------------------------------

/// Web fetch tool configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WebFetchConfig {
    /// Maximum download size in bytes. Default: 2 MB.
    /// Increasing this allows fetching larger pages but peak memory is ~2x this value.
    pub max_download_bytes: usize,
}

fn default_max_download_bytes() -> usize {
    2_000_000
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            max_download_bytes: default_max_download_bytes(),
        }
    }
}

// ---------------------------------------------------------------------------
// Web Search
// ---------------------------------------------------------------------------

/// Web search provider configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WebSearchConfig {
    /// Search provider: `"duckduckgo"` (default) or `"brave"`.
    pub provider: String,

    /// Name of the environment variable holding the Brave Search API key.
    pub api_key_env: String,

    /// Maximum number of results to return (default: 5).
    pub max_results: usize,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: "duckduckgo".into(),
            api_key_env: "BRAVE_SEARCH_API_KEY".into(),
            max_results: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// MCP (Model Context Protocol)
// ---------------------------------------------------------------------------

/// MCP client configuration — connects to external MCP servers over stdio.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct McpConfig {
    /// Named MCP server configurations.
    pub servers: HashMap<String, McpServerConfig>,
}

/// Configuration for a single MCP server process.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpServerConfig {
    /// Command to spawn the MCP server process.
    pub command: String,

    /// Arguments to pass to the command.
    pub args: Vec<String>,

    /// Additional environment variables for the server process.
    pub env: HashMap<String, String>,

    /// Timeout for a single tool call (seconds). Default: 60.
    pub timeout_secs: u64,

    /// Maximum output bytes from a tool call. Default: 65536.
    pub max_output_bytes: usize,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            timeout_secs: 60,
            max_output_bytes: 65536,
        }
    }
}

// ---------------------------------------------------------------------------
// Auth (API Key vs OAuth)
// ---------------------------------------------------------------------------

/// Authentication configuration: API key (default) or Anthropic OAuth 2.0.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AuthConfig {
    /// Authentication mode: `"api_key"` (default) or `"oauth"`.
    pub mode: String,

    /// OAuth client ID.
    pub client_id: String,

    /// File path for persisting OAuth tokens (default `"./auth_tokens.json"`).
    pub token_file: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: "api_key".into(),
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(),
            token_file: "./auth_tokens.json".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

/// Automatic backup configuration.
///
/// When enabled the gateway periodically archives data files into a local
/// directory using `tar czf`.  Old backups beyond `retention_days` are
/// automatically pruned.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackupConfig {
    /// Master switch — set to `false` to disable automatic backups entirely.
    pub enabled: bool,

    /// Directory where backup archives are stored.
    pub dir: String,

    /// Minimum interval between two consecutive backups (hours).
    pub interval_hours: u64,

    /// Backups older than this many days are automatically deleted.
    pub retention_days: u64,

    /// Maximum total size of all backups in MB.  When exceeded the oldest
    /// backups are deleted until the total is within budget.
    pub max_size_mb: u64,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: "./backups".into(),
            interval_hours: 24,
            retention_days: 7,
            max_size_mb: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// Loading logic
// ---------------------------------------------------------------------------

/// Well-known file name used when probing default locations.
const CONFIG_FILENAME: &str = "openclaw.json";

impl GatewayConfig {
    /// Load configuration from an explicit path, or fall back to probing
    /// default locations (`./openclaw.json`, `~/.openclaw/openclaw.json`).
    ///
    /// Environment variable placeholders (`${VAR}`) in the raw JSON5 text are
    /// expanded before parsing.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayError::Config`] when the file cannot be found or
    /// contains invalid JSON5, and [`GatewayError::Io`] on filesystem errors.
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let path = match explicit_path {
            Some(p) => {
                if p.exists() {
                    p.to_path_buf()
                } else {
                    return Err(GatewayError::Config(format!(
                        "config file not found: {}",
                        p.display()
                    )));
                }
            }
            None => resolve_default_path()?,
        };

        debug!("loading config from {}", path.display());

        let raw = std::fs::read_to_string(&path).map_err(|e| {
            GatewayError::Config(format!("failed to read {}: {e}", path.display()))
        })?;

        let expanded = substitute_env_vars(&raw);

        let config: GatewayConfig = json5::from_str(&expanded).map_err(|e| {
            GatewayError::Config(format!("failed to parse {}: {e}", path.display()))
        })?;

        Ok(config)
    }

    /// Build a [`ProviderConfig`] from the loaded gateway config and the
    /// environment.
    ///
    /// Uses a built-in defaults table for known providers (anthropic, groq,
    /// deepseek, glm, openai).  User overrides in `providerConfig` take
    /// precedence.  Unknown provider names fall through to the `openai-compat`
    /// path which requires explicit `baseUrl` and `apiKeyEnv`.
    pub fn provider_config(&self) -> Result<ProviderConfig> {
        // Known-provider defaults: (api_key_env, base_url, default_model)
        let (default_key_env, default_base_url, default_model) = match self.provider.as_str() {
            "anthropic" => (
                "ANTHROPIC_API_KEY",
                None,
                "claude-sonnet-4-5-20250929",
            ),
            "groq" => (
                "GROQ_API_KEY",
                Some("https://api.groq.com/openai/v1"),
                "llama-3.3-70b-versatile",
            ),
            "deepseek" => (
                "DEEPSEEK_API_KEY",
                Some("https://api.deepseek.com/v1"),
                "deepseek-chat",
            ),
            "glm" => (
                "GLM_API_KEY",
                Some("https://api.z.ai/api/paas/v4"),
                "glm-5",
            ),
            "openai" => (
                "OPENAI_API_KEY",
                Some("https://api.openai.com/v1"),
                "gpt-4o",
            ),
            _ => (
                // Unknown / "openai-compat": no defaults, user must provide overrides
                "",
                None,
                "",
            ),
        };

        let ovr = &self.provider_config_override;

        // Resolve API key env var name (override > default)
        let key_env = ovr
            .api_key_env
            .as_deref()
            .unwrap_or(default_key_env);

        if key_env.is_empty() {
            return Err(GatewayError::Config(format!(
                "provider {:?} requires providerConfig.apiKeyEnv to be set",
                self.provider
            )));
        }

        // In OAuth mode for Anthropic, the API key is not required.
        let api_key = if self.auth.mode == "oauth" && self.provider == "anthropic" {
            std::env::var(key_env).unwrap_or_default()
        } else {
            std::env::var(key_env).map_err(|_| {
                GatewayError::Config(format!(
                    "environment variable {} is not set (required by provider {:?})",
                    key_env, self.provider
                ))
            })?
        };

        // Resolve model (config top-level > default)
        let model = if self.model != "claude-sonnet-4-5-20250929" || self.provider == "anthropic" {
            // User explicitly set a model, or it's the anthropic default
            self.model.clone()
        } else if !default_model.is_empty() {
            default_model.into()
        } else {
            return Err(GatewayError::Config(format!(
                "provider {:?} requires an explicit model in config",
                self.provider
            )));
        };

        // Resolve base URL (override > default)
        let base_url = ovr
            .base_url
            .clone()
            .or_else(|| default_base_url.map(String::from));

        let max_tokens = ovr.max_tokens;

        Ok(ProviderConfig {
            api_key,
            model,
            max_tokens,
            base_url,
        })
    }

    /// Build an [`SttConfig`] enriched with provider-specific credentials
    /// from the environment.
    pub fn stt_config(&self) -> Result<SttConfig> {
        let mut cfg = self.messages.media_understanding.audio.clone();
        match cfg.provider.as_str() {
            "volcengine" | "doubao" => {
                // Back-fill access_token from environment if not set in config
                if let Some(ref mut vc) = cfg.volcengine {
                    if vc.access_token.is_empty() {
                        vc.access_token =
                            std::env::var("VOLCENGINE_ACCESS_TOKEN").unwrap_or_default();
                    }
                } else {
                    // No volcengine section — create one from env
                    cfg.volcengine = Some(VolcengineSttConfig {
                        access_token: std::env::var("VOLCENGINE_ACCESS_TOKEN")
                            .unwrap_or_default(),
                        ..VolcengineSttConfig::default()
                    });
                }
            }
            "google" => {
                if cfg.api_key.is_empty() {
                    cfg.api_key = std::env::var("GOOGLE_STT_API_KEY").unwrap_or_default();
                }
            }
            _ => {
                // Default: Groq-compatible API key
                if cfg.api_key.is_empty() {
                    cfg.api_key = Self::groq_api_key().unwrap_or_default();
                }
            }
        }
        Ok(cfg)
    }

    /// Return the Groq API key, reading from the environment.
    pub fn groq_api_key() -> Result<String> {
        std::env::var("GROQ_API_KEY").map_err(|_| {
            GatewayError::Config("GROQ_API_KEY environment variable is not set".into())
        })
    }
}

/// Try well-known locations and return the first path that exists.
fn resolve_default_path() -> Result<PathBuf> {
    // 1. ./openclaw.json
    let cwd_path = PathBuf::from(CONFIG_FILENAME);
    if cwd_path.exists() {
        return Ok(cwd_path);
    }

    // 2. ~/.openclaw/openclaw.json
    if let Some(home) = home_dir() {
        let home_path = home.join(".openclaw").join(CONFIG_FILENAME);
        if home_path.exists() {
            return Ok(home_path);
        }
    }

    Err(GatewayError::Config(format!(
        "no config file found; tried ./{CONFIG_FILENAME} and ~/.openclaw/{CONFIG_FILENAME}"
    )))
}

/// Best-effort home directory lookup (works on Windows and Unix).
fn home_dir() -> Option<PathBuf> {
    // Prefer the standard HOME var; fall back to USERPROFILE on Windows.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Replace every `${VAR_NAME}` in `input` with the corresponding environment
/// variable value.  Unknown variables are replaced with the empty string and a
/// warning is logged.
fn substitute_env_vars(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            // Consume the '{'
            chars.next();

            // Collect the variable name until '}'
            let mut var_name = String::new();
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                var_name.push(c);
            }

            match std::env::var(&var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => {
                    warn!(
                        "environment variable {var_name} is not set; \
                         substituting empty string"
                    );
                }
            }
        } else {
            result.push(ch);
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
    fn env_substitution_replaces_known_var() {
        std::env::set_var("_TEST_OPENCLAW_VAR", "hello");
        let out = substitute_env_vars("token=${_TEST_OPENCLAW_VAR}!");
        assert_eq!(out, "token=hello!");
        std::env::remove_var("_TEST_OPENCLAW_VAR");
    }

    #[test]
    fn env_substitution_unknown_var_becomes_empty() {
        let out = substitute_env_vars("key=${_SURELY_UNSET_12345}");
        assert_eq!(out, "key=");
    }

    #[test]
    fn env_substitution_no_placeholders() {
        let out = substitute_env_vars("plain text, no vars");
        assert_eq!(out, "plain text, no vars");
    }

    #[test]
    fn env_substitution_multiple_vars() {
        std::env::set_var("_TEST_A", "foo");
        std::env::set_var("_TEST_B", "bar");
        let out = substitute_env_vars("${_TEST_A}/${_TEST_B}");
        assert_eq!(out, "foo/bar");
        std::env::remove_var("_TEST_A");
        std::env::remove_var("_TEST_B");
    }

    #[test]
    fn env_substitution_adjacent_dollars() {
        // A bare '$' not followed by '{' should be kept as-is.
        let out = substitute_env_vars("price is $5");
        assert_eq!(out, "price is $5");
    }

    #[test]
    fn default_config_is_well_formed() {
        let cfg = GatewayConfig::default();
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.agents.agent_timeout_secs, 900);
        assert_eq!(cfg.session.history_limit, 0);
        assert!(cfg.tools.allow.is_empty());
        assert_eq!(cfg.memory.dir, "./memory");
        assert_eq!(cfg.memory.max_memory_bytes, 4096);
    }

    #[test]
    fn parse_example_json5() {
        // Minimal JSON5 snippet covering all top-level keys.
        let json5 = r#"{
            provider: "anthropic",
            model: "claude-sonnet-4-5-20250929",
            channels: {
                telegram: {
                    botToken: "tok123",
                    allowedUsers: [111, 222]
                }
            },
            messages: {
                tts: {
                    auto: "always",
                    provider: "edge",
                    maxTextLength: 300,
                    edge: { voice: "en-US-GuyNeural", rate: "+0%" }
                },
                mediaUnderstanding: {
                    audio: { provider: "groq", model: "whisper-large-v3-turbo" }
                }
            },
            tools: { allow: ["ha_control", "web_fetch"] },
            homeAssistant: { url: "http://ha:8123", token: "secret" },
            agents: {
                systemPrompt: "Hi",
                agentTimeoutSecs: 30
            },
            session: { dir: "/tmp/sess", historyLimit: 50 }
        }"#;

        let cfg: GatewayConfig = json5::from_str(json5).expect("parse failed");
        assert_eq!(cfg.channels.telegram.bot_token, "tok123");
        assert_eq!(cfg.channels.telegram.allowed_users, vec![111, 222]);
        assert_eq!(cfg.messages.tts.auto, "always");
        assert_eq!(cfg.messages.tts.max_text_length, 300);
        assert_eq!(
            cfg.messages.tts.edge.as_ref().unwrap().voice.as_deref(),
            Some("en-US-GuyNeural")
        );
        assert_eq!(
            cfg.messages.media_understanding.audio.provider,
            "groq"
        );
        assert_eq!(
            cfg.messages.media_understanding.audio.model.as_deref(),
            Some("whisper-large-v3-turbo")
        );
        assert_eq!(cfg.tools.allow, vec!["ha_control", "web_fetch"]);
        assert_eq!(cfg.home_assistant.url, "http://ha:8123");
        assert_eq!(cfg.agents.system_prompt, "Hi");
        assert_eq!(cfg.agents.agent_timeout_secs, 30);
        assert_eq!(cfg.session.dir, "/tmp/sess");
        assert_eq!(cfg.session.history_limit, 50);
    }

    #[test]
    fn parse_empty_json5_uses_defaults() {
        let cfg: GatewayConfig = json5::from_str("{}").expect("parse failed");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.messages.tts.provider, "edge");
        assert_eq!(
            cfg.messages.media_understanding.audio.model.as_deref(),
            Some("whisper-large-v3-turbo")
        );
        assert!(cfg.tools.allow.is_empty());
        assert!(cfg.messages.tts.edge.is_none());
    }

    #[test]
    fn provider_config_defaults() {
        let pc = ProviderConfig::default();
        assert!(pc.api_key.is_empty());
        assert_eq!(pc.model, "claude-sonnet-4-5-20250929");
        assert!(pc.max_tokens.is_none());
        assert!(pc.base_url.is_none());
    }

    #[test]
    fn stt_config_defaults() {
        let sc = SttConfig::default();
        assert_eq!(sc.provider, "groq");
        assert_eq!(sc.model.as_deref(), Some("whisper-large-v3-turbo"));
        assert!(sc.api_key.is_empty());
        assert!(sc.base_url.is_none());
    }

    #[test]
    fn provider_config_deepseek_defaults() {
        std::env::set_var("DEEPSEEK_API_KEY", "sk-deepseek-test");
        let cfg: GatewayConfig =
            json5::from_str(r#"{ provider: "deepseek" }"#).unwrap();
        let pc = cfg.provider_config().unwrap();
        assert_eq!(
            pc.base_url.as_deref(),
            Some("https://api.deepseek.com/v1")
        );
        assert_eq!(pc.model, "deepseek-chat");
        assert_eq!(pc.api_key, "sk-deepseek-test");
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    #[test]
    fn provider_config_groq_defaults() {
        std::env::set_var("GROQ_API_KEY", "gsk-groq-test");
        let cfg: GatewayConfig =
            json5::from_str(r#"{ provider: "groq" }"#).unwrap();
        let pc = cfg.provider_config().unwrap();
        assert_eq!(
            pc.base_url.as_deref(),
            Some("https://api.groq.com/openai/v1")
        );
        assert_eq!(pc.model, "llama-3.3-70b-versatile");
        assert_eq!(pc.api_key, "gsk-groq-test");
        std::env::remove_var("GROQ_API_KEY");
    }

    #[test]
    fn provider_config_unknown_requires_overrides() {
        // Unknown provider with no overrides → error about missing apiKeyEnv
        let cfg: GatewayConfig =
            json5::from_str(r#"{ provider: "my-custom-llm" }"#).unwrap();
        let err = cfg.provider_config().unwrap_err();
        match err {
            GatewayError::Config(msg) => {
                assert!(
                    msg.contains("apiKeyEnv"),
                    "should mention apiKeyEnv: {msg}"
                );
            }
            other => panic!("expected Config error, got: {other:?}"),
        }
    }

    #[test]
    fn provider_config_override_precedence() {
        // Groq has defaults, but providerConfig overrides should win
        std::env::set_var("MY_CUSTOM_KEY", "custom-key-value");
        let cfg: GatewayConfig = json5::from_str(
            r#"{
                provider: "groq",
                model: "my-custom-model",
                providerConfig: {
                    baseUrl: "https://my-proxy.example.com/v1",
                    apiKeyEnv: "MY_CUSTOM_KEY",
                    maxTokens: 1024
                }
            }"#,
        )
        .unwrap();
        let pc = cfg.provider_config().unwrap();
        assert_eq!(
            pc.base_url.as_deref(),
            Some("https://my-proxy.example.com/v1"),
            "override base_url should win over groq default"
        );
        assert_eq!(pc.api_key, "custom-key-value");
        assert_eq!(pc.model, "my-custom-model");
        assert_eq!(pc.max_tokens, Some(1024));
        std::env::remove_var("MY_CUSTOM_KEY");
    }

    #[test]
    fn provider_config_local_ollama() {
        std::env::set_var("OLLAMA_KEY", "ollama");
        let cfg: GatewayConfig = json5::from_str(
            r#"{
                provider: "ollama",
                model: "qwen2.5:14b",
                providerConfig: {
                    baseUrl: "http://localhost:11434/v1",
                    apiKeyEnv: "OLLAMA_KEY"
                }
            }"#,
        )
        .unwrap();
        let pc = cfg.provider_config().unwrap();
        assert_eq!(
            pc.base_url.as_deref(),
            Some("http://localhost:11434/v1"),
            "should use Ollama base URL"
        );
        assert_eq!(pc.model, "qwen2.5:14b");
        assert_eq!(pc.api_key, "ollama");
        std::env::remove_var("OLLAMA_KEY");
    }

    #[test]
    fn provider_config_local_vllm() {
        std::env::set_var("VLLM_KEY", "token-abc");
        let cfg: GatewayConfig = json5::from_str(
            r#"{
                provider: "vllm",
                model: "Qwen/Qwen2.5-72B-Instruct",
                providerConfig: {
                    baseUrl: "http://192.168.1.100:8000/v1",
                    apiKeyEnv: "VLLM_KEY",
                    maxTokens: 4096
                }
            }"#,
        )
        .unwrap();
        let pc = cfg.provider_config().unwrap();
        assert_eq!(
            pc.base_url.as_deref(),
            Some("http://192.168.1.100:8000/v1")
        );
        assert_eq!(pc.model, "Qwen/Qwen2.5-72B-Instruct");
        assert_eq!(pc.api_key, "token-abc");
        assert_eq!(pc.max_tokens, Some(4096));
        std::env::remove_var("VLLM_KEY");
    }

    #[test]
    fn provider_config_local_no_model_fails() {
        // Custom provider without explicit model → should error
        std::env::set_var("LLAMACPP_KEY", "dummy");
        let cfg: GatewayConfig = json5::from_str(
            r#"{
                provider: "llamacpp",
                providerConfig: {
                    baseUrl: "http://localhost:8080/v1",
                    apiKeyEnv: "LLAMACPP_KEY"
                }
            }"#,
        )
        .unwrap();
        let err = cfg.provider_config().unwrap_err();
        match err {
            GatewayError::Config(msg) => {
                assert!(
                    msg.contains("model"),
                    "should mention model requirement: {msg}"
                );
            }
            other => panic!("expected Config error, got: {other:?}"),
        }
        std::env::remove_var("LLAMACPP_KEY");
    }

    #[test]
    fn openai_tts_config_defaults() {
        let cfg = OpenAiTtsConfig::default();
        assert_eq!(cfg.api_key_env, "OPENAI_API_KEY");
        assert_eq!(cfg.model, "tts-1");
        assert_eq!(cfg.voice, "alloy");
        assert!(cfg.base_url.is_none());
    }

    #[test]
    fn elevenlabs_tts_config_defaults() {
        let cfg = ElevenLabsTtsConfig::default();
        assert_eq!(cfg.api_key_env, "ELEVENLABS_API_KEY");
        assert_eq!(cfg.model_id, "eleven_multilingual_v2");
        assert!(cfg.voice_id.is_empty());
        assert!(cfg.base_url.is_none());
    }

    #[test]
    fn parse_openai_tts_config() {
        let json5 = r#"{
            messages: {
                tts: {
                    provider: "openai",
                    auto: "always",
                    openai: {
                        baseUrl: "https://custom.api.com/v1",
                        apiKeyEnv: "MY_TTS_KEY",
                        model: "tts-1-hd",
                        voice: "nova"
                    }
                }
            }
        }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.messages.tts.provider, "openai");
        let openai = cfg.messages.tts.openai.unwrap();
        assert_eq!(openai.base_url.unwrap(), "https://custom.api.com/v1");
        assert_eq!(openai.api_key_env, "MY_TTS_KEY");
        assert_eq!(openai.model, "tts-1-hd");
        assert_eq!(openai.voice, "nova");
    }

    #[test]
    fn parse_elevenlabs_tts_config() {
        let json5 = r#"{
            messages: {
                tts: {
                    provider: "elevenlabs",
                    elevenlabs: {
                        apiKeyEnv: "MY_EL_KEY",
                        modelId: "eleven_turbo_v2_5",
                        voiceId: "abc123def"
                    }
                }
            }
        }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.messages.tts.provider, "elevenlabs");
        let el = cfg.messages.tts.elevenlabs.unwrap();
        assert_eq!(el.api_key_env, "MY_EL_KEY");
        assert_eq!(el.model_id, "eleven_turbo_v2_5");
        assert_eq!(el.voice_id, "abc123def");
    }

    #[test]
    fn tts_auto_tagged_mode() {
        let json5 = r#"{ messages: { tts: { auto: "tagged" } } }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.messages.tts.auto, "tagged");
    }

    #[test]
    fn parse_response_prefix() {
        let json5 = r#"{ messages: { responsePrefix: "[{model}] " } }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.messages.response_prefix, "[{model}] ");
    }

    #[test]
    fn response_prefix_default_empty() {
        let cfg: GatewayConfig = json5::from_str("{}").unwrap();
        assert!(cfg.messages.response_prefix.is_empty());
    }

    #[test]
    fn thinking_budget_mapping() {
        let mut cfg = AgentConfig::default();
        assert_eq!(cfg.thinking_budget(), None);

        cfg.thinking = "low".into();
        assert_eq!(cfg.thinking_budget(), Some(2048));

        cfg.thinking = "medium".into();
        assert_eq!(cfg.thinking_budget(), Some(8192));

        cfg.thinking = "high".into();
        assert_eq!(cfg.thinking_budget(), Some(32768));

        cfg.thinking = "invalid".into();
        assert_eq!(cfg.thinking_budget(), None);
    }

    #[test]
    fn parse_thinking_config() {
        let json5 = r#"{ agents: { thinking: "medium", compactRatio: 0.3 } }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.agents.thinking, "medium");
        assert!((cfg.agents.compact_ratio - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn web_search_config_defaults() {
        let cfg = WebSearchConfig::default();
        assert_eq!(cfg.provider, "duckduckgo");
        assert_eq!(cfg.api_key_env, "BRAVE_SEARCH_API_KEY");
        assert_eq!(cfg.max_results, 5);
    }

    #[test]
    fn parse_web_search_config() {
        let json5 = r#"{ webSearch: { provider: "brave", apiKeyEnv: "MY_BRAVE_KEY", maxResults: 3 } }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.web_search.provider, "brave");
        assert_eq!(cfg.web_search.api_key_env, "MY_BRAVE_KEY");
        assert_eq!(cfg.web_search.max_results, 3);
    }

    #[test]
    fn memory_config_defaults() {
        let cfg = MemoryConfig::default();
        assert_eq!(cfg.dir, "./memory");
        assert_eq!(cfg.max_memory_bytes, 4096);
        assert_eq!(cfg.max_context_bytes, 4096);
    }

    #[test]
    fn memory_config_custom() {
        let cfg: GatewayConfig = json5::from_str(
            r#"{ memory: { dir: "/data/mem", maxMemoryBytes: 8192, maxContextBytes: 2048 } }"#,
        )
        .unwrap();
        assert_eq!(cfg.memory.dir, "/data/mem");
        assert_eq!(cfg.memory.max_memory_bytes, 8192);
        assert_eq!(cfg.memory.max_context_bytes, 2048);
    }

    #[test]
    fn auth_config_defaults() {
        let cfg = AuthConfig::default();
        assert_eq!(cfg.mode, "api_key");
        assert_eq!(cfg.client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(cfg.token_file, "./auth_tokens.json");
    }

    #[test]
    fn parse_auth_config_oauth() {
        let json5 = r#"{
            auth: {
                mode: "oauth",
                clientId: "my-custom-client-id",
                tokenFile: "/data/tokens.json"
            }
        }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert_eq!(cfg.auth.mode, "oauth");
        assert_eq!(cfg.auth.client_id, "my-custom-client-id");
        assert_eq!(cfg.auth.token_file, "/data/tokens.json");
    }

    #[test]
    fn parse_empty_config_has_default_auth() {
        let cfg: GatewayConfig = json5::from_str("{}").unwrap();
        assert_eq!(cfg.auth.mode, "api_key");
    }

    #[test]
    fn backup_config_defaults() {
        let cfg = BackupConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.dir, "./backups");
        assert_eq!(cfg.interval_hours, 24);
        assert_eq!(cfg.retention_days, 7);
        assert_eq!(cfg.max_size_mb, 200);
    }

    #[test]
    fn parse_backup_config() {
        let json5 = r#"{
            backup: {
                enabled: false,
                dir: "/data/backups",
                intervalHours: 12,
                retentionDays: 30,
                maxSizeMb: 500
            }
        }"#;
        let cfg: GatewayConfig = json5::from_str(json5).unwrap();
        assert!(!cfg.backup.enabled);
        assert_eq!(cfg.backup.dir, "/data/backups");
        assert_eq!(cfg.backup.interval_hours, 12);
        assert_eq!(cfg.backup.retention_days, 30);
        assert_eq!(cfg.backup.max_size_mb, 500);
    }

    #[test]
    fn parse_empty_config_has_default_backup() {
        let cfg: GatewayConfig = json5::from_str("{}").unwrap();
        assert!(cfg.backup.enabled);
        assert_eq!(cfg.backup.dir, "./backups");
    }

    #[test]
    fn oauth_mode_skips_api_key_requirement() {
        // In OAuth mode for Anthropic, missing API key should NOT error
        let cfg: GatewayConfig = json5::from_str(
            r#"{ provider: "anthropic", auth: { mode: "oauth" } }"#,
        )
        .unwrap();
        let result = cfg.provider_config();
        assert!(result.is_ok(), "OAuth mode should not require ANTHROPIC_API_KEY");
    }
}
