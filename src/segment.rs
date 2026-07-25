use anyhow::{Context as _, Result};
use oxidebot::source::message::{File, Message, MessageSegment};
use serde::Serialize;
use serde_json::Value;

use crate::{
    telegram::{
        Location, Message as TelegramMessage, MessageEntity, PhotoSize, ReactionType, User, Venue,
    },
    utils::{message_id, split_id},
};

pub fn parse_message(mut message: TelegramMessage) -> Message {
    let mut segments = Vec::new();

    if let Some(text) = message.text.take() {
        parse_text(&text, message.entities.take(), &mut segments);
    }

    if let Some(photo) = message.photo.take().and_then(|photos| {
        photos
            .into_iter()
            .max_by_key(|photo| photo.width * photo.height)
    }) {
        segments.push(parse_photo(photo));
    }
    if let Some(animation) = message.animation.take() {
        segments.push(MessageSegment::video(
            media_file(
                animation.file_id,
                animation.file_name,
                animation.mime_type,
                animation.file_size,
            ),
            duration(animation.duration),
        ));
    }
    if let Some(audio) = message.audio.take() {
        segments.push(MessageSegment::audio(
            media_file(
                audio.file_id,
                audio.file_name,
                audio.mime_type,
                audio.file_size,
            ),
            duration(audio.duration),
        ));
    }
    if let Some(video) = message.video.take() {
        segments.push(MessageSegment::video(
            media_file(
                video.file_id,
                video.file_name,
                video.mime_type,
                video.file_size,
            ),
            duration(video.duration),
        ));
    }
    if let Some(video) = message.video_note.take() {
        segments.push(MessageSegment::video(
            media_file(
                video.file_id,
                video.file_name,
                video.mime_type,
                video.file_size,
            ),
            duration(video.duration),
        ));
    }
    if let Some(document) = message.document.take() {
        segments.push(MessageSegment::file(media_file(
            document.file_id,
            document.file_name,
            document.mime_type,
            document.file_size,
        )));
    }
    if let Some(sticker) = message.sticker.take() {
        // `MessageSegment::Emoji` is the only oxidebot segment that can retain
        // a Telegram sticker file_id and send it again without losing quality.
        segments.push(MessageSegment::emoji(sticker.file_id));
    }
    if let Some(voice) = message.voice.take() {
        segments.push(MessageSegment::audio(
            media_file(
                voice.file_id,
                voice.file_name,
                voice.mime_type,
                voice.file_size,
            ),
            duration(voice.duration),
        ));
    }

    if let Some(caption) = message.caption.take() {
        parse_text(&caption, message.caption_entities.take(), &mut segments);
    }
    if let Some(venue) = message.venue.take() {
        segments.push(parse_venue(venue));
    } else if let Some(location) = message.location.take() {
        segments.push(parse_location(location));
    }

    push_raw_segment(&mut segments, "rich_message", message.rich_message.take());
    push_raw_segment(&mut segments, "live_photo", message.live_photo.take());
    push_raw_segment(&mut segments, "paid_media", message.paid_media.take());
    push_raw_segment(&mut segments, "story", message.story.take());
    push_raw_segment(&mut segments, "contact", message.contact.take());
    for (kind, data) in message.extra {
        if !data.is_null() {
            segments.push(MessageSegment::custom_value(
                format!("telegram.message.{kind}"),
                data,
            ));
        }
    }

    Message {
        id: message_id(message.chat.id, message.message_id),
        segments,
    }
}

fn push_raw_segment(segments: &mut Vec<MessageSegment>, kind: &str, data: Option<Value>) {
    if let Some(data) = data {
        segments.push(MessageSegment::custom_value(
            format!("telegram.message.{kind}"),
            data,
        ));
    }
}

fn parse_text(
    text: &str,
    entities: Option<Vec<MessageEntity>>,
    segments: &mut Vec<MessageSegment>,
) {
    let mut mentions = entities
        .into_iter()
        .flatten()
        .filter(|entity| matches!(entity.kind.as_str(), "mention" | "text_mention"))
        .collect::<Vec<_>>();
    mentions.sort_by_key(|entity| entity.offset);

    let mut cursor = 0;
    let mut found_valid_mention = false;
    for entity in mentions {
        let end = entity.offset.saturating_add(entity.length);
        if entity.offset < cursor || utf16_slice(text, entity.offset, entity.length).is_none() {
            continue;
        }
        let mention = match entity.kind.as_str() {
            "text_mention" => entity.user.map(|user| user.id.to_string()),
            "mention" => utf16_slice(text, entity.offset, entity.length),
            _ => None,
        };
        let Some(mention) = mention else {
            continue;
        };
        if let Some(prefix) = utf16_slice(text, cursor, entity.offset - cursor) {
            if !prefix.is_empty() {
                segments.push(MessageSegment::text(prefix));
            }
        }
        segments.push(MessageSegment::at(mention));
        cursor = end;
        found_valid_mention = true;
    }
    if !found_valid_mention {
        segments.push(MessageSegment::text(text));
    } else if let Some(suffix) = utf16_slice(text, cursor, text.encode_utf16().count() - cursor) {
        if !suffix.is_empty() {
            segments.push(MessageSegment::text(suffix));
        }
    }
}

fn utf16_slice(text: &str, offset: usize, length: usize) -> Option<String> {
    let units = text
        .encode_utf16()
        .skip(offset)
        .take(length)
        .collect::<Vec<_>>();
    (units.len() == length)
        .then(|| String::from_utf16(&units).ok())
        .flatten()
}

fn duration(value: i64) -> Option<i32> {
    i32::try_from(value).ok()
}

fn media_file(
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
) -> File {
    File {
        id: Some(file_id.clone()),
        name: file_name.unwrap_or(file_id),
        uri: None,
        base64: None,
        mime: mime_type.and_then(|mime| mime.parse().ok()),
        size: file_size,
    }
}

pub fn parse_photo(photo: PhotoSize) -> MessageSegment {
    MessageSegment::image(File {
        id: Some(photo.file_id.clone()),
        name: photo.file_id,
        uri: None,
        base64: None,
        mime: "image/jpeg".parse().ok(),
        size: photo.file_size,
    })
}

pub fn parse_venue(venue: Venue) -> MessageSegment {
    MessageSegment::location(
        venue.location.latitude,
        venue.location.longitude,
        venue.title,
        Some(venue.address),
    )
}

pub fn parse_location(location: Location) -> MessageSegment {
    MessageSegment::location(location.latitude, location.longitude, String::new(), None)
}

pub fn parse_reaction(reaction: ReactionType) -> String {
    match reaction.kind.as_str() {
        "emoji" => reaction.emoji.unwrap_or_else(|| "unknown".to_owned()),
        "custom_emoji" => reaction
            .custom_emoji_id
            .unwrap_or_else(|| "unknown".to_owned()),
        "paid" => "paid".to_owned(),
        other => format!("telegram:{other}"),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReplyParameters {
    pub message_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
    Audio,
    Document,
}

impl MediaKind {
    pub fn telegram_type(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Document => "document",
        }
    }

    pub fn field_name(self) -> &'static str {
        self.telegram_type()
    }

    pub fn group_class(self) -> MediaGroupClass {
        match self {
            Self::Photo | Self::Video => MediaGroupClass::Visual,
            Self::Audio => MediaGroupClass::Audio,
            Self::Document => MediaGroupClass::Document,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaGroupClass {
    Visual,
    Audio,
    Document,
}

#[derive(Clone, Debug)]
pub struct OutgoingMedia {
    pub kind: MediaKind,
    pub file: File,
    pub duration: Option<i32>,
    pub caption: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OutgoingVenue {
    pub latitude: f64,
    pub longitude: f64,
    pub title: String,
    pub address: String,
}

#[derive(Clone, Debug, Default)]
pub struct OutgoingMessage {
    pub text: String,
    pub entities: Vec<MessageEntity>,
    pub media: Vec<OutgoingMedia>,
    pub reply: Option<ReplyParameters>,
    pub venues: Vec<OutgoingVenue>,
    pub stickers: Vec<String>,
}

#[derive(Default)]
struct TextComposer {
    text: String,
    entities: Vec<MessageEntity>,
}

impl TextComposer {
    fn append(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn mention(&mut self, user_id: String) -> Result<()> {
        let offset = self.text.encode_utf16().count();
        if user_id.starts_with('@') {
            self.text.push_str(&user_id);
            self.entities.push(MessageEntity {
                kind: "mention".to_owned(),
                offset,
                length: user_id.encode_utf16().count(),
                ..Default::default()
            });
        } else {
            let id: i64 = user_id
                .parse()
                .with_context(|| format!("invalid Telegram mention user id {user_id:?}"))?;
            let label = format!("@{user_id}");
            self.text.push_str(&label);
            self.entities.push(MessageEntity {
                kind: "text_mention".to_owned(),
                offset,
                length: label.encode_utf16().count(),
                user: Some(User {
                    id,
                    first_name: user_id,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        Ok(())
    }
}

pub fn process_message_segments(message: Vec<MessageSegment>) -> Result<OutgoingMessage> {
    let mut output = OutgoingMessage::default();
    let mut text = TextComposer::default();
    let mut unsupported = Vec::new();

    for segment in message {
        match segment {
            MessageSegment::Text { content } => text.append(&content),
            MessageSegment::Image { file } => {
                output
                    .media
                    .push(outgoing_media(MediaKind::Photo, file, None, None)?)
            }
            MessageSegment::Video { file, length } => {
                output
                    .media
                    .push(outgoing_media(MediaKind::Video, file, length, None)?)
            }
            MessageSegment::Audio { file, length } => {
                output
                    .media
                    .push(outgoing_media(MediaKind::Audio, file, length, None)?)
            }
            MessageSegment::File { file } => {
                output
                    .media
                    .push(outgoing_media(MediaKind::Document, file, None, None)?)
            }
            MessageSegment::Reply { message_id } => {
                anyhow::ensure!(
                    output.reply.is_none(),
                    "multiple reply segments are not supported"
                );
                let (chat_id, message_id) = split_id(&message_id)?;
                output.reply = Some(ReplyParameters {
                    message_id,
                    chat_id: Some(chat_id_value(&chat_id)),
                });
            }
            MessageSegment::At { user_id } => text.mention(user_id)?,
            MessageSegment::AtAll => text.append("@all"),
            MessageSegment::Share {
                title,
                content,
                url,
                image,
            } => {
                let caption = [Some(title), content, Some(url)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                if image.is_some() {
                    output.media.push(outgoing_media(
                        MediaKind::Photo,
                        image,
                        None,
                        Some(caption),
                    )?);
                } else {
                    text.append(&caption);
                }
            }
            MessageSegment::Location {
                latitude,
                longitude,
                title,
                content,
            } => output.venues.push(OutgoingVenue {
                latitude,
                longitude,
                title,
                address: content.unwrap_or_default(),
            }),
            MessageSegment::Emoji { id } => output.stickers.push(id),
            MessageSegment::Reference { .. } => unsupported.push("reference"),
            MessageSegment::ForwardNode { .. } => unsupported.push("forward node"),
            MessageSegment::ForwardCustomNode { .. } => unsupported.push("custom forward node"),
            MessageSegment::CustomString { .. } => unsupported.push("custom string"),
            MessageSegment::CustomValue { .. } => unsupported.push("custom value"),
        }
    }

    anyhow::ensure!(
        unsupported.is_empty(),
        "Telegram cannot send oxidebot segment(s): {}",
        unsupported.join(", ")
    );
    anyhow::ensure!(
        !text.text.is_empty()
            || !output.media.is_empty()
            || !output.venues.is_empty()
            || !output.stickers.is_empty(),
        "message contains no Telegram-sendable segments"
    );

    output.text = text.text;
    output.entities = text.entities;
    Ok(output)
}

fn outgoing_media(
    kind: MediaKind,
    file: Option<File>,
    duration: Option<i32>,
    caption: Option<String>,
) -> Result<OutgoingMedia> {
    let file = file.ok_or_else(|| anyhow::anyhow!("{kind:?} segment has no file"))?;
    anyhow::ensure!(
        file.id.is_some() || file.uri.is_some() || file.base64.is_some(),
        "{kind:?} file has no Telegram file_id, URI, path, or base64 data"
    );
    Ok(OutgoingMedia {
        kind,
        file,
        duration,
        caption,
    })
}

pub fn chat_id_value(chat_id: &str) -> Value {
    chat_id
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::from(chat_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_uses_telegram_file_size_not_pixel_count() {
        let segment = parse_photo(PhotoSize {
            file_id: "photo".to_owned(),
            width: 4000,
            height: 3000,
            file_size: Some(123_456),
        });
        let MessageSegment::Image { file: Some(file) } = segment else {
            panic!("expected image")
        };
        assert_eq!(file.size, Some(123_456));
    }

    #[test]
    fn numeric_mentions_have_valid_utf16_offsets() {
        let output =
            process_message_segments(vec![MessageSegment::text("😀"), MessageSegment::at("42")])
                .unwrap();
        assert_eq!(output.text, "😀@42");
        assert_eq!(output.entities[0].offset, 2);
        assert_eq!(output.entities[0].length, 3);
    }

    #[test]
    fn inbound_mentions_do_not_duplicate_visible_text_segments() {
        let message = TelegramMessage {
            message_id: 1,
            chat: crate::telegram::Chat {
                id: 2,
                kind: "private".to_owned(),
                ..Default::default()
            },
            text: Some("hi Alice!".to_owned()),
            entities: Some(vec![MessageEntity {
                kind: "text_mention".to_owned(),
                offset: 3,
                length: 5,
                user: Some(User {
                    id: 42,
                    first_name: "Alice".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let parsed = parse_message(message.clone());
        assert_eq!(
            parsed.segments,
            vec![
                MessageSegment::text("hi "),
                MessageSegment::at("42"),
                MessageSegment::text("!")
            ]
        );
    }

    #[test]
    fn location_only_message_does_not_create_empty_text() {
        let output =
            process_message_segments(vec![MessageSegment::location(1.0, 2.0, "", None::<&str>)])
                .unwrap();
        assert!(output.text.is_empty());
        assert_eq!(output.venues.len(), 1);
    }

    #[test]
    fn share_with_image_keeps_the_url() {
        let output = process_message_segments(vec![MessageSegment::share(
            "title",
            "https://example.com",
            Some("description"),
            Some(File {
                id: Some("file-id".to_owned()),
                ..Default::default()
            }),
        )])
        .unwrap();
        assert_eq!(
            output.media[0].caption.as_deref(),
            Some("title\ndescription\nhttps://example.com")
        );
    }
}
