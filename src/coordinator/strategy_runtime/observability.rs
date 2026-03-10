use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tracing::warn;

pub(super) async fn persist_live_order_signal_history(
    pool: &PgPool,
    account_id: &str,
    strategy_label: &str,
    strategy_id: &str,
    signal_type: &str,
    token_id: Option<&str>,
    side: Option<&str>,
    order_price: Option<Decimal>,
    fill_price: Option<Decimal>,
    context: Value,
) {
    let agent_id = format!("{}_runtime", strategy_label);
    let result = sqlx::query(
        r#"
        INSERT INTO signal_history (
            account_id, intent_id, agent_id, strategy_id, domain, signal_type,
            market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
        )
        VALUES (
            $1, NULL, $2, $3, 'strategy_runtime', $4,
            NULL, $5, NULL, $6, NULL, $7, $8, NULL, NULL, $9
        )
        "#,
    )
    .bind(account_id)
    .bind(agent_id)
    .bind(strategy_id)
    .bind(signal_type)
    .bind(token_id)
    .bind(side)
    .bind(order_price)
    .bind(fill_price)
    .bind(sqlx::types::Json(context))
    .execute(pool)
    .await;

    if let Err(e) = result {
        warn!(
            strategy = strategy_label,
            strategy_id = strategy_id,
            signal_type = signal_type,
            error = %e,
            "failed to persist live order signal_history observation"
        );
    }
}
