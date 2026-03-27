# OpenClaw Rust Gateway Design Documentation

> Version: 0.1.0 | Target Platform: 200MB RAM Embedded Linux (small box) | Runtime Memory: ~3MB
>
> The Rust lightweight edition of OpenClaw — a universal personal AI assistant with extreme resource optimization.
> Supports multiple LLM backends, multiple channels (Telegram/Feishu/HTTP/CLI), tool calls, long-term memory, and scheduled tasks.
> Not limited to any specific use case — suitable for smart home, development assistance, information retrieval, scheduling, and more.

---

## Documentation Navigation

| Document | Sections | Contents |
|----------|----------|----------|
| **[Core Specifications & Trait Definitions](core-traits.md)** | Ch. 1–2 | Rust project conventions, dependency list, error handling, memory optimization, five-layer Trait definitions, unified message types, AgentRuntime |
| **[Channel Layer Design](channels.md)** | Ch. 3 | Telegram / Feishu / CLI / HTTP API implementation, multi-channel concurrent dispatch, ChatQueueManager |
| **[Module Design & API Protocols](modules.md)** | Ch. 4–5 | Detailed module design (config / error / LLM / STT / TTS / ReAct / Tools / Session), API protocol specifications |
| **[Operations: Memory, Config, Build, Test](operations.md)** | Ch. 6–9 | Memory budget, configuration system, Docker reproducible builds, cross-compilation, deployment, testing strategy |
| **[Subsystems: Memory, Cron, Extensions, OAuth](subsystems.md)** | Ch. 10–13 | Long-term memory system, Cron auto-execution, exec / web_search / MCP client, Anthropic OAuth 2.0 |
| **[Advanced Features](advanced.md)** | Ch. 14–17 | Concurrency & streaming preview, model Failover, loop detection, multimodal & sub-Agent, Followup Debounce, System Prompt enhancements |
