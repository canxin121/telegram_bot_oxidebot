use std::{any::Any, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use oxidebot::{bot::BotObject, matcher::Matcher, source::bot::BotInfo, BotTrait};
use tokio::sync::broadcast;

use crate::{
    event::UpdateEvent,
    telegram::{GetUpdatesConfig, TelegramApiError, TelegramClient},
    utils::telegram_user_name,
    SERVER,
};

#[derive(Debug, Clone)]
pub struct TelegramBot {
    client: Arc<TelegramClient>,
    bot_info: Arc<BotInfo>,
    config: GetUpdatesConfig,
}

impl TelegramBot {
    /// Backwards-compatible constructor.
    ///
    /// New code should prefer [`Self::try_new`] so invalid credentials or a
    /// network error can be handled without a panic.
    #[allow(clippy::new_ret_no_self)] // Kept for the crate's 0.1 API compatibility.
    pub async fn new(token: String, config: GetUpdatesConfig) -> BotObject {
        Self::try_new(token, config).await.expect(
            "failed to initialize Telegram bot; use TelegramBot::try_new to handle this error",
        )
    }

    pub async fn try_new(token: impl Into<String>, config: GetUpdatesConfig) -> Result<BotObject> {
        let client = TelegramClient::new(token)?;
        Self::try_from_client(client, config).await
    }

    /// Creates a bot using a local or test Bot API server.
    pub async fn try_with_api_base(
        token: impl Into<String>,
        api_base: impl Into<String>,
        config: GetUpdatesConfig,
    ) -> Result<BotObject> {
        let client = TelegramClient::with_api_base(token, api_base)?;
        Self::try_from_client(client, config).await
    }

    async fn try_from_client(
        client: TelegramClient,
        config: GetUpdatesConfig,
    ) -> Result<BotObject> {
        anyhow::ensure!(
            (1..=100).contains(&config.limit),
            "Telegram getUpdates limit must be between 1 and 100"
        );
        let me = client
            .get_me()
            .await
            .context("Telegram getMe failed while initializing the bot")?;
        let bot_info = BotInfo {
            id: Some(me.id.to_string()),
            nickname: Some(telegram_user_name(&me)),
        };
        tracing::info!(bot = ?bot_info, api_version = crate::telegram::BOT_API_VERSION, "connected to Telegram");
        Ok(Box::new(Self {
            client: Arc::new(client),
            bot_info: Arc::new(bot_info),
            config,
        }))
    }

    /// Returns the low-level, token-redacting client for Telegram-specific
    /// methods that do not have an oxidebot-wide abstraction.
    pub fn client(&self) -> &TelegramClient {
        &self.client
    }
}

#[async_trait::async_trait]
impl BotTrait for TelegramBot {
    async fn bot_info(&self) -> BotInfo {
        self.bot_info.as_ref().clone()
    }

    async fn start_sending_events(&self, sender: broadcast::Sender<Matcher>) {
        let mut config = self.config.clone();
        let mut retry_delay = Duration::from_secs(1);

        loop {
            match self.client.get_updates(&config).await {
                Ok(updates) => {
                    retry_delay = Duration::from_secs(1);
                    for update in updates {
                        config.offset = Some(update.update_id.saturating_add(1));
                        let matchers = Matcher::new(UpdateEvent::boxed(update), self.clone_box());
                        for matcher in matchers {
                            if sender.send(matcher).is_err() {
                                tracing::warn!(
                                    "oxidebot event receiver was dropped; stopping Telegram polling"
                                );
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    let telegram_retry_delay = error
                        .downcast_ref::<TelegramApiError>()
                        .and_then(|error| error.parameters.as_ref())
                        .and_then(|parameters| parameters.retry_after)
                        .map(Duration::from_secs);
                    let delay = telegram_retry_delay.unwrap_or(retry_delay);
                    tracing::error!(
                        error = %error,
                        retry_in_seconds = delay.as_secs(),
                        "Telegram getUpdates failed"
                    );
                    tokio::time::sleep(delay).await;
                    retry_delay = if telegram_retry_delay.is_some() {
                        Duration::from_secs(1)
                    } else {
                        retry_delay.saturating_mul(2).min(Duration::from_secs(30))
                    };
                }
            }
        }
    }

    fn server(&self) -> &'static str {
        SERVER
    }

    fn clone_box(&self) -> BotObject {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
