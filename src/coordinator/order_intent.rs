use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use crate::control_plane::TradeIntent;
use crate::domain::{OrderType, Side, TimeInForce};
use crate::domain::Domain;

/// 訂單優先級
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrderPriority {
    /// 緊急 - 止損、強制平倉
    Critical = 0,
    /// 高 - 套利對沖腿
    High = 1,
    /// 正常 - 一般開倉
    Normal = 2,
    /// 低 - 投機性訂單
    Low = 3,
}

impl Default for OrderPriority {
    fn default() -> Self {
        OrderPriority::Normal
    }
}

fn metadata_needs_value(metadata: &HashMap<String, String>, key: &str) -> bool {
    metadata
        .get(key)
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
}

fn trade_intent_priority(priority: Option<&str>) -> OrderPriority {
    match priority
        .unwrap_or("normal")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "critical" => OrderPriority::Critical,
        "high" => OrderPriority::High,
        "low" => OrderPriority::Low,
        _ => OrderPriority::Normal,
    }
}

impl From<TradeIntent> for OrderIntent {
    fn from(intent: TradeIntent) -> Self {
        let TradeIntent {
            intent_id,
            deployment_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            side,
            is_buy,
            size,
            price_limit,
            confidence,
            edge,
            event_time,
            reason,
            priority,
            mut metadata,
        } = intent;

        if metadata_needs_value(&metadata, "deployment_id") {
            metadata.insert("deployment_id".to_string(), deployment_id);
        }
        if metadata_needs_value(&metadata, "intent_reason") {
            if let Some(reason) = reason {
                metadata.insert("intent_reason".to_string(), reason);
            }
        }
        if metadata_needs_value(&metadata, "signal_confidence") {
            if let Some(confidence) = confidence {
                metadata.insert(
                    "signal_confidence".to_string(),
                    confidence.normalize().to_string(),
                );
            }
        }
        if metadata_needs_value(&metadata, "signal_edge") {
            if let Some(edge) = edge {
                metadata.insert("signal_edge".to_string(), edge.normalize().to_string());
            }
        }
        if metadata_needs_value(&metadata, "event_time") {
            if let Some(event_time) = event_time {
                metadata.insert("event_time".to_string(), event_time.to_rfc3339());
            }
        }

        let mut order_intent = Self::new(
            agent_id,
            domain,
            market_slug,
            token_id,
            side,
            is_buy,
            size,
            price_limit,
        );
        order_intent.priority = trade_intent_priority(priority.as_deref());
        order_intent.intent_id = intent_id;
        order_intent.client_order_id = format!("intent:{intent_id}");
        order_intent.metadata = metadata;
        order_intent
    }
}

/// 訂單意圖 - Agent 提交給平台的下單請求
#[derive(Debug, Clone)]
pub struct OrderIntent {
    /// 提交的 Agent ID
    pub agent_id: String,
    /// 意圖 ID (用於追蹤)
    pub intent_id: Uuid,
    /// Strategy/runtime-scoped client order ID.
    pub client_order_id: String,
    /// 領域
    pub domain: Domain,
    /// 市場 slug
    pub market_slug: String,
    /// Token ID
    pub token_id: String,
    /// 買/賣方向
    pub side: Side,
    /// 買入或賣出
    pub is_buy: bool,
    /// 數量
    pub shares: u64,
    /// 限價
    pub limit_price: Decimal,
    /// 訂單類型
    pub order_type: OrderType,
    /// 有效時間
    pub time_in_force: TimeInForce,
    /// 優先級
    pub priority: OrderPriority,
    /// 創建時間
    pub created_at: DateTime<Utc>,
    /// 過期時間
    pub expires_at: Option<DateTime<Utc>>,
    /// 元數據 (策略相關信息)
    pub metadata: HashMap<String, String>,
}

impl OrderIntent {
    const METADATA_KEY_DEPLOYMENT_ID: &'static str = "deployment_id";
    const METADATA_KEY_CONDITION_ID: &'static str = "condition_id";

    pub fn new(
        agent_id: impl Into<String>,
        domain: Domain,
        market_slug: impl Into<String>,
        token_id: impl Into<String>,
        side: Side,
        is_buy: bool,
        shares: u64,
        limit_price: Decimal,
    ) -> Self {
        let intent_id = Uuid::new_v4();
        Self {
            agent_id: agent_id.into(),
            intent_id,
            client_order_id: format!("intent:{}", intent_id),
            domain,
            market_slug: market_slug.into(),
            token_id: token_id.into(),
            side,
            is_buy,
            shares,
            limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            priority: OrderPriority::Normal,
            created_at: Utc::now(),
            expires_at: None,
            metadata: HashMap::new(),
        }
    }

    pub fn with_priority(mut self, priority: OrderPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_client_order_id(mut self, client_order_id: impl Into<String>) -> Self {
        let client_order_id = client_order_id.into();
        if !client_order_id.trim().is_empty() {
            self.client_order_id = client_order_id;
        }
        self
    }

    pub fn with_order_type(mut self, order_type: OrderType) -> Self {
        self.order_type = order_type;
        self
    }

    pub fn with_time_in_force(mut self, time_in_force: TimeInForce) -> Self {
        self.time_in_force = time_in_force;
        self
    }

    pub fn with_expiry(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn with_deployment_id(mut self, deployment_id: impl Into<String>) -> Self {
        self.metadata.insert(
            Self::METADATA_KEY_DEPLOYMENT_ID.to_string(),
            deployment_id.into(),
        );
        self
    }

    pub fn deployment_id(&self) -> Option<&str> {
        self.metadata_value(Self::METADATA_KEY_DEPLOYMENT_ID)
    }

    pub fn with_condition_id(mut self, condition_id: impl Into<String>) -> Self {
        self.metadata.insert(
            Self::METADATA_KEY_CONDITION_ID.to_string(),
            condition_id.into(),
        );
        self
    }

    pub fn condition_id(&self) -> Option<&str> {
        const CONDITION_ID_KEYS: &[&str] = &[
            "condition_id",
            "conditionId",
            "condition",
            "market_condition_id",
            "marketConditionId",
        ];
        CONDITION_ID_KEYS
            .iter()
            .find_map(|key| self.metadata_value(key))
    }

    fn metadata_value(&self, key: &str) -> Option<&str> {
        self.metadata
            .get(key)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// 計算訂單價值 (USD)
    pub fn notional_value(&self) -> Decimal {
        self.limit_price * Decimal::from(self.shares)
    }

    /// 是否已過期
    pub fn is_expired(&self) -> bool {
        if let Some(expires) = self.expires_at {
            Utc::now() > expires
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn sample_intent() -> OrderIntent {
        OrderIntent::new(
            "agent-1",
            Domain::Crypto,
            "btc-updown-15m",
            "token-yes",
            Side::Up,
            true,
            10,
            Decimal::new(42, 2),
        )
    }

    #[test]
    fn order_intent_deployment_id_accessor_trims_and_rejects_blank() {
        let intent = sample_intent().with_deployment_id(" deploy.crypto.15m ");
        assert_eq!(intent.deployment_id(), Some("deploy.crypto.15m"));

        let blank = sample_intent().with_deployment_id("   ");
        assert_eq!(blank.deployment_id(), None);
    }

    #[test]
    fn order_intent_condition_id_accessor_supports_aliases() {
        let canonical = sample_intent().with_condition_id("0xabc");
        assert_eq!(canonical.condition_id(), Some("0xabc"));

        let alias = sample_intent().with_metadata("marketConditionId", " 0xdef ");
        assert_eq!(alias.condition_id(), Some("0xdef"));
    }

    #[test]
    fn order_intent_defaults_to_limit_gtc() {
        let intent = sample_intent();
        assert!(intent.client_order_id.starts_with("intent:"));
        assert_eq!(intent.order_type, OrderType::Limit);
        assert_eq!(intent.time_in_force, TimeInForce::GTC);
    }

    #[test]
    fn order_intent_from_trade_intent_maps_priority_and_metadata() {
        let intent = TradeIntent {
            intent_id: Uuid::new_v4(),
            deployment_id: "deploy.crypto.15m".to_string(),
            agent_id: "openclaw-agent".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-updown-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            size: 10,
            price_limit: dec!(0.42),
            confidence: Some(dec!(0.73)),
            edge: Some(dec!(0.05)),
            event_time: None,
            reason: Some("signal_edge".to_string()),
            priority: Some("high".to_string()),
            metadata: HashMap::new(),
        };

        let mapped = OrderIntent::from(intent);
        assert_eq!(mapped.priority, OrderPriority::High);
        assert_eq!(mapped.deployment_id(), Some("deploy.crypto.15m"));
        assert_eq!(
            mapped.metadata.get("intent_reason").map(String::as_str),
            Some("signal_edge")
        );
    }

    #[test]
    fn order_intent_from_trade_intent_normalizes_blank_deployment_metadata() {
        let mut intent = TradeIntent {
            intent_id: Uuid::new_v4(),
            deployment_id: "deploy.crypto.15m".to_string(),
            agent_id: "openclaw-agent".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-updown-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            size: 10,
            price_limit: dec!(0.42),
            confidence: None,
            edge: None,
            event_time: None,
            reason: None,
            priority: None,
            metadata: HashMap::new(),
        };
        intent
            .metadata
            .insert("deployment_id".to_string(), "   ".to_string());

        let mapped = OrderIntent::from(intent);
        assert_eq!(mapped.deployment_id(), Some("deploy.crypto.15m"));
    }
}
