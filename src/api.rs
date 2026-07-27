use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr as _,
    time::Duration,
};

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
    application::{
        AppSurface, AppSurfaceKind, BotProfile, CommandContext, CommandDefinition, CommandKind,
        Localized, MiniAppEvent, Suggestion, SuggestionRequest, SurfaceContent,
    },
    capability::{
        ApplicationCapabilities, BotCapabilities, ButtonCapabilities, CollaborationCapabilities,
        ComponentCapabilities, ContentCapabilities, ConversationCapabilities, DeliveryCapabilities,
        InteractionLifecycleCapabilities, PlatformLimits, SupportLevel,
    },
    collaboration::{ChatActivity, PinOptions, PinnedMessage, Reaction, ReactionOptions},
    commerce::{CheckoutRequest, Invoice, InvoiceOptions, LineItem, Payment, ShippingOption},
    content::{
        Checklist, DeliveryTime, ForwardOptions, LayoutNode, LinkPreviewOptions, Media,
        MediaGalleryItem, MediaType, MessageContent, MessageEnvelope, MessageQuery,
        MessageVisibility, NotificationPolicy, OutgoingMessage, Poll, PollOption, PollType,
        ReplyOptions, RichLayout, RichText, TextStyle,
    },
    conversation::{
        ConversationKind, ConversationMember, ConversationPermission, ConversationProfile,
        ConversationRef, InviteLink, InviteLinkOptions, MessageRef, MessageTarget, Page,
        PageRequest, PermissionSet, RoleRef, Thread, ThreadOptions, ThreadState,
    },
    interaction::{
        ActionRow, BotCommand, BotCommandQuery, BotCommandSet, Button, ButtonAction, ButtonIcon,
        ButtonStyle, ChatMenu, CommandScope, InlineKeyboard, InlineQueryTarget,
        InteractionCapabilities, InteractionComponent, InteractionNotificationStyle,
        InteractionResponse, InteractionResponseHandle, InteractionVisibility, MessageComponents,
        MessageOptions, PlatformNativeData, PollKind, ReplyButton, ReplyButtonAction,
        ReplyKeyboard, UnsupportedInteractionError,
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
use serde_json::{json, Map, Value};

use crate::{
    bot::TelegramBot,
    segment::{
        chat_id_value, process_message_segments, MediaKind, OutgoingMedia,
        OutgoingMessage as LegacyOutgoingMessage, ReplyParameters,
    },
    telegram::{ChatFullInfo, ChatMember, Message, MessageEntity, Upload},
    utils::{parse_user, split_id, split_join_request_id, telegram_user_name},
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

    fn interaction_capabilities(&self) -> InteractionCapabilities {
        InteractionCapabilities {
            inline_buttons: true,
            reply_keyboard: true,
            select_menus: false,
            modals: false,
            commands: true,
            chat_menu: true,
            web_apps: true,
            update_components: true,
        }
    }

    fn bot_capabilities(&self) -> BotCapabilities {
        use SupportLevel::{Emulated, Native, Unsupported};

        BotCapabilities {
            content: ContentCapabilities {
                plain_text: Native,
                rich_text: Native,
                rich_layout: Native,
                images: Native,
                video: Native,
                audio: Native,
                animation: Native,
                voice_notes: Native,
                video_notes: Native,
                files: Native,
                media_galleries: Native,
                location: Native,
                contacts: Native,
                stickers: Native,
                custom_emoji: Native,
                polls: Native,
                quizzes: Native,
                checklists: Native,
                platform_native: Native,
            },
            delivery: DeliveryCapabilities {
                replies: Native,
                quoted_replies: Native,
                threads: Native,
                silent: Native,
                forced_notification: Emulated,
                protected_content: Native,
                link_preview_control: Native,
                mention_control: Unsupported,
                ephemeral: Native,
                private_to_users: Native,
                scheduling: Unsupported,
                drafts: Native,
                idempotency_keys: Unsupported,
                client_message_ids: Unsupported,
                metadata: Unsupported,
                supported_visibilities: vec![
                    MessageVisibility::Public,
                    MessageVisibility::Ephemeral,
                    MessageVisibility::PrivateTo(Vec::new()),
                ],
            },
            components: ComponentCapabilities {
                inline_keyboard: Native,
                reply_keyboard: Native,
                selects: Unsupported,
                text_input: Unsupported,
                number_input: Unsupported,
                checkbox: Unsupported,
                checkbox_group: Unsupported,
                radio_group: Unsupported,
                toggle: Unsupported,
                date_picker: Unsupported,
                time_picker: Unsupported,
                datetime_picker: Unsupported,
                file_upload: Unsupported,
                dynamic_suggestions: Unsupported,
                modals: Unsupported,
                platform_native: Native,
                buttons: ButtonCapabilities {
                    callback: Native,
                    send_text: Unsupported,
                    url: Native,
                    web_app: Native,
                    login: Native,
                    switch_inline_query: Native,
                    copy_text: Native,
                    game: Native,
                    pay: Native,
                    disabled: Unsupported,
                    icons: Native,
                    styles: [
                        ButtonStyle::Default,
                        ButtonStyle::Primary,
                        ButtonStyle::Success,
                        ButtonStyle::Danger,
                    ]
                    .into_iter()
                    .collect(),
                    max_callback_bytes: Some(64),
                    max_buttons_per_row: None,
                    max_rows: None,
                },
            },
            interaction_lifecycle: InteractionLifecycleCapabilities {
                acknowledge: Native,
                defer: Emulated,
                notifications: Native,
                open_url: Native,
                initial_message: Emulated,
                update_original: Emulated,
                followups: Emulated,
                edit_original: Emulated,
                delete_original: Emulated,
                validation_errors: Unsupported,
                view_navigation: Unsupported,
            },
            conversations: ConversationCapabilities {
                direct: Native,
                groups: Native,
                channels: Native,
                threads: Native,
                topics: Native,
                forums: Native,
                create_threads: Native,
                manage_threads: Native,
                history: Emulated,
                search: Unsupported,
                members: Emulated,
                roles: Emulated,
                member_tags: Native,
                permissions: Native,
                moderation: Native,
                join_requests: Native,
                invite_links: Native,
                profile: Native,
                leave: Native,
            },
            collaboration: CollaborationCapabilities {
                reactions: Native,
                multiple_reactions: Unsupported,
                reaction_users: Unsupported,
                pins: Native,
                typing: Native,
                read_receipts: Native,
                call_events: Native,
                call_management: Unsupported,
                forwarding: Native,
                copying: Native,
                batch_send: Native,
                bulk_delete: Native,
                broadcast: Native,
            },
            application: ApplicationCapabilities {
                commands: Native,
                structured_commands: Emulated,
                command_localizations: Native,
                autocomplete: Native,
                chat_menu: Native,
                home_surface: Unsupported,
                rich_menu: Unsupported,
                mini_apps: Native,
                payments: Native,
                subscriptions: Native,
            },
            limits: PlatformLimits {
                max_text_length: Some(TEXT_LIMIT),
                max_caption_length: Some(CAPTION_LIMIT),
                max_rich_text_length: None,
                max_media_per_message: Some(MEDIA_GROUP_LIMIT),
                max_file_bytes: None,
                max_components: None,
                max_commands: Some(100),
                max_poll_options: Some(12),
                max_batch_size: Some(100),
                edit_window_seconds: None,
                supported_mime_types: Vec::new(),
                platform_limits: BTreeMap::new(),
            },
        }
    }

    async fn send_message_with_options(
        &self,
        message: Vec<MessageSegment>,
        target: SendMessageTarget,
        options: MessageOptions,
    ) -> Result<Vec<SendMessageResponse>> {
        send_message_impl(self, message, target, options).await
    }

    async fn send_outgoing_message(
        &self,
        target: MessageTarget,
        message: OutgoingMessage,
    ) -> Result<Vec<MessageRef>> {
        telegram_send_outgoing(self, target, message).await
    }

    async fn edit_outgoing_message(
        &self,
        message: MessageRef,
        new_message: OutgoingMessage,
    ) -> Result<()> {
        telegram_edit_outgoing(self, message, new_message).await
    }

    async fn delete_message_ref(&self, message: MessageRef) -> Result<()> {
        let (payload, address) = telegram_edit_address(&message)?;
        let method = match address {
            TelegramEditAddress::Regular => "deleteMessage",
            TelegramEditAddress::Ephemeral => "deleteEphemeralMessage",
            TelegramEditAddress::Inline => {
                return Err(
                    UnsupportedInteractionError::new("deleting Telegram inline messages").into(),
                )
            }
        };
        let _: bool = self.call(method, Value::Object(payload)).await?;
        Ok(())
    }

    async fn delete_messages(&self, messages: Vec<MessageRef>) -> Result<()> {
        anyhow::ensure!(!messages.is_empty(), "no Telegram messages to delete");
        let mut grouped = BTreeMap::<String, Vec<i64>>::new();
        let mut individual = Vec::new();
        for message in messages {
            if message.platform_data.as_ref().is_some_and(|native| {
                native.data.get("receiver_user_id").is_some()
                    || native.data.get("ephemeral_message_id").is_some()
                    || native.data.get("inline_message_id").is_some()
            }) {
                individual.push(message);
                continue;
            }
            let (chat_id, message_id) = telegram_message_ref(&message)?;
            grouped.entry(chat_id).or_default().push(message_id);
        }
        for (chat_id, message_ids) in grouped {
            for chunk in message_ids.chunks(100) {
                let _: bool = self
                    .call(
                        "deleteMessages",
                        json!({"chat_id": chat_id_value(&chat_id), "message_ids": chunk}),
                    )
                    .await?;
            }
        }
        for message in individual {
            self.delete_message_ref(message).await?;
        }
        Ok(())
    }

    async fn edit_message_components(
        &self,
        message_id: String,
        components: Option<MessageComponents>,
    ) -> Result<()> {
        let (chat_id, message_id) = split_id(message_id)?;
        let reply_markup = match components {
            Some(components) => telegram_editable_components(components)?,
            None => json!({"inline_keyboard": []}),
        };
        let _: Value = self
            .call(
                "editMessageReplyMarkup",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "message_id": message_id,
                    "reply_markup": reply_markup,
                }),
            )
            .await?;
        Ok(())
    }

    async fn answer_interaction(
        &self,
        interaction_id: String,
        response: InteractionResponse,
    ) -> Result<()> {
        anyhow::ensure!(!interaction_id.is_empty(), "interaction id cannot be empty");
        let mut payload = match response {
            InteractionResponse::Acknowledge => json!({}),
            InteractionResponse::Notification {
                text,
                style,
                cache_time,
            } => {
                anyhow::ensure!(
                    text.chars().count() <= 200,
                    "Telegram callback notification text is limited to 200 characters"
                );
                json!({
                    "text": text,
                    "show_alert": matches!(style, InteractionNotificationStyle::Alert),
                    "cache_time": cache_time.map(|duration| duration.as_secs()),
                })
            }
            InteractionResponse::OpenUrl { url, cache_time } => {
                anyhow::ensure!(!url.is_empty(), "interaction URL cannot be empty");
                json!({
                    "url": url,
                    "cache_time": cache_time.map(|duration| duration.as_secs()),
                })
            }
            InteractionResponse::PlatformNative(native) => {
                telegram_native_object(native, "interaction response")?
            }
            InteractionResponse::Message { .. }
            | InteractionResponse::UpdateMessage { .. }
            | InteractionResponse::OpenModal(_)
            | InteractionResponse::Defer { .. }
            | InteractionResponse::ValidationErrors(_)
            | InteractionResponse::Navigate(_)
            | InteractionResponse::CloseView => {
                return Err(UnsupportedInteractionError::new(
                    "this Telegram callback response type",
                )
                .into())
            }
        };
        payload["callback_query_id"] = interaction_id.into();
        let _: bool = self.call("answerCallbackQuery", payload).await?;
        Ok(())
    }

    async fn defer_interaction(
        &self,
        handle: InteractionResponseHandle,
        _visibility: InteractionVisibility,
    ) -> Result<()> {
        self.answer_interaction(handle.id, InteractionResponse::Acknowledge)
            .await
    }

    async fn send_interaction_followup(
        &self,
        handle: InteractionResponseHandle,
        mut message: OutgoingMessage,
    ) -> Result<Vec<MessageRef>> {
        let native = telegram_native_object(
            handle
                .platform_data
                .context("Telegram interaction follow-up requires response context")?,
            "interaction response context",
        )?;
        let fields = native
            .as_object()
            .context("Telegram interaction response context must be an object")?;
        let chat_id = fields
            .get("chat_id")
            .and_then(|value| {
                value
                    .as_i64()
                    .map(|id| id.to_string())
                    .or_else(|| value.as_str().map(str::to_owned))
            })
            .context("Telegram interaction follow-up requires chat_id")?;
        let conversation = ConversationRef::new(
            chat_id,
            fields
                .get("chat_type")
                .and_then(Value::as_str)
                .map(telegram_conversation_kind)
                .unwrap_or(ConversationKind::Unknown),
        );
        let mut target = MessageTarget::new(conversation);
        if matches!(message.options.visibility, MessageVisibility::Ephemeral) {
            let user_id = fields
                .get("user_id")
                .and_then(Value::as_i64)
                .context("Telegram ephemeral follow-up requires user_id")?;
            target.recipients.push(user_id.to_string());
            target.platform_data = Some(PlatformNativeData::new(
                crate::SERVER,
                json!({"callback_query_id": handle.id}),
            ));
        }
        if message.options.notification == NotificationPolicy::Force {
            message.options.notification = NotificationPolicy::Default;
        }
        self.send_outgoing_message(target, message).await
    }

    async fn edit_interaction_response(
        &self,
        handle: InteractionResponseHandle,
        message: OutgoingMessage,
    ) -> Result<()> {
        let reference = telegram_interaction_message_ref(&handle)?;
        self.edit_outgoing_message(reference, message).await
    }

    async fn delete_interaction_response(&self, handle: InteractionResponseHandle) -> Result<()> {
        let reference = telegram_interaction_message_ref(&handle)?;
        let (payload, address) = telegram_edit_address(&reference)?;
        match address {
            TelegramEditAddress::Ephemeral => {
                let _: bool = self
                    .call("deleteEphemeralMessage", Value::Object(payload))
                    .await?;
            }
            TelegramEditAddress::Regular => {
                let _: bool = self.call("deleteMessage", Value::Object(payload)).await?;
            }
            TelegramEditAddress::Inline => {
                return Err(
                    UnsupportedInteractionError::new("deleting Telegram inline messages").into(),
                )
            }
        }
        Ok(())
    }

    async fn set_bot_commands(&self, commands: BotCommandSet) -> Result<()> {
        let BotCommandSet {
            commands,
            scope,
            language_code,
        } = commands;
        validate_commands(&commands)?;
        validate_language_code(language_code.as_deref())?;
        anyhow::ensure!(
            commands.len() <= 100,
            "Telegram supports at most 100 bot commands per scope"
        );
        let commands = commands
            .into_iter()
            .map(|command| {
                json!({
                    "command": command.command,
                    "description": command.description,
                    "is_ephemeral": command.is_ephemeral,
                })
            })
            .collect::<Vec<_>>();
        let _: bool = self
            .call(
                "setMyCommands",
                json!({
                    "commands": commands,
                    "scope": telegram_command_scope(&scope)?,
                    "language_code": language_code,
                }),
            )
            .await?;
        Ok(())
    }

    async fn get_bot_commands(&self, query: BotCommandQuery) -> Result<Vec<BotCommand>> {
        validate_language_code(query.language_code.as_deref())?;
        let response: Vec<TelegramBotCommand> = self
            .call(
                "getMyCommands",
                json!({
                    "scope": telegram_command_scope(&query.scope)?,
                    "language_code": query.language_code,
                }),
            )
            .await?;
        Ok(response
            .into_iter()
            .map(|command| {
                BotCommand::new(command.command, command.description)
                    .ephemeral(command.is_ephemeral)
            })
            .collect())
    }

    async fn delete_bot_commands(&self, query: BotCommandQuery) -> Result<()> {
        validate_language_code(query.language_code.as_deref())?;
        let _: bool = self
            .call(
                "deleteMyCommands",
                json!({
                    "scope": telegram_command_scope(&query.scope)?,
                    "language_code": query.language_code,
                }),
            )
            .await?;
        Ok(())
    }

    async fn set_chat_menu(&self, chat_id: Option<String>, menu: ChatMenu) -> Result<()> {
        let _: bool = self
            .call(
                "setChatMenuButton",
                json!({
                    "chat_id": optional_private_chat_id(chat_id)?,
                    "menu_button": telegram_chat_menu(menu)?,
                }),
            )
            .await?;
        Ok(())
    }

    async fn get_chat_menu(&self, chat_id: Option<String>) -> Result<ChatMenu> {
        let menu: Value = self
            .call(
                "getChatMenuButton",
                json!({"chat_id": optional_private_chat_id(chat_id)?}),
            )
            .await?;
        telegram_chat_menu_response(menu)
    }

    async fn send_message(
        &self,
        message: Vec<MessageSegment>,
        target: SendMessageTarget,
    ) -> Result<Vec<SendMessageResponse>> {
        send_message_impl(self, message, target, MessageOptions::default()).await
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
        anyhow::ensure!(
            reaction_id != "paid",
            "Telegram bots cannot set paid reactions"
        );
        let reaction = if reaction_id
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

    async fn set_message_reactions(
        &self,
        message: MessageRef,
        reactions: Vec<Reaction>,
    ) -> Result<()> {
        anyhow::ensure!(
            reactions.len() <= 1,
            "Telegram bots can set at most one reaction per message"
        );
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let reactions = reactions
            .into_iter()
            .map(telegram_reaction)
            .collect::<Result<Vec<_>>>()?;
        let _: bool = self
            .call(
                "setMessageReaction",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "message_id": message_id,
                    "reaction": reactions,
                }),
            )
            .await?;
        Ok(())
    }

    async fn add_message_reaction(
        &self,
        message: MessageRef,
        reaction: Reaction,
        options: ReactionOptions,
    ) -> Result<()> {
        anyhow::ensure!(
            !matches!(reaction, Reaction::Paid),
            "Telegram bots cannot set paid reactions"
        );
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let _: bool = self
            .call(
                "setMessageReaction",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "message_id": message_id,
                    "reaction": [telegram_reaction(reaction)?],
                    "is_big": options.emphasized,
                }),
            )
            .await?;
        Ok(())
    }

    async fn remove_message_reaction(
        &self,
        message: MessageRef,
        _reaction: Reaction,
    ) -> Result<()> {
        self.set_message_reactions(message, Vec::new()).await
    }

    async fn clear_message_reactions(&self, message: MessageRef) -> Result<()> {
        self.set_message_reactions(message, Vec::new()).await
    }

    async fn pin_message(&self, message: MessageRef, options: PinOptions) -> Result<()> {
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let mut payload = telegram_chat_payload(message.conversation.as_ref(), &chat_id)?;
        payload.insert("message_id".to_owned(), message_id.into());
        payload.insert(
            "disable_notification".to_owned(),
            options.notify_members.map(|notify| !notify).into(),
        );
        let _: bool = self.call("pinChatMessage", Value::Object(payload)).await?;
        Ok(())
    }

    async fn unpin_message(&self, message: MessageRef) -> Result<()> {
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let mut payload = telegram_chat_payload(message.conversation.as_ref(), &chat_id)?;
        payload.insert("message_id".to_owned(), message_id.into());
        let _: bool = self
            .call("unpinChatMessage", Value::Object(payload))
            .await?;
        Ok(())
    }

    async fn clear_pins(&self, conversation: ConversationRef) -> Result<()> {
        let payload = telegram_conversation_payload(&conversation)?;
        let method = if matches!(
            conversation.kind,
            ConversationKind::Thread | ConversationKind::Topic
        ) {
            "unpinAllForumTopicMessages"
        } else {
            "unpinAllChatMessages"
        };
        let _: bool = self.call(method, Value::Object(payload)).await?;
        Ok(())
    }

    async fn list_pins(
        &self,
        conversation: ConversationRef,
        page: PageRequest,
    ) -> Result<Page<PinnedMessage>> {
        anyhow::ensure!(
            page.cursor.is_none(),
            "Telegram pinned-message lookup does not support cursors"
        );
        let chat: Value = self
            .call(
                "getChat",
                Value::Object(telegram_conversation_payload(&conversation)?),
            )
            .await?;
        let items = chat
            .get("pinned_message")
            .and_then(|message| message.get("message_id"))
            .and_then(Value::as_i64)
            .map(|message_id| PinnedMessage {
                message: MessageRef::new(message_id.to_string())
                    .in_conversation(conversation.clone()),
                pinned_by: None,
                pinned_at: None,
                platform_data: chat
                    .get("pinned_message")
                    .cloned()
                    .map(|message| PlatformNativeData::new(crate::SERVER, message)),
            })
            .into_iter()
            .take(page.limit.unwrap_or(1).min(1) as usize)
            .collect();
        Ok(Page {
            items,
            next_cursor: None,
            previous_cursor: None,
            total: None,
        })
    }

    async fn set_chat_activity(
        &self,
        conversation: ConversationRef,
        activity: ChatActivity,
        duration: Option<Duration>,
    ) -> Result<()> {
        anyhow::ensure!(
            duration.is_none_or(|duration| duration <= Duration::from_secs(5)),
            "one Telegram chat action lasts at most 5 seconds"
        );
        let action = match activity {
            ChatActivity::Typing => "typing",
            ChatActivity::UploadingPhoto => "upload_photo",
            ChatActivity::UploadingVideo => "upload_video",
            ChatActivity::UploadingAudio => "upload_voice",
            ChatActivity::UploadingFile => "upload_document",
            ChatActivity::RecordingAudio => "record_voice",
            ChatActivity::RecordingVideo => "record_video",
            ChatActivity::ChoosingSticker => "choose_sticker",
            ChatActivity::Playing => {
                return Err(UnsupportedInteractionError::new("Telegram playing activity").into())
            }
            ChatActivity::PlatformNative(action) => {
                let mut payload = telegram_conversation_payload(&conversation)?;
                payload.insert("action".to_owned(), action.into());
                let _: bool = self.call("sendChatAction", Value::Object(payload)).await?;
                return Ok(());
            }
        };
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert("action".to_owned(), action.into());
        let _: bool = self.call("sendChatAction", Value::Object(payload)).await?;
        Ok(())
    }

    async fn mark_read(
        &self,
        conversation: ConversationRef,
        through_message: Option<MessageRef>,
    ) -> Result<()> {
        let business_connection_id = conversation
            .platform_data
            .as_ref()
            .and_then(|native| native.data.get("business_connection_id"))
            .and_then(Value::as_str)
            .context("Telegram read receipts require a business_connection_id")?;
        let message = through_message.context("Telegram read receipts require a message")?;
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let chat_id = chat_id
            .parse::<i64>()
            .context("Telegram business read receipts require a numeric chat id")?;
        let _: bool = self
            .call(
                "readBusinessMessage",
                json!({
                    "business_connection_id": business_connection_id,
                    "chat_id": chat_id,
                    "message_id": message_id,
                }),
            )
            .await?;
        Ok(())
    }

    async fn stop_poll(&self, message: MessageRef) -> Result<Poll> {
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let mut payload = telegram_chat_payload(message.conversation.as_ref(), &chat_id)?;
        payload.remove("message_thread_id");
        payload.remove("direct_messages_topic_id");
        payload.insert("message_id".to_owned(), message_id.into());
        let poll: Value = self.call("stopPoll", Value::Object(payload)).await?;
        telegram_poll(poll)
    }

    async fn edit_checklist(&self, message: MessageRef, checklist: Checklist) -> Result<()> {
        let (chat_id, message_id) = telegram_message_ref(&message)?;
        let mut payload = telegram_chat_payload(message.conversation.as_ref(), &chat_id)?;
        payload.remove("message_thread_id");
        payload.remove("direct_messages_topic_id");
        anyhow::ensure!(
            payload.contains_key("business_connection_id"),
            "Telegram checklist edits require business_connection_id in the conversation"
        );
        payload.insert("message_id".to_owned(), message_id.into());
        payload.insert("checklist".to_owned(), telegram_input_checklist(checklist)?);
        let _: Message = self
            .call("editMessageChecklist", Value::Object(payload))
            .await?;
        Ok(())
    }

    async fn create_thread(
        &self,
        parent: ConversationRef,
        options: ThreadOptions,
    ) -> Result<Thread> {
        let title = options
            .title
            .clone()
            .context("Telegram forum topics require a title")?;
        anyhow::ensure!(
            options.auto_close_after.is_none()
                && options.invitable.is_none()
                && options.slow_mode.is_none(),
            "Telegram forum topics do not support auto-close, invitable, or per-topic slow-mode options"
        );
        let mut payload = telegram_conversation_payload(&parent)?;
        payload.insert("name".to_owned(), title.clone().into());
        if let Some(icon) = &options.icon {
            payload.insert("icon_custom_emoji_id".to_owned(), icon.clone().into());
        }
        if let Some(native) = options.platform_data {
            merge_native_fields(&mut payload, native, "thread options")?;
        }
        let topic: TelegramForumTopic = self
            .call("createForumTopic", Value::Object(payload))
            .await?;
        let conversation =
            ConversationRef::new(topic.message_thread_id.to_string(), ConversationKind::Topic)
                .child_of(parent);
        Ok(Thread {
            conversation,
            title: Some(topic.name),
            creator: None,
            state: ThreadState::Open,
            created_at: None,
            auto_close_at: None,
            platform_data: topic.icon_custom_emoji_id.map(|icon| {
                PlatformNativeData::new(crate::SERVER, json!({"icon_custom_emoji_id": icon}))
            }),
        })
    }

    async fn edit_thread(&self, thread: ConversationRef, options: ThreadOptions) -> Result<Thread> {
        anyhow::ensure!(
            options.auto_close_after.is_none()
                && options.invitable.is_none()
                && options.slow_mode.is_none(),
            "Telegram forum topics do not support auto-close, invitable, or per-topic slow-mode options"
        );
        let mut payload = telegram_conversation_payload(&thread)?;
        if let Some(title) = &options.title {
            payload.insert("name".to_owned(), title.clone().into());
        }
        if let Some(icon) = &options.icon {
            payload.insert("icon_custom_emoji_id".to_owned(), icon.clone().into());
        }
        if let Some(native) = options.platform_data {
            merge_native_fields(&mut payload, native, "thread options")?;
        }
        let _: bool = self.call("editForumTopic", Value::Object(payload)).await?;
        Ok(Thread {
            conversation: thread,
            title: options.title,
            creator: None,
            state: ThreadState::Open,
            created_at: None,
            auto_close_at: None,
            platform_data: None,
        })
    }

    async fn close_thread(&self, thread: ConversationRef) -> Result<()> {
        let _: bool = self
            .call(
                "closeForumTopic",
                Value::Object(telegram_conversation_payload(&thread)?),
            )
            .await?;
        Ok(())
    }

    async fn reopen_thread(&self, thread: ConversationRef) -> Result<()> {
        let _: bool = self
            .call(
                "reopenForumTopic",
                Value::Object(telegram_conversation_payload(&thread)?),
            )
            .await?;
        Ok(())
    }

    async fn delete_thread(&self, thread: ConversationRef) -> Result<()> {
        let _: bool = self
            .call(
                "deleteForumTopic",
                Value::Object(telegram_conversation_payload(&thread)?),
            )
            .await?;
        Ok(())
    }

    async fn get_conversation_profile(
        &self,
        conversation: ConversationRef,
    ) -> Result<ConversationProfile> {
        let payload = telegram_conversation_payload(&conversation)?;
        let chat: Value = self.call("getChat", Value::Object(payload.clone())).await?;
        let object = chat
            .as_object()
            .context("Telegram returned an invalid ChatFullInfo")?;
        let member_count = if matches!(
            object.get("type").and_then(Value::as_str),
            Some("group" | "supergroup" | "channel")
        ) {
            self.call::<i64>("getChatMemberCount", Value::Object(payload))
                .await
                .ok()
                .and_then(|count| u64::try_from(count).ok())
        } else {
            None
        };
        let kind = match object.get("type").and_then(Value::as_str) {
            Some("private") => ConversationKind::Direct,
            Some("group" | "supergroup") => ConversationKind::Group,
            Some("channel") => ConversationKind::Channel,
            Some(kind) => ConversationKind::PlatformNative(kind.to_owned()),
            None => conversation.kind.clone(),
        };
        let name = object
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                let first = object
                    .get("first_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let last = object
                    .get("last_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = format!("{first} {last}").trim().to_owned();
                (!name.is_empty()).then_some(name)
            });
        let avatar = chat
            .pointer("/photo/big_file_id")
            .and_then(Value::as_str)
            .map(|id| File {
                id: Some(id.to_owned()),
                ..Default::default()
            });
        let default_permissions = object
            .get("permissions")
            .and_then(Value::as_object)
            .map(telegram_permission_set);
        Ok(ConversationProfile {
            name,
            username: object
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_owned),
            description: object
                .get("description")
                .or_else(|| object.get("bio"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            avatar,
            member_count,
            kind: Some(kind),
            parent: object
                .get("linked_chat_id")
                .and_then(Value::as_i64)
                .map(|id| ConversationRef::new(id.to_string(), ConversationKind::Unknown)),
            default_permissions,
            slow_mode: object
                .get("slow_mode_delay")
                .and_then(Value::as_u64)
                .map(Duration::from_secs),
            archived: None,
            created_at: None,
            locale: None,
            metadata: BTreeMap::new(),
            platform_data: Some(PlatformNativeData::new(crate::SERVER, chat)),
        })
    }

    async fn set_conversation_profile(
        &self,
        conversation: ConversationRef,
        profile: ConversationProfile,
    ) -> Result<()> {
        let payload = telegram_conversation_payload(&conversation)?;
        anyhow::ensure!(
            profile.username.is_none()
                && profile.member_count.is_none()
                && profile.kind.is_none()
                && profile.parent.is_none()
                && profile.archived.is_none()
                && profile.created_at.is_none()
                && profile.locale.is_none()
                && profile.metadata.is_empty(),
            "Telegram cannot set read-only conversation profile fields"
        );
        let delete_photo = if let Some(native) = profile.platform_data.clone() {
            let Value::Object(mut native) = telegram_native_object(native, "conversation profile")?
            else {
                unreachable!("native object helper always returns an object")
            };
            let delete_photo = native
                .remove("delete_photo")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            anyhow::ensure!(
                native.is_empty(),
                "unsupported Telegram conversation profile fields: {:?}",
                native.keys().collect::<Vec<_>>()
            );
            delete_photo
        } else {
            false
        };
        anyhow::ensure!(
            !delete_photo || profile.avatar.is_none(),
            "Telegram conversation profile cannot set and delete the avatar together"
        );
        if let Some(name) = &profile.name {
            anyhow::ensure!(
                (1..=128).contains(&name.chars().count()),
                "Telegram chat titles must contain 1 to 128 characters"
            );
        }
        if let Some(description) = &profile.description {
            anyhow::ensure!(
                description.chars().count() <= 255,
                "Telegram chat descriptions are limited to 255 characters"
            );
        }
        if let Some(slow_mode) = profile.slow_mode {
            anyhow::ensure!(
                matches!(slow_mode.as_secs(), 0 | 10 | 30 | 60 | 300 | 900 | 3600),
                "Telegram slow mode accepts 0, 10, 30, 60, 300, 900, or 3600 seconds"
            );
        }
        if let Some(name) = profile.name {
            let mut request = payload.clone();
            request.insert("title".to_owned(), name.into());
            let _: bool = self.call("setChatTitle", Value::Object(request)).await?;
        }
        if let Some(description) = profile.description {
            let mut request = payload.clone();
            request.insert("description".to_owned(), description.into());
            let _: bool = self
                .call("setChatDescription", Value::Object(request))
                .await?;
        }
        if let Some(avatar) = profile.avatar {
            let upload = avatar_file_upload(&avatar).await?;
            let form = form_from_payload(payload.clone())?.part("photo", upload.into_part()?);
            let _: bool = self.client().call_multipart("setChatPhoto", form).await?;
        }
        if let Some(default_permissions) = profile.default_permissions {
            self.set_default_permissions(conversation.clone(), default_permissions)
                .await?;
        }
        if let Some(slow_mode) = profile.slow_mode {
            let delay = slow_mode.as_secs();
            let mut request = payload.clone();
            request.insert("slow_mode_delay".to_owned(), delay.into());
            let _: bool = self
                .call("setChatSlowModeDelay", Value::Object(request))
                .await?;
        }
        if delete_photo {
            let _: bool = self.call("deleteChatPhoto", Value::Object(payload)).await?;
        }
        Ok(())
    }

    async fn get_conversation_member(
        &self,
        conversation: ConversationRef,
        user_id: String,
    ) -> Result<ConversationMember> {
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert(
            "user_id".to_owned(),
            user_id
                .parse::<i64>()
                .context("invalid Telegram user id")?
                .into(),
        );
        let member: Value = self.call("getChatMember", Value::Object(payload)).await?;
        telegram_conversation_member(member)
    }

    async fn list_conversation_members(
        &self,
        conversation: ConversationRef,
        page: PageRequest,
    ) -> Result<Page<ConversationMember>> {
        anyhow::ensure!(
            page.cursor.is_none(),
            "Telegram administrator lists do not support cursors"
        );
        let payload = telegram_conversation_payload(&conversation)?;
        let members: Vec<Value> = self
            .call("getChatAdministrators", Value::Object(payload))
            .await?;
        let limit = page
            .limit
            .map(|limit| usize::try_from(limit).expect("u32 fits usize"))
            .unwrap_or(members.len());
        Ok(Page {
            items: members
                .into_iter()
                .take(limit)
                .map(telegram_conversation_member)
                .collect::<Result<Vec<_>>>()?,
            next_cursor: None,
            previous_cursor: None,
            total: None,
        })
    }

    async fn set_member_permissions(
        &self,
        conversation: ConversationRef,
        user_id: String,
        permissions: PermissionSet,
    ) -> Result<()> {
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert(
            "user_id".to_owned(),
            user_id
                .parse::<i64>()
                .context("invalid Telegram user id")?
                .into(),
        );
        if telegram_has_admin_permissions(&permissions) {
            let mut admin = payload;
            for (name, value) in telegram_administrator_permissions(&permissions) {
                admin.insert(name, value.into());
            }
            let _: bool = self.call("promoteChatMember", Value::Object(admin)).await?;
        } else {
            let mut demote = payload.clone();
            for (name, value) in telegram_administrator_permissions(&PermissionSet::default()) {
                demote.insert(name, value.into());
            }
            let _: bool = self
                .call("promoteChatMember", Value::Object(demote))
                .await?;
            payload.insert(
                "permissions".to_owned(),
                Value::Object(telegram_chat_permissions(&permissions)),
            );
            payload.insert("use_independent_chat_permissions".to_owned(), true.into());
            let _: bool = self
                .call("restrictChatMember", Value::Object(payload))
                .await?;
        }
        Ok(())
    }

    async fn set_default_permissions(
        &self,
        conversation: ConversationRef,
        permissions: PermissionSet,
    ) -> Result<()> {
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert(
            "permissions".to_owned(),
            Value::Object(telegram_chat_permissions(&permissions)),
        );
        payload.insert("use_independent_chat_permissions".to_owned(), true.into());
        let _: bool = self
            .call("setChatPermissions", Value::Object(payload))
            .await?;
        Ok(())
    }

    async fn set_member_tag(
        &self,
        conversation: ConversationRef,
        user_id: String,
        tag: Option<String>,
    ) -> Result<()> {
        if let Some(tag) = &tag {
            anyhow::ensure!(
                tag.chars().count() <= 16,
                "Telegram member tags are limited to 16 characters"
            );
        }
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert(
            "user_id".to_owned(),
            user_id
                .parse::<i64>()
                .context("invalid Telegram user id")?
                .into(),
        );
        payload.insert("tag".to_owned(), tag.into());
        let _: bool = self
            .call("setChatMemberTag", Value::Object(payload))
            .await?;
        Ok(())
    }

    async fn remove_conversation_member(
        &self,
        conversation: ConversationRef,
        user_id: String,
    ) -> Result<()> {
        self.ban_conversation_member(conversation.clone(), user_id.clone(), None, false)
            .await?;
        self.unban_conversation_member(conversation, user_id).await
    }

    async fn ban_conversation_member(
        &self,
        conversation: ConversationRef,
        user_id: String,
        duration: Option<Duration>,
        delete_history: bool,
    ) -> Result<()> {
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert(
            "user_id".to_owned(),
            user_id
                .parse::<i64>()
                .context("invalid Telegram user id")?
                .into(),
        );
        payload.insert("revoke_messages".to_owned(), delete_history.into());
        if let Some(duration) = duration {
            let seconds = i64::try_from(duration.as_secs()).context("ban duration is too large")?;
            payload.insert(
                "until_date".to_owned(),
                chrono::Utc::now()
                    .timestamp()
                    .saturating_add(seconds)
                    .into(),
            );
        }
        let _: bool = self.call("banChatMember", Value::Object(payload)).await?;
        Ok(())
    }

    async fn unban_conversation_member(
        &self,
        conversation: ConversationRef,
        user_id: String,
    ) -> Result<()> {
        let mut payload = telegram_conversation_payload(&conversation)?;
        payload.insert(
            "user_id".to_owned(),
            user_id
                .parse::<i64>()
                .context("invalid Telegram user id")?
                .into(),
        );
        payload.insert("only_if_banned".to_owned(), true.into());
        let _: bool = self.call("unbanChatMember", Value::Object(payload)).await?;
        Ok(())
    }

    async fn approve_conversation_join_request(
        &self,
        conversation: ConversationRef,
        user_id: String,
    ) -> Result<()> {
        telegram_answer_join_request(self, "approveChatJoinRequest", conversation, user_id).await
    }

    async fn decline_conversation_join_request(
        &self,
        conversation: ConversationRef,
        user_id: String,
    ) -> Result<()> {
        telegram_answer_join_request(self, "declineChatJoinRequest", conversation, user_id).await
    }

    async fn leave_conversation(&self, conversation: ConversationRef) -> Result<()> {
        let _: bool = self
            .call(
                "leaveChat",
                Value::Object(telegram_conversation_payload(&conversation)?),
            )
            .await?;
        Ok(())
    }

    async fn create_invite_link(
        &self,
        conversation: ConversationRef,
        options: InviteLinkOptions,
    ) -> Result<InviteLink> {
        validate_invite_options(&options)?;
        let mut payload = telegram_conversation_payload(&conversation)?;
        add_invite_options(&mut payload, &options);
        let method = add_subscription_invite_options(&mut payload, &options, false)?;
        let invite: TelegramChatInviteLink = self.call(method, Value::Object(payload)).await?;
        Ok(telegram_invite_link(invite, conversation, options))
    }

    async fn edit_invite_link(
        &self,
        invite: InviteLink,
        options: InviteLinkOptions,
    ) -> Result<InviteLink> {
        validate_invite_options(&options)?;
        let mut payload = telegram_conversation_payload(&invite.conversation)?;
        payload.insert("invite_link".to_owned(), invite.url.into());
        add_invite_options(&mut payload, &options);
        let method = add_subscription_invite_options(&mut payload, &options, true)?;
        let response: TelegramChatInviteLink = self.call(method, Value::Object(payload)).await?;
        Ok(telegram_invite_link(response, invite.conversation, options))
    }

    async fn revoke_invite_link(&self, invite: InviteLink) -> Result<()> {
        let mut payload = telegram_conversation_payload(&invite.conversation)?;
        payload.insert("invite_link".to_owned(), invite.url.into());
        let _: TelegramChatInviteLink = self
            .call("revokeChatInviteLink", Value::Object(payload))
            .await?;
        Ok(())
    }

    async fn list_messages(&self, query: MessageQuery) -> Result<Page<MessageEnvelope>> {
        anyhow::ensure!(
            query.thread.is_none()
                && query.before.is_none()
                && query.after.is_none()
                && query.around.is_none()
                && query.search.is_none()
                && query.cursor.is_none()
                && query.platform_data.is_none(),
            "Telegram personal-chat history supports only a user and limit"
        );
        let conversation = query
            .conversation
            .context("Telegram personal-chat history requires a user conversation")?;
        anyhow::ensure!(
            matches!(conversation.kind, ConversationKind::Direct),
            "Telegram Bot API exposes history only for a user's personal chat"
        );
        let limit = query.limit.unwrap_or(20);
        anyhow::ensure!(
            (1..=20).contains(&limit),
            "Telegram personal-chat history limit must be between 1 and 20"
        );
        let messages: Vec<Value> = self
            .call(
                "getUserPersonalChatMessages",
                json!({
                    "user_id": conversation.id.parse::<i64>()
                        .context("invalid Telegram user id")?,
                    "limit": limit,
                }),
            )
            .await?;
        let items = messages
            .into_iter()
            .map(|raw| {
                let message: Message = serde_json::from_value(raw.clone())?;
                Ok(crate::event::telegram_message_envelope(&message, raw))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Page {
            items,
            next_cursor: None,
            previous_cursor: None,
            total: None,
        })
    }

    async fn forward_messages(
        &self,
        messages: Vec<MessageRef>,
        target: oxidebot::conversation::MessageTarget,
        options: ForwardOptions,
    ) -> Result<Vec<MessageRef>> {
        telegram_forward_or_copy(self, "forwardMessages", messages, target, options).await
    }

    async fn copy_messages(
        &self,
        messages: Vec<MessageRef>,
        target: oxidebot::conversation::MessageTarget,
        options: ForwardOptions,
    ) -> Result<Vec<MessageRef>> {
        telegram_forward_or_copy(self, "copyMessages", messages, target, options).await
    }

    async fn set_command_definitions(&self, commands: Vec<CommandDefinition>) -> Result<()> {
        let mut translations = BTreeSet::new();
        for command in &commands {
            anyhow::ensure!(
                matches!(command.kind, CommandKind::ChatInput)
                    && command.options.is_empty()
                    && command.default_permissions.is_none()
                    && command.platform_data.is_none(),
                "Telegram command menus cannot register typed options, targets, or default permissions"
            );
            translations.extend(command.name.translations.keys().cloned());
            translations.extend(command.description.translations.keys().cloned());
        }
        let scope = telegram_definition_scope(&commands)?;
        self.set_bot_commands(BotCommandSet {
            commands: telegram_commands_for_locale(&commands, None)?,
            scope: scope.clone(),
            language_code: None,
        })
        .await?;
        for locale in translations {
            self.set_bot_commands(BotCommandSet {
                commands: telegram_commands_for_locale(&commands, Some(&locale))?,
                scope: scope.clone(),
                language_code: Some(locale),
            })
            .await?;
        }
        Ok(())
    }

    async fn get_command_definitions(
        &self,
        conversation: Option<ConversationRef>,
    ) -> Result<Vec<CommandDefinition>> {
        let scope = conversation
            .map(|conversation| CommandScope::Chat {
                chat_id: root_conversation(&conversation).id.clone(),
            })
            .unwrap_or(CommandScope::Default);
        Ok(self
            .get_bot_commands(BotCommandQuery {
                scope,
                language_code: None,
            })
            .await?
            .into_iter()
            .map(|command| CommandDefinition {
                id: None,
                name: Localized {
                    default: command.command,
                    translations: BTreeMap::new(),
                },
                description: Localized {
                    default: command.description,
                    translations: BTreeMap::new(),
                },
                kind: CommandKind::ChatInput,
                options: Vec::new(),
                default_permissions: None,
                contexts: BTreeSet::from([CommandContext::Any]),
                ephemeral: command.is_ephemeral,
                platform_data: None,
            })
            .collect())
    }

    async fn delete_command_definitions(
        &self,
        conversation: Option<ConversationRef>,
    ) -> Result<()> {
        self.delete_bot_commands(BotCommandQuery {
            scope: conversation
                .map(|conversation| CommandScope::Chat {
                    chat_id: root_conversation(&conversation).id.clone(),
                })
                .unwrap_or(CommandScope::Default),
            language_code: None,
        })
        .await
    }

    async fn answer_suggestion_request(
        &self,
        request: SuggestionRequest,
        suggestions: Vec<Suggestion>,
        next_cursor: Option<String>,
    ) -> Result<()> {
        anyhow::ensure!(
            suggestions.len() <= 50,
            "Telegram inline queries support at most 50 results"
        );
        let results = suggestions
            .into_iter()
            .map(telegram_inline_query_result)
            .collect::<Result<Vec<_>>>()?;
        let mut payload = json!({
            "inline_query_id": request.id,
            "results": results,
            "next_offset": next_cursor.unwrap_or_default(),
        });
        if let Some(native) = request.platform_data {
            let Value::Object(fields) = telegram_native_object(native, "suggestion request")?
            else {
                unreachable!("native object helper always returns an object")
            };
            let Value::Object(object) = &mut payload else {
                unreachable!("inline query answer is an object")
            };
            for (name, value) in fields {
                if matches!(
                    name.as_str(),
                    "id" | "inline_query_id"
                        | "from"
                        | "query"
                        | "offset"
                        | "chat_type"
                        | "location"
                ) {
                    continue;
                }
                anyhow::ensure!(
                    matches!(name.as_str(), "cache_time" | "is_personal" | "button"),
                    "unsupported Telegram inline-query answer field {name:?}"
                );
                anyhow::ensure!(
                    !object.contains_key(&name),
                    "suggestion request duplicates Telegram field {name:?}"
                );
                object.insert(name, value);
            }
        }
        let _: bool = self.call("answerInlineQuery", payload).await?;
        Ok(())
    }

    async fn publish_surface(&self, surface: AppSurface) -> Result<AppSurface> {
        anyhow::ensure!(
            matches!(surface.kind, AppSurfaceKind::ChatMenu),
            "Telegram exposes only the chat menu as a persistent app surface"
        );
        anyhow::ensure!(
            surface.user_id.is_none() && surface.platform_data.is_none(),
            "Telegram chat-menu surfaces cannot be scoped to a user or carry outer native data"
        );
        let chat_id = surface
            .conversation
            .as_ref()
            .map(root_conversation)
            .map(|conversation| conversation.id.clone());
        let menu = match &surface.content {
            SurfaceContent::MiniApp(app) => ChatMenu::WebApp {
                text: surface
                    .title
                    .clone()
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| "Open".to_owned()),
                url: app.url.clone(),
            },
            SurfaceContent::PlatformNative(native) => ChatMenu::PlatformNative(native.clone()),
            _ => {
                return Err(UnsupportedInteractionError::new(
                    "this content in the Telegram chat-menu surface",
                )
                .into())
            }
        };
        self.set_chat_menu(chat_id, menu).await?;
        Ok(surface)
    }

    async fn delete_surface(&self, surface: AppSurface) -> Result<()> {
        anyhow::ensure!(
            matches!(surface.kind, AppSurfaceKind::ChatMenu),
            "Telegram exposes only the chat menu as a persistent app surface"
        );
        anyhow::ensure!(
            surface.user_id.is_none() && surface.platform_data.is_none(),
            "Telegram chat-menu surfaces cannot be scoped to a user or carry outer native data"
        );
        let chat_id = surface
            .conversation
            .as_ref()
            .map(root_conversation)
            .map(|conversation| conversation.id.clone());
        self.set_chat_menu(chat_id, ChatMenu::Default).await
    }

    async fn answer_mini_app_query(
        &self,
        event: MiniAppEvent,
        message: OutgoingMessage,
    ) -> Result<Vec<MessageRef>> {
        let query_id = event
            .query_id
            .context("Telegram Web App query has no query_id")?;
        let result = telegram_inline_query_result(Suggestion {
            id: query_id.clone(),
            title: "Web App result".to_owned(),
            description: None,
            image: None,
            value: oxidebot::FormValue::Text(String::new()),
            message: Some(message),
            platform_data: None,
        })?;
        let response: Value = self
            .call(
                "answerWebAppQuery",
                json!({"web_app_query_id": query_id, "result": result}),
            )
            .await?;
        Ok(response
            .get("inline_message_id")
            .and_then(Value::as_str)
            .map(|id| MessageRef {
                id: id.to_owned(),
                conversation: event.conversation,
                platform_data: Some(PlatformNativeData::new(
                    crate::SERVER,
                    json!({"inline_message_id": id}),
                )),
            })
            .into_iter()
            .collect())
    }

    async fn set_bot_profile_v2(&self, profile: BotProfile) -> Result<()> {
        for (language_code, name) in profile.names {
            let _: bool = self
                .call(
                    "setMyName",
                    json!({
                        "name": name,
                        "language_code": (!language_code.is_empty()).then_some(language_code),
                    }),
                )
                .await?;
        }
        for (language_code, description) in profile.descriptions {
            let _: bool = self
                .call(
                    "setMyDescription",
                    json!({
                        "description": description,
                        "language_code": (!language_code.is_empty()).then_some(language_code),
                    }),
                )
                .await?;
        }
        for (language_code, short_description) in profile.short_descriptions {
            let _: bool = self
                .call(
                    "setMyShortDescription",
                    json!({
                        "short_description": short_description,
                        "language_code": (!language_code.is_empty()).then_some(language_code),
                    }),
                )
                .await?;
        }
        if let Some(avatar) = profile.avatar {
            let upload = avatar_file_upload(&avatar).await?;
            let photo_type = if avatar
                .mime
                .as_ref()
                .is_some_and(|mime| mime.type_().as_str() == "video")
            {
                "animated"
            } else {
                "static"
            };
            let field = if photo_type == "animated" {
                "animation"
            } else {
                "photo"
            };
            let form = Form::new()
                .text(
                    "photo",
                    serde_json::to_string(&json!({
                        "type": photo_type,
                        field: "attach://profile_photo",
                    }))?,
                )
                .part("profile_photo", upload.into_part()?);
            let _: bool = self
                .client()
                .call_multipart("setMyProfilePhoto", form)
                .await?;
        }
        if let Some(permissions) = profile.default_permissions {
            let rights: Map<String, Value> = telegram_administrator_permissions(&permissions)
                .into_iter()
                .map(|(name, value)| (name, value.into()))
                .collect();
            let _: bool = self
                .call("setMyDefaultAdministratorRights", json!({"rights": rights}))
                .await?;
        }
        if let Some(native) = profile.platform_data {
            let Value::Object(mut native) = telegram_native_object(native, "bot profile")? else {
                unreachable!("native object helper always returns an object")
            };
            if native
                .remove("remove_photo")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
            {
                let _: bool = self.call("removeMyProfilePhoto", json!({})).await?;
            }
            anyhow::ensure!(
                native.is_empty(),
                "unsupported Telegram bot profile fields: {:?}",
                native.keys().collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    async fn get_bot_profile_v2(&self) -> Result<BotProfile> {
        let name: Value = self.call("getMyName", json!({})).await?;
        let description: Value = self.call("getMyDescription", json!({})).await?;
        let short_description: Value = self.call("getMyShortDescription", json!({})).await?;
        let rights: Value = self
            .call("getMyDefaultAdministratorRights", json!({}))
            .await?;
        Ok(BotProfile {
            names: BTreeMap::from([(
                String::new(),
                name.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )]),
            descriptions: BTreeMap::from([(
                String::new(),
                description
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )]),
            short_descriptions: BTreeMap::from([(
                String::new(),
                short_description
                    .get("short_description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            )]),
            avatar: None,
            default_permissions: rights.as_object().map(telegram_permission_set),
            platform_data: Some(PlatformNativeData::new(
                crate::SERVER,
                json!({"default_administrator_rights": rights}),
            )),
        })
    }

    async fn send_invoice(
        &self,
        target: MessageTarget,
        invoice: Invoice,
        options: InvoiceOptions,
    ) -> Result<MessageRef> {
        anyhow::ensure!(
            !invoice.items.is_empty(),
            "Telegram invoices require prices"
        );
        let currency = invoice
            .items
            .first()
            .expect("invoice has an item")
            .amount
            .currency
            .clone();
        anyhow::ensure!(
            invoice
                .items
                .iter()
                .all(|item| item.amount.currency == currency),
            "all Telegram invoice prices must use the same currency"
        );
        let prices = invoice
            .items
            .iter()
            .map(telegram_labeled_price)
            .collect::<Result<Vec<_>>>()?;
        let mut payload = telegram_conversation_payload(&target.conversation)?;
        if let Some(native) = target.platform_data {
            merge_native_fields(&mut payload, native, "invoice target")?;
        }
        payload.insert("title".to_owned(), invoice.title.into());
        payload.insert("description".to_owned(), invoice.description.into());
        payload.insert("payload".to_owned(), invoice.payload.into());
        payload.insert("currency".to_owned(), currency.into());
        payload.insert("prices".to_owned(), Value::Array(prices));
        payload.insert("provider_token".to_owned(), options.provider_token.into());
        payload.insert("start_parameter".to_owned(), options.start_parameter.into());
        payload.insert("photo_url".to_owned(), options.photo_url.into());
        payload.insert("protect_content".to_owned(), options.protect_content.into());
        payload.insert(
            "need_name".to_owned(),
            invoice.customer_details.name.is_some().into(),
        );
        payload.insert(
            "need_phone_number".to_owned(),
            invoice.customer_details.phone.is_some().into(),
        );
        payload.insert(
            "need_email".to_owned(),
            invoice.customer_details.email.is_some().into(),
        );
        payload.insert(
            "need_shipping_address".to_owned(),
            invoice.customer_details.shipping_address.is_some().into(),
        );
        payload.insert("is_flexible".to_owned(), invoice.flexible_shipping.into());
        if let Some(native) = invoice.platform_data {
            merge_native_fields(&mut payload, native, "invoice")?;
        }
        if let Some(native) = options.platform_data {
            merge_native_fields(&mut payload, native, "invoice options")?;
        }
        let message: Message = self.call("sendInvoice", Value::Object(payload)).await?;
        Ok(telegram_sent_ref(message, &target.conversation))
    }

    async fn answer_shipping_request(
        &self,
        request_id: String,
        options: Vec<ShippingOption>,
        error: Option<String>,
    ) -> Result<()> {
        anyhow::ensure!(
            error.is_none() || options.is_empty(),
            "a rejected Telegram shipping query cannot include shipping options"
        );
        let shipping_options = options
            .into_iter()
            .map(|option| {
                Ok(json!({
                    "id": option.id,
                    "title": option.title,
                    "prices": option.prices.iter()
                        .map(telegram_labeled_price)
                        .collect::<Result<Vec<_>>>()?,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let _: bool = self
            .call(
                "answerShippingQuery",
                json!({
                    "shipping_query_id": request_id,
                    "ok": error.is_none(),
                    "shipping_options": (!shipping_options.is_empty()).then_some(shipping_options),
                    "error_message": error,
                }),
            )
            .await?;
        Ok(())
    }

    async fn answer_checkout_request(
        &self,
        request: CheckoutRequest,
        approved: bool,
        error: Option<String>,
    ) -> Result<()> {
        anyhow::ensure!(
            approved != error.is_some(),
            "approved Telegram checkouts cannot include an error, and rejected checkouts require one"
        );
        let _: bool = self
            .call(
                "answerPreCheckoutQuery",
                json!({
                    "pre_checkout_query_id": request.id,
                    "ok": approved,
                    "error_message": error,
                }),
            )
            .await?;
        Ok(())
    }

    async fn refund_payment(&self, payment: Payment) -> Result<()> {
        anyhow::ensure!(
            payment.total.currency == "XTR",
            "Telegram Bot API can refund only Telegram Star payments"
        );
        let user_id = payment
            .payer
            .context("Telegram refunds require the payer")?
            .id
            .parse::<i64>()
            .context("invalid Telegram payer user id")?;
        let _: bool = self
            .call(
                "refundStarPayment",
                json!({
                    "user_id": user_id,
                    "telegram_payment_charge_id": payment.id,
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

async fn send_message_impl(
    bot: &TelegramBot,
    message: Vec<MessageSegment>,
    target: SendMessageTarget,
    options: MessageOptions,
) -> Result<Vec<SendMessageResponse>> {
    let mut non_component_options = options.clone();
    non_component_options.components = None;
    anyhow::ensure!(
        non_component_options.is_empty(),
        "legacy Telegram send_message_with_options supports components only; use send_outgoing_message for v2 message options"
    );
    let chat_id = match target {
        SendMessageTarget::Group(id) | SendMessageTarget::Private(id) => id,
    };
    let mut outgoing = process_message_segments(message)?;
    let mut sent = Vec::new();
    let mut reply = outgoing.reply.take();
    let mut reply_markup = options
        .components
        .map(telegram_message_components)
        .transpose()?;

    attach_caption_or_send_text(
        bot,
        &chat_id,
        &mut outgoing,
        &mut reply,
        &mut reply_markup,
        &mut sent,
    )
    .await?;
    send_media(
        bot,
        &chat_id,
        outgoing.media,
        &mut reply,
        &mut reply_markup,
        &mut sent,
    )
    .await?;

    for venue in outgoing.venues {
        let response: Message = if venue.title.trim().is_empty() {
            bot.call(
                "sendLocation",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "latitude": venue.latitude,
                    "longitude": venue.longitude,
                    "reply_parameters": reply.take(),
                    "reply_markup": reply_markup.take(),
                }),
            )
            .await?
        } else {
            bot.call(
                "sendVenue",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "latitude": venue.latitude,
                    "longitude": venue.longitude,
                    "title": venue.title,
                    "address": venue.address,
                    "reply_parameters": reply.take(),
                    "reply_markup": reply_markup.take(),
                }),
            )
            .await?
        };
        sent.push(sent_response(response));
    }

    for sticker in outgoing.stickers {
        let response: Message = bot
            .call(
                "sendSticker",
                json!({
                    "chat_id": chat_id_value(&chat_id),
                    "sticker": sticker,
                    "reply_parameters": reply.take(),
                    "reply_markup": reply_markup.take(),
                }),
            )
            .await?;
        sent.push(sent_response(response));
    }

    Ok(sent)
}

struct TelegramOutgoingContext {
    conversation: ConversationRef,
    base: Map<String, Value>,
    first_only: Map<String, Value>,
    link_preview: Option<Value>,
    delivery_time: DeliveryTime,
}

impl TelegramOutgoingContext {
    fn payload(&mut self) -> Map<String, Value> {
        let mut payload = self.base.clone();
        payload.append(&mut self.first_only);
        payload
    }
}

async fn telegram_send_outgoing(
    bot: &TelegramBot,
    target: MessageTarget,
    message: OutgoingMessage,
) -> Result<Vec<MessageRef>> {
    let OutgoingMessage { content, options } = message;
    anyhow::ensure!(
        !content.is_empty(),
        "Telegram message content cannot be empty"
    );
    let mut context = telegram_outgoing_context(target, options)?;

    if matches!(context.delivery_time, DeliveryTime::Draft) {
        return send_telegram_draft(bot, context, content).await;
    }
    anyhow::ensure!(
        matches!(context.delivery_time, DeliveryTime::Immediate),
        "Telegram Bot API does not support scheduled messages"
    );

    let mut sent = Vec::new();
    for content in content {
        match content {
            MessageContent::PlainText(text) => {
                let mut payload = context.payload();
                payload.insert("text".to_owned(), text.into());
                if let Some(link_preview) = context.link_preview.clone() {
                    payload.insert("link_preview_options".to_owned(), link_preview);
                }
                let message: Message = bot.call("sendMessage", Value::Object(payload)).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::RichText(text) => {
                let mut payload = context.payload();
                payload.insert("text".to_owned(), text.text.clone().into());
                payload.insert(
                    "entities".to_owned(),
                    Value::Array(telegram_rich_text_entities(&text)?),
                );
                if let Some(link_preview) = context.link_preview.clone() {
                    payload.insert("link_preview_options".to_owned(), link_preview);
                }
                let message: Message = bot.call("sendMessage", Value::Object(payload)).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Image(media) => {
                let message =
                    send_v2_media(bot, "sendPhoto", "photo", media, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Video(media) => {
                let message =
                    send_v2_media(bot, "sendVideo", "video", media, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Audio(media) => {
                let message =
                    send_v2_media(bot, "sendAudio", "audio", media, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Animation(media) => {
                let message =
                    send_v2_media(bot, "sendAnimation", "animation", media, context.payload())
                        .await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::VoiceNote(media) => {
                let message =
                    send_v2_media(bot, "sendVoice", "voice", media, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::VideoNote(media) => {
                anyhow::ensure!(
                    media.caption.is_none(),
                    "Telegram video notes do not support captions"
                );
                let message =
                    send_v2_media(bot, "sendVideoNote", "video_note", media, context.payload())
                        .await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::File(media) => {
                let message =
                    send_v2_media(bot, "sendDocument", "document", media, context.payload())
                        .await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Sticker(sticker) => {
                let mut media = Media::new(sticker.file.unwrap_or_else(|| File {
                    id: Some(sticker.id),
                    ..Default::default()
                }));
                media.platform_data = sticker.platform_data;
                let message =
                    send_v2_media(bot, "sendSticker", "sticker", media, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::CustomEmoji(emoji) => {
                let alternative = emoji
                    .fallback
                    .clone()
                    .or(emoji.name.clone())
                    .context("Telegram custom emoji content requires fallback text")?;
                let text = RichText::plain(alternative.clone()).span(
                    0..alternative.len(),
                    TextStyle::CustomEmoji {
                        id: emoji.id,
                        fallback: Some(alternative),
                    },
                );
                let mut payload = context.payload();
                payload.insert("text".to_owned(), text.text.clone().into());
                payload.insert(
                    "entities".to_owned(),
                    Value::Array(telegram_rich_text_entities(&text)?),
                );
                let message: Message = bot.call("sendMessage", Value::Object(payload)).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Location(location) => {
                anyhow::ensure!(
                    location.title.is_some() || location.address.is_none(),
                    "Telegram locations cannot contain an address without venue title"
                );
                anyhow::ensure!(
                    location.title.is_none()
                        || location
                            .address
                            .as_ref()
                            .is_some_and(|address| !address.is_empty()),
                    "Telegram venues require a non-empty address"
                );
                let mut payload = context.payload();
                payload.insert("latitude".to_owned(), location.latitude.into());
                payload.insert("longitude".to_owned(), location.longitude.into());
                payload.insert(
                    "horizontal_accuracy".to_owned(),
                    location.horizontal_accuracy.into(),
                );
                payload.insert(
                    "live_period".to_owned(),
                    location.live_period.map(|value| value.as_secs()).into(),
                );
                payload.insert("heading".to_owned(), location.heading.into());
                payload.insert(
                    "proximity_alert_radius".to_owned(),
                    location.proximity_alert_radius.into(),
                );
                if let Some(native) = location.platform_data {
                    merge_native_fields(&mut payload, native, "location")?;
                }
                let method = if let Some(title) = location.title {
                    payload.insert("title".to_owned(), title.into());
                    payload.insert(
                        "address".to_owned(),
                        location.address.unwrap_or_default().into(),
                    );
                    "sendVenue"
                } else {
                    "sendLocation"
                };
                let message: Message = bot.call(method, Value::Object(payload)).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Contact(contact) => {
                anyhow::ensure!(
                    contact.phone_numbers.len() == 1
                        && contact.phone_numbers[0].label.is_none()
                        && contact.user_id.is_none()
                        && contact.emails.is_empty()
                        && contact.organization.is_none(),
                    "Telegram contacts support one unlabeled phone number; put additional contact data in vCard"
                );
                let phone_number = contact
                    .phone_numbers
                    .first()
                    .context("Telegram contacts require a phone number")?
                    .value
                    .clone();
                let mut payload = context.payload();
                payload.insert("phone_number".to_owned(), phone_number.into());
                payload.insert("first_name".to_owned(), contact.first_name.into());
                payload.insert("last_name".to_owned(), contact.last_name.into());
                payload.insert("vcard".to_owned(), contact.vcard.into());
                if let Some(native) = contact.platform_data {
                    merge_native_fields(&mut payload, native, "contact")?;
                }
                let message: Message = bot.call("sendContact", Value::Object(payload)).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Poll(poll) => {
                let message = send_telegram_poll(bot, poll, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::Checklist(checklist) => {
                let message = send_telegram_checklist(bot, checklist, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::RichLayout(layout) => {
                let message = send_telegram_rich_layout(bot, layout, context.payload()).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
            MessageContent::MediaGallery(items) => {
                let messages = send_v2_media_group(bot, items, context.payload()).await?;
                sent.extend(
                    messages
                        .into_iter()
                        .map(|message| telegram_sent_ref(message, &context.conversation)),
                );
            }
            MessageContent::PlatformNative(native) => {
                let Value::Object(mut native) = telegram_native_object(native, "message content")?
                else {
                    unreachable!("native object helper always returns an object")
                };
                let method = native
                    .remove("method")
                    .and_then(|method| method.as_str().map(str::to_owned))
                    .context("Telegram native message content requires a method")?;
                let mut payload = context.payload();
                for (name, value) in native {
                    anyhow::ensure!(
                        !payload.contains_key(&name),
                        "Telegram native message content duplicates field {name:?}"
                    );
                    payload.insert(name, value);
                }
                let message: Message = bot.call(&method, Value::Object(payload)).await?;
                sent.push(telegram_sent_ref(message, &context.conversation));
            }
        }
    }
    Ok(sent)
}

fn telegram_outgoing_context(
    target: MessageTarget,
    options: MessageOptions,
) -> Result<TelegramOutgoingContext> {
    let MessageOptions {
        components,
        reply,
        notification,
        visibility,
        link_preview,
        mentions,
        protect_content,
        delivery_time,
        idempotency_key,
        client_message_id,
        metadata,
        platform_data,
    } = options;
    anyhow::ensure!(
        idempotency_key.is_none() && client_message_id.is_none() && metadata.is_empty(),
        "Telegram Bot API does not expose message idempotency keys or portable metadata"
    );
    if let Some(mentions) = mentions {
        anyhow::ensure!(
            mentions == oxidebot::content::MentionPolicy::default(),
            "Telegram Bot API does not expose allowed-mention policy controls"
        );
    }

    let mut base = telegram_conversation_payload(&target.conversation)?;
    if let Some(native) = target.platform_data {
        merge_native_fields(&mut base, native, "message target")?;
    }
    base.insert(
        "disable_notification".to_owned(),
        matches!(notification, NotificationPolicy::Silent).into(),
    );
    base.insert("protect_content".to_owned(), protect_content.into());

    let ephemeral = matches!(&visibility, MessageVisibility::Ephemeral);
    anyhow::ensure!(
        !matches!(&visibility, MessageVisibility::Public) || target.recipients.is_empty(),
        "public Telegram messages cannot specify private recipients"
    );
    if let MessageVisibility::PrivateTo(users) = &visibility {
        anyhow::ensure!(
            !users.is_empty(),
            "MessageVisibility::PrivateTo requires a Telegram receiver"
        );
    }
    let explicit_recipients = match visibility {
        MessageVisibility::Public => Vec::new(),
        MessageVisibility::Ephemeral => target.recipients.clone(),
        MessageVisibility::PrivateTo(users) => users,
    };
    let recipients = if explicit_recipients.is_empty() {
        target.recipients
    } else {
        explicit_recipients
    };
    anyhow::ensure!(
        recipients.len() <= 1,
        "Telegram ephemeral messages support one receiver"
    );
    if let Some(receiver) = recipients.first() {
        base.insert(
            "receiver_user_id".to_owned(),
            receiver
                .parse::<i64>()
                .context("invalid Telegram ephemeral receiver user id")?
                .into(),
        );
    }
    if ephemeral
        && !base.contains_key("receiver_user_id")
        && !base.contains_key("callback_query_id")
    {
        return Err(anyhow::anyhow!(
            "Telegram ephemeral messages require a receiver or callback_query_id"
        ));
    }
    if let Some(native) = platform_data {
        merge_native_fields(&mut base, native, "message options")?;
    }

    let mut first_only = Map::new();
    if let Some(reply) = reply {
        first_only.insert(
            "reply_parameters".to_owned(),
            telegram_reply_options(reply)?,
        );
    }
    if let Some(components) = components {
        first_only.insert(
            "reply_markup".to_owned(),
            telegram_message_components(components)?,
        );
    }
    Ok(TelegramOutgoingContext {
        conversation: target.conversation,
        base,
        first_only,
        link_preview: link_preview.map(telegram_link_preview),
        delivery_time,
    })
}

fn telegram_link_preview(options: LinkPreviewOptions) -> Value {
    json!({
        "is_disabled": !options.enabled,
        "prefer_large_media": options.prefer_large_media,
        "show_above_text": options.above_text,
    })
}

fn telegram_reply_options(reply: ReplyOptions) -> Result<Value> {
    anyhow::ensure!(
        reply.notify_original_sender.is_none(),
        "Telegram does not expose reply-author notification control"
    );
    let (chat_id, message_id) = telegram_message_ref(&reply.message)?;
    let (quote, quote_entities) = reply
        .quote
        .map(|quote| {
            let entities = telegram_rich_text_entities(&quote)?;
            Ok::<_, anyhow::Error>((Some(quote.text), Some(entities)))
        })
        .transpose()?
        .unwrap_or((None, None));
    let mut value = json!({
        "message_id": message_id,
        "chat_id": chat_id_value(&chat_id),
        "allow_sending_without_reply": reply.allow_without_original,
        "quote": quote,
        "quote_entities": quote_entities,
        "quote_position": reply.quote_position,
    });
    if let Some(native) = reply.platform_data {
        let Value::Object(fields) = telegram_native_object(native, "reply options")? else {
            unreachable!("native object helper always returns an object")
        };
        let Value::Object(object) = &mut value else {
            unreachable!("reply options are an object")
        };
        for (name, value) in fields {
            anyhow::ensure!(
                !object.contains_key(&name),
                "Telegram reply options duplicate field {name:?}"
            );
            object.insert(name, value);
        }
    }
    strip_nulls(&mut value);
    Ok(value)
}

fn telegram_sent_ref(message: Message, conversation: &ConversationRef) -> MessageRef {
    let mut platform_data = Map::new();
    for name in ["business_connection_id", "receiver_user_id"] {
        if let Some(value) = message.extra.get(name) {
            platform_data.insert(name.to_owned(), value.clone());
        }
    }
    if message
        .extra
        .get("is_ephemeral")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        platform_data.insert("ephemeral_message_id".to_owned(), message.message_id.into());
    }
    MessageRef {
        id: message.message_id.to_string(),
        conversation: Some(conversation.clone()),
        platform_data: (!platform_data.is_empty())
            .then(|| PlatformNativeData::new(crate::SERVER, Value::Object(platform_data))),
    }
}

fn telegram_rich_text_entities(text: &RichText) -> Result<Vec<Value>> {
    let mut entities = Vec::new();
    for span in &text.spans {
        anyhow::ensure!(
            span.range.start <= span.range.end
                && span.range.end <= text.text.len()
                && text.text.is_char_boundary(span.range.start)
                && text.text.is_char_boundary(span.range.end),
            "rich-text span is not on valid UTF-8 boundaries"
        );
        let offset = text.text[..span.range.start].encode_utf16().count();
        let length = text.text[span.range.clone()].encode_utf16().count();
        anyhow::ensure!(length > 0, "Telegram message entities cannot be empty");
        for style in &span.styles {
            let mut entity = json!({"offset": offset, "length": length});
            match style {
                TextStyle::Bold => entity["type"] = "bold".into(),
                TextStyle::Italic => entity["type"] = "italic".into(),
                TextStyle::Underline => entity["type"] = "underline".into(),
                TextStyle::Strikethrough => entity["type"] = "strikethrough".into(),
                TextStyle::Spoiler => entity["type"] = "spoiler".into(),
                TextStyle::Code => entity["type"] = "code".into(),
                TextStyle::Preformatted { language } => {
                    entity["type"] = "pre".into();
                    entity["language"] = language.clone().into();
                }
                TextStyle::Link { url } => {
                    entity["type"] = "text_link".into();
                    entity["url"] = url.clone().into();
                }
                TextStyle::UserMention { user_id } => {
                    let user_id = user_id
                        .parse::<i64>()
                        .context("invalid Telegram mentioned user id")?;
                    entity["type"] = "text_link".into();
                    entity["url"] = format!("tg://user?id={user_id}").into();
                }
                TextStyle::CustomEmoji { id, .. } => {
                    entity["type"] = "custom_emoji".into();
                    entity["custom_emoji_id"] = id.clone().into();
                }
                TextStyle::Quote => entity["type"] = "blockquote".into(),
                TextStyle::Hashtag => entity["type"] = "hashtag".into(),
                TextStyle::Cashtag => entity["type"] = "cashtag".into(),
                TextStyle::BotCommand => entity["type"] = "bot_command".into(),
                TextStyle::Email => entity["type"] = "email".into(),
                TextStyle::Phone => entity["type"] = "phone_number".into(),
                TextStyle::DateTime { timestamp } => {
                    entity["type"] = "date_time".into();
                    entity["unix_time"] = timestamp.map(|value| value.timestamp()).into();
                }
                TextStyle::PlatformNative(native) => {
                    let Value::Object(fields) =
                        telegram_native_object(native.clone(), "rich-text entity")?
                    else {
                        unreachable!("native object helper always returns an object")
                    };
                    let Value::Object(object) = &mut entity else {
                        unreachable!("entity is an object")
                    };
                    for (name, value) in fields {
                        anyhow::ensure!(
                            !object.contains_key(&name),
                            "Telegram rich-text entity duplicates field {name:?}"
                        );
                        object.insert(name, value);
                    }
                }
                TextStyle::RoleMention { .. }
                | TextStyle::ConversationMention { .. }
                | TextStyle::Marked
                | TextStyle::Subscript
                | TextStyle::Superscript
                | TextStyle::MathematicalExpression
                | TextStyle::Reference { .. } => {
                    return Err(UnsupportedInteractionError::new(
                        "this rich-text style in a regular Telegram text message; use RichLayout",
                    )
                    .into())
                }
            }
            strip_nulls(&mut entity);
            entities.push(entity);
        }
    }
    Ok(entities)
}

async fn send_telegram_draft(
    bot: &TelegramBot,
    mut context: TelegramOutgoingContext,
    content: Vec<MessageContent>,
) -> Result<Vec<MessageRef>> {
    anyhow::ensure!(
        content.len() == 1 && context.first_only.is_empty(),
        "Telegram drafts support one text item without reply markup"
    );
    let (text, entities) = match content.into_iter().next().expect("one item") {
        MessageContent::PlainText(text) => (text, Vec::new()),
        MessageContent::RichText(text) => {
            let entities = telegram_rich_text_entities(&text)?;
            (text.text, entities)
        }
        _ => {
            return Err(UnsupportedInteractionError::new("non-text Telegram drafts").into());
        }
    };
    let draft_id = context
        .base
        .remove("draft_id")
        .and_then(|value| value.as_i64())
        .context("Telegram drafts require a non-zero draft_id in platform data")?;
    anyhow::ensure!(draft_id != 0, "Telegram draft_id cannot be zero");
    context
        .base
        .retain(|name, _| matches!(name.as_str(), "chat_id" | "message_thread_id"));
    context.base.insert("draft_id".to_owned(), draft_id.into());
    context.base.insert("text".to_owned(), text.into());
    context
        .base
        .insert("entities".to_owned(), Value::Array(entities));
    let _: bool = bot
        .call("sendMessageDraft", Value::Object(context.base))
        .await?;
    Ok(vec![MessageRef {
        id: format!("draft:{draft_id}"),
        conversation: Some(context.conversation),
        platform_data: Some(PlatformNativeData::new(
            crate::SERVER,
            json!({"draft_id": draft_id}),
        )),
    }])
}

fn add_media_fields(payload: &mut Map<String, Value>, method: &str, media: &Media) -> Result<()> {
    anyhow::ensure!(
        media.alt_text.is_none(),
        "Telegram media messages do not expose portable alt text"
    );
    anyhow::ensure!(
        media.waveform.is_none(),
        "Telegram outgoing media does not accept a waveform"
    );
    let supports_caption = !matches!(method, "sendSticker" | "sendVideoNote");
    if let Some(caption) = &media.caption {
        anyhow::ensure!(supports_caption, "{method} does not support captions");
        anyhow::ensure!(
            utf16_len(&caption.text) <= CAPTION_LIMIT,
            "Telegram media caption exceeds {CAPTION_LIMIT} UTF-16 code units"
        );
        payload.insert("caption".to_owned(), caption.text.clone().into());
        payload.insert(
            "caption_entities".to_owned(),
            Value::Array(telegram_rich_text_entities(caption)?),
        );
    }
    if let Some(duration) = media.duration {
        payload.insert("duration".to_owned(), duration.as_secs().into());
    }
    if matches!(method, "sendVideo" | "sendAnimation") {
        payload.insert("width".to_owned(), media.width.into());
        payload.insert("height".to_owned(), media.height.into());
        payload.insert("has_spoiler".to_owned(), media.spoiler.into());
    } else if method == "sendPhoto" {
        payload.insert("has_spoiler".to_owned(), media.spoiler.into());
    } else if method == "sendVideoNote" {
        if let (Some(width), Some(height)) = (media.width, media.height) {
            anyhow::ensure!(width == height, "Telegram video notes must be square");
            payload.insert("length".to_owned(), width.into());
        }
    }
    Ok(())
}

async fn send_v2_media(
    bot: &TelegramBot,
    method: &str,
    field: &str,
    media: Media,
    mut payload: Map<String, Value>,
) -> Result<Message> {
    add_media_fields(&mut payload, method, &media)?;
    if let Some(native) = media.platform_data {
        merge_native_fields(&mut payload, native, "media options")?;
    }
    let thumbnail = media.thumbnail;
    match resolve_file(media.file).await? {
        ResolvedFile::Remote(file) => {
            payload.insert(field.to_owned(), file.into());
            if let Some(thumbnail) = thumbnail {
                match resolve_file(thumbnail).await? {
                    ResolvedFile::Remote(file) => {
                        payload.insert("thumbnail".to_owned(), file.into());
                    }
                    ResolvedFile::Upload(_) => {
                        return Err(anyhow::anyhow!(
                            "a local Telegram thumbnail requires the primary media to be uploaded"
                        ));
                    }
                }
            }
            bot.call(method, Value::Object(payload)).await
        }
        ResolvedFile::Upload(upload) => {
            let mut form = form_from_payload(payload)?.part(field.to_owned(), upload.into_part()?);
            if let Some(thumbnail) = thumbnail {
                match resolve_file(thumbnail).await? {
                    ResolvedFile::Upload(upload) => {
                        form = form.part("thumbnail", upload.into_part()?);
                    }
                    ResolvedFile::Remote(file) => {
                        form = form.text("thumbnail", file);
                    }
                }
            }
            bot.client().call_multipart(method, form).await
        }
    }
}

async fn send_v2_media_group(
    bot: &TelegramBot,
    items: Vec<MediaGalleryItem>,
    mut payload: Map<String, Value>,
) -> Result<Vec<Message>> {
    anyhow::ensure!(
        (2..=MEDIA_GROUP_LIMIT).contains(&items.len()),
        "Telegram media groups require 2 to {MEDIA_GROUP_LIMIT} items"
    );

    let reply_markup = payload.remove("reply_markup");
    if let Some(reply_markup) = &reply_markup {
        anyhow::ensure!(
            reply_markup.get("inline_keyboard").is_some(),
            "Telegram media groups can be followed only by an inline keyboard"
        );
    }

    let classes = items
        .iter()
        .map(|item| match &item.kind {
            MediaType::Audio => Ok("audio"),
            MediaType::Document => Ok("document"),
            MediaType::Image | MediaType::Video => Ok("visual"),
            MediaType::PlatformNative(kind)
                if matches!(kind.as_str(), "photo" | "live_photo" | "video") =>
            {
                Ok("visual")
            }
            _ => Err(
                UnsupportedInteractionError::new("this media kind in a Telegram media group")
                    .into(),
            ),
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        classes.iter().all(|class| class == &classes[0]),
        "Telegram audio and document albums cannot be mixed with other media kinds"
    );

    let mut input_media = Vec::with_capacity(items.len());
    let mut uploads = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let MediaGalleryItem { kind, media } = item;
        let (telegram_type, send_method) = match &kind {
            MediaType::Image => ("photo", "sendPhoto"),
            MediaType::Video => ("video", "sendVideo"),
            MediaType::Audio => ("audio", "sendAudio"),
            MediaType::Document => ("document", "sendDocument"),
            MediaType::PlatformNative(kind) => (kind.as_str(), "sendVideo"),
            _ => unreachable!("media-group kinds were validated above"),
        };
        let mut object = Map::new();
        object.insert("type".to_owned(), telegram_type.into());
        add_media_fields(&mut object, send_method, &media)?;
        if let Some(native) = media.platform_data.clone() {
            merge_native_fields(&mut object, native, "media-group item")?;
        }

        match resolve_file(media.file).await? {
            ResolvedFile::Remote(file) => {
                object.insert("media".to_owned(), file.into());
            }
            ResolvedFile::Upload(upload) => {
                let name = format!("media{index}");
                object.insert("media".to_owned(), format!("attach://{name}").into());
                uploads.push((name, upload));
            }
        }
        if let Some(thumbnail) = media.thumbnail {
            match resolve_file(thumbnail).await? {
                ResolvedFile::Remote(file) => {
                    object.insert("thumbnail".to_owned(), file.into());
                }
                ResolvedFile::Upload(upload) => {
                    let name = format!("thumbnail{index}");
                    object.insert("thumbnail".to_owned(), format!("attach://{name}").into());
                    uploads.push((name, upload));
                }
            }
        }
        let mut value = Value::Object(object);
        strip_nulls(&mut value);
        input_media.push(value);
    }
    payload.insert("media".to_owned(), Value::Array(input_media));

    let messages: Vec<Message> = if uploads.is_empty() {
        bot.call("sendMediaGroup", Value::Object(payload)).await?
    } else {
        let mut form = form_from_payload(payload)?;
        for (name, upload) in uploads {
            form = form.part(name, upload.into_part()?);
        }
        bot.client().call_multipart("sendMediaGroup", form).await?
    };

    if let (Some(reply_markup), Some(first)) = (reply_markup, messages.first()) {
        let chat_id = messages
            .first()
            .map(|message| message.chat.id)
            .context("Telegram returned an empty media group")?;
        let _: Value = bot
            .call(
                "editMessageReplyMarkup",
                json!({
                    "chat_id": chat_id,
                    "message_id": first.message_id,
                    "reply_markup": reply_markup,
                }),
            )
            .await?;
    }
    Ok(messages)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TelegramEditAddress {
    Regular,
    Inline,
    Ephemeral,
}

fn telegram_edit_address(
    message: &MessageRef,
) -> Result<(Map<String, Value>, TelegramEditAddress)> {
    let native = message
        .platform_data
        .clone()
        .map(|native| telegram_native_object(native, "message reference"))
        .transpose()?;
    let native = native.as_ref().and_then(Value::as_object);

    if let Some(inline_message_id) = native
        .and_then(|fields| fields.get("inline_message_id"))
        .and_then(Value::as_str)
    {
        return Ok((
            Map::from_iter([(
                "inline_message_id".to_owned(),
                inline_message_id.to_owned().into(),
            )]),
            TelegramEditAddress::Inline,
        ));
    }

    let ephemeral = native.is_some_and(|fields| {
        fields.contains_key("receiver_user_id") || fields.contains_key("ephemeral_message_id")
    });
    let (chat_id, message_id) = telegram_message_ref(message)?;
    let mut payload = telegram_chat_payload(message.conversation.as_ref(), &chat_id)?;
    payload.remove("message_thread_id");
    payload.remove("direct_messages_topic_id");

    if ephemeral {
        let receiver_user_id = native
            .and_then(|fields| fields.get("receiver_user_id"))
            .and_then(Value::as_i64)
            .context("Telegram ephemeral message reference requires receiver_user_id")?;
        let ephemeral_message_id = native
            .and_then(|fields| fields.get("ephemeral_message_id"))
            .and_then(Value::as_i64)
            .unwrap_or(message_id);
        payload.remove("business_connection_id");
        payload.insert("receiver_user_id".to_owned(), receiver_user_id.into());
        payload.insert(
            "ephemeral_message_id".to_owned(),
            ephemeral_message_id.into(),
        );
        Ok((payload, TelegramEditAddress::Ephemeral))
    } else {
        payload.insert("message_id".to_owned(), message_id.into());
        if let Some(business_connection_id) = native
            .and_then(|fields| fields.get("business_connection_id"))
            .cloned()
        {
            payload.insert("business_connection_id".to_owned(), business_connection_id);
        }
        Ok((payload, TelegramEditAddress::Regular))
    }
}

fn telegram_edit_options(
    options: MessageOptions,
) -> Result<(Value, Option<Value>, Option<PlatformNativeData>, bool)> {
    let MessageOptions {
        components,
        reply,
        notification,
        visibility,
        link_preview,
        mentions,
        protect_content,
        delivery_time,
        idempotency_key,
        client_message_id,
        metadata,
        platform_data,
    } = options;
    anyhow::ensure!(
        reply.is_none()
            && notification == NotificationPolicy::Default
            && matches!(
                &visibility,
                MessageVisibility::Public | MessageVisibility::Ephemeral
            )
            && mentions
                .as_ref()
                .is_none_or(|mentions| mentions == &oxidebot::content::MentionPolicy::default())
            && !protect_content
            && delivery_time == DeliveryTime::Immediate
            && idempotency_key.is_none()
            && client_message_id.is_none()
            && metadata.is_empty(),
        "Telegram message edits do not support reply, delivery, visibility, mention, protection, idempotency, or metadata changes"
    );
    let reply_markup = components
        .map(telegram_editable_components)
        .transpose()?
        .unwrap_or_else(|| json!({"inline_keyboard": []}));
    let ephemeral = visibility == MessageVisibility::Ephemeral;
    Ok((
        reply_markup,
        link_preview.map(telegram_link_preview),
        platform_data,
        ephemeral,
    ))
}

fn telegram_input_rich_layout(layout: RichLayout) -> Result<(Value, Option<Value>)> {
    let mut actions = Vec::new();
    let blocks = telegram_layout_nodes(layout.nodes, &mut actions)?;
    anyhow::ensure!(!blocks.is_empty(), "Telegram rich message cannot be empty");
    let actions = if actions.is_empty() {
        None
    } else {
        Some(json!({
            "inline_keyboard": telegram_inline_keyboard(InlineKeyboard::new(actions))?,
        }))
    };
    let mut rich_message = json!({"blocks": blocks});
    if let Some(native) = layout.platform_data {
        let Value::Object(fields) = telegram_native_object(native, "rich message")? else {
            unreachable!("native object helper always returns an object")
        };
        let Value::Object(object) = &mut rich_message else {
            unreachable!("rich message is an object")
        };
        for (name, value) in fields {
            anyhow::ensure!(
                !object.contains_key(&name),
                "Telegram rich message duplicates field {name:?}"
            );
            object.insert(name, value);
        }
    }
    Ok((rich_message, actions))
}

async fn telegram_edit_outgoing(
    bot: &TelegramBot,
    message: MessageRef,
    new_message: OutgoingMessage,
) -> Result<()> {
    let OutgoingMessage {
        mut content,
        options,
    } = new_message;
    anyhow::ensure!(
        content.len() == 1,
        "Telegram can replace a message with exactly one content item"
    );
    let (mut payload, address) = telegram_edit_address(&message)?;
    let (reply_markup, link_preview, native_options, ephemeral_requested) =
        telegram_edit_options(options)?;
    anyhow::ensure!(
        !ephemeral_requested || address == TelegramEditAddress::Ephemeral,
        "MessageVisibility::Ephemeral requires an ephemeral Telegram message reference"
    );
    let content = content.pop().expect("one content item");

    let method = match content {
        MessageContent::PlainText(text) => {
            payload.insert("text".to_owned(), text.into());
            if let Some(link_preview) = link_preview {
                payload.insert("link_preview_options".to_owned(), link_preview);
            }
            match address {
                TelegramEditAddress::Ephemeral => "editEphemeralMessageText",
                _ => "editMessageText",
            }
        }
        MessageContent::RichText(text) => {
            payload.insert("text".to_owned(), text.text.clone().into());
            payload.insert(
                "entities".to_owned(),
                Value::Array(telegram_rich_text_entities(&text)?),
            );
            if let Some(link_preview) = link_preview {
                payload.insert("link_preview_options".to_owned(), link_preview);
            }
            match address {
                TelegramEditAddress::Ephemeral => "editEphemeralMessageText",
                _ => "editMessageText",
            }
        }
        MessageContent::RichLayout(layout) => {
            return telegram_edit_rich_layout(
                bot,
                payload,
                address,
                layout,
                reply_markup,
                native_options,
            )
            .await;
        }
        MessageContent::Checklist(checklist) => {
            anyhow::ensure!(
                address == TelegramEditAddress::Regular,
                "Telegram can edit checklists only in regular business messages"
            );
            anyhow::ensure!(
                payload.contains_key("business_connection_id"),
                "Telegram checklist edits require business_connection_id"
            );
            payload.insert("checklist".to_owned(), telegram_input_checklist(checklist)?);
            "editMessageChecklist"
        }
        MessageContent::Image(media) => {
            return telegram_edit_media_v2(
                bot,
                payload,
                address,
                MediaType::Image,
                media,
                reply_markup,
                native_options,
            )
            .await;
        }
        MessageContent::Video(media) => {
            return telegram_edit_media_v2(
                bot,
                payload,
                address,
                MediaType::Video,
                media,
                reply_markup,
                native_options,
            )
            .await;
        }
        MessageContent::Audio(media) => {
            return telegram_edit_media_v2(
                bot,
                payload,
                address,
                MediaType::Audio,
                media,
                reply_markup,
                native_options,
            )
            .await;
        }
        MessageContent::Animation(media) => {
            return telegram_edit_media_v2(
                bot,
                payload,
                address,
                MediaType::Animation,
                media,
                reply_markup,
                native_options,
            )
            .await;
        }
        MessageContent::File(media) => {
            return telegram_edit_media_v2(
                bot,
                payload,
                address,
                MediaType::Document,
                media,
                reply_markup,
                native_options,
            )
            .await;
        }
        MessageContent::PlatformNative(native) => {
            let Value::Object(mut fields) =
                telegram_native_object(native, "edited message content")?
            else {
                unreachable!("native object helper always returns an object")
            };
            let method = fields
                .remove("method")
                .and_then(|method| method.as_str().map(str::to_owned))
                .context("native edited message content requires a method")?;
            for (name, value) in fields {
                anyhow::ensure!(
                    !payload.contains_key(&name),
                    "native edit duplicates field {name:?}"
                );
                payload.insert(name, value);
            }
            payload.insert("reply_markup".to_owned(), reply_markup);
            if let Some(native_options) = native_options {
                merge_native_fields(&mut payload, native_options, "edit options")?;
            }
            let _: Value = bot.call(&method, Value::Object(payload)).await?;
            return Ok(());
        }
        _ => {
            return Err(UnsupportedInteractionError::new(
                "editing a Telegram message to this content kind",
            )
            .into())
        }
    };
    payload.insert("reply_markup".to_owned(), reply_markup);
    if let Some(native_options) = native_options {
        merge_native_fields(&mut payload, native_options, "edit options")?;
    }
    let _: Value = bot.call(method, Value::Object(payload)).await?;
    Ok(())
}

async fn telegram_edit_rich_layout(
    bot: &TelegramBot,
    mut payload: Map<String, Value>,
    address: TelegramEditAddress,
    mut layout: RichLayout,
    mut reply_markup: Value,
    native_options: Option<PlatformNativeData>,
) -> Result<()> {
    anyhow::ensure!(
        address != TelegramEditAddress::Ephemeral,
        "Telegram ephemeral messages cannot contain rich layouts"
    );
    let uploads = prepare_rich_layout_uploads(&mut layout).await?;
    anyhow::ensure!(
        uploads.is_empty() || address == TelegramEditAddress::Regular,
        "Telegram inline rich-message edits cannot upload new files"
    );
    let (rich_message, layout_markup) = telegram_input_rich_layout(layout)?;
    if let Some(layout_markup) = layout_markup {
        anyhow::ensure!(
            reply_markup == json!({"inline_keyboard": []}),
            "Telegram rich-layout actions conflict with MessageOptions components"
        );
        reply_markup = layout_markup;
    }
    payload.insert("reply_markup".to_owned(), reply_markup);
    if let Some(native_options) = native_options {
        merge_native_fields(&mut payload, native_options, "edit options")?;
    }
    if uploads.is_empty() {
        payload.insert("rich_message".to_owned(), rich_message);
        let _: Value = bot.call("editMessageText", Value::Object(payload)).await?;
    } else {
        let mut form =
            form_from_payload(payload)?.text("rich_message", serde_json::to_string(&rich_message)?);
        for (name, upload) in uploads {
            form = form.part(name, upload.into_part()?);
        }
        let _: Value = bot.client().call_multipart("editMessageText", form).await?;
    }
    Ok(())
}

async fn telegram_edit_media_v2(
    bot: &TelegramBot,
    mut payload: Map<String, Value>,
    address: TelegramEditAddress,
    kind: MediaType,
    media: Media,
    reply_markup: Value,
    native_options: Option<PlatformNativeData>,
) -> Result<()> {
    let telegram_type = match kind {
        MediaType::Image => "photo",
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        MediaType::Document => "document",
        MediaType::Animation => "animation",
        _ => unreachable!("unsupported editable media kind"),
    };
    let send_method = match telegram_type {
        "photo" => "sendPhoto",
        "video" => "sendVideo",
        "audio" => "sendAudio",
        "document" => "sendDocument",
        "animation" => "sendAnimation",
        _ => unreachable!(),
    };
    let mut input = Map::new();
    input.insert("type".to_owned(), telegram_type.into());
    add_media_fields(&mut input, send_method, &media)?;
    if let Some(native) = media.platform_data.clone() {
        merge_native_fields(&mut input, native, "edited media")?;
    }
    let source = resolve_file(media.file).await?;
    let thumbnail = media.thumbnail;
    payload.insert("reply_markup".to_owned(), reply_markup);
    if let Some(native_options) = native_options {
        merge_native_fields(&mut payload, native_options, "edit options")?;
    }
    let method = if address == TelegramEditAddress::Ephemeral {
        "editEphemeralMessageMedia"
    } else {
        "editMessageMedia"
    };

    match source {
        ResolvedFile::Remote(file) => {
            input.insert("media".to_owned(), file.into());
            if let Some(thumbnail) = thumbnail {
                match resolve_file(thumbnail).await? {
                    ResolvedFile::Remote(file) => {
                        input.insert("thumbnail".to_owned(), file.into());
                    }
                    ResolvedFile::Upload(_) => {
                        return Err(anyhow::anyhow!(
                            "a local Telegram thumbnail requires a local primary media file"
                        ));
                    }
                }
            }
            payload.insert("media".to_owned(), Value::Object(input));
            let _: Value = bot.call(method, Value::Object(payload)).await?;
        }
        ResolvedFile::Upload(upload) => {
            anyhow::ensure!(
                address == TelegramEditAddress::Regular,
                "Telegram inline and ephemeral message edits cannot upload new files"
            );
            input.insert("media".to_owned(), "attach://media_file".into());
            let mut thumbnail_upload = None;
            if let Some(thumbnail) = thumbnail {
                match resolve_file(thumbnail).await? {
                    ResolvedFile::Upload(upload) => {
                        input.insert("thumbnail".to_owned(), "attach://thumbnail".into());
                        thumbnail_upload = Some(upload);
                    }
                    ResolvedFile::Remote(file) => {
                        input.insert("thumbnail".to_owned(), file.into());
                    }
                }
            }
            let mut form = form_from_payload(payload)?
                .text("media", serde_json::to_string(&Value::Object(input))?)
                .part("media_file", upload.into_part()?);
            if let Some(upload) = thumbnail_upload {
                form = form.part("thumbnail", upload.into_part()?);
            }
            let _: Value = bot.client().call_multipart(method, form).await?;
        }
    }
    Ok(())
}

fn form_from_payload(payload: Map<String, Value>) -> Result<Form> {
    let mut form = Form::new();
    for (name, value) in payload {
        if value.is_null() {
            continue;
        }
        form = form.text(name, platform_form_value(value)?);
    }
    Ok(form)
}

async fn send_telegram_poll(
    bot: &TelegramBot,
    poll: Poll,
    mut payload: Map<String, Value>,
) -> Result<Message> {
    let option_count = poll.options.len();
    anyhow::ensure!(
        (2..=12).contains(&option_count),
        "Telegram polls require 2 to 12 options"
    );
    anyhow::ensure!(
        (1..=300).contains(&poll.question.text.chars().count()),
        "Telegram poll questions must contain 1 to 300 characters"
    );
    anyhow::ensure!(
        poll.options
            .iter()
            .all(|option| (1..=100).contains(&option.text.text.chars().count())),
        "Telegram poll options must contain 1 to 100 characters"
    );
    anyhow::ensure!(
        poll.explanation
            .as_ref()
            .is_none_or(|explanation| explanation.text.chars().count() <= 200),
        "Telegram poll explanations are limited to 200 characters"
    );
    anyhow::ensure!(
        poll.open_for
            .is_none_or(|duration| (5..=600).contains(&duration.as_secs())),
        "Telegram poll open periods must be between 5 and 600 seconds"
    );
    anyhow::ensure!(
        poll.country_codes.iter().all(|country| {
            country == "FT"
                || (country.len() == 2
                    && country
                        .chars()
                        .all(|character| character.is_ascii_uppercase()))
        }),
        "Telegram poll country codes must be uppercase ISO 3166-1 alpha-2 codes or FT"
    );
    payload.insert("question".to_owned(), poll.question.text.clone().into());
    payload.insert(
        "question_entities".to_owned(),
        Value::Array(telegram_rich_text_entities(&poll.question)?),
    );
    payload.insert(
        "options".to_owned(),
        Value::Array(
            poll.options
                .into_iter()
                .map(|option| {
                    let mut value = json!({
                        "text": option.text.text,
                        "text_entities": telegram_rich_text_entities(&option.text)?,
                    });
                    if let Some(native) = option.platform_data {
                        let Value::Object(fields) = telegram_native_object(native, "poll option")?
                        else {
                            unreachable!("native object helper always returns an object")
                        };
                        let Value::Object(object) = &mut value else {
                            unreachable!("poll option is an object")
                        };
                        for (name, value) in fields {
                            anyhow::ensure!(
                                !object.contains_key(&name),
                                "Telegram poll option duplicates field {name:?}"
                            );
                            object.insert(name, value);
                        }
                    }
                    Ok(value)
                })
                .collect::<Result<Vec<_>>>()?,
        ),
    );
    payload.insert(
        "is_anonymous".to_owned(),
        poll.anonymous.unwrap_or(true).into(),
    );
    payload.insert(
        "type".to_owned(),
        match poll.kind {
            PollType::Regular => "regular".into(),
            PollType::Quiz => "quiz".into(),
            PollType::PlatformNative(kind) => kind.into(),
        },
    );
    payload.insert(
        "allows_multiple_answers".to_owned(),
        poll.allows_multiple_answers.into(),
    );
    payload.insert("allows_revoting".to_owned(), poll.allows_revoting.into());
    payload.insert("members_only".to_owned(), poll.members_only.into());
    payload.insert("country_codes".to_owned(), poll.country_codes.into());
    payload.insert(
        "open_period".to_owned(),
        poll.open_for.map(|duration| duration.as_secs()).into(),
    );
    anyhow::ensure!(
        poll.open_for.is_none() || poll.closes_at.is_none(),
        "Telegram polls cannot use open_for and closes_at together"
    );
    let correct_options = if poll.correct_options.is_empty() {
        poll.correct_option.into_iter().collect::<Vec<_>>()
    } else {
        anyhow::ensure!(
            poll.correct_option
                .is_none_or(|option| poll.correct_options.contains(&option)),
            "poll correct_option conflicts with correct_options"
        );
        poll.correct_options
    };
    anyhow::ensure!(
        correct_options.iter().all(|option| *option < option_count),
        "Telegram poll correct-option indices are out of range"
    );
    payload.insert(
        "correct_option_ids".to_owned(),
        (!correct_options.is_empty())
            .then_some(correct_options)
            .into(),
    );
    if let Some(explanation) = poll.explanation {
        payload.insert("explanation".to_owned(), explanation.text.clone().into());
        payload.insert(
            "explanation_entities".to_owned(),
            Value::Array(telegram_rich_text_entities(&explanation)?),
        );
    }
    payload.insert(
        "close_date".to_owned(),
        poll.closes_at.map(|date| date.timestamp()).into(),
    );
    payload.insert("is_closed".to_owned(), poll.closed.into());
    if let Some(native) = poll.platform_data {
        merge_native_fields(&mut payload, native, "poll")?;
    }
    bot.call("sendPoll", Value::Object(payload)).await
}

fn telegram_poll(value: Value) -> Result<Poll> {
    let object = value
        .as_object()
        .context("Telegram returned an invalid poll object")?;
    let options = object
        .get("options")
        .and_then(Value::as_array)
        .context("Telegram poll has no options")?
        .iter()
        .map(|option| {
            let option_object = option
                .as_object()
                .context("Telegram returned an invalid poll option")?;
            Ok(PollOption {
                id: None,
                text: RichText::plain(
                    option_object
                        .get("text")
                        .and_then(Value::as_str)
                        .context("Telegram poll option has no text")?,
                ),
                voter_count: option_object.get("voter_count").and_then(Value::as_u64),
                platform_data: Some(PlatformNativeData::new(crate::SERVER, option.clone())),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let correct_options = object
        .get("correct_option_ids")
        .and_then(Value::as_array)
        .and_then(|options| options.first())
        .and_then(Value::as_u64)
        .and_then(|option| usize::try_from(option).ok());
    let kind = match object.get("type").and_then(Value::as_str) {
        Some("regular") | None => PollType::Regular,
        Some("quiz") => PollType::Quiz,
        Some(kind) => PollType::PlatformNative(kind.to_owned()),
    };
    Ok(Poll {
        id: object.get("id").and_then(Value::as_str).map(str::to_owned),
        question: RichText::plain(
            object
                .get("question")
                .and_then(Value::as_str)
                .context("Telegram poll has no question")?,
        ),
        options,
        allows_multiple_answers: object
            .get("allows_multiple_answers")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        allows_revoting: object.get("allows_revoting").and_then(Value::as_bool),
        members_only: object.get("members_only").and_then(Value::as_bool),
        country_codes: object
            .get("country_codes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        anonymous: object.get("is_anonymous").and_then(Value::as_bool),
        kind,
        open_for: object
            .get("open_period")
            .and_then(Value::as_u64)
            .map(Duration::from_secs),
        closes_at: object
            .get("close_date")
            .and_then(Value::as_i64)
            .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        correct_option: correct_options,
        correct_options: object
            .get("correct_option_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .filter_map(|option| usize::try_from(option).ok())
            .collect(),
        total_voter_count: object.get("total_voter_count").and_then(Value::as_u64),
        explanation: object
            .get("explanation")
            .and_then(Value::as_str)
            .map(RichText::plain),
        closed: object
            .get("is_closed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        platform_data: Some(PlatformNativeData::new(crate::SERVER, value)),
    })
}

fn telegram_input_checklist(checklist: Checklist) -> Result<Value> {
    anyhow::ensure!(
        (1..=30).contains(&checklist.tasks.len()),
        "Telegram checklists require 1 to 30 tasks"
    );
    anyhow::ensure!(
        (1..=255).contains(&checklist.title.text.chars().count()),
        "Telegram checklist titles must contain 1 to 255 characters"
    );
    anyhow::ensure!(
        checklist.tasks.iter().all(|task| {
            (1..=100).contains(&task.text.text.chars().count())
                && !task.completed
                && task.completed_by.is_none()
                && task.completed_at.is_none()
        }),
        "Telegram checklist tasks must contain 1 to 100 characters and cannot set completion state"
    );
    let mut task_ids = BTreeSet::new();
    let tasks = checklist
        .tasks
        .into_iter()
        .enumerate()
        .map(|(index, task)| {
            let id = task
                .id
                .as_deref()
                .map(str::parse::<i64>)
                .transpose()
                .context("invalid Telegram checklist task id")?
                .unwrap_or_else(|| i64::try_from(index + 1).expect("task count fits i64"));
            anyhow::ensure!(
                id > 0 && task_ids.insert(id),
                "Telegram checklist task ids must be positive and unique"
            );
            let mut value = json!({
                "id": id,
                "text": task.text.text,
                "text_entities": telegram_rich_text_entities(&task.text)?,
            });
            if let Some(native) = task.platform_data {
                let Value::Object(fields) = telegram_native_object(native, "checklist task")?
                else {
                    unreachable!("native object helper always returns an object")
                };
                let Value::Object(object) = &mut value else {
                    unreachable!("checklist task is an object")
                };
                for (name, value) in fields {
                    anyhow::ensure!(
                        !object.contains_key(&name),
                        "Telegram checklist task duplicates field {name:?}"
                    );
                    object.insert(name, value);
                }
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut value = json!({
        "title": checklist.title.text,
        "title_entities": telegram_rich_text_entities(&checklist.title)?,
        "tasks": tasks,
        "others_can_add_tasks": checklist.can_add_tasks,
        "others_can_mark_tasks_as_done": checklist.can_mark_tasks_done,
    });
    if let Some(native) = checklist.platform_data {
        let Value::Object(fields) = telegram_native_object(native, "checklist")? else {
            unreachable!("native object helper always returns an object")
        };
        let Value::Object(object) = &mut value else {
            unreachable!("checklist is an object")
        };
        for (name, value) in fields {
            anyhow::ensure!(
                !object.contains_key(&name),
                "Telegram checklist duplicates field {name:?}"
            );
            object.insert(name, value);
        }
    }
    strip_nulls(&mut value);
    Ok(value)
}

async fn send_telegram_checklist(
    bot: &TelegramBot,
    checklist: Checklist,
    mut payload: Map<String, Value>,
) -> Result<Message> {
    payload.insert("checklist".to_owned(), telegram_input_checklist(checklist)?);
    bot.call("sendChecklist", Value::Object(payload)).await
}

fn telegram_rich_text_value(text: &RichText) -> Result<Value> {
    if text.spans.is_empty() {
        return Ok(text.text.clone().into());
    }
    let mut boundaries = BTreeSet::from([0, text.text.len()]);
    for span in &text.spans {
        anyhow::ensure!(
            span.range.start < span.range.end
                && span.range.end <= text.text.len()
                && text.text.is_char_boundary(span.range.start)
                && text.text.is_char_boundary(span.range.end),
            "rich-text span is not on valid UTF-8 boundaries"
        );
        boundaries.insert(span.range.start);
        boundaries.insert(span.range.end);
    }
    let mut result = Vec::new();
    let boundaries = boundaries.into_iter().collect::<Vec<_>>();
    for range in boundaries.windows(2) {
        let [start, end] = range else { unreachable!() };
        if start == end {
            continue;
        }
        let source = &text.text[*start..*end];
        let mut value: Value = source.to_owned().into();
        let styles = text
            .spans
            .iter()
            .filter(|span| span.range.start <= *start && span.range.end >= *end)
            .flat_map(|span| span.styles.iter())
            .collect::<Vec<_>>();
        for style in styles.into_iter().rev() {
            value = telegram_rich_text_style(style, source, value)?;
        }
        result.push(value);
    }
    Ok(if result.len() == 1 {
        result.pop().expect("one rich-text item")
    } else {
        Value::Array(result)
    })
}

fn telegram_rich_text_style(style: &TextStyle, source: &str, text: Value) -> Result<Value> {
    Ok(match style {
        TextStyle::Bold => json!({"type": "bold", "text": text}),
        TextStyle::Italic => json!({"type": "italic", "text": text}),
        TextStyle::Underline => json!({"type": "underline", "text": text}),
        TextStyle::Strikethrough => json!({"type": "strikethrough", "text": text}),
        TextStyle::Spoiler => json!({"type": "spoiler", "text": text}),
        TextStyle::Code | TextStyle::Preformatted { .. } => {
            json!({"type": "code", "text": text})
        }
        TextStyle::Link { url } => json!({"type": "url", "text": text, "url": url}),
        TextStyle::UserMention { user_id } => json!({
            "type": "text_mention",
            "text": text,
            "user": {
                "id": user_id.parse::<i64>().context("invalid Telegram mentioned user id")?,
                "is_bot": false,
                "first_name": user_id,
            }
        }),
        TextStyle::CustomEmoji { id, fallback } => json!({
            "type": "custom_emoji",
            "custom_emoji_id": id,
            "alternative_text": fallback.as_deref().unwrap_or(source),
        }),
        TextStyle::Marked => json!({"type": "marked", "text": text}),
        TextStyle::Subscript => json!({"type": "subscript", "text": text}),
        TextStyle::Superscript => json!({"type": "superscript", "text": text}),
        TextStyle::MathematicalExpression => {
            json!({"type": "mathematical_expression", "expression": source})
        }
        TextStyle::Hashtag => json!({"type": "hashtag", "text": text, "hashtag": source}),
        TextStyle::Cashtag => json!({"type": "cashtag", "text": text, "cashtag": source}),
        TextStyle::BotCommand => {
            json!({"type": "bot_command", "text": text, "bot_command": source})
        }
        TextStyle::Email => {
            json!({"type": "email_address", "text": text, "email_address": source})
        }
        TextStyle::Phone => {
            json!({"type": "phone_number", "text": text, "phone_number": source})
        }
        TextStyle::DateTime { timestamp } => json!({
            "type": "date_time",
            "text": text,
            "unix_time": timestamp.map(|value| value.timestamp()),
        }),
        TextStyle::Reference { message } => json!({
            "type": "reference",
            "text": text,
            "name": message.as_ref().map(|message| message.id.as_str()).unwrap_or(source),
        }),
        TextStyle::Quote => json!({"type": "italic", "text": text}),
        TextStyle::RoleMention { .. } | TextStyle::ConversationMention { .. } => {
            return Err(UnsupportedInteractionError::new(
                "role or conversation mentions in Telegram rich text",
            )
            .into())
        }
        TextStyle::PlatformNative(native) => {
            let Value::Object(mut object) =
                telegram_native_object(native.clone(), "rich-text style")?
            else {
                unreachable!("native object helper always returns an object")
            };
            object.entry("text".to_owned()).or_insert(text);
            Value::Object(object)
        }
    })
}

fn remote_media_reference(media: &Media) -> Result<String> {
    if let Some(id) = &media.file.id {
        return Ok(id.clone());
    }
    if let Some(uri) = &media.file.uri {
        anyhow::ensure!(
            matches!(uri.scheme_str(), Some("http" | "https")),
            "Telegram rich-message media must use a file_id or HTTP(S) URL"
        );
        return Ok(uri.to_string());
    }
    Err(anyhow::anyhow!(
        "Telegram rich-message media uploads must be supplied through PlatformNativeData"
    ))
}

fn telegram_input_media(media: &Media, kind: &MediaType) -> Result<Value> {
    anyhow::ensure!(
        media.alt_text.is_none() && media.waveform.is_none(),
        "Telegram rich-message media cannot carry portable alt text or waveform data"
    );
    let mut value = json!({
        "type": match kind {
            MediaType::Image => "photo",
            MediaType::Video => "video",
            MediaType::Audio => "audio",
            MediaType::Document => "document",
            MediaType::Animation => "animation",
            MediaType::VoiceNote => "voice_note",
            MediaType::VideoNote => "video",
            MediaType::PlatformNative(kind) => kind,
        },
        "media": remote_media_reference(media)?,
        "caption": media.caption.as_ref().map(|caption| caption.text.clone()),
        "caption_entities": media.caption.as_ref()
            .map(telegram_rich_text_entities)
            .transpose()?,
        "thumbnail": media.thumbnail.as_ref().and_then(|thumbnail| {
            thumbnail.id.clone().or_else(|| thumbnail.uri.as_ref().map(ToString::to_string))
        }),
        "width": media.width,
        "height": media.height,
        "duration": media.duration.map(|duration| duration.as_secs()),
        "has_spoiler": media.spoiler,
    });
    if let Some(native) = &media.platform_data {
        let Value::Object(fields) = telegram_native_object(native.clone(), "rich-message media")?
        else {
            unreachable!("native object helper always returns an object")
        };
        let Value::Object(object) = &mut value else {
            unreachable!("input media is an object")
        };
        for (name, value) in fields {
            anyhow::ensure!(
                !object.contains_key(&name),
                "Telegram rich-message media duplicates field {name:?}"
            );
            object.insert(name, value);
        }
    }
    strip_nulls(&mut value);
    Ok(value)
}

fn layout_text(nodes: &[LayoutNode]) -> Result<RichText> {
    let mut text = String::new();
    for node in nodes {
        let part = match node {
            LayoutNode::Text(text) | LayoutNode::Quote(text) => text.text.as_str(),
            LayoutNode::Code { text, .. } => text.as_str(),
            _ => {
                return Err(UnsupportedInteractionError::new(
                    "non-text content inside a Telegram table cell",
                )
                .into())
            }
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(part);
    }
    Ok(RichText::plain(text))
}

fn telegram_layout_nodes(
    nodes: Vec<LayoutNode>,
    actions: &mut Vec<ActionRow>,
) -> Result<Vec<Value>> {
    let mut blocks = Vec::new();
    for node in nodes {
        match node {
            LayoutNode::Text(text) => blocks.push(json!({
                "type": "paragraph",
                "text": telegram_rich_text_value(&text)?,
            })),
            LayoutNode::Section {
                children,
                accessory,
                style,
            } => {
                anyhow::ensure!(
                    style == oxidebot::LayoutStyle::default(),
                    "Telegram cannot represent portable section styling without platform data"
                );
                blocks.extend(telegram_layout_nodes(children, actions)?);
                if let Some(accessory) = accessory {
                    blocks.extend(telegram_layout_nodes(vec![*accessory], actions)?);
                }
            }
            LayoutNode::Container { children, style } => {
                anyhow::ensure!(
                    style == oxidebot::LayoutStyle::default(),
                    "Telegram cannot represent portable container styling without platform data"
                );
                blocks.extend(telegram_layout_nodes(children, actions)?);
            }
            LayoutNode::Columns(columns) => {
                let cells = columns
                    .into_iter()
                    .map(|column| {
                        Ok(json!({
                            "text": telegram_rich_text_value(&layout_text(&column.nodes)?)?,
                            "align": "left",
                            "valign": "top",
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                blocks.push(json!({"type": "table", "cells": [cells]}));
            }
            LayoutNode::Grid { columns, children } => {
                let columns = usize::from(columns.max(1));
                let mut rows = Vec::new();
                for row in children.chunks(columns) {
                    rows.push(
                        row.iter()
                            .map(|node| {
                                Ok(json!({
                                    "text": telegram_rich_text_value(&layout_text(std::slice::from_ref(node))?)?,
                                    "align": "left",
                                    "valign": "top",
                                }))
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
                blocks.push(json!({"type": "table", "cells": rows}));
            }
            LayoutNode::Image(media) => blocks.push(json!({
                "type": "photo",
                "photo": telegram_input_media(&media, &MediaType::Image)?,
            })),
            LayoutNode::MediaGallery(items) => {
                let children = items
                    .into_iter()
                    .map(|item| {
                        Ok(json!({
                            "type": match item.kind {
                                MediaType::Image => "photo",
                                MediaType::Video => "video",
                                MediaType::Audio => "audio",
                                MediaType::Animation => "animation",
                                _ => return Err(UnsupportedInteractionError::new(
                                    "this media kind in a Telegram rich collage",
                                ).into()),
                            },
                            match item.kind {
                                MediaType::Image => "photo",
                                MediaType::Video => "video",
                                MediaType::Audio => "audio",
                                MediaType::Animation => "animation",
                                _ => unreachable!(),
                            }: telegram_input_media(&item.media, &item.kind)?,
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                blocks.push(json!({"type": "collage", "blocks": children}));
            }
            LayoutNode::File(_) => {
                return Err(UnsupportedInteractionError::new(
                    "file blocks in Telegram rich messages",
                )
                .into())
            }
            LayoutNode::List { ordered, items } => {
                let items = items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| {
                        Ok(json!({
                            "type": if ordered { Some("1") } else { None },
                            "value": if ordered { Some(index + 1) } else { None },
                            "blocks": telegram_layout_nodes(item, actions)?,
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                blocks.push(json!({"type": "list", "items": items}));
            }
            LayoutNode::Table { rows } => {
                let cells = rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|cell| {
                                Ok(json!({
                                    "text": telegram_rich_text_value(&layout_text(&cell.content)?)?,
                                    "is_header": cell.header,
                                    "colspan": cell.colspan,
                                    "rowspan": cell.rowspan,
                                    "align": "left",
                                    "valign": "top",
                                }))
                            })
                            .collect::<Result<Vec<_>>>()
                    })
                    .collect::<Result<Vec<_>>>()?;
                blocks.push(json!({"type": "table", "cells": cells}));
            }
            LayoutNode::Quote(text) => blocks.push(json!({
                "type": "pull_quotation",
                "text": telegram_rich_text_value(&text)?,
            })),
            LayoutNode::Code { text, language } => blocks.push(json!({
                "type": "preformatted",
                "text": text,
                "language": language,
            })),
            LayoutNode::Divider => blocks.push(json!({"type": "divider"})),
            LayoutNode::Map(location) => {
                let native = location
                    .platform_data
                    .map(|native| telegram_native_object(native, "rich-message map"))
                    .transpose()?;
                let zoom = native
                    .as_ref()
                    .and_then(|native| native.get("zoom"))
                    .and_then(Value::as_u64)
                    .unwrap_or(13);
                let width = native
                    .as_ref()
                    .and_then(|native| native.get("width"))
                    .and_then(Value::as_u64)
                    .unwrap_or(320);
                let height = native
                    .as_ref()
                    .and_then(|native| native.get("height"))
                    .and_then(Value::as_u64)
                    .unwrap_or(200);
                blocks.push(json!({
                    "type": "map",
                    "location": {
                        "latitude": location.latitude,
                        "longitude": location.longitude,
                        "horizontal_accuracy": location.horizontal_accuracy,
                    },
                    "zoom": zoom,
                    "width": width,
                    "height": height,
                }));
            }
            LayoutNode::Details {
                summary,
                children,
                open,
            } => blocks.push(json!({
                "type": "details",
                "summary": telegram_rich_text_value(&summary)?,
                "blocks": telegram_layout_nodes(children, actions)?,
                "is_open": open,
            })),
            LayoutNode::Actions(row) => actions.push(row),
            LayoutNode::PlatformNative(native) => {
                blocks.push(telegram_native_object(native, "rich-message block")?)
            }
        }
    }
    Ok(blocks)
}

async fn send_telegram_rich_layout(
    bot: &TelegramBot,
    mut layout: RichLayout,
    mut payload: Map<String, Value>,
) -> Result<Message> {
    anyhow::ensure!(
        !payload.contains_key("receiver_user_id") && !payload.contains_key("callback_query_id"),
        "Telegram rich messages cannot be ephemeral"
    );
    let uploads = prepare_rich_layout_uploads(&mut layout).await?;
    let (rich_message, actions) = telegram_input_rich_layout(layout)?;
    if let Some(actions) = actions {
        anyhow::ensure!(
            !payload.contains_key("reply_markup"),
            "Telegram rich layout actions conflict with MessageOptions components"
        );
        payload.insert("reply_markup".to_owned(), actions);
    }
    payload.insert("rich_message".to_owned(), rich_message);
    if uploads.is_empty() {
        bot.call("sendRichMessage", Value::Object(payload)).await
    } else {
        let mut form = form_from_payload(payload)?;
        for (name, upload) in uploads {
            form = form.part(name, upload.into_part()?);
        }
        bot.client().call_multipart("sendRichMessage", form).await
    }
}

async fn prepare_rich_layout_uploads(layout: &mut RichLayout) -> Result<Vec<(String, Upload)>> {
    let mut uploads = Vec::new();
    let mut stack = layout.nodes.iter_mut().collect::<Vec<_>>();
    while let Some(node) = stack.pop() {
        match node {
            LayoutNode::Section {
                children,
                accessory,
                ..
            } => {
                stack.extend(children.iter_mut());
                if let Some(accessory) = accessory {
                    stack.push(accessory.as_mut());
                }
            }
            LayoutNode::Container { children, .. }
            | LayoutNode::Grid { children, .. }
            | LayoutNode::Details { children, .. } => stack.extend(children.iter_mut()),
            LayoutNode::Columns(columns) => {
                for column in columns {
                    stack.extend(column.nodes.iter_mut());
                }
            }
            LayoutNode::Image(media) | LayoutNode::File(media) => {
                prepare_rich_media_upload(media, &mut uploads).await?;
            }
            LayoutNode::MediaGallery(items) => {
                for item in items {
                    prepare_rich_media_upload(&mut item.media, &mut uploads).await?;
                }
            }
            LayoutNode::List { items, .. } => {
                for item in items {
                    stack.extend(item.iter_mut());
                }
            }
            LayoutNode::Table { rows } => {
                for row in rows {
                    for cell in row {
                        stack.extend(cell.content.iter_mut());
                    }
                }
            }
            LayoutNode::Text(_)
            | LayoutNode::Quote(_)
            | LayoutNode::Code { .. }
            | LayoutNode::Divider
            | LayoutNode::Map(_)
            | LayoutNode::Actions(_)
            | LayoutNode::PlatformNative(_) => {}
        }
    }
    Ok(uploads)
}

async fn prepare_rich_media_upload(
    media: &mut Media,
    uploads: &mut Vec<(String, Upload)>,
) -> Result<()> {
    let source = resolve_file(std::mem::take(&mut media.file)).await?;
    media.file = match source {
        ResolvedFile::Remote(file) => File {
            id: Some(file),
            ..Default::default()
        },
        ResolvedFile::Upload(upload) => {
            let name = format!("rich_media_{}", uploads.len());
            uploads.push((name.clone(), upload));
            File {
                id: Some(format!("attach://{name}")),
                ..Default::default()
            }
        }
    };
    if let Some(thumbnail) = media.thumbnail.take() {
        media.thumbnail = Some(match resolve_file(thumbnail).await? {
            ResolvedFile::Remote(file) => File {
                id: Some(file),
                ..Default::default()
            },
            ResolvedFile::Upload(upload) => {
                let name = format!("rich_media_{}", uploads.len());
                uploads.push((name.clone(), upload));
                File {
                    id: Some(format!("attach://{name}")),
                    ..Default::default()
                }
            }
        });
    }
    Ok(())
}

async fn attach_caption_or_send_text(
    bot: &TelegramBot,
    chat_id: &str,
    outgoing: &mut LegacyOutgoingMessage,
    reply: &mut Option<ReplyParameters>,
    reply_markup: &mut Option<Value>,
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
        reply_markup,
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
    reply_markup: &mut Option<Value>,
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
                    "reply_markup": reply_markup.take(),
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
    reply_markup: &mut Option<Value>,
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
                let response = send_single_media(
                    bot,
                    chat_id,
                    chunk[0].clone(),
                    reply.take(),
                    reply_markup.take(),
                )
                .await?;
                sent.push(sent_response(response));
            } else {
                let responses = send_media_group(
                    bot,
                    chat_id,
                    chunk.to_vec(),
                    reply.take(),
                    reply_markup.take(),
                )
                .await?;
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
    reply_markup: Option<Value>,
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
                    "reply_markup": reply_markup,
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
            form = form_optional_json(form, "reply_markup", reply_markup)?;
            bot.client().call_multipart(method, form).await
        }
    }
}

async fn send_media_group(
    bot: &TelegramBot,
    chat_id: &str,
    media: Vec<OutgoingMedia>,
    reply: Option<ReplyParameters>,
    reply_markup: Option<Value>,
) -> Result<Vec<Message>> {
    if let Some(reply_markup) = &reply_markup {
        anyhow::ensure!(
            reply_markup.get("inline_keyboard").is_some(),
            "Telegram media groups can be followed only by an inline keyboard; reply keyboards require a separate message"
        );
    }
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

    let messages: Vec<Message> = if uploads.is_empty() {
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
    }?;
    if let (Some(reply_markup), Some(first)) = (reply_markup, messages.first()) {
        let _: Value = bot
            .call(
                "editMessageReplyMarkup",
                json!({
                    "chat_id": chat_id_value(chat_id),
                    "message_id": first.message_id,
                    "reply_markup": reply_markup,
                }),
            )
            .await?;
    }
    Ok(messages)
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

fn telegram_message_components(components: MessageComponents) -> Result<Value> {
    let mut value = match components {
        MessageComponents::InlineKeyboard(keyboard) => {
            json!({"inline_keyboard": telegram_inline_keyboard(keyboard)?})
        }
        MessageComponents::ReplyKeyboard(keyboard) => telegram_reply_keyboard(keyboard)?,
        MessageComponents::RemoveReplyKeyboard { selective } => json!({
            "remove_keyboard": true,
            "selective": selective.then_some(true),
        }),
        MessageComponents::ForceReply {
            input_field_placeholder,
            selective,
        } => {
            validate_placeholder(input_field_placeholder.as_deref(), "force-reply")?;
            json!({
                "force_reply": true,
                "input_field_placeholder": input_field_placeholder,
                "selective": selective.then_some(true),
            })
        }
        MessageComponents::PlatformNative(native) => {
            telegram_native_object(native, "message components")?
        }
    };
    strip_nulls(&mut value);
    Ok(value)
}

fn telegram_editable_components(components: MessageComponents) -> Result<Value> {
    match components {
        MessageComponents::InlineKeyboard(keyboard) => {
            Ok(json!({"inline_keyboard": telegram_inline_keyboard(keyboard)?}))
        }
        MessageComponents::PlatformNative(native) => {
            let value = telegram_native_object(native, "editable message components")?;
            anyhow::ensure!(
                value.get("inline_keyboard").is_some(),
                "Telegram can edit only inline keyboards"
            );
            Ok(value)
        }
        _ => Err(UnsupportedInteractionError::new("editing Telegram reply keyboards").into()),
    }
}

fn telegram_inline_keyboard(
    keyboard: oxidebot::interaction::InlineKeyboard,
) -> Result<Vec<Vec<Value>>> {
    anyhow::ensure!(
        !keyboard.rows.is_empty(),
        "Telegram inline keyboard must contain at least one row"
    );
    keyboard
        .rows
        .into_iter()
        .enumerate()
        .map(|(row_index, row)| {
            anyhow::ensure!(
                !row.components.is_empty(),
                "Telegram inline keyboard row cannot be empty"
            );
            row.components
                .into_iter()
                .enumerate()
                .map(|(column_index, component)| match component {
                    InteractionComponent::Button(button) => {
                        telegram_inline_button(button, row_index, column_index)
                    }
                    InteractionComponent::PlatformNative(native) => {
                        telegram_native_object(native, "inline keyboard button")
                    }
                    InteractionComponent::Select(_) => {
                        Err(UnsupportedInteractionError::new("Telegram inline select menus").into())
                    }
                    InteractionComponent::Input(_) => Err(UnsupportedInteractionError::new(
                        "Telegram inline input components",
                    )
                    .into()),
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect()
}

fn telegram_inline_button(button: Button, row: usize, column: usize) -> Result<Value> {
    anyhow::ensure!(!button.label.is_empty(), "button label cannot be empty");
    anyhow::ensure!(
        !button.disabled,
        "Telegram inline buttons cannot be disabled"
    );
    let (label, custom_icon) = telegram_button_label(button.label, button.icon)?;
    let mut object = Map::new();
    object.insert("text".to_owned(), label.into());
    if let Some(custom_icon) = custom_icon {
        object.insert("icon_custom_emoji_id".to_owned(), custom_icon.into());
    }
    if let Some(style) = telegram_button_style(button.style) {
        object.insert("style".to_owned(), style.into());
    }

    match button.action {
        ButtonAction::Callback { data } => {
            anyhow::ensure!(
                (1..=64).contains(&data.len()),
                "Telegram callback data must contain 1 to 64 bytes"
            );
            object.insert("callback_data".to_owned(), data.into());
        }
        ButtonAction::SendText { .. } => {
            return Err(UnsupportedInteractionError::new(
                "send-text actions on Telegram inline buttons",
            )
            .into())
        }
        ButtonAction::Url { url } => {
            validate_button_url(&url)?;
            object.insert("url".to_owned(), url.into());
        }
        ButtonAction::WebApp { url } => {
            validate_https_url(&url, "Web App URL")?;
            object.insert("web_app".to_owned(), json!({"url": url}));
        }
        ButtonAction::Login(login) => {
            validate_https_url(&login.url, "login URL")?;
            object.insert(
                "login_url".to_owned(),
                json!({
                    "url": login.url,
                    "forward_text": login.forward_text,
                    "bot_username": login.bot_username,
                    "request_write_access": login.request_write_access.then_some(true),
                }),
            );
        }
        ButtonAction::SwitchInlineQuery { query, target } => match target {
            InlineQueryTarget::ChooseChat => {
                object.insert("switch_inline_query".to_owned(), query.into());
            }
            InlineQueryTarget::CurrentChat => {
                object.insert("switch_inline_query_current_chat".to_owned(), query.into());
            }
            InlineQueryTarget::ChosenChat(criteria) => {
                object.insert(
                    "switch_inline_query_chosen_chat".to_owned(),
                    json!({
                        "query": query,
                        "allow_user_chats": criteria.allow_user_chats,
                        "allow_bot_chats": criteria.allow_bot_chats,
                        "allow_group_chats": criteria.allow_group_chats,
                        "allow_channel_chats": criteria.allow_channel_chats,
                    }),
                );
            }
        },
        ButtonAction::CopyText { text } => {
            anyhow::ensure!(
                (1..=256).contains(&text.chars().count()),
                "Telegram copied text must contain 1 to 256 characters"
            );
            object.insert("copy_text".to_owned(), json!({"text": text}));
        }
        ButtonAction::Game => {
            anyhow::ensure!(
                row == 0 && column == 0,
                "Telegram game button must be the first button in the first row"
            );
            object.insert("callback_game".to_owned(), json!({}));
        }
        ButtonAction::Pay => {
            anyhow::ensure!(
                row == 0 && column == 0,
                "Telegram pay button must be the first button in the first row"
            );
            object.insert("pay".to_owned(), true.into());
        }
        ButtonAction::PlatformNative(native) => {
            merge_native_fields(&mut object, native, "inline button action")?;
        }
    }
    let mut value = Value::Object(object);
    strip_nulls(&mut value);
    Ok(value)
}

fn telegram_reply_keyboard(keyboard: ReplyKeyboard) -> Result<Value> {
    anyhow::ensure!(
        !keyboard.rows.is_empty(),
        "Telegram reply keyboard must contain at least one row"
    );
    validate_placeholder(
        keyboard.input_field_placeholder.as_deref(),
        "reply-keyboard",
    )?;
    let rows = keyboard
        .rows
        .into_iter()
        .map(|row| {
            anyhow::ensure!(
                !row.buttons.is_empty(),
                "Telegram reply keyboard row cannot be empty"
            );
            row.buttons
                .into_iter()
                .map(telegram_reply_button)
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let mut value = json!({
        "keyboard": rows,
        "is_persistent": keyboard.persistent.then_some(true),
        "resize_keyboard": keyboard.resize.then_some(true),
        "one_time_keyboard": keyboard.one_time.then_some(true),
        "input_field_placeholder": keyboard.input_field_placeholder,
        "selective": keyboard.selective.then_some(true),
    });
    strip_nulls(&mut value);
    Ok(value)
}

fn telegram_reply_button(button: ReplyButton) -> Result<Value> {
    anyhow::ensure!(!button.label.is_empty(), "button label cannot be empty");
    let (label, custom_icon) = telegram_button_label(button.label, button.icon)?;
    let mut object = Map::new();
    object.insert("text".to_owned(), label.into());
    if let Some(custom_icon) = custom_icon {
        object.insert("icon_custom_emoji_id".to_owned(), custom_icon.into());
    }
    if let Some(style) = telegram_button_style(button.style) {
        object.insert("style".to_owned(), style.into());
    }

    match button.action {
        ReplyButtonAction::SendText => {}
        ReplyButtonAction::RequestUsers(request) => {
            if let Some(max_quantity) = request.max_quantity {
                anyhow::ensure!(
                    (1..=10).contains(&max_quantity),
                    "Telegram request-users quantity must be between 1 and 10"
                );
            }
            object.insert(
                "request_users".to_owned(),
                json!({
                    "request_id": request.request_id,
                    "user_is_bot": request.user_is_bot,
                    "user_is_premium": request.user_is_premium,
                    "max_quantity": request.max_quantity,
                    "request_name": request.request_name.then_some(true),
                    "request_username": request.request_username.then_some(true),
                    "request_photo": request.request_photo.then_some(true),
                }),
            );
        }
        ReplyButtonAction::RequestChat(request) => {
            let mut request_chat = json!({
                "request_id": request.request_id,
                "chat_is_channel": request.chat_is_channel,
                "chat_is_forum": request.chat_is_forum,
                "chat_has_username": request.chat_has_username,
                "chat_is_created": request.chat_is_created,
                "bot_is_member": request.bot_is_member.then_some(true),
                "request_title": request.request_title.then_some(true),
                "request_username": request.request_username.then_some(true),
                "request_photo": request.request_photo.then_some(true),
            });
            strip_nulls(&mut request_chat);
            if let Some(native) = request.platform_data {
                let Value::Object(native) =
                    telegram_native_object(native, "request-chat criteria")?
                else {
                    unreachable!("native object helper always returns an object")
                };
                let Value::Object(object) = &mut request_chat else {
                    unreachable!("request_chat is an object")
                };
                for (name, value) in native {
                    anyhow::ensure!(
                        !object.contains_key(&name),
                        "request-chat criteria duplicates field {name:?}"
                    );
                    object.insert(name, value);
                }
            }
            object.insert("request_chat".to_owned(), request_chat);
        }
        ReplyButtonAction::RequestManagedBot(request) => {
            object.insert(
                "request_managed_bot".to_owned(),
                json!({
                    "request_id": request.request_id,
                    "suggested_name": request.suggested_name,
                    "suggested_username": request.suggested_username,
                }),
            );
        }
        ReplyButtonAction::RequestContact => {
            object.insert("request_contact".to_owned(), true.into());
        }
        ReplyButtonAction::RequestLocation => {
            object.insert("request_location".to_owned(), true.into());
        }
        ReplyButtonAction::RequestPoll { kind } => {
            object.insert(
                "request_poll".to_owned(),
                json!({
                    "type": kind.map(|kind| match kind {
                        PollKind::Regular => "regular",
                        PollKind::Quiz => "quiz",
                    }),
                }),
            );
        }
        ReplyButtonAction::WebApp { url } => {
            validate_https_url(&url, "Web App URL")?;
            object.insert("web_app".to_owned(), json!({"url": url}));
        }
        ReplyButtonAction::PlatformNative(native) => {
            merge_native_fields(&mut object, native, "reply button action")?;
        }
    }
    let mut value = Value::Object(object);
    strip_nulls(&mut value);
    Ok(value)
}

fn telegram_button_label(
    label: String,
    icon: Option<ButtonIcon>,
) -> Result<(String, Option<String>)> {
    Ok(match icon {
        Some(ButtonIcon::Emoji(emoji)) => {
            anyhow::ensure!(!emoji.is_empty(), "button emoji cannot be empty");
            (format!("{emoji} {label}"), None)
        }
        Some(ButtonIcon::Custom(id)) => {
            anyhow::ensure!(!id.is_empty(), "custom button emoji id cannot be empty");
            (label, Some(id))
        }
        None => (label, None),
    })
}

fn validate_placeholder(placeholder: Option<&str>, context: &str) -> Result<()> {
    if let Some(placeholder) = placeholder {
        anyhow::ensure!(
            (1..=64).contains(&placeholder.chars().count()),
            "Telegram {context} placeholder must contain 1 to 64 characters"
        );
    }
    Ok(())
}

fn validate_button_url(url: &str) -> Result<()> {
    let uri = Uri::from_str(url).context("invalid Telegram button URL")?;
    anyhow::ensure!(
        matches!(uri.scheme_str(), Some("http" | "https" | "tg")) && uri.authority().is_some(),
        "Telegram button URL must use http, https, or tg"
    );
    Ok(())
}

fn validate_https_url(url: &str, context: &str) -> Result<()> {
    let uri = Uri::from_str(url).with_context(|| format!("invalid Telegram {context}"))?;
    anyhow::ensure!(
        uri.scheme_str() == Some("https") && uri.authority().is_some(),
        "Telegram {context} must be an HTTPS URL"
    );
    Ok(())
}

fn telegram_button_style(style: ButtonStyle) -> Option<&'static str> {
    match style {
        ButtonStyle::Default | ButtonStyle::Secondary => None,
        ButtonStyle::Primary => Some("primary"),
        ButtonStyle::Success => Some("success"),
        ButtonStyle::Danger => Some("danger"),
    }
}

fn telegram_native_object(native: PlatformNativeData, context: &str) -> Result<Value> {
    anyhow::ensure!(
        native.platform == crate::SERVER,
        "{context} targets platform {:?}, not Telegram",
        native.platform
    );
    anyhow::ensure!(native.data.is_object(), "{context} must be a JSON object");
    Ok(native.data)
}

fn merge_native_fields(
    destination: &mut Map<String, Value>,
    native: PlatformNativeData,
    context: &str,
) -> Result<()> {
    let Value::Object(fields) = telegram_native_object(native, context)? else {
        unreachable!("native object helper always returns an object")
    };
    for (name, value) in fields {
        anyhow::ensure!(
            !destination.contains_key(&name),
            "{context} duplicates field {name:?}"
        );
        destination.insert(name, value);
    }
    Ok(())
}

fn telegram_command_scope(scope: &CommandScope) -> Result<Value> {
    Ok(match scope {
        CommandScope::Default => json!({"type": "default"}),
        CommandScope::AllPrivateChats => json!({"type": "all_private_chats"}),
        CommandScope::AllGroupChats => json!({"type": "all_group_chats"}),
        CommandScope::AllChatAdministrators => {
            json!({"type": "all_chat_administrators"})
        }
        CommandScope::Chat { chat_id } => {
            json!({"type": "chat", "chat_id": chat_id_value(chat_id)})
        }
        CommandScope::ChatAdministrators { chat_id } => {
            json!({"type": "chat_administrators", "chat_id": chat_id_value(chat_id)})
        }
        CommandScope::ChatMember { chat_id, user_id } => json!({
            "type": "chat_member",
            "chat_id": chat_id_value(chat_id),
            "user_id": user_id.parse::<i64>().context("invalid Telegram user id")?,
        }),
        CommandScope::PlatformNative(native) => {
            telegram_native_object(native.clone(), "command scope")?
        }
    })
}

fn validate_commands(commands: &[BotCommand]) -> Result<()> {
    for command in commands {
        anyhow::ensure!(
            (1..=32).contains(&command.command.len())
                && command
                    .command
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
            "invalid Telegram bot command {:?}; use 1-32 lowercase ASCII letters, digits, or underscores",
            command.command
        );
        anyhow::ensure!(
            (1..=256).contains(&command.description.chars().count()),
            "Telegram bot command descriptions must contain 1 to 256 characters"
        );
    }
    Ok(())
}

fn telegram_definition_scope(commands: &[CommandDefinition]) -> Result<CommandScope> {
    let contexts = commands
        .first()
        .map(|command| command.contexts.clone())
        .unwrap_or_default();
    anyhow::ensure!(
        commands.iter().all(|command| command.contexts == contexts),
        "Telegram requires all commands in one set to use the same scope"
    );
    anyhow::ensure!(
        !contexts
            .iter()
            .any(|context| matches!(context, CommandContext::PlatformNative(_))),
        "Telegram platform-native command contexts require the legacy scoped command API"
    );
    Ok(if contexts == BTreeSet::from([CommandContext::Direct]) {
        CommandScope::AllPrivateChats
    } else if contexts == BTreeSet::from([CommandContext::Administrator]) {
        CommandScope::AllChatAdministrators
    } else if !contexts.is_empty()
        && contexts
            .iter()
            .all(|context| matches!(context, CommandContext::Group | CommandContext::Channel))
    {
        CommandScope::AllGroupChats
    } else {
        CommandScope::Default
    })
}

fn telegram_commands_for_locale(
    commands: &[CommandDefinition],
    locale: Option<&str>,
) -> Result<Vec<BotCommand>> {
    let commands = commands
        .iter()
        .map(|command| {
            let name = locale
                .and_then(|locale| command.name.translations.get(locale))
                .unwrap_or(&command.name.default)
                .clone();
            let description = locale
                .and_then(|locale| command.description.translations.get(locale))
                .unwrap_or(&command.description.default)
                .clone();
            Ok(BotCommand::new(name, description).ephemeral(command.ephemeral))
        })
        .collect::<Result<Vec<_>>>()?;
    validate_commands(&commands)?;
    Ok(commands)
}

fn telegram_inline_query_result(suggestion: Suggestion) -> Result<Value> {
    if let Some(native) = suggestion.platform_data {
        let mut result = telegram_native_object(native, "inline query result")?;
        let Value::Object(object) = &mut result else {
            unreachable!("native object helper always returns an object")
        };
        object
            .entry("id".to_owned())
            .or_insert(suggestion.id.into());
        return Ok(result);
    }
    anyhow::ensure!(
        !suggestion.id.is_empty(),
        "inline query result id cannot be empty"
    );
    let (input_message_content, reply_markup) = if let Some(message) = suggestion.message {
        telegram_inline_input_message(message)?
    } else {
        let text = telegram_form_value_text(suggestion.value)?;
        (
            json!({"message_text": if text.is_empty() { suggestion.title.clone() } else { text }}),
            None,
        )
    };
    let thumbnail_url = suggestion
        .image
        .as_ref()
        .and_then(|file| file.uri.as_ref())
        .filter(|uri| matches!(uri.scheme_str(), Some("http" | "https")))
        .map(ToString::to_string);
    let mut result = json!({
        "type": "article",
        "id": suggestion.id,
        "title": suggestion.title,
        "description": suggestion.description,
        "thumbnail_url": thumbnail_url,
        "input_message_content": input_message_content,
        "reply_markup": reply_markup,
    });
    strip_nulls(&mut result);
    Ok(result)
}

fn telegram_inline_input_message(message: OutgoingMessage) -> Result<(Value, Option<Value>)> {
    let OutgoingMessage {
        mut content,
        options,
    } = message;
    anyhow::ensure!(
        content.len() == 1,
        "Telegram inline query results require exactly one content item"
    );
    let MessageOptions {
        components,
        reply,
        notification,
        visibility,
        link_preview,
        mentions,
        protect_content,
        delivery_time,
        idempotency_key,
        client_message_id,
        metadata,
        platform_data,
    } = options;
    anyhow::ensure!(
        reply.is_none()
            && notification == NotificationPolicy::Default
            && visibility == MessageVisibility::Public
            && mentions.is_none()
            && !protect_content
            && delivery_time == DeliveryTime::Immediate
            && idempotency_key.is_none()
            && client_message_id.is_none()
            && metadata.is_empty()
            && platform_data.is_none(),
        "Telegram inline result messages do not support these portable message options"
    );
    let reply_markup = components.map(telegram_editable_components).transpose()?;
    let input = match content.pop().expect("one content item") {
        MessageContent::PlainText(text) => json!({
            "message_text": text,
            "link_preview_options": link_preview.map(telegram_link_preview),
        }),
        MessageContent::RichText(text) => json!({
            "message_text": text.text,
            "entities": telegram_rich_text_entities(&text)?,
            "link_preview_options": link_preview.map(telegram_link_preview),
        }),
        MessageContent::Location(location) => json!({
            "latitude": location.latitude,
            "longitude": location.longitude,
            "horizontal_accuracy": location.horizontal_accuracy,
            "live_period": location.live_period.map(|duration| duration.as_secs()),
            "heading": location.heading,
            "proximity_alert_radius": location.proximity_alert_radius,
        }),
        MessageContent::Contact(contact) => json!({
            "phone_number": contact.phone_numbers.first()
                .context("Telegram inline contacts require a phone number")?.value,
            "first_name": contact.first_name,
            "last_name": contact.last_name,
            "vcard": contact.vcard,
        }),
        _ => {
            return Err(UnsupportedInteractionError::new(
                "this content in a portable Telegram inline query result",
            )
            .into())
        }
    };
    let mut input = input;
    strip_nulls(&mut input);
    Ok((input, reply_markup))
}

fn telegram_form_value_text(value: oxidebot::FormValue) -> Result<String> {
    Ok(match value {
        oxidebot::FormValue::Text(value) => value,
        oxidebot::FormValue::Integer(value) => value.to_string(),
        oxidebot::FormValue::Number(value) => value.to_string(),
        oxidebot::FormValue::Boolean(value) => value.to_string(),
        oxidebot::FormValue::Date(value) => value.to_string(),
        oxidebot::FormValue::Time(value) => value.to_string(),
        oxidebot::FormValue::DateTime(value) => value.to_rfc3339(),
        oxidebot::FormValue::User(value) => value.id,
        oxidebot::FormValue::Conversation(value) => value.id,
        oxidebot::FormValue::File(value) => value
            .id
            .or_else(|| value.uri.map(|uri| uri.to_string()))
            .context("inline suggestion file has no portable identifier")?,
        oxidebot::FormValue::Json(value) => serde_json::to_string(&value)?,
    })
}

fn telegram_labeled_price(item: &LineItem) -> Result<Value> {
    let quantity = i64::from(item.quantity.unwrap_or(1));
    let amount = item
        .amount
        .amount
        .checked_mul(quantity)
        .context("invoice line-item amount overflow")?;
    anyhow::ensure!(amount >= 0, "Telegram invoice prices cannot be negative");
    Ok(json!({"label": item.label, "amount": amount}))
}

fn validate_language_code(language_code: Option<&str>) -> Result<()> {
    if let Some(language_code) = language_code {
        anyhow::ensure!(
            language_code.len() == 2 && language_code.bytes().all(|byte| byte.is_ascii_lowercase()),
            "Telegram command language code must contain two lowercase ASCII letters"
        );
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct TelegramBotCommand {
    command: String,
    description: String,
    #[serde(default)]
    is_ephemeral: bool,
}

fn optional_private_chat_id(chat_id: Option<String>) -> Result<Option<i64>> {
    chat_id
        .map(|chat_id| {
            chat_id
                .parse::<i64>()
                .with_context(|| format!("invalid Telegram private chat id {chat_id:?}"))
        })
        .transpose()
}

fn telegram_chat_menu(menu: ChatMenu) -> Result<Value> {
    Ok(match menu {
        ChatMenu::Default => json!({"type": "default"}),
        ChatMenu::Commands => json!({"type": "commands"}),
        ChatMenu::WebApp { text, url } => {
            anyhow::ensure!(!text.is_empty(), "menu button text cannot be empty");
            validate_https_url(&url, "menu Web App URL")?;
            json!({"type": "web_app", "text": text, "web_app": {"url": url}})
        }
        ChatMenu::PlatformNative(native) => telegram_native_object(native, "chat menu")?,
        ChatMenu::Actions { .. } => {
            return Err(
                UnsupportedInteractionError::new("Telegram action-based persistent menus").into(),
            )
        }
    })
}

fn telegram_chat_menu_response(menu: Value) -> Result<ChatMenu> {
    let kind = menu
        .get("type")
        .and_then(Value::as_str)
        .context("Telegram menu button response has no type")?;
    match kind {
        "default" => Ok(ChatMenu::Default),
        "commands" => Ok(ChatMenu::Commands),
        "web_app" => Ok(ChatMenu::WebApp {
            text: menu
                .get("text")
                .and_then(Value::as_str)
                .context("Telegram Web App menu has no text")?
                .to_owned(),
            url: menu
                .pointer("/web_app/url")
                .and_then(Value::as_str)
                .context("Telegram Web App menu has no URL")?
                .to_owned(),
        }),
        _ => Ok(ChatMenu::PlatformNative(PlatformNativeData::new(
            crate::SERVER,
            menu,
        ))),
    }
}

fn telegram_message_ref(message: &MessageRef) -> Result<(String, i64)> {
    if let Ok((chat_id, message_id)) = split_id(&message.id) {
        return Ok((chat_id, message_id));
    }
    let conversation = message
        .conversation
        .as_ref()
        .context("Telegram message reference has no conversation")?;
    let chat = root_conversation(conversation);
    let message_id = message
        .id
        .parse::<i64>()
        .context("invalid Telegram message id")?;
    Ok((chat.id.clone(), message_id))
}

fn telegram_interaction_message_ref(handle: &InteractionResponseHandle) -> Result<MessageRef> {
    let native = telegram_native_object(
        handle
            .platform_data
            .clone()
            .context("Telegram interaction response has no message context")?,
        "interaction response context",
    )?;
    let fields = native
        .as_object()
        .context("Telegram interaction response context must be an object")?;
    if let Some(inline_message_id) = fields.get("inline_message_id").and_then(Value::as_str) {
        return Ok(MessageRef {
            id: inline_message_id.to_owned(),
            conversation: None,
            platform_data: Some(PlatformNativeData::new(
                crate::SERVER,
                json!({"inline_message_id": inline_message_id}),
            )),
        });
    }
    let chat_id = fields
        .get("chat_id")
        .and_then(|value| {
            value
                .as_i64()
                .map(|id| id.to_string())
                .or_else(|| value.as_str().map(str::to_owned))
        })
        .context("Telegram interaction response context has no chat_id")?;
    let message_id = fields
        .get("message_id")
        .and_then(Value::as_i64)
        .context("Telegram interaction response context has no message_id")?;
    let mut platform_data = Map::new();
    for name in [
        "business_connection_id",
        "receiver_user_id",
        "ephemeral_message_id",
    ] {
        if let Some(value) = fields.get(name) {
            platform_data.insert(name.to_owned(), value.clone());
        }
    }
    Ok(MessageRef {
        id: message_id.to_string(),
        conversation: Some(ConversationRef::new(
            chat_id,
            fields
                .get("chat_type")
                .and_then(Value::as_str)
                .map(telegram_conversation_kind)
                .unwrap_or(ConversationKind::Unknown),
        )),
        platform_data: (!platform_data.is_empty())
            .then(|| PlatformNativeData::new(crate::SERVER, Value::Object(platform_data))),
    })
}

fn telegram_conversation_kind(kind: &str) -> ConversationKind {
    match kind {
        "private" => ConversationKind::Direct,
        "group" | "supergroup" => ConversationKind::Group,
        "channel" => ConversationKind::Channel,
        kind => ConversationKind::PlatformNative(kind.to_owned()),
    }
}

fn root_conversation(mut conversation: &ConversationRef) -> &ConversationRef {
    while let Some(parent) = conversation.parent.as_deref() {
        conversation = parent;
    }
    conversation
}

fn telegram_chat_payload(
    conversation: Option<&ConversationRef>,
    fallback_chat_id: &str,
) -> Result<Map<String, Value>> {
    let mut payload = Map::new();
    let chat_id = conversation
        .map(root_conversation)
        .map(|conversation| conversation.id.as_str())
        .unwrap_or(fallback_chat_id);
    payload.insert("chat_id".to_owned(), chat_id_value(chat_id));
    if let Some(conversation) = conversation {
        merge_conversation_native_fields(&mut payload, conversation)?;
    }
    Ok(payload)
}

fn telegram_conversation_payload(conversation: &ConversationRef) -> Result<Map<String, Value>> {
    let root = root_conversation(conversation);
    let mut payload = Map::new();
    payload.insert("chat_id".to_owned(), chat_id_value(&root.id));

    if matches!(
        conversation.kind,
        ConversationKind::Thread | ConversationKind::Topic
    ) {
        let topic_id = conversation
            .id
            .parse::<i64>()
            .context("invalid Telegram thread or topic id")?;
        if conversation
            .platform_data
            .as_ref()
            .and_then(|native| native.data.get("direct_messages_topic_id"))
            .is_some()
        {
            payload.insert("direct_messages_topic_id".to_owned(), topic_id.into());
        } else {
            payload.insert("message_thread_id".to_owned(), topic_id.into());
        }
    }
    merge_conversation_native_fields(&mut payload, root)?;
    if !std::ptr::eq(root, conversation) {
        merge_conversation_native_fields(&mut payload, conversation)?;
    }
    Ok(payload)
}

fn merge_conversation_native_fields(
    payload: &mut Map<String, Value>,
    conversation: &ConversationRef,
) -> Result<()> {
    let Some(native) = &conversation.platform_data else {
        return Ok(());
    };
    let Value::Object(fields) = telegram_native_object(native.clone(), "conversation data")? else {
        unreachable!("native object helper always returns an object")
    };
    for (name, value) in fields {
        if matches!(
            name.as_str(),
            "chat_id" | "message_thread_id" | "direct_messages_topic_id"
        ) {
            continue;
        }
        anyhow::ensure!(
            !payload.contains_key(&name),
            "Telegram conversation data duplicates field {name:?}"
        );
        payload.insert(name, value);
    }
    Ok(())
}

fn telegram_reaction(reaction: Reaction) -> Result<Value> {
    Ok(match reaction {
        Reaction::UnicodeEmoji(emoji) => json!({"type": "emoji", "emoji": emoji}),
        Reaction::CustomEmoji { id, .. } => {
            json!({"type": "custom_emoji", "custom_emoji_id": id})
        }
        Reaction::Paid => return Err(anyhow::anyhow!("Telegram bots cannot set paid reactions")),
        Reaction::PlatformNative { platform, data } => telegram_native_object(
            PlatformNativeData::new(platform, serde_json::from_str::<Value>(&data)?),
            "reaction",
        )?,
    })
}

const TELEGRAM_CHAT_PERMISSION_FIELDS: &[(&str, ConversationPermission)] = &[
    ("can_send_messages", ConversationPermission::SendMessages),
    ("can_send_audios", ConversationPermission::SendMedia),
    ("can_send_documents", ConversationPermission::SendMedia),
    ("can_send_photos", ConversationPermission::SendMedia),
    ("can_send_videos", ConversationPermission::SendMedia),
    ("can_send_video_notes", ConversationPermission::SendMedia),
    ("can_send_voice_notes", ConversationPermission::SendMedia),
    ("can_send_polls", ConversationPermission::SendPolls),
    ("can_send_other_messages", ConversationPermission::SendMedia),
    (
        "can_add_web_page_previews",
        ConversationPermission::SendMedia,
    ),
    (
        "can_react_to_messages",
        ConversationPermission::SendReactions,
    ),
    ("can_edit_tag", ConversationPermission::EditOwnTag),
    ("can_change_info", ConversationPermission::ManageProfile),
    ("can_invite_users", ConversationPermission::AddMembers),
    ("can_pin_messages", ConversationPermission::PinMessages),
    ("can_manage_topics", ConversationPermission::ManageThreads),
];

fn telegram_permission_set(fields: &Map<String, Value>) -> PermissionSet {
    let mut permissions = PermissionSet::default();
    for (name, permission) in TELEGRAM_CHAT_PERMISSION_FIELDS.iter().cloned().chain([
        (
            "can_delete_messages",
            ConversationPermission::ManageMessages,
        ),
        ("can_restrict_members", ConversationPermission::BanMembers),
        ("can_promote_members", ConversationPermission::ManageRoles),
        (
            "can_manage_video_chats",
            ConversationPermission::ManageCalls,
        ),
        ("can_manage_tags", ConversationPermission::ManageTags),
    ]) {
        match fields.get(name).and_then(Value::as_bool) {
            Some(true) => {
                permissions.allow.insert(permission.clone());
            }
            Some(false) => {
                permissions.deny.insert(permission.clone());
            }
            None => {}
        }
    }
    permissions
}

fn telegram_chat_permissions(permissions: &PermissionSet) -> Map<String, Value> {
    TELEGRAM_CHAT_PERMISSION_FIELDS
        .iter()
        .map(|(name, permission)| {
            (
                (*name).to_owned(),
                permissions.allow.contains(permission).into(),
            )
        })
        .collect()
}

fn telegram_has_admin_permissions(permissions: &PermissionSet) -> bool {
    permissions.allow.iter().any(|permission| {
        matches!(
            permission,
            ConversationPermission::RemoveMembers
                | ConversationPermission::BanMembers
                | ConversationPermission::ManageMessages
                | ConversationPermission::ManageThreads
                | ConversationPermission::ManageProfile
                | ConversationPermission::ManageInvites
                | ConversationPermission::ManageRoles
                | ConversationPermission::ManageTags
                | ConversationPermission::ManageCalls
        )
    })
}

fn telegram_administrator_permissions(permissions: &PermissionSet) -> Vec<(String, bool)> {
    let allowed = |permission| permissions.allow.contains(&permission);
    vec![
        (
            "can_manage_chat".to_owned(),
            telegram_has_admin_permissions(permissions),
        ),
        (
            "can_change_info".to_owned(),
            allowed(ConversationPermission::ManageProfile),
        ),
        (
            "can_delete_messages".to_owned(),
            allowed(ConversationPermission::ManageMessages),
        ),
        (
            "can_invite_users".to_owned(),
            allowed(ConversationPermission::AddMembers)
                || allowed(ConversationPermission::ManageInvites),
        ),
        (
            "can_restrict_members".to_owned(),
            allowed(ConversationPermission::RemoveMembers)
                || allowed(ConversationPermission::BanMembers),
        ),
        (
            "can_pin_messages".to_owned(),
            allowed(ConversationPermission::PinMessages),
        ),
        (
            "can_manage_topics".to_owned(),
            allowed(ConversationPermission::ManageThreads),
        ),
        (
            "can_promote_members".to_owned(),
            allowed(ConversationPermission::ManageRoles),
        ),
        (
            "can_manage_tags".to_owned(),
            allowed(ConversationPermission::ManageTags),
        ),
        (
            "can_manage_video_chats".to_owned(),
            allowed(ConversationPermission::ManageCalls),
        ),
    ]
}

fn telegram_conversation_member(value: Value) -> Result<ConversationMember> {
    let object = value
        .as_object()
        .context("Telegram returned an invalid ChatMember")?;
    let telegram_user: crate::telegram::User = serde_json::from_value(
        object
            .get("user")
            .cloned()
            .context("Telegram chat member has no user")?,
    )?;
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let permissions = telegram_permission_set(object);
    let roles = if status == "member" || status == "restricted" || status == "left" {
        Vec::new()
    } else {
        vec![RoleRef {
            id: status.clone(),
            name: object
                .get("custom_title")
                .and_then(Value::as_str)
                .map(str::to_owned),
            permissions: permissions.clone(),
        }]
    };
    Ok(ConversationMember {
        user: parse_user(telegram_user),
        roles,
        permissions,
        joined_at: None,
        nickname: object
            .get("custom_title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tag: object.get("tag").and_then(Value::as_str).map(str::to_owned),
        platform_data: Some(PlatformNativeData::new(crate::SERVER, value)),
    })
}

#[derive(serde::Deserialize)]
struct TelegramForumTopic {
    message_thread_id: i64,
    name: String,
    #[serde(default)]
    icon_custom_emoji_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct TelegramChatInviteLink {
    invite_link: String,
    creator: crate::telegram::User,
    creates_join_request: bool,
    is_revoked: bool,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    expire_date: Option<i64>,
    #[serde(default)]
    member_limit: Option<u32>,
    #[serde(default)]
    pending_join_request_count: Option<u64>,
    #[serde(default)]
    subscription_price: Option<i64>,
}

fn validate_invite_options(options: &InviteLinkOptions) -> Result<()> {
    if options.require_approval {
        anyhow::ensure!(
            options.max_uses.is_none(),
            "Telegram join-request invite links cannot have a member limit"
        );
    }
    anyhow::ensure!(
        !options.temporary_membership,
        "Telegram invite links do not expose temporary membership"
    );
    if let Some(price) = &options.subscription_price {
        anyhow::ensure!(
            price.currency == "XTR" && (1..=10_000).contains(&price.amount),
            "Telegram subscription invite prices must contain 1 to 10000 XTR"
        );
        anyhow::ensure!(
            !options.require_approval && options.max_uses.is_none(),
            "Telegram subscription invite links cannot require approval or have a member limit"
        );
    }
    Ok(())
}

fn add_invite_options(payload: &mut Map<String, Value>, options: &InviteLinkOptions) {
    payload.insert("name".to_owned(), options.name.clone().into());
    payload.insert(
        "expire_date".to_owned(),
        options.expires_at.map(|date| date.timestamp()).into(),
    );
    payload.insert("member_limit".to_owned(), options.max_uses.into());
    payload.insert(
        "creates_join_request".to_owned(),
        options.require_approval.into(),
    );
}

fn add_subscription_invite_options(
    payload: &mut Map<String, Value>,
    options: &InviteLinkOptions,
    editing: bool,
) -> Result<&'static str> {
    if let Some(price) = &options.subscription_price {
        payload.remove("member_limit");
        payload.remove("creates_join_request");
        payload.insert("subscription_period".to_owned(), 2_592_000.into());
        payload.insert("subscription_price".to_owned(), price.amount.into());
        Ok(if editing {
            "editChatSubscriptionInviteLink"
        } else {
            "createChatSubscriptionInviteLink"
        })
    } else {
        Ok(if editing {
            "editChatInviteLink"
        } else {
            "createChatInviteLink"
        })
    }
}

fn telegram_invite_link(
    invite: TelegramChatInviteLink,
    conversation: ConversationRef,
    mut options: InviteLinkOptions,
) -> InviteLink {
    options.name = invite.name;
    options.expires_at = invite
        .expire_date
        .and_then(|date| chrono::DateTime::from_timestamp(date, 0));
    options.max_uses = invite.member_limit;
    options.require_approval = invite.creates_join_request;
    if let Some(price) = invite.subscription_price {
        options.subscription_price = Some(oxidebot::commerce::Money::new("XTR", price));
    }
    InviteLink {
        id: invite.invite_link.clone(),
        url: invite.invite_link,
        conversation,
        creator: Some(parse_user(invite.creator)),
        options,
        use_count: invite.pending_join_request_count,
        revoked: invite.is_revoked,
        platform_data: None,
    }
}

#[derive(serde::Deserialize)]
struct TelegramMessageId {
    message_id: i64,
}

async fn telegram_forward_or_copy(
    bot: &TelegramBot,
    method: &str,
    messages: Vec<MessageRef>,
    target: oxidebot::conversation::MessageTarget,
    options: ForwardOptions,
) -> Result<Vec<MessageRef>> {
    anyhow::ensure!(
        (1..=100).contains(&messages.len()),
        "Telegram forward/copy batches must contain 1 to 100 messages"
    );
    anyhow::ensure!(
        target.recipients.is_empty() && target.platform_data.is_none(),
        "Telegram forward/copy targets cannot contain fan-out recipients or target-native data"
    );
    anyhow::ensure!(
        method == "copyMessages" || options.preserve_caption,
        "Telegram forwarding cannot remove captions; use copy_messages instead"
    );
    let mut source_chat = None;
    let mut message_ids = Vec::with_capacity(messages.len());
    for message in &messages {
        anyhow::ensure!(
            message.platform_data.as_ref().is_none_or(|native| {
                native.data.get("receiver_user_id").is_none()
                    && native.data.get("ephemeral_message_id").is_none()
                    && native.data.get("inline_message_id").is_none()
            }),
            "Telegram cannot forward or copy ephemeral or inline messages"
        );
        let (chat_id, message_id) = telegram_message_ref(message)?;
        if let Some(source_chat) = &source_chat {
            anyhow::ensure!(
                source_chat == &chat_id,
                "Telegram batch forward/copy messages must come from one chat"
            );
        } else {
            source_chat = Some(chat_id);
        }
        message_ids.push(message_id);
    }
    anyhow::ensure!(
        message_ids.windows(2).all(|ids| ids[0] < ids[1]),
        "Telegram batch message identifiers must be strictly increasing"
    );
    let mut payload = telegram_conversation_payload(&target.conversation)?;
    payload.insert(
        "from_chat_id".to_owned(),
        chat_id_value(source_chat.as_deref().expect("nonempty batch")),
    );
    payload.insert("message_ids".to_owned(), json!(message_ids));
    payload.insert(
        "disable_notification".to_owned(),
        matches!(
            options.notification,
            oxidebot::content::NotificationPolicy::Silent
        )
        .into(),
    );
    if method == "copyMessages" {
        payload.insert(
            "remove_caption".to_owned(),
            (!options.preserve_caption).into(),
        );
    }
    if let Some(native) = options.platform_data {
        merge_native_fields(&mut payload, native, "forward/copy options")?;
    }
    let response: Vec<TelegramMessageId> = bot.call(method, Value::Object(payload)).await?;
    Ok(response
        .into_iter()
        .map(|message| {
            MessageRef::new(message.message_id.to_string())
                .in_conversation(target.conversation.clone())
        })
        .collect())
}

async fn telegram_answer_join_request(
    bot: &TelegramBot,
    method: &str,
    conversation: ConversationRef,
    user_id: String,
) -> Result<()> {
    let mut payload = telegram_conversation_payload(&conversation)?;
    payload.insert(
        "user_id".to_owned(),
        user_id
            .parse::<i64>()
            .context("invalid Telegram user id")?
            .into(),
    );
    let _: bool = bot.call(method, Value::Object(payload)).await?;
    Ok(())
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
            let response = crate::tls::webpki_client_builder()
                .context("failed to load WebPKI root certificates")?
                .build()
                .context("failed to create avatar download client")?
                .get(uri.to_string())
                .send()
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

async fn avatar_file_upload(file: &File) -> Result<Upload> {
    if let Some(base64) = &file.base64 {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .context("invalid base64 avatar data")?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_AVATAR_BYTES,
            "avatar is larger than 10 MiB"
        );
        return Ok(Upload {
            name: nonempty_name(file.name.clone()),
            mime: file.mime.as_ref().map(ToString::to_string),
            bytes,
        });
    }
    if let Some(uri) = &file.uri {
        return avatar_upload(uri).await;
    }
    Err(anyhow::anyhow!(
        "Telegram profile photos must contain base64 data or a readable URI"
    ))
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

    #[test]
    fn inline_keyboard_maps_common_actions_and_bot_api_10_2_style() -> Result<()> {
        let markup = telegram_message_components(MessageComponents::InlineKeyboard(
            oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([
                    Button::callback("Confirm", "confirm")
                        .style(ButtonStyle::Success)
                        .icon(ButtonIcon::Custom("emoji-id".to_owned())),
                    Button::url("Docs", "https://example.com"),
                    Button::web_app("Open", "https://example.com/app"),
                ]),
            ]),
        ))?;

        assert_eq!(markup["inline_keyboard"][0][0]["callback_data"], "confirm");
        assert_eq!(markup["inline_keyboard"][0][0]["style"], "success");
        assert_eq!(
            markup["inline_keyboard"][0][0]["icon_custom_emoji_id"],
            "emoji-id"
        );
        assert_eq!(
            markup["inline_keyboard"][0][1]["url"],
            "https://example.com"
        );
        assert_eq!(
            markup["inline_keyboard"][0][2]["web_app"]["url"],
            "https://example.com/app"
        );
        Ok(())
    }

    #[test]
    fn inline_keyboard_maps_telegram_advanced_actions() -> Result<()> {
        let markup = telegram_message_components(MessageComponents::InlineKeyboard(
            oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([
                    Button::login(
                        "Sign in",
                        oxidebot::interaction::LoginAction::new("https://example.com/login"),
                    ),
                    Button::switch_inline_query(
                        "Choose",
                        "query",
                        InlineQueryTarget::ChosenChat(oxidebot::interaction::ChosenChatCriteria {
                            allow_user_chats: Some(true),
                            ..Default::default()
                        }),
                    ),
                    Button::copy_text("Copy", "copied"),
                ]),
            ]),
        ))?;

        assert_eq!(
            markup["inline_keyboard"][0][0]["login_url"]["url"],
            "https://example.com/login"
        );
        assert_eq!(
            markup["inline_keyboard"][0][1]["switch_inline_query_chosen_chat"]["allow_user_chats"],
            true
        );
        assert_eq!(
            markup["inline_keyboard"][0][2]["copy_text"]["text"],
            "copied"
        );
        Ok(())
    }

    #[test]
    fn reply_keyboard_maps_request_and_web_app_actions() -> Result<()> {
        let markup =
            telegram_message_components(MessageComponents::ReplyKeyboard(ReplyKeyboard::new([
                oxidebot::interaction::ReplyButtonRow::new([
                    ReplyButton::new(
                        "Users",
                        ReplyButtonAction::RequestUsers(oxidebot::interaction::RequestUsers {
                            request_id: 7,
                            user_is_bot: Some(false),
                            user_is_premium: None,
                            max_quantity: Some(3),
                            request_name: true,
                            request_username: true,
                            request_photo: false,
                        }),
                    ),
                    ReplyButton::new("Location", ReplyButtonAction::RequestLocation),
                    ReplyButton::new(
                        "Poll",
                        ReplyButtonAction::RequestPoll {
                            kind: Some(PollKind::Quiz),
                        },
                    ),
                ]),
            ])))?;

        assert_eq!(markup["keyboard"][0][0]["request_users"]["request_id"], 7);
        assert_eq!(markup["keyboard"][0][0]["request_users"]["max_quantity"], 3);
        assert_eq!(markup["keyboard"][0][1]["request_location"], true);
        assert_eq!(markup["keyboard"][0][2]["request_poll"]["type"], "quiz");
        Ok(())
    }

    #[test]
    fn reply_keyboard_maps_chat_and_managed_bot_requests() -> Result<()> {
        let mut request_chat = oxidebot::interaction::RequestChat::new(9, false);
        request_chat.request_title = true;
        request_chat.platform_data = Some(PlatformNativeData::new(
            crate::SERVER,
            json!({
                "user_administrator_rights": {"can_manage_chat": true},
                "bot_administrator_rights": {"can_manage_chat": true}
            }),
        ));
        let markup = telegram_message_components(MessageComponents::ReplyKeyboard(
            ReplyKeyboard::new([oxidebot::interaction::ReplyButtonRow::new([
                ReplyButton::request_chat("Chat", request_chat),
                ReplyButton::request_managed_bot(
                    "Bot",
                    oxidebot::interaction::RequestManagedBot {
                        request_id: 10,
                        suggested_name: Some("Helper".to_owned()),
                        suggested_username: Some("helper_bot".to_owned()),
                    },
                ),
            ])])
            .persistent(true)
            .placeholder("Choose one"),
        ))?;

        assert_eq!(markup["keyboard"][0][0]["request_chat"]["request_id"], 9);
        assert_eq!(
            markup["keyboard"][0][0]["request_chat"]["user_administrator_rights"]
                ["can_manage_chat"],
            true
        );
        assert_eq!(
            markup["keyboard"][0][1]["request_managed_bot"]["request_id"],
            10
        );
        assert_eq!(markup["is_persistent"], true);
        Ok(())
    }

    #[test]
    fn telegram_rejects_unrepresentable_component_states() {
        let disabled =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([Button::callback(
                    "Disabled", "disabled",
                )
                .disabled(true)]),
            ]));
        assert!(telegram_message_components(disabled).is_err());

        let oversized =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([Button::callback(
                    "Too large",
                    "x".repeat(65),
                )]),
            ]));
        assert!(telegram_message_components(oversized).is_err());

        let send_text =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([Button::send_text("Send", "/start")]),
            ]));
        assert!(telegram_message_components(send_text).is_err());

        let oversized_copy =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([Button::copy_text(
                    "Copy",
                    "x".repeat(257),
                )]),
            ]));
        assert!(telegram_message_components(oversized_copy).is_err());

        let insecure_web_app =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([Button::web_app(
                    "Open",
                    "http://example.com/app",
                )]),
            ]));
        assert!(telegram_message_components(insecure_web_app).is_err());

        let misplaced_game =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([
                    Button::callback("First", "first"),
                    Button::game("Play"),
                ]),
            ]));
        assert!(telegram_message_components(misplaced_game).is_err());

        let wrong_platform = MessageComponents::PlatformNative(PlatformNativeData::new(
            "discord",
            json!({"inline_keyboard": []}),
        ));
        assert!(telegram_message_components(wrong_platform).is_err());

        let invalid_placeholder = MessageComponents::ReplyKeyboard(
            ReplyKeyboard::new([oxidebot::interaction::ReplyButtonRow::new([
                ReplyButton::text("Choice"),
            ])])
            .placeholder(""),
        );
        assert!(telegram_message_components(invalid_placeholder).is_err());

        let empty_icon =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([
                    Button::callback("OK", "ok").icon(ButtonIcon::Custom(String::new()))
                ]),
            ]));
        assert!(telegram_message_components(empty_icon).is_err());
    }

    #[test]
    fn telegram_validates_commands_and_ephemeral_flag() -> Result<()> {
        validate_commands(&[BotCommand::new("start_2", "Start")])?;
        assert!(validate_commands(&[BotCommand::new("Start", "Start")]).is_err());
        assert!(validate_commands(&[BotCommand::new("x".repeat(33), "Start")]).is_err());
        validate_language_code(Some("zh"))?;
        assert!(validate_language_code(Some("zh-CN")).is_err());

        let command = BotCommand::new("private", "Private response").ephemeral(true);
        assert!(command.is_ephemeral);
        Ok(())
    }

    #[tokio::test]
    async fn components_require_sendable_message_content() -> Result<()> {
        let (api_base, server) = mock_telegram_server(vec![
            r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Bot"}}"#,
        ])
        .await?;
        let bot = TelegramBot::try_with_api_base("123:test", api_base, Default::default()).await?;
        let error = bot
            .send_message_with_options(
                Vec::new(),
                SendMessageTarget::Private("42".to_owned()),
                MessageOptions::default().components(MessageComponents::InlineKeyboard(
                    oxidebot::interaction::InlineKeyboard::new([
                        oxidebot::interaction::ActionRow::buttons([Button::callback("OK", "ok")]),
                    ]),
                )),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Telegram-sendable segments"));
        assert_eq!(server.await?.len(), 1);
        Ok(())
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

    #[tokio::test]
    async fn typed_interactions_commands_and_menu_use_telegram_api() -> Result<()> {
        let (api_base, server) = mock_telegram_server(vec![
            r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Bot"}}"#,
            r#"{"ok":true,"result":{"message_id":10,"date":1,"chat":{"id":42,"type":"private"},"text":"Choose"}}"#,
            r#"{"ok":true,"result":true}"#,
            r#"{"ok":true,"result":true}"#,
            r#"{"ok":true,"result":[{"command":"start","description":"Start","is_ephemeral":true}]}"#,
            r#"{"ok":true,"result":true}"#,
            r#"{"ok":true,"result":true}"#,
            r#"{"ok":true,"result":{"type":"web_app","text":"Open","web_app":{"url":"https://example.com/app"}}}"#,
            r#"{"ok":true,"result":true}"#,
            r#"{"ok":true,"result":true}"#,
        ])
        .await?;
        let bot = TelegramBot::try_with_api_base("123:test", api_base, Default::default()).await?;
        let keyboard =
            MessageComponents::InlineKeyboard(oxidebot::interaction::InlineKeyboard::new([
                oxidebot::interaction::ActionRow::buttons([Button::callback("Confirm", "confirm")]),
            ]));

        let sent = bot
            .send_message_with_options(
                vec![MessageSegment::text("Choose")],
                SendMessageTarget::Private("42".to_owned()),
                MessageOptions::default().components(keyboard.clone()),
            )
            .await?;
        assert_eq!(sent[0].sent_message_id, "42_10");
        bot.answer_interaction(
            "callback-1".to_owned(),
            InteractionResponse::Notification {
                text: "Done".to_owned(),
                style: InteractionNotificationStyle::Toast,
                cache_time: None,
            },
        )
        .await?;
        bot.set_bot_commands(BotCommandSet::new([
            BotCommand::new("start", "Start").ephemeral(true)
        ]))
        .await?;
        assert_eq!(
            bot.get_bot_commands(BotCommandQuery::default()).await?,
            [BotCommand::new("start", "Start").ephemeral(true)]
        );
        bot.delete_bot_commands(BotCommandQuery::default()).await?;
        bot.set_chat_menu(
            Some("42".to_owned()),
            ChatMenu::WebApp {
                text: "Open".to_owned(),
                url: "https://example.com/app".to_owned(),
            },
        )
        .await?;
        assert_eq!(
            bot.get_chat_menu(Some("42".to_owned())).await?,
            ChatMenu::WebApp {
                text: "Open".to_owned(),
                url: "https://example.com/app".to_owned(),
            }
        );
        bot.edit_message_components("42_10".to_owned(), Some(keyboard))
            .await?;
        bot.edit_message_components("42_10".to_owned(), None)
            .await?;

        let requests = server.await?;
        let send_request = String::from_utf8_lossy(&requests[1]);
        assert!(send_request.contains(r#""reply_markup":{"inline_keyboard"#));
        assert!(String::from_utf8_lossy(&requests[2])
            .starts_with("POST /bot123:test/answerCallbackQuery "));
        assert!(
            String::from_utf8_lossy(&requests[3]).starts_with("POST /bot123:test/setMyCommands ")
        );
        assert!(String::from_utf8_lossy(&requests[8])
            .starts_with("POST /bot123:test/editMessageReplyMarkup "));
        let remove_request = String::from_utf8_lossy(&requests[9]);
        assert!(remove_request.starts_with("POST /bot123:test/editMessageReplyMarkup "));
        assert!(remove_request.contains(r#""reply_markup":{"inline_keyboard":[]}"#));
        Ok(())
    }

    #[tokio::test]
    async fn v2_api_maps_poll_checklist_tags_bulk_delete_and_invoice() -> Result<()> {
        let message_response =
            r#"{"ok":true,"result":{"message_id":10,"date":1,"chat":{"id":42,"type":"private"}}}"#;
        let (api_base, server) = mock_telegram_server(vec![
            r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Bot"}}"#,
            message_response,
            message_response,
            message_response,
            r#"{"ok":true,"result":{"id":"poll-1","question":"Question?","options":[{"text":"Yes","voter_count":1},{"text":"No","voter_count":0}],"total_voter_count":1,"is_closed":true,"is_anonymous":true,"type":"regular","allows_multiple_answers":false}}"#,
            r#"{"ok":true,"result":true}"#,
            r#"{"ok":true,"result":true}"#,
            message_response,
        ])
        .await?;
        let bot = TelegramBot::try_with_api_base("123:test", api_base, Default::default()).await?;
        let conversation = ConversationRef::direct("42");
        let poll = Poll {
            id: None,
            question: RichText::plain("Question?"),
            options: vec![
                PollOption {
                    id: None,
                    text: RichText::plain("Yes"),
                    voter_count: None,
                    platform_data: None,
                },
                PollOption {
                    id: None,
                    text: RichText::plain("No"),
                    voter_count: None,
                    platform_data: None,
                },
            ],
            allows_multiple_answers: false,
            allows_revoting: None,
            members_only: None,
            country_codes: Vec::new(),
            anonymous: Some(true),
            kind: PollType::Regular,
            open_for: None,
            closes_at: None,
            total_voter_count: None,
            correct_option: None,
            correct_options: Vec::new(),
            explanation: None,
            closed: false,
            platform_data: None,
        };
        let checklist = Checklist {
            id: None,
            title: RichText::plain("Release"),
            tasks: vec![oxidebot::ChecklistTask {
                id: Some("1".to_owned()),
                text: RichText::plain("Test"),
                completed: false,
                completed_by: None,
                completed_at: None,
                platform_data: None,
            }],
            can_add_tasks: true,
            can_mark_tasks_done: true,
            platform_data: None,
        };
        let sent = bot
            .send_outgoing_message(
                MessageTarget::new(conversation.clone()),
                OutgoingMessage::new([
                    MessageContent::PlainText("hello".to_owned()),
                    MessageContent::Poll(poll),
                    MessageContent::Checklist(checklist),
                ]),
            )
            .await?;
        assert_eq!(sent.len(), 3);
        assert!(
            bot.stop_poll(MessageRef::new("10").in_conversation(conversation.clone()))
                .await?
                .closed
        );
        bot.set_member_tag(
            conversation.clone(),
            "7".to_owned(),
            Some("helper".to_owned()),
        )
        .await?;
        bot.delete_messages(vec![
            MessageRef::new("10").in_conversation(conversation.clone()),
            MessageRef::new("11").in_conversation(conversation.clone()),
        ])
        .await?;
        bot.send_invoice(
            MessageTarget::new(conversation),
            Invoice {
                id: None,
                title: "Support".to_owned(),
                description: "One month".to_owned(),
                payload: "support-monthly".to_owned(),
                items: vec![LineItem {
                    label: "Month".to_owned(),
                    amount: oxidebot::Money::new("XTR", 50),
                    quantity: Some(1),
                }],
                customer_details: Default::default(),
                flexible_shipping: false,
                platform_data: None,
            },
            InvoiceOptions::default(),
        )
        .await?;

        let requests = server.await?;
        let paths = requests
            .iter()
            .map(|request| {
                String::from_utf8_lossy(request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert!(paths[1].contains("/sendMessage "));
        assert!(paths[2].contains("/sendPoll "));
        assert!(paths[3].contains("/sendChecklist "));
        assert!(paths[4].contains("/stopPoll "));
        assert!(paths[5].contains("/setChatMemberTag "));
        assert!(paths[6].contains("/deleteMessages "));
        assert!(paths[7].contains("/sendInvoice "));
        assert!(String::from_utf8_lossy(&requests[6]).contains(r#""message_ids":[10,11]"#));
        assert!(String::from_utf8_lossy(&requests[7]).contains(r#""currency":"XTR""#));
        Ok(())
    }

    #[test]
    fn rich_layout_supports_nested_spans_and_valid_table_cells() -> Result<()> {
        let text = RichText::plain("abcdef")
            .span(0..6, TextStyle::Bold)
            .span(2..4, TextStyle::Italic);
        let value = telegram_rich_text_value(&text)?;
        assert_eq!(value.as_array().unwrap().len(), 3);
        assert_eq!(value[1]["type"], "bold");
        assert_eq!(value[1]["text"]["type"], "italic");

        let mut actions = Vec::new();
        let blocks = telegram_layout_nodes(
            vec![LayoutNode::Table {
                rows: vec![vec![oxidebot::TableCell {
                    content: vec![LayoutNode::Text(RichText::plain("cell"))],
                    header: true,
                    colspan: None,
                    rowspan: None,
                }]],
            }],
            &mut actions,
        )?;
        assert_eq!(blocks[0]["cells"][0][0]["align"], "left");
        assert_eq!(blocks[0]["cells"][0][0]["valign"], "top");
        Ok(())
    }

    #[tokio::test]
    async fn rich_layout_uploads_local_media_with_attach_references() -> Result<()> {
        let (api_base, server) = mock_telegram_server(vec![
            r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Bot"}}"#,
            r#"{"ok":true,"result":{"message_id":10,"date":1,"chat":{"id":42,"type":"private"}}}"#,
        ])
        .await?;
        let bot = TelegramBot::try_with_api_base("123:test", api_base, Default::default()).await?;
        let file = File {
            name: "pixel.jpg".to_owned(),
            base64: Some(base64::engine::general_purpose::STANDARD.encode(b"image-bytes")),
            ..Default::default()
        };
        bot.send_outgoing_message(
            MessageTarget::new(ConversationRef::direct("42")),
            OutgoingMessage::new([MessageContent::RichLayout(RichLayout {
                nodes: vec![LayoutNode::Image(Media::new(file))],
                fallback_text: None,
                platform_data: None,
            })]),
        )
        .await?;

        let requests = server.await?;
        let request = String::from_utf8_lossy(&requests[1]);
        assert!(request.starts_with("POST /bot123:test/sendRichMessage "));
        assert!(request.contains("attach://rich_media_0"));
        assert!(request.contains("name=\"rich_media_0\""));
        assert!(request.contains("image-bytes"));
        Ok(())
    }
}
