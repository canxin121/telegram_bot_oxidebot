use oxidebot::source::message::{File, Message, MessageSegment};
use telegram_bot_api_rs::available_types::{
    self, InputMedia, Location, MessageEntity, PhotoSize, ReactionType, ReplyParameters, User,
    Venue,
};

use crate::utils::split_id;
pub fn parse_message(message: available_types::Message) -> Message {
    let mut segments = Vec::new();
    if let Some(text) = message.text {
        segments.push(MessageSegment::text(text));
    }
    if let Some(entities) = message.entities {
        for entity in entities.into_iter() {
            if let Some(segment) = parse_message_entity(entity) {
                segments.push(segment);
            }
        }
    }
    if let Some(photo) = message.photo {
        let photo = photo.into_iter().max_by_key(|p| p.width * p.height);
        if let Some(photo) = photo {
            segments.push(parse_photo(photo));
        }
    }
    if let Some(animation) = message.animation {
        segments.push(parse_animation(animation))
    }
    if let Some(audio) = message.audio {
        segments.push(parse_audio(audio))
    }
    if let Some(video) = message.video {
        segments.push(parse_video(video))
    }
    if let Some(document) = message.document {
        segments.push(parse_document(document))
    }
    if let Some(_) = message.paid_media {
        tracing::warn!("Paid media not supported");
    }
    if let Some(sticker) = message.sticker {
        segments.push(parse_sticker(sticker))
    }
    if let Some(_) = message.story {
        tracing::warn!("Story not supported");
    }
    if message.video_note.is_some() {
        tracing::warn!("Video note not supported");
    }
    if let Some(voice) = message.voice {
        segments.push(parse_voice(voice))
    }
    if let Some(caption) = message.caption {
        segments.push(MessageSegment::text(caption));
    }
    if let Some(caption_entities) = message.caption_entities {
        for entity in caption_entities.into_iter() {
            if let Some(segment) = parse_message_entity(entity) {
                segments.push(segment);
            }
        }
    }
    if let Some(_) = message.contact {
        tracing::warn!("Contact not supported");
    }
    if let Some(venue) = message.venue {
        segments.push(parse_venue(venue))
    }
    if let Some(location) = message.location {
        segments.push(parse_location(location))
    }
    Message {
        id: format!("{}_{}", message.chat.id, message.message_id),
        segments,
    }
}
pub fn parse_photo(photo: PhotoSize) -> MessageSegment {
    MessageSegment::Image {
        file: Some(File {
            id: Some(photo.file_id.clone()),
            name: photo.file_id,
            uri: None,
            base64: None,
            mime: None,
            size: Some((photo.height * photo.width) as u64),
        }),
    }
}

pub fn parse_animation(
    animation: telegram_bot_api_rs::available_types::Animation,
) -> MessageSegment {
    MessageSegment::Video {
        file: Some(File {
            id: Some(animation.file_id.clone()),
            name: animation.file_id.clone(),
            uri: None,
            base64: None,
            mime: None,
            size: Some(animation.duration as u64),
        }),
        length: Some(animation.duration as i32),
    }
}

pub fn parse_audio(audio: telegram_bot_api_rs::available_types::Audio) -> MessageSegment {
    MessageSegment::Audio {
        file: Some(File {
            id: Some(audio.file_id.clone()),
            name: audio.file_id.clone(),
            uri: None,
            base64: None,
            mime: None,
            size: Some(audio.duration as u64),
        }),
        length: Some(audio.duration as i32),
    }
}

pub fn parse_video(video: telegram_bot_api_rs::available_types::Video) -> MessageSegment {
    MessageSegment::Video {
        file: Some(File {
            id: Some(video.file_id.clone()),
            name: video.file_id,
            uri: None,
            base64: None,
            mime: None,
            size: Some(video.duration as u64),
        }),
        length: Some(video.duration as i32),
    }
}

pub fn parse_document(document: telegram_bot_api_rs::available_types::Document) -> MessageSegment {
    MessageSegment::file(File {
        id: Some(document.file_id.clone()),
        name: document.file_name.unwrap_or_default(),
        uri: None,
        base64: None,
        mime: None,
        size: document.file_size.and_then(|s| Some(s as u64)),
    })
}

pub fn parse_sticker(sticker: telegram_bot_api_rs::stickers::types::Sticker) -> MessageSegment {
    MessageSegment::image(File {
        id: Some(sticker.file_id.clone()),
        name: sticker.emoji.unwrap_or("sticker".to_string()),
        uri: None,
        base64: None,
        mime: None,
        size: sticker.file_size.and_then(|s| Some(s as u64)),
    })
}

pub fn parse_voice(voice: telegram_bot_api_rs::available_types::Voice) -> MessageSegment {
    MessageSegment::audio(
        File {
            id: Some(voice.file_id.clone()),
            name: voice.file_id,
            uri: None,
            base64: None,
            mime: None,
            size: Some(voice.duration as u64),
        },
        Some(voice.duration as i32),
    )
}

pub fn parse_message_entity(
    entity: telegram_bot_api_rs::available_types::MessageEntity,
) -> Option<MessageSegment> {
    match entity {
        MessageEntity::TextMention { user, .. } => Some(MessageSegment::At {
            user_id: user.id.to_string(),
        }),
        MessageEntity::TextLink { url, .. } => Some(MessageSegment::text(url)),
        _ => None,
    }
}

pub fn parse_venue(venue: telegram_bot_api_rs::available_types::Venue) -> MessageSegment {
    MessageSegment::location(
        venue.location.latitude,
        venue.location.longitude,
        venue.title,
        Some(venue.address),
    )
}

pub fn parse_location(location: telegram_bot_api_rs::available_types::Location) -> MessageSegment {
    MessageSegment::location(location.latitude, location.longitude, "".to_string(), None)
}

pub fn parse_reaction(reaction: ReactionType) -> String {
    match reaction {
        ReactionType::Emoji { emoji } => emoji,
        ReactionType::CustomEmoji { custom_emoji_id } => custom_emoji_id,
        ReactionType::Paid => "paid".to_string(),
    }
}

pub fn process_message_segments(
    message: Vec<MessageSegment>,
) -> (
    Vec<String>,
    Vec<InputMedia>,
    Option<ReplyParameters>,
    Vec<MessageEntity>,
    Vec<Venue>,
    Vec<String>,
) {
    let mut text_segments = Vec::new();
    let mut media_segments = Vec::new();
    let mut reply = None;
    let mut entities = Vec::new();
    let mut venues = Vec::new();
    let mut stickers = Vec::new();

    for segment in message {
        match segment {
            MessageSegment::Text { content } => {
                text_segments.push(content);
            }
            MessageSegment::Image { file } => {
                if let Some(media_file) = file {
                    if let Some(id) = media_file.id {
                        media_segments.push(InputMedia::Photo {
                            media: id.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            show_caption_above_media: None,
                            has_spoiler: None,
                        });
                        tracing::debug!("Used id to send image");
                    } else if let Some(uri) = &media_file.uri {
                        media_segments.push(InputMedia::Photo {
                            media: uri.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            show_caption_above_media: None,
                            has_spoiler: None,
                        });
                        tracing::debug!("Used uri to send image");
                    } else {
                        tracing::warn!("No uri found for image");
                    }
                } else {
                    tracing::warn!("No file found for image");
                }
            }
            MessageSegment::Video { file, length } => {
                if let Some(media_file) = file {
                    if let Some(id) = media_file.id {
                        media_segments.push(InputMedia::Video {
                            media: id.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            duration: length.and_then(|l| Some(l as i64)),
                            width: None,
                            height: None,
                            supports_streaming: None,
                            has_spoiler: None,
                            thumbnail: None,
                            show_caption_above_media: None,
                        });
                        tracing::debug!("Used id to send video");
                    } else if let Some(uri) = &media_file.uri {
                        media_segments.push(InputMedia::Video {
                            media: uri.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            duration: length.and_then(|l| Some(l as i64)),
                            width: None,
                            height: None,
                            supports_streaming: None,
                            has_spoiler: None,
                            thumbnail: None,
                            show_caption_above_media: None,
                        });
                        tracing::debug!("Used uri to send video");
                    } else {
                        tracing::warn!("No uri found for video");
                    }
                } else {
                    tracing::warn!("No file found for video");
                }
            }
            MessageSegment::Audio { file, length } => {
                if let Some(media_file) = file {
                    if let Some(id) = media_file.id {
                        media_segments.push(InputMedia::Audio {
                            media: id.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            duration: length.and_then(|l| Some(l as i64)),
                            performer: None,
                            title: None,
                            thumbnail: None,
                        });
                        tracing::debug!("Used id to send audio");
                    } else if let Some(uri) = &media_file.uri {
                        media_segments.push(InputMedia::Audio {
                            media: uri.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            duration: length.and_then(|l| Some(l as i64)),
                            performer: None,
                            title: None,
                            thumbnail: None,
                        });
                        tracing::debug!("Used uri to send audio");
                    } else {
                        tracing::warn!("No uri found for audio");
                    }
                } else {
                    tracing::warn!("No file found for audio");
                }
            }
            MessageSegment::File { file } => {
                if let Some(media_file) = file {
                    if let Some(id) = media_file.id {
                        media_segments.push(InputMedia::Document {
                            media: id.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            thumbnail: None,
                            disable_content_type_detection: None,
                        });
                        tracing::debug!("Used id to send file");
                    } else if let Some(uri) = &media_file.uri {
                        media_segments.push(InputMedia::Document {
                            media: uri.to_string(),
                            caption: None,
                            parse_mode: None,
                            caption_entities: None,
                            thumbnail: None,
                            disable_content_type_detection: None,
                        });
                        tracing::debug!("Used uri to send file");
                    } else {
                        tracing::warn!("No uri found for file");
                    }
                } else {
                    tracing::warn!("No file found for file");
                }
            }
            MessageSegment::Share {
                title,
                content,
                url,
                image,
            } => {
                let mut caption = title;
                if let Some(desc) = content {
                    caption.push_str(&format!("\n{}", desc));
                }
                if let Some(media_file) = image {
                    if let Some(id) = media_file.id {
                        media_segments.push(InputMedia::Photo {
                            media: id.to_string(),
                            caption: Some(caption),
                            parse_mode: None,
                            caption_entities: None,
                            show_caption_above_media: None,
                            has_spoiler: None,
                        });
                    } else if let Some(uri) = &media_file.uri {
                        media_segments.push(InputMedia::Photo {
                            media: uri.to_string(),
                            caption: Some(caption),
                            parse_mode: None,
                            caption_entities: None,
                            show_caption_above_media: None,
                            has_spoiler: None,
                        });
                    }
                } else {
                    text_segments.push(format!("{}\n{}", caption, url));
                }
            }
            MessageSegment::Reply { message_id } => match split_id(message_id) {
                Ok((chat_id, message_id)) => {
                    match message_id.parse() {
                        Ok(id) => {
                            reply = Some(ReplyParameters {
                                message_id: Some(id),
                                chat_id: Some(chat_id),
                                ..Default::default()
                            });
                        }
                        Err(e) => {
                            tracing::error!("Error while parsing message id: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error while splitting id: {:?}", e);
                }
            },
            MessageSegment::At { user_id } => {
                if let Ok(id) = user_id.parse() {
                    entities.push(MessageEntity::TextMention {
                        offset: 0,
                        length: 0,
                        user: User {
                            id: id,
                            ..Default::default()
                        },
                    });
                } else {
                    tracing::warn!("Invalid user id for mention");
                }
            }
            MessageSegment::AtAll => {
                tracing::error!("At all is not supported in telegram");
            }
            MessageSegment::Reference { .. } => {
                tracing::error!("Reference is not supported in telegram");
            }
            MessageSegment::Location {
                latitude,
                longitude,
                title,
                content,
            } => venues.push(Venue {
                location: Location {
                    latitude,
                    longitude,
                    ..Default::default()
                },
                title: format!("{}\n{}", title, content.unwrap_or_default()),
                ..Default::default()
            }),
            MessageSegment::Emoji { id } => {
                stickers.push(id);
            }
            MessageSegment::ForwardNode { .. } => {
                tracing::error!("Forward node is not supported in telegram");
            }
            MessageSegment::ForwardCustomNode { .. } => {
                tracing::error!("Forward custom node is not supported in telegram");
            }
            MessageSegment::CustomString { .. } => {
                tracing::error!("Custom string is not supported in telegram");
            }
            MessageSegment::CustomValue { .. } => {
                tracing::error!("Custom value is not supported in telegram");
            }
        }
    }

    (
        text_segments,
        media_segments,
        reply,
        entities,
        venues,
        stickers,
    )
}
