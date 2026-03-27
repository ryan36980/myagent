[中文](README.md) | [English](README.en.md)

# OpenClaw Light

OpenClaw 的 Rust 轻量版本 —— 通用个人 AI 助手，专为资源受限的嵌入式设备（小盒子）设计。

常驻内存 ~1.3MB，常规负载 <4MB，极端峰值 <8MB（Node.js 方案的 ~2%），产出单个静态链接二进制，无外部依赖。

## 功能

- **多通道**：Telegram 语音/文字（长轮询）、飞书/Lark（WebSocket 长连接）、CLI、HTTP API（SSE streaming）
- **多 LLM 后端**：Anthropic Claude / DeepSeek / Groq / OpenAI 兼容，支持 failover 自动切换
- **ReAct Agent**：自动工具调用，时间超时（默认 900s），三级循环检测 + 全局断路器
- **异步子 Agent**：`sessions_spawn` 异步启动，不阻塞主对话，自动回报结果
- **SSE Streaming**：实时流式输出到 Telegram / HTTP API / CLI
- **内嵌 Web Chat UI**：`GET /` 返回嵌入式聊天页面，SSE 流式、dark/light 主题、Markdown 渲染
- **HTTP API 认证**：可选 Bearer Token 保护 API 端点 + CORS 跨域支持
- **中断模式**：新消息自动中断当前 agent 执行，立即处理
- **可选 Home Assistant 集成**
- **多 TTS 提供商**：Edge TTS（免费默认）/ OpenAI TTS / ElevenLabs / 火山引擎
- **TTS 四模式**：inbound（语音触发）/ always / tagged（`<speak>` 标签）/ off
- **多 STT 提供商**：Groq Whisper / 火山引擎（豆包）/ Google Cloud STT
- **长期记忆系统**：per-chat MEMORY.md + 每日日志 + 子串搜索 + 自适应注入
- **定时任务**：cron 表达式 + 一次性调度 + 多投递模式（announce/webhook/silent）
- **会话管理**：JSONL 持久化 + 轮次限制（dmHistoryLimit）+ 自动压缩
- **Context Files**：加载自定义知识文件到 system prompt（SOUL.md 等价）
- **图片/视觉**：Telegram 照片/文档 → 多模态 LLM
- **并行工具执行**：`join_all` 协作式并发
- **文件操作工具**：file_read / file_write / file_edit / file_find
- **自动备份**：cron 定期打包 + agent 工具按需触发，滚动清理（按年龄 + 总大小）
- **瞬态错误重试**：网络/过载错误自动 2.5s 退避重试
- **Context Pruning**：自动裁剪老旧工具输出，节省上下文窗口
- **Anthropic OAuth 2.0**：PKCE 授权码流程，支持 Claude Max/Pro 订阅

## 支持平台

| 目标 | 适用设备 |
|------|---------|
| `x86_64-unknown-linux-musl` | 通用 Linux 服务器 |
| `aarch64-unknown-linux-musl` | 树莓派 4/5、RK3588（64 位 ARM） |
| `armv7-unknown-linux-musleabihf` | 树莓派 2/3、NanoPi（32 位 ARM） |
| `x86_64-pc-windows-gnu` | Windows x86_64 |

所有二进制均已在 [Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) 页提供预编译下载，无需自行构建。

## 前置条件

- **Docker**（仅从源码构建时需要，宿主机不需要 Rust 工具链；使用预编译二进制可跳过）
- **API Keys**（按需配置）：
  - Telegram Bot Token（[@BotFather](https://t.me/BotFather) 创建）
  - 飞书 App ID + App Secret（[飞书开放平台](https://open.feishu.cn/) 创建自建应用）
  - Anthropic API Key（Claude）或 OAuth 2.0 认证
  - STT：Groq API Key / 火山引擎 Access Token / Google STT API Key（可选）
  - Home Assistant Long-Lived Access Token（可选）

## 快速开始

### 1. 获取二进制

#### 方式一：下载预编译二进制（推荐）

从 [Releases](https://gitcode.com/bell-innovation/OpenClaw_light/releases) 页下载对应平台的二进制文件：

| 文件 | 平台 |
|------|------|
| `openclaw-light-<version>-x86_64-unknown-linux-musl` | Linux x86_64 |
| `openclaw-light-<version>-aarch64-unknown-linux-musl` | Linux ARM64（树莓派 4/5） |
| `openclaw-light-<version>-armv7-unknown-linux-musleabihf` | Linux ARMv7（树莓派 2/3） |
| `openclaw-light-<version>-x86_64-pc-windows-gnu.exe` | Windows x86_64 |

Linux 下载后赋予执行权限即可运行：

```bash
chmod +x openclaw-light-*
```

#### 方式二：从源码构建

需要 Docker，宿主机不需要 Rust 工具链。

```bash
# 构建 ARM64（树莓派 4/5）
./scripts/docker-build.sh aarch64

# 构建全部 4 个平台（含 Windows）
./scripts/docker-build.sh

# 验证构建可复现性
./scripts/docker-build.sh --verify x86_64
```

产物位于 `dist/<target-triple>/openclaw-light`。

> 首次构建需下载 ~1GB 的 rust-musl-cross 镜像和依赖编译，后续构建利用 Docker 层缓存会快很多。

### 2. 配置

```bash
cp config/openclaw.json.example config/openclaw.json
```

编辑 `config/openclaw.json`，填入你的 API keys。敏感信息通过环境变量注入：

```json5
{
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": ["你的Telegram用户ID"]  // 空数组=不限制
    },
    "httpApi": {
      "enabled": true,
      "listen": "0.0.0.0:8080",
      "authToken": "${HTTP_AUTH_TOKEN}"  // 可选，空/未设置=无认证
    }
  },
  "homeAssistant": {
    "url": "http://192.168.1.100:8123",
    "token": "${HA_TOKEN}"
  }
}
```

配置文件使用 JSON5 格式，支持注释和尾随逗号。完整示例见 `config/openclaw.json.example`。

### 3. 部署到目标设备

#### 方式一：脚本部署（推荐）

```bash
# 设置目标主机（默认 pi@raspberrypi.local）
export DEPLOY_HOST=pi@192.168.1.50
export DEPLOY_TARGET=aarch64-unknown-linux-musl

# 部署二进制 + 配置模板
./scripts/deploy.sh
```

部署后在目标设备上：

```bash
# 创建配置文件
cd /opt/openclaw
cp openclaw.json.example openclaw.json
# 编辑填入你的 API keys
nano openclaw.json

# 创建环境变量文件
cat > .env << 'EOF'
TELEGRAM_BOT_TOKEN=123456:ABCxxx
ANTHROPIC_API_KEY=sk-ant-xxx
GROQ_API_KEY=gsk_xxx              # 可选，仅语音识别需要
HA_TOKEN=eyJhbGci...
EOF
chmod 600 .env
```

#### 方式二：Docker Compose（推荐容器化部署）

```bash
# 1. 构建镜像
docker build -t openclaw-light:latest .

# 2. 准备配置文件和目录
cp config/openclaw.json.example openclaw.json
# 编辑 openclaw.json 填入 API keys
mkdir -p sessions memory skills

# 3. 创建 .env 文件（可选，按需配置）
cat > .env << 'EOF'
GROQ_API_KEY=gsk_xxx                    # Groq STT（可选）
VOLCENGINE_ACCESS_TOKEN=xxx             # 火山引擎 STT/TTS（可选）
HTTP_AUTH_TOKEN=your-secret-token       # HTTP API 认证（可选）
EOF

# 4. 启动
docker compose up -d

# 查看日志
docker compose logs -f
```

容器安全特性：只读文件系统、丢弃全部 capabilities、禁止提权、10MB 内存限制、非 root 用户运行。
exec 工具自动隔离环境变量，防止 LLM 通过 `env` 命令读取 API key 等密钥。

#### 方式三：手动部署

```bash
# 1. 复制二进制到目标设备
scp dist/aarch64-unknown-linux-musl/openclaw-light pi@192.168.1.50:/opt/openclaw/

# 2. 复制配置
scp config/openclaw.json pi@192.168.1.50:/opt/openclaw/

# 3. SSH 到设备运行
ssh pi@192.168.1.50
cd /opt/openclaw
export TELEGRAM_BOT_TOKEN=123456:ABCxxx
export ANTHROPIC_API_KEY=sk-ant-xxx
export GROQ_API_KEY=gsk_xxx
./openclaw-light --config openclaw.json
```

### 4. 设置开机自启（systemd）

service 文件已包含在仓库 `deploy/openclaw-light.service` 中，`deploy.sh` 会自动安装。
如需手动安装：

```bash
sudo cp /opt/openclaw/openclaw-light.service /etc/systemd/system/
```

```bash
# 创建专用用户
sudo useradd -r -s /usr/sbin/nologin openclaw
sudo chown -R openclaw:openclaw /opt/openclaw

# 启动服务
sudo systemctl daemon-reload
sudo systemctl enable --now openclaw-light

# 查看日志
sudo journalctl -u openclaw-light -f
```

## 项目结构

```
├── Cargo.toml                 # 依赖和 release profile
├── rust-toolchain.toml        # Rust 1.84.0 + musl targets
├── Dockerfile                 # 多阶段可复现构建（busybox shell + 非 root）
├── docker-compose.yml         # 生产部署（安全加固）
├── docker-compose.build.yml   # compose 构建（可选）
├── config/
│   └── openclaw.json.example  # 配置模板
├── deploy/
│   └── openclaw-light.service  # systemd 服务单元
├── scripts/
│   ├── docker-build.sh        # Docker 构建/验证脚本
│   └── deploy.sh              # 远程部署脚本
├── src/
│   ├── main.rs                # 入口 + 多通道调度 + 优雅关闭 + interrupt mode
│   ├── config.rs              # JSON5 配置加载 + 环境变量替换
│   ├── error.rs               # 统一错误类型
│   ├── backup.rs              # 自动备份引擎（打包 + 滚动清理）
│   ├── channel/
│   │   ├── telegram.rs        # Telegram Bot API（长轮询 + streaming）
│   │   ├── feishu.rs          # 飞书/Lark（WebSocket 长连接）
│   │   ├── cli.rs             # CLI 交互通道
│   │   ├── http_api.rs        # HTTP API 通道（SSE streaming + Bearer Token + CORS）
│   │   ├── streaming.rs       # StreamingWriter（节流式消息编辑）
│   │   ├── types.rs           # 消息类型定义
│   │   └── web_chat.html      # 内嵌 Web Chat UI（dark/light 主题 + Markdown 渲染）
│   ├── provider/
│   │   ├── llm/
│   │   │   ├── claude.rs      # Anthropic Messages API（SSE + Extended Thinking）
│   │   │   ├── openai_compat.rs # OpenAI 兼容（DeepSeek / Groq 等）
│   │   │   └── failover.rs    # LLM failover 链（自动切换 + 指数退避冷却）
│   │   ├── stt/
│   │   │   ├── groq.rs        # Groq Whisper 语音识别
│   │   │   ├── volcengine.rs  # 火山引擎（豆包）语音识别
│   │   │   └── google.rs      # Google Cloud STT
│   │   └── tts/
│   │       ├── edge.rs        # Edge TTS（免费默认）
│   │       ├── openai.rs      # OpenAI TTS
│   │       ├── elevenlabs.rs  # ElevenLabs TTS
│   │       ├── volcengine.rs  # 火山引擎 TTS
│   │       └── webm_to_ogg.rs # WebM Opus → OGG Opus 转换
│   ├── agent/
│   │   ├── react_loop.rs      # ReAct 循环 + 循环检测 + context pruning + 瞬态重试
│   │   └── context.rs         # System prompt 组装（记忆 + 工具 + runtime info + context files）
│   ├── auth/mod.rs            # Anthropic OAuth 2.0（PKCE）
│   ├── memory/store.rs        # 长期记忆（MEMORY.md + 日志 + 搜索）
│   ├── tools/
│   │   ├── agent_tool.rs      # 异步子 Agent（spawn/list/history/send）
│   │   ├── ha_control.rs      # Home Assistant 控制
│   │   ├── html_utils.rs      # HTML→纯文本转换（零依赖）
│   │   ├── web_fetch.rs       # 网页抓取（HTML→Text + 128KB + 分页 + SSRF 防护）
│   │   ├── web_search.rs      # 网页搜索（DuckDuckGo + Brave）
│   │   ├── cron.rs            # 定时任务
│   │   ├── memory.rs          # 记忆管理（6 actions）
│   │   ├── exec.rs            # Shell 命令执行（环境隔离）
│   │   ├── file.rs            # 文件操作（read / write / edit / find）
│   │   ├── backup.rs          # 备份工具（agent 按需触发）
│   │   ├── get_time.rs        # 时间查询
│   │   └── mcp.rs             # MCP 客户端（stdio JSON-RPC）
│   └── session/store.rs       # JSONL 会话持久化 + 轮次限制
└── docs/
    ├── zh/                     # 中文文档
    │   ├── guides/             # 用户指南（配置指南等）
    │   ├── design/             # 架构设计文档
    │   ├── specs/              # 编码规范
    │   ├── reports/            # 监控报告
    │   └── requirements/       # 需求与背景
    └── en/                     # English docs (mirror of zh/)
```

## 日志

通过 `RUST_LOG` 环境变量控制日志级别：

```bash
RUST_LOG=info ./openclaw-light     # 生产环境（默认）
RUST_LOG=debug ./openclaw-light    # 调试
RUST_LOG=openclaw_light=debug ./openclaw-light  # 只调试本项目
```

## 内存占用

实测数据（Docker 容器，10 MiB 限制）：

| 场景 | 内存 | 占比 |
|------|------|------|
| 空闲 | ~1.3 MiB | 13% |
| 单工具执行 | 2.0 ~ 2.6 MiB | 20~26% |
| ReAct 循环活跃 | 2.7 ~ 3.3 MiB | 27~33% |
| 子 Agent 运行 | 3.0 ~ 3.6 MiB | 30~36% |
| 并行网页搜索+抓取 | ~8 MiB（峰值） | ~80% |

详细监控报告：[docs/reports/](docs/zh/reports/)

## 文档

- **[配置指南](docs/zh/guides/configuration.md)** — 全量配置说明：LLM 提供商、认证、Failover、通道、TTS/STT、工具、完整示例
- [架构设计](docs/zh/design/README.md) — Trait 定义、模块设计、API 协议、内存预算、记忆系统、子 Agent、Failover
- [编码规范](docs/zh/specs/rust-conventions.md) — 命名、错误处理、内存规则、可复现构建、开发流程
- [监控报告](docs/zh/reports/) — 内存监控、性能分析
- [更新日志](CHANGELOG.md) — 版本变更记录
- [原始设计](docs/zh/requirements/design.md) — Node.js 初始方案（归档，仅供参考）
- [技术选型](docs/zh/requirements/extreme-optimization-plan.md) — 三方案对比（已选定 Plan C: Rust）

## License

本项目采用 [Apache License 2.0](LICENSE-APACHE) 或 [MIT License](LICENSE-MIT) 双许可，任选其一。
