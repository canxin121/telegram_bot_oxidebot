use std::time::Duration;

use chrono::{DateTime, Utc};
use oxidebot::{
    event::{
        any::{AnyEvent, AnyEventDataTrait},
        notice::{
            GroupAdminChangeEvent, GroupAdminChangeType, GroupMemberAliasChangeEvent,
            GroupMemberDecreaseEvent, GroupMemberDecreaseReason, GroupMemberIncreseEvent,
            GroupMemberIncreseReason, GroupMemberMuteChangeEvent, MessageEditedEvent,
            MessageReactionsEvent, MuteType,
        },
        request::GroupAddEvent,
        Event, EventObject, MessageEvent, NoticeEvent, RequestEvent,
    },
    source::message::Message,
    EventTrait,
};
use serde_json::Value;

use crate::{
    segment::{parse_message, parse_reaction},
    telegram::{ChatMemberUpdated, Message as TelegramMessage, MessageReactionUpdated, Update},
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
        "message" => update
            .decode::<TelegramMessage>(kind)
            .map(parse_message_update),
        "edited_message" => update
            .decode::<TelegramMessage>(kind)
            .map(parse_edited_message),
        "channel_post" => update
            .decode::<TelegramMessage>(kind)
            .map(parse_message_update),
        "edited_channel_post" => update
            .decode::<TelegramMessage>(kind)
            .map(parse_edited_message),
        "message_reaction" => update
            .decode::<MessageReactionUpdated>(kind)
            .map(parse_message_reaction),
        "my_chat_member" | "chat_member" => update
            .decode::<ChatMemberUpdated>(kind)
            .map(parse_chat_member_update),
        "chat_join_request" => {
            update
                .decode::<crate::telegram::ChatJoinRequest>(kind)
                .map(|request| {
                    vec![Event::RequestEvent(RequestEvent::GroupAddEvent(
                        GroupAddEvent {
                            id: join_request_id(request.chat.id, request.from.id),
                            user: parse_user(request.from),
                            group: parse_group(request.chat),
                            message: request.bio,
                        },
                    ))]
                })
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

fn parse_message_update(message: TelegramMessage) -> Vec<Event> {
    let mut events = Vec::new();

    let sender = message
        .from
        .clone()
        .map(parse_user)
        .or_else(|| message.sender_chat.clone().map(parse_chat_sender));
    let parsed_message = parse_message(message.clone());
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

fn parse_edited_message(message: TelegramMessage) -> Vec<Event> {
    let sender = message
        .from
        .clone()
        .map(parse_user)
        .or_else(|| message.sender_chat.clone().map(parse_chat_sender));
    let Some(sender) = sender else {
        return Vec::new();
    };

    vec![Event::NoticeEvent(NoticeEvent::MessageEditedEvent(
        MessageEditedEvent {
            user: sender.clone(),
            group: (!is_private(&message)).then(|| parse_group(message.chat.clone())),
            new_message: Some(parse_message(message)),
            operator: Some(sender),
            old_message: None,
        },
    ))]
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

    vec![Event::NoticeEvent(NoticeEvent::MessageReactionsEvent(
        MessageReactionsEvent {
            user: actor,
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
        },
    ))]
}

fn parse_chat_member_update(update: ChatMemberUpdated) -> Vec<Event> {
    let mut events = Vec::new();
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

    events
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
    fn bot_api_10_2_subscription_is_forwarded_raw() {
        let update = update(serde_json::json!({
            "update_id": 1,
            "subscription": {"currency": "XTR"}
        }));
        let events = parse_update(&update);
        let Event::AnyEvent(event) = &events[0] else {
            panic!("expected raw event")
        };
        assert_eq!(event.r#type, "subscription");
        assert!(event.downcast_ref::<TelegramRawEvent>().is_some());
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
    fn known_update_with_no_common_semantic_mapping_is_not_dropped() {
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
        let Event::AnyEvent(event) = &events[0] else {
            panic!("expected a raw fallback event")
        };
        assert_eq!(event.r#type, "chat_member");
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
}
