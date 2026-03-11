use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::coordinator::DrawdownSnapshot;
use crate::coordinator::OrderIntent;

use super::{metadata_decimal, ExecutionJournal};

impl ExecutionJournal {
    pub(in crate::coordinator) async fn persist_signal_from_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let strategy_id = intent
            .metadata
            .get("strategy")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let signal_type = intent
            .metadata
            .get("signal_type")
            .cloned()
            .unwrap_or_else(|| {
                if intent.is_buy {
                    "entry_intent".to_string()
                } else {
                    "exit_intent".to_string()
                }
            });
        let symbol = intent.metadata.get("symbol").cloned();
        let confidence = metadata_decimal(intent, "signal_confidence");
        let momentum_value = metadata_decimal(intent, "signal_momentum_value");
        let short_ma = metadata_decimal(intent, "signal_short_ma");
        let long_ma = metadata_decimal(intent, "signal_long_ma");
        let rolling_volatility = metadata_decimal(intent, "signal_rolling_volatility");
        let fair_value = metadata_decimal(intent, "signal_fair_value");
        let market_price = metadata_decimal(intent, "signal_market_price");
        let edge = metadata_decimal(intent, "signal_edge");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let context =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO signal_history (
                account_id, intent_id, agent_id, strategy_id, domain, signal_type, market_slug, token_id,
                symbol, side, confidence, momentum_value, short_ma, long_ma, rolling_volatility,
                fair_value, market_price, edge, config_hash, context
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15,
                $16,$17,$18,$19,$20
            )
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(&strategy_id)
        .bind(intent.domain.to_string())
        .bind(&signal_type)
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(symbol)
        .bind(intent.side.as_str())
        .bind(confidence)
        .bind(momentum_value)
        .bind(short_ma)
        .bind(long_ma)
        .bind(rolling_volatility)
        .bind(fair_value)
        .bind(market_price)
        .bind(edge)
        .bind(config_hash)
        .bind(sqlx::types::Json(context))
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist signal history"
            );
        }
    }

    pub(in crate::coordinator) async fn persist_risk_decision(
        &self,
        intent: &OrderIntent,
        decision: &str,
        block_reason: Option<String>,
        adjusted: Option<(u64, String)>,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let (suggestion_max_shares, suggestion_reason) = adjusted
            .map(|(shares, reason)| (Some(shares as i64), Some(reason)))
            .unwrap_or((None, None));
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO risk_gate_decisions (
                account_id, intent_id, agent_id, domain, decision, block_reason, suggestion_max_shares,
                suggestion_reason, notional_value, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
            ON CONFLICT (intent_id) DO UPDATE SET
                decision = EXCLUDED.decision,
                block_reason = EXCLUDED.block_reason,
                suggestion_max_shares = EXCLUDED.suggestion_max_shares,
                suggestion_reason = EXCLUDED.suggestion_reason,
                notional_value = EXCLUDED.notional_value,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                decided_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(decision)
        .bind(block_reason)
        .bind(suggestion_max_shares)
        .bind(suggestion_reason)
        .bind(intent.notional_value())
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist risk gate decision"
            );
        }
    }

    pub(in crate::coordinator) async fn persist_exit_reason_intent(&self, intent: &OrderIntent) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let reason_code = intent
            .metadata
            .get("exit_reason")
            .or_else(|| intent.metadata.get("reason_code"))
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());
        let reason_detail = intent.metadata.get("exit_detail").cloned();
        let entry_price = metadata_decimal(intent, "entry_price");
        let pnl_pct = metadata_decimal(intent, "pnl_pct");
        let config_hash = intent.metadata.get("config_hash").cloned();
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, pnl_pct, status, config_hash, metadata
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'INTENT_SUBMITTED',$12,$13)
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_code = EXCLUDED.reason_code,
                reason_detail = EXCLUDED.reason_detail,
                entry_price = COALESCE(EXCLUDED.entry_price, exit_reasons.entry_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(reason_code)
        .bind(reason_detail)
        .bind(entry_price)
        .bind(pnl_pct)
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist exit reason intent"
            );
        }
    }

    pub(in crate::coordinator) async fn persist_risk_runtime_state(
        &self,
        risk_state: String,
        daily_date: NaiveDate,
        daily_pnl: Decimal,
        daily_loss_limit: Decimal,
        drawdown: DrawdownSnapshot,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let result = sqlx::query(
            r#"
            INSERT INTO risk_runtime_state (
                account_id,
                risk_state,
                daily_date,
                daily_pnl,
                daily_loss_limit,
                current_equity,
                equity_peak,
                current_drawdown,
                max_drawdown_observed
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
            ON CONFLICT (account_id) DO UPDATE SET
                risk_state = EXCLUDED.risk_state,
                daily_date = EXCLUDED.daily_date,
                daily_pnl = EXCLUDED.daily_pnl,
                daily_loss_limit = EXCLUDED.daily_loss_limit,
                current_equity = EXCLUDED.current_equity,
                equity_peak = EXCLUDED.equity_peak,
                current_drawdown = EXCLUDED.current_drawdown,
                max_drawdown_observed = EXCLUDED.max_drawdown_observed,
                updated_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(risk_state)
        .bind(daily_date)
        .bind(daily_pnl)
        .bind(daily_loss_limit)
        .bind(drawdown.current_equity)
        .bind(drawdown.equity_peak)
        .bind(drawdown.current_drawdown)
        .bind(drawdown.max_drawdown_observed)
        .execute(pool)
        .await;

        if let Err(error) = result {
            tracing::warn!(
                account_id = %self.account_id,
                error = %error,
                "failed to persist risk runtime state"
            );
        }
    }
}
