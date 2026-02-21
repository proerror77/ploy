//! Core types for the Order Platform

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use std::str::FromStr;

use crate::domain::Side;

/// 領域類型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// 領域事件 - 不同領域有不同的事件結構
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// 體育事件更新
    Sports(SportsEvent),
    /// 加密貨幣價格更新
    Crypto(CryptoEvent),
    /// 政治事件更新
    Politics(PoliticsEvent),
    /// 通用報價更新
    QuoteUpdate(QuoteUpdateEvent),
    /// 訂單狀態更新
    OrderUpdate(OrderUpdateEvent),
    /// 定時觸發
    Tick(DateTime<Utc>),
}

impl DomainEvent {
    pub fn domain(&self) -> Domain {
        match self {
            DomainEvent::Sports(_) => Domain::Sports,
            DomainEvent::Crypto(_) => Domain::Crypto,
            DomainEvent::Politics(_) => Domain::Politics,
            DomainEvent::QuoteUpdate(e) => e.domain,
            DomainEvent::OrderUpdate(e) => e.domain,
            DomainEvent::Tick(_) => Domain::Crypto, // Default
        }
    }
}

/// 體育事件
#[derive(Debug, Clone)]
pub struct SportsEvent {
    /// 比賽 ID
    pub game_id: String,
    /// 市場 slug
    pub market_slug: String,
    /// 隊伍
    pub teams: (String, String),
    /// 聯盟
    pub league: String,
    /// 開賽時間
    pub game_time: Option<DateTime<Utc>>,
    /// 當前報價
    pub quotes: Option<QuoteData>,
    /// 賠率更新
    pub odds_update: Option<OddsData>,
    /// 傷病消息
    pub injury_news: Option<Vec<String>>,
}

/// 加密貨幣事件
#[derive(Debug, Clone)]
pub struct CryptoEvent {
    /// 交易對 (e.g., "BTCUSDT")
    pub symbol: String,
    /// 現貨價格
    pub spot_price: Decimal,
    /// 輪次 slug
    pub round_slug: Option<String>,
    /// UP/DOWN 報價
    pub quotes: Option<QuoteData>,
    /// 價格動量 (1s, 5s, 15s, 60s)
    pub momentum: Option<[f64; 4]>,
}

/// 政治事件
#[derive(Debug, Clone)]
pub struct PoliticsEvent {
    /// 事件 ID
    pub event_id: String,
    /// 市場 slug
    pub market_slug: String,
    /// 描述
    pub description: String,
    /// 當前報價
    pub quotes: Option<QuoteData>,
    /// 民調數據
    pub poll_data: Option<PollData>,
}

/// 報價數據
#[derive(Debug, Clone)]
pub struct QuoteData {
    pub up_bid: Decimal,
    pub up_ask: Decimal,
    pub down_bid: Decimal,
    pub down_ask: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl QuoteData {
    pub fn sum_of_asks(&self) -> Decimal {
        self.up_ask + self.down_ask
    }

    pub fn spread(&self, side: Side) -> Decimal {
        match side {
            Side::Up => self.up_ask - self.up_bid,
            Side::Down => self.down_ask - self.down_bid,
        }
    }
}

/// 賠率數據
#[derive(Debug, Clone)]
pub struct OddsData {
    pub spread: Option<f64>,
    pub over_under: Option<f64>,
    pub moneyline: Option<(i32, i32)>,
}

/// 民調數據
#[derive(Debug, Clone)]
pub struct PollData {
    pub candidate1_pct: f64,
    pub candidate2_pct: f64,
    pub margin_of_error: f64,
    pub source: String,
    pub date: DateTime<Utc>,
}

/// 報價更新事件
#[derive(Debug, Clone)]
pub struct QuoteUpdateEvent {
    pub domain: Domain,
    pub market_slug: String,
    pub token_id: String,
    pub side: Side,
    pub bid: Decimal,
    pub ask: Decimal,
    pub timestamp: DateTime<Utc>,
}

/// 訂單更新事件
#[derive(Debug, Clone)]
pub struct OrderUpdateEvent {
    pub domain: Domain,
    pub order_id: String,
    pub client_order_id: String,
    pub status: String,
    pub filled_shares: u64,
    pub avg_price: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
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

/// 執行狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// 等待執行
    Pending,
    /// 已提交
    Submitted,
    /// 部分成交
    PartiallyFilled,
    /// 完全成交
    Filled,
    /// 已取消
    Cancelled,
    /// 被拒絕
    Rejected,
    /// 過期
    Expired,
    /// 風控攔截
    RiskBlocked,
}

/// 執行報告 - 平台返回給 Agent 的執行結果
#[derive(Debug, Clone)]
pub struct ExecutionReport {
    /// 原始意圖 ID
    pub intent_id: Uuid,
    /// Agent ID
    pub agent_id: String,
    /// 交易所訂單 ID
    pub order_id: Option<String>,
    /// 執行狀態
    pub status: ExecutionStatus,
    /// 成交數量
    pub filled_shares: u64,
    /// 平均成交價
    pub avg_fill_price: Option<Decimal>,
    /// 手續費
    pub fees: Decimal,
    /// 錯誤信息
    pub error_message: Option<String>,
    /// 執行時間
    pub executed_at: DateTime<Utc>,
    /// 延遲 (毫秒)
    pub latency_ms: u64,
}

impl ExecutionReport {
    pub fn success(
        intent: &OrderIntent,
        order_id: String,
        filled: u64,
        avg_price: Decimal,
    ) -> Self {
        Self {
            intent_id: intent.intent_id,
            agent_id: intent.agent_id.clone(),
            order_id: Some(order_id),
            status: if filled == intent.shares {
                ExecutionStatus::Filled
            } else {
                ExecutionStatus::PartiallyFilled
            },
            filled_shares: filled,
            avg_fill_price: Some(avg_price),
            fees: Decimal::ZERO,
            error_message: None,
            executed_at: Utc::now(),
            latency_ms: 0,
        }
    }

    pub fn rejected(intent: &OrderIntent, reason: impl Into<String>) -> Self {
        Self {
            intent_id: intent.intent_id,
            agent_id: intent.agent_id.clone(),
            order_id: None,
            status: ExecutionStatus::Rejected,
            filled_shares: 0,
            avg_fill_price: None,
            fees: Decimal::ZERO,
            error_message: Some(reason.into()),
            executed_at: Utc::now(),
            latency_ms: 0,
        }
    }

    pub fn risk_blocked(intent: &OrderIntent, reason: impl Into<String>) -> Self {
        Self {
            intent_id: intent.intent_id,
            agent_id: intent.agent_id.clone(),
            order_id: None,
            status: ExecutionStatus::RiskBlocked,
            filled_shares: 0,
            avg_fill_price: None,
            fees: Decimal::ZERO,
            error_message: Some(reason.into()),
            executed_at: Utc::now(),
            latency_ms: 0,
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Filled | ExecutionStatus::PartiallyFilled
        )
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
