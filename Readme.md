# telegram Bot for oxidebot

# Usage
```
cargo add telegram_bot_oxidebot 
```

Example
```rust
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let manager = oxidebot::OxideBotManager::new()
        .bot(TelegramBot::new("token".to_string(), Default::default()).await)
        .await;
    manager.run_block().await;
}
```