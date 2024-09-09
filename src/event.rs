use std::time::Duration;

use chrono::DateTime;
use oxidebot::{
    event::{
        any::{AnyEvent, AnyEventDataTrait},
        notice::{
            GroupAdminChangeEvent, GroupAdminChangeType, GroupMemberDecreaseEvent,
            GroupMemberMuteChangeEvent, MessageEditedEvent, MessageReactionsEvent,
        },
        request::GroupAddEvent,
        Event, EventObject, MessageEvent,
    },
    source::message::Message,
    EventTrait,
};
use telegram_bot_api_rs::{
    available_types::{
        BusinessConnection, BusinessMessagesDeleted, ChatBoostRemoved, ChatBoostUpdated,
        ChatMember, MessageReactionCountUpdated,
    },
    getting_updates::types::UpdateData,
    inline_mode::types::{ChosenInlineResult, InlineQuery},
    payments::types::{PreCheckoutQuery, ShippingQuery},
};

use crate::{
    segment::{self, parse_message, parse_reaction},
    utils::{parse_group, parse_user},
    SERVER,
};

pub struct UpdateEvent(pub UpdateData);

impl UpdateEvent {
    pub fn new(update: UpdateData) -> EventObject {
        Box::new(UpdateEvent(update))
    }
}

impl EventTrait for UpdateEvent {
    fn get_events(&self) -> Vec<Event> {
        parse_update(self.0.clone())
    }

    fn server(&self) -> &'static str {
        SERVER
    }

    fn clone_box(&self) -> EventObject {
        Box::new(UpdateEvent(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct BussinessConnectionEventWrapper(pub BusinessConnection);

impl AnyEventDataTrait for BussinessConnectionEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(BussinessConnectionEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct ChatBoostEventWrapper(pub ChatBoostUpdated);

impl AnyEventDataTrait for ChatBoostEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(ChatBoostEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct RemovedChatBoostEventWrapper(pub ChatBoostRemoved);
impl AnyEventDataTrait for RemovedChatBoostEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(RemovedChatBoostEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct MessageReactionCountEventWrapper(pub MessageReactionCountUpdated);
impl AnyEventDataTrait for MessageReactionCountEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(MessageReactionCountEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct InlineQueryEventWrapper(pub InlineQuery);
impl AnyEventDataTrait for InlineQueryEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(InlineQueryEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct ChosenInlineResultEventWrapper(pub ChosenInlineResult);
impl AnyEventDataTrait for ChosenInlineResultEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(ChosenInlineResultEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct PreCheckoutQueryEventWrapper(pub PreCheckoutQuery);
impl AnyEventDataTrait for PreCheckoutQueryEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(PreCheckoutQueryEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct CallbackQueryEventWrapper(pub telegram_bot_api_rs::available_types::CallbackQuery);
impl AnyEventDataTrait for CallbackQueryEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(CallbackQueryEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct ShippingQueryEventWrapper(pub ShippingQuery);
impl AnyEventDataTrait for ShippingQueryEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(ShippingQueryEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct PollEventWrapper(pub telegram_bot_api_rs::available_types::Poll);
impl AnyEventDataTrait for PollEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(PollEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct PollAnswerEventWrapper(pub telegram_bot_api_rs::available_types::PollAnswer);

impl AnyEventDataTrait for PollAnswerEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(PollAnswerEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct DeletedBusinessMessagesEventWrapper(pub BusinessMessagesDeleted);
impl AnyEventDataTrait for DeletedBusinessMessagesEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(DeletedBusinessMessagesEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub struct EditedBusinessMessageEventWrapper(pub telegram_bot_api_rs::available_types::Message);

impl AnyEventDataTrait for EditedBusinessMessageEventWrapper {
    fn clone_box(&self) -> Box<dyn AnyEventDataTrait> {
        Box::new(EditedBusinessMessageEventWrapper(self.0.clone()))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn parse_update(update: UpdateData) -> Vec<Event> {
    let mut results = Vec::new();
    match update {
        UpdateData::Message { message } => {
            if let Some(user) = message.from.clone().and_then(|f| Some(parse_user(f))) {
                results.push(Event::MessageEvent(MessageEvent {
                    id: format!("{}_{}", message.chat.id, message.message_id),
                    time: DateTime::from_timestamp(message.date, 0),
                    sender: user,
                    group: {
                        if message.chat.r#type == "private" {
                            None
                        } else {
                            Some(parse_group(message.chat.clone()))
                        }
                    },
                    message: segment::parse_message(message.clone()),
                }));
            }
            if let Some(new_chatmembers) = message.new_chat_members {
                for new_chatmember in new_chatmembers {
                    results.push(Event::NoticeEvent(
                        oxidebot::event::NoticeEvent::GroupMemberIncreseEvent(
                            oxidebot::event::notice::GroupMemberIncreseEvent {
                                group: parse_group(message.chat.clone()),
                                user: parse_user(new_chatmember),
                                reason: oxidebot::event::notice::GroupMemberIncreseReason::Unknown,
                            },
                        ),
                    ));
                }
            }
            if let Some(left_member) = message.left_chat_member {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberDecreaseEvent(
                        GroupMemberDecreaseEvent {
                            group: parse_group(message.chat.clone()),
                            user: parse_user(left_member),
                            reason: oxidebot::event::notice::GroupMemberDecreaseReason::Unknown,
                        },
                    ),
                ))
            }
        }
        UpdateData::EditedMessage {
            edited_message: message,
        } => {
            if let Some(user) = message.from.clone().and_then(|f| Some(parse_user(f))) {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::MessageEditedEvent(MessageEditedEvent {
                        user: user,
                        group: {
                            if message.chat.r#type == "private" {
                                None
                            } else {
                                Some(parse_group(message.chat.clone()))
                            }
                        },
                        new_message: Some(parse_message(message.clone())),
                        operator: {
                            if let Some(user) = message.from.clone() {
                                Some(parse_user(user))
                            } else {
                                None
                            }
                        },
                        old_message: None,
                    }),
                ));
            }
        }
        UpdateData::ChannelPost { channel_post } => {
            if let Some(user) = channel_post.from.clone().and_then(|f| Some(parse_user(f))) {
                results.push(Event::MessageEvent(MessageEvent {
                    id: format!("{}_{}", channel_post.chat.id, channel_post.message_id),
                    time: DateTime::from_timestamp(channel_post.date, 0),
                    sender: user,
                    group: {
                        if channel_post.chat.r#type == "private" {
                            None
                        } else {
                            Some(parse_group(channel_post.chat.clone()))
                        }
                    },
                    message: segment::parse_message(channel_post),
                }));
            }
        }
        UpdateData::EditedChannelPost {
            edited_channel_post,
        } => {
            if let Some(user) = edited_channel_post
                .from
                .clone()
                .and_then(|f| Some(parse_user(f)))
            {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::MessageEditedEvent(MessageEditedEvent {
                        user: user,
                        group: {
                            if edited_channel_post.chat.r#type == "private" {
                                None
                            } else {
                                Some(parse_group(edited_channel_post.chat.clone()))
                            }
                        },
                        new_message: Some(parse_message(edited_channel_post.clone())),
                        operator: {
                            if let Some(user) = edited_channel_post.from.clone() {
                                Some(parse_user(user))
                            } else {
                                None
                            }
                        },
                        old_message: None,
                    }),
                ));
            }
        }
        UpdateData::MessageReaction { message_reaction } => {
            if let Some(user) = message_reaction
                .user
                .clone()
                .and_then(|f| Some(parse_user(f)))
            {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::MessageReactionsEvent(MessageReactionsEvent {
                        user: user,
                        group: {
                            if message_reaction.chat.r#type == "private" {
                                None
                            } else {
                                Some(parse_group(message_reaction.chat.clone()))
                            }
                        },
                        message: {
                            Message {
                                id: format!(
                                    "{}_{}",
                                    message_reaction.chat.id, message_reaction.message_id
                                ),
                                segments: Vec::with_capacity(0),
                            }
                        },
                        reactions: message_reaction
                            .new_reaction
                            .into_iter()
                            .map(|r| parse_reaction(r))
                            .collect(),
                    }),
                ));
            }
        }
        UpdateData::MyChatMember { my_chat_member } => {
            if let ChatMember::Left { .. } = my_chat_member.new_chat_member {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberDecreaseEvent(
                        GroupMemberDecreaseEvent {
                            group: parse_group(my_chat_member.chat),
                            user: parse_user(my_chat_member.from),
                            reason: oxidebot::event::notice::GroupMemberDecreaseReason::Unknown,
                        },
                    ),
                ))
            } else if let ChatMember::Banned { user, until_date } = my_chat_member.new_chat_member {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberMuteChangeEvent(
                        GroupMemberMuteChangeEvent {
                            group: parse_group(my_chat_member.chat),
                            user: parse_user(user),
                            operator: None,
                            r#type: oxidebot::event::notice::MuteType::Mute {
                                duration: Some(Duration::from_secs({
                                    // 使用结束时间减去当前时间得到禁言时长
                                    if let Some(until_date) = until_date {
                                        (until_date - chrono::Local::now().timestamp()) as u64
                                    } else {
                                        0
                                    }
                                })),
                            },
                        },
                    ),
                ))
            } else if let ChatMember::Administrator { .. } | ChatMember::Owner { .. } =
                my_chat_member.new_chat_member
            {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupAdminChangeEvent(GroupAdminChangeEvent {
                        group: parse_group(my_chat_member.chat),
                        user: parse_user(my_chat_member.from),
                        r#type: {
                            if let ChatMember::Administrator { .. } | ChatMember::Owner { .. } =
                                my_chat_member.new_chat_member
                            {
                                GroupAdminChangeType::Set
                            } else {
                                GroupAdminChangeType::Unset
                            }
                        },
                    }),
                ))
            }
        }
        UpdateData::ChatMember { chat_member } => {
            if let ChatMember::Left { .. } = chat_member.new_chat_member {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberDecreaseEvent(
                        GroupMemberDecreaseEvent {
                            group: parse_group(chat_member.chat),
                            user: parse_user(chat_member.from),
                            reason: oxidebot::event::notice::GroupMemberDecreaseReason::Unknown,
                        },
                    ),
                ))
            } else if let ChatMember::Banned { user, .. } = chat_member.new_chat_member {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberDecreaseEvent(
                        GroupMemberDecreaseEvent {
                            group: parse_group(chat_member.chat),
                            user: parse_user(user),
                            reason: oxidebot::event::notice::GroupMemberDecreaseReason::Kick {
                                operator: None,
                            },
                        },
                    ),
                ))
            } else if let ChatMember::Restricted {
                user, until_date, ..
            } = chat_member.new_chat_member
            {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberMuteChangeEvent(
                        GroupMemberMuteChangeEvent {
                            group: parse_group(chat_member.chat),
                            user: parse_user(user),
                            operator: None,
                            r#type: oxidebot::event::notice::MuteType::Mute {
                                duration: Some(Duration::from_secs({
                                    // 使用结束时间减去当前时间得到禁言时长
                                    (until_date - chrono::Local::now().timestamp()) as u64
                                })),
                            },
                        },
                    ),
                ))
            } else if let ChatMember::Administrator { .. } | ChatMember::Owner { .. } =
                chat_member.new_chat_member
            {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupAdminChangeEvent(GroupAdminChangeEvent {
                        group: parse_group(chat_member.chat),
                        user: parse_user(chat_member.from),
                        r#type: {
                            if let ChatMember::Administrator { .. } | ChatMember::Owner { .. } =
                                chat_member.new_chat_member
                            {
                                GroupAdminChangeType::Set
                            } else {
                                GroupAdminChangeType::Unset
                            }
                        },
                    }),
                ))
            } else if let ChatMember::Restricted {
                user, until_date, ..
            } = chat_member.new_chat_member
            {
                results.push(Event::NoticeEvent(
                    oxidebot::event::NoticeEvent::GroupMemberMuteChangeEvent(
                        GroupMemberMuteChangeEvent {
                            group: parse_group(chat_member.chat),
                            user: parse_user(user),
                            operator: None,
                            r#type: oxidebot::event::notice::MuteType::Mute {
                                duration: Some(Duration::from_secs({
                                    // 使用结束时间减去当前时间得到禁言时长
                                    (until_date - chrono::Local::now().timestamp()) as u64
                                })),
                            },
                        },
                    ),
                ))
            }
        }
        UpdateData::ChatJoinRequest { chat_join_request } => results.push({
            Event::RequestEvent(oxidebot::event::RequestEvent::GroupAddEvent(
                GroupAddEvent {
                    id: chat_join_request.user_chat_id.to_string(),
                    user: parse_user(chat_join_request.from),
                    group: parse_group(chat_join_request.chat),
                    message: chat_join_request.bio,
                },
            ))
        }),
        UpdateData::ChatBoost { chat_boost } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "ChatBoost".to_string(),
            data: Box::new(ChatBoostEventWrapper(chat_boost)),
        })),
        UpdateData::RemovedChatBoost { removed_chat_boost } => {
            results.push(Event::AnyEvent(AnyEvent {
                server: SERVER,
                r#type: "RemovedChatBoost".to_string(),
                data: Box::new(RemovedChatBoostEventWrapper(removed_chat_boost)),
            }))
        }
        UpdateData::MessageReactionCount {
            message_reaction_count,
        } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "MessageReactionCount".to_string(),
            data: Box::new(MessageReactionCountEventWrapper(message_reaction_count)),
        })),
        UpdateData::InlineQuery { inline_query } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "InlineQuery".to_string(),
            data: Box::new(InlineQueryEventWrapper(inline_query)),
        })),
        UpdateData::ChosenInlineResult {
            chosen_inline_result,
        } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "ChosenInlineResult".to_string(),
            data: Box::new(ChosenInlineResultEventWrapper(chosen_inline_result)),
        })),
        UpdateData::CallbackQuery { callback_query } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "CallbackQuery".to_string(),
            data: Box::new(CallbackQueryEventWrapper(callback_query)),
        })),
        UpdateData::ShippingQuery { shipping_query } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "ShippingQuery".to_string(),
            data: Box::new(ShippingQueryEventWrapper(shipping_query)),
        })),
        UpdateData::PreCheckoutQuery { pre_checkout_query } => {
            results.push(Event::AnyEvent(AnyEvent {
                server: SERVER,
                r#type: "PreCheckoutQuery".to_string(),
                data: Box::new(PreCheckoutQueryEventWrapper(pre_checkout_query)),
            }))
        }
        UpdateData::Poll { poll } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "Poll".to_string(),
            data: Box::new(PollEventWrapper(poll)),
        })),
        UpdateData::PollAnswer { poll_answer } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "PollAnswer".to_string(),
            data: Box::new(PollAnswerEventWrapper(poll_answer)),
        })),
        UpdateData::DeletedBusinessMessages {
            deleted_business_messages,
        } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "DeletedBusinessMessages".to_string(),
            data: Box::new(DeletedBusinessMessagesEventWrapper(
                deleted_business_messages,
            )),
        })),
        UpdateData::EditedBusinessMessage {
            edited_business_message,
        } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "EditedBusinessMessage".to_string(),
            data: Box::new(EditedBusinessMessageEventWrapper(edited_business_message)),
        })),
        UpdateData::BusinessConnection {
            business_connection,
        } => results.push(Event::AnyEvent(AnyEvent {
            server: SERVER,
            r#type: "BusinessConnection".to_string(),
            data: Box::new(BussinessConnectionEventWrapper(business_connection)),
        })),
        UpdateData::BusinessMessage { business_message } => {
            results.push(Event::AnyEvent(AnyEvent {
                server: SERVER,
                r#type: "BusinessMessage".to_string(),
                data: Box::new(EditedBusinessMessageEventWrapper(business_message)),
            }))
        }
    }

    results
}
