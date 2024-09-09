use std::{any::Any, sync::Arc};

use oxidebot::{bot::BotObject, matcher::Matcher, source::bot::BotInfo, BotTrait};
use telegram_bot_api_rs::getting_updates::GetUpdateConfig;
use tokio::sync::broadcast;

use crate::{event::UpdateEvent, SERVER};

#[derive(Debug, Clone)]
pub struct TelegramBot {
    pub bot: Arc<telegram_bot_api_rs::bot::Bot>,
    pub bot_info: Arc<BotInfo>,
    pub config: GetUpdateConfig,
}

impl TelegramBot {
    pub async fn new(token: String, config: GetUpdateConfig) -> BotObject {
        let bot = telegram_bot_api_rs::bot::Bot::new(token);
        let bot_info = bot.get_me().await.unwrap();
        let bot_info = BotInfo {
            id: Some(bot_info.id.to_string()),
            nickname: Some(bot_info.username.unwrap_or_else(|| {
                format!(
                    "{}{}",
                    bot_info.first_name,
                    bot_info.last_name.unwrap_or("".to_string())
                )
            })),
        };
        tracing::info!("Connection succeed: {:?}", bot_info);
        Box::new(Self {
            bot: bot.into(),
            bot_info: bot_info.into(),
            config: config,
        })
    }
}

impl BotTrait for TelegramBot {
    #[must_use]
    #[allow(clippy::type_complexity, clippy::type_repetition_in_bounds)]
    fn bot_info<'life0, 'async_trait>(
        &'life0 self,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = BotInfo> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.bot_info.as_ref().clone() })
    }

    #[must_use]
    #[allow(clippy::type_complexity, clippy::type_repetition_in_bounds)]
    fn start_sending_events<'life0, 'async_trait>(
        &'life0 self,
        sender: broadcast::Sender<Matcher>,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = ()> + ::core::marker::Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        self.bot.start_get_updates(self.config.clone());
        let mut subscriber = self.bot.subscribe_updates();
        Box::pin(async move {
            loop {
                match subscriber.recv().await {
                    Ok(update) => {
                        let matchers = Matcher::new(UpdateEvent::new(update), self.clone_box());
                        for matcher in matchers {
                            sender.send(matcher).unwrap();
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error while receiving update: {:?}", e);
                    }
                }
            }
        })
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
