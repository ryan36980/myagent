# Troubleshooting Guide

This document records known issues, diagnostic methods, and solutions.

---

## 1. Edge TTS

### 1.1 Returns 403 Error

**Symptom:** TTS request returns HTTP 403; speech synthesis fails.

**Cause:** The Edge TTS WebSocket endpoint requires DRM authentication — an outdated `chromiumVersion` will be rejected.

**Solution:** Update the `chromiumVersion` field in the configuration. Check the [edge-tts Python library](https://github.com/rany2/edge-tts) for the latest version number — no code changes needed.

### 1.2 OGG Format Voice Cannot Play

**Symptom:** Telegram/Feishu receives the voice message but it cannot be played.

**Cause:** Bing TTS does not support direct OGG Opus output. WebM Opus must be requested and then converted to OGG Opus.

**Solution:** The system has a built-in automatic WebM → OGG conversion (`webm_to_ogg.rs`). Confirm that no unsupported output format has been manually specified in the configuration.

---

## 2. Volcengine STT (Doubao Speech Recognition)

### 2.1 Long Audio (>10 seconds) Recognition Timeout

**Symptom:** After sending 14–20 seconds of audio, the server returns a timeout error or closes the connection.

**Cause:** The server has a 3-second processing timeout (`timeout_config=3s`). Sending the entire audio as a single frame requires the server to process all data within 3 seconds, causing a timeout.

**Solution:** Fixed in `a4a972d`. Audio is sent in 3200-byte chunks (approximately 100ms of PCM) as a stream, allowing the server to process while receiving.

### 2.2 Long Audio Returns Only Partial Text

**Symptom:** Sending a 15-second audio clip, the recognition result only contains the first 2 seconds.

**Cause:** In the old version, when extracting the final result, the code iterated over the utterances array and returned as soon as it encountered the first one with `definite: true`, causing multi-sentence results to be truncated.

**Solution:** Fixed in `a4a972d`. When the final response is received (`is_last=true`), the server-assembled top-level `result.text` is used as the primary source, which contains the complete text of all utterances.

### 2.3 WebSocket Protocol Notes

- Audio frame `sequence` starts at 2 and increments (1 is the start message)
- The final frame (`is_last=true`) sequence is the negated value (e.g. seq=162 → send -162)
- The field in the response is `definite` (not `definitive`)

### 2.4 DNS Resolution Failure (Transient Network Glitch)

**Symptom:** Voice message replies with `stt error: WebSocket connect error: IO error: failed to lookup address information: Try again`.

**Cause:** DNS inside the container (Docker's built-in `127.0.0.11`) is briefly unavailable and cannot resolve `openspeech.bytedance.com`. This is a transient network issue that usually recovers on its own within a few seconds.

**Solution:** STT calls now include automatic transient error retry (1 retry, 2-second interval). DNS glitches no longer produce an error message for the user. `is_transient_error` covers keywords like `lookup`, `connection`, and `timed out`.

---

## 3. Volcengine TTS (Doubao Speech Synthesis)

### 3.1 Configuration Field Name

**Symptom:** `accessTokenEnv` is configured but the token is reported as empty.

**Cause:** The configuration field has been unified to `accessToken` (fill in a value directly or reference an environment variable with `${ENV_VAR}`); `accessTokenEnv` is no longer used.

**Solution:** Refer to the Volcengine TTS section in the [Voice Configuration](config-voice.md) guide.

---

## 4. Feishu / Lark

### 4.1 Voice Message Duration Displays as 0

**Symptom:** Feishu client receives the voice message but displays a duration of 0 seconds.

**Cause:** The voice `duration` must be specified (in milliseconds) in the file upload form — it is not part of the message content JSON.

**Solution:** Fixed in `8e8aa47`. Voice duration is now automatically detected and set correctly on upload.

### 4.2 Links in Markdown Are Broken

**Symptom:** When a sent URL contains `_`, Feishu's Markdown renderer treats `_` as an italic marker, breaking the link.

**Cause:** Feishu's Markdown parser does not handle `_` within URLs intelligently.

**Solution:** Fixed in `d6624a4`. `_` within URLs is automatically escaped to `\_` before sending.

### 4.3 Old Messages Replayed After Restart

**Symptom:** After restarting the gateway, the Feishu bot replies to messages it had already responded to.

**Cause:** After a Feishu WebSocket reconnection, events that accumulated during the offline period may be redelivered. The gateway's `event_id` deduplication set is in-memory only and is cleared on process restart, causing old events to be treated as new messages.

**Solution:** The `create_time` timestamp in the event header is parsed; events older than 2 minutes are skipped (info-level log `feishu: skipping stale event`).

---

## 5. Anthropic OAuth

### 5.1 Token Exchange Fails

**Symptom:** OAuth authorization code is obtained successfully, but an error is returned when exchanging for a token.

**Cause:** Anthropic's OAuth token exchange endpoint requires a JSON body — it does not accept form-encoded requests.

**Solution:** Fixed in `f94227e`. The request now uses `.json()` instead of `.form()`.

### 5.2 Authorization Code Contains Extra Suffix

**Symptom:** The authorization code in the callback URL has `#state_value` appended, causing the token exchange to fail.

**Cause:** The browser may append the fragment portion to the code parameter during redirect.

**Solution:** Fixed in `f94227e`. The `#` and everything after it is automatically stripped when parsing the code.

### 5.3 Refresh Token Expired (invalid_grant)

**Symptom:** All requests return `config error: token refresh failed: {"error": "invalid_grant", ...}` and users cannot use the bot.

**Cause:** The refresh token on the Anthropic side has expired or been revoked, but the old token is still stored on disk. Every request attempts to refresh with the invalid token and keeps failing.

**Solution:** When `refresh()` detects `invalid_grant`, it automatically clears the invalid token from both memory and disk, and prompts the user to re-authorize with `/auth`. This prevents a stale token from causing all subsequent requests to fail in a chain.

---

## 6. Sessions & Persistence

### 6.1 Tool Calls Unresponsive After Session Truncation

**Symptom:** The Agent suddenly stops responding mid-conversation; the log shows `tool_result` without a corresponding `tool_use`.

**Cause:** Session history truncation may remove a `tool_use` while retaining the `tool_result`, causing the API to reject the request.

**Solution:** Fixed in `2423d1e`. Orphaned trailing `tool_result` entries are automatically repaired when loading the session (`repair_trailing_tool_use`).

### 6.2 Agent Enters Infinite Loop After History Compaction

**Symptom:** The Agent repeatedly calls the same tool; the log shows context was lost after compaction was triggered.

**Cause:** Edge case handling in the compaction logic was incorrect and could produce empty messages.

**Solution:** Fixed in `c0a8c78`.

---

## 7. Docker Deployment

### 7.1 Still Running Old Version After Deployment

**Symptom:** No behavioral change after `docker compose up -d`.

**Cause:** The image name used in `docker build -t` does not match the `image:` field in `docker-compose.yml`; compose is pulling the old image.

**Solution:** Ensure both sides use the same image name — `openclaw-light`:
```bash
docker build -t openclaw-light .
docker compose up -d
```

### 7.2 BuildKit Cache Prevents Code Updates

**Symptom:** `docker build --no-cache` still uses old code.

**Cause:** BuildKit's layer cache is independent of the `--no-cache` flag.

**Solution:**
```bash
docker builder prune -f
docker build -t openclaw-light .
```

---

## 8. ARM / systemd Bare-Metal Deployment

### 8.1 ProtectSystem=strict Causes EROFS (Read-only file system)

**Symptom:** Writing `auth_tokens.json` or the `skills/` directory reports `io error: Read-only file system (os error 30)`.

**Cause:** systemd `ProtectSystem=strict` makes the entire filesystem read-only; all writable paths must be explicitly listed in `ReadWritePaths` in the service file.

**Debugging trap:** Logging into the device via SSH and running `touch /opt/openclaw/auth_tokens.json` **will not reproduce** the issue — the sandbox only applies to the service process. You must check the actual runtime logs via `journalctl -u openclaw-light`.

**Solution:** Edit the service file to add the complete `ReadWritePaths`, then reload:
```bash
sudo systemctl daemon-reload
sudo systemctl restart openclaw-light
```

A complete `ReadWritePaths` should include: `sessions`, `memory`, `skills`, `auth_tokens.json`. Refer to `deploy/openclaw-light.service`.

---

## 9. File Operation Tools

### 9.1 Chinese Files Display Garbled on Telegram Mobile {#91-chinese-files-display-garbled-on-telegram-mobile}

**Symptom:** A `.md` file containing Chinese is written via `file_write` and sent using Telegram `sendDocument`. The file appears garbled (`?` diamonds/boxes) on mobile, but displays correctly on desktop.

**Cause:** The file is valid UTF-8, but lacks a BOM (Byte Order Mark). The Telegram mobile app does not auto-detect UTF-8 for `.md` files and defaults to interpreting them as Latin-1 or GBK, causing multi-byte UTF-8 sequences to be split and display as garbled text. Desktop editors have smarter encoding detection and are unaffected.

**Solution:** When writing files containing non-ASCII characters (Chinese, Japanese, emoji, etc.), prepend the UTF-8 BOM character `\uFEFF` to the `content` in `file_write`:

```json
{"path": "/app/memory/user_files/report.md", "content": "\uFEFF# Report Title\nContent..."}
```

The BOM is encoded as 3 bytes `EF BB BF` in UTF-8, has no visible effect on reading, but allows all platforms to correctly identify the encoding.

**Adding BOM to an existing file:**
```bash
printf '\xEF\xBB\xBF' > /tmp/bom && cat original.md >> /tmp/bom && mv /tmp/bom original.md
```

---

## 10. Local LLM (Ollama / vLLM / llama.cpp)

### 10.1 Startup Error "requires providerConfig.apiKeyEnv"

**Symptom:** Configured `provider: "ollama"` but startup reports `Config error: provider "ollama" requires providerConfig.apiKeyEnv to be set`.

**Cause:** Non-built-in providers (ollama / vllm / llamacpp etc.) have no default `apiKeyEnv` — it must be explicitly specified in `providerConfig`.

**Solution:** Add `providerConfig.apiKeyEnv` to the configuration and set the corresponding environment variable in `.env`:
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
OLLAMA_API_KEY=ollama   # Ollama doesn't validate, any non-empty value works
```

### 10.2 Startup Error "requires an explicit model in config"

**Symptom:** Configured a custom provider but forgot to set `model`, resulting in `Config error: provider "llamacpp" requires an explicit model in config`.

**Cause:** Non-built-in providers have no default model — one must be explicitly specified.

**Solution:** Add a `model` field at the top level of the configuration. For services like llama.cpp that don't differentiate by model name, any value will work (e.g. `"local"`).

### 10.3 Connection to Local LLM Times Out or Is Refused

**Symptom:** Startup succeeds but sending a message reports `Transport error` or `Connection refused`.

**Troubleshooting:**
1. **Confirm the LLM service is running** and the port is accessible: `curl http://localhost:11434/v1/models`
2. **Confirm baseUrl includes `/v1`** — the code automatically appends `/chat/completions`, so the final request URL is `{baseUrl}/chat/completions`
3. **Docker environment note**: `localhost` inside a container refers to the container itself, not the host machine. If the LLM is running on the host:
   - Linux: use `http://host.docker.internal:11434/v1` or `http://172.17.0.1:11434/v1`
   - Add `extra_hosts: ["host.docker.internal:host-gateway"]` to `docker-compose.yml`

### 10.4 LLM Returns Empty Response or Format Error

**Symptom:** Request succeeds (200) but the Agent reports an `empty choices` error.

**Cause:** The local LLM's JSON response is not fully compatible with the OpenAI Chat Completions format (e.g. `choices` array is empty, or `finish_reason` field is missing).

**Solution:** Check that the LLM service's OpenAI-compatible mode is correctly enabled. Ollama is compatible by default; vLLM requires `--served-model-name` and `--api-key` parameters; llama.cpp requires the `--chat-template` parameter.

---

### 9.2 Agent Uses Stale Credentials (Bot Token / API Key)

**Symptom:** When the Agent calls an external API via `exec` (e.g. Telegram `sendDocument`), it uses old/incorrect credentials and the operation targets the wrong destination (e.g. an old bot).

**Cause:** The Agent's memory files (`memory/SHARED/MEMORY.md` or per-chat `MEMORY.md`) stored the old credentials. After replacing the Bot Token or API Key, if only `openclaw.json` was updated without also updating the memory files, the Agent will read the old values from memory.

**Solution:**
1. After replacing credentials, search for and update all old values referenced in memory files:
   ```bash
   grep -r "old-token-prefix" memory/
   ```
2. Update `memory/SHARED/MEMORY.md` and any relevant per-chat `MEMORY.md`
3. If file permissions inside the container are restricted, edit directly via the volume-mounted directory on the host machine

**Prevention:** When recording credentials in memory, note their source (e.g. "from openclaw.json") for easier future tracking and updates. Avoid writing sensitive credentials to memory where possible — let the Agent obtain them from configuration or environment variables instead.
