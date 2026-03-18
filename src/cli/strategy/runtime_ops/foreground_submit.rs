use crate::adapters::PostgresStore;
use crate::domain::OrderStatus;
use crate::error::{PloyError, Result};
use crate::strategy::traits::StrategyOrderIntent;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tracing::{error, info, warn};

pub(super) struct ForegroundIntentSubmitter {
    dry_run: bool,
}

impl ForegroundIntentSubmitter {
    pub(super) fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    async fn submit(
        &self,
        strategy_id: &str,
        intent: &StrategyOrderIntent,
    ) -> Result<ForegroundSubmitOutcome> {
        if let Some(payload) = build_coordinator_payload(strategy_id, intent, self.dry_run)? {
            let response = submit_intent_via_coordinator(&payload).await?;
            return Ok(ForegroundSubmitOutcome::CoordinatorAccepted(response));
        }

        Ok(ForegroundSubmitOutcome::Skipped {
            reason: coordinator_rejection_reason(intent).to_string(),
        })
    }
}

#[derive(Debug)]
enum ForegroundSubmitOutcome {
    CoordinatorAccepted(CoordinatorIntentResponse),
    Skipped { reason: String },
}

#[derive(Debug, Deserialize)]
struct CoordinatorIntentResponse {
    success: bool,
    intent_id: String,
    message: String,
    dry_run: bool,
}

pub(super) async fn handle_submit_intent(
    strategy_id: &str,
    intent: StrategyOrderIntent,
    submitter: &ForegroundIntentSubmitter,
    store: Option<&Arc<PostgresStore>>,
) {
    let client_order_id = intent.client_order_id.clone();
    let mut order = crate::domain::order_request_from_strategy_intent(&intent);
    if order.client_order_id != client_order_id {
        warn!(
            "Mismatched order IDs in strategy action: action={}, request={}; using action ID",
            client_order_id, order.client_order_id
        );
        order.client_order_id = client_order_id.clone();
    }

    let tracked_order_id = order.client_order_id.clone();
    let price_cents = order.limit_price * rust_decimal::Decimal::from(100);
    print_order_submission(
        strategy_id,
        &tracked_order_id,
        &order.token_id,
        &order,
        price_cents,
    );

    persist_pending_order(store, strategy_id, &order, &tracked_order_id).await;

    match submitter.submit(strategy_id, &intent).await {
        Ok(ForegroundSubmitOutcome::CoordinatorAccepted(response)) => {
            println!("  \x1b[32m✓ Intent submitted via coordinator\x1b[0m");
            println!("    Intent ID: {}", response.intent_id);
            println!("    Success: {}", response.success);
            println!("    Message: {}", response.message);
            println!("    Dry Run: {}\n", response.dry_run);
            info!(
                "Foreground runtime routed order {} through coordinator intent {}",
                tracked_order_id, response.intent_id
            );
        }
        Ok(ForegroundSubmitOutcome::Skipped { reason }) => {
            println!(
                "  \x1b[33m⚠ Coordinator ingress required - order logged but not submitted\x1b[0m"
            );
            println!("    Reason: {}\n", reason);
            warn!("Order {} not submitted: {}", tracked_order_id, reason);
            mark_order_status(store, &tracked_order_id, OrderStatus::Failed, None).await;
        }
        Err(error) => {
            println!("  \x1b[31m✗ Order failed: {}\x1b[0m\n", error);
            error!(
                "Foreground order submission failed for {}: {}",
                tracked_order_id, error
            );
            let failed_status = if matches!(error, PloyError::Validation(_)) {
                OrderStatus::Rejected
            } else {
                OrderStatus::Failed
            };
            mark_order_status(store, &tracked_order_id, failed_status, None).await;
        }
    }
}

async fn persist_pending_order(
    store: Option<&Arc<PostgresStore>>,
    strategy_id: &str,
    order: &crate::domain::OrderRequest,
    tracked_order_id: &str,
) {
    let Some(store) = store else {
        return;
    };

    let db_order =
        crate::domain::Order::from_request(order, None, 1, Some(strategy_id.to_string()));
    if let Err(error) = store.insert_order(&db_order).await {
        warn!(
            "Failed to persist strategy order {}: {}",
            tracked_order_id, error
        );
    }
}

async fn mark_order_status(
    store: Option<&Arc<PostgresStore>>,
    tracked_order_id: &str,
    status: OrderStatus,
    exchange_order_id: Option<&str>,
) {
    let Some(store) = store else {
        return;
    };

    if let Err(error) = store
        .update_order_status(tracked_order_id, status, exchange_order_id)
        .await
    {
        warn!(
            "Failed to update order {} status to {:?}: {}",
            tracked_order_id, status, error
        );
    }
}

fn print_order_submission(
    strategy_id: &str,
    tracked_order_id: &str,
    token_id: &str,
    order: &crate::domain::OrderRequest,
    price_cents: rust_decimal::Decimal,
) {
    println!("\n  \x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!(
        "  \x1b[36m║\x1b[0m  📤 ORDER SUBMISSION                                          \x1b[36m║\x1b[0m"
    );
    println!("  \x1b[36m╠══════════════════════════════════════════════════════════════╣\x1b[0m");
    println!(
        "  \x1b[36m║\x1b[0m  Strategy: {:<47}\x1b[36m║\x1b[0m",
        strategy_id
    );
    println!(
        "  \x1b[36m║\x1b[0m  Order ID: {:<47}\x1b[36m║\x1b[0m",
        tracked_order_id
    );
    println!(
        "  \x1b[36m║\x1b[0m  Token: {:<50}\x1b[36m║\x1b[0m",
        &token_id[..token_id.len().min(50)]
    );
    println!(
        "  \x1b[36m║\x1b[0m  Side: {:?}, Shares: {}, Price: {:.2}¢{:<20}\x1b[36m║\x1b[0m",
        order.market_side, order.shares, price_cents, ""
    );
    println!("  \x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");
}

fn build_coordinator_payload(
    strategy_id: &str,
    intent: &StrategyOrderIntent,
    dry_run: bool,
) -> Result<Option<Value>> {
    let Some(deployment_id) = deployment_id_from_metadata(&intent.metadata) else {
        return Ok(None);
    };
    let price_limit = intent.limit_price.to_f64().ok_or_else(|| {
        PloyError::Validation(format!(
            "strategy intent {} has price that cannot be represented as f64",
            intent.client_order_id
        ))
    })?;

    let mut metadata = intent.metadata.clone();
    metadata
        .entry("source".to_string())
        .or_insert_with(|| "cli.strategy.foreground".to_string());
    metadata
        .entry("strategy_id".to_string())
        .or_insert_with(|| strategy_id.to_string());
    metadata
        .entry("runtime".to_string())
        .or_insert_with(|| "cli.foreground".to_string());
    metadata
        .entry("client_order_id".to_string())
        .or_insert_with(|| intent.client_order_id.clone());

    Ok(Some(json!({
        "deployment_id": deployment_id,
        "domain": intent.domain.to_string().to_ascii_lowercase(),
        "market_slug": intent.market_slug.clone(),
        "token_id": intent.token_id.clone(),
        "side": intent.side.as_str(),
        "order_side": if intent.is_buy { "BUY" } else { "SELL" },
        "is_buy": intent.is_buy,
        "size": intent.shares,
        "price_limit": price_limit,
        "idempotency_key": intent.client_order_id.clone(),
        "reason": format!("foreground strategy submit: {}", strategy_id),
        "priority": external_priority_label(intent.priority),
        "metadata": metadata,
        "dry_run": dry_run,
    })))
}

fn deployment_id_from_metadata(
    metadata: &std::collections::HashMap<String, String>,
) -> Option<String> {
    ["deployment_id", "deploymentId"].iter().find_map(|key| {
        metadata
            .get(*key)
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn coordinator_rejection_reason(intent: &StrategyOrderIntent) -> &'static str {
    if deployment_id_from_metadata(&intent.metadata).is_none() {
        "strategy intent is missing deployment_id metadata required for coordinator ingress"
    } else {
        "foreground runtime no longer supports direct execution fallback"
    }
}

fn external_priority_label(priority: u8) -> &'static str {
    match priority {
        90..=u8::MAX => "high",
        8..=89 => "high",
        5..=7 => "normal",
        _ => "low",
    }
}

fn coordinator_intent_ingress_url() -> String {
    std::env::var("PLOY_RPC_COORDINATOR_INTENT_URL")
        .or_else(|_| std::env::var("PLOY_COORDINATOR_INTENT_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8081/api/sidecar/intents".to_string())
}

fn coordinator_intent_ingress_token() -> Option<String> {
    std::env::var("PLOY_RPC_SIDECAR_AUTH_TOKEN")
        .or_else(|_| std::env::var("PLOY_SIDECAR_AUTH_TOKEN"))
        .or_else(|_| std::env::var("PLOY_API_SIDECAR_AUTH_TOKEN"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn submit_intent_via_coordinator(payload: &Value) -> Result<CoordinatorIntentResponse> {
    let url = coordinator_intent_ingress_url();
    let client = coordinator_ingress_http_client()?;

    let mut request = client.post(&url).json(payload);
    if let Some(token) = coordinator_intent_ingress_token() {
        request = request.header("x-ploy-sidecar-token", token);
    }

    let response = request.send().await.map_err(|error| {
        PloyError::Internal(format!(
            "failed to reach coordinator intent ingress {}: {}",
            url, error
        ))
    })?;
    let status = response.status();
    let text = response
        .text()
        .await
        .unwrap_or_else(|_| "<empty>".to_string());

    if !status.is_success() {
        let message = format!(
            "coordinator intent ingress rejected request (status={}): {}",
            status, text
        );
        return Err(if status.is_client_error() {
            PloyError::Validation(message)
        } else {
            PloyError::Internal(message)
        });
    }

    serde_json::from_str(&text).map_err(|error| {
        PloyError::Internal(format!(
            "invalid ingress JSON from coordinator intent ingress: {}",
            error
        ))
    })
}

fn coordinator_ingress_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<std::result::Result<reqwest::Client, String>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(16)
            .tcp_nodelay(true)
            .build()
            .map_err(|error| format!("failed to build http client: {}", error))
    });

    client
        .as_ref()
        .map_err(|msg| PloyError::Internal(msg.clone()))
}

#[cfg(test)]
mod tests {
    use super::{build_coordinator_payload, external_priority_label};
    use crate::domain::Domain;
    use crate::domain::{OrderType, Side, TimeInForce};
    use crate::strategy::traits::StrategyOrderIntent;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn sample_intent(metadata: HashMap<String, String>) -> StrategyOrderIntent {
        StrategyOrderIntent {
            client_order_id: "intent-123".to_string(),
            domain: Domain::Crypto,
            market_slug: "btc-15m".to_string(),
            token_id: "token-yes".to_string(),
            side: Side::Up,
            is_buy: true,
            shares: 25,
            limit_price: dec!(0.44),
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            priority: 7,
            metadata,
        }
    }

    #[test]
    fn build_coordinator_payload_requires_deployment_id() {
        let payload = build_coordinator_payload("momentum", &sample_intent(HashMap::new()), false)
            .expect("payload build should succeed");
        assert!(payload.is_none());
    }

    #[test]
    fn build_coordinator_payload_preserves_strategy_metadata() {
        let payload = build_coordinator_payload(
            "momentum",
            &sample_intent(HashMap::from([(
                "deployment_id".to_string(),
                "deploy.crypto.test".to_string(),
            )])),
            true,
        )
        .expect("payload build should succeed")
        .expect("payload should route through coordinator");

        assert_eq!(payload["deployment_id"], "deploy.crypto.test");
        assert_eq!(payload["domain"], "crypto");
        assert_eq!(payload["market_slug"], "btc-15m");
        assert_eq!(payload["token_id"], "token-yes");
        assert_eq!(payload["side"], "UP");
        assert_eq!(payload["order_side"], "BUY");
        assert_eq!(payload["size"], 25);
        assert_eq!(payload["price_limit"], 0.44);
        assert_eq!(payload["idempotency_key"], "intent-123");
        assert_eq!(payload["priority"], "normal");
        assert_eq!(payload["dry_run"], true);
        assert_eq!(payload["metadata"]["strategy_id"], "momentum");
        assert_eq!(payload["metadata"]["runtime"], "cli.foreground");
        assert_eq!(payload["metadata"]["client_order_id"], "intent-123");
    }

    #[test]
    fn external_priority_label_clamps_critical_to_high() {
        assert_eq!(external_priority_label(90), "high");
        assert_eq!(external_priority_label(8), "high");
        assert_eq!(external_priority_label(7), "normal");
        assert_eq!(external_priority_label(1), "low");
    }
}
