//! A small, forward-compatible Telegram Bot API client.
//!
//! The adapter used to depend on `telegram_bot_api_rs`, whose public schema is
//! frozen at Bot API 7.9.  Updates are intentionally decoded as a map here: a
//! new Telegram update kind can therefore be forwarded as a raw event instead
//! of making the whole `getUpdates` response fail to deserialize.

use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use hmac::{Hmac, KeyInit as _, Mac as _};
use reqwest::multipart::{Form, Part};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::Sha256;

pub const BOT_API_VERSION: &str = "10.2";

#[derive(Clone)]
pub struct TelegramClient {
    token: Arc<str>,
    api_base: Arc<str>,
    client: reqwest::Client,
}

impl fmt::Debug for TelegramClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramClient")
            .field("token", &"<redacted>")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl TelegramClient {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        Self::with_api_base(token, "https://api.telegram.org")
    }

    /// Creates a client for Telegram or a compatible local Bot API server.
    pub fn with_api_base(token: impl Into<String>, api_base: impl Into<String>) -> Result<Self> {
        let token = token.into();
        anyhow::ensure!(
            !token.trim().is_empty(),
            "Telegram bot token cannot be empty"
        );
        anyhow::ensure!(
            token
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_:-".contains(character)),
            "Telegram bot token contains invalid characters"
        );

        let api_base = api_base.into().trim_end_matches('/').to_owned();
        let client = crate::tls::webpki_client_builder()
            .context("failed to load WebPKI root certificates")?
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .context("failed to create Telegram HTTP client")?;

        Ok(Self {
            token: token.into(),
            api_base: api_base.into(),
            client,
        })
    }

    fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base, self.token, method)
    }

    pub fn file_url(&self, path: &str) -> String {
        format!("{}/file/bot{}/{}", self.api_base, self.token, path)
    }

    pub async fn call<P, T>(&self, method: &str, payload: &P) -> Result<T>
    where
        P: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        validate_method(method)?;
        let response = self
            .client
            .post(self.method_url(method))
            .json(payload)
            .send()
            .await
            .with_context(|| format!("Telegram {method} request failed"))?;
        self.decode_response(method, response).await
    }

    pub async fn call_multipart<T>(&self, method: &str, form: Form) -> Result<T>
    where
        T: DeserializeOwned,
    {
        validate_method(method)?;
        let response = self
            .client
            .post(self.method_url(method))
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("Telegram {method} upload failed"))?;
        self.decode_response(method, response).await
    }

    async fn decode_response<T>(&self, method: &str, response: reqwest::Response) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let status = response.status();
        let body = response
            .bytes()
            .await
            .with_context(|| format!("failed to read Telegram {method} response"))?;
        let response: ApiResponse<T> = serde_json::from_slice(&body).with_context(|| {
            let preview = String::from_utf8_lossy(&body);
            let preview: String = preview.chars().take(500).collect();
            format!("Telegram {method} returned invalid JSON (HTTP {status}): {preview}")
        })?;

        if response.ok {
            response
                .result
                .ok_or_else(|| anyhow::anyhow!("Telegram {method} succeeded without a result"))
        } else {
            Err(TelegramApiError {
                method: method.to_owned(),
                http_status: status.as_u16(),
                error_code: response.error_code,
                description: response
                    .description
                    .unwrap_or_else(|| "unknown Telegram API error".to_owned()),
                parameters: response.parameters,
            }
            .into())
        }
    }

    pub async fn get_updates(&self, config: &GetUpdatesConfig) -> Result<Vec<Update>> {
        self.call("getUpdates", config).await
    }

    pub async fn get_me(&self) -> Result<User> {
        self.call("getMe", &EmptyPayload {}).await
    }

    /// Verifies Telegram Mini App initialization data with the bot-token HMAC.
    /// `max_age` rejects otherwise valid replayed payloads when supplied.
    pub fn verify_web_app_init_data(
        &self,
        init_data: &str,
        max_age: Option<Duration>,
    ) -> Result<Map<String, Value>> {
        let mut fields = url::form_urlencoded::parse(init_data.as_bytes())
            .into_owned()
            .collect::<Vec<_>>();
        anyhow::ensure!(
            fields.iter().filter(|(name, _)| name == "hash").count() == 1,
            "Telegram Mini App init data must contain exactly one hash"
        );
        let hash_index = fields
            .iter()
            .position(|(name, _)| name == "hash")
            .context("Telegram Mini App init data has no hash")?;
        let (_, hash) = fields.remove(hash_index);
        fields.retain(|(name, _)| name != "signature");
        fields.sort_by(|left, right| left.0.cmp(&right.0));
        anyhow::ensure!(
            fields.windows(2).all(|fields| fields[0].0 != fields[1].0),
            "Telegram Mini App init data contains duplicate fields"
        );
        let data_check_string = fields
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("\n");

        let mut secret =
            Hmac::<Sha256>::new_from_slice(b"WebAppData").expect("HMAC accepts keys of any length");
        secret.update(self.token.as_bytes());
        let secret = secret.finalize().into_bytes();
        let expected = decode_hex(&hash).context("invalid Telegram Mini App hash")?;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&secret).expect("HMAC accepts keys of any length");
        mac.update(data_check_string.as_bytes());
        mac.verify_slice(&expected)
            .map_err(|_| anyhow::anyhow!("invalid Telegram Mini App signature"))?;

        let mut result = Map::new();
        for (name, value) in fields {
            let value = if matches!(name.as_str(), "user" | "receiver" | "chat") {
                serde_json::from_str(&value)
                    .with_context(|| format!("invalid Telegram Mini App {name} JSON"))?
            } else if matches!(name.as_str(), "auth_date" | "can_send_after") {
                value
                    .parse::<i64>()
                    .map(Value::from)
                    .unwrap_or(Value::String(value))
            } else {
                Value::String(value)
            };
            result.insert(name, value);
        }
        result.insert("hash".to_owned(), hash.into());

        if let Some(max_age) = max_age {
            let auth_date = result
                .get("auth_date")
                .and_then(Value::as_i64)
                .context("Telegram Mini App init data has no valid auth_date")?;
            let age = chrono::Utc::now().timestamp().saturating_sub(auth_date);
            anyhow::ensure!(age >= 0, "Telegram Mini App auth_date is in the future");
            anyhow::ensure!(
                u64::try_from(age).unwrap_or(u64::MAX) <= max_age.as_secs(),
                "Telegram Mini App init data has expired"
            );
        }
        Ok(result)
    }

    pub async fn get_file(&self, file_id: &str) -> Result<TelegramFile> {
        self.call("getFile", &serde_json::json!({ "file_id": file_id }))
            .await
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(value.len().is_multiple_of(2), "hex value has odd length");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("hex input is UTF-8");
            u8::from_str_radix(pair, 16).with_context(|| format!("invalid hex byte {pair:?}"))
        })
        .collect()
}

fn validate_method(method: &str) -> Result<()> {
    anyhow::ensure!(!method.is_empty(), "Telegram API method cannot be empty");
    anyhow::ensure!(
        method
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'),
        "invalid Telegram API method {method:?}"
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: Option<i64>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<ResponseParameters>,
}

/// Additional recovery information returned with a failed Telegram request.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ResponseParameters {
    #[serde(default)]
    pub migrate_to_chat_id: Option<i64>,
    #[serde(default)]
    pub retry_after: Option<u64>,
}

/// A structured Bot API error that can be recovered with
/// [`anyhow::Error::downcast_ref`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramApiError {
    pub method: String,
    pub http_status: u16,
    pub error_code: Option<i64>,
    pub description: String,
    pub parameters: Option<ResponseParameters>,
}

impl fmt::Display for TelegramApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Telegram {} failed (HTTP {}, API code {}): {}",
            self.method,
            self.http_status,
            self.error_code.unwrap_or_default(),
            self.description
        )?;
        if let Some(parameters) = &self.parameters {
            if let Some(chat_id) = parameters.migrate_to_chat_id {
                write!(formatter, "; migrated to chat {chat_id}")?;
            }
            if let Some(seconds) = parameters.retry_after {
                write!(formatter, "; retry after {seconds}s")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for TelegramApiError {}

#[derive(Serialize)]
struct EmptyPayload {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetUpdatesConfig {
    #[serde(default = "default_limit")]
    pub limit: u8,
    #[serde(default = "default_timeout")]
    pub timeout: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_updates: Option<Vec<String>>,
}

const fn default_limit() -> u8 {
    100
}

const fn default_timeout() -> u16 {
    60
}

impl Default for GetUpdatesConfig {
    fn default() -> Self {
        Self {
            limit: default_limit(),
            timeout: default_timeout(),
            offset: None,
            allowed_updates: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(flatten)]
    pub data: Map<String, Value>,
}

impl Update {
    pub fn kind(&self) -> Option<&str> {
        self.data.keys().next().map(String::as_str)
    }

    pub fn value(&self) -> Option<&Value> {
        self.data.values().next()
    }

    pub fn decode<T: DeserializeOwned>(&self, name: &str) -> Result<T> {
        let value = self
            .data
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("update does not contain {name}"))?;
        serde_json::from_value(value)
            .with_context(|| format!("failed to decode Telegram {name} update"))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatFullInfo {
    pub id: i64,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub birthdate: Option<Birthdate>,
    #[serde(default)]
    pub photo: Option<ChatPhoto>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Birthdate {
    pub day: u32,
    pub month: u32,
    #[serde(default)]
    pub year: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatPhoto {
    pub small_file_id: String,
    pub big_file_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub date: i64,
    pub chat: Chat,
    #[serde(default)]
    pub from: Option<User>,
    #[serde(default)]
    pub sender_chat: Option<Chat>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub entities: Option<Vec<MessageEntity>>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub animation: Option<Animation>,
    #[serde(default)]
    pub audio: Option<Audio>,
    #[serde(default)]
    pub video: Option<Video>,
    #[serde(default)]
    pub video_note: Option<VideoNote>,
    #[serde(default)]
    pub document: Option<Document>,
    #[serde(default)]
    pub sticker: Option<Sticker>,
    #[serde(default)]
    pub voice: Option<Voice>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub caption_entities: Option<Vec<MessageEntity>>,
    #[serde(default)]
    pub venue: Option<Venue>,
    #[serde(default)]
    pub location: Option<Location>,
    #[serde(default)]
    pub new_chat_members: Option<Vec<User>>,
    #[serde(default)]
    pub left_chat_member: Option<User>,
    #[serde(default)]
    pub rich_message: Option<Value>,
    #[serde(default)]
    pub live_photo: Option<Value>,
    #[serde(default)]
    pub paid_media: Option<Value>,
    #[serde(default)]
    pub story: Option<Value>,
    #[serde(default)]
    pub contact: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub kind: String,
    pub offset: usize,
    pub length: usize,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub custom_emoji_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    #[serde(default)]
    pub width: u64,
    #[serde(default)]
    pub height: u64,
    #[serde(default)]
    pub file_size: Option<u64>,
}

macro_rules! telegram_media_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Default, Deserialize)]
        pub struct $name {
            pub file_id: String,
            #[serde(default)]
            pub file_name: Option<String>,
            #[serde(default)]
            pub mime_type: Option<String>,
            #[serde(default)]
            pub duration: i64,
            #[serde(default)]
            pub file_size: Option<u64>,
        }
    };
}

telegram_media_type!(Animation);
telegram_media_type!(Audio);
telegram_media_type!(Video);
telegram_media_type!(VideoNote);
telegram_media_type!(Document);
telegram_media_type!(Voice);

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Sticker {
    pub file_id: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Venue {
    pub location: Location,
    pub title: String,
    pub address: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct MessageReactionUpdated {
    pub chat: Chat,
    pub message_id: i64,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub actor_chat: Option<Chat>,
    #[serde(default)]
    pub old_reaction: Vec<ReactionType>,
    #[serde(default)]
    pub new_reaction: Vec<ReactionType>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub message: Option<Message>,
    #[serde(default)]
    pub inline_message_id: Option<String>,
    #[serde(default)]
    pub chat_instance: String,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub game_short_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReactionType {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub custom_emoji_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatMemberUpdated {
    pub chat: Chat,
    pub from: User,
    pub date: i64,
    pub old_chat_member: ChatMember,
    pub new_chat_member: ChatMember,
    #[serde(default)]
    pub via_join_request: Option<bool>,
    #[serde(default)]
    pub via_chat_folder_invite_link: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatMember {
    pub status: String,
    pub user: User,
    #[serde(default)]
    pub is_member: Option<bool>,
    #[serde(default)]
    pub can_send_messages: Option<bool>,
    #[serde(default)]
    pub until_date: Option<i64>,
    #[serde(default)]
    pub custom_title: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ChatMember {
    pub fn is_present(&self) -> bool {
        match self.status.as_str() {
            "creator" | "administrator" | "member" => true,
            "restricted" => self.is_member.unwrap_or(true),
            _ => false,
        }
    }

    pub fn is_admin(&self) -> bool {
        matches!(self.status.as_str(), "creator" | "administrator")
    }

    pub fn is_muted(&self) -> bool {
        self.status == "restricted" && self.can_send_messages == Some(false)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ChatJoinRequest {
    pub chat: Chat,
    pub from: User,
    #[serde(default)]
    pub user_chat_id: i64,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub query_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TelegramFile {
    pub file_id: String,
    #[serde(default)]
    pub file_unique_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub file_path: Option<String>,
}

#[derive(Debug)]
pub struct Upload {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: Option<String>,
}

impl Upload {
    pub fn into_part(self) -> Result<Part> {
        let mut part = Part::bytes(self.bytes).file_name(self.name);
        if let Some(mime) = self.mime {
            part = part
                .mime_str(&mime)
                .with_context(|| format!("invalid upload MIME type {mime}"))?;
        }
        Ok(part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_bot_api_10_2_update_stays_decodable() {
        let update: Update = serde_json::from_value(serde_json::json!({
            "update_id": 42,
            "subscription": {
                "id": "sub",
                "new_field_from_a_future_api": true
            }
        }))
        .unwrap();

        assert_eq!(update.kind(), Some("subscription"));
        assert_eq!(update.value().unwrap()["new_field_from_a_future_api"], true);
    }

    #[test]
    fn client_debug_never_contains_the_token() {
        let client = TelegramClient::with_api_base("123:top-secret", "http://localhost").unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains("top-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn restricted_member_presence_uses_is_member() {
        let mut member = ChatMember {
            status: "restricted".to_owned(),
            is_member: Some(false),
            ..Default::default()
        };
        assert!(!member.is_present());
        member.is_member = Some(true);
        assert!(member.is_present());
    }

    #[test]
    fn method_names_cannot_escape_the_bot_api_path() {
        assert!(validate_method("sendRichMessage").is_ok());
        assert!(validate_method("future_method_1").is_ok());
        assert!(validate_method("../getMe").is_err());
        assert!(validate_method("").is_err());
    }

    #[test]
    fn api_errors_preserve_all_response_parameters() {
        let error = TelegramApiError {
            method: "sendMessage".to_owned(),
            http_status: 429,
            error_code: Some(429),
            description: "Too Many Requests".to_owned(),
            parameters: Some(ResponseParameters {
                migrate_to_chat_id: Some(-1_001),
                retry_after: Some(5),
            }),
        };

        assert!(error.to_string().contains("migrated to chat -1001"));
        assert!(error.to_string().contains("retry after 5s"));
    }

    #[test]
    fn mini_app_init_data_is_authenticated_and_decoded() {
        let token = "123:test-secret";
        let user = r#"{"id":42,"first_name":"User","is_bot":false}"#;
        let check = format!("auth_date=1700000000\nquery_id=query-1\nuser={user}");
        let mut secret = Hmac::<Sha256>::new_from_slice(b"WebAppData").unwrap();
        secret.update(token.as_bytes());
        let secret = secret.finalize().into_bytes();
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret).unwrap();
        mac.update(check.as_bytes());
        let hash = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let init_data = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("query_id", "query-1")
            .append_pair("user", user)
            .append_pair("auth_date", "1700000000")
            .append_pair("hash", &hash)
            .finish();
        let client = TelegramClient::with_api_base(token, "http://localhost").unwrap();
        let verified = client.verify_web_app_init_data(&init_data, None).unwrap();
        assert_eq!(verified["user"]["id"], 42);
        assert_eq!(verified["auth_date"], 1_700_000_000_i64);

        let tampered = init_data.replace("query-1", "query-2");
        assert!(client.verify_web_app_init_data(&tampered, None).is_err());
    }
}
