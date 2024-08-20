use anyhow::Result;
use oxidebot::source::{
    group::{Group, GroupProfile},
    user::{User, UserProfile},
};

pub fn parse_user(user: telegram_bot_api_rs::available_types::User) -> User {
    User {
        id: user.id.to_string(),
        profile: Some(UserProfile {
            nickname: user.username,
            sex: None,
            age: None,
            avatar: None,
            email: None,
            phone: None,
            signature: None,
            level: None,
        }),
        group_info: None,
    }
}

pub fn parse_group(group: telegram_bot_api_rs::available_types::Chat) -> Group {
    Group {
        id: group.id.to_string(),
        profile: Some(GroupProfile {
            name: group.title,
            avatar: None,
            member_count: None,
        }),
    }
}

pub fn split_id(id: String) -> Result<(String, String)> {
    let mut iter = id.split('_');

    let chat_id = iter
        .next()
        .ok_or(anyhow::anyhow!("Failed to split chat_id"))?
        .to_string();
    let message_id = iter
        .next()
        .ok_or(anyhow::anyhow!("Failed to split message_id"))?
        .to_string();

    Ok((chat_id, message_id))
}
