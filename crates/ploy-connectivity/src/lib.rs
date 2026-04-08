use ploy_trading::{FillRecord, TradeSide};
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::clob::types::request::TradesRequest;
use polymarket_client_sdk::clob::types::response::{MakerOrder, TradeResponse};
use polymarket_client_sdk::clob::types::{OrderType, Side, SignatureType};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::types::{Address, U256};
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use secrecy::{ExposeSecret, SecretString};
use std::str::FromStr;
use thiserror::Error;

pub const CRATE_MARKER: &str = "ploy-connectivity";
const DEFAULT_POLY_CLOB_HOST: &str = "https://clob.polymarket.com";

/// Order execution type for live trading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderExecutionType {
    /// Good-Til-Cancelled (rests on book).
    GTC,
    /// Fill-and-Kill (fill what you can, cancel rest). Default for 5-min markets.
    #[default]
    FAK,
    /// Fill-or-Kill (fill entirely or cancel entirely).
    FOK,
}

impl OrderExecutionType {
    pub fn into_sdk(self) -> OrderType {
        match self {
            Self::GTC => OrderType::GTC,
            Self::FAK => OrderType::FAK,
            Self::FOK => OrderType::FOK,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRequest {
    pub order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    /// Order execution type (default: FAK for immediate execution).
    pub order_type: OrderExecutionType,
    /// Extra ticks above best ask for aggressive pricing (0 = use exact limit_price).
    pub aggressive_ticks: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedOrder {
    pub order_id: String,
    pub venue_order_id: String,
    pub token_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationRequest {
    pub order_id: String,
    pub venue_order_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceRequest {
    pub order_id: String,
    pub venue_order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    Acknowledged { venue_order_id: String },
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CancellationOutcome {
    Canceled,
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplaceOutcome {
    Replaced { venue_order_id: String },
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecutionError {
    #[error("live execution misconfigured: {0}")]
    Configuration(String),
    #[error("live execution validation failed: {0}")]
    Validation(String),
    #[error("live execution transport failed: {0}")]
    Transport(String),
}

pub trait LiveExecutionGateway: Send + Sync + std::fmt::Debug {
    fn submit(&self, request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError>;

    fn cancel(&self, request: &CancellationRequest) -> Result<CancellationOutcome, ExecutionError>;

    fn replace(&self, request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError>;

    fn reconcile_fills(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError>;
}

#[derive(Debug, Clone)]
pub struct StaticExecutionGateway {
    result: Result<ExecutionOutcome, ExecutionError>,
    cancel_result: Result<CancellationOutcome, ExecutionError>,
    replace_result: Result<ReplaceOutcome, ExecutionError>,
    reconcile_result: Result<Vec<FillRecord>, ExecutionError>,
}

impl StaticExecutionGateway {
    pub fn acknowledged(venue_order_id: impl Into<String>) -> Self {
        let venue_order_id = venue_order_id.into();
        Self {
            result: Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: venue_order_id.clone(),
            }),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Ok(ReplaceOutcome::Replaced {
                venue_order_id: format!("{venue_order_id}-replaced"),
            }),
            reconcile_result: Ok(Vec::new()),
        }
    }

    pub fn rejected(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            result: Ok(ExecutionOutcome::Rejected {
                reason: reason.clone(),
            }),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Ok(ReplaceOutcome::Rejected { reason }),
            reconcile_result: Ok(Vec::new()),
        }
    }

    pub fn failed(error: ExecutionError) -> Self {
        Self {
            result: Err(error.clone()),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Err(error),
            reconcile_result: Ok(Vec::new()),
        }
    }

    pub fn with_cancel_result(
        mut self,
        result: Result<CancellationOutcome, ExecutionError>,
    ) -> Self {
        self.cancel_result = result;
        self
    }

    pub fn with_replace_result(mut self, result: Result<ReplaceOutcome, ExecutionError>) -> Self {
        self.replace_result = result;
        self
    }

    pub fn with_reconciled_fills(mut self, fills: Vec<FillRecord>) -> Self {
        self.reconcile_result = Ok(fills);
        self
    }

    pub fn with_reconcile_result(
        mut self,
        result: Result<Vec<FillRecord>, ExecutionError>,
    ) -> Self {
        self.reconcile_result = result;
        self
    }
}

impl LiveExecutionGateway for StaticExecutionGateway {
    fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        self.result.clone()
    }

    fn cancel(
        &self,
        _request: &CancellationRequest,
    ) -> Result<CancellationOutcome, ExecutionError> {
        self.cancel_result.clone()
    }

    fn reconcile_fills(
        &self,
        _tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError> {
        self.reconcile_result.clone()
    }

    fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
        self.replace_result.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletSignatureType {
    Eoa,
    Proxy,
    GnosisSafe,
}

impl WalletSignatureType {
    fn into_sdk(self) -> SignatureType {
        match self {
            Self::Eoa => SignatureType::Eoa,
            Self::Proxy => SignatureType::Proxy,
            Self::GnosisSafe => SignatureType::GnosisSafe,
        }
    }
}

impl Default for WalletSignatureType {
    fn default() -> Self {
        Self::Eoa
    }
}

impl FromStr for WalletSignatureType {
    type Err = ExecutionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "eoa" => Ok(Self::Eoa),
            "proxy" => Ok(Self::Proxy),
            "gnosis_safe" => Ok(Self::GnosisSafe),
            other => Err(ExecutionError::Configuration(format!(
                "unsupported POLY_SIGNATURE_TYPE `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PolymarketExecutionConfig {
    pub host: String,
    pub private_key: Option<SecretString>,
    pub use_server_time: bool,
    pub funder: Option<String>,
    pub signature_type: WalletSignatureType,
}

impl Default for PolymarketExecutionConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_POLY_CLOB_HOST.to_string(),
            private_key: std::env::var(PRIVATE_KEY_VAR).ok().map(SecretString::from),
            use_server_time: true,
            funder: std::env::var("POLY_FUNDER").ok(),
            signature_type: std::env::var("POLY_SIGNATURE_TYPE")
                .ok()
                .map(|value| WalletSignatureType::from_str(&value))
                .transpose()
                .unwrap_or(None)
                .unwrap_or_default(),
        }
    }
}

impl PolymarketExecutionConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(value) = std::env::var("POLY_CLOB_HOST") {
            config.host = value;
        }
        if let Ok(value) = std::env::var("POLY_USE_SERVER_TIME") {
            config.use_server_time = matches!(value.as_str(), "1" | "true" | "TRUE" | "yes");
        }

        config
    }
}

#[derive(Debug, Clone)]
pub struct PolymarketExecutionGateway {
    config: PolymarketExecutionConfig,
}

impl PolymarketExecutionGateway {
    pub fn from_env() -> Self {
        Self {
            config: PolymarketExecutionConfig::from_env(),
        }
    }

    pub fn new(config: PolymarketExecutionConfig) -> Self {
        Self { config }
    }
}

impl LiveExecutionGateway for PolymarketExecutionGateway {
    fn submit(&self, request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        let limit_price = request.limit_price.ok_or_else(|| {
            ExecutionError::Validation(
                "live Polymarket execution currently requires a limit price".to_string(),
            )
        })?;
        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ExecutionError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;
        let token_id = U256::from_str(&request.token_id).map_err(|err| {
            ExecutionError::Validation(format!("invalid token_id `{}`: {err}", request.token_id))
        })?;
        let funder = self
            .config
            .funder
            .as_deref()
            .map(Address::from_str)
            .transpose()
            .map_err(|err| ExecutionError::Configuration(format!("invalid POLY_FUNDER: {err}")))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| ExecutionError::Transport(format!("create tokio runtime: {err}")))?;

        runtime.block_on(async {
            let signer = LocalSigner::from_str(private_key)
                .map_err(|err| {
                    ExecutionError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}"))
                })?
                .with_chain_id(Some(POLYGON));

            let client = Client::new(
                &self.config.host,
                Config::builder()
                    .use_server_time(self.config.use_server_time)
                    .build(),
            )
            .map_err(|err| ExecutionError::Transport(format!("build client: {err}")))?;

            let mut auth = client.authentication_builder(&signer);
            auth = auth.signature_type(self.config.signature_type.into_sdk());
            if let Some(funder) = funder {
                auth = auth.funder(funder);
            }

            let client = auth
                .authenticate()
                .await
                .map_err(|err| ExecutionError::Transport(format!("authenticate client: {err}")))?;

            // Compute aggressive price: limit_price + N ticks (capped at 0.99).
            let tick_size = dec!(0.01);
            let aggressive_price = if request.aggressive_ticks > 0 {
                let offset = tick_size * Decimal::from(request.aggressive_ticks);
                (limit_price + offset).min(dec!(0.99))
            } else {
                limit_price
            };

            let order = client
                .limit_order()
                .token_id(token_id)
                .order_type(request.order_type.into_sdk())
                .price(aggressive_price)
                .size(request.quantity)
                .side(polymarket_side(request.side))
                .build()
                .await
                .map_err(|err| ExecutionError::Validation(format!("build limit order: {err}")))?;

            let signed_order = client
                .sign(&signer, order)
                .await
                .map_err(|err| ExecutionError::Transport(format!("sign order: {err}")))?;

            let response = client
                .post_order(signed_order)
                .await
                .map_err(|err| ExecutionError::Transport(format!("submit order: {err}")))?;

            if response.success {
                Ok(ExecutionOutcome::Acknowledged {
                    venue_order_id: response.order_id,
                })
            } else {
                Ok(ExecutionOutcome::Rejected {
                    reason: response.error_msg.unwrap_or_else(|| {
                        format!("venue rejected order with status {}", response.status)
                    }),
                })
            }
        })
    }

    fn reconcile_fills(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError> {
        if tracked_orders.is_empty() {
            return Ok(Vec::new());
        }

        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ExecutionError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;
        let funder = self
            .config
            .funder
            .as_deref()
            .map(Address::from_str)
            .transpose()
            .map_err(|err| ExecutionError::Configuration(format!("invalid POLY_FUNDER: {err}")))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| ExecutionError::Transport(format!("create tokio runtime: {err}")))?;

        runtime.block_on(async {
            let signer = LocalSigner::from_str(private_key)
                .map_err(|err| {
                    ExecutionError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}"))
                })?
                .with_chain_id(Some(POLYGON));

            let client = Client::new(
                &self.config.host,
                Config::builder()
                    .use_server_time(self.config.use_server_time)
                    .build(),
            )
            .map_err(|err| ExecutionError::Transport(format!("build client: {err}")))?;

            let mut auth = client.authentication_builder(&signer);
            auth = auth.signature_type(self.config.signature_type.into_sdk());
            if let Some(funder) = funder {
                auth = auth.funder(funder);
            }

            let client = auth
                .authenticate()
                .await
                .map_err(|err| ExecutionError::Transport(format!("authenticate client: {err}")))?;

            let mut fills = Vec::new();
            for tracked_order in tracked_orders {
                let asset_id = U256::from_str(&tracked_order.token_id).map_err(|err| {
                    ExecutionError::Validation(format!(
                        "invalid token_id `{}`: {err}",
                        tracked_order.token_id
                    ))
                })?;
                let request = TradesRequest::builder().asset_id(asset_id).build();
                let mut next_cursor = None;
                loop {
                    let page = client
                        .trades(&request, next_cursor.clone())
                        .await
                        .map_err(|err| ExecutionError::Transport(format!("load trades: {err}")))?;
                    for trade in &page.data {
                        if let Some(fill) = tracked_trade_fill(tracked_order, trade) {
                            fills.push(fill);
                        }
                    }

                    if page.next_cursor.is_empty() {
                        break;
                    }
                    next_cursor = Some(page.next_cursor);
                }
            }

            Ok(fills)
        })
    }

    fn cancel(&self, request: &CancellationRequest) -> Result<CancellationOutcome, ExecutionError> {
        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ExecutionError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;
        let funder = self
            .config
            .funder
            .as_deref()
            .map(Address::from_str)
            .transpose()
            .map_err(|err| ExecutionError::Configuration(format!("invalid POLY_FUNDER: {err}")))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| ExecutionError::Transport(format!("create tokio runtime: {err}")))?;

        runtime.block_on(async {
            let signer = LocalSigner::from_str(private_key)
                .map_err(|err| {
                    ExecutionError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}"))
                })?
                .with_chain_id(Some(POLYGON));

            let client = Client::new(
                &self.config.host,
                Config::builder()
                    .use_server_time(self.config.use_server_time)
                    .build(),
            )
            .map_err(|err| ExecutionError::Transport(format!("build client: {err}")))?;

            let mut auth = client.authentication_builder(&signer);
            auth = auth.signature_type(self.config.signature_type.into_sdk());
            if let Some(funder) = funder {
                auth = auth.funder(funder);
            }

            let client = auth
                .authenticate()
                .await
                .map_err(|err| ExecutionError::Transport(format!("authenticate client: {err}")))?;

            let response = client
                .cancel_order(&request.venue_order_id)
                .await
                .map_err(|err| ExecutionError::Transport(format!("cancel order: {err}")))?;

            if response
                .canceled
                .iter()
                .any(|order_id| order_id == &request.venue_order_id)
            {
                Ok(CancellationOutcome::Canceled)
            } else if let Some(reason) = response.not_canceled.get(&request.venue_order_id) {
                Ok(CancellationOutcome::Rejected {
                    reason: reason.clone(),
                })
            } else {
                Ok(CancellationOutcome::Rejected {
                    reason: format!(
                        "venue did not confirm cancellation for order `{}`",
                        request.venue_order_id
                    ),
                })
            }
        })
    }

    fn replace(&self, request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
        match self.cancel(&CancellationRequest {
            order_id: request.order_id.clone(),
            venue_order_id: request.venue_order_id.clone(),
        })? {
            CancellationOutcome::Canceled => {}
            CancellationOutcome::Rejected { reason } => {
                return Ok(ReplaceOutcome::Rejected { reason });
            }
        }

        match self.submit(&ExecutionRequest {
            order_id: request.order_id.clone(),
            token_id: request.token_id.clone(),
            side: request.side,
            quantity: request.quantity,
            limit_price: request.limit_price,
            order_type: OrderExecutionType::GTC,
            aggressive_ticks: 0,
        })? {
            ExecutionOutcome::Acknowledged { venue_order_id } => {
                Ok(ReplaceOutcome::Replaced { venue_order_id })
            }
            ExecutionOutcome::Rejected { reason } => Ok(ReplaceOutcome::Rejected { reason }),
        }
    }
}

fn polymarket_side(side: TradeSide) -> Side {
    match side {
        TradeSide::Buy => Side::Buy,
        TradeSide::Sell => Side::Sell,
    }
}

fn tracked_trade_fill(tracked_order: &TrackedOrder, trade: &TradeResponse) -> Option<FillRecord> {
    if trade.taker_order_id == tracked_order.venue_order_id {
        return Some(FillRecord {
            fill_id: tracked_fill_id(tracked_order, &trade.id),
            order_id: tracked_order.order_id.clone(),
            token_id: tracked_order.token_id.clone(),
            side: trade_side(trade.side.clone()),
            quantity: trade.size,
            price: trade.price,
            fee: fee_from_bps(trade.size, trade.price, trade.fee_rate_bps),
            timestamp: trade.match_time,
        });
    }

    trade
        .maker_orders
        .iter()
        .find(|maker_order| maker_order.order_id == tracked_order.venue_order_id)
        .map(|maker_order| tracked_maker_fill(tracked_order, trade, maker_order))
}

fn tracked_maker_fill(
    tracked_order: &TrackedOrder,
    trade: &TradeResponse,
    maker_order: &MakerOrder,
) -> FillRecord {
    FillRecord {
        fill_id: tracked_fill_id(tracked_order, &trade.id),
        order_id: tracked_order.order_id.clone(),
        token_id: tracked_order.token_id.clone(),
        side: trade_side(maker_order.side.clone()),
        quantity: maker_order.matched_amount,
        price: maker_order.price,
        fee: fee_from_bps(
            maker_order.matched_amount,
            maker_order.price,
            maker_order.fee_rate_bps,
        ),
        timestamp: trade.match_time,
    }
}

fn tracked_fill_id(tracked_order: &TrackedOrder, trade_id: &str) -> String {
    format!("{trade_id}:{}", tracked_order.order_id)
}

fn trade_side(side: Side) -> TradeSide {
    match side {
        Side::Buy => TradeSide::Buy,
        Side::Sell => TradeSide::Sell,
        _ => TradeSide::Buy,
    }
}

fn fee_from_bps(quantity: Decimal, price: Decimal, fee_rate_bps: Decimal) -> Decimal {
    quantity * price * fee_rate_bps / Decimal::from(10_000_u64)
}

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}

#[cfg(test)]
mod tests {
    use super::{
        tracked_trade_fill, CancellationOutcome, CancellationRequest, ExecutionError,
        ExecutionOutcome, ExecutionRequest, LiveExecutionGateway, PolymarketExecutionConfig,
        PolymarketExecutionGateway, ReplaceOutcome, ReplaceRequest, StaticExecutionGateway,
        TrackedOrder, WalletSignatureType,
    };
    use chrono::Utc;
    use ploy_trading::{FillRecord, TradeSide};
    use polymarket_client_sdk::auth::ApiKey;
    use polymarket_client_sdk::clob::types::response::{MakerOrder, TradeResponse};
    use polymarket_client_sdk::clob::types::{TradeStatusType, TraderSide};
    use polymarket_client_sdk::types::{Address, B256, U256};
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    #[test]
    fn static_gateway_returns_acknowledged_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1");
        let outcome = gateway
            .submit(&ExecutionRequest {
                order_id: "order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.55)),
                order_type: OrderExecutionType::GTC,
                aggressive_ticks: 0,
            })
            .expect("ack outcome");

        assert_eq!(
            outcome,
            ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-order-1".to_string()
            }
        );
    }

    #[test]
    fn polymarket_gateway_rejects_missing_limit_price_before_network() {
        let gateway = PolymarketExecutionGateway::new(PolymarketExecutionConfig {
            host: "https://clob.polymarket.com".to_string(),
            private_key: None,
            use_server_time: true,
            funder: None,
            signature_type: WalletSignatureType::Eoa,
        });

        let error = gateway
            .submit(&ExecutionRequest {
                order_id: "order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: None,
                order_type: OrderExecutionType::GTC,
                aggressive_ticks: 0,
            })
            .expect_err("limit price should be required");

        assert_eq!(
            error,
            ExecutionError::Validation(
                "live Polymarket execution currently requires a limit price".to_string()
            )
        );
    }

    #[test]
    fn static_gateway_replays_reconciled_fills() {
        let fill = FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.55),
            fee: dec!(0.01),
            timestamp: Utc::now(),
        };
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1")
            .with_reconciled_fills(vec![fill.clone()]);

        let fills = gateway
            .reconcile_fills(&[TrackedOrder {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-order-1".to_string(),
                token_id: "1".to_string(),
            }])
            .expect("fills");

        assert_eq!(fills, vec![fill]);
    }

    #[test]
    fn static_gateway_replays_cancel_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1").with_cancel_result(Ok(
            CancellationOutcome::Rejected {
                reason: "already filled".to_string(),
            },
        ));

        let outcome = gateway
            .cancel(&CancellationRequest {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-order-1".to_string(),
            })
            .expect("cancel outcome");

        assert_eq!(
            outcome,
            CancellationOutcome::Rejected {
                reason: "already filled".to_string(),
            }
        );
    }

    #[test]
    fn static_gateway_replays_replace_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1").with_replace_result(
            Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-order-2".to_string(),
            }),
        );

        let outcome = gateway
            .replace(&ReplaceRequest {
                order_id: "order-1".to_string(),
                venue_order_id: "venue-order-1".to_string(),
                token_id: "1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.57)),
            })
            .expect("replace outcome");

        assert_eq!(
            outcome,
            ReplaceOutcome::Replaced {
                venue_order_id: "venue-order-2".to_string(),
            }
        );
    }

    #[test]
    fn tracked_fill_ids_are_scoped_by_tracked_order() {
        let trade = TradeResponse::builder()
            .id("trade-1")
            .taker_order_id("venue-order-a")
            .market(B256::ZERO)
            .asset_id(U256::from(1_u64))
            .side(polymarket_client_sdk::clob::types::Side::Buy)
            .size(dec!(2))
            .fee_rate_bps(dec!(5))
            .price(dec!(0.55))
            .status(TradeStatusType::Matched)
            .match_time(Utc::now())
            .last_update(Utc::now())
            .outcome("YES")
            .bucket_index(0)
            .owner(ApiKey::nil())
            .maker_address(
                Address::from_str("0x0000000000000000000000000000000000000001")
                    .expect("maker address"),
            )
            .maker_orders(vec![MakerOrder::builder()
                .order_id("venue-order-b")
                .owner(ApiKey::nil())
                .maker_address(
                    Address::from_str("0x0000000000000000000000000000000000000002")
                        .expect("second maker address"),
                )
                .matched_amount(dec!(2))
                .price(dec!(0.56))
                .fee_rate_bps(dec!(4))
                .asset_id(U256::from(1_u64))
                .outcome("YES")
                .side(polymarket_client_sdk::clob::types::Side::Sell)
                .build()])
            .transaction_hash(B256::ZERO)
            .trader_side(TraderSide::Taker)
            .build();

        let taker_fill = tracked_trade_fill(
            &TrackedOrder {
                order_id: "order-a".to_string(),
                venue_order_id: "venue-order-a".to_string(),
                token_id: "token-a".to_string(),
            },
            &trade,
        )
        .expect("taker fill");
        let maker_fill = tracked_trade_fill(
            &TrackedOrder {
                order_id: "order-b".to_string(),
                venue_order_id: "venue-order-b".to_string(),
                token_id: "token-b".to_string(),
            },
            &trade,
        )
        .expect("maker fill");

        assert_eq!(taker_fill.fill_id, "trade-1:order-a");
        assert_eq!(maker_fill.fill_id, "trade-1:order-b");
        assert_ne!(taker_fill.fill_id, maker_fill.fill_id);
    }
}
