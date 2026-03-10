use super::*;

/// Market response from CLOB API
#[derive(Debug, Deserialize, Serialize)]
pub struct MarketResponse {
    pub condition_id: String,
    #[serde(default)]
    pub question_id: Option<String>,
    #[serde(default)]
    pub tokens: Vec<TokenInfo>,
    #[serde(default)]
    pub minimum_order_size: Option<serde_json::Value>,
    #[serde(default)]
    pub minimum_tick_size: Option<serde_json::Value>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub end_date_iso: Option<String>,
    #[serde(default)]
    pub neg_risk: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TokenInfo {
    pub token_id: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default, deserialize_with = "deserialize_price")]
    pub price: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn deserialize_price<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Ok(Some(other.to_string())),
    }
}

fn deserialize_stringified<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(String::new()),
        Some(serde_json::Value::String(s)) => Ok(s),
        Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
        Some(serde_json::Value::Bool(b)) => Ok(b.to_string()),
        Some(other) => Ok(other.to_string()),
    }
}

fn deserialize_option_stringified<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s)),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b.to_string())),
        Some(other) => Ok(Some(other.to_string())),
    }
}

fn deserialize_option_boolish<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
        Some(serde_json::Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Ok(Some(i != 0))
            } else if let Some(u) = n.as_u64() {
                Ok(Some(u != 0))
            } else if let Some(f) = n.as_f64() {
                Ok(Some(f != 0.0))
            } else {
                Ok(None)
            }
        }
        Some(serde_json::Value::String(s)) => {
            let normalized = s.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "y" | "on" => Ok(Some(true)),
                "false" | "0" | "no" | "n" | "off" => Ok(Some(false)),
                _ => Ok(None),
            }
        }
        Some(_) => Ok(None),
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderBookResponse {
    pub market: Option<String>,
    pub asset_id: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderBookLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderResponse {
    pub id: String,
    pub status: String,
    pub owner: Option<String>,
    pub market: Option<String>,
    pub asset_id: Option<String>,
    pub side: Option<String>,
    pub original_size: Option<String>,
    pub size_matched: Option<String>,
    pub price: Option<String>,
    pub associate_trades: Option<Vec<TradeInfo>>,
    pub created_at: Option<String>,
    pub expiration: Option<String>,
    #[serde(rename = "type")]
    pub order_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradeInfo {
    pub id: String,
    pub taker_order_id: String,
    pub market: String,
    pub asset_id: String,
    pub side: String,
    pub size: String,
    pub fee_rate_bps: String,
    pub price: String,
    pub status: String,
    pub match_time: String,
    pub outcome: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateOrderResponse {
    pub success: Option<bool>,
    pub error_msg: Option<String>,
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CancelOrderResponse {
    pub canceled: Option<Vec<String>>,
    pub not_canceled: Option<Vec<NotCanceledOrder>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NotCanceledOrder {
    pub order_id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MarketsSearchResponse {
    pub data: Option<Vec<MarketSummary>>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarketSummary {
    pub condition_id: String,
    pub question: Option<String>,
    pub slug: Option<String>,
    pub active: bool,
    #[serde(default)]
    pub clob_token_ids: Option<String>,
    #[serde(default)]
    pub outcome_prices: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ApiKeyResponse {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

impl std::fmt::Debug for ApiKeyResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyResponse")
            .field(
                "api_key",
                &format_args!("{}...", &self.api_key[..8.min(self.api_key.len())]),
            )
            .field("secret", &"[REDACTED]")
            .field("passphrase", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BalanceResponse {
    pub balance: String,
    #[serde(default)]
    pub allowance: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PositionResponse {
    #[serde(
        default,
        alias = "assetId",
        deserialize_with = "deserialize_stringified"
    )]
    pub asset_id: String,
    #[serde(
        default,
        alias = "token_id",
        alias = "tokenId",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub token_id: Option<String>,
    #[serde(
        default,
        alias = "conditionId",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub condition_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_stringified")]
    pub outcome: Option<String>,
    #[serde(
        default,
        alias = "outcomeIndex",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub outcome_index: Option<String>,
    #[serde(default, deserialize_with = "deserialize_stringified")]
    pub size: String,
    #[serde(
        default,
        alias = "avgPrice",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub avg_price: Option<String>,
    #[serde(
        default,
        alias = "realizedPnl",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub realized_pnl: Option<String>,
    #[serde(
        default,
        alias = "unrealizedPnl",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub unrealized_pnl: Option<String>,
    #[serde(
        default,
        alias = "curPrice",
        deserialize_with = "deserialize_option_stringified"
    )]
    pub cur_price: Option<String>,
    #[serde(
        default,
        alias = "isRedeemable",
        deserialize_with = "deserialize_option_boolish"
    )]
    pub redeemable: Option<bool>,
    #[serde(
        default,
        alias = "negativeRisk",
        deserialize_with = "deserialize_option_boolish"
    )]
    pub negative_risk: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl PositionResponse {
    pub fn value(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        let price = self.avg_price.as_ref()?.parse::<Decimal>().ok()?;
        Some(size * price)
    }

    pub fn market_value(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        let price = self.cur_price.as_ref()?.parse::<Decimal>().ok()?;
        Some(size * price)
    }

    pub fn payout_if_win(&self) -> Option<Decimal> {
        let size = self.size.parse::<Decimal>().ok()?;
        Some(size)
    }

    pub fn is_redeemable(&self) -> bool {
        self.redeemable.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradeResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub asset_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub fee: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountSummary {
    pub usdc_balance: Decimal,
    pub open_order_count: usize,
    pub open_order_value: Decimal,
    pub position_count: usize,
    pub position_value: Decimal,
    pub total_equity: Decimal,
    pub open_orders: Vec<OrderResponse>,
    pub positions: Vec<PositionResponse>,
}

impl AccountSummary {
    pub fn has_sufficient_balance(&self, required: Decimal) -> bool {
        self.usdc_balance >= required
    }

    pub fn available_balance(&self) -> Decimal {
        (self.usdc_balance - self.open_order_value).max(Decimal::ZERO)
    }

    pub fn log_summary(&self) {
        info!(
            "Account Summary: USDC={:.2}, Orders={} (${:.2}), Positions={} (${:.2}), Equity=${:.2}",
            self.usdc_balance,
            self.open_order_count,
            self.open_order_value,
            self.position_count,
            self.position_value,
            self.total_equity
        );
    }
}
