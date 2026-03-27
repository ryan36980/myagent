# OpenClaw Rust Gateway 设计文档

> 版本: 0.1.0 | 目标平台: 200MB 内存嵌入式 Linux (小盒子) | 运行时内存: ~3MB
>
> OpenClaw 的 Rust 轻量版本——通用个人 AI 助手，极致优化资源占用。
> 支持多 LLM 后端、多通道（Telegram/飞书/HTTP/CLI）、工具调用、长期记忆、定时任务。
> 不限定使用场景，可用于智能家居、开发辅助、信息检索、日程管理等任何用途。

---

## 文档导航

| 文档 | 章节 | 内容 |
|------|------|------|
| **[核心规范与 Trait 定义](core-traits.md)** | 第一~二章 | Rust 项目规范、依赖清单、错误处理、内存优化、五层 Trait 定义、统一消息类型、AgentRuntime |
| **[通道层设计](channels.md)** | 第三章 | Telegram / 飞书 / CLI / HTTP API 实现、多通道并发调度、ChatQueueManager |
| **[模块设计与 API 协议](modules.md)** | 第四~五章 | 各模块详细设计（config / error / LLM / STT / TTS / ReAct / Tools / Session）、API 协议规范 |
| **[运维：内存、配置、构建、测试](operations.md)** | 第六~九章 | 内存预算、配置系统、Docker 可复现构建、交叉编译、部署、测试策略 |
| **[子系统：记忆、Cron、扩展、OAuth](subsystems.md)** | 第十~十三章 | 长期记忆系统、Cron 自动执行、exec / web_search / MCP 客户端、Anthropic OAuth 2.0 |
| **[高级特性](advanced.md)** | 第十四~十七章 | 并发与流式预览、模型 Failover、循环检测、多模态与子 Agent、Followup Debounce、System Prompt 增强 |
