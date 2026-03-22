use alloy::core::sol;
use alloy::dyn_abi::Eip712Domain;
use alloy::primitives::{keccak256, Address as AlloyAddress, B256, Bytes, U256 as AlloyU256};
use alloy::sol_types::{SolCall, SolStruct as _};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use hmac::{Hmac, Mac as _};
use ploy_trading::{FillRecord, TradeSide};
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::clob::types::request::TradesRequest;
use polymarket_client_sdk::clob::types::response::{MakerOrder, TradeResponse};
use polymarket_client_sdk::clob::types::{OrderType, Side, SignatureType};
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::types::response::Position as DataPosition;
use polymarket_client_sdk::data::Client as DataClient;
use polymarket_client_sdk::derive_proxy_wallet;
use polymarket_client_sdk::derive_safe_wallet;
use polymarket_client_sdk::types::{Address, U256};
use polymarket_client_sdk::{POLYGON, PRIVATE_KEY_VAR, contract_config, wallet_contract_config};
use reqwest::blocking::Client as BlockingHttpClient;
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const CRATE_MARKER: &str = "ploy-connectivity";
const DEFAULT_POLY_CLOB_HOST: &str = "https://clob.polymarket.com";
const DEFAULT_POLY_DATA_HOST: &str = "https://data-api.polymarket.com";
const DEFAULT_POLYGON_RPC_URL: &str = "https://polygon.drpc.org";
const DEFAULT_POLY_RELAYER_URL: &str = "https://relayer-v2.polymarket.com";
const DEFAULT_RELAY_POLL_FREQUENCY_MS: u64 = 2_000;
const DEFAULT_RELAY_MAX_POLLS: usize = 30;
const DEFAULT_PROXY_GAS_LIMIT: u64 = 10_000_000;
const POLY_RELAYER_URL_VAR: &str = "POLYMARKET_RELAYER_URL";
const POLY_RELAYER_URL_ALIAS: &str = "POLY_RELAYER_URL";
const POLY_BUILDER_API_KEY_VAR: &str = "POLY_BUILDER_API_KEY";
const POLY_BUILDER_API_KEY_ALIAS: &str = "BUILDER_API_KEY";
const POLY_BUILDER_SECRET_VAR: &str = "POLY_BUILDER_SECRET";
const POLY_BUILDER_SECRET_ALIAS: &str = "BUILDER_SECRET";
const POLY_BUILDER_PASS_PHRASE_VAR: &str = "POLY_BUILDER_PASS_PHRASE";
const POLY_BUILDER_PASSPHRASE_VAR: &str = "POLY_BUILDER_PASSPHRASE";
const POLY_BUILDER_PASS_PHRASE_ALIAS: &str = "BUILDER_PASS_PHRASE";
const POLY_RELAY_HUB: &str = "0xD216153c06E857cD7f72665E0aF1d7D82172F494";

sol! {
    #[derive(Debug)]
    struct RelayProxyTransaction {
        uint8 typeCode;
        address to;
        uint256 value;
        bytes data;
    }

    #[derive(Debug)]
    struct SafeTx {
        address to;
        uint256 value;
        bytes data;
        uint8 operation;
        uint256 safeTxGas;
        uint256 baseGas;
        uint256 gasPrice;
        address gasToken;
        address refundReceiver;
        uint256 nonce;
    }

    #[derive(Debug)]
    struct CreateProxy {
        address paymentToken;
        uint256 payment;
        address paymentReceiver;
    }

    interface IConditionalTokensRelay {
        function redeemPositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;
    }

    interface INegRiskAdapterRelay {
        function redeemPositions(
            bytes32 conditionId,
            uint256[] calldata amounts
        ) external;
    }

    interface IProxyWalletFactoryRelay {
        function proxy(RelayProxyTransaction[] calldata txns) external;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRequest {
    pub order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
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
pub struct RedeemablePosition {
    pub account_id: String,
    pub wallet_address: String,
    pub condition_id: String,
    pub market_id: Option<String>,
    pub token_ids: Vec<String>,
    pub outcome_labels: Vec<String>,
    pub outcome_indexes: Vec<u8>,
    pub outcome_amounts: Vec<Decimal>,
    pub redeemable_size: Decimal,
    pub estimated_payout: Decimal,
    pub negative_risk: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimRequest {
    pub account_id: String,
    pub wallet_address: String,
    pub condition_id: String,
    pub outcome_indexes: Vec<u8>,
    pub outcome_amounts: Vec<Decimal>,
    pub negative_risk: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimResult {
    pub tx_hash: String,
    pub block_number: u64,
    pub amount_claimed: Decimal,
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
pub enum ClaimError {
    #[error("claim misconfigured: {0}")]
    Configuration(String),
    #[error("claim validation failed: {0}")]
    Validation(String),
    #[error("claim transport failed: {0}")]
    Transport(String),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimBackend {
    RelayProxy,
    RelaySafe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimRelayTxType {
    Proxy,
    Safe,
}

#[derive(Debug, Clone)]
struct BuilderApiCredentials {
    api_key: SecretString,
    secret: SecretString,
    passphrase: SecretString,
}

#[derive(Debug, Clone, Default)]
struct BuilderAuthConfig {
    local: Option<BuilderApiCredentials>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayerSignatureParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    gas_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relayer_fee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gas_limit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relay_hub: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relay: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    safe_txn_gas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_gas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gas_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refund_receiver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payment_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    payment_receiver: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RelayerTransactionRequest {
    #[serde(rename = "type")]
    tx_type: String,
    from: String,
    to: String,
    #[serde(rename = "proxyWallet", skip_serializing_if = "Option::is_none")]
    proxy_wallet: Option<String>,
    data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    signature: String,
    #[serde(rename = "signatureParams")]
    signature_params: RelayerSignatureParams,
    metadata: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RelayerNoncePayload {
    nonce: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RelayerRelayPayload {
    address: String,
    nonce: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RelayerSubmitResponse {
    #[serde(rename = "transactionID")]
    transaction_id: String,
    state: String,
    #[serde(rename = "transactionHash")]
    transaction_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RelayerTransaction {
    state: String,
    #[serde(rename = "transactionHash")]
    transaction_hash: String,
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

pub trait ClaimGateway: Send + Sync + std::fmt::Debug {
    fn discover_redeemable_positions(
        &self,
        account_id: &str,
    ) -> Result<Vec<RedeemablePosition>, ClaimError>;

    fn claim(&self, request: &ClaimRequest) -> Result<ClaimResult, ClaimError>;
}

#[derive(Debug, Clone)]
pub struct StaticExecutionGateway {
    result: Result<ExecutionOutcome, ExecutionError>,
    cancel_result: Result<CancellationOutcome, ExecutionError>,
    replace_result: Result<ReplaceOutcome, ExecutionError>,
    reconcile_result: Result<Vec<FillRecord>, ExecutionError>,
}

#[derive(Debug, Clone)]
pub struct StaticClaimGateway {
    positions: Result<Vec<RedeemablePosition>, ClaimError>,
    claim_result: Result<ClaimResult, ClaimError>,
}

impl Default for StaticClaimGateway {
    fn default() -> Self {
        Self {
            positions: Ok(Vec::new()),
            claim_result: Err(ClaimError::Transport(
                "static claim gateway result not configured".to_string(),
            )),
        }
    }
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

impl StaticClaimGateway {
    pub fn with_positions(mut self, positions: Vec<RedeemablePosition>) -> Self {
        self.positions = Ok(positions);
        self
    }

    pub fn with_positions_result(
        mut self,
        result: Result<Vec<RedeemablePosition>, ClaimError>,
    ) -> Self {
        self.positions = result;
        self
    }

    pub fn with_claim_result(mut self, result: Result<ClaimResult, ClaimError>) -> Self {
        self.claim_result = result;
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

impl ClaimGateway for StaticClaimGateway {
    fn discover_redeemable_positions(
        &self,
        _account_id: &str,
    ) -> Result<Vec<RedeemablePosition>, ClaimError> {
        self.positions.clone()
    }

    fn claim(&self, _request: &ClaimRequest) -> Result<ClaimResult, ClaimError> {
        self.claim_result.clone()
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
            "gnosis_safe" | "gnosis-safe" => Ok(Self::GnosisSafe),
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
pub struct PolymarketClaimConfig {
    pub data_host: String,
    pub rpc_url: String,
    pub relayer_url: String,
    pub private_key: Option<SecretString>,
    pub signature_type: WalletSignatureType,
    builder_auth: BuilderAuthConfig,
}

impl Default for PolymarketClaimConfig {
    fn default() -> Self {
        Self {
            data_host: DEFAULT_POLY_DATA_HOST.to_string(),
            rpc_url: std::env::var("POLYGON_RPC_URL")
                .or_else(|_| std::env::var("POLYMARKET_RPC_URL"))
                .unwrap_or_else(|_| DEFAULT_POLYGON_RPC_URL.to_string()),
            relayer_url: read_env(POLY_RELAYER_URL_VAR)
                .or_else(|| read_env(POLY_RELAYER_URL_ALIAS))
                .unwrap_or_else(|| DEFAULT_POLY_RELAYER_URL.to_string()),
            private_key: std::env::var(PRIVATE_KEY_VAR).ok().map(SecretString::from),
            signature_type: std::env::var("POLY_SIGNATURE_TYPE")
                .ok()
                .or_else(|| std::env::var("POLYMARKET_SIGNATURE_TYPE").ok())
                .map(|value| WalletSignatureType::from_str(&value))
                .transpose()
                .unwrap_or(None)
                .unwrap_or_default(),
            builder_auth: BuilderAuthConfig::from_env(),
        }
    }
}

impl PolymarketClaimConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(value) = std::env::var("POLY_DATA_HOST") {
            config.data_host = value;
        }
        if let Ok(value) = std::env::var("POLYMARKET_DATA_HOST") {
            config.data_host = value;
        }
        if let Ok(value) = std::env::var("POLYMARKET_PRIVATE_KEY") {
            if !value.trim().is_empty() {
                config.private_key = Some(SecretString::from(value));
            }
        }
        config
    }

    fn claim_backend(&self) -> Result<ClaimBackend, ClaimError> {
        match claim_relay_tx_type(self.signature_type)? {
            ClaimRelayTxType::Proxy => Ok(ClaimBackend::RelayProxy),
            ClaimRelayTxType::Safe => Ok(ClaimBackend::RelaySafe),
        }
    }
}

impl BuilderAuthConfig {
    fn from_env() -> Self {
        let api_key = read_env(POLY_BUILDER_API_KEY_VAR).or_else(|| read_env(POLY_BUILDER_API_KEY_ALIAS));
        let secret = read_env(POLY_BUILDER_SECRET_VAR).or_else(|| read_env(POLY_BUILDER_SECRET_ALIAS));
        let passphrase = read_env(POLY_BUILDER_PASS_PHRASE_VAR)
            .or_else(|| read_env(POLY_BUILDER_PASSPHRASE_VAR))
            .or_else(|| read_env(POLY_BUILDER_PASS_PHRASE_ALIAS));

        let local = match (api_key, secret, passphrase) {
            (Some(api_key), Some(secret), Some(passphrase)) => Some(BuilderApiCredentials {
                api_key: SecretString::from(api_key),
                secret: SecretString::from(secret),
                passphrase: SecretString::from(passphrase),
            }),
            _ => None,
        };

        Self { local }
    }

    fn headers(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<Vec<(String, String)>, ClaimError> {
        let Some(local) = &self.local else {
            return Err(ClaimError::Configuration(
                "relay-first auto-claim requires builder credentials".to_string(),
            ));
        };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| ClaimError::Transport(format!("compute builder timestamp: {err}")))?
            .as_secs() as i64;
        let signature = build_builder_signature(
            local.secret.expose_secret(),
            timestamp,
            method,
            path,
            body,
        )?;

        Ok(vec![
            (
                "POLY_BUILDER_API_KEY".to_string(),
                local.api_key.expose_secret().to_string(),
            ),
            (
                "POLY_BUILDER_PASSPHRASE".to_string(),
                local.passphrase.expose_secret().to_string(),
            ),
            ("POLY_BUILDER_SIGNATURE".to_string(), signature),
            (
                "POLY_BUILDER_TIMESTAMP".to_string(),
                timestamp.to_string(),
            ),
        ])
    }
}

fn claim_relay_tx_type(signature_type: WalletSignatureType) -> Result<ClaimRelayTxType, ClaimError> {
    match signature_type {
        WalletSignatureType::Proxy => Ok(ClaimRelayTxType::Proxy),
        WalletSignatureType::GnosisSafe => Ok(ClaimRelayTxType::Safe),
        WalletSignatureType::Eoa => Err(ClaimError::Configuration(
            "relay-first auto-claim requires POLY_SIGNATURE_TYPE=proxy or gnosis_safe".to_string(),
        )),
    }
}

fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone)]
pub struct PolymarketClaimGateway {
    config: PolymarketClaimConfig,
}

impl PolymarketClaimGateway {
    pub fn from_env() -> Self {
        Self {
            config: PolymarketClaimConfig::from_env(),
        }
    }

    pub fn new(config: PolymarketClaimConfig) -> Self {
        Self { config }
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

impl PolymarketClaimGateway {
    fn claim_via_relayer(&self, request: &ClaimRequest, backend: ClaimBackend) -> Result<ClaimResult, ClaimError> {
        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ClaimError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;
        let relayer_url = self.config.relayer_url.as_str();
        let signer = LocalSigner::from_str(private_key)
            .map_err(|err| ClaimError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}")))?
            .with_chain_id(Some(POLYGON));
        let owner = signer.address();
        let (target, calldata) = build_claim_transaction(request)?;
        let client = BlockingHttpClient::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|err| ClaimError::Transport(format!("build relayer client: {err}")))?;

        let submit_request = match backend {
            ClaimBackend::RelayProxy => {
                let relay_payload = fetch_relay_payload(&client, relayer_url, owner)?;
                let proxy_factory = wallet_contract_config(POLYGON)
                    .and_then(|config| config.proxy_factory)
                    .ok_or_else(|| {
                        ClaimError::Configuration(
                            "proxy wallet factory is not configured for this chain".to_string(),
                        )
                    })?;
                let wrapped = build_proxy_request(
                    &signer,
                    owner,
                    proxy_factory,
                    target,
                    calldata,
                    &relay_payload,
                    Some(self.config.rpc_url.as_str()),
                    None,
                )?;
                wrapped
            }
            ClaimBackend::RelaySafe => {
                ensure_safe_wallet_deployed(
                    &client,
                    relayer_url,
                    &self.config.builder_auth,
                    &signer,
                    owner,
                )?;
                let nonce = fetch_safe_nonce(&client, relayer_url, owner)?;
                build_safe_request(&signer, owner, target, calldata, &nonce)?
            }
        };

        let submit_body = serde_json::to_string(&submit_request)
            .map_err(|err| ClaimError::Transport(format!("serialize relayer request: {err}")))?;
        let submit_response = submit_relayer_transaction(
            &client,
            relayer_url,
            &self.config.builder_auth,
            &submit_body,
        )?;
        let mined = wait_for_relayer_transaction(
            &client,
            relayer_url,
            &submit_response.transaction_id,
        )?;
        let block_number = fetch_receipt_block_number(&self.config.rpc_url, &mined.transaction_hash)?;

        Ok(ClaimResult {
            tx_hash: mined.transaction_hash,
            block_number,
            amount_claimed: request
                .outcome_amounts
                .iter()
                .copied()
                .fold(Decimal::ZERO, |acc, value| acc + value),
        })
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

            let order = client
                .limit_order()
                .token_id(token_id)
                .order_type(OrderType::GTC)
                .price(limit_price)
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
        })? {
            ExecutionOutcome::Acknowledged { venue_order_id } => {
                Ok(ReplaceOutcome::Replaced { venue_order_id })
            }
            ExecutionOutcome::Rejected { reason } => Ok(ReplaceOutcome::Rejected { reason }),
        }
    }
}

impl ClaimGateway for PolymarketClaimGateway {
    fn discover_redeemable_positions(
        &self,
        account_id: &str,
    ) -> Result<Vec<RedeemablePosition>, ClaimError> {
        let private_key = self
            .config
            .private_key
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .ok_or_else(|| {
                ClaimError::Configuration(format!("{PRIVATE_KEY_VAR} is not configured"))
            })?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| ClaimError::Transport(format!("create tokio runtime: {err}")))?;

        runtime.block_on(async {
            let signer = LocalSigner::from_str(private_key)
                .map_err(|err| {
                    ClaimError::Configuration(format!("invalid {PRIVATE_KEY_VAR}: {err}"))
                })?
                .with_chain_id(Some(POLYGON));
            let wallet_address = claim_wallet_address(&signer, self.config.signature_type)?;
            let data_client = DataClient::new(&self.config.data_host)
                .map_err(|err| ClaimError::Transport(format!("build data client: {err}")))?;
            let request = PositionsRequest::builder()
                .user(wallet_address)
                .redeemable(true)
                .build();
            let positions = data_client.positions(&request).await.map_err(|err| {
                ClaimError::Transport(format!("load redeemable positions: {err}"))
            })?;

            Ok(collapse_redeemable_positions(
                account_id,
                &wallet_address.to_string(),
                positions,
            ))
        })
    }

    fn claim(&self, request: &ClaimRequest) -> Result<ClaimResult, ClaimError> {
        self.claim_via_relayer(request, self.config.claim_backend()?)
    }
}

fn polymarket_side(side: TradeSide) -> Side {
    match side {
        TradeSide::Buy => Side::Buy,
        TradeSide::Sell => Side::Sell,
    }
}

fn claim_wallet_address<S: Signer>(
    signer: &S,
    signature_type: WalletSignatureType,
) -> Result<Address, ClaimError> {
    match signature_type {
        WalletSignatureType::Eoa => Ok(signer.address()),
        WalletSignatureType::Proxy => {
            derive_proxy_wallet(signer.address(), POLYGON).ok_or_else(|| {
                ClaimError::Configuration("could not derive proxy wallet address".to_string())
            })
        }
        WalletSignatureType::GnosisSafe => derive_safe_wallet(signer.address(), POLYGON)
            .ok_or_else(|| {
                ClaimError::Configuration("could not derive gnosis safe address".to_string())
            }),
    }
}

fn build_builder_signature(
    secret: &str,
    timestamp: i64,
    method: &str,
    path: &str,
    body: &str,
) -> Result<String, ClaimError> {
    let secret = BASE64_STANDARD
        .decode(secret)
        .map_err(|err| ClaimError::Configuration(format!("decode builder secret: {err}")))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&secret)
        .map_err(|err| ClaimError::Configuration(format!("build builder hmac: {err}")))?;
    mac.update(format!("{timestamp}{method}{path}{body}").as_bytes());
    let signature = BASE64_STANDARD.encode(mac.finalize().into_bytes());
    Ok(signature.replace('+', "-").replace('/', "_"))
}

fn build_claim_transaction(
    request: &ClaimRequest,
) -> Result<(AlloyAddress, Vec<u8>), ClaimError> {
    let config = contract_config(POLYGON, request.negative_risk).ok_or_else(|| {
        ClaimError::Configuration(format!(
            "missing Polymarket contract config for chain {POLYGON}"
        ))
    })?;
    let condition_id = B256::from_str(&request.condition_id).map_err(|err| {
        ClaimError::Validation(format!(
            "invalid condition_id `{}`: {err}",
            request.condition_id
        ))
    })?;

    if request.negative_risk {
        let adapter = config.neg_risk_adapter.ok_or_else(|| {
            ClaimError::Configuration("neg-risk adapter is not configured".to_string())
        })?;
        let calldata = INegRiskAdapterRelay::redeemPositionsCall {
            conditionId: condition_id,
            amounts: neg_risk_amounts(&request.outcome_indexes, &request.outcome_amounts)?,
        }
        .abi_encode();
        Ok((adapter, calldata))
    } else {
        let calldata = IConditionalTokensRelay::redeemPositionsCall {
            collateralToken: config.collateral,
            parentCollectionId: B256::ZERO,
            conditionId: condition_id,
            indexSets: redeem_index_sets(&request.outcome_indexes),
        }
        .abi_encode();
        Ok((config.conditional_tokens, calldata))
    }
}

fn fetch_relay_payload(
    client: &BlockingHttpClient,
    relayer_url: &str,
    owner: AlloyAddress,
) -> Result<RelayerRelayPayload, ClaimError> {
    client
        .get(format!("{relayer_url}/relay-payload"))
        .query(&[
            ("address", owner.to_string()),
            ("type", "PROXY".to_string()),
        ])
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|err| ClaimError::Transport(format!("load relay payload: {err}")))?
        .json()
        .map_err(|err| ClaimError::Transport(format!("parse relay payload: {err}")))
}

fn fetch_safe_nonce(
    client: &BlockingHttpClient,
    relayer_url: &str,
    owner: AlloyAddress,
) -> Result<RelayerNoncePayload, ClaimError> {
    client
        .get(format!("{relayer_url}/nonce"))
        .query(&[
            ("address", owner.to_string()),
            ("type", "SAFE".to_string()),
        ])
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|err| ClaimError::Transport(format!("load safe nonce: {err}")))?
        .json()
        .map_err(|err| ClaimError::Transport(format!("parse safe nonce: {err}")))
}

fn ensure_safe_wallet_deployed(
    client: &BlockingHttpClient,
    relayer_url: &str,
    builder_auth: &BuilderAuthConfig,
    signer: &(impl Signer + Sync),
    owner: AlloyAddress,
) -> Result<(), ClaimError> {
    #[derive(Deserialize)]
    struct DeployedPayload {
        deployed: bool,
    }

    let safe_address = derive_safe_wallet(owner, POLYGON).ok_or_else(|| {
        ClaimError::Configuration("could not derive gnosis safe address".to_string())
    })?;

    let payload: DeployedPayload = client
        .get(format!("{relayer_url}/deployed"))
        .query(&[("address", safe_address.to_string())])
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|err| ClaimError::Transport(format!("load safe deployment status: {err}")))?
        .json()
        .map_err(|err| ClaimError::Transport(format!("parse safe deployment status: {err}")))?;

    if payload.deployed {
        return Ok(());
    }

    let safe_factory = wallet_contract_config(POLYGON)
        .map(|config| config.safe_factory)
        .ok_or_else(|| {
            ClaimError::Configuration("safe wallet factory is not configured for this chain".to_string())
        })?;
    let request = build_safe_create_request(signer, owner, safe_factory)?;
    let body = serde_json::to_string(&request)
        .map_err(|err| ClaimError::Transport(format!("serialize safe deploy request: {err}")))?;
    let submit = submit_relayer_transaction(client, relayer_url, builder_auth, &body)?;
    wait_for_relayer_transaction(client, relayer_url, &submit.transaction_id)?;
    Ok(())
}

fn estimate_proxy_gas_limit(
    rpc_url: Option<&str>,
    from: AlloyAddress,
    to: AlloyAddress,
    data: &[u8],
) -> Option<u64> {
    #[derive(Serialize)]
    struct JsonRpcRequest<'a> {
        jsonrpc: &'static str,
        method: &'static str,
        params: [EstimateGasParams<'a>; 1],
        id: u8,
    }

    #[derive(Serialize)]
    struct EstimateGasParams<'a> {
        from: &'a str,
        to: &'a str,
        data: &'a str,
    }

    #[derive(Deserialize)]
    struct JsonRpcResponse {
        result: Option<String>,
    }

    let rpc_url = rpc_url?;
    let client = BlockingHttpClient::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let from = from.to_string();
    let to = to.to_string();
    let data = format!("0x{}", alloy::hex::encode(data));
    let payload = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "eth_estimateGas",
        params: [EstimateGasParams {
            from: &from,
            to: &to,
            data: &data,
        }],
        id: 1,
    };
    let response: JsonRpcResponse = client.post(rpc_url).json(&payload).send().ok()?.json().ok()?;
    let result = response.result?;
    u64::from_str_radix(result.trim_start_matches("0x"), 16).ok()
}

fn build_proxy_request<S: Signer + Sync>(
    signer: &S,
    owner: AlloyAddress,
    proxy_factory: AlloyAddress,
    target: AlloyAddress,
    calldata: Vec<u8>,
    relay_payload: &RelayerRelayPayload,
    rpc_url: Option<&str>,
    gas_limit_override: Option<u64>,
) -> Result<RelayerTransactionRequest, ClaimError> {
    let relay_address = AlloyAddress::from_str(&relay_payload.address).map_err(|err| {
        ClaimError::Transport(format!("invalid relayer relay address `{}`: {err}", relay_payload.address))
    })?;
    let relay_hub = AlloyAddress::from_str(POLY_RELAY_HUB)
        .map_err(|err| ClaimError::Configuration(format!("invalid relay hub: {err}")))?;
    let proxy_wallet = derive_proxy_wallet(owner, POLYGON).ok_or_else(|| {
        ClaimError::Configuration("could not derive proxy wallet address".to_string())
    })?;
    let proxy_call_data = IProxyWalletFactoryRelay::proxyCall {
        txns: vec![RelayProxyTransaction {
            to: target,
            typeCode: 1,
            data: Bytes::from(calldata),
            value: AlloyU256::ZERO,
        }],
    }
    .abi_encode();
    let gas_limit = gas_limit_override
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            estimate_proxy_gas_limit(rpc_url, owner, proxy_factory, &proxy_call_data)
                .unwrap_or(DEFAULT_PROXY_GAS_LIMIT)
                .to_string()
        });
    let nonce = AlloyU256::from_str(&relay_payload.nonce)
        .map_err(|err| ClaimError::Transport(format!("invalid relay nonce `{}`: {err}", relay_payload.nonce)))?;
    let relay_struct_hash = keccak256([
        b"rlx:".as_slice(),
        owner.as_slice(),
        proxy_factory.as_slice(),
        proxy_call_data.as_slice(),
        AlloyU256::ZERO.to_be_bytes::<32>().as_slice(),
        AlloyU256::ZERO.to_be_bytes::<32>().as_slice(),
        AlloyU256::from_str(&gas_limit)
            .map_err(|err| ClaimError::Transport(format!("invalid proxy gas limit `{gas_limit}`: {err}")))?
            .to_be_bytes::<32>()
            .as_slice(),
        nonce.to_be_bytes::<32>().as_slice(),
        relay_hub.as_slice(),
        relay_address.as_slice(),
    ]
    .concat());
    let signature = sign_personal_message(signer, relay_struct_hash.as_slice())?;

    Ok(RelayerTransactionRequest {
        tx_type: "PROXY".to_string(),
        from: owner.to_string(),
        to: proxy_factory.to_string(),
        proxy_wallet: Some(proxy_wallet.to_string()),
        data: format!("0x{}", alloy::hex::encode(proxy_call_data)),
        nonce: Some(relay_payload.nonce.clone()),
        signature,
        signature_params: RelayerSignatureParams {
            gas_price: Some("0".to_string()),
            relayer_fee: Some("0".to_string()),
            gas_limit: Some(gas_limit),
            relay_hub: Some(POLY_RELAY_HUB.to_string()),
            relay: Some(relay_payload.address.clone()),
            operation: None,
            safe_txn_gas: None,
            base_gas: None,
            gas_token: None,
            refund_receiver: None,
            payment_token: None,
            payment: None,
            payment_receiver: None,
        },
        metadata: "auto-redeem".to_string(),
    })
}

fn build_safe_request<S: Signer + Sync>(
    signer: &S,
    owner: AlloyAddress,
    target: AlloyAddress,
    calldata: Vec<u8>,
    nonce: &RelayerNoncePayload,
) -> Result<RelayerTransactionRequest, ClaimError> {
    let _safe_factory = wallet_contract_config(POLYGON)
        .map(|config| config.safe_factory)
        .ok_or_else(|| {
            ClaimError::Configuration("safe wallet factory is not configured for this chain".to_string())
        })?;
    let safe_address = derive_safe_wallet(owner, POLYGON).ok_or_else(|| {
        ClaimError::Configuration("could not derive gnosis safe address".to_string())
    })?;
    let nonce = AlloyU256::from_str(&nonce.nonce)
        .map_err(|err| ClaimError::Transport(format!("invalid safe nonce `{}`: {err}", nonce.nonce)))?;
    let domain = Eip712Domain {
        chain_id: Some(AlloyU256::from(POLYGON)),
        verifying_contract: Some(safe_address),
        ..Eip712Domain::default()
    };
    let safe_tx = SafeTx {
        to: target,
        value: AlloyU256::ZERO,
        data: Bytes::from(calldata.clone()),
        operation: 0,
        safeTxGas: AlloyU256::ZERO,
        baseGas: AlloyU256::ZERO,
        gasPrice: AlloyU256::ZERO,
        gasToken: AlloyAddress::ZERO,
        refundReceiver: AlloyAddress::ZERO,
        nonce,
    };
    let signature =
        sign_personal_message(signer, safe_tx.eip712_signing_hash(&domain).as_slice())?;
    let packed_signature = pack_safe_signature(&signature)?;

    Ok(RelayerTransactionRequest {
        tx_type: "SAFE".to_string(),
        from: owner.to_string(),
        to: target.to_string(),
        proxy_wallet: Some(safe_address.to_string()),
        data: format!("0x{}", alloy::hex::encode(calldata)),
        nonce: Some(nonce.to_string()),
        signature: packed_signature,
        signature_params: RelayerSignatureParams {
            gas_price: Some("0".to_string()),
            relayer_fee: None,
            gas_limit: None,
            relay_hub: None,
            relay: None,
            operation: Some("0".to_string()),
            safe_txn_gas: Some("0".to_string()),
            base_gas: Some("0".to_string()),
            gas_token: Some(AlloyAddress::ZERO.to_string()),
            refund_receiver: Some(AlloyAddress::ZERO.to_string()),
            payment_token: None,
            payment: None,
            payment_receiver: None,
        },
        metadata: "auto-redeem".to_string(),
    })
}

fn build_safe_create_request<S: Signer + Sync>(
    signer: &S,
    owner: AlloyAddress,
    safe_factory: AlloyAddress,
) -> Result<RelayerTransactionRequest, ClaimError> {
    let domain = Eip712Domain {
        name: Some(std::borrow::Cow::Borrowed("Polymarket Contract Proxy Factory")),
        chain_id: Some(AlloyU256::from(POLYGON)),
        verifying_contract: Some(safe_factory),
        ..Eip712Domain::default()
    };
    let create_proxy = CreateProxy {
        paymentToken: AlloyAddress::ZERO,
        payment: AlloyU256::ZERO,
        paymentReceiver: AlloyAddress::ZERO,
    };
    let signature = sign_raw_hash(signer, create_proxy.eip712_signing_hash(&domain))?;
    let safe_address = derive_safe_wallet(owner, POLYGON).ok_or_else(|| {
        ClaimError::Configuration("could not derive gnosis safe address".to_string())
    })?;

    Ok(RelayerTransactionRequest {
        tx_type: "SAFE-CREATE".to_string(),
        from: owner.to_string(),
        to: safe_factory.to_string(),
        proxy_wallet: Some(safe_address.to_string()),
        data: "0x".to_string(),
        nonce: None,
        signature,
        signature_params: RelayerSignatureParams {
            gas_price: None,
            relayer_fee: None,
            gas_limit: None,
            relay_hub: None,
            relay: None,
            operation: None,
            safe_txn_gas: None,
            base_gas: None,
            gas_token: None,
            refund_receiver: None,
            payment_token: Some(AlloyAddress::ZERO.to_string()),
            payment: Some("0".to_string()),
            payment_receiver: Some(AlloyAddress::ZERO.to_string()),
        },
        metadata: "auto-redeem-safe-deploy".to_string(),
    })
}

fn sign_personal_message<S: Signer + Sync>(signer: &S, message: &[u8]) -> Result<String, ClaimError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| ClaimError::Transport(format!("create tokio runtime: {err}")))?;
    runtime
        .block_on(async { signer.sign_message(message).await })
        .map(|sig| sig.to_string())
        .map_err(|err| ClaimError::Transport(format!("sign relayer payload: {err}")))
}

fn sign_raw_hash<S: Signer + Sync>(signer: &S, hash: B256) -> Result<String, ClaimError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| ClaimError::Transport(format!("create tokio runtime: {err}")))?;
    runtime
        .block_on(async { signer.sign_hash(&hash).await })
        .map(|sig| sig.to_string())
        .map_err(|err| ClaimError::Transport(format!("sign relayer payload: {err}")))
}

fn pack_safe_signature(signature: &str) -> Result<String, ClaimError> {
    let sig = signature
        .strip_prefix("0x")
        .ok_or_else(|| ClaimError::Transport("safe signature missing 0x prefix".to_string()))?;
    if sig.len() != 130 {
        return Err(ClaimError::Transport(format!(
            "safe signature has unexpected length {}",
            sig.len()
        )));
    }

    let mut v = u8::from_str_radix(&sig[128..130], 16)
        .map_err(|err| ClaimError::Transport(format!("parse safe signature v: {err}")))?;
    v = match v {
        0 | 1 => v + 31,
        27 | 28 => v + 4,
        other => {
            return Err(ClaimError::Transport(format!(
                "invalid safe signature v {other}"
            )))
        }
    };

    let r = AlloyU256::from_str_radix(&sig[0..64], 16)
        .map_err(|err| ClaimError::Transport(format!("parse safe signature r: {err}")))?;
    let s = AlloyU256::from_str_radix(&sig[64..128], 16)
        .map_err(|err| ClaimError::Transport(format!("parse safe signature s: {err}")))?;

    let mut out = Vec::with_capacity(65);
    out.extend_from_slice(&r.to_be_bytes::<32>());
    out.extend_from_slice(&s.to_be_bytes::<32>());
    out.push(v);
    Ok(format!("0x{}", alloy::hex::encode(out)))
}

fn submit_relayer_transaction(
    client: &BlockingHttpClient,
    relayer_url: &str,
    builder_auth: &BuilderAuthConfig,
    body: &str,
) -> Result<RelayerSubmitResponse, ClaimError> {
    let mut request = client
        .post(format!("{relayer_url}/submit"))
        .header("content-type", "application/json")
        .body(body.to_string());
    for (name, value) in builder_auth.headers("POST", "/submit", body)? {
        request = request.header(name, value);
    }
    request
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|err| ClaimError::Transport(format!("submit relayer transaction: {err}")))?
        .json()
        .map_err(|err| ClaimError::Transport(format!("parse relayer submit response: {err}")))
}

fn wait_for_relayer_transaction(
    client: &BlockingHttpClient,
    relayer_url: &str,
    transaction_id: &str,
) -> Result<RelayerTransaction, ClaimError> {
    for _ in 0..DEFAULT_RELAY_MAX_POLLS {
        let transactions: Vec<RelayerTransaction> = client
            .get(format!("{relayer_url}/transaction"))
            .query(&[("id", transaction_id)])
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|err| ClaimError::Transport(format!("load relayer transaction: {err}")))?
            .json()
            .map_err(|err| ClaimError::Transport(format!("parse relayer transaction: {err}")))?;

        if let Some(transaction) = transactions.into_iter().next() {
            match transaction.state.as_str() {
                "STATE_MINED" | "STATE_CONFIRMED" => return Ok(transaction),
                "STATE_FAILED" | "STATE_INVALID" => {
                    return Err(ClaimError::Transport(format!(
                        "relayer transaction `{transaction_id}` failed with state {}",
                        transaction.state
                    )))
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(DEFAULT_RELAY_POLL_FREQUENCY_MS));
    }

    Err(ClaimError::Transport(format!(
        "timed out waiting for relayer transaction `{transaction_id}`"
    )))
}

fn fetch_receipt_block_number(rpc_url: &str, transaction_hash: &str) -> Result<u64, ClaimError> {
    #[derive(Serialize)]
    struct JsonRpcRequest<'a> {
        jsonrpc: &'static str,
        method: &'static str,
        params: [&'a str; 1],
        id: u8,
    }

    #[derive(Deserialize)]
    struct JsonRpcResponse {
        result: Option<TransactionReceipt>,
        error: Option<JsonRpcError>,
    }

    #[derive(Deserialize)]
    struct TransactionReceipt {
        #[serde(rename = "blockNumber")]
        block_number: Option<String>,
    }

    #[derive(Deserialize)]
    struct JsonRpcError {
        message: String,
    }

    let client = BlockingHttpClient::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| ClaimError::Transport(format!("build rpc client: {err}")))?;
    let payload = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "eth_getTransactionReceipt",
        params: [transaction_hash],
        id: 1,
    };
    let response: JsonRpcResponse = client
        .post(rpc_url)
        .json(&payload)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|err| ClaimError::Transport(format!("load transaction receipt: {err}")))?
        .json()
        .map_err(|err| ClaimError::Transport(format!("parse transaction receipt: {err}")))?;

    if let Some(error) = response.error {
        return Err(ClaimError::Transport(format!(
            "polygon rpc returned receipt error: {}",
            error.message
        )));
    }

    let block_number = response
        .result
        .and_then(|receipt| receipt.block_number)
        .ok_or_else(|| {
            ClaimError::Transport(format!(
                "receipt for transaction `{transaction_hash}` is not available yet"
            ))
        })?;

    u64::from_str_radix(block_number.trim_start_matches("0x"), 16).map_err(|err| {
        ClaimError::Transport(format!(
            "parse receipt block number `{block_number}`: {err}"
        ))
    })
}

fn collapse_redeemable_positions(
    account_id: &str,
    wallet_address: &str,
    positions: Vec<DataPosition>,
) -> Vec<RedeemablePosition> {
    let mut grouped: BTreeMap<String, RedeemablePosition> = BTreeMap::new();

    for position in positions {
        if !position.redeemable || position.size <= Decimal::ZERO {
            continue;
        }

        let condition_id = position.condition_id.to_string();
        let entry = grouped
            .entry(condition_id.clone())
            .or_insert_with(|| RedeemablePosition {
                account_id: account_id.to_string(),
                wallet_address: wallet_address.to_string(),
                condition_id: condition_id.clone(),
                market_id: position
                    .event_id
                    .clone()
                    .or_else(|| Some(position.slug.clone())),
                token_ids: Vec::new(),
                outcome_labels: Vec::new(),
                outcome_indexes: Vec::new(),
                outcome_amounts: Vec::new(),
                redeemable_size: Decimal::ZERO,
                estimated_payout: Decimal::ZERO,
                negative_risk: position.negative_risk,
            });

        entry.token_ids.push(position.asset.to_string());
        entry.outcome_labels.push(position.outcome);
        entry.outcome_indexes.push(position.outcome_index as u8);
        entry.outcome_amounts.push(position.size);
        entry.redeemable_size += position.size;
        entry.estimated_payout += position.size;
        entry.negative_risk = entry.negative_risk || position.negative_risk;
    }

    grouped.into_values().collect()
}

fn redeem_index_sets(outcome_indexes: &[u8]) -> Vec<U256> {
    if outcome_indexes.is_empty() {
        return vec![U256::from(1_u8), U256::from(2_u8)];
    }

    let mut index_sets: Vec<U256> = outcome_indexes
        .iter()
        .map(|index| U256::from(u64::from(*index) + 1))
        .collect();
    index_sets.sort();
    index_sets.dedup();
    index_sets
}

fn neg_risk_amounts(
    outcome_indexes: &[u8],
    outcome_amounts: &[Decimal],
) -> Result<Vec<U256>, ClaimError> {
    if outcome_indexes.len() != outcome_amounts.len() {
        return Err(ClaimError::Validation(
            "outcome_indexes and outcome_amounts length mismatch".to_string(),
        ));
    }

    let mut amounts = [Decimal::ZERO, Decimal::ZERO];
    for (index, amount) in outcome_indexes.iter().zip(outcome_amounts.iter()) {
        match *index {
            0 | 1 => amounts[*index as usize] += *amount,
            other => {
                return Err(ClaimError::Validation(format!(
                    "unsupported neg-risk outcome index `{other}`"
                )))
            }
        }
    }

    amounts
        .iter()
        .map(|amount| decimal_to_usdc_u256(*amount))
        .collect()
}

fn decimal_to_usdc_u256(amount: Decimal) -> Result<U256, ClaimError> {
    if amount < Decimal::ZERO {
        return Err(ClaimError::Validation(format!(
            "claim amount must be non-negative, got {amount}"
        )));
    }

    let scaled = amount * Decimal::from(1_000_000_u64);
    if !scaled.fract().is_zero() {
        return Err(ClaimError::Validation(format!(
            "claim amount exceeds USDC precision: {amount}"
        )));
    }

    let raw: u64 = scaled
        .try_into()
        .map_err(|_| ClaimError::Validation(format!("claim amount too large: {amount}")))?;
    Ok(U256::from(raw))
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
        build_proxy_request, build_safe_create_request, build_safe_request, claim_relay_tx_type,
        tracked_trade_fill, BuilderAuthConfig, CancellationOutcome, CancellationRequest,
        ClaimGateway, ClaimRelayTxType, ClaimRequest, ClaimResult, DEFAULT_POLY_RELAYER_URL,
        ExecutionError, ExecutionOutcome, ExecutionRequest, LiveExecutionGateway,
        PolymarketClaimConfig, PolymarketExecutionConfig, PolymarketExecutionGateway,
        RedeemablePosition, RelayerNoncePayload, RelayerRelayPayload, ReplaceOutcome,
        ReplaceRequest, StaticClaimGateway, StaticExecutionGateway, TrackedOrder,
        WalletSignatureType,
    };
    use chrono::Utc;
    use ploy_trading::{FillRecord, TradeSide};
    use polymarket_client_sdk::auth::{ApiKey, LocalSigner, Signer};
    use polymarket_client_sdk::clob::types::response::{MakerOrder, TradeResponse};
    use polymarket_client_sdk::clob::types::{TradeStatusType, TraderSide};
    use polymarket_client_sdk::{POLYGON, wallet_contract_config};
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

    #[test]
    fn static_claim_gateway_replays_redeemable_positions() {
        let gateway = StaticClaimGateway::default().with_positions(vec![RedeemablePosition {
            account_id: "acct-live".to_string(),
            wallet_address: "0xwallet".to_string(),
            condition_id: "0xcondition".to_string(),
            market_id: Some("market-1".to_string()),
            token_ids: vec!["1".to_string(), "2".to_string()],
            outcome_labels: vec!["YES".to_string(), "NO".to_string()],
            outcome_indexes: vec![0, 1],
            outcome_amounts: vec![dec!(3), dec!(0)],
            redeemable_size: dec!(3),
            estimated_payout: dec!(3),
            negative_risk: false,
        }]);

        let positions = gateway
            .discover_redeemable_positions("acct-live")
            .expect("positions");

        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].condition_id, "0xcondition");
        assert_eq!(positions[0].wallet_address, "0xwallet");
    }

    #[test]
    fn static_claim_gateway_replays_claim_result() {
        let gateway = StaticClaimGateway::default().with_claim_result(Ok(ClaimResult {
            tx_hash: "0xtx".to_string(),
            block_number: 123,
            amount_claimed: dec!(4.2),
        }));

        let result = gateway
            .claim(&ClaimRequest {
                account_id: "acct-live".to_string(),
                wallet_address: "0xwallet".to_string(),
                condition_id: "0xcondition".to_string(),
                outcome_indexes: vec![0, 1],
                outcome_amounts: vec![dec!(4.2), dec!(0)],
                negative_risk: false,
            })
            .expect("claim result");

        assert_eq!(result.tx_hash, "0xtx");
        assert_eq!(result.block_number, 123);
        assert_eq!(result.amount_claimed, dec!(4.2));
    }

    #[test]
    fn claim_config_has_default_rpc_url() {
        let config = PolymarketClaimConfig::default();
        assert!(!config.rpc_url.is_empty());
    }

    #[test]
    fn claim_config_has_default_relayer_url() {
        let config = PolymarketClaimConfig::default();
        assert_eq!(config.relayer_url, DEFAULT_POLY_RELAYER_URL);
    }

    #[test]
    fn claim_relay_tx_type_rejects_eoa_signature_type() {
        let err = claim_relay_tx_type(WalletSignatureType::Eoa).expect_err("eoa unsupported");
        assert!(err.to_string().contains("relay-first auto-claim"));
    }

    #[test]
    fn claim_relay_tx_type_maps_proxy_and_safe_wallets() {
        assert_eq!(
            claim_relay_tx_type(WalletSignatureType::Proxy).expect("proxy relay type"),
            ClaimRelayTxType::Proxy
        );
        assert_eq!(
            claim_relay_tx_type(WalletSignatureType::GnosisSafe).expect("safe relay type"),
            ClaimRelayTxType::Safe
        );
    }

    #[test]
    fn builder_auth_requires_credentials_for_relayer_claims() {
        let err = BuilderAuthConfig::default()
            .headers("POST", "/submit", "{}")
            .expect_err("missing builder credentials should fail");
        assert!(err.to_string().contains("builder credentials"));
    }

    #[test]
    fn proxy_request_signature_matches_official_vector() {
        let signer = LocalSigner::from_str(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .expect("signer")
        .with_chain_id(Some(POLYGON));
        let owner = signer.address();
        let proxy_factory = wallet_contract_config(POLYGON)
            .and_then(|config| config.proxy_factory)
            .expect("proxy factory");
        let target = alloy::primitives::Address::from_str("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174")
            .expect("usdc");
        let calldata = alloy::hex::decode("095ea7b30000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
            .expect("approve calldata");
        let request = build_proxy_request(
            &signer,
            owner,
            proxy_factory,
            target,
            calldata,
            &RelayerRelayPayload {
                address: "0xae700edfd9ab986395f3999fe11177b9903a52f1".to_string(),
                nonce: "0".to_string(),
            },
            None,
            Some(85_338),
        )
        .expect("proxy request");

        assert_eq!(
            request.signature,
            "0x4c18e2d2294a00d686714aff8e7936ab657cb4655dfccb2b556efadcb7e835f800dc2fecec69c501e29bb36ecb54b4da6b7c410c4dc740a33af2afde2b77297e1b"
        );
    }

    #[test]
    fn safe_request_signature_matches_official_vector() {
        let signer = LocalSigner::from_str(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .expect("signer")
        .with_chain_id(Some(POLYGON));
        let owner = signer.address();
        let target = alloy::primitives::Address::from_str("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174")
            .expect("usdc");
        let calldata = alloy::hex::decode("095ea7b30000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
            .expect("approve calldata");
        let request = build_safe_request(
            &signer,
            owner,
            target,
            calldata,
            &RelayerNoncePayload {
                nonce: "0".to_string(),
            },
        )
        .expect("safe request");

        assert_eq!(
            request.signature,
            "0xf368488355b0566e99eff3bccc35e98b77d8f3a6e6866176188488c34f0305b07e4a4c600c7a1592e4ac1e96b5887ebff2cb26987a3ad501006b39944df098c21f"
        );
    }

    #[test]
    fn safe_create_request_signature_matches_official_vector() {
        let signer = LocalSigner::from_str(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        )
        .expect("signer")
        .with_chain_id(Some(POLYGON));
        let owner = signer.address();
        let safe_factory = wallet_contract_config(POLYGON)
            .map(|config| config.safe_factory)
            .expect("safe factory");
        let request = build_safe_create_request(&signer, owner, safe_factory)
            .expect("safe create request");

        assert_eq!(
            request.signature,
            "0xe3e791c24134b7bebe93b4771bd07c7fe7bbe115eeb0bf629ac3b7a435e7ac8d05f979729d873f7d0e16205becf48ee450aa382bc28c65eedcd6454e81d81f921b"
        );
        assert_eq!(request.tx_type, "SAFE-CREATE");
    }
}
