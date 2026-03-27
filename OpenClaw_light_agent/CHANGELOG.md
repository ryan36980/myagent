# 更新日志

格式遵循 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)。

## [未发布]

## [0.1.0] - 2026-02-25

首次正式发布。

### 新增
- 5 层 trait 抽象：Channel、LlmProvider、SttProvider、TtsProvider、Tool
- 通道：Telegram（长轮询）、飞书/Lark（WebSocket 长连接）、CLI、HTTP API（SSE streaming）
- LLM 提供商：Anthropic Claude（流式 + 扩展思考）、OpenAI 兼容（DeepSeek / Groq 等）
- TTS 提供商：Edge（免费）、OpenAI、ElevenLabs、火山引擎
- STT 提供商：Groq Whisper、火山引擎（豆包）、Google Cloud STT
- ReAct Agent 循环（时间超时 + 循环检测 + 全局断路器）
- 工具：web_fetch（SSRF 防护 + HTML→Text + 128KB + 分页）、web_search（DuckDuckGo + Brave）、get_time、cron、memory、exec、file_read / file_write / file_edit / file_find、backup
- MCP 客户端（stdio JSON-RPC）+ 代理工具桥接
- 记忆系统：per-chat MEMORY.md + 每日日志 + 搜索 + 上下文注入 + 召回指引
- 定时任务系统：5 字段 cron + 一次性调度 + 多投递模式 + 隔离执行
- 会话存储：JSONL + 滑动窗口 + 截断后清理
- **异步子 Agent 架构**：4 个独立工具（sessions_spawn / sessions_list / sessions_history / sessions_send）。spawn 通过 tokio::spawn 非阻塞执行，完成后自动回报结果。最多 8 个并发，task_local 递归守卫。
- **LLM Failover 链**：主模型失败时自动切换备选提供商，指数退避冷却。
- **SSE 流式输出**：实时流式输出到 Telegram（消息编辑）、HTTP API（SSE）、CLI（标准输出）。StreamingWriter 节流更新。
- **Web Chat SSE 流式输出**：HTTP API 通道新增 `POST /chat/stream` SSE 端点，实时推送增量文本（delta）、typing 指示和完成事件。
- **内嵌 Web Chat UI**：`GET /` 返回编译进二进制的单文件 HTML 聊天页面（~10KB），零外部依赖。支持 SSE 实时流式、dark/light 主题跟随、简单 Markdown 渲染、刷新恢复历史、响应式移动端适配。
- **会话历史端点**：`GET /chat/history?chat_id=xxx` 从 JSONL session 读取历史。
- **HTTP API Bearer Token 认证**：`httpApi.authToken` 可选配置，非空时所有 POST 和 GET /chat/* 端点需 Authorization 头。
- **CORS 支持**：所有 HTTP API 响应添加 `Access-Control-Allow-Origin: *` 等头，`OPTIONS` 预检返回 204。
- **火山引擎（豆包）STT 提供商**：WebSocket 二进制协议连接火山引擎大模型 ASR（v3 bigmodel）。支持 WAV/OGG/PCM 格式，自动剥离 WAV 文件头。
- **轮次限制**（dmHistoryLimit）：仅计算包含文本内容的用户消息为一"轮"，默认 20 轮。
- **中断模式**（interrupt queue mode）：新用户消息中断当前 Agent 执行并立即处理。
- **Context Pruning**：自动裁剪老旧工具输出（>4000 字符 → 保留头 1500 + 尾 1500），节省上下文窗口。
- **瞬态错误重试**：网络/过载/5xx 错误自动 2.5 秒退避重试。
- **全局断路器**：30 次工具调用无进展后终止 Agent。
- **Context Files**：启动时加载自定义知识文件（SOUL.md 等价）到系统提示词。
- **追加消息防抖**：可配置窗口（默认 2000ms），快速连续消息合并为一次 Agent 处理。
- 图片/视觉支持：Telegram 照片和文档 → base64 → 多模态 LLM（Anthropic + OpenAI）
- 并行工具执行：`join_all` 协作式并发
- ChatQueueManager：per-chat 串行处理 + 待处理缓冲区
- Anthropic OAuth 2.0（PKCE）认证
- 扩展思考支持（off/low/medium/high）
- 自动压缩 + 紧急渐进式降级
- 响应前缀 + 模板变量
- **Windows 交叉编译**：`docker-build.sh windows` 一键生成 Windows exe（mingw 静态链接，无外部依赖）。
- **统一构建脚本**：`scripts/docker-build.sh` 支持 4 个目标（aarch64 / armv7 / x86_64 / windows），`--verify` 可复现性校验。
- **预编译二进制发布**：[Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) 页提供全平台预编译下载。
- Docker 多阶段构建（scratch + busybox:musl，<8MB）
- 容器安全：只读文件系统、丢弃全部 capabilities、非 root 用户、环境变量隔离
- 完整配置指南 + 内存监控报告

### 变更
- Docker 内存限制：32m → 10m（经监控验证：空闲 ~1.3 MiB，峰值 <8 MiB）
- 子 Agent 超时：移除 LLM 自行传入的 timeout 参数，统一使用全局 agentTimeoutSecs（900s）
- 默认 agentTimeoutSecs：600 → 900

### 修复
- **Edge TTS DRM 认证**：添加 Sec-MS-GEC token、muid cookie 和必需 HTTP headers，解决微软 DRM 验证导致的 403 错误。
- **流式输出 TTS 语音回复**：修复流式模式下 TTS 语音不发送的问题。
- 子 Agent 超时（120s 而非 900s）：移除 LLM 自行传入的 timeout 参数。
- StreamingWriter `stop()` 在缓冲区 < 20 字符时静默丢弃文本：改为异步，停止前刷新缓冲区。
- 空 LLM 响应（EndTurn 无文本）发送了字面 `"(no response)"`：返回空字符串，由 `handle()` 过滤。
- 循环阻断遗漏其他 tool_uses 导致 API 校验错误：阻断时为所有 tool_uses 生成错误结果。
- limit_history_turns 将所有 User 消息计为轮次：改为仅计含文本内容的。
- 流式 tool_use 的 input 字段为 null：默认空对象兜底。
