# 故障排除指南

本文档记录已知问题、排查方法和解决方案。

---

## 1. Edge TTS

### 1.1 返回 403 错误

**现象：** TTS 请求返回 HTTP 403，语音合成失败。

**原因：** Edge TTS 的 WebSocket 端点需要 DRM 认证，`chromiumVersion` 过时会被拒绝。

**解决：** 更新配置中的 `chromiumVersion` 字段。参考 [edge-tts Python 库](https://github.com/rany2/edge-tts) 获取最新版本号，无需修改代码。

### 1.2 OGG 格式语音无法播放

**现象：** Telegram/飞书收到语音但无法播放。

**原因：** Bing TTS 不支持直接输出 OGG Opus 格式。必须请求 WebM Opus 再转换为 OGG Opus。

**解决：** 系统已内置 WebM → OGG 自动转换（`webm_to_ogg.rs`），确认配置中未手动指定不支持的输出格式。

---

## 2. 火山引擎 STT（豆包语音识别）

### 2.1 长语音（>10 秒）识别超时

**现象：** 发送 14-20 秒的语音后，服务端返回超时错误或连接被关闭。

**原因：** 服务端有 3 秒处理超时（`timeout_config=3s`）。如果将整个音频作为单帧发送，服务端需要在 3 秒内处理所有数据，导致超时。

**解决：** 已在 `a4a972d` 中修复。音频按 3200 字节（约 100ms PCM）分块流式发送，服务端可以边收边处理。

### 2.2 长语音只返回部分文字

**现象：** 发送一段 15 秒语音，识别结果只有前 2 秒的内容。

**原因：** 旧版本在提取最终结果时遍历 utterances 数组，碰到第一个 `definite: true` 的就返回了，导致多句结果被截断。

**解决：** 已在 `a4a972d` 中修复。当收到最终响应（`is_last=true`）时，优先使用服务端拼接好的顶层 `result.text`，包含所有 utterance 的完整文本。

### 2.3 WebSocket 协议要点

- 音频帧的 `sequence` 从 2 开始递增（1 是 start message）
- 结束帧（`is_last=true`）的 sequence 取负值（如 seq=162 → 发 -162）
- 响应中的字段是 `definite`（不是 `definitive`）

### 2.4 DNS 解析失败（瞬时网络抖动）

**现象：** 语音消息回复 `stt error: WebSocket connect error: IO error: failed to lookup address information: Try again`。

**原因：** 容器内 DNS（Docker 内置 `127.0.0.11`）短暂不可用，无法解析 `openspeech.bytedance.com`。属于瞬时网络问题，通常几秒后自行恢复。

**解决：** STT 调用已加入 transient error 自动重试（1 次，间隔 2 秒）。DNS 抖动不再直接报错给用户。`is_transient_error` 覆盖 `lookup`、`connection`、`timed out` 等关键词。

---

## 3. 火山引擎 TTS（豆包语音合成）

### 3.1 配置字段名

**现象：** 配置了 `accessTokenEnv` 但提示 token 为空。

**原因：** 配置字段已统一为 `accessToken`（直接填值或用 `${ENV_VAR}` 引用环境变量），不再使用 `accessTokenEnv`。

**解决：** 参考 [语音配置](config-voice.md) 中火山引擎 TTS 部分。

---

## 4. 飞书 / Lark

### 4.1 语音消息时长显示为 0

**现象：** 飞书客户端收到语音但显示时长为 0 秒。

**原因：** 语音 duration 需要在文件上传 form 中指定（毫秒），不在发消息的 content JSON 里。

**解决：** 已在 `8e8aa47` 中修复。上传语音时自动检测并设置正确时长。

### 4.2 Markdown 中链接被截断

**现象：** 发送的 URL 包含 `_` 时，飞书渲染 Markdown 会把 `_` 当作斜体标记，导致链接断裂。

**原因：** 飞书的 Markdown 解析器对 URL 中的 `_` 不够智能。

**解决：** 已在 `d6624a4` 中修复。发送前自动将 URL 中的 `_` 转义为 `\_`。

### 4.3 重启后旧消息被重放

**现象：** 网关重启后，飞书机器人把之前已经回复过的消息又回复了一遍。

**原因：** 飞书 WebSocket 重连后可能重新投递离线期间积压的事件。网关的 event_id 去重集合仅在内存中，进程重启后清空，旧事件被当作新消息处理。

**解决：** 解析事件 header 中的 `create_time` 时间戳，跳过超过 2 分钟的旧事件（info 级别日志 `feishu: skipping stale event`）。

---

## 5. Anthropic OAuth

### 5.1 Token Exchange 失败

**现象：** OAuth 授权码获取成功，但换 token 时返回错误。

**原因：** Anthropic OAuth 的 token exchange 端点要求 JSON body，不接受 form-encoded。

**解决：** 已在 `f94227e` 中修复。请求使用 `.json()` 而非 `.form()`。

### 5.2 授权码包含多余后缀

**现象：** 回调 URL 中的 authorization code 后面带有 `#state_value`，导致 token exchange 失败。

**原因：** 浏览器 redirect 时 fragment 部分可能被附加到 code 参数后。

**解决：** 已在 `f94227e` 中修复。解析 code 时自动去除 `#` 及后续内容。

### 5.3 Refresh Token 失效（invalid_grant）

**现象：** 所有请求返回 `config error: token refresh failed: {"error": "invalid_grant", ...}`，用户无法使用 bot。

**原因：** Anthropic 侧的 refresh token 过期或被撤销，但本地磁盘上仍保存着旧 token。每次请求都尝试用无效 token 刷新，持续失败。

**解决：** `refresh()` 检测到 `invalid_grant` 时自动清除内存和磁盘上的无效 token，并提示用户 `/auth` 重新授权。避免旧 token 导致后续所有请求连锁失败。

---

## 6. 会话与持久化

### 6.1 会话截断后工具调用无响应

**现象：** Agent 在长对话中突然停止响应，日志显示 `tool_result` 缺少对应的 `tool_use`。

**原因：** 会话历史截断时可能将 `tool_use` 截掉但保留了 `tool_result`，导致 API 拒绝请求。

**解决：** 已在 `2423d1e` 中修复。加载会话时自动修复尾部孤立的 `tool_result`（`repair_trailing_tool_use`）。

### 6.2 历史压缩后 Agent 进入死循环

**现象：** Agent 反复调用同一个工具，日志显示 compaction 触发后上下文丢失。

**原因：** 压缩逻辑的边界条件处理不当，可能产生空消息。

**解决：** 已在 `c0a8c78` 中修复。

---

## 7. Docker 部署

### 7.1 部署后仍然是旧版本

**现象：** `docker compose up -d` 后行为没变。

**原因：** `docker build -t` 的镜像名和 `docker-compose.yml` 中 `image:` 字段不一致，compose 拉取的还是旧镜像。

**解决：** 确认两边镜像名一致，都是 `openclaw-light`：
```bash
docker build -t openclaw-light .
docker compose up -d
```

### 7.2 BuildKit 缓存导致代码不更新

**现象：** `docker build --no-cache` 仍然使用旧代码。

**原因：** BuildKit 的 layer 缓存独立于 `--no-cache` 标志。

**解决：**
```bash
docker builder prune -f
docker build -t openclaw-light .
```

---

## 8. ARM / systemd 裸机部署

### 8.1 ProtectSystem=strict 导致 EROFS（Read-only file system）

**现象：** 写 `auth_tokens.json` 或 `skills/` 目录时报 `io error: Read-only file system (os error 30)`。

**原因：** systemd `ProtectSystem=strict` 将整个文件系统设为只读，必须在 service 文件的 `ReadWritePaths` 中显式列出所有可写路径。

**排查陷阱：** SSH 直接登录设备后 `touch /opt/openclaw/auth_tokens.json` **不会复现**——沙箱只对 service 进程生效。必须通过 `journalctl -u openclaw-light` 查看实际运行日志。

**解决：** 编辑 service 文件补全 `ReadWritePaths`，然后重载：
```bash
sudo systemctl daemon-reload
sudo systemctl restart openclaw-light
```

完整的 `ReadWritePaths` 应包含：`sessions`、`memory`、`skills`、`auth_tokens.json`。参考 `deploy/openclaw-light.service`。

---

## 9. 文件操作工具

### 9.1 中文文件在 Telegram 手机端显示乱码

**现象：** 通过 `file_write` 写入含中文的 `.md` 文件，用 Telegram `sendDocument` 发送后，手机端打开是乱码（`?`菱形/方块），桌面端显示正常。

**原因：** 文件是合法 UTF-8 编码，但缺少 BOM（Byte Order Mark）。Telegram 手机端对 `.md` 文件不会自动检测 UTF-8，默认按 Latin-1 或 GBK 解读，导致多字节 UTF-8 序列被拆散显示为乱码。桌面端编辑器有更智能的编码探测，所以不受影响。

**解决：** 写含非 ASCII 文字（中文、日文、emoji 等）的文件时，在 `file_write` 的 `content` 开头加上 UTF-8 BOM 字符 `\uFEFF`：

```json
{"path": "/app/memory/user_files/report.md", "content": "\uFEFF# 报告标题\n内容..."}
```

BOM 在 UTF-8 中编码为 3 字节 `EF BB BF`，对正常阅读无影响，但能让所有平台正确识别编码。

**已有文件补 BOM：**
```bash
printf '\xEF\xBB\xBF' > /tmp/bom && cat original.md >> /tmp/bom && mv /tmp/bom original.md
```

---

## 10. 本地 LLM（Ollama / vLLM / llama.cpp）

### 10.1 启动报错 "requires providerConfig.apiKeyEnv"

**现象：** 配置了 `provider: "ollama"` 但启动时报 `Config error: provider "ollama" requires providerConfig.apiKeyEnv to be set`。

**原因：** 非内置 provider（ollama / vllm / llamacpp 等）没有默认的 `apiKeyEnv`，必须在 `providerConfig` 中显式指定。

**解决：** 在配置中添加 `providerConfig.apiKeyEnv`，并在 `.env` 中设置对应的环境变量：
```json5
{
  "provider": "ollama",
  "model": "qwen2.5:14b",
  "providerConfig": {
    "baseUrl": "http://localhost:11434/v1",
    "apiKeyEnv": "OLLAMA_API_KEY"
  }
}
```
```
OLLAMA_API_KEY=ollama   # Ollama 不校验，任意非空值即可
```

### 10.2 启动报错 "requires an explicit model in config"

**现象：** 配置了自定义 provider 但忘记设 `model`，报 `Config error: provider "llamacpp" requires an explicit model in config`。

**原因：** 非内置 provider 没有默认 model，必须显式指定。

**解决：** 在配置顶层加上 `model` 字段。对于 llama.cpp 等不区分 model 名的服务，填任意值即可（如 `"local"`）。

### 10.3 连接本地 LLM 超时或拒绝

**现象：** 启动正常但发消息后报 `Transport error` 或 `Connection refused`。

**排查：**
1. **确认 LLM 服务已启动**，端口可访问：`curl http://localhost:11434/v1/models`
2. **确认 baseUrl 包含 `/v1`** — 代码会自动追加 `/chat/completions`，最终请求地址是 `{baseUrl}/chat/completions`
3. **Docker 环境注意**：容器内 `localhost` 指的是容器自身，不是宿主机。如果 LLM 运行在宿主机上：
   - Linux：用 `http://host.docker.internal:11434/v1` 或 `http://172.17.0.1:11434/v1`
   - `docker-compose.yml` 加 `extra_hosts: ["host.docker.internal:host-gateway"]`

### 10.4 LLM 返回空响应或格式错误

**现象：** 请求成功（200）但 Agent 报 `empty choices` 错误。

**原因：** 本地 LLM 返回的 JSON 不完全兼容 OpenAI Chat Completions 格式（如 `choices` 数组为空，或缺少 `finish_reason` 字段）。

**解决：** 检查 LLM 服务的 OpenAI 兼容模式是否正确启用。Ollama 默认兼容，vLLM 需要 `--served-model-name` 和 `--api-key` 参数，llama.cpp 需要 `--chat-template` 参数。

---

### 9.2 Agent 使用过期凭据（Bot Token / API Key）

**现象：** Agent 通过 `exec` 调用外部 API（如 Telegram `sendDocument`）时，使用了旧的/错误的凭据，操作发送到了错误的目标（如旧 bot）。

**原因：** Agent 记忆文件（`memory/SHARED/MEMORY.md` 或 per-chat `MEMORY.md`）中存储了旧凭据。更换 Bot Token 或 API Key 后，如果只更新了 `openclaw.json` 配置而未同步更新记忆文件，Agent 会从记忆中读取旧值。

**解决：**
1. 更换凭据后，检查并更新所有记忆文件中引用的旧值：
   ```bash
   grep -r "旧token前缀" memory/
   ```
2. 更新 `memory/SHARED/MEMORY.md` 和相关 per-chat `MEMORY.md`
3. 如果容器内文件权限受限，在宿主机通过 volume 挂载目录直接编辑

**预防：** 在记忆中记录凭据时，注明来源（如"取自 openclaw.json"），便于后续追踪更新。敏感凭据尽量不写入记忆，让 Agent 从配置或环境变量获取。
