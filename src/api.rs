use std::{str::FromStr as _, time::Duration};

use anyhow::{Context as _, Result};
use base64::Engine as _;
use chrono::Datelike as _;
use hyper::Uri;
use oxidebot::{
    api::{
        payload::{GroupAdminChangeType, GroupMuteType, RequestResponse, SendMessageTarget},
        platform::{
            PlatformApiFile, PlatformApiFileSource, PlatformApiRequest, PlatformApiResponse,
        },
        BotGetFriendListResponse, BotGetGroupListResponse, BotGetProfileResponse, CallApiTrait,
        GetMessageDetailResponse, GroupGetFileCountResponse, GroupGetFsListResponse,
        GroupGetProfileResponse, GroupMemberListResponse, SendMessageResponse,
        UserGetProfileResponse,
    },
    source::{
        group::GroupProfile,
        message::{File, MessageSegment},
        user::{Role, User, UserGroupInfo, UserProfile},
    },
    BotTrait as _,
};
use reqwest::multipart::Form;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::{
    bot::TelegramBot,
    segment::{
        chat_id_value, process_message_segments, MediaKind, OutgoingMedia, OutgoingMessage,
        ReplyParameters,
    },
    telegram::{ChatFullInfo, ChatMember, Message, MessageEntity, Upload},
    utils::{split_id, split_join_request_id, telegram_user_name},
    TELEGRAM_BOT_API_METHODS,
};

const TEXT_LIMIT: usize = 4096;
const CAPTION_LIMIT: usize = 1024;
const MEDIA_GROUP_LIMIT: usize = 10;
const MAX_AVATAR_BYTES: u64 = 10 * 1024 * 1024;

#[async_trait::async_trait]
impl CallApiTrait for TelegramBot {
    async fn call_platform_api(&self, request: PlatformApiRequest) -> Result<PlatformApiResponse> {
        let PlatformApiRequest {
            method,
            mut parameters,
            files,
        } = request;
        anyhow::ensure!(
            parameters.is_object(),
            "platform API parameters must be a JSON object"
        );
        strip_nulls(&mut parameters);
        let result = if files.is_empty() {
            self.client().call::<_, Value>(&method, &parameters).await?
        } else {
            let Value::Object(parameters) = parameters else {
                unreachable!("platform API parameters were validated above")
            };
            let mut form = Form::new();
            let mut fields =
                std::collections::HashSet::with_capacity(parameters.len() + files.len());
            for (name, value) in parameters {
                anyhow::ensure!(
                    !name.is_empty(),
                    "platform API multipart parameter name cannot be empty"
                );
                fields.insert(name.clone());
                form = form.text(name, platform_form_value(value)?);
            }
            for file in files {
                let PlatformApiFile {
                    field,
                    file_name,
                    mime_type,
                    source,
                } = file;
                anyhow::ensure!(
                    !field.is_empty(),
                    "platform API attachment field cannot be empty"
                );
                anyhow::ensure!(
                    fields.insert(field.clone()),
                    "duplicate platform API multipart field {:?}",
                    field
                );
                anyhow::ensure!(
                    !file_name.is_empty(),
                    "platform API attachment file name cannot be empty"
                );
                let mut part = match source {
                    PlatformApiFileSource::Bytes(bytes) => Upload {
                        name: file_name.clone(),
                        bytes,
                        mime: None,
                    }
                    .into_part()?,
                    PlatformApiFileSource::Path(path) => reqwest::multipart::Part::file(&path)
                        .await
                        .with_context(|| {
                            format!("failed to open platform API attachment {path:?}")
                        })?
                        .file_name(file_name),
                };
                if let Some(mime_type) = mime_type {
                    part = part.mime_str(&mime_type).with_context(|| {
                        format!("invalid platform API attachment MIME type {mime_type:?}")
                    })?;
                }
                form = form.part(field, part);
            }
            self.client().call_multipart::<Value>(&method, form).await?
        };
        Ok(PlatformApiResponse { result })
    }

    fn platform_api_methods(&self) -> &'static [&'static str] {
        TELEGRAM_BOT_API_METHODS
    }

    async fn send_message(
        &self,
        message: Vec<MessageSegment>,
        target: SendMessageTarget,
    ) -> Result<Vec<SendMessageResponse>> {
        let chat_id = match target {
            SendMessageTarget::Group(id) | SendMessageTarget::Private(id) => id,
        };
        let mut outgoing = process_message_segments(message)?;
        let mut sent = Vec::new();
        let mut reply = outgoing.reply.take();

        attach_caption_or_send_text(self, &chat_id, &mut outgoing, &mut reply, &mut sent).await?;
        send_media(self, &chat_id, outgoing.media, &mut reply, &mut sent).await?;

        for venue in outgoing.venues {
            let response: Message = if venue.title.trim().is_empty() {
                self.call(
                    "sendLocation",
                    json!({
                        "chat_id": chat_id_value(&chat_id),
                        "latitude": venue.latitude,
                        "longitude": venue.longitude,
                        "reply_parameters": reply.take(),
                    }),
                )
                .await?
            } else {
                self.call(
                    "sendVenue",
                    json!({
                        "chat_id": chat_id_value(&chat_id),
                        "latitude": venue.latitude,
                        "longitude": venue.longitude,
                        "title": venue.title,
                        "address": venue.address,
                        "reply_parameters": reply.take(),
                    }),
                )
                .await?
            };
            sent.push(sent_response(response));
        }

        for sticker in outgoing.stickers {
            let response: Message = self
                .call(
                    "sendSticker",
                    json!({
                        "chat_id": chat_id_value(&chat_id),
                        "sticker": sticker,
                        "reply_parameters": reply.take(),
                    }),
                )
                .await?;
            sent.push(sent_response(response));
        }

        Ok(sent)
    }

    async fn delete_message(&self, message_id: String) -> Result<()> {
        let (chat_id, message_id) = split_id(message_id)?;
        let _: bool = self
            .call(
                "deleteMessage",
                json!({"chat_id": chat_id_value(&chat_id), "message_id": message_id}),
            )
            .await?;
        Ok(())
    }

    async fn edit_messagee(
        &self,
        message_id: String,
        new_message: Vec<MessageSegment>,
    ) -> Result<()> {
        let (chat_id, message_id) = split_id(message_id)?;
        let mut outgoing = process_message_segments(new_message)?;
        anyhow::ensure!(
            outgoing.reply.is_none(),
            "an edited message cannot contain a reply segment"
        );
        anyhow::ensure!(
            outgoing.venues.is_empty() && outgoing.stickers.is_empty(),
            "Telegram cannot edit a message into a venue or sticker"
        );
        anyhow::ensure!(
            outgoing.media.len() <= 1,
            "Telegram can edit only one media item at a time"
        );

        if outgoing.media.is_empty() {
            anyhow::ensure!(
                !outgoing.text.is_empty(),
                "edited message text cannot be empty"
            );
            anyhow::ensure!(
                utf16_len(&outgoing.text) <= TEXT_LIMIT,
                "edited Telegram message exceeds {TEXT_LIMIT} UTF-16 code units"
            );
            let _: Message = self
                .call(
                    "editMessageText",
                    json!({
                        "chat_id": chat_id_value(&chat_id),
                        "message_id": message_id,
                        "text": outgoing.text,
                        "entities": optional_entities(outgoing.entities),
                    }),
                )
                .await?;
        } else {
            let mut media = outgoing.media.remove(0);
            if !outgoing.text.is_empty() {
                media.caption = Some(match media.caption.take() {
                    Some(caption) => format!("{}\n{caption}", outgoing.text),
                    None => outgoing.text,
                });
            }
            anyhow::ensure!(
                media.caption.as_deref().map(utf16_len).unwrap_or_default() <= CAPTION_LIMIT,
                "edited Telegram media caption exceeds {CAPTION_LIMIT} UTF-16 code units"
            );
            edit_media(self, &chat_id, message_id, media, outgoing.entities).await?;
        }
        Ok(())
    }

    async fn get_message_detail(&self, _message_id: String) -> Result<GetMessageDetailResponse> {
        Err(anyhow::anyhow!(
            "Telegram Bot API does not provide a get-message-by-id method"
        ))
    }

    async fn set_message_reaction(&self, message_id: String, reaction_id: String) -> Result<()> {
        let (chat_id, message_id) = split_id(message_id)?;
        let reaction = if reaction_id == "paid" {
            json!({"type": "paid"})
        } else if reaction_id
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            json!({"type": "custom_emoji", "custom_emoji_id": reaction_id})
        } else {
            json!({"type": "emoji", "emoji": reaction_id})
        };
        let _: bool = self
            .call(
                "setMessageReaction",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "message_id": message_id,
                    "reaction": [reaction],
                }),
            )
            .await?;
        Ok(())
    }

    async fn get_group_member_list(&self, group_id: String) -> Result<GroupMemberListResponse> {
        tracing::warn!("Telegram Bot API exposes administrators, not a complete group member list");
        let members: Vec<ChatMember> = self
            .call(
                "getChatAdministrators",
                json!({"chat_id": chat_id_value(&group_id), "return_bots": true}),
            )
            .await?;
        let members = members
            .into_iter()
            .map(|member| User {
                id: member.user.id.to_string(),
                profile: Some(UserProfile {
                    nickname: Some(telegram_user_name(&member.user)),
                    ..Default::default()
                }),
                group_info: Some(UserGroupInfo {
                    alias: member.custom_title,
                    role: Some(if member.status == "creator" {
                        Role::Owner
                    } else {
                        Role::Admin
                    }),
                    ..Default::default()
                }),
            })
            .collect();
        Ok(GroupMemberListResponse { members })
    }

    async fn kick_group_member(
        &self,
        group_id: String,
        user_id: String,
        reject_add_request: Option<bool>,
    ) -> Result<()> {
        let user_id: i64 = user_id.parse().context("invalid Telegram user id")?;
        let _: bool = self
            .call(
                "banChatMember",
                json!({
                    "chat_id": chat_id_value(&group_id),
                    "user_id": user_id,
                    "revoke_messages": true,
                }),
            )
            .await?;
        if reject_add_request == Some(false) {
            let _: bool = self
                .call(
                    "unbanChatMember",
                    json!({
                        "chat_id": chat_id_value(&group_id),
                        "user_id": user_id,
                        "only_if_banned": true,
                    }),
                )
                .await?;
        }
        Ok(())
    }

    async fn mute_group(
        &self,
        group_id: String,
        duration: Option<Duration>,
        r#type: GroupMuteType,
    ) -> Result<()> {
        anyhow::ensure!(
            duration.is_none(),
            "Telegram does not support expiring chat-wide permissions; mute members individually for a temporary mute"
        );
        let allow = matches!(r#type, GroupMuteType::Unmute);
        let _: bool = self
            .call(
                "setChatPermissions",
                json!({
                    "chat_id": chat_id_value(&group_id),
                    "permissions": chat_permissions(allow),
                    "use_independent_chat_permissions": true,
                }),
            )
            .await?;
        Ok(())
    }

    async fn mute_group_member(
        &self,
        group_id: String,
        user_id: String,
        r#type: GroupMuteType,
        duration: Option<Duration>,
    ) -> Result<()> {
        let allow = matches!(r#type, GroupMuteType::Unmute);
        let until_date = if allow {
            None
        } else {
            duration
                .map(|duration| {
                    i64::try_from(duration.as_secs())
                        .context("mute duration is too large")
                        .map(|duration| chrono::Utc::now().timestamp().saturating_add(duration))
                })
                .transpose()?
        };
        let _: bool = self
            .call(
                "restrictChatMember",
                json!({
                    "chat_id": chat_id_value(&group_id),
                    "user_id": user_id.parse::<i64>().context("invalid Telegram user id")?,
                    "permissions": chat_permissions(allow),
                    "use_independent_chat_permissions": true,
                    "until_date": until_date,
                }),
            )
            .await?;
        Ok(())
    }

    async fn change_group_admin(
        &self,
        group_id: String,
        user_id: String,
        r#type: GroupAdminChangeType,
    ) -> Result<()> {
        let promote = matches!(r#type, GroupAdminChangeType::Set);
        let _: bool = self
            .call(
                "promoteChatMember",
                json!({
                    "chat_id": chat_id_value(&group_id),
                    "user_id": user_id.parse::<i64>().context("invalid Telegram user id")?,
                    "can_manage_chat": promote,
                    "can_change_info": false,
                    "can_post_messages": false,
                    "can_edit_messages": false,
                    "can_delete_messages": false,
                    "can_invite_users": false,
                    "can_restrict_members": false,
                    "can_pin_messages": false,
                    "can_manage_topics": false,
                    "can_promote_members": false,
                    "can_manage_video_chats": false,
                    "can_post_stories": false,
                    "can_edit_stories": false,
                    "can_delete_stories": false,
                    "can_manage_direct_messages": false,
                    "can_manage_tags": false,
                }),
            )
            .await?;
        Ok(())
    }

    async fn set_group_member_alias(
        &self,
        group_id: String,
        user_id: String,
        new_alias: String,
    ) -> Result<()> {
        anyhow::ensure!(
            new_alias.chars().count() <= 16,
            "Telegram administrator custom titles are limited to 16 characters"
        );
        let _: bool = self
            .call(
                "setChatAdministratorCustomTitle",
                json!({
                    "chat_id": chat_id_value(&group_id),
                    "user_id": user_id.parse::<i64>().context("invalid Telegram user id")?,
                    "custom_title": new_alias,
                }),
            )
            .await?;
        Ok(())
    }

    async fn get_group_profile(&self, group_id: String) -> Result<GroupGetProfileResponse> {
        let chat: ChatFullInfo = self
            .call("getChat", json!({"chat_id": chat_id_value(&group_id)}))
            .await?;
        let count: i64 = self
            .call(
                "getChatMemberCount",
                json!({"chat_id": chat_id_value(&group_id)}),
            )
            .await?;
        let avatar = match chat.photo {
            Some(photo) => self.telegram_file_uri(&photo.big_file_id).await?,
            None => None,
        };
        Ok(GroupGetProfileResponse {
            profile: GroupProfile {
                name: chat.title,
                avatar,
                member_count: u64::try_from(count).ok(),
            },
        })
    }

    async fn set_group_profile(&self, group_id: String, new_profile: GroupProfile) -> Result<()> {
        if let Some(title) = new_profile.name {
            anyhow::ensure!(
                (1..=128).contains(&title.chars().count()),
                "Telegram chat titles must contain 1 to 128 characters"
            );
            let _: bool = self
                .call(
                    "setChatTitle",
                    json!({"chat_id": chat_id_value(&group_id), "title": title}),
                )
                .await?;
        }
        if let Some(avatar) = new_profile.avatar {
            let upload = avatar_upload(&avatar).await?;
            let form = Form::new()
                .text("chat_id", group_id)
                .part("photo", upload.into_part()?);
            let _: bool = self.client().call_multipart("setChatPhoto", form).await?;
        }
        Ok(())
    }

    async fn get_group_file_count(
        &self,
        _group_id: String,
        _parent_folder_id: Option<String>,
    ) -> Result<GroupGetFileCountResponse> {
        Err(anyhow::anyhow!(
            "Telegram chats do not expose a browsable group file system"
        ))
    }

    async fn get_group_fs_list(
        &self,
        _group_id: String,
        _start_index: u64,
        _count: u64,
    ) -> Result<GroupGetFsListResponse> {
        Err(anyhow::anyhow!(
            "Telegram chats do not expose a browsable group file system"
        ))
    }

    async fn get_user_profile(&self, user_id: String) -> Result<UserGetProfileResponse> {
        let chat: ChatFullInfo = self
            .call("getChat", json!({"chat_id": chat_id_value(&user_id)}))
            .await?;
        let nickname = chat.username.or_else(|| {
            let name = format!(
                "{} {}",
                chat.first_name.as_deref().unwrap_or_default(),
                chat.last_name.as_deref().unwrap_or_default()
            );
            (!name.trim().is_empty()).then(|| name.trim().to_owned())
        });
        let age = chat.birthdate.and_then(|birthdate| {
            let year = birthdate.year?;
            let today = chrono::Utc::now().date_naive();
            let mut age = today.year() - year;
            if (today.month(), today.day()) < (birthdate.month, birthdate.day) {
                age -= 1;
            }
            u64::try_from(age).ok()
        });
        let avatar = match chat.photo {
            Some(photo) => self.telegram_file_uri(&photo.big_file_id).await?,
            None => None,
        };
        Ok(UserGetProfileResponse {
            profile: UserProfile {
                nickname,
                signature: chat.bio,
                age,
                avatar,
                ..Default::default()
            },
        })
    }

    async fn set_bot_profile(&self, new_profile: UserProfile) -> Result<()> {
        if let Some(name) = new_profile.nickname {
            let _: bool = self.call("setMyName", json!({"name": name})).await?;
        }
        if let Some(description) = new_profile.signature {
            let _: bool = self
                .call("setMyDescription", json!({"description": description}))
                .await?;
        }
        Ok(())
    }

    async fn get_bot_profile(&self) -> Result<BotGetProfileResponse> {
        #[derive(serde::Deserialize)]
        struct Description {
            description: String,
        }
        let description: Description = self.call("getMyDescription", json!({})).await?;
        let bot_info = self.bot_info().await;
        Ok(BotGetProfileResponse {
            profile: UserProfile {
                nickname: bot_info.nickname,
                signature: Some(description.description),
                ..Default::default()
            },
        })
    }

    async fn get_bot_friend_list(&self) -> Result<BotGetFriendListResponse> {
        Err(anyhow::anyhow!(
            "Telegram Bot API does not expose a bot friend list"
        ))
    }

    async fn get_bot_group_list(&self) -> Result<BotGetGroupListResponse> {
        Err(anyhow::anyhow!(
            "Telegram Bot API does not expose a bot chat list"
        ))
    }

    async fn handle_add_group_request(&self, id: String, response: RequestResponse) -> Result<()> {
        let (chat_id, user_id) = split_join_request_id(&id)?;
        let method = match response {
            RequestResponse::Approve => "approveChatJoinRequest",
            RequestResponse::Reject => "declineChatJoinRequest",
        };
        let _: bool = self
            .call(method, json!({"chat_id": chat_id, "user_id": user_id}))
            .await?;
        Ok(())
    }

    async fn handle_add_friend_request(
        &self,
        _id: String,
        _response: RequestResponse,
    ) -> Result<()> {
        Err(anyhow::anyhow!(
            "Telegram private chats do not use friend requests"
        ))
    }

    async fn handle_invite_group_request(
        &self,
        _id: String,
        _response: RequestResponse,
    ) -> Result<()> {
        Err(anyhow::anyhow!(
            "Telegram bots cannot accept an invitation through Bot API"
        ))
    }

    async fn get_file_info(&self, file_id: String) -> Result<File> {
        let file = self.client().get_file(&file_id).await?;
        let uri = file
            .file_path
            .as_deref()
            .map(|path| Uri::from_str(&self.client().file_url(path)))
            .transpose()
            .context("Telegram returned an invalid file URL")?;
        Ok(File {
            id: Some(file.file_id),
            name: file
                .file_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
                .unwrap_or_default()
                .to_owned(),
            uri,
            base64: None,
            mime: file
                .file_path
                .as_deref()
                .and_then(|path| mime_guess::from_path(path).first()),
            size: file.file_size,
        })
    }
}

impl TelegramBot {
    async fn call<T: DeserializeOwned>(&self, method: &str, mut payload: Value) -> Result<T> {
        strip_nulls(&mut payload);
        self.client().call(method, &payload).await
    }

    async fn telegram_file_uri(&self, file_id: &str) -> Result<Option<Uri>> {
        let file = self.client().get_file(file_id).await?;
        file.file_path
            .map(|path| Uri::from_str(&self.client().file_url(&path)))
            .transpose()
            .context("Telegram returned an invalid file URL")
    }
}

async fn attach_caption_or_send_text(
    bot: &TelegramBot,
    chat_id: &str,
    outgoing: &mut OutgoingMessage,
    reply: &mut Option<ReplyParameters>,
    sent: &mut Vec<SendMessageResponse>,
) -> Result<()> {
    if outgoing.text.is_empty() {
        return Ok(());
    }

    if outgoing.entities.is_empty() {
        if let Some(first_media) = outgoing.media.first_mut() {
            let caption = match first_media.caption.take() {
                Some(caption) => format!("{}\n{caption}", outgoing.text),
                None => outgoing.text.clone(),
            };
            if utf16_len(&caption) <= CAPTION_LIMIT {
                first_media.caption = Some(caption);
                return Ok(());
            }
        }
    }

    send_text(
        bot,
        chat_id,
        std::mem::take(&mut outgoing.text),
        std::mem::take(&mut outgoing.entities),
        reply,
        sent,
    )
    .await
}

async fn send_text(
    bot: &TelegramBot,
    chat_id: &str,
    text: String,
    entities: Vec<MessageEntity>,
    reply: &mut Option<ReplyParameters>,
    sent: &mut Vec<SendMessageResponse>,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    if utf16_len(&text) > TEXT_LIMIT && !entities.is_empty() {
        return Err(anyhow::anyhow!(
            "Telegram text with entities exceeds {TEXT_LIMIT} UTF-16 code units and cannot be split safely"
        ));
    }

    let chunks = split_utf16(&text, TEXT_LIMIT);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let response: Message = bot
            .call(
                "sendMessage",
                json!({
                    "chat_id": chat_id_value(chat_id),
                    "text": chunk,
                    "entities": if index == 0 { optional_entities(entities.clone()) } else { None },
                    "reply_parameters": reply.take(),
                }),
            )
            .await?;
        sent.push(sent_response(response));
    }
    Ok(())
}

async fn send_media(
    bot: &TelegramBot,
    chat_id: &str,
    media: Vec<OutgoingMedia>,
    reply: &mut Option<ReplyParameters>,
    sent: &mut Vec<SendMessageResponse>,
) -> Result<()> {
    let mut index = 0;
    while index < media.len() {
        let class = media[index].kind.group_class();
        let end = media[index..]
            .iter()
            .position(|item| item.kind.group_class() != class)
            .map(|offset| index + offset)
            .unwrap_or(media.len());
        for chunk in media[index..end].chunks(MEDIA_GROUP_LIMIT) {
            if chunk.len() == 1 {
                let response =
                    send_single_media(bot, chat_id, chunk[0].clone(), reply.take()).await?;
                sent.push(sent_response(response));
            } else {
                let responses =
                    send_media_group(bot, chat_id, chunk.to_vec(), reply.take()).await?;
                sent.extend(responses.into_iter().map(sent_response));
            }
        }
        index = end;
    }
    Ok(())
}

async fn send_single_media(
    bot: &TelegramBot,
    chat_id: &str,
    media: OutgoingMedia,
    reply: Option<ReplyParameters>,
) -> Result<Message> {
    anyhow::ensure!(
        media.caption.as_deref().map(utf16_len).unwrap_or_default() <= CAPTION_LIMIT,
        "Telegram media caption exceeds {CAPTION_LIMIT} UTF-16 code units"
    );
    let method = match media.kind {
        MediaKind::Photo => "sendPhoto",
        MediaKind::Video => "sendVideo",
        MediaKind::Audio => "sendAudio",
        MediaKind::Document => "sendDocument",
    };
    let kind = media.kind;
    match resolve_file(media.file).await? {
        ResolvedFile::Remote(file) => {
            bot.call(
                method,
                json!({
                    "chat_id": chat_id_value(chat_id),
                    kind.field_name(): file,
                    "caption": media.caption,
                    "duration": media.duration,
                    "reply_parameters": reply,
                }),
            )
            .await
        }
        ResolvedFile::Upload(upload) => {
            let mut form = Form::new()
                .text("chat_id", chat_id.to_owned())
                .part(kind.field_name(), upload.into_part()?);
            form = form_optional_text(form, "caption", media.caption);
            form = form_optional_text(
                form,
                "duration",
                media.duration.map(|value| value.to_string()),
            );
            form = form_optional_json(form, "reply_parameters", reply)?;
            bot.client().call_multipart(method, form).await
        }
    }
}

async fn send_media_group(
    bot: &TelegramBot,
    chat_id: &str,
    media: Vec<OutgoingMedia>,
    reply: Option<ReplyParameters>,
) -> Result<Vec<Message>> {
    let mut input_media = Vec::with_capacity(media.len());
    let mut uploads = Vec::new();
    for (index, media) in media.into_iter().enumerate() {
        anyhow::ensure!(
            media.caption.as_deref().map(utf16_len).unwrap_or_default() <= CAPTION_LIMIT,
            "Telegram media caption exceeds {CAPTION_LIMIT} UTF-16 code units"
        );
        let source = match resolve_file(media.file).await? {
            ResolvedFile::Remote(file) => file,
            ResolvedFile::Upload(upload) => {
                let name = format!("media{index}");
                uploads.push((name.clone(), upload));
                format!("attach://{name}")
            }
        };
        let mut value = json!({
            "type": media.kind.telegram_type(),
            "media": source,
            "caption": media.caption,
        });
        if let Some(duration) = media.duration {
            value["duration"] = duration.into();
        }
        strip_nulls(&mut value);
        input_media.push(value);
    }

    if uploads.is_empty() {
        bot.call(
            "sendMediaGroup",
            json!({
                "chat_id": chat_id_value(chat_id),
                "media": input_media,
                "reply_parameters": reply,
            }),
        )
        .await
    } else {
        let mut form = Form::new()
            .text("chat_id", chat_id.to_owned())
            .text("media", serde_json::to_string(&input_media)?);
        form = form_optional_json(form, "reply_parameters", reply)?;
        for (name, upload) in uploads {
            form = form.part(name, upload.into_part()?);
        }
        bot.client().call_multipart("sendMediaGroup", form).await
    }
}

async fn edit_media(
    bot: &TelegramBot,
    chat_id: &str,
    message_id: i64,
    media: OutgoingMedia,
    caption_entities: Vec<MessageEntity>,
) -> Result<()> {
    let kind = media.kind;
    let mut input_media = json!({
        "type": kind.telegram_type(),
        "caption": media.caption,
        "caption_entities": optional_entities(caption_entities),
    });
    if let Some(duration) = media.duration {
        input_media["duration"] = duration.into();
    }
    strip_nulls(&mut input_media);
    match resolve_file(media.file).await? {
        ResolvedFile::Remote(file) => {
            input_media["media"] = file.into();
            let _: Message = bot
                .call(
                    "editMessageMedia",
                    json!({
                        "chat_id": chat_id_value(chat_id),
                        "message_id": message_id,
                        "media": input_media,
                    }),
                )
                .await?;
        }
        ResolvedFile::Upload(upload) => {
            input_media["media"] = "attach://media_file".into();
            let form = Form::new()
                .text("chat_id", chat_id.to_owned())
                .text("message_id", message_id.to_string())
                .text("media", serde_json::to_string(&input_media)?)
                .part("media_file", upload.into_part()?);
            let _: Message = bot
                .client()
                .call_multipart("editMessageMedia", form)
                .await?;
        }
    }
    Ok(())
}

enum ResolvedFile {
    Remote(String),
    Upload(Upload),
}

async fn resolve_file(file: File) -> Result<ResolvedFile> {
    if let Some(id) = file.id {
        return Ok(ResolvedFile::Remote(id));
    }
    if let Some(base64) = file.base64 {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .context("invalid base64 media data")?;
        return Ok(ResolvedFile::Upload(Upload {
            name: nonempty_name(file.name),
            bytes,
            mime: file.mime.map(|mime| mime.to_string()),
        }));
    }
    let uri = file
        .uri
        .ok_or_else(|| anyhow::anyhow!("media file has no Telegram file_id, URI, or data"))?;
    match uri.scheme_str() {
        Some("http" | "https") => Ok(ResolvedFile::Remote(uri.to_string())),
        Some("file") => {
            let path = uri.path();
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("failed to read media file {path:?}"))?;
            Ok(ResolvedFile::Upload(Upload {
                name: nonempty_name(file.name),
                bytes,
                mime: file.mime.map(|mime| mime.to_string()),
            }))
        }
        None => {
            let path = uri.path();
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("failed to read media file {path:?}"))?;
            Ok(ResolvedFile::Upload(Upload {
                name: nonempty_name(file.name),
                bytes,
                mime: file.mime.map(|mime| mime.to_string()),
            }))
        }
        Some(scheme) => Err(anyhow::anyhow!("unsupported media URI scheme {scheme:?}")),
    }
}

fn nonempty_name(name: String) -> String {
    if name.is_empty() {
        "upload.bin".to_owned()
    } else {
        name
    }
}

fn chat_permissions(allow: bool) -> Value {
    json!({
        "can_send_messages": allow,
        "can_send_audios": allow,
        "can_send_documents": allow,
        "can_send_photos": allow,
        "can_send_videos": allow,
        "can_send_video_notes": allow,
        "can_send_voice_notes": allow,
        "can_send_polls": allow,
        "can_send_other_messages": allow,
        "can_add_web_page_previews": allow,
        "can_react_to_messages": allow,
        // Muting and unmuting must not accidentally grant group-management
        // capabilities that the member did not previously have.
        "can_change_info": false,
        "can_invite_users": false,
        "can_pin_messages": false,
        "can_manage_topics": false,
        "can_edit_tag": false,
    })
}

fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, value| !value.is_null());
            for value in object.values_mut() {
                strip_nulls(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                strip_nulls(value);
            }
        }
        _ => {}
    }
}

fn platform_form_value(value: Value) -> Result<String> {
    match value {
        Value::String(value) => Ok(value),
        Value::Null => Err(anyhow::anyhow!("null multipart parameters must be omitted")),
        value => serde_json::to_string(&value).context("failed to encode multipart parameter"),
    }
}

async fn avatar_upload(uri: &Uri) -> Result<Upload> {
    let name = uri
        .path()
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("avatar.jpg")
        .to_owned();
    let bytes = match uri.scheme_str() {
        Some("http" | "https") => {
            let response = reqwest::get(uri.to_string())
                .await
                .context("failed to download group avatar")?
                .error_for_status()
                .context("group avatar download returned an error")?;
            if let Some(length) = response.content_length() {
                anyhow::ensure!(
                    length <= MAX_AVATAR_BYTES,
                    "group avatar is larger than 10 MiB"
                );
            }
            let bytes = response
                .bytes()
                .await
                .context("failed to read group avatar")?;
            anyhow::ensure!(
                bytes.len() as u64 <= MAX_AVATAR_BYTES,
                "group avatar is larger than 10 MiB"
            );
            bytes.to_vec()
        }
        Some("file") | None => tokio::fs::read(uri.path())
            .await
            .with_context(|| format!("failed to read group avatar {:?}", uri.path()))?,
        Some(scheme) => return Err(anyhow::anyhow!("unsupported avatar URI scheme {scheme:?}")),
    };
    Ok(Upload {
        mime: mime_guess::from_path(&name)
            .first()
            .map(|mime| mime.to_string()),
        name,
        bytes,
    })
}

fn optional_entities(entities: Vec<MessageEntity>) -> Option<Vec<MessageEntity>> {
    (!entities.is_empty()).then_some(entities)
}

fn form_optional_text(form: Form, name: &'static str, value: Option<String>) -> Form {
    match value {
        Some(value) => form.text(name, value),
        None => form,
    }
}

fn form_optional_json<T: serde::Serialize>(
    form: Form,
    name: &'static str,
    value: Option<T>,
) -> Result<Form> {
    match value {
        Some(value) => Ok(form.text(name, serde_json::to_string(&value)?)),
        None => Ok(form),
    }
}

fn sent_response(message: Message) -> SendMessageResponse {
    SendMessageResponse {
        sent_message_id: format!("{}_{}", message.chat.id, message.message_id),
    }
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn split_utf16(value: &str, limit: usize) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;
    for character in value.chars() {
        let len = character.len_utf16();
        if current_len + len > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
        current.push(character);
        current_len += len;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;
    use crate::segment::MediaGroupClass;

    static TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    async fn mock_telegram_server(
        responses: Vec<&'static str>,
    ) -> Result<(String, JoinHandle<Vec<Vec<u8>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(responses.len());
            for response_body in responses {
                let (mut stream, _) = listener.accept().await.expect("mock accept failed");
                let mut request = Vec::new();
                let expected_length = loop {
                    let mut buffer = [0; 4096];
                    let read = stream.read(&mut buffer).await.expect("mock read failed");
                    assert!(read > 0, "request ended before its headers");
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(header_end) =
                        request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..header_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .expect("mock requests must include Content-Length");
                        break header_end + 4 + content_length;
                    }
                };
                while request.len() < expected_length {
                    let mut buffer = [0; 4096];
                    let read = stream
                        .read(&mut buffer)
                        .await
                        .expect("mock body read failed");
                    assert!(read > 0, "request ended before its declared body length");
                    request.extend_from_slice(&buffer[..read]);
                }
                requests.push(request);

                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("mock response failed");
            }
            requests
        });
        Ok((format!("http://{address}"), server))
    }

    fn temporary_test_file() -> PathBuf {
        std::env::temp_dir().join(format!(
            "telegram-bot-oxidebot-{}-{}-stream.txt",
            std::process::id(),
            TEST_FILE_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn text_split_respects_utf16_boundaries() {
        let chunks = split_utf16("😀😀a", 3);
        assert_eq!(chunks, ["😀", "😀a"]);
        assert!(chunks.iter().all(|chunk| utf16_len(chunk) <= 3));
    }

    #[test]
    fn media_classes_match_telegram_album_rules() {
        assert_eq!(MediaKind::Photo.group_class(), MediaGroupClass::Visual);
        assert_eq!(MediaKind::Video.group_class(), MediaGroupClass::Visual);
        assert_eq!(MediaKind::Audio.group_class(), MediaGroupClass::Audio);
        assert_eq!(MediaKind::Document.group_class(), MediaGroupClass::Document);
    }

    #[test]
    fn permissions_include_bot_api_10_reaction_permission() {
        assert_eq!(chat_permissions(false)["can_react_to_messages"], false);
        assert_eq!(chat_permissions(true)["can_send_messages"], true);
        assert_eq!(chat_permissions(true)["can_change_info"], false);
        assert_eq!(chat_permissions(true)["can_edit_tag"], false);
    }

    #[test]
    fn optional_json_fields_are_omitted_instead_of_sent_as_null() {
        let mut value = json!({
            "text": "hello",
            "reply_parameters": null,
            "nested": {"caption": null, "type": "photo"}
        });
        strip_nulls(&mut value);
        assert_eq!(value, json!({"text": "hello", "nested": {"type": "photo"}}));
    }

    #[test]
    fn platform_form_values_use_telegram_encoding() {
        assert_eq!(platform_form_value(json!(42)).unwrap(), "42");
        assert_eq!(platform_form_value(json!(true)).unwrap(), "true");
        assert_eq!(
            platform_form_value(json!({"type": "photo"})).unwrap(),
            r#"{"type":"photo"}"#
        );
        assert!(platform_form_value(Value::Null).is_err());
    }

    #[tokio::test]
    async fn platform_api_calls_json_and_streams_path_attachments() -> Result<()> {
        let (api_base, server) = mock_telegram_server(vec![
            r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Bot"}}"#,
            r#"{"ok":true,"result":{"future":true}}"#,
            r#"{"ok":true,"result":{"uploaded":true}}"#,
            r#"{"ok":false,"error_code":429,"description":"slow down","parameters":{"migrate_to_chat_id":-1001,"retry_after":5}}"#,
        ])
        .await?;
        let bot = TelegramBot::try_with_api_base("123:test", api_base, Default::default()).await?;

        let response = bot
            .call_platform_api(
                PlatformApiRequest::new("future_method_1")
                    .parameters(json!({"known": true, "omitted": null})),
            )
            .await?;
        assert_eq!(response.result, json!({"future": true}));

        let path = temporary_test_file();
        tokio::fs::write(&path, b"streamed-body").await?;
        let attachment = PlatformApiFile::from_path("payload", &path).await?;
        let response = bot
            .call_platform_api(
                PlatformApiRequest::new("future_upload_1")
                    .parameters(json!({"file": "attach://payload"}))
                    .file(attachment),
            )
            .await?;
        assert_eq!(response.result, json!({"uploaded": true}));
        tokio::fs::remove_file(&path).await?;

        let error = bot
            .call_platform_api(PlatformApiRequest::new("future_rate_limited_1"))
            .await
            .unwrap_err();
        let error = error
            .downcast_ref::<crate::TelegramApiError>()
            .expect("Telegram API errors must remain downcastable");
        assert_eq!(error.parameters.as_ref().unwrap().retry_after, Some(5));

        let requests = server.await?;
        let json_request = String::from_utf8_lossy(&requests[1]);
        assert!(json_request.starts_with("POST /bot123:test/future_method_1 "));
        assert!(json_request.ends_with(r#"{"known":true}"#));
        let upload_request = String::from_utf8_lossy(&requests[2]);
        assert!(upload_request.starts_with("POST /bot123:test/future_upload_1 "));
        assert!(upload_request.contains("name=\"payload\""));
        assert!(upload_request.contains("streamed-body"));
        Ok(())
    }
}
