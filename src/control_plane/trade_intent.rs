use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::coordinator::{OrderIntent, OrderPriority};
use crate::domain::Side;
use crate::platform::Domain;

/// Unified strategy output contract (agent -> coordinator).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeIntent {
    pub intent_id: Uuid,
    pub deployment_id: String,
    pub agent_id: String,
    pub domain: Domain,
    pub market_slug: String,
    pub token_id: String,
    /// Binary outcome side: YES/NO mapped to UP/DOWN internally.
    pub side: Side,
    /// `true` = buy/open, `false` = sell/close.
    pub is_buy: bool,
    pub size: u64,
    pub price_limit: Decimal,
    pub confidence: Option<Decimal>,
    pub edge: Option<Decimal>,
    pub event_time: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    /// Optional priority hint (`critical|high|normal|low`).
    pub priority: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl TradeIntent {
    /// Convert control-plane intent to the coordinator queue type.
    pub fn into_order_intent(mut self) -> OrderIntent {
        if self
            .metadata
            .get("deployment_id")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            self.metadata
                .insert("deployment_id".to_string(), self.deployment_id.clone());
        }
        if self
            .metadata
            .get("intent_reason")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(reason) = self.reason.clone() {
                self.metadata.insert("intent_reason".to_string(), reason);
            }
        }
        if self
            .metadata
            .get("signal_confidence")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(confidence) = self.confidence {
                self.metadata.insert(
                    "signal_confidence".to_string(),
                    confidence.normalize().to_string(),
                );
            }
        }
        if self
            .metadata
            .get("signal_edge")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(edge) = self.edge {
                self.metadata
                    .insert("signal_edge".to_string(), edge.normalize().to_string());
            }
        }
        if self
            .metadata
            .get("event_time")
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
        {
            if let Some(ts) = self.event_time {
                self.metadata
                    .insert("event_time".to_string(), ts.to_rfc3339());
            }
        }

        let mut intent = OrderIntent::new(
            self.agent_id,
            self.domain,
            self.market_slug,
            self.token_id,
            self.side,
            self.is_buy,
            self.size,
            self.price_limit,
        );
        intent.priority = match self
            .priority
            .as_deref()
            .unwrap_or("normal")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "critical" => OrderPriority::Critical,
            "high" => OrderPriority::High,
            "low" => OrderPriority::Low,
            _ => OrderPriority::Normal,
        };
        intent.intent_id = self.intent_id;
        intent.client_order_id = format!("intent:{}", self.intent_id);
        intent.metadata = self.metadata;
        intent
    }
}
