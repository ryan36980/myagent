> [配置指南](configuration.md) > 通道配置

# 通道配置（Telegram / 飞书 / HTTP API / CLI）

## Telegram（默认通道）

```json5
{
  "channels": {
    "telegram": {
      "botToken": "${TELEGRAM_BOT_TOKEN}",
      "allowedUsers": [123456789]  // Telegram 用户 ID，空数组 = 不限制
    }
  }
}
```

**.env 文件：**
```
TELEGRAM_BOT_TOKEN=123456:ABCdefGHI...
```

**设置步骤：**
1. 在 Telegram 中找到 [@BotFather](https://t.me/BotFather)
2. 发送 `/newbot`，按提示创建机器人
3. 复制 Bot Token 到 `.env` 文件
4. （推荐）设置 `allowedUsers` 限制谁能使用机器人

**获取你的 Telegram 用户 ID：** 向 [@userinfobot](https://t.me/userinfobot) 发送任意消息。

## HTTP API + Web Chat

```json5
{
  "channels": {
    "httpApi": {
      "enabled": true,
      "listen": "0.0.0.0:8080",                // 监听地址
      "authToken": "${HTTP_AUTH_TOKEN}"          // 可选 Bearer Token 认证
    }
  }
}
```

**.env 文件（如需认证）：**
```
HTTP_AUTH_TOKEN=your-secret-token
```

启用后：
- 浏览器访问 `http://<device>:8080/` 即可打开内嵌 Web Chat 页面
- `POST /chat` — 传统请求-响应模式
- `POST /chat/stream` — SSE 流式输出（推荐）
- `GET /chat/history?chat_id=xxx` — 获取会话历史

**认证：** `authToken` 非空时，所有 `POST` 和 `GET /chat/*` 端点需要 `Authorization: Bearer xxx` 头。Web Chat 页面（`GET /`）无需认证，页面会提示输入 Token。

**Web Chat 特性：**
- 单文件嵌入（~10KB），零外部依赖
- SSE 实时流式输出，逐字显示 AI 回复
- 自动跟随系统 dark/light 主题
- 简单 Markdown 渲染（加粗、代码、代码块）
- 刷新后自动恢复聊天历史
- 响应式布局，移动端适配

默认关闭。

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

启用后，直接在终端中与 Agent 对话。适合本地调试。默认关闭。

## 飞书/Lark（WebSocket 长连接）

```json5
{
  "channels": {
    "feishu": {
      "appId": "${FEISHU_APP_ID}",
      "appSecret": "${FEISHU_APP_SECRET}",
      "domain": "https://open.feishu.cn",  // 国际版: "https://open.larksuite.com"
      "allowedUsers": []  // open_id 白名单，空 = 不限制
    }
  }
}
```

**.env 文件：**
```
FEISHU_APP_ID=cli_axxxxxxxxxxxxxxxxx
FEISHU_APP_SECRET=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

**设置步骤：**
1. 登录[飞书开放平台](https://open.feishu.cn/)，创建自建应用
2. 获取 App ID 和 App Secret
3. **添加能力** → 机器人
4. **权限管理** → 开通以下权限：
   - `im:message`（收发消息）
   - `im:message:send_as_bot`（机器人发送消息）
   - `im:message.p2p_msg:readonly`（读取私聊消息）
   - `im:message:update`（更新消息，流式输出必需）
   - `im:resource`（文件下载，语音消息必需）
5. **事件订阅** → 选择 **长连接模式** → 添加 `im.message.receive_v1` 事件
   > 注意：长连接模式需要先启动服务建立 WebSocket 连接后，控制台才能保存。可以先完成其他配置，启动服务后回来保存。
6. **版本管理** → 创建版本 → 发布上线
7. 将 App ID 和 App Secret 写入 `.env` 文件

**获取用户 open_id（用于 allowedUsers）：** 机器人收到消息后，日志中会打印 sender open_id。

> 飞书通道使用 WebSocket 长连接，无需公网 URL。网关主动连接飞书服务器，与 Telegram 长轮询模式一致。

**技术细节：**
- 文本消息使用飞书**交互式卡片**（Interactive Card）发送，支持 Markdown 渲染，并支持流式编辑（streaming preview）。
- 事件通过 `event_id` 自动去重，防止 WebSocket 重复投递导致重复回复。
- 语音发送自动检测音频格式（MP3/Opus），正确标记 MIME 类型上传。上传时自动计算并传递 `duration`（毫秒），确保飞书客户端正确显示语音时长。

> 注意：所有通道可以同时启用，共享同一个 Agent 配置和工具集。
