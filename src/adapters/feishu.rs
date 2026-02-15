//! Feishu (Lark) webhook notifications
//!
//! Sends trade notifications to Feishu bot.

use reqwest::Client;
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, error, info};

/// Feishu notification client
#[derive(Clone)]
pub struct FeishuNotifier {
    client: Client,
    webhook_url: String,
}

#[derive(Serialize)]
struct FeishuMessage {
    msg_type: String,
    content: FeishuContent,
}

#[derive(Serialize)]
struct FeishuContent {
    text: String,
}

impl FeishuNotifier {
    /// Create a new Feishu notifier from environment variable
    pub fn from_env() -> Option<Arc<Self>> {
        std::env::var("FEISHU_WEBHOOK_URL").ok().map(|url| {
            info!("Feishu notifications enabled");
            Arc::new(Self {
                client: Client::new(),
                webhook_url: url,
            })
        })
    }

    /// Create a new Feishu notifier with explicit URL
    pub fn new(webhook_url: String) -> Arc<Self> {
        Arc::new(Self {
            client: Client::new(),
            webhook_url,
        })
    }

    /// Send a text message to Feishu
    pub async fn send_message(&self, text: &str) -> Result<(), String> {
        let message = FeishuMessage {
            msg_type: "text".to_string(),
            content: FeishuContent {
                text: text.to_string(),
            },
        };

        match self
            .client
            .post(&self.webhook_url)
            .json(&message)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    debug!("Feishu notification sent successfully");
                    Ok(())
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    error!("Feishu notification failed: {} - {}", status, body);
                    Err(format!("HTTP {}: {}", status, body))
                }
            }
            Err(e) => {
                error!("Feishu request failed: {}", e);
                Err(e.to_string())
            }
        }
    }

    /// Send trade notification
    pub async fn notify_trade(
        &self,
        action: &str, // "BUY" or "SELL"
        market: &str, // Market name
        side: &str,   // "UP" or "DOWN"
        price: f64,
        size: f64,
        order_id: Option<&str>,
    ) {
        let emoji = if action == "BUY" { "🟢" } else { "🔴" };
        let cost = price * size;

        let text = format!(
            "{} {} {} {}\n\
             Price: {:.2}¢ | Size: {} | Cost: ${:.2}\n\
             {}",
            emoji,
            action,
            side,
            market,
            price * 100.0,
            size as i64,
            cost,
            order_id
                .map(|id| format!("Order: {}", &id[..16.min(id.len())]))
                .unwrap_or_default()
        );

        if let Err(e) = self.send_message(&text).await {
            error!("Failed to send trade notification: {}", e);
        }
    }

    /// Send order filled notification
    pub async fn notify_order_filled(
        &self,
        action: &str,
        market: &str,
        side: &str,
        price: f64,
        size: f64,
        pnl: Option<f64>,
    ) {
        let emoji = if action == "BUY" { "🟢" } else { "🔴" };

        let pnl_str = pnl
            .map(|p| {
                let pnl_emoji = if p >= 0.0 { "📈" } else { "📉" };
                format!("\n{} PnL: ${:.2}", pnl_emoji, p)
            })
            .unwrap_or_default();

        let text = format!(
            "{} FILLED: {} {} {}\n\
             Price: {:.2}¢ | Size: {}{}",
            emoji,
            action,
            side,
            market,
            price * 100.0,
            size as i64,
            pnl_str
        );

        if let Err(e) = self.send_message(&text).await {
            error!("Failed to send fill notification: {}", e);
        }
    }

    /// Send position closed notification with PnL
    pub async fn notify_position_closed(
        &self,
        market: &str,
        side: &str,
        entry_price: f64,
        exit_price: f64,
        size: f64,
        pnl: f64,
        pnl_percent: f64,
    ) {
        let emoji = if pnl >= 0.0 { "🎉" } else { "😢" };
        let pnl_emoji = if pnl >= 0.0 { "📈" } else { "📉" };

        let text = format!(
            "{} POSITION CLOSED: {} {}\n\
             Entry: {:.2}¢ -> Exit: {:.2}¢\n\
             Size: {} shares\n\
             {} PnL: ${:.2} ({:+.1}%)",
            emoji,
            side,
            market,
            entry_price * 100.0,
            exit_price * 100.0,
            size as i64,
            pnl_emoji,
            pnl,
            pnl_percent
        );

        if let Err(e) = self.send_message(&text).await {
            error!("Failed to send position closed notification: {}", e);
        }
    }

    /// Send startup notification
    pub async fn notify_startup(&self, symbols: &[String], config_summary: &str) {
        let text = format!(
            "🚀 Trading Bot Started\n\
             Symbols: {}\n\
             {}",
            symbols.join(", "),
            config_summary
        );

        if let Err(e) = self.send_message(&text).await {
            error!("Failed to send startup notification: {}", e);
        }
    }

    /// Send error notification
    pub async fn notify_error(&self, error: &str) {
        let text = format!("⚠️ Error: {}", error);
        if let Err(e) = self.send_message(&text).await {
            error!("Failed to send error notification: {}", e);
        }
    }
}
