use anyhow::{Context as _, Result};
use oxidebot::source::{
    group::{Group, GroupProfile},
    user::{User, UserProfile},
};

use crate::telegram::{Chat, User as TelegramUser};

pub fn telegram_user_name(user: &TelegramUser) -> String {
    user.username.clone().unwrap_or_else(|| {
        let name = format!(
            "{} {}",
            user.first_name,
            user.last_name.as_deref().unwrap_or_default()
        );
        name.trim().to_owned()
    })
}

pub fn parse_user(user: TelegramUser) -> User {
    User {
        id: user.id.to_string(),
        profile: Some(UserProfile {
            nickname: Some(telegram_user_name(&user)),
            ..Default::default()
        }),
        group_info: None,
    }
}

pub fn parse_chat_sender(chat: Chat) -> User {
    let nickname = chat.title.clone().or(chat.username.clone()).or_else(|| {
        let name = format!(
            "{} {}",
            chat.first_name.as_deref().unwrap_or_default(),
            chat.last_name.as_deref().unwrap_or_default()
        );
        (!name.trim().is_empty()).then(|| name.trim().to_owned())
    });
    User {
        id: chat.id.to_string(),
        profile: Some(UserProfile {
            nickname,
            ..Default::default()
        }),
        group_info: None,
    }
}

pub fn parse_group(group: Chat) -> Group {
    Group {
        id: group.id.to_string(),
        profile: Some(GroupProfile {
            name: group.title.or(group.username),
            avatar: None,
            member_count: None,
        }),
    }
}

pub fn message_id(chat_id: i64, message_id: i64) -> String {
    format!("{chat_id}_{message_id}")
}

pub fn split_id(id: impl AsRef<str>) -> Result<(String, i64)> {
    let id = id.as_ref();
    let (chat_id, message_id) = id
        .split_once('_')
        .ok_or_else(|| anyhow::anyhow!("invalid Telegram message id {id:?}"))?;
    anyhow::ensure!(!chat_id.is_empty(), "Telegram chat id is empty");
    anyhow::ensure!(
        !message_id.contains('_'),
        "invalid Telegram message id {id:?}"
    );
    let message_id = message_id
        .parse::<i64>()
        .with_context(|| format!("invalid Telegram message number in {id:?}"))?;
    Ok((chat_id.to_owned(), message_id))
}

pub fn join_request_id(chat_id: i64, user_id: i64) -> String {
    format!("{chat_id}:{user_id}")
}

pub fn split_join_request_id(id: &str) -> Result<(i64, i64)> {
    let (chat_id, user_id) = id
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid Telegram join-request id {id:?}"))?;
    Ok((
        chat_id
            .parse()
            .with_context(|| format!("invalid chat id in join-request id {id:?}"))?,
        user_id
            .parse()
            .with_context(|| format!("invalid user id in join-request id {id:?}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_round_trip_supports_negative_chat_ids() {
        assert_eq!(
            split_id(message_id(-1_001_234, 56)).unwrap(),
            ("-1001234".to_owned(), 56)
        );
    }

    #[test]
    fn message_id_rejects_trailing_components() {
        assert!(split_id("-100_12_extra").is_err());
    }

    #[test]
    fn join_request_id_round_trip() {
        let id = join_request_id(-100, 200);
        assert_eq!(split_join_request_id(&id).unwrap(), (-100, 200));
    }
}
