use std::{str::FromStr as _, time::Duration};

use anyhow::Result;
use chrono::Datelike as _;
use hyper::Uri;
use oxidebot::{
    api::{
        payload::{GroupAdminChangeType, GroupMuteType, RequestResponse, SendMessageTarget},
        BotGetFriendListResponse, BotGetGroupListResponse, BotGetProfileResponse, CallApiTrait,
        GetMessageDetailResponse, GroupGetFileCountResponse, GroupGetFsListResponse,
        GroupGetProfileResponse, GroupMemberListResponse, SendMessageResponse,
        UserGetProfileResponse,
    },
    source::{
        group::GroupProfile,
        message::{File, MessageSegment},
        user::{Role, UserGroupInfo, UserProfile},
    },
    BotTrait as _,
};
use telegram_bot_api_rs::{
    available_methods::payload::{
        ChatIdPayload, GetFilePayload, RestrictChatMemberPayload, SendMediaGroupPayload,
        SendMessagePayload, SendVenuePayload,
    },
    available_types::{Birthdate, ChatMember, ChatPermissions, InputMedia, ReactionType},
    stickers::payload::SendStickerPayload,
    updateing_messages::payload::{
        DeleteMessagePayload, EditMessageMediaPayload, EditMessageTextPayload,
    },
};

use crate::{bot::TelegramBot, segment::process_message_segments, utils::split_id};

impl CallApiTrait for TelegramBot {
    fn send_message<'life0, 'async_trait>(
        &'life0 self,
        message: Vec<MessageSegment>,
        target: SendMessageTarget,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<Vec<SendMessageResponse>>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let bot = self.bot.clone();
        Box::pin(async move {
            let (text_segments, mut media_segments, reply, entities, venues, stickers) =
                process_message_segments(message);
            let mut results: Vec<SendMessageResponse> = Vec::new();
            let chat_id = match target {
                SendMessageTarget::Group(id) => id,
                SendMessageTarget::Private(id) => id,
            };
            let text = text_segments.join("\n");
            if media_segments.is_empty() {
                let response = bot
                    .send_message(&SendMessagePayload {
                        chat_id: chat_id.clone(),
                        text,
                        entities: Some(entities),
                        reply_parameters: reply.clone(),
                        ..Default::default()
                    })
                    .await?;
                results.push(SendMessageResponse {
                    sent_message_id: format!("{}_{}", response.chat.id, response.message_id),
                });
            } else {
                if !entities.is_empty() {
                    let msg = bot
                        .send_message(&SendMessagePayload {
                            chat_id: chat_id.clone(),
                            text,
                            entities: Some(entities),
                            reply_parameters: reply.clone(),
                            ..Default::default()
                        })
                        .await?;
                    results.push(SendMessageResponse {
                        sent_message_id: format!("{}_{}", msg.chat.id, msg.message_id),
                    });
                } else {
                    let last = media_segments.last_mut().unwrap();
                    match last {
                        InputMedia::Photo { caption, .. }
                        | InputMedia::Video { caption, .. }
                        | InputMedia::Animation { caption, .. }
                        | InputMedia::Document { caption, .. }
                        | InputMedia::Audio { caption, .. } => {
                            *caption = Some(text_segments.join("\n"));
                        }
                    }
                }
                let msg = bot
                    .send_media_group(SendMediaGroupPayload {
                        chat_id: chat_id.clone(),
                        reply_parameters: reply.clone(),
                        media: media_segments,
                        ..Default::default()
                    })
                    .await?;
                results.append(
                    &mut msg
                        .into_iter()
                        .map(|m| SendMessageResponse {
                            sent_message_id: format!("{}_{}", m.chat.id, m.message_id),
                        })
                        .collect::<Vec<SendMessageResponse>>(),
                )
            }
            if !venues.is_empty() {
                for venue in venues {
                    let response = bot
                        .send_venue(&SendVenuePayload {
                            chat_id: chat_id.clone(),
                            latitude: venue.location.latitude,
                            longitude: venue.location.longitude,
                            title: venue.title,
                            reply_parameters: reply.clone(),
                            ..Default::default()
                        })
                        .await?;
                    results.push(SendMessageResponse {
                        sent_message_id: format!("{}_{}", response.chat.id, response.message_id),
                    });
                }
            }
            if !stickers.is_empty() {
                for sticker in stickers {
                    let response = bot
                        .send_sticker(SendStickerPayload {
                            chat_id: chat_id.clone(),
                            sticker,
                            reply_parameters: reply.clone(),
                            ..Default::default()
                        })
                        .await?;
                    results.push(SendMessageResponse {
                        sent_message_id: format!("{}_{}", response.chat.id, response.message_id),
                    });
                }
            }
            Ok(results)
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn delete_message<'life0, 'async_trait>(
        &'life0 self,
        message_id: String,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let (chat_id, message_id) = split_id(message_id)?;
            self.bot
                .delete_message(&DeleteMessagePayload {
                    chat_id: chat_id,
                    message_id: message_id.parse()?,
                    ..Default::default()
                })
                .await?;
            Ok(())
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn edit_messagee<'life0, 'async_trait>(
        &'life0 self,
        message_id: String,
        new_message: Vec<MessageSegment>,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let (chat_id, message_id) = split_id(message_id)?;

            let (text_segments, mut media_segments, ..) = process_message_segments(new_message);
            if media_segments.is_empty() {
                self.bot
                    .edit_message_text(&EditMessageTextPayload {
                        chat_id: Some(chat_id),
                        message_id: Some(message_id.parse()?),
                        text: text_segments.join("\n"),
                        ..Default::default()
                    })
                    .await?;
            } else {
                if media_segments.len() > 1 {
                    tracing::warn!("Media segments more than 1, only the first one will be sent");
                }
                match media_segments.last_mut() {
                    Some(InputMedia::Photo { caption, .. })
                    | Some(InputMedia::Video { caption, .. })
                    | Some(InputMedia::Animation { caption, .. })
                    | Some(InputMedia::Document { caption, .. })
                    | Some(InputMedia::Audio { caption, .. }) => {
                        *caption = Some(text_segments.join("\n"));
                    }
                    _ => {}
                };
                self.bot
                    .edit_message_media(&EditMessageMediaPayload {
                        chat_id: Some(chat_id),
                        message_id: Some(message_id.parse()?),
                        media: media_segments.into_iter().next().unwrap(),
                        business_connection_id: None,
                        inline_message_id: None,
                        reply_markup: None,
                    })
                    .await?;
            }
            Ok(())
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_message_detail<'life0, 'async_trait>(
        &'life0 self,
        _: String,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<GetMessageDetailResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support get message by message_id"
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn set_message_reaction<'life0, 'async_trait>(
        &'life0 self,
        message_id: String,
        reaction_id: String,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let (chat_id, message_id) = split_id(message_id)?;
            self.bot
                .set_message_reaction(
                    &telegram_bot_api_rs::available_methods::payload::SetMessageReactionPayload {
                        chat_id,
                        message_id: message_id.parse()?,
                        reaction: vec![ReactionType::Emoji { emoji: reaction_id }],
                        ..Default::default()
                    },
                )
                .await?;
            Ok(())
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_group_member_list<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<GroupMemberListResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            tracing::warn!("Telegram bot can only get admins of group.");
            let users = self
                .bot
                .get_chat_administrators(&ChatIdPayload { chat_id: group_id })
                .await?;
            let mut results = Vec::new();
            for m in users {
                let role = Some({
                    if let ChatMember::Owner { .. } = m {
                        Role::Owner
                    } else {
                        Role::Admin
                    }
                });
                match m {
                    telegram_bot_api_rs::available_types::ChatMember::Owner { user, .. }
                    | telegram_bot_api_rs::available_types::ChatMember::Administrator {
                        user,
                        ..
                    } => results.push(oxidebot::source::user::User {
                        id: user.id.to_string(),
                        profile: Some(UserProfile {
                            nickname: Some(user.username.unwrap_or_else(|| {
                                format!(
                                    "{} {}",
                                    user.first_name,
                                    user.last_name.unwrap_or_default()
                                )
                            })),
                            ..Default::default()
                        }),
                        group_info: Some(UserGroupInfo {
                            role,
                            ..Default::default()
                        }),
                    }),
                    _ => {}
                }
            }
            Ok(GroupMemberListResponse { members: results })
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn kick_group_member<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        user_id: String,
        _: Option<bool>,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            self.bot
                .ban_chat_member(
                    &telegram_bot_api_rs::available_methods::payload::BanChatMemberPayload {
                        chat_id: group_id,
                        user_id: user_id.parse()?,
                        until_date: None,
                        revoke_messages: None,
                    },
                )
                .await?;
            Ok(())
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn mute_group<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        duration: Option<Duration>,
        r#type: GroupMuteType,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = r#type;
        let _ = duration;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support mute whole group."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn mute_group_member<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        user_id: String,
        r#type: GroupMuteType,
        duration: Option<Duration>,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            match r#type {
                GroupMuteType::Mute => {
                    self.bot
                        .restrict_chat_member(&RestrictChatMemberPayload {
                            chat_id: group_id,
                            user_id: user_id.parse()?,
                            permissions: ChatPermissions {
                                can_send_messages: Some(false),
                                can_send_audios: Some(false),
                                can_send_documents: Some(false),
                                can_send_photos: Some(false),
                                can_send_videos: Some(false),
                                can_send_video_notes: Some(false),
                                can_send_voice_notes: Some(false),
                                can_send_polls: Some(false),
                                can_send_other_messages: Some(false),
                                ..Default::default()
                            },
                            until_date: Some({
                                let now = chrono::Utc::now();
                                let duration = duration.unwrap_or_else(|| Duration::from_secs(60));
                                now.timestamp() + duration.as_secs() as i64
                            }),
                            ..Default::default()
                        })
                        .await?;
                }
                GroupMuteType::Unmute => {
                    self.bot
                        .restrict_chat_member(&RestrictChatMemberPayload {
                            chat_id: group_id,
                            user_id: user_id.parse()?,
                            permissions: ChatPermissions {
                                can_send_messages: Some(true),
                                can_send_audios: Some(true),
                                can_send_documents: Some(true),
                                can_send_photos: Some(true),
                                can_send_videos: Some(true),
                                can_send_video_notes: Some(true),
                                can_send_voice_notes: Some(true),
                                can_send_polls: Some(true),
                                can_send_other_messages: Some(true),
                                ..Default::default()
                            },
                            until_date: Some({
                                let now = chrono::Utc::now();
                                let duration = duration.unwrap_or_else(|| Duration::from_secs(60));
                                now.timestamp() + duration.as_secs() as i64
                            }),
                            ..Default::default()
                        })
                        .await?;
                }
            }

            Ok(())
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn change_group_admin<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        user_id: String,
        r#type: GroupAdminChangeType,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = r#type;
        let _ = user_id;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support change group admin."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn set_group_member_alias<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        user_id: String,
        new_alias: String,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = new_alias;
        let _ = user_id;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support set group member alias."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_group_profile<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<GroupGetProfileResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let chat_full_info = self
                .bot
                .get_chat(&ChatIdPayload {
                    chat_id: group_id.clone(),
                })
                .await?;
            let count = self
                .bot
                .get_chat_member_count(&ChatIdPayload { chat_id: group_id })
                .await?;

            let group_profile = GroupProfile {
                name: chat_full_info.title,
                avatar: None,
                member_count: Some(count as u64),
            };
            Ok(GroupGetProfileResponse {
                profile: group_profile,
            })
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn set_group_profile<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        new_profile: GroupProfile,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = new_profile;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support set group profile."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_group_file_count<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        parent_folder_id: Option<String>,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<GroupGetFileCountResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = parent_folder_id;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support get group file count."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_group_fs_list<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        start_index: u64,
        count: u64,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<GroupGetFsListResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = count;
        let _ = start_index;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support get group file system list."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn delete_group_file<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        file_id: String,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = file_id;
        let _ = group_id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support delete group file."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn delete_group_folder<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        folder_id: String,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let _ = folder_id;
            let _ = group_id;
            return Err(anyhow::anyhow!(
                "Telegram doesn't support delete group folder."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn create_group_folder<'life0, 'async_trait>(
        &'life0 self,
        group_id: String,
        folder_name: String,
        parent_folder_id: Option<String>,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let _ = folder_name;
            let _ = parent_folder_id;
            let _ = group_id;
            return Err(anyhow::anyhow!(
                "Telegram doesn't support create group folder."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_user_profile<'life0, 'async_trait>(
        &'life0 self,
        user_id: String,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<UserGetProfileResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let chat_full_info = self
                .bot
                .get_chat(&ChatIdPayload {
                    chat_id: user_id.clone(),
                })
                .await?;
            let user_profile = UserProfile {
                nickname: Some(chat_full_info.username.unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        chat_full_info.first_name.unwrap_or_default(),
                        chat_full_info.last_name.unwrap_or_default()
                    )
                })),
                signature: chat_full_info.bio,
                age: {
                    if let Some(Birthdate {
                        year: Some(year), ..
                    }) = chat_full_info.birthdate
                    {
                        Some((chrono::Utc::now().year() as i64 - year) as u64)
                    } else {
                        None
                    }
                },
                ..Default::default()
            };
            Ok(UserGetProfileResponse {
                profile: user_profile,
            })
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn set_bot_profile<'life0, 'async_trait>(
        &'life0 self,
        new_profile: UserProfile,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let UserProfile {
                nickname,
                signature,
                ..
            } = new_profile;
            if let Some(nickname) = nickname {
                self.bot
                    .set_my_name(
                        &telegram_bot_api_rs::available_methods::payload::SetMyNamePayload {
                            name: nickname,
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            if let Some(signature) = signature {
                self.bot
                    .set_my_description(
                        &telegram_bot_api_rs::available_methods::payload::SetMyDescriptionPayload {
                            description: signature,
                            ..Default::default()
                        },
                    )
                    .await?;
            }
            Ok(())
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_bot_profile<'life0, 'async_trait>(
        &'life0 self,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<BotGetProfileResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let bot_info = self.bot_info().await;
            Ok(BotGetProfileResponse {
                profile: UserProfile {
                    nickname: bot_info.nickname,
                    ..Default::default()
                },
            })
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_bot_friend_list<'life0, 'async_trait>(
        &'life0 self,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<BotGetFriendListResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support get bot friend list."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_bot_group_list<'life0, 'async_trait>(
        &'life0 self,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<BotGetGroupListResponse>>
                + ::core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support get bot group list."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn handle_add_friend_request<'life0, 'async_trait>(
        &'life0 self,
        id: String,
        response: RequestResponse,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = response;
        let _ = id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support handle add friend request."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn handle_add_group_request<'life0, 'async_trait>(
        &'life0 self,
        id: String,
        response: RequestResponse,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = response;
        let _ = id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support handle add group request."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn handle_invite_group_request<'life0, 'async_trait>(
        &'life0 self,
        id: String,
        response: RequestResponse,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = Result<()>> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        let _ = response;
        let _ = id;
        Box::pin(async move {
            return Err(anyhow::anyhow!(
                "Telegram doesn't support handle invite group request."
            ));
        })
    }

    #[must_use]
    #[allow(
        clippy::async_yields_async,
        clippy::diverging_sub_expression,
        clippy::let_unit_value,
        clippy::no_effect_underscore_binding,
        clippy::shadow_same,
        clippy::type_complexity,
        clippy::type_repetition_in_bounds,
        clippy::used_underscore_binding
    )]
    fn get_file_info<'life0, 'async_trait>(
        &'life0 self,
        file_id: String,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = Result<File>> + ::core::marker::Send + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: ::core::marker::Sync + 'async_trait,
    {
        Box::pin(async move {
            let file = self.bot.get_file(&GetFilePayload { file_id }).await?;
            if let Some(path) = file.file_path {
                Ok(File {
                    uri: Some(
                        Uri::from_str(&format!(
                            "https://api.telegram.org/file/bot{}/{}",
                            self.bot.token, path
                        ))
                        .unwrap(),
                    ),
                    ..Default::default()
                })
            } else {
                Err(anyhow::anyhow!("File path not found"))
            }
        })
    }
}
