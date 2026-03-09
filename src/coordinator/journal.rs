use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;

use sqlx::{PgPool, Row};

use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::Result;
use crate::platform::{Domain, DrawdownSnapshot, OrderIntent};
use crate::strategy::executor::ExecutionResult;

#[derive(Debug, Clone)]
pub(super) struct PersistedExecutionFill {
    pub(super) intent_id: uuid::Uuid,
    pub(super) agent_id: String,
    pub(super) domain: Domain,
    pub(super) market_slug: String,
    pub(super) token_id: String,
    pub(super) side: Side,
    pub(super) is_buy: bool,
    pub(super) filled_shares: u64,
    pub(super) fill_price: Decimal,
    pub(super) executed_at: DateTime<Utc>,
    pub(super) metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub(super) struct PersistedExecutionOutcome {
    pub(super) agent_id: String,
    pub(super) executed_at: DateTime<Utc>,
    pub(super) is_failure: bool,
}

#[derive(Debug, Clone)]
pub(super) struct PersistedRiskRuntimeState {
    pub(super) drawdown: DrawdownSnapshot,
    pub(super) daily_date: Option<NaiveDate>,
    pub(super) daily_pnl: Decimal,
    pub(super) risk_state_raw: String,
}

#[derive(Debug, Clone)]
pub(super) struct ExecutionRestoreData {
    pub(super) fills: Vec<PersistedExecutionFill>,
    pub(super) outcomes_today: Vec<PersistedExecutionOutcome>,
}

#[derive(Debug, Clone)]
pub(super) struct ExecutionJournal {
    account_id: String,
    pool: Option<PgPool>,
}

impl ExecutionJournal {
    pub(super) fn new(account_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            pool: None,
        }
    }

    pub(super) fn set_pool(&mut self, pool: PgPool) {
        self.pool = Some(pool);
    }

    pub(super) async fn load_risk_runtime_state(
        &self,
    ) -> Result<Option<PersistedRiskRuntimeState>> {
        let Some(pool) = self.pool.as_ref() else {
            return Ok(None);
        };

        let row = sqlx::query(
            r#"
            SELECT
                current_equity,
                equity_peak,
                current_drawdown,
                max_drawdown_observed,
                daily_pnl,
                daily_date,
                risk_state
            FROM risk_runtime_state
            WHERE account_id = $1
            "#,
        )
        .bind(&self.account_id)
        .fetch_optional(pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        Ok(Some(PersistedRiskRuntimeState {
            drawdown: DrawdownSnapshot {
                current_equity: row.try_get("current_equity").unwrap_or(Decimal::ZERO),
                equity_peak: row.try_get("equity_peak").unwrap_or(Decimal::ZERO),
                current_drawdown: row.try_get("current_drawdown").unwrap_or(Decimal::ZERO),
                max_drawdown_observed: row
                    .try_get("max_drawdown_observed")
                    .unwrap_or(Decimal::ZERO),
            },
            daily_date: row.try_get("daily_date").ok(),
            daily_pnl: row.try_get("daily_pnl").unwrap_or(Decimal::ZERO),
            risk_state_raw: row.try_get("risk_state").unwrap_or_default(),
        }))
    }

    pub(super) async fn load_execution_restore_data(
        &self,
        dry_run: bool,
        window_start: DateTime<Utc>,
        window_end: DateTime<Utc>,
    ) -> Result<Option<ExecutionRestoreData>> {
        let Some(pool) = self.pool.as_ref() else {
            return Ok(None);
        };

        let fills = load_execution_log_fills(pool, &self.account_id, dry_run).await?;
        let outcomes_today =
            load_execution_log_outcomes(pool, &self.account_id, dry_run, window_start, window_end)
                .await?;

        Ok(Some(ExecutionRestoreData {
            fills,
            outcomes_today,
        }))
    }

    pub(super) async fn persist_execution(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&ExecutionResult>,
        error_message: Option<String>,
        queue_delay_ms: Option<i64>,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let (order_id, status, filled_shares, avg_fill_price, elapsed_ms) = match result {
            Some(r) => (
                Some(r.order_id.clone()),
                format!("{:?}", r.status),
                r.filled_shares as i64,
                r.avg_fill_price,
                Some(r.elapsed_ms as i64),
            ),
            None => (None, format!("{:?}", OrderStatus::Failed), 0, None, None),
        };

        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let config_hash = intent.metadata.get("config_hash").cloned();

        let query = sqlx::query(
            r#"
            INSERT INTO agent_order_executions (
                account_id,
                agent_id,
                intent_id,
                domain,
                market_slug,
                token_id,
                market_side,
                is_buy,
                shares,
                limit_price,
                order_id,
                status,
                filled_shares,
                avg_fill_price,
                elapsed_ms,
                dry_run,
                error,
                intent_created_at,
                metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                order_id = EXCLUDED.order_id,
                status = EXCLUDED.status,
                filled_shares = EXCLUDED.filled_shares,
                avg_fill_price = EXCLUDED.avg_fill_price,
                elapsed_ms = EXCLUDED.elapsed_ms,
                dry_run = EXCLUDED.dry_run,
                error = EXCLUDED.error,
                metadata = EXCLUDED.metadata,
                executed_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(&intent.agent_id)
        .bind(intent.intent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.side.as_str())
        .bind(intent.is_buy)
        .bind(intent.shares as i64)
        .bind(request.limit_price)
        .bind(order_id)
        .bind(status)
        .bind(filled_shares)
        .bind(avg_fill_price)
        .bind(elapsed_ms)
        .bind(dry_run)
        .bind(error_message.clone())
        .bind(intent.created_at)
        .bind(sqlx::types::Json(metadata));

        if let Err(e) = query.execute(pool).await {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist agent order execution"
            );
        }

        self.persist_execution_analysis(
            dry_run,
            intent,
            request,
            result,
            queue_delay_ms,
            config_hash,
        )
        .await;

        if !intent.is_buy {
            self.persist_exit_reason_execution(intent, result, error_message)
                .await;
        }
    }

    pub(super) async fn persist_signal_from_intent(&self, intent: &OrderIntent) {
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

        if let Err(e) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist signal history"
            );
        }
    }

    pub(super) async fn persist_risk_decision(
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

        if let Err(e) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist risk gate decision"
            );
        }
    }

    pub(super) async fn persist_exit_reason_intent(&self, intent: &OrderIntent) {
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

        if let Err(e) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason intent"
            );
        }
    }

    pub(super) async fn persist_risk_runtime_state(
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

        if let Err(e) = result {
            tracing::warn!(
                account_id = %self.account_id,
                error = %e,
                "failed to persist risk runtime state"
            );
        }
    }

    async fn persist_exit_reason_execution(
        &self,
        intent: &OrderIntent,
        result: Option<&ExecutionResult>,
        error_message: Option<String>,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let executed_price = result.and_then(|r| r.avg_fill_price);
        let status = result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());
        let reason_detail = error_message.or_else(|| {
            intent
                .metadata
                .get("exit_detail")
                .cloned()
                .or_else(|| intent.metadata.get("error").cloned())
        });
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let result = sqlx::query(
            r#"
            INSERT INTO exit_reasons (
                account_id, intent_id, agent_id, domain, market_slug, token_id, market_side, reason_code,
                reason_detail, entry_price, exit_price, pnl_pct, status, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,$8,
                $9,$10,$11,$12,$13,$14,$15
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                reason_detail = COALESCE(EXCLUDED.reason_detail, exit_reasons.reason_detail),
                exit_price = COALESCE(EXCLUDED.exit_price, exit_reasons.exit_price),
                pnl_pct = COALESCE(EXCLUDED.pnl_pct, exit_reasons.pnl_pct),
                status = EXCLUDED.status,
                config_hash = COALESCE(EXCLUDED.config_hash, exit_reasons.config_hash),
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
        .bind(
            intent
                .metadata
                .get("exit_reason")
                .or_else(|| intent.metadata.get("reason_code"))
                .cloned()
                .unwrap_or_else(|| "UNKNOWN".to_string()),
        )
        .bind(reason_detail)
        .bind(metadata_decimal(intent, "entry_price"))
        .bind(executed_price)
        .bind(metadata_decimal(intent, "pnl_pct"))
        .bind(status)
        .bind(intent.metadata.get("config_hash").cloned())
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist exit reason execution"
            );
        }
    }

    async fn persist_execution_analysis(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        request: &OrderRequest,
        execution_result: Option<&ExecutionResult>,
        queue_delay_ms: Option<i64>,
        config_hash: Option<String>,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let expected_price = request.limit_price;
        let executed_price = execution_result.and_then(|r| r.avg_fill_price);
        let execution_latency_ms = execution_result.map(|r| r.elapsed_ms as i64);
        let total_latency_ms = match (queue_delay_ms, execution_latency_ms) {
            (Some(q), Some(e)) => Some(q + e),
            (Some(q), None) => Some(q),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        };

        let actual_slippage_bps = executed_price.and_then(|fill| {
            if expected_price.is_zero() {
                return None;
            }
            let signed = if intent.is_buy {
                (fill - expected_price) / expected_price
            } else {
                (expected_price - fill) / expected_price
            };
            Some(signed * Decimal::from(10_000))
        });

        let expected_slippage_bps = metadata_decimal(intent, "expected_slippage_bps")
            .or_else(|| metadata_decimal(intent, "signal_expected_slippage_bps"));
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let status = execution_result
            .map(|r| format!("{:?}", r.status))
            .unwrap_or_else(|| "Failed".to_string());

        let persist_result = sqlx::query(
            r#"
            INSERT INTO execution_analysis (
                account_id, intent_id, agent_id, domain, market_slug, token_id, is_buy,
                expected_price, executed_price, expected_slippage_bps, actual_slippage_bps,
                queue_delay_ms, execution_latency_ms, total_latency_ms,
                status, dry_run, config_hash, metadata
            )
            VALUES (
                $1,$2,$3,$4,$5,$6,$7,
                $8,$9,$10,$11,
                $12,$13,$14,
                $15,$16,$17,$18
            )
            ON CONFLICT (intent_id) DO UPDATE SET
                executed_price = EXCLUDED.executed_price,
                expected_slippage_bps = EXCLUDED.expected_slippage_bps,
                actual_slippage_bps = EXCLUDED.actual_slippage_bps,
                queue_delay_ms = EXCLUDED.queue_delay_ms,
                execution_latency_ms = EXCLUDED.execution_latency_ms,
                total_latency_ms = EXCLUDED.total_latency_ms,
                status = EXCLUDED.status,
                dry_run = EXCLUDED.dry_run,
                config_hash = EXCLUDED.config_hash,
                metadata = EXCLUDED.metadata,
                recorded_at = NOW()
            "#,
        )
        .bind(&self.account_id)
        .bind(intent.intent_id)
        .bind(&intent.agent_id)
        .bind(intent.domain.to_string())
        .bind(&intent.market_slug)
        .bind(&intent.token_id)
        .bind(intent.is_buy)
        .bind(expected_price)
        .bind(executed_price)
        .bind(expected_slippage_bps)
        .bind(actual_slippage_bps)
        .bind(queue_delay_ms)
        .bind(execution_latency_ms)
        .bind(total_latency_ms)
        .bind(status)
        .bind(dry_run)
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = persist_result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist execution analysis"
            );
        }

        self.persist_live_strategy_evaluation(
            dry_run,
            intent,
            request,
            execution_result,
            expected_slippage_bps,
            actual_slippage_bps,
            total_latency_ms,
        )
        .await;
    }

    async fn persist_live_strategy_evaluation(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        request: &OrderRequest,
        execution_result: Option<&ExecutionResult>,
        expected_slippage_bps: Option<Decimal>,
        actual_slippage_bps: Option<Decimal>,
        total_latency_ms: Option<i64>,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let strategy_id = intent
            .metadata
            .get("strategy")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let deployment_id = intent
            .metadata
            .get("deployment_id")
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let timeframe = intent
            .metadata
            .get("timeframe")
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string);
        let score = metadata_decimal(intent, "signal_confidence")
            .or_else(|| metadata_decimal(intent, "signal_edge"));

        let status = match execution_result {
            Some(result) => match result.status {
                OrderStatus::Submitted | OrderStatus::PartiallyFilled | OrderStatus::Filled => {
                    "PASS"
                }
                OrderStatus::Cancelled => "WARN",
                OrderStatus::Pending
                | OrderStatus::Rejected
                | OrderStatus::Expired
                | OrderStatus::Failed => "FAIL",
            },
            None => "FAIL",
        };

        let evidence_hash = intent.intent_id.to_string();
        let evidence_payload = serde_json::json!({
            "intent_id": intent.intent_id.to_string(),
            "agent_id": intent.agent_id.clone(),
            "is_buy": intent.is_buy,
            "shares": intent.shares,
            "request_limit_price": request.limit_price.to_string(),
            "order_side": request.order_side.to_string(),
            "expected_slippage_bps": expected_slippage_bps.map(|v| v.to_string()),
            "actual_slippage_bps": actual_slippage_bps.map(|v| v.to_string()),
            "total_latency_ms": total_latency_ms,
            "dry_run": dry_run,
            "execution": execution_result.map(|r| serde_json::json!({
                "order_id": r.order_id.clone(),
                "status": format!("{:?}", r.status),
                "filled_shares": r.filled_shares,
                "avg_fill_price": r.avg_fill_price.map(|p| p.to_string()),
                "elapsed_ms": r.elapsed_ms
            })),
        });
        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

        let insert = sqlx::query(
            r#"
            INSERT INTO strategy_evaluations (
                account_id,
                strategy_id,
                deployment_id,
                domain,
                stage,
                status,
                score,
                timeframe,
                sample_size,
                evidence_kind,
                evidence_ref,
                evidence_hash,
                evidence_payload,
                metadata
            )
            VALUES (
                $1,$2,$3,$4,'LIVE',$5,$6,$7,1,
                'execution_analysis',$8,$9,$10,$11
            )
            ON CONFLICT (account_id, strategy_id, stage, evidence_hash) DO NOTHING
            "#,
        )
        .bind(&self.account_id)
        .bind(strategy_id)
        .bind(deployment_id)
        .bind(intent.domain.to_string())
        .bind(status)
        .bind(score)
        .bind(timeframe)
        .bind(intent.intent_id.to_string())
        .bind(evidence_hash)
        .bind(sqlx::types::Json(evidence_payload))
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(e) = insert {
            tracing::warn!(
                account_id = %self.account_id,
                intent_id = %intent.intent_id,
                error = %e,
                "failed to persist live strategy evaluation evidence"
            );
        }
    }
}

fn metadata_decimal(intent: &OrderIntent, key: &str) -> Option<Decimal> {
    intent
        .metadata
        .get(key)
        .and_then(|v| Decimal::from_str(v).ok())
}

fn parse_persisted_domain(raw: &str) -> Option<Domain> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "sports" => Some(Domain::Sports),
        "crypto" => Some(Domain::Crypto),
        "politics" => Some(Domain::Politics),
        "economics" => Some(Domain::Economics),
        _ => {
            if let Some(raw_id) = normalized.strip_prefix("custom:") {
                return raw_id.trim().parse::<u32>().ok().map(Domain::Custom);
            }
            if let Some(raw_id) = normalized
                .strip_prefix("custom(")
                .and_then(|v| v.strip_suffix(')'))
            {
                return raw_id.trim().parse::<u32>().ok().map(Domain::Custom);
            }
            None
        }
    }
}

fn parse_persisted_side(raw: &str) -> Option<Side> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "UP" | "YES" => Some(Side::Up),
        "DOWN" | "NO" => Some(Side::Down),
        _ => None,
    }
}

fn string_metadata_from_json(
    raw: Option<sqlx::types::Json<serde_json::Value>>,
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    let Some(sqlx::types::Json(value)) = raw else {
        return metadata;
    };
    let Some(object) = value.as_object() else {
        return metadata;
    };

    for (key, value) in object {
        if value.is_null() {
            continue;
        }
        if let Some(v) = value.as_str() {
            metadata.insert(key.clone(), v.to_string());
        } else {
            metadata.insert(key.clone(), value.to_string());
        }
    }

    metadata
}

fn execution_error_is_failure(error: Option<&str>) -> bool {
    error.map(str::trim).filter(|msg| !msg.is_empty()).is_some()
}

async fn load_execution_log_fills(
    pool: &PgPool,
    account_id: &str,
    dry_run: bool,
) -> Result<Vec<PersistedExecutionFill>> {
    let rows = sqlx::query_as::<
        _,
        (
            uuid::Uuid,
            String,
            String,
            String,
            String,
            String,
            bool,
            i64,
            Option<Decimal>,
            Decimal,
            DateTime<Utc>,
            Option<sqlx::types::Json<serde_json::Value>>,
        ),
    >(
        r#"
        SELECT
            intent_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            market_side,
            is_buy,
            filled_shares,
            avg_fill_price,
            limit_price,
            executed_at,
            metadata
        FROM agent_order_executions
        WHERE account_id = $1
          AND dry_run = $2
          AND filled_shares > 0
        ORDER BY executed_at ASC, id ASC
        "#,
    )
    .bind(account_id)
    .bind(dry_run)
    .fetch_all(pool)
    .await
    .map_err(|e| crate::error::PloyError::Internal(format!("load execution log fills: {}", e)))?;

    let mut fills = Vec::new();
    for (
        intent_id,
        agent_id,
        domain_raw,
        market_slug,
        token_id,
        side_raw,
        is_buy,
        filled_shares_raw,
        avg_fill_price,
        limit_price,
        executed_at,
        metadata_raw,
    ) in rows
    {
        let Some(domain) = parse_persisted_domain(&domain_raw) else {
            tracing::warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                domain = %domain_raw,
                "skipping execution-log row with unknown domain during restore"
            );
            continue;
        };
        let Some(side) = parse_persisted_side(&side_raw) else {
            tracing::warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                side = %side_raw,
                "skipping execution-log row with unknown side during restore"
            );
            continue;
        };
        let Ok(filled_shares) = u64::try_from(filled_shares_raw) else {
            tracing::warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                filled_shares = filled_shares_raw,
                "skipping execution-log row with invalid filled_shares during restore"
            );
            continue;
        };
        if filled_shares == 0 {
            continue;
        }
        let fill_price = avg_fill_price.unwrap_or(limit_price);
        if fill_price <= Decimal::ZERO {
            tracing::warn!(
                account_id = %account_id,
                intent_id = %intent_id,
                fill_price = %fill_price,
                "skipping execution-log row with non-positive fill price during restore"
            );
            continue;
        }

        fills.push(PersistedExecutionFill {
            intent_id,
            agent_id,
            domain,
            market_slug,
            token_id,
            side,
            is_buy,
            filled_shares,
            fill_price,
            executed_at,
            metadata: string_metadata_from_json(metadata_raw),
        });
    }

    Ok(fills)
}

async fn load_execution_log_outcomes(
    pool: &PgPool,
    account_id: &str,
    dry_run: bool,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<Vec<PersistedExecutionOutcome>> {
    let rows = sqlx::query_as::<_, (String, DateTime<Utc>, Option<String>)>(
        r#"
        SELECT
            agent_id,
            executed_at,
            error
        FROM agent_order_executions
        WHERE account_id = $1
          AND dry_run = $2
          AND executed_at >= $3
          AND executed_at < $4
        ORDER BY executed_at ASC, id ASC
        "#,
    )
    .bind(account_id)
    .bind(dry_run)
    .bind(window_start)
    .bind(window_end)
    .fetch_all(pool)
    .await
    .map_err(|e| {
        crate::error::PloyError::Internal(format!("load execution log outcomes: {}", e))
    })?;

    Ok(rows
        .into_iter()
        .map(|(agent_id, executed_at, error)| PersistedExecutionOutcome {
            agent_id,
            executed_at,
            is_failure: execution_error_is_failure(error.as_deref()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        execution_error_is_failure, parse_persisted_domain, parse_persisted_side,
        string_metadata_from_json,
    };
    use crate::platform::Domain;

    #[test]
    fn test_execution_error_is_failure_treats_blank_as_success() {
        assert!(execution_error_is_failure(Some("transport timeout")));
        assert!(!execution_error_is_failure(Some("   ")));
        assert!(!execution_error_is_failure(None));
    }

    #[test]
    fn test_parse_persisted_domain_supports_runtime_and_custom_encodings() {
        assert_eq!(parse_persisted_domain("Crypto"), Some(Domain::Crypto));
        assert_eq!(
            parse_persisted_domain("custom:42"),
            Some(Domain::Custom(42))
        );
        assert_eq!(parse_persisted_domain("Custom(7)"), Some(Domain::Custom(7)));
        assert_eq!(parse_persisted_domain(""), None);
        assert_eq!(parse_persisted_domain("custom:oops"), None);
    }

    #[test]
    fn test_parse_persisted_side_accepts_yes_no_aliases() {
        assert_eq!(parse_persisted_side("UP"), Some(crate::domain::Side::Up));
        assert_eq!(parse_persisted_side("NO"), Some(crate::domain::Side::Down));
        assert_eq!(parse_persisted_side("flat"), None);
    }

    #[test]
    fn test_string_metadata_from_json_normalizes_scalar_values() {
        let metadata = string_metadata_from_json(Some(sqlx::types::Json(serde_json::json!({
            "deployment_id": "deploy.crypto.15m",
            "signal_confidence": 0.73,
            "flag": true,
            "skip": null
        }))));
        assert_eq!(
            metadata.get("deployment_id").map(String::as_str),
            Some("deploy.crypto.15m")
        );
        assert_eq!(
            metadata.get("signal_confidence").map(String::as_str),
            Some("0.73")
        );
        assert_eq!(metadata.get("flag").map(String::as_str), Some("true"));
        assert!(!metadata.contains_key("skip"));
    }
}
