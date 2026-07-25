use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use base64::Engine as _;
use chrono::{DateTime, Utc};
use oxidebot::{
    application::{
        CommandInvocation, MiniAppEvent, SuggestionRequest, SuggestionSelection, VerificationState,
    },
    collaboration::{
        CallKind, CallSession, CallState, PinnedMessage, Reaction, ReactionChange, ReactionSummary,
    },
    commerce::{
        CheckoutRequest, CustomerDetails, Money, Payment, PaymentStatus, ShippingRequest,
        Subscription,
    },
    content::{
        Checklist, ChecklistChange, ChecklistTask, ContactCard, ForwardContext, Media,
        MessageContent, MessageEnvelope, MessageOrigin, PhoneNumber, Poll, PollOption, PollType,
        ReplyContext, RichText, Sticker as PortableSticker, TextSpan, TextStyle,
    },
    conversation::{
        ConversationKind, ConversationMember, ConversationPermission, ConversationProfile,
        ConversationRef, JoinRequest, MessageRef, PermissionSet, RoleRef, Thread, ThreadState,
    },
    event::{
        any::{AnyEvent, AnyEventDataTrait},
        notice::{
            GroupAdminChangeEvent, GroupAdminChangeType, GroupMemberAliasChangeEvent,
            GroupMemberDecreaseEvent, GroupMemberDecreaseReason, GroupMemberIncreseEvent,
            GroupMemberIncreseReason, GroupMemberMuteChangeEvent, MessageEditedEvent,
            MessageReactionsEvent, MuteType,
        },
        request::GroupAddEvent,
        Event, EventEnvelope, EventObject, LifecycleEvent, MessageEvent, NoticeEvent, RequestEvent,
    },
    interaction::{
        InteractionEvent, InteractionKind, InteractionResponseHandle, PlatformNativeData,
    },
    source::message::{File, Message},
    EventTrait, FormValue,
};
use serde_json::Value;

use crate::{
    segment::{parse_message, parse_reaction},
    telegram::{
        CallbackQuery, ChatMemberUpdated, Message as TelegramMessage, MessageReactionUpdated,
        Update,
    },
    utils::{join_request_id, message_id, parse_chat_sender, parse_group, parse_user},
    SERVER,
};

#[derive(Clone, Debug)]
pub struct UpdateEvent(pub Update);

impl UpdateEvent {
    pub fn boxed(update: Update) -> EventObject {
        Box::new(Self(update))
    }
}

impl EventTrait for UpdateEvent {
    fn get_events(&self) -> Vec<Event> {
        parse_update(&self.0)
    }

    fn server(&self) -> &'static str {
        SERVER
    }

    fn get_event_envelopes(&self) -> Vec<EventEnvelope> {
        let raw = self.0.value().cloned();
        let occurred_at = raw
            .as_ref()
            .and_then(|value| value.get("date"))
            .and_then(Value::as_i64)
            .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0));
        let conversation = raw.as_ref().and_then(telegram_raw_conversation);
        parse_update(&self.0)
            .into_iter()
            .enumerate()
            .map(|(index, event)| {
                let mut envelope = EventEnvelope::new(SERVER, event);
                envelope.id = format!("{}:{index}", self.0.update_id);
                envelope.occurred_at = occurred_at;
                envelope.conversation = conversation.clone();
                envelope.raw = raw.clone();
                envelope
            })
            .collect()
    }

    fn clone_box(&self) -> EventObject {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Raw Telegram data for update kinds that do not have an oxidebot-native
/// event. This is also the compatibility escape hatch for future Bot API
/// update kinds.
#[derive(Clone, Debug)]
pub struct TelegramRawEvent {
    pub update_id: i64,
    pub kind: String,
    pub data: Value,
}

impl AnyEventDataTrait for TelegramRawEvent {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn parse_update(update: &Update) -> Vec<Event> {
    let Some(kind) = update.kind() else {
        return vec![raw_event(update, "unknown", Value::Null)];
    };

    let parsed = match kind {
        "message" | "business_message" | "guest_message" => {
            update.decode::<TelegramMessage>(kind).map(|message| {
                parse_message_update(message, update.value().cloned().unwrap_or(Value::Null))
            })
        }
        "edited_message" | "edited_business_message" => {
            update.decode::<TelegramMessage>(kind).map(|message| {
                parse_edited_message(message, update.value().cloned().unwrap_or(Value::Null))
            })
        }
        "channel_post" => update.decode::<TelegramMessage>(kind).map(|message| {
            parse_message_update(message, update.value().cloned().unwrap_or(Value::Null))
        }),
        "edited_channel_post" => update.decode::<TelegramMessage>(kind).map(|message| {
            parse_edited_message(message, update.value().cloned().unwrap_or(Value::Null))
        }),
        "deleted_business_messages" => Ok(parse_deleted_messages(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "message_reaction" => update
            .decode::<MessageReactionUpdated>(kind)
            .map(parse_message_reaction),
        "message_reaction_count" => Ok(parse_message_reaction_count(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "poll" => Ok(parse_poll_update(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "poll_answer" => Ok(parse_poll_answer(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "inline_query" => Ok(parse_inline_query(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "chosen_inline_result" => Ok(parse_chosen_inline_result(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "shipping_query" => Ok(parse_shipping_query(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "pre_checkout_query" => Ok(parse_checkout_query(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "purchased_paid_media" => Ok(parse_paid_media_purchase(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "subscription" => Ok(parse_subscription(
            update.value().cloned().unwrap_or(Value::Null),
        )),
        "callback_query" => update.decode::<CallbackQuery>(kind).map(|query| {
            vec![parse_callback_query(
                query,
                update.value().cloned().unwrap_or(Value::Null),
            )]
        }),
        "my_chat_member" | "chat_member" => update
            .decode::<ChatMemberUpdated>(kind)
            .map(parse_chat_member_update),
        "chat_join_request" => {
            update
                .decode::<crate::telegram::ChatJoinRequest>(kind)
                .map(|request| {
                    let conversation = telegram_chat_conversation(&request.chat);
                    let user = parse_user(request.from.clone());
                    vec![
                        Event::RequestEvent(RequestEvent::GroupAddEvent(GroupAddEvent {
                            id: join_request_id(request.chat.id, request.from.id),
                            user: user.clone(),
                            group: parse_group(request.chat.clone()),
                            message: request.bio.clone(),
                        })),
                        Event::LifecycleEvent(LifecycleEvent::JoinRequested(JoinRequest {
                            id: join_request_id(request.chat.id, request.from.id),
                            conversation,
                            user,
                            message: request.bio,
                            requested_at: DateTime::from_timestamp(request.date, 0),
                            platform_data: Some(PlatformNativeData::new(
                                SERVER,
                                serde_json::json!({
                                    "user_chat_id": request.user_chat_id,
                                    "query_id": request.query_id,
                                }),
                            )),
                        })),
                    ]
                })
        }
        "business_connection" | "chat_boost" | "removed_chat_boost" | "managed_bot" => {
            return vec![
                Event::LifecycleEvent(LifecycleEvent::PlatformNative(PlatformNativeData::new(
                    SERVER,
                    update.value().cloned().unwrap_or(Value::Null),
                ))),
                raw_event(update, kind, update.value().cloned().unwrap_or(Value::Null)),
            ];
        }
        _ => {
            return vec![raw_event(
                update,
                kind,
                update.value().cloned().unwrap_or(Value::Null),
            )]
        }
    };

    match parsed {
        Ok(events) if !events.is_empty() => events,
        Ok(_) => vec![raw_event(
            update,
            kind,
            update.value().cloned().unwrap_or(Value::Null),
        )],
        Err(error) => {
            tracing::error!(
                update_id = update.update_id,
                update_kind = kind,
                error = %error,
                "failed to map Telegram update; forwarding it as a raw event"
            );
            vec![raw_event(
                update,
                kind,
                update.value().cloned().unwrap_or(Value::Null),
            )]
        }
    }
}

fn raw_event(update: &Update, kind: &str, data: Value) -> Event {
    Event::AnyEvent(AnyEvent {
        server: SERVER,
        r#type: kind.to_owned(),
        data: Box::new(TelegramRawEvent {
            update_id: update.update_id,
            kind: kind.to_owned(),
            data,
        }),
    })
}

fn parse_message_update(message: TelegramMessage, raw_data: Value) -> Vec<Event> {
    let mut events = Vec::new();

    let sender = message
        .from
        .clone()
        .map(parse_user)
        .or_else(|| message.sender_chat.clone().map(parse_chat_sender));
    let parsed_message = parse_message(message.clone());
    if let Some(sender) = sender.clone() {
        events.extend(parse_message_interactions(
            &message,
            &parsed_message,
            sender,
            raw_data.clone(),
        ));
    }
    if !parsed_message.segments.is_empty() {
        if let Some(sender) = sender {
            events.push(Event::MessageEvent(MessageEvent {
                id: message_id(message.chat.id, message.message_id),
                time: DateTime::from_timestamp(message.date, 0),
                sender,
                group: (!is_private(&message)).then(|| parse_group(message.chat.clone())),
                message: parsed_message,
            }));
        }
    }
    events.extend(parse_message_lifecycle(&message, &raw_data));
    events.push(Event::LifecycleEvent(LifecycleEvent::MessageCreated(
        Box::new(telegram_message_envelope(&message, raw_data)),
    )));

    for member in message.new_chat_members.clone().into_iter().flatten() {
        events.push(Event::NoticeEvent(NoticeEvent::GroupMemberIncreseEvent(
            GroupMemberIncreseEvent {
                group: parse_group(message.chat.clone()),
                user: parse_user(member),
                reason: GroupMemberIncreseReason::Unknown,
            },
        )));
    }
    if let Some(member) = message.left_chat_member {
        events.push(Event::NoticeEvent(NoticeEvent::GroupMemberDecreaseEvent(
            GroupMemberDecreaseEvent {
                group: parse_group(message.chat),
                user: parse_user(member),
                reason: GroupMemberDecreaseReason::Unknown,
            },
        )));
    }
    events
}

fn parse_message_interactions(
    telegram_message: &TelegramMessage,
    message: &Message,
    user: oxidebot::source::user::User,
    raw_data: Value,
) -> Vec<Event> {
    let group = (telegram_message.chat.kind != "private")
        .then(|| parse_group(telegram_message.chat.clone()));
    let mut interactions = Vec::new();

    if let (Some(text), Some(command_entity)) = (
        telegram_message.text.as_deref(),
        telegram_message.entities.as_ref().and_then(|entities| {
            entities
                .iter()
                .find(|entity| entity.kind == "bot_command" && entity.offset == 0)
        }),
    ) {
        if let Some(command_end) = utf16_to_byte_offset(text, command_entity.length) {
            let command_text = &text[..command_end];
            let command_name = command_text
                .trim_start_matches('/')
                .split('@')
                .next()
                .unwrap_or_default()
                .to_owned();
            let arguments = text[command_end..].trim().to_owned();
            let fields = if arguments.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([(
                    "arguments".to_owned(),
                    vec![FormValue::Text(arguments.clone())],
                )])
            };
            interactions.push(Event::InteractionEvent(InteractionEvent {
                id: format!("{}:command", message.id),
                kind: InteractionKind::Command,
                action_id: Some(command_name.clone()),
                values: (!arguments.is_empty())
                    .then_some(arguments.clone())
                    .into_iter()
                    .collect(),
                user: user.clone(),
                group: group.clone(),
                message: Some(message.clone()),
                context_id: Some(message.id.clone()),
                response: None,
                fields: fields.clone(),
                command: Some(CommandInvocation {
                    command_id: None,
                    name: command_name,
                    path: Vec::new(),
                    options: fields,
                    raw_text: Some(text.to_owned()),
                    target_user: None,
                    target_message: None,
                    permissions: BTreeSet::new(),
                    locale: raw_data
                        .pointer("/from/language_code")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    platform_data: Some(PlatformNativeData::new(SERVER, raw_data.clone())),
                }),
                locale: raw_data
                    .pointer("/from/language_code")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                permissions: BTreeSet::new(),
                data: raw_data.clone(),
            }));
        }
    }

    if let Some(shared) = telegram_message.extra.get("users_shared") {
        let action_id = shared
            .get("request_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string());
        let values = shared
            .get("users")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|user| user.get("user_id").and_then(Value::as_i64))
            .map(|id| id.to_string())
            .collect::<Vec<_>>();
        let form_values = values
            .iter()
            .map(|id| {
                FormValue::User(oxidebot::source::user::User {
                    id: id.clone(),
                    ..Default::default()
                })
            })
            .collect();
        interactions.push(message_interaction(
            message,
            "users_shared",
            InteractionKind::Select,
            action_id,
            values,
            form_values,
            user.clone(),
            group.clone(),
            raw_data.clone(),
        ));
    }

    if let Some(shared) = telegram_message.extra.get("chat_shared") {
        let action_id = shared
            .get("request_id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string());
        let values = shared
            .get("chat_id")
            .and_then(Value::as_i64)
            .map(|id| vec![id.to_string()])
            .unwrap_or_default();
        let form_values = values
            .iter()
            .map(|id| {
                FormValue::Conversation(ConversationRef::new(id.clone(), ConversationKind::Unknown))
            })
            .collect();
        interactions.push(message_interaction(
            message,
            "chat_shared",
            InteractionKind::Select,
            action_id,
            values,
            form_values,
            user.clone(),
            group.clone(),
            raw_data.clone(),
        ));
    }

    if let Some(web_app) = telegram_message.extra.get("web_app_data") {
        let action_id = web_app
            .get("button_text")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let values = web_app
            .get("data")
            .and_then(Value::as_str)
            .map(|data| vec![data.to_owned()])
            .unwrap_or_default();
        interactions.push(message_interaction(
            message,
            "web_app_data",
            InteractionKind::Form,
            action_id,
            values.clone(),
            values.into_iter().map(FormValue::Text).collect(),
            user.clone(),
            group.clone(),
            raw_data.clone(),
        ));
    }

    if let Some(managed_bot) = telegram_message.extra.get("managed_bot_created") {
        let values = managed_bot
            .pointer("/bot/id")
            .and_then(Value::as_i64)
            .map(|id| vec![id.to_string()])
            .unwrap_or_default();
        interactions.push(message_interaction(
            message,
            "managed_bot_created",
            InteractionKind::Select,
            Some("managed_bot_created".to_owned()),
            values.clone(),
            values.into_iter().map(FormValue::Text).collect(),
            user,
            group,
            raw_data,
        ));
    }

    interactions
}

#[allow(clippy::too_many_arguments)]
fn message_interaction(
    message: &Message,
    kind_name: &str,
    kind: InteractionKind,
    action_id: Option<String>,
    values: Vec<String>,
    field_values: Vec<FormValue>,
    user: oxidebot::source::user::User,
    group: Option<oxidebot::source::group::Group>,
    data: Value,
) -> Event {
    let field_id = action_id.clone().unwrap_or_else(|| kind_name.to_owned());
    let fields = [(field_id, field_values)].into_iter().collect();
    Event::InteractionEvent(InteractionEvent {
        id: format!("{}:{kind_name}", message.id),
        kind,
        action_id,
        values,
        user,
        group,
        message: Some(message.clone()),
        context_id: Some(message.id.clone()),
        response: None,
        fields,
        command: None,
        locale: None,
        permissions: BTreeSet::new(),
        data,
    })
}

fn parse_edited_message(message: TelegramMessage, raw_data: Value) -> Vec<Event> {
    let sender = message
        .from
        .clone()
        .map(parse_user)
        .or_else(|| message.sender_chat.clone().map(parse_chat_sender));
    let Some(sender) = sender else {
        return Vec::new();
    };

    vec![
        Event::NoticeEvent(NoticeEvent::MessageEditedEvent(MessageEditedEvent {
            user: sender.clone(),
            group: (!is_private(&message)).then(|| parse_group(message.chat.clone())),
            new_message: Some(parse_message(message.clone())),
            operator: Some(sender),
            old_message: None,
        })),
        Event::LifecycleEvent(LifecycleEvent::MessageUpdated(Box::new(
            telegram_message_envelope(&message, raw_data),
        ))),
    ]
}

fn is_private(message: &TelegramMessage) -> bool {
    message.chat.kind == "private"
}

fn parse_message_reaction(reaction: MessageReactionUpdated) -> Vec<Event> {
    let actor = reaction
        .user
        .map(parse_user)
        .or_else(|| reaction.actor_chat.map(parse_chat_sender));
    let Some(actor) = actor else {
        return Vec::new();
    };

    let old = reaction.old_reaction.clone();
    let new = reaction.new_reaction.clone();
    let message_reference = MessageRef::new(reaction.message_id.to_string())
        .in_conversation(telegram_chat_conversation(&reaction.chat));
    vec![
        Event::NoticeEvent(NoticeEvent::MessageReactionsEvent(MessageReactionsEvent {
            user: actor.clone(),
            group: (reaction.chat.kind != "private").then(|| parse_group(reaction.chat.clone())),
            message: Message {
                id: message_id(reaction.chat.id, reaction.message_id),
                segments: Vec::new(),
            },
            reactions: reaction
                .new_reaction
                .into_iter()
                .map(parse_reaction)
                .collect(),
        })),
        Event::LifecycleEvent(LifecycleEvent::ReactionsChanged {
            message: message_reference,
            actor: Some(actor),
            change: ReactionChange {
                added: new
                    .iter()
                    .filter(|reaction| !old.contains(reaction))
                    .cloned()
                    .map(telegram_reaction_type)
                    .collect(),
                removed: old
                    .iter()
                    .filter(|reaction| !new.contains(reaction))
                    .cloned()
                    .map(telegram_reaction_type)
                    .collect(),
                current: new
                    .into_iter()
                    .map(|reaction| ReactionSummary {
                        reaction: telegram_reaction_type(reaction),
                        count: 1,
                        reacted_by_bot: false,
                        recent_users: Vec::new(),
                        platform_data: None,
                    })
                    .collect(),
            },
        }),
    ]
}

fn telegram_chat_conversation(chat: &crate::telegram::Chat) -> ConversationRef {
    ConversationRef::new(
        chat.id.to_string(),
        match chat.kind.as_str() {
            "private" => ConversationKind::Direct,
            "group" | "supergroup" => ConversationKind::Group,
            "channel" => ConversationKind::Channel,
            kind => ConversationKind::PlatformNative(kind.to_owned()),
        },
    )
}

fn telegram_raw_conversation(value: &Value) -> Option<ConversationRef> {
    let chat = value
        .get("chat")
        .or_else(|| value.get("message").and_then(|message| message.get("chat")))?;
    let id = chat.get("id")?.as_i64()?;
    let kind = chat
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(ConversationRef::new(
        id.to_string(),
        match kind {
            "private" => ConversationKind::Direct,
            "group" | "supergroup" => ConversationKind::Group,
            "channel" => ConversationKind::Channel,
            kind => ConversationKind::PlatformNative(kind.to_owned()),
        },
    ))
}

fn telegram_message_conversation(message: &TelegramMessage) -> ConversationRef {
    let root = telegram_chat_conversation(&message.chat);
    if let Some(topic_id) = message
        .extra
        .get("direct_messages_topic")
        .and_then(|topic| topic.get("topic_id"))
        .or_else(|| message.extra.get("direct_messages_topic_id"))
        .and_then(Value::as_i64)
    {
        return ConversationRef::new(topic_id.to_string(), ConversationKind::Topic)
            .child_of(root)
            .platform_data(PlatformNativeData::new(
                SERVER,
                serde_json::json!({"direct_messages_topic_id": topic_id}),
            ));
    }
    if let Some(thread_id) = message
        .extra
        .get("message_thread_id")
        .and_then(Value::as_i64)
    {
        return ConversationRef::new(thread_id.to_string(), ConversationKind::Topic).child_of(root);
    }
    root
}

pub(crate) fn telegram_message_envelope(message: &TelegramMessage, raw: Value) -> MessageEnvelope {
    let conversation = telegram_message_conversation(message);
    let mut reference =
        MessageRef::new(message.message_id.to_string()).in_conversation(conversation.clone());
    let mut reference_data = serde_json::Map::new();
    for name in [
        "business_connection_id",
        "receiver_user_id",
        "ephemeral_message_id",
    ] {
        if let Some(value) = message.extra.get(name) {
            reference_data.insert(name.to_owned(), value.clone());
        }
    }
    if !reference_data.is_empty() {
        reference.platform_data = Some(PlatformNativeData::new(
            SERVER,
            Value::Object(reference_data),
        ));
    }
    let sender = message
        .from
        .clone()
        .map(parse_user)
        .or_else(|| message.sender_chat.clone().map(parse_chat_sender));
    let reply_to = message
        .extra
        .get("reply_to_message")
        .and_then(|reply| reply.get("message_id"))
        .and_then(Value::as_i64)
        .map(|id| MessageRef::new(id.to_string()).in_conversation(conversation.clone()));
    let forward_context = message
        .extra
        .get("forward_origin")
        .and_then(telegram_forward_context);
    MessageEnvelope {
        reference,
        sender,
        conversation: Some(conversation.clone()),
        created_at: DateTime::from_timestamp(message.date, 0),
        edited_at: message
            .extra
            .get("edit_date")
            .and_then(Value::as_i64)
            .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
        content: telegram_message_content(message, &raw),
        reply_to: reply_to.clone(),
        reply_context: (reply_to.is_some()
            || message.extra.contains_key("quote")
            || message.extra.contains_key("external_reply"))
        .then(|| ReplyContext {
            message: reply_to,
            quote: message
                .extra
                .get("quote")
                .and_then(|quote| quote.get("text"))
                .and_then(Value::as_str)
                .map(RichText::plain),
            quote_position: message
                .extra
                .get("quote")
                .and_then(|quote| quote.get("position"))
                .and_then(Value::as_u64)
                .and_then(|position| u32::try_from(position).ok()),
            external_origin: message
                .extra
                .get("external_reply")
                .and_then(|reply| reply.get("origin"))
                .and_then(telegram_message_origin),
            platform_data: message
                .extra
                .get("external_reply")
                .or_else(|| message.extra.get("quote"))
                .cloned()
                .map(|value| PlatformNativeData::new(SERVER, value)),
        }),
        forwarded_from: message
            .extra
            .get("forward_origin")
            .and_then(|origin| origin.get("message_id"))
            .and_then(Value::as_i64)
            .map(|id| MessageRef::new(id.to_string())),
        forward_context,
        reactions: raw
            .get("reactions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|count| ReactionSummary {
                reaction: count
                    .get("type")
                    .map(telegram_reaction_value)
                    .unwrap_or_else(|| Reaction::UnicodeEmoji(String::new())),
                count: count
                    .get("total_count")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                reacted_by_bot: count
                    .get("is_chosen")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                recent_users: Vec::new(),
                platform_data: Some(PlatformNativeData::new(SERVER, count.clone())),
            })
            .collect(),
        pinned: raw.get("is_pinned").and_then(Value::as_bool),
        metadata: BTreeMap::new(),
        platform_data: Some(PlatformNativeData::new(SERVER, raw)),
    }
}

fn telegram_forward_context(origin: &Value) -> Option<ForwardContext> {
    Some(ForwardContext {
        origin: telegram_message_origin(origin)?,
        sent_at: origin
            .get("date")
            .and_then(Value::as_i64)
            .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
        automatically_forwarded: origin
            .get("is_automatic_forward")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        platform_data: Some(PlatformNativeData::new(SERVER, origin.clone())),
    })
}

fn telegram_message_origin(origin: &Value) -> Option<MessageOrigin> {
    match origin.get("type").and_then(Value::as_str) {
        Some("user") => origin
            .get("sender_user")
            .and_then(telegram_value_user)
            .map(MessageOrigin::User),
        Some("hidden_user") => Some(MessageOrigin::HiddenUser {
            name: origin
                .get("sender_user_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }),
        Some("chat" | "channel") => {
            let chat = origin.get("sender_chat").or_else(|| origin.get("chat"))?;
            let id = chat.get("id").and_then(Value::as_i64)?;
            let kind = chat
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let conversation = ConversationRef::new(
                id.to_string(),
                match kind {
                    "private" => ConversationKind::Direct,
                    "group" | "supergroup" => ConversationKind::Group,
                    "channel" => ConversationKind::Channel,
                    kind => ConversationKind::PlatformNative(kind.to_owned()),
                },
            );
            Some(MessageOrigin::Conversation {
                conversation: conversation.clone(),
                message: origin
                    .get("message_id")
                    .and_then(Value::as_i64)
                    .map(|id| MessageRef::new(id.to_string()).in_conversation(conversation)),
                author_signature: origin
                    .get("author_signature")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        _ => Some(MessageOrigin::PlatformNative(PlatformNativeData::new(
            SERVER,
            origin.clone(),
        ))),
    }
}

fn telegram_message_content(message: &TelegramMessage, raw: &Value) -> Vec<MessageContent> {
    let mut content = Vec::new();
    if let Some(text) = &message.text {
        let rich =
            telegram_inbound_rich_text(text, message.entities.as_deref().unwrap_or_default());
        content.push(if rich.spans.is_empty() {
            MessageContent::PlainText(text.clone())
        } else {
            MessageContent::RichText(rich)
        });
    }
    let caption = message.caption.as_ref().map(|caption| {
        telegram_inbound_rich_text(
            caption,
            message.caption_entities.as_deref().unwrap_or_default(),
        )
    });
    if let Some(photo) = raw
        .get("photo")
        .and_then(Value::as_array)
        .and_then(|photos| photos.last())
    {
        content.push(MessageContent::Image(telegram_inbound_media(
            photo,
            caption.clone(),
        )));
    }
    for (field, constructor) in [
        (
            "animation",
            MessageContent::Animation as fn(Media) -> MessageContent,
        ),
        ("audio", MessageContent::Audio),
        ("video", MessageContent::Video),
        ("video_note", MessageContent::VideoNote),
        ("document", MessageContent::File),
        ("voice", MessageContent::VoiceNote),
    ] {
        if let Some(media) = raw.get(field) {
            content.push(constructor(telegram_inbound_media(media, caption.clone())));
        }
    }
    if let Some(sticker) = raw.get("sticker") {
        content.push(MessageContent::Sticker(PortableSticker {
            id: sticker
                .get("file_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            pack_id: sticker
                .get("set_name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            emoji: sticker
                .get("emoji")
                .and_then(Value::as_str)
                .map(str::to_owned),
            file: Some(telegram_inbound_file(sticker)),
            animated: sticker
                .get("is_animated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            video: sticker
                .get("is_video")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            platform_data: Some(PlatformNativeData::new(SERVER, sticker.clone())),
        }));
    }
    if let Some(venue) = raw.get("venue") {
        if let Some(location) = venue.get("location") {
            content.push(MessageContent::Location(oxidebot::LocationContent {
                latitude: location
                    .get("latitude")
                    .and_then(Value::as_f64)
                    .unwrap_or_default(),
                longitude: location
                    .get("longitude")
                    .and_then(Value::as_f64)
                    .unwrap_or_default(),
                title: venue
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                address: venue
                    .get("address")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                platform_data: Some(PlatformNativeData::new(SERVER, venue.clone())),
                ..Default::default()
            }));
        }
    } else if let Some(location) = raw.get("location") {
        content.push(MessageContent::Location(oxidebot::LocationContent {
            latitude: location
                .get("latitude")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            longitude: location
                .get("longitude")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            horizontal_accuracy: location.get("horizontal_accuracy").and_then(Value::as_f64),
            live_period: location
                .get("live_period")
                .and_then(Value::as_u64)
                .map(Duration::from_secs),
            heading: location
                .get("heading")
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok()),
            proximity_alert_radius: location
                .get("proximity_alert_radius")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
            platform_data: Some(PlatformNativeData::new(SERVER, location.clone())),
            ..Default::default()
        }));
    }
    if let Some(contact) = raw.get("contact") {
        content.push(MessageContent::Contact(ContactCard {
            user_id: contact
                .get("user_id")
                .and_then(Value::as_i64)
                .map(|id| id.to_string()),
            first_name: contact
                .get("first_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            last_name: contact
                .get("last_name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            phone_numbers: contact
                .get("phone_number")
                .and_then(Value::as_str)
                .map(|number| {
                    vec![PhoneNumber {
                        label: None,
                        value: number.to_owned(),
                    }]
                })
                .unwrap_or_default(),
            emails: Vec::new(),
            organization: None,
            vcard: contact
                .get("vcard")
                .and_then(Value::as_str)
                .map(str::to_owned),
            platform_data: Some(PlatformNativeData::new(SERVER, contact.clone())),
        }));
    }
    if let Some(poll) = raw.get("poll").and_then(telegram_inbound_poll) {
        content.push(MessageContent::Poll(poll));
    }
    if let Some(checklist) = raw.get("checklist").and_then(telegram_inbound_checklist) {
        content.push(MessageContent::Checklist(checklist));
    }
    for field in ["rich_message", "live_photo", "paid_media", "story"] {
        if let Some(value) = raw.get(field) {
            content.push(MessageContent::PlatformNative(PlatformNativeData::new(
                SERVER,
                serde_json::json!({"field": field, "value": value}),
            )));
        }
    }
    content
}

fn telegram_inbound_file(value: &Value) -> File {
    File {
        id: value
            .get("file_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        name: value
            .get("file_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        uri: None,
        base64: None,
        mime: value
            .get("mime_type")
            .and_then(Value::as_str)
            .and_then(|mime| mime.parse().ok()),
        size: value.get("file_size").and_then(Value::as_u64),
    }
}

fn telegram_inbound_media(value: &Value, caption: Option<RichText>) -> Media {
    Media {
        file: telegram_inbound_file(value),
        caption,
        thumbnail: value
            .get("thumbnail")
            .map(telegram_inbound_file)
            .filter(|file| file.id.is_some()),
        width: value
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        height: value
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        duration: value
            .get("duration")
            .and_then(Value::as_u64)
            .map(Duration::from_secs),
        spoiler: value
            .get("has_spoiler")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        alt_text: None,
        waveform: value
            .get("waveform")
            .and_then(Value::as_str)
            .and_then(|waveform| {
                base64::engine::general_purpose::STANDARD
                    .decode(waveform)
                    .ok()
            }),
        platform_data: Some(PlatformNativeData::new(SERVER, value.clone())),
    }
}

fn telegram_inbound_rich_text(text: &str, entities: &[crate::telegram::MessageEntity]) -> RichText {
    let spans = entities
        .iter()
        .filter_map(|entity| {
            let start = utf16_to_byte_offset(text, entity.offset)?;
            let end = utf16_to_byte_offset(text, entity.offset.checked_add(entity.length)?)?;
            let style = match entity.kind.as_str() {
                "bold" => TextStyle::Bold,
                "italic" => TextStyle::Italic,
                "underline" => TextStyle::Underline,
                "strikethrough" => TextStyle::Strikethrough,
                "spoiler" => TextStyle::Spoiler,
                "code" => TextStyle::Code,
                "pre" => TextStyle::Preformatted { language: None },
                "text_link" => TextStyle::Link {
                    url: entity.url.clone()?,
                },
                "text_mention" => TextStyle::UserMention {
                    user_id: entity.user.as_ref()?.id.to_string(),
                },
                "custom_emoji" => TextStyle::CustomEmoji {
                    id: entity.custom_emoji_id.clone()?,
                    fallback: Some(text[start..end].to_owned()),
                },
                "blockquote" | "expandable_blockquote" => TextStyle::Quote,
                "hashtag" => TextStyle::Hashtag,
                "cashtag" => TextStyle::Cashtag,
                "bot_command" => TextStyle::BotCommand,
                "email" => TextStyle::Email,
                "phone_number" => TextStyle::Phone,
                kind => TextStyle::PlatformNative(PlatformNativeData::new(
                    SERVER,
                    serde_json::json!({"type": kind}),
                )),
            };
            Some(TextSpan {
                range: start..end,
                styles: vec![style],
            })
        })
        .collect();
    RichText {
        text: text.to_owned(),
        spans,
    }
}

fn utf16_to_byte_offset(text: &str, target: usize) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let mut units = 0;
    for (offset, character) in text.char_indices() {
        units += character.len_utf16();
        if units == target {
            return Some(offset + character.len_utf8());
        }
        if units > target {
            return None;
        }
    }
    (units == target).then_some(text.len())
}

fn telegram_inbound_poll(value: &Value) -> Option<Poll> {
    let options = value
        .get("options")?
        .as_array()?
        .iter()
        .map(|option| PollOption {
            id: option
                .get("persistent_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            text: RichText::plain(
                option
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            voter_count: option.get("voter_count").and_then(Value::as_u64),
            platform_data: Some(PlatformNativeData::new(SERVER, option.clone())),
        })
        .collect();
    Some(Poll {
        id: value.get("id").and_then(Value::as_str).map(str::to_owned),
        question: RichText::plain(value.get("question")?.as_str()?),
        options,
        allows_multiple_answers: value
            .get("allows_multiple_answers")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        allows_revoting: value.get("allows_revoting").and_then(Value::as_bool),
        members_only: value.get("members_only").and_then(Value::as_bool),
        country_codes: value
            .get("country_codes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        anonymous: value.get("is_anonymous").and_then(Value::as_bool),
        kind: match value.get("type").and_then(Value::as_str) {
            Some("quiz") => PollType::Quiz,
            Some("regular") | None => PollType::Regular,
            Some(kind) => PollType::PlatformNative(kind.to_owned()),
        },
        open_for: value
            .get("open_period")
            .and_then(Value::as_u64)
            .map(Duration::from_secs),
        closes_at: value
            .get("close_date")
            .and_then(Value::as_i64)
            .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
        correct_option: value
            .get("correct_option_ids")
            .and_then(Value::as_array)
            .and_then(|options| options.first())
            .and_then(Value::as_u64)
            .and_then(|option| usize::try_from(option).ok()),
        correct_options: value
            .get("correct_option_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .filter_map(|option| usize::try_from(option).ok())
            .collect(),
        total_voter_count: value.get("total_voter_count").and_then(Value::as_u64),
        explanation: value
            .get("explanation")
            .and_then(Value::as_str)
            .map(RichText::plain),
        closed: value
            .get("is_closed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        platform_data: Some(PlatformNativeData::new(SERVER, value.clone())),
    })
}

fn telegram_inbound_checklist(value: &Value) -> Option<Checklist> {
    let tasks = value
        .get("tasks")?
        .as_array()?
        .iter()
        .map(telegram_checklist_task)
        .collect();
    Some(Checklist {
        id: None,
        title: RichText::plain(value.get("title")?.as_str()?),
        tasks,
        can_add_tasks: value
            .get("others_can_add_tasks")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        can_mark_tasks_done: value
            .get("others_can_mark_tasks_as_done")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        platform_data: Some(PlatformNativeData::new(SERVER, value.clone())),
    })
}

fn telegram_checklist_task(task: &Value) -> ChecklistTask {
    let completed_at = task
        .get("completion_date")
        .and_then(Value::as_i64)
        .filter(|timestamp| *timestamp > 0)
        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0));
    ChecklistTask {
        id: task
            .get("id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string()),
        text: RichText::plain(task.get("text").and_then(Value::as_str).unwrap_or_default()),
        completed: completed_at.is_some(),
        completed_by: task.get("completed_by_user").and_then(telegram_value_user),
        completed_at,
        platform_data: Some(PlatformNativeData::new(SERVER, task.clone())),
    }
}

fn telegram_value_user(value: &Value) -> Option<oxidebot::source::user::User> {
    serde_json::from_value::<crate::telegram::User>(value.clone())
        .ok()
        .map(parse_user)
}

fn telegram_reaction_type(reaction: crate::telegram::ReactionType) -> Reaction {
    match reaction.kind.as_str() {
        "emoji" => Reaction::UnicodeEmoji(reaction.emoji.unwrap_or_default()),
        "custom_emoji" => Reaction::CustomEmoji {
            id: reaction.custom_emoji_id.unwrap_or_default(),
            name: None,
        },
        "paid" => Reaction::Paid,
        kind => Reaction::PlatformNative {
            platform: SERVER.to_owned(),
            data: serde_json::json!({"type": kind}).to_string(),
        },
    }
}

fn telegram_reaction_value(value: &Value) -> Reaction {
    match value.get("type").and_then(Value::as_str) {
        Some("emoji") => Reaction::UnicodeEmoji(
            value
                .get("emoji")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        Some("custom_emoji") => Reaction::CustomEmoji {
            id: value
                .get("custom_emoji_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: None,
        },
        Some("paid") => Reaction::Paid,
        _ => Reaction::PlatformNative {
            platform: SERVER.to_owned(),
            data: value.to_string(),
        },
    }
}

fn parse_message_lifecycle(message: &TelegramMessage, raw: &Value) -> Vec<Event> {
    let conversation = telegram_message_conversation(message);
    let mut events = Vec::new();
    if [
        "group_chat_created",
        "supergroup_chat_created",
        "channel_chat_created",
    ]
    .iter()
    .any(|field| raw.get(*field).and_then(Value::as_bool).unwrap_or(false))
    {
        events.push(Event::LifecycleEvent(LifecycleEvent::ConversationCreated {
            conversation: conversation.clone(),
            profile: Some(Box::new(ConversationProfile {
                name: message.chat.title.clone(),
                username: message.chat.username.clone(),
                kind: Some(conversation.kind.clone()),
                platform_data: Some(PlatformNativeData::new(SERVER, raw.clone())),
                ..Default::default()
            })),
        }));
    }
    if raw.get("new_chat_title").is_some()
        || raw.get("new_chat_photo").is_some()
        || raw.get("delete_chat_photo").is_some()
    {
        let avatar = raw
            .get("new_chat_photo")
            .and_then(Value::as_array)
            .and_then(|photos| photos.last())
            .map(telegram_inbound_file);
        events.push(Event::LifecycleEvent(LifecycleEvent::ConversationUpdated {
            conversation: conversation.clone(),
            old_profile: None,
            new_profile: Box::new(ConversationProfile {
                name: raw
                    .get("new_chat_title")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                avatar,
                kind: Some(conversation.kind.clone()),
                platform_data: Some(PlatformNativeData::new(SERVER, raw.clone())),
                ..Default::default()
            }),
        }));
    }
    if let Some(pinned) = raw.get("pinned_message") {
        if let Some(message_id) = pinned.get("message_id").and_then(Value::as_i64) {
            events.push(Event::LifecycleEvent(LifecycleEvent::MessagePinned(
                PinnedMessage {
                    message: MessageRef::new(message_id.to_string())
                        .in_conversation(conversation.clone()),
                    pinned_by: message.from.clone().map(parse_user),
                    pinned_at: DateTime::from_timestamp(message.date, 0),
                    platform_data: Some(PlatformNativeData::new(SERVER, pinned.clone())),
                },
            )));
        }
    }
    for (field, state) in [
        ("forum_topic_created", ThreadState::Open),
        ("forum_topic_edited", ThreadState::Open),
        ("forum_topic_closed", ThreadState::Closed),
        ("forum_topic_reopened", ThreadState::Open),
        ("general_forum_topic_hidden", ThreadState::Hidden),
        ("general_forum_topic_unhidden", ThreadState::Open),
    ] {
        if let Some(topic) = raw.get(field) {
            let thread = Thread {
                conversation: conversation.clone(),
                title: topic.get("name").and_then(Value::as_str).map(str::to_owned),
                creator: message.from.clone().map(parse_user),
                state,
                created_at: DateTime::from_timestamp(message.date, 0),
                auto_close_at: None,
                platform_data: Some(PlatformNativeData::new(SERVER, topic.clone())),
            };
            let lifecycle = match field {
                "forum_topic_created" => LifecycleEvent::ThreadCreated(thread),
                "forum_topic_closed" | "general_forum_topic_hidden" => {
                    LifecycleEvent::ThreadClosed(thread)
                }
                _ => LifecycleEvent::ThreadUpdated(thread),
            };
            events.push(Event::LifecycleEvent(lifecycle));
        }
    }
    if let Some(checklist) = raw.get("checklist").and_then(telegram_inbound_checklist) {
        events.push(Event::LifecycleEvent(LifecycleEvent::ChecklistUpdated(
            checklist,
        )));
    }
    if let Some(change) = raw
        .get("checklist_tasks_done")
        .or_else(|| raw.get("checklist_tasks_added"))
    {
        let ids = |name: &str| {
            change
                .get(name)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
        };
        events.push(Event::LifecycleEvent(LifecycleEvent::ChecklistChanged(
            ChecklistChange {
                message: change
                    .get("checklist_message")
                    .and_then(|message| message.get("message_id"))
                    .and_then(Value::as_i64)
                    .map(|id| {
                        MessageRef::new(id.to_string()).in_conversation(conversation.clone())
                    }),
                added_tasks: change
                    .get("tasks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(telegram_checklist_task)
                    .collect(),
                completed_task_ids: ids("marked_as_done_task_ids"),
                reopened_task_ids: ids("marked_as_not_done_task_ids"),
                actor: message.from.clone().map(parse_user),
                platform_data: Some(PlatformNativeData::new(SERVER, change.clone())),
            },
        )));
    }
    if let Some(payment) = raw.get("successful_payment") {
        events.push(Event::LifecycleEvent(LifecycleEvent::PaymentUpdated(
            telegram_successful_payment(message, payment),
        )));
    }
    for (field, state) in [
        ("video_chat_scheduled", CallState::Scheduled),
        ("video_chat_started", CallState::Active),
        ("video_chat_ended", CallState::Ended),
        (
            "video_chat_participants_invited",
            CallState::PlatformNative("participants_updated".to_owned()),
        ),
    ] {
        if let Some(call) = raw.get(field) {
            events.push(Event::LifecycleEvent(LifecycleEvent::CallUpdated(
                CallSession {
                    id: Some(format!("{}:{}", message.chat.id, message.message_id)),
                    conversation: conversation.clone(),
                    kind: CallKind::Video,
                    state,
                    title: None,
                    scheduled_for: call
                        .get("start_date")
                        .and_then(Value::as_i64)
                        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
                    started_at: (field == "video_chat_started")
                        .then(|| DateTime::from_timestamp(message.date, 0))
                        .flatten(),
                    ended_at: (field == "video_chat_ended")
                        .then(|| DateTime::from_timestamp(message.date, 0))
                        .flatten(),
                    duration: call
                        .get("duration")
                        .and_then(Value::as_u64)
                        .map(Duration::from_secs),
                    participants: call
                        .get("users")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(telegram_value_user)
                        .collect(),
                    platform_data: Some(PlatformNativeData::new(SERVER, call.clone())),
                },
            )));
        }
    }
    if let (Some(web_app), Some(user)) = (
        raw.get("web_app_data"),
        message.from.clone().map(parse_user),
    ) {
        events.push(Event::LifecycleEvent(LifecycleEvent::MiniApp(
            MiniAppEvent {
                query_id: message
                    .extra
                    .get("web_app_query_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                user,
                conversation: Some(conversation),
                data: web_app
                    .get("data")
                    .cloned()
                    .unwrap_or_else(|| web_app.clone()),
                verification: VerificationState::Unverified,
                platform_data: Some(PlatformNativeData::new(SERVER, web_app.clone())),
            },
        )));
    }
    events
}

fn telegram_successful_payment(message: &TelegramMessage, payment: &Value) -> Payment {
    Payment {
        id: payment
            .get("telegram_payment_charge_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        invoice_id: payment
            .get("invoice_payload")
            .and_then(Value::as_str)
            .map(str::to_owned),
        payer: message.from.clone().map(parse_user),
        conversation: Some(telegram_message_conversation(message)),
        total: Money::new(
            payment
                .get("currency")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            payment
                .get("total_amount")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        ),
        status: PaymentStatus::Paid,
        occurred_at: DateTime::from_timestamp(message.date, 0),
        platform_data: Some(PlatformNativeData::new(SERVER, payment.clone())),
    }
}

fn parse_deleted_messages(value: Value) -> Vec<Event> {
    let conversation = telegram_raw_conversation(&value);
    let messages = value
        .get("message_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .map(|id| {
            let mut message = MessageRef::new(id.to_string());
            message.conversation = conversation.clone();
            message
        })
        .collect();
    vec![Event::LifecycleEvent(LifecycleEvent::MessagesDeleted {
        conversation,
        messages,
        platform_data: Some(PlatformNativeData::new(SERVER, value)),
    })]
}

fn parse_message_reaction_count(value: Value) -> Vec<Event> {
    let conversation = telegram_raw_conversation(&value);
    let message = MessageRef {
        id: value
            .get("message_id")
            .and_then(Value::as_i64)
            .unwrap_or_default()
            .to_string(),
        conversation,
        platform_data: None,
    };
    let current = value
        .get("reactions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|count| ReactionSummary {
            reaction: count
                .get("type")
                .map(telegram_reaction_value)
                .unwrap_or_else(|| Reaction::UnicodeEmoji(String::new())),
            count: count
                .get("total_count")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            reacted_by_bot: false,
            recent_users: Vec::new(),
            platform_data: Some(PlatformNativeData::new(SERVER, count.clone())),
        })
        .collect();
    vec![Event::LifecycleEvent(LifecycleEvent::ReactionsChanged {
        message,
        actor: None,
        change: ReactionChange {
            added: Vec::new(),
            removed: Vec::new(),
            current,
        },
    })]
}

fn parse_poll_update(value: Value) -> Vec<Event> {
    telegram_inbound_poll(&value)
        .map(|poll| Event::LifecycleEvent(LifecycleEvent::PollUpdated(poll)))
        .into_iter()
        .collect()
}

fn parse_poll_answer(value: Value) -> Vec<Event> {
    let option_ids = value
        .get("option_persistent_ids")
        .and_then(Value::as_array)
        .filter(|ids| !ids.is_empty())
        .or_else(|| value.get("option_ids").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|id| {
            id.as_str()
                .map(str::to_owned)
                .or_else(|| id.as_i64().map(|id| id.to_string()))
        })
        .collect();
    vec![Event::LifecycleEvent(LifecycleEvent::PollVoteChanged {
        poll_id: value
            .get("poll_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        user: value.get("user").and_then(telegram_value_user),
        option_ids,
        platform_data: Some(PlatformNativeData::new(SERVER, value)),
    })]
}

fn parse_inline_query(value: Value) -> Vec<Event> {
    let Some(user) = value.get("from").and_then(telegram_value_user) else {
        return Vec::new();
    };
    vec![Event::LifecycleEvent(LifecycleEvent::SuggestionRequested(
        SuggestionRequest {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            query: value
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            field_id: None,
            user,
            conversation: None,
            limit: Some(50),
            cursor: value
                .get("offset")
                .and_then(Value::as_str)
                .filter(|offset| !offset.is_empty())
                .map(str::to_owned),
            platform_data: Some(PlatformNativeData::new(SERVER, value)),
        },
    ))]
}

fn parse_chosen_inline_result(value: Value) -> Vec<Event> {
    let Some(user) = value.get("from").and_then(telegram_value_user) else {
        return Vec::new();
    };
    vec![Event::LifecycleEvent(LifecycleEvent::SuggestionSelected(
        SuggestionSelection {
            result_id: value
                .get("result_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            query: value
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_owned),
            user,
            conversation: None,
            message_id: value
                .get("inline_message_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            platform_data: Some(PlatformNativeData::new(SERVER, value)),
        },
    ))]
}

fn parse_shipping_query(value: Value) -> Vec<Event> {
    let Some(user) = value.get("from").and_then(telegram_value_user) else {
        return Vec::new();
    };
    vec![Event::LifecycleEvent(LifecycleEvent::ShippingRequested(
        ShippingRequest {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            user,
            invoice_payload: value
                .get("invoice_payload")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            address: value
                .get("shipping_address")
                .cloned()
                .unwrap_or(Value::Null),
            platform_data: Some(PlatformNativeData::new(SERVER, value)),
        },
    ))]
}

fn telegram_customer_details(order: Option<&Value>) -> CustomerDetails {
    CustomerDetails {
        name: order
            .and_then(|order| order.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        email: order
            .and_then(|order| order.get("email"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        phone: order
            .and_then(|order| order.get("phone_number"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        shipping_address: order
            .and_then(|order| order.get("shipping_address"))
            .cloned(),
    }
}

fn parse_checkout_query(value: Value) -> Vec<Event> {
    let Some(user) = value.get("from").and_then(telegram_value_user) else {
        return Vec::new();
    };
    vec![Event::LifecycleEvent(LifecycleEvent::CheckoutRequested(
        CheckoutRequest {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            user,
            invoice_payload: value
                .get("invoice_payload")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            total: Money::new(
                value
                    .get("currency")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                value
                    .get("total_amount")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
            ),
            customer_details: telegram_customer_details(value.get("order_info")),
            platform_data: Some(PlatformNativeData::new(SERVER, value)),
        },
    ))]
}

fn parse_paid_media_purchase(value: Value) -> Vec<Event> {
    let payer = value.get("from").and_then(telegram_value_user);
    vec![Event::LifecycleEvent(LifecycleEvent::PaymentUpdated(
        Payment {
            id: value
                .get("paid_media_payload")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            invoice_id: value
                .get("paid_media_payload")
                .and_then(Value::as_str)
                .map(str::to_owned),
            payer,
            conversation: None,
            total: Money::new("XTR", 0),
            status: PaymentStatus::Paid,
            occurred_at: None,
            platform_data: Some(PlatformNativeData::new(SERVER, value)),
        },
    ))]
}

fn parse_subscription(value: Value) -> Vec<Event> {
    let Some(user) = value.get("user").and_then(telegram_value_user) else {
        return Vec::new();
    };
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    vec![Event::LifecycleEvent(LifecycleEvent::SubscriptionUpdated(
        Subscription {
            id: value
                .get("invoice_payload")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            user,
            price: Money::new("XTR", 0),
            status: match state {
                "active" => PaymentStatus::Paid,
                "canceled" => PaymentStatus::Canceled,
                "failed" => PaymentStatus::Failed,
                state => PaymentStatus::PlatformNative(state.to_owned()),
            },
            period_start: None,
            period_end: None,
            auto_renewing: match state {
                "active" => Some(true),
                "canceled" => Some(false),
                _ => None,
            },
            platform_data: Some(PlatformNativeData::new(SERVER, value)),
        },
    ))]
}

fn parse_callback_query(query: CallbackQuery, data: Value) -> Event {
    let group = query.message.as_ref().and_then(|message| {
        (message.chat.kind != "private").then(|| parse_group(message.chat.clone()))
    });
    let mut response_context = serde_json::Map::new();
    response_context.insert("user_id".to_owned(), query.from.id.into());
    response_context.insert(
        "chat_instance".to_owned(),
        query.chat_instance.clone().into(),
    );
    if let Some(message) = &query.message {
        response_context.insert("chat_id".to_owned(), message.chat.id.into());
        response_context.insert("chat_type".to_owned(), message.chat.kind.clone().into());
        response_context.insert("message_id".to_owned(), message.message_id.into());
        for name in [
            "business_connection_id",
            "message_thread_id",
            "direct_messages_topic_id",
        ] {
            if let Some(value) = message.extra.get(name) {
                response_context.insert(name.to_owned(), value.clone());
            }
        }
    }
    if let Some(inline_message_id) = &query.inline_message_id {
        response_context.insert(
            "inline_message_id".to_owned(),
            inline_message_id.clone().into(),
        );
    }
    let followups_supported = response_context.contains_key("chat_id");
    let message = query.message.map(parse_message);
    let action_id = query.data.clone().or_else(|| query.game_short_name.clone());
    let values = action_id.clone().into_iter().collect();
    let fields = action_id
        .as_ref()
        .map(|action_id| {
            BTreeMap::from([(action_id.clone(), vec![FormValue::Text(action_id.clone())])])
        })
        .unwrap_or_default();
    let response_id = query.id.clone();
    Event::InteractionEvent(InteractionEvent {
        id: query.id,
        kind: InteractionKind::Button,
        action_id,
        values,
        user: parse_user(query.from),
        group,
        message,
        context_id: query
            .inline_message_id
            .or_else(|| (!query.chat_instance.is_empty()).then_some(query.chat_instance)),
        response: Some(InteractionResponseHandle {
            id: response_id,
            deadline: Some(Utc::now() + chrono::TimeDelta::seconds(10)),
            ack_required: true,
            followups_supported,
            platform_data: Some(oxidebot::PlatformNativeData::new(
                SERVER,
                Value::Object(response_context),
            )),
        }),
        fields,
        command: None,
        locale: None,
        permissions: BTreeSet::new(),
        data,
    })
}

fn parse_chat_member_update(update: ChatMemberUpdated) -> Vec<Event> {
    let mut events = Vec::new();
    let old_portable_member = telegram_portable_member(&update.old_chat_member);
    let new_portable_member = telegram_portable_member(&update.new_chat_member);
    let portable_conversation = telegram_chat_conversation(&update.chat);
    let group = parse_group(update.chat.clone());
    let actor = parse_user(update.from.clone());
    let target = parse_user(update.new_chat_member.user.clone());
    let was_present = update.old_chat_member.is_present();
    let is_present = update.new_chat_member.is_present();

    if !was_present && is_present {
        let reason = if update.via_join_request.unwrap_or(false) {
            GroupMemberIncreseReason::Approve {
                operator: Some(actor.clone()),
            }
        } else if update.from.id != update.new_chat_member.user.id {
            GroupMemberIncreseReason::Invite {
                inviter: Some(actor.clone()),
                operator: Some(actor.clone()),
            }
        } else {
            GroupMemberIncreseReason::Unknown
        };
        events.push(Event::NoticeEvent(NoticeEvent::GroupMemberIncreseEvent(
            GroupMemberIncreseEvent {
                group: group.clone(),
                user: target.clone(),
                reason,
            },
        )));
    } else if was_present && !is_present {
        let reason = if update.new_chat_member.status == "kicked" {
            GroupMemberDecreaseReason::Kick {
                operator: Some(actor.clone()),
            }
        } else if update.from.id == update.new_chat_member.user.id {
            GroupMemberDecreaseReason::Leave
        } else {
            GroupMemberDecreaseReason::Kick {
                operator: Some(actor.clone()),
            }
        };
        events.push(Event::NoticeEvent(NoticeEvent::GroupMemberDecreaseEvent(
            GroupMemberDecreaseEvent {
                group: group.clone(),
                user: target.clone(),
                reason,
            },
        )));
    }

    let was_admin = update.old_chat_member.is_admin();
    let is_admin = update.new_chat_member.is_admin();
    if was_admin != is_admin {
        events.push(Event::NoticeEvent(NoticeEvent::GroupAdminChangeEvent(
            GroupAdminChangeEvent {
                group: group.clone(),
                user: target.clone(),
                r#type: if is_admin {
                    GroupAdminChangeType::Set
                } else {
                    GroupAdminChangeType::Unset
                },
            },
        )));
    }

    let was_muted = update.old_chat_member.is_muted();
    let is_muted = update.new_chat_member.is_muted();
    if was_muted != is_muted {
        let mute_type = if is_muted {
            MuteType::Mute {
                duration: remaining_duration(update.new_chat_member.until_date),
            }
        } else {
            MuteType::UnMute
        };
        events.push(Event::NoticeEvent(NoticeEvent::GroupMemberMuteChangeEvent(
            GroupMemberMuteChangeEvent {
                group: group.clone(),
                user: target.clone(),
                operator: Some(actor.clone()),
                r#type: mute_type,
            },
        )));
    }

    if update.old_chat_member.custom_title != update.new_chat_member.custom_title {
        events.push(Event::NoticeEvent(
            NoticeEvent::GroupMemberAliasChangeEvent(GroupMemberAliasChangeEvent {
                group,
                user: target,
                operator: Some(actor),
                old_alias: update.old_chat_member.custom_title,
                new_alias: update.new_chat_member.custom_title,
            }),
        ));
    }

    events.push(Event::LifecycleEvent(LifecycleEvent::MemberUpdated {
        conversation: portable_conversation,
        old_member: Some(Box::new(old_portable_member)),
        new_member: Box::new(new_portable_member),
    }));

    events
}

fn telegram_portable_member(member: &crate::telegram::ChatMember) -> ConversationMember {
    let mut permissions = PermissionSet::default();
    let mut permission = |allowed: Option<bool>, value: ConversationPermission| match allowed {
        Some(true) => {
            permissions.allow.insert(value);
        }
        Some(false) => {
            permissions.deny.insert(value);
        }
        None => {}
    };
    permission(
        member.can_send_messages,
        ConversationPermission::SendMessages,
    );
    permission(
        member
            .extra
            .get("can_react_to_messages")
            .and_then(Value::as_bool),
        ConversationPermission::SendReactions,
    );
    permission(
        member
            .extra
            .get("can_delete_messages")
            .and_then(Value::as_bool),
        ConversationPermission::ManageMessages,
    );
    permission(
        member
            .extra
            .get("can_manage_topics")
            .and_then(Value::as_bool),
        ConversationPermission::ManageThreads,
    );
    permission(
        member.extra.get("can_manage_tags").and_then(Value::as_bool),
        ConversationPermission::ManageTags,
    );
    let roles = matches!(member.status.as_str(), "creator" | "administrator")
        .then(|| RoleRef {
            id: member.status.clone(),
            name: member.custom_title.clone(),
            permissions: permissions.clone(),
        })
        .into_iter()
        .collect();
    ConversationMember {
        user: parse_user(member.user.clone()),
        roles,
        permissions,
        joined_at: None,
        nickname: member.custom_title.clone(),
        tag: member.tag.clone(),
        platform_data: serde_json::to_value(member)
            .ok()
            .map(|value| PlatformNativeData::new(SERVER, value)),
    }
}

fn remaining_duration(until_date: Option<i64>) -> Option<Duration> {
    let until_date = until_date.filter(|date| *date > 0)?;
    let seconds = until_date.saturating_sub(Utc::now().timestamp()).max(0);
    Some(Duration::from_secs(seconds as u64))
}

#[cfg(test)]
mod tests {
    use oxidebot::event::notice::GroupMemberDecreaseReason;

    use super::*;

    fn update(value: Value) -> Update {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn bot_api_10_2_subscription_maps_to_lifecycle_event() {
        let update = update(serde_json::json!({
            "update_id": 1,
            "subscription": {
                "user": {"id": 42, "is_bot": false, "first_name": "User"},
                "invoice_payload": "monthly",
                "state": "active"
            }
        }));
        let events = parse_update(&update);
        let Event::LifecycleEvent(LifecycleEvent::SubscriptionUpdated(event)) = &events[0] else {
            panic!("expected subscription lifecycle event")
        };
        assert_eq!(event.user.id, "42");
        assert_eq!(event.status, PaymentStatus::Paid);
    }

    #[test]
    fn channel_post_uses_sender_chat_instead_of_being_dropped() {
        let update = update(serde_json::json!({
            "update_id": 2,
            "channel_post": {
                "message_id": 7,
                "date": 1,
                "chat": {"id": -100, "type": "channel", "title": "News"},
                "sender_chat": {"id": -100, "type": "channel", "title": "News"},
                "text": "hello"
            }
        }));
        let events = parse_update(&update);
        let Event::MessageEvent(event) = &events[0] else {
            panic!("expected message event")
        };
        assert_eq!(event.sender.id, "-100");
        assert_eq!(event.message.get_raw_text(), "hello");
    }

    #[test]
    fn kicked_member_is_target_not_the_operator() {
        let update = update(serde_json::json!({
            "update_id": 3,
            "chat_member": {
                "chat": {"id": -100, "type": "supergroup", "title": "Group"},
                "from": {"id": 1, "is_bot": false, "first_name": "Admin"},
                "date": 1,
                "old_chat_member": {
                    "status": "member",
                    "user": {"id": 2, "is_bot": false, "first_name": "Member"}
                },
                "new_chat_member": {
                    "status": "kicked",
                    "user": {"id": 2, "is_bot": false, "first_name": "Member"}
                }
            }
        }));
        let events = parse_update(&update);
        let Event::NoticeEvent(NoticeEvent::GroupMemberDecreaseEvent(event)) = &events[0] else {
            panic!("expected decrease event")
        };
        assert_eq!(event.user.id, "2");
        let GroupMemberDecreaseReason::Kick { operator } = &event.reason else {
            panic!("expected kick reason")
        };
        assert_eq!(operator.as_ref().unwrap().id, "1");
    }

    #[test]
    fn expired_mute_duration_never_wraps_to_u64_max() {
        assert_eq!(remaining_duration(Some(1)), Some(Duration::ZERO));
    }

    #[test]
    fn join_request_id_contains_chat_and_user() {
        let update = update(serde_json::json!({
            "update_id": 4,
            "chat_join_request": {
                "chat": {"id": -100, "type": "supergroup", "title": "Group"},
                "from": {"id": 2, "is_bot": false, "first_name": "User"},
                "user_chat_id": 2,
                "date": 1
            }
        }));
        let events = parse_update(&update);
        let Event::RequestEvent(RequestEvent::GroupAddEvent(event)) = &events[0] else {
            panic!("expected join request")
        };
        assert_eq!(event.id, "-100:2");
    }

    #[test]
    fn administrator_permission_change_maps_to_member_lifecycle() {
        let update = update(serde_json::json!({
            "update_id": 6,
            "chat_member": {
                "chat": {"id": -100, "type": "supergroup", "title": "Group"},
                "from": {"id": 1, "is_bot": false, "first_name": "Admin"},
                "date": 1,
                "old_chat_member": {
                    "status": "administrator",
                    "user": {"id": 2, "is_bot": false, "first_name": "Member"},
                    "can_delete_messages": false
                },
                "new_chat_member": {
                    "status": "administrator",
                    "user": {"id": 2, "is_bot": false, "first_name": "Member"},
                    "can_delete_messages": true
                }
            }
        }));

        let events = parse_update(&update);
        let Event::LifecycleEvent(LifecycleEvent::MemberUpdated { new_member, .. }) =
            events.last().unwrap()
        else {
            panic!("expected a portable member update")
        };
        assert!(new_member
            .permissions
            .allow
            .contains(&ConversationPermission::ManageMessages));
    }

    #[test]
    fn administrator_title_change_maps_to_alias_event() {
        let update = update(serde_json::json!({
            "update_id": 7,
            "chat_member": {
                "chat": {"id": -100, "type": "supergroup", "title": "Group"},
                "from": {"id": 1, "is_bot": false, "first_name": "Admin"},
                "date": 1,
                "old_chat_member": {
                    "status": "administrator",
                    "user": {"id": 2, "is_bot": false, "first_name": "Member"},
                    "custom_title": "old"
                },
                "new_chat_member": {
                    "status": "administrator",
                    "user": {"id": 2, "is_bot": false, "first_name": "Member"},
                    "custom_title": "new"
                }
            }
        }));

        let events = parse_update(&update);
        let Event::NoticeEvent(NoticeEvent::GroupMemberAliasChangeEvent(event)) = &events[0] else {
            panic!("expected an alias change event")
        };
        assert_eq!(event.old_alias.as_deref(), Some("old"));
        assert_eq!(event.new_alias.as_deref(), Some("new"));
    }

    #[test]
    fn callback_query_maps_to_typed_interaction_event() {
        let update = update(serde_json::json!({
            "update_id": 8,
            "callback_query": {
                "id": "callback-1",
                "from": {"id": 42, "is_bot": false, "first_name": "User"},
                "message": {
                    "message_id": 7,
                    "date": 1,
                    "chat": {"id": -100, "type": "supergroup", "title": "Group"},
                    "text": "Choose"
                },
                "chat_instance": "instance",
                "data": "confirm"
            }
        }));

        let events = parse_update(&update);
        let Event::InteractionEvent(event) = &events[0] else {
            panic!("expected an interaction event")
        };
        assert_eq!(event.id, "callback-1");
        assert_eq!(event.action_id.as_deref(), Some("confirm"));
        assert_eq!(event.values, ["confirm"]);
        assert_eq!(event.user.id, "42");
        assert_eq!(event.group.as_ref().unwrap().id, "-100");
        assert_eq!(event.message.as_ref().unwrap().id, "-100_7");
        assert_eq!(event.data["chat_instance"], "instance");
        let context = event
            .response
            .as_ref()
            .and_then(|response| response.platform_data.as_ref())
            .expect("callback response context");
        assert_eq!(context.data["chat_id"], -100);
        assert_eq!(context.data["message_id"], 7);
        assert_eq!(context.data["user_id"], 42);
    }

    #[test]
    fn reply_keyboard_results_map_to_typed_interactions() {
        let update = update(serde_json::json!({
            "update_id": 9,
            "message": {
                "message_id": 8,
                "date": 1,
                "from": {"id": 42, "is_bot": false, "first_name": "User"},
                "chat": {"id": 42, "type": "private"},
                "users_shared": {
                    "request_id": 7,
                    "users": [{"user_id": 100}, {"user_id": 101}]
                },
                "web_app_data": {
                    "button_text": "Open form",
                    "data": "{\"choice\":1}"
                }
            }
        }));

        let events = parse_update(&update);
        let interactions = events
            .iter()
            .filter_map(|event| match event {
                Event::InteractionEvent(event) => Some(event),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(interactions.len(), 2);
        assert_eq!(interactions[0].kind, InteractionKind::Select);
        assert_eq!(interactions[0].action_id.as_deref(), Some("7"));
        assert_eq!(interactions[0].values, ["100", "101"]);
        assert_eq!(interactions[1].kind, InteractionKind::Form);
        assert_eq!(interactions[1].action_id.as_deref(), Some("Open form"));
        assert_eq!(interactions[1].values, [r#"{"choice":1}"#]);
        assert_eq!(interactions[1].context_id.as_deref(), Some("42_8"));
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::MessageEvent(_))));
    }

    #[test]
    fn message_update_exposes_rich_v2_envelope_and_command() {
        let update = update(serde_json::json!({
            "update_id": 10,
            "message": {
                "message_id": 9,
                "date": 100,
                "edit_date": 101,
                "message_thread_id": 5,
                "from": {
                    "id": 42,
                    "is_bot": false,
                    "first_name": "User",
                    "language_code": "zh-hans"
                },
                "chat": {"id": -100, "type": "supergroup", "title": "Group"},
                "text": "/go 😀bold",
                "entities": [
                    {"type": "bot_command", "offset": 0, "length": 3},
                    {"type": "bold", "offset": 4, "length": 6}
                ]
            }
        }));
        let events = parse_update(&update);
        let command = events
            .iter()
            .find_map(|event| match event {
                Event::InteractionEvent(event) if event.kind == InteractionKind::Command => {
                    Some(event)
                }
                _ => None,
            })
            .expect("command interaction");
        assert_eq!(command.action_id.as_deref(), Some("go"));
        assert_eq!(
            command.command.as_ref().unwrap().locale.as_deref(),
            Some("zh-hans")
        );

        let envelope = events
            .iter()
            .find_map(|event| match event {
                Event::LifecycleEvent(LifecycleEvent::MessageCreated(event)) => Some(event),
                _ => None,
            })
            .expect("message envelope");
        assert_eq!(envelope.reference.id, "9");
        let conversation = envelope.conversation.as_ref().unwrap();
        assert_eq!(conversation.id, "5");
        assert_eq!(conversation.parent.as_ref().unwrap().id, "-100");
        let MessageContent::RichText(text) = &envelope.content[0] else {
            panic!("expected rich text")
        };
        assert_eq!(&text.text[text.spans[1].range.clone()], "😀bold");
    }

    #[test]
    fn anonymous_reaction_counts_map_to_portable_summaries() {
        let update = update(serde_json::json!({
            "update_id": 11,
            "message_reaction_count": {
                "chat": {"id": -100, "type": "supergroup"},
                "message_id": 7,
                "date": 1,
                "reactions": [{
                    "type": {"type": "emoji", "emoji": "👍"},
                    "total_count": 3
                }]
            }
        }));
        let events = parse_update(&update);
        let Event::LifecycleEvent(LifecycleEvent::ReactionsChanged { change, .. }) = &events[0]
        else {
            panic!("expected reactions lifecycle event")
        };
        assert_eq!(change.current[0].count, 3);
        assert_eq!(
            change.current[0].reaction,
            Reaction::UnicodeEmoji("👍".to_owned())
        );
    }

    #[test]
    fn commerce_and_inline_updates_are_typed() {
        let inline = update(serde_json::json!({
            "update_id": 12,
            "inline_query": {
                "id": "query-1",
                "from": {"id": 42, "is_bot": false, "first_name": "User"},
                "query": "oxide",
                "offset": "next"
            }
        }));
        assert!(matches!(
            &parse_update(&inline)[0],
            Event::LifecycleEvent(LifecycleEvent::SuggestionRequested(request))
                if request.query == "oxide" && request.cursor.as_deref() == Some("next")
        ));

        let checkout = update(serde_json::json!({
            "update_id": 13,
            "pre_checkout_query": {
                "id": "checkout-1",
                "from": {"id": 42, "is_bot": false, "first_name": "User"},
                "currency": "XTR",
                "total_amount": 50,
                "invoice_payload": "invoice"
            }
        }));
        assert!(matches!(
            &parse_update(&checkout)[0],
            Event::LifecycleEvent(LifecycleEvent::CheckoutRequested(request))
                if request.total.amount == 50 && request.total.currency == "XTR"
        ));
    }

    #[test]
    fn deleted_business_messages_keep_conversation_and_ids() {
        let update = update(serde_json::json!({
            "update_id": 14,
            "deleted_business_messages": {
                "business_connection_id": "business",
                "chat": {"id": 42, "type": "private"},
                "message_ids": [7, 8]
            }
        }));
        let events = parse_update(&update);
        let Event::LifecycleEvent(LifecycleEvent::MessagesDeleted {
            conversation,
            messages,
            ..
        }) = &events[0]
        else {
            panic!("expected deleted messages lifecycle event")
        };
        assert_eq!(conversation.as_ref().unwrap().id, "42");
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            ["7", "8"]
        );
    }
}
