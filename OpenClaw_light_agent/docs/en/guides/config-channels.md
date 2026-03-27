> [Configuration Guide](configuration.md) > Channel Configuration

# Channel Configuration (Telegram / Feishu / HTTP API / CLI)

## Telegram (Default Channel)

```json5
{
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": [123456789]  // Telegram user IDs, empty array = no restriction
    }
  }
}
```

**.env file:**
```
TELEGRAM_BOT_TOKEN=123456:ABCdefGHI...
```

**Setup steps:**
1. Find [@BotFather](https://t.me/BotFather) in Telegram
2. Send `/newbot` and follow the prompts to create a bot
3. Copy the Bot Token to the `.env` file
4. (Recommended) Set `allowedUsers` to restrict who can use the bot

**Get your Telegram user ID:** Send any message to [@userinfobot](https://t.me/userinfobot).

## HTTP API + Web Chat

```json5
{
  "channels": {
    "httpApi": {
      "enabled": true,
      "listen": "0.0.0.0:8080",                // listen address
      "authToken": "${HTTP_AUTH_TOKEN}"          // optional Bearer Token auth
    }
  }
}
```

**.env file (if auth is needed):**
```
HTTP_AUTH_TOKEN=your-secret-token
```

When enabled:
- Open `http://<device>:8080/` in a browser for the embedded Web Chat UI
- `POST /chat` — traditional request-response mode
- `POST /chat/stream` — SSE streaming output (recommended)
- `GET /chat/history?chat_id=xxx` — get session history

**Authentication:** When `authToken` is non-empty, all `POST` and `GET /chat/*` endpoints require the `Authorization: Bearer xxx` header. The Web Chat page (`GET /`) does not require authentication; the page prompts for a token.

**Web Chat features:**
- Single-file embedded (~10KB), zero external dependencies
- SSE real-time streaming, displays AI replies character by character
- Follows system dark/light theme automatically
- Simple Markdown rendering (bold, code, code blocks)
- Automatically restores chat history on page refresh
- Responsive layout, mobile-friendly

Disabled by default.

## CLI

```json5
{
  "channels": {
    "cli": {
      "enabled": true
    }
  }
}
```

When enabled, interact with the Agent directly in the terminal. Suitable for local debugging. Disabled by default.

## Feishu/Lark (WebSocket Long Connection)

```json5
{
  "channels": {
    "feishu": {
      "appId": "${FEISHU_APP_ID}",
      "appSecret": "${FEISHU_APP_SECRET}",
      "domain": "https://open.feishu.cn",  // International version: "https://open.larksuite.com"
      "allowedUsers": []  // open_id whitelist, empty = no restriction
    }
  }
}
```

**.env file:**
```
FEISHU_APP_ID=cli_axxxxxxxxxxxxxxxxx
FEISHU_APP_SECRET=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

**Setup steps:**
1. Log in to the [Feishu Open Platform](https://open.feishu.cn/) and create a custom app
2. Obtain the App ID and App Secret
3. **Add Capability** → Bot
4. **Permission Management** → Enable the following permissions:
   - `im:message` (send and receive messages)
   - `im:message:send_as_bot` (bot sends messages)
   - `im:message.p2p_msg:readonly` (read direct messages)
   - `im:message:update` (update messages, required for streaming output)
   - `im:resource` (file download, required for voice messages)
5. **Event Subscriptions** → Select **Long Connection Mode** → Add the `im.message.receive_v1` event
   > Note: Long connection mode requires starting the service to establish a WebSocket connection before the console can save. You can complete other configurations first, then start the service and come back to save.
6. **Version Management** → Create version → Publish
7. Write the App ID and App Secret to the `.env` file

**Get user open_id (for allowedUsers):** After the bot receives a message, the sender's open_id is printed in the logs.

> The Feishu channel uses a WebSocket long connection — no public URL is needed. The gateway actively connects to Feishu servers, consistent with Telegram's long-polling mode.

**Technical details:**
- Text messages are sent using Feishu **Interactive Cards**, which support Markdown rendering and streaming editing (streaming preview).
- Events are automatically deduplicated by `event_id` to prevent duplicate replies from WebSocket redelivery.
- Voice sending automatically detects the audio format (MP3/Opus) and correctly marks the MIME type on upload. The `duration` (in milliseconds) is automatically calculated and passed on upload, ensuring the Feishu client displays the correct voice duration.

> Note: All channels can be enabled simultaneously, sharing the same Agent configuration and tool set.
