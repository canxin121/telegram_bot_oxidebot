# telegram_bot_oxidebot

`telegram_bot_oxidebot` 是 [oxidebot](https://github.com/canxin121/oxidebot) 的 Telegram Bot API 适配器。

当前版本按 **Telegram Bot API 10.2（2026-07-14）** 核对。适配器只实现 oxidebot 通用接口所需的 Telegram API 子集，但接收更新时采用向前兼容设计：Telegram 新增字段会被保留，尚未映射为 oxidebot 标准事件的更新会作为原始事件交给上层，而不会导致整个 `getUpdates` 响应反序列化失败。

## 主要能力

- 纯 Rustls HTTPS，不依赖 OpenSSL。
- 长轮询支持 offset 提交、失败指数退避、Telegram `retry_after` 限流等待和自定义 `allowed_updates`。
- 支持文本、提及、回复、图片、视频、音频、文件、相册、贴纸、位置以及本地文件/base64 上传。
- 自动遵守文本 4096、caption 1024、相册 2–10 项等 Telegram 约束；单个媒体不会再错误调用 `sendMediaGroup`。
- 支持删改消息、消息反应、成员禁言/解禁、全群权限、踢出/封禁、管理员升降级、管理员头衔、群资料、用户资料、入群申请审批和文件信息。
- 将普通消息、频道消息、编辑、成员变化、反应和入群申请映射为 oxidebot 标准事件。
- Bot API 10.x 的 Rich Message、Guest Message、Managed Bot、Subscription、Ephemeral Message 等尚无 oxidebot 通用抽象的内容，会通过 `TelegramRawEvent` 或 `MessageSegment::CustomValue` 完整保留。
- 通过 oxidebot 的 `PlatformApiRequest` 暴露全部 185 个 Bot API 10.2 方法；其他适配器不支持时会返回明确错误。
- 通过 `TelegramBot::client()` 继续暴露更低层的 `TelegramClient::call`，新发布且尚未进入方法目录的 Telegram API 也能立即调用。

## 安装

```bash
cargo add telegram_bot_oxidebot
```

## 使用

```rust,no_run
use anyhow::Result;
use oxidebot::OxideBotManager;
use telegram_bot_oxidebot::{GetUpdatesConfig, TelegramBot};

async fn run_bot() -> Result<()> {
    let config = GetUpdatesConfig {
        // chat_member 和 message_reaction 默认不会由 Telegram 推送，
        // 需要使用 allowed_updates 显式订阅；None 会沿用 Telegram 端设置。
        allowed_updates: None,
        ..Default::default()
    };
    let bot = TelegramBot::try_new(std::env::var("TELEGRAM_BOT_TOKEN")?, config).await?;

    OxideBotManager::new().bot(bot).await.run_block().await
}
```

`TelegramBot::new` 为兼容 0.1 版本仍然保留，但连接失败时会 panic。新代码应使用返回 `Result` 的 `TelegramBot::try_new`。本地 Bot API 服务器或测试服务可使用 `TelegramBot::try_with_api_base`。

## 原始 Telegram 事件

无法直接表示为 oxidebot 标准事件的更新会成为 `Event::AnyEvent`：

- `AnyEvent.r#type` 是 Telegram 原始 update 字段名，例如 `callback_query`、`guest_message` 或 `subscription`；
- `AnyEvent.data` 可向下转换为 `telegram_bot_oxidebot::event::TelegramRawEvent`；
- `TelegramRawEvent.data` 保留 Telegram 返回的完整 JSON。

对于已经映射成 oxidebot 标准事件的 update，`Matcher.event_object` 仍保存完整的 `UpdateEvent`，可通过 `EventTrait::as_any` 向下转换后读取原始 `Update`；因此标准事件映射不会阻断业务层访问 Telegram 的完整数据。这种设计也意味着 Telegram 后续增加 update 类型时，机器人不会因为依赖库缺少枚举变体而卡死在同一个 offset。

## 调用全部 Telegram 原生 API

oxidebot 的平台 API 扩展层可以直接通过 `BotObject` 使用，因此不需要向下转换到 `TelegramBot`。JSON 参数和返回值不会丢失 Telegram 专属字段：

```rust,no_run
use anyhow::Result;
use oxidebot::{BotTrait, PlatformApiRequest};
use serde_json::json;

async fn get_star_balance(bot: &dyn BotTrait) -> Result<serde_json::Value> {
    let response = bot
        .call_platform_api(PlatformApiRequest::new("getMyStarBalance").parameters(json!({})))
        .await?;
    Ok(response.result)
}
```

需要上传文件时，使用 `PlatformApiFile::bytes` 或异步的 `PlatformApiFile::from_path`，再通过 `PlatformApiRequest::file` 添加 multipart 字段。嵌套 Telegram 对象可以按官方语法用 `attach://字段名` 引用附件。

`TELEGRAM_BOT_API_METHODS` 完整列出 Bot API 10.2 的 185 个方法，`TELEGRAM_BOT_API_UPDATE_TYPES` 完整列出 26 个 update 类型。原生调用不会被目录硬性限制，因此 Telegram 发布第 186 个方法后也仍然可以直接调用。

Telegram 返回 API 错误时，可以从 `anyhow::Error` 向下转换为 `TelegramApiError`。错误会保留 HTTP 状态、Telegram `error_code`、描述，以及 `migrate_to_chat_id`、`retry_after` 两个完整的 `ResponseParameters` 字段。

## 兼容性边界

oxidebot 的通用模型不能表达 Telegram 的全部概念，因此以下行为是有意的：

- Telegram 不提供按 message_id 查询历史消息、好友列表、机器人所在群列表或群文件目录，这些通用接口会返回明确错误。
- `get_group_member_list` 只能返回 Telegram Bot API 允许查询的管理员列表。
- 临时“全群禁言”没有对应 Telegram API；永久修改群默认权限可用，临时禁言应逐成员执行。
- Rich Message、Guest Query、Ephemeral Message、Business Connection、支付、Inline Query 等 Telegram 专属流程目前以原始 JSON 暴露，没有伪装成语义不完整的通用事件。
- `get_file_info` 返回的 Telegram 下载 URL 含 bot token。不要记录、公开或持久化完整 URL；应把它视为敏感凭据。

## 从 0.1.2 升级到 0.1.3

0.1.3 移除了停留在 Bot API 7.9 的 `telegram_bot_api_rs` 依赖，并将配置类型改为本 crate 的 `GetUpdatesConfig`。常见迁移只需：

1. 把 `telegram_bot_api_rs::getting_updates::GetUpdateConfig` 改为 `telegram_bot_oxidebot::GetUpdatesConfig`；
2. 把 `TelegramBot::new(...).await` 改为 `TelegramBot::try_new(...).await?`；
3. 如果处理过适配器内部的 Telegram 类型，改为使用 `telegram_bot_oxidebot::telegram` 中的精简类型或 `TelegramRawEvent` 的 JSON。
