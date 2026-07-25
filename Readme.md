# telegram_bot_oxidebot

`telegram_bot_oxidebot` 是 [oxidebot](https://github.com/canxin121/oxidebot) 的 Telegram Bot API 适配器。

当前版本按 **Telegram Bot API 10.2（2026-07-14）** 核对。适配器只实现 oxidebot 通用接口所需的 Telegram API 子集，但接收更新时采用向前兼容设计：Telegram 新增字段会被保留，尚未映射为 oxidebot 标准事件的更新会作为原始事件交给上层，而不会导致整个 `getUpdates` 响应反序列化失败。

## 主要能力

- 纯 Rustls HTTPS，不依赖 OpenSSL。
- 长轮询支持 offset 提交、失败指数退避、Telegram `retry_after` 限流等待和自定义 `allowed_updates`。
- 支持 v2 `OutgoingMessage`：纯文本、UTF-16 实体富文本、图片、视频、音频、动画、语音、视频便笺、文件、相册、贴纸、自定义 emoji、位置、联系人、Poll/Quiz、Checklist 和 Bot API 10.2 Rich Message；支持 file_id、HTTP(S)、本地文件和 base64 上传。
- 自动遵守文本 4096、caption 1024、相册 2–10 项等 Telegram 约束；单个媒体不会再错误调用 `sendMediaGroup`。
- 支持消息发送/编辑/删除/批量删除、转发/复制、Reaction、Pin、Activity、Business Read Receipt、Poll 停止、Checklist 编辑、Forum Topic、邀请/订阅邀请、成员权限/标签/封禁、入群审批、群资料、用户资料和文件信息。
- 支持结构化命令的 Telegram 菜单降级、Inline Query suggestion、Chat Menu surface、Mini App query、localized bot profile、Invoice、Shipping、Checkout、Star refund。
- 将普通/频道/Business/Guest 消息、编辑、删除、成员、反应计数、Poll/Vote、Checklist、Topic、Pin、Inline Query、Mini App、支付和订阅映射为 v2 `LifecycleEvent`；同时保留旧 `MessageEvent`/`NoticeEvent`。
- 完整映射 Telegram inline keyboard、reply keyboard、Callback Query、Bot Commands 和 Chat Menu Button；包括 Bot API 10.2 的按钮颜色、自定义 emoji 图标、Managed Bot 请求和 ephemeral command。
- `callback_query` 会成为 `Event::InteractionEvent`；`users_shared`、`chat_shared`、`managed_bot_created` 和 `web_app_data` 也会产生类型化的选择/表单交互事件。
- 入站消息同时生成 `MessageEnvelope`，保留 rich content、forward/reply origin、topic、时间、reaction、pin 和完整 Telegram JSON；无法诚实映射的字段继续通过 `PlatformNativeData`/`TelegramRawEvent` 保留。
- `BotCapabilities` 逐项报告 Native、Emulated、Unsupported 和 Telegram 硬限制，不把“能发一条消息”误报成“支持全部富消息语义”。
- 通过 oxidebot 的 `PlatformApiRequest` 暴露全部 185 个 Bot API 10.2 方法；其他适配器不支持时会返回明确错误。
- 通过 `TelegramBot::client()` 继续暴露更低层的 `TelegramClient::call`，新发布且尚未进入方法目录的 Telegram API 也能立即调用。

## v2 公共模型映射

| oxidebot 能力 | Telegram Bot API 10.2 映射 | 边界 |
| --- | --- | --- |
| `ConversationRef` | private/group/channel、forum topic、direct-messages topic | `list_threads` 无官方 API |
| `RichText` | `MessageEntity` / `RichText`，自动转换 UTF-8 byte range 与 UTF-16 offset | Regular Message 与 Rich Message 支持的 style 集合不同 |
| `RichLayout` | `sendRichMessage` / `editMessageText.rich_message` | 不可表示的 layout style 明确报错 |
| `MessageOptions` | reply/quote、silent、protect、link preview、ephemeral receiver、draft、keyboard、business/native fields | Telegram 不提供 portable mention allow-list、scheduled message 和 idempotency key |
| `InteractionResponseHandle` | Callback Query ID + chat/message/inline 上下文 | Inline callback 没有 chat_id 时不能发送 follow-up |
| Reaction | `setMessageReaction` | 非 Premium bot 每条消息最多设置一个 reaction；不能列出 reaction users |
| Poll / Checklist | `sendPoll`、`stopPoll`、`sendChecklist`、`editMessageChecklist` | Checklist 仅 Connected Business 可发送/编辑 |
| Thread | Forum Topic create/edit/close/reopen/delete | Telegram 不支持枚举所有 topic |
| History | `getUserPersonalChatMessages` | 仅用户 Personal Chat，1–20 条；普通聊天历史不可查询 |
| Commands / suggestions | `setMyCommands`、`answerInlineQuery` | Telegram 命令菜单不注册 typed options；复杂 schema 返回错误 |
| Surface / Mini App | Chat Menu、`answerWebAppQuery`、init-data HMAC verification | 无 App Home / Sidebar / Rich Menu |
| Commerce | Invoice、Shipping、Pre-checkout、Star refund、Subscription update | Bot API 只能直接退款 Telegram Stars |

所有 `MessageRef`、`ConversationRef`、`MessageTarget` 和 option 类型都可以携带 `PlatformNativeData::new("telegram", ...)`。适配器会校验 platform，重复或错误平台字段会报错，不会覆盖 portable 字段。

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

## 按钮和交互

组件通过 `MessageOptions` 附在整条消息上，不属于 `MessageSegment`。这样相册拆分、长文本拆分和媒体上传不会把同一组按钮错误复制到每个子消息。

```rust,no_run
use anyhow::Result;
use oxidebot::{
    api::payload::SendMessageTarget, ActionRow, Button, ButtonStyle, CallApiTrait,
    InlineKeyboard, MessageComponents, MessageOptions,
};
use oxidebot::source::message::MessageSegment;
use telegram_bot_oxidebot::TelegramBot;

async fn send_actions(bot: &TelegramBot) -> Result<()> {
    let keyboard = InlineKeyboard::new([ActionRow::buttons([
        Button::callback("确认", "confirm").style(ButtonStyle::Success),
        Button::url("文档", "https://core.telegram.org/bots/api"),
        Button::copy_text("复制编号", "order-42"),
    ])]);

    bot.send_message_with_options(
        vec![MessageSegment::text("请选择操作")],
        SendMessageTarget::Private("123456".to_owned()),
        MessageOptions::default().components(MessageComponents::InlineKeyboard(keyboard)),
    )
    .await?;
    Ok(())
}
```

Inline keyboard 已适配以下动作：

- Callback、HTTP/HTTPS/`tg://` URL、HTTPS Web App 和 Login URL；
- 切换 inline query（选择聊天、当前聊天和按聊天类型选择）；
- Copy Text、Game 和 Pay；
- Bot API 10.2 的 `primary`、`success`、`danger` 样式和 `icon_custom_emoji_id`；
- `PlatformNativeData::new("telegram", json!(...))` 提供完整原生字段逃生口。

Telegram 不支持 disabled inline button 和 inline select menu，调用时会返回 `UnsupportedInteractionError`，不会静默降级。`callback_data` 会在发送前检查 1–64 bytes，Copy Text 检查 1–256 个字符，Game/Pay 必须位于第一行第一列，Web App/Login URL 必须是 HTTPS。

Callback Query 会直接成为 `Event::InteractionEvent`，可以用统一响应 API 完成 ACK、toast、alert 或打开 URL：

```rust,no_run
use anyhow::Result;
use oxidebot::{event::Event, InteractionResponse, Matcher};

async fn handle_interaction(matcher: &Matcher) -> Result<()> {
    if let Event::InteractionEvent(interaction) = matcher.event.as_ref() {
        interaction
            .respond(matcher.bot.clone(), InteractionResponse::toast("已处理"))
            .await?;
    }
    Ok(())
}
```

Telegram 的 `answerCallbackQuery` 本身只能 ACK、toast/alert 或打开 URL。适配器把 `InteractionResponse::Message` 模拟为 ACK 后向来源 chat 发送 follow-up，把 `UpdateMessage` 映射为编辑 callback 来源消息；公开/ephemeral visibility 会映射到 Bot API 10.2 的 receiver/callback 字段。Inline callback 若没有 `chat_id` 不能发送 follow-up，会返回明确错误。Telegram 没有 modal callback response，所以 `OpenModal` 仍明确报不支持。传 `None` 给 `edit_message_components` 会移除 inline keyboard。

## Reply Keyboard

Reply keyboard 支持普通文字、选择用户、选择聊天、创建 Managed Bot、请求联系人、位置、投票和 Web App，并支持 persistent、resize、one-time、selective、输入框 placeholder、Bot API 10.2 样式及自定义 emoji 图标。

```rust,no_run
use anyhow::Result;
use oxidebot::{
    api::payload::SendMessageTarget, CallApiTrait, MessageComponents, MessageOptions, ReplyButton,
    ReplyButtonRow, ReplyKeyboard, RequestUsers,
};
use oxidebot::source::message::MessageSegment;
use telegram_bot_oxidebot::TelegramBot;

async fn ask_for_user(bot: &TelegramBot) -> Result<()> {
    let keyboard = ReplyKeyboard::new([ReplyButtonRow::new([
        ReplyButton::request_users("选择用户", RequestUsers::new(1)),
        ReplyButton::request_location("发送位置"),
    ])])
    .resize(true)
    .one_time(true)
    .placeholder("请选择");

    bot.send_message_with_options(
        vec![MessageSegment::text("需要一些信息")],
        SendMessageTarget::Private("123456".to_owned()),
        MessageOptions::default().components(MessageComponents::ReplyKeyboard(keyboard)),
    )
    .await?;
    Ok(())
}
```

Telegram 只允许 reply keyboard、Force Reply 和 Remove Keyboard 附在单条消息上。对于相册，inline keyboard 会在相册发送成功后编辑到第一条媒体消息；reply keyboard 必须改为单独发送一条消息。

## Rich Message、Poll、Checklist 和 Topic

新代码可以直接使用 v2 API，避免把 reply、caption 或 topic 编码进旧的 segment/string ID：

```rust,no_run
use anyhow::Result;
use oxidebot::{
    CallApiTrait, ConversationKind, ConversationRef, MessageContent, MessageTarget,
    OutgoingMessage, RichText, TextStyle,
};
use telegram_bot_oxidebot::TelegramBot;

async fn send_to_topic(bot: &TelegramBot) -> Result<()> {
    let topic = ConversationRef::new("17", ConversationKind::Topic)
        .child_of(ConversationRef::group("-100123456"));
    let text = RichText::plain("Important update")
        .span(0..9, TextStyle::Bold);

    bot.send_outgoing_message(
        MessageTarget::new(topic),
        OutgoingMessage::new([MessageContent::RichText(text)]),
    )
    .await?;
    Ok(())
}
```

`MediaGallery` 会检查 2–10 项以及 Telegram 的 album class 规则：Photo/Video 可混合，Audio 只能与 Audio、Document 只能与 Document 组合。本地附件在 multipart 的 `attach://` 名称下发送。相册不能原子携带 markup，所以 inline keyboard 会在成功后编辑到第一条消息；reply keyboard 会提前报错。

`RichLayout` 映射 Telegram `InputRichBlock` 的 paragraph、table、list、quotation、preformatted、divider、collage、details、map、photo/video/audio/animation 等 block。表格会补齐 Telegram 必需的 align/valign，ordered list 使用 Telegram 规定的 `type: "1"`。需要 Telegram 独有 block 时可用 `LayoutNode::PlatformNative`。

Checklist 会验证 1–30 个 task、正数且不重复的 task ID、title/task 长度和不可直接设置的 completion state。Poll 会验证 2–12 个 option、open period、country code、correct option 和 Bot API 字符限制。

## Mini App 数据校验

`TelegramClient::verify_web_app_init_data` 按 Telegram 官方两层 HMAC-SHA256 算法校验 `initData`，使用常量时间 MAC 比较，并可用 `max_age` 阻止重放。返回值会把 `user`、`receiver`、`chat` 解码为 JSON，把 `auth_date` 解码为整数。

```rust,no_run
use std::time::Duration;

use anyhow::Result;
use telegram_bot_oxidebot::TelegramClient;

fn verify_init_data(client: &TelegramClient, init_data: &str) -> Result<i64> {
    let data = client.verify_web_app_init_data(init_data, Some(Duration::from_secs(300)))?;
    Ok(data["user"]["id"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("Mini App user has no id"))?)
}
```

## Bot Commands 和聊天菜单

```rust,no_run
use anyhow::Result;
use oxidebot::{
    BotCommand, BotCommandSet, CallApiTrait, ChatMenu, CommandScope,
};
use telegram_bot_oxidebot::TelegramBot;

async fn configure_ui(bot: &TelegramBot) -> Result<()> {
    bot.set_bot_commands(
        BotCommandSet::new([
            BotCommand::new("start", "开始"),
            BotCommand::new("account", "查看账户").ephemeral(true),
        ])
        .scope(CommandScope::AllPrivateChats)
        .language_code("zh"),
    )
    .await?;

    bot.set_chat_menu(
        None,
        ChatMenu::WebApp {
            text: "打开应用".to_owned(),
            url: "https://example.com/app".to_owned(),
        },
    )
    .await?;
    Ok(())
}
```

命令 API 覆盖 Telegram 的 7 种 scope、语言代码，以及 Bot API 10.2 的 `is_ephemeral`。聊天菜单覆盖 Default、Commands 和 Web App；Telegram 无法表示 oxidebot 的多行动作持久菜单，所以 `ChatMenu::Actions` 会返回明确错误。

## 原始 Telegram 事件

无法直接表示为 oxidebot 标准事件的更新会成为 `Event::AnyEvent`：

- `AnyEvent.r#type` 是 Telegram 原始 update 字段名；当前或未来出现且还没有 portable semantic mapping 的 update 会走这个路径；
- `AnyEvent.data` 可向下转换为 `telegram_bot_oxidebot::event::TelegramRawEvent`；
- `TelegramRawEvent.data` 保留 Telegram 返回的完整 JSON。

对于已经映射成 oxidebot 标准事件的 update，`Matcher.event_object` 仍保存完整的 `UpdateEvent`，`Matcher::event_envelopes` 还会给出 update ID、时间、conversation 和 raw JSON；也可通过 `EventTrait::as_any` 向下转换后读取原始 `Update`。因此标准事件映射不会阻断业务层访问 Telegram 的完整数据。这种设计也意味着 Telegram 后续增加 update 类型时，机器人不会因为依赖库缺少枚举变体而卡死在同一个 offset。

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

- Telegram 不提供按 message_id 查询普通历史消息、好友列表、机器人所在群列表或群文件目录；仅 `getUserPersonalChatMessages` 可读取用户 Personal Chat 的最后 1–20 条消息。
- `get_group_member_list` 只能返回 Telegram Bot API 允许查询的管理员列表。
- 临时“全群禁言”没有对应 Telegram API；永久修改群默认权限可用，临时禁言应逐成员执行。
- `getChat` 只能返回最近一条 pinned message，所以 `list_pins` 最多返回一条；Bot API 也不能枚举全部 forum topics。
- Telegram 非 Premium bot 每条消息最多设置一个 reaction，且 Bot API 不提供 reaction-user 列表。
- `MessageVisibility::Ephemeral` 需要 receiver 或 Callback Query 上下文；Inline callback 没有 chat 地址时不能模拟 follow-up。
- Rich Message、Guest/Business/Ephemeral Message、支付和 Inline Query 已映射公共语义；仍无法表达的 Telegram 字段保留在各对象的 `PlatformNativeData`，完整未知 update 才使用 `TelegramRawEvent`。
- `get_file_info` 返回的 Telegram 下载 URL 含 bot token。不要记录、公开或持久化完整 URL；应把它视为敏感凭据。

## 从 0.1.2 升级到 0.1.3

0.1.3 移除了停留在 Bot API 7.9 的 `telegram_bot_api_rs` 依赖，并将配置类型改为本 crate 的 `GetUpdatesConfig`。常见迁移只需：

1. 把 `telegram_bot_api_rs::getting_updates::GetUpdateConfig` 改为 `telegram_bot_oxidebot::GetUpdatesConfig`；
2. 把 `TelegramBot::new(...).await` 改为 `TelegramBot::try_new(...).await?`；
3. 如果处理过适配器内部的 Telegram 类型，改为使用 `telegram_bot_oxidebot::telegram` 中的精简类型或 `TelegramRawEvent` 的 JSON。

## 从 0.1.3 升级到 0.1.4

0.1.4 使用 `oxidebot 0.1.8` 新增的统一交互层。原有 `send_message` 和事件处理代码保持可用；需要按钮时改用 `send_message_with_options`。`callback_query` 不再以 `TelegramRawEvent` 交付，而是 `Event::InteractionEvent`，完整 Telegram JSON 仍保存在 `InteractionEvent.data` 和 `Matcher.event_object` 中。
