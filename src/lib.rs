#![doc = include_str!("../Readme.md")]

pub mod api;
pub mod bot;
pub mod event;
pub mod methods;
pub mod segment;
pub mod telegram;
mod tls;
pub mod utils;

pub use bot::TelegramBot;
pub use methods::{TELEGRAM_BOT_API_METHODS, TELEGRAM_BOT_API_UPDATE_TYPES};
pub use telegram::{
    GetUpdatesConfig, ResponseParameters, TelegramApiError, TelegramClient, BOT_API_VERSION,
};

pub const SERVER: &str = "telegram";
