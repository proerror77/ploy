use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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
