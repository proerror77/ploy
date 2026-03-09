//! Core types for the Order Platform

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use std::str::FromStr;

use crate::domain::Side;

/// 領域類型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Domain {
    /// 體育賽事 (NBA, NFL, etc.)
    Sports,
    /// 加密貨幣 (BTC, ETH, SOL 15分鐘輪)
    Crypto,
    /// 政治事件
    Politics,
    /// 經濟指標
    Economics,
    /// 自定義領域
    Custom(u32),
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Domain::Sports => write!(f, "Sports"),
            Domain::Crypto => write!(f, "Crypto"),
            Domain::Politics => write!(f, "Politics"),
            Domain::Economics => write!(f, "Economics"),
            Domain::Custom(id) => write!(f, "Custom({})", id),
        }
    }
}

impl FromStr for Domain {
    type Err = &'static str;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err("domain is empty");
        }

        if let Some(custom) = normalized.strip_prefix("custom:") {
            let id = custom
                .trim()
                .parse::<u32>()
                .map_err(|_| "custom domain id must be a non-negative integer")?;
            return Ok(Domain::Custom(id));
        }

        match normalized.as_str() {
            "crypto" => Ok(Domain::Crypto),
            "sports" => Ok(Domain::Sports),
            "politics" => Ok(Domain::Politics),
            "economics" => Ok(Domain::Economics),
            _ => Err("invalid domain; expected crypto|sports|politics|economics|custom:<id>"),
        }
    }
}

impl<'de> Deserialize<'de> for Domain {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct DomainVisitor;

        impl<'de> de::Visitor<'de> for DomainVisitor {
            type Value = Domain;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a domain string like \"crypto\" or \"custom:42\"")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Domain, E> {
                Domain::from_str(v).map_err(de::Error::custom)
            }

            fn visit_map<A: de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Domain, A::Error> {
                // Handle derived-Serialize format: {"Custom": 42}
                if let Some(key) = map.next_key::<String>()? {
                    if key.eq_ignore_ascii_case("custom") {
                        let id: u32 = map.next_value()?;
                        return Ok(Domain::Custom(id));
                    }
                    // Try as a known domain name (shouldn't happen, but be safe)
                    return Domain::from_str(&key).map_err(de::Error::custom);
                }
                Err(de::Error::custom("empty map for Domain"))
            }
        }

        deserializer.deserialize_any(DomainVisitor)
    }
}

impl Domain {
    pub fn parse_optional(raw: Option<&str>, default: Domain) -> std::result::Result<Self, String> {
        match raw {
            None => Ok(default),
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    Ok(default)
                } else {
                    Self::from_str(trimmed).map_err(|e| e.to_string())
                }
            }
        }
    }
}

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

/// 訂單意圖 - Agent 提交給平台的下單請求
#[derive(Debug, Clone)]
pub struct OrderIntent {
    /// 提交的 Agent ID
    pub agent_id: String,
    /// 意圖 ID (用於追蹤)
    pub intent_id: Uuid,
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
        Self {
            agent_id: agent_id.into(),
            intent_id: Uuid::new_v4(),
            domain,
            market_slug: market_slug.into(),
            token_id: token_id.into(),
            side,
            is_buy,
            shares,
            limit_price,
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
}
