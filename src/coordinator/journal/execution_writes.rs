use rust_decimal::Decimal;
use std::future::Future;

use crate::coordinator::OrderIntent;
use crate::domain::{OrderRequest, OrderStatus};
use crate::strategy::executor::ExecutionResult;

use super::{metadata_decimal, ExecutionJournal};

struct ExecutionAnalysisContext {
    expected_price: Decimal,
    executed_price: Option<Decimal>,
    queue_delay_ms: Option<i64>,
    execution_latency_ms: Option<i64>,
    total_latency_ms: Option<i64>,
    expected_slippage_bps: Option<Decimal>,
    actual_slippage_bps: Option<Decimal>,
    status: String,
}

fn build_execution_analysis_context(
    intent: &OrderIntent,
    request: &OrderRequest,
    execution_result: Option<&ExecutionResult>,
    queue_delay_ms: Option<i64>,
) -> ExecutionAnalysisContext {
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
    let status = execution_result
        .map(|r| format!("{:?}", r.status))
        .unwrap_or_else(|| "Failed".to_string());

    ExecutionAnalysisContext {
        expected_price,
        executed_price,
        queue_delay_ms,
        execution_latency_ms,
        total_latency_ms,
        expected_slippage_bps,
        actual_slippage_bps,
        status,
    }
}

async fn join_execution_persistence_tasks<F1, F2, F3, F4>(
    write_execution: F1,
    write_analysis: F2,
    write_live_evaluation: F3,
    write_exit_reason: F4,
) where
    F1: Future<Output = ()>,
    F2: Future<Output = ()>,
    F3: Future<Output = ()>,
    F4: Future<Output = ()>,
{
    tokio::join!(
        write_execution,
        write_analysis,
        write_live_evaluation,
        write_exit_reason
    );
}

impl ExecutionJournal {
    pub(in crate::coordinator) async fn persist_execution(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&ExecutionResult>,
        error_message: Option<String>,
        queue_delay_ms: Option<i64>,
    ) {
        if self.pool.is_none() {
            return;
        }

        let analysis_context =
            build_execution_analysis_context(intent, request, result, queue_delay_ms);
        let config_hash = intent.metadata.get("config_hash").cloned();

        join_execution_persistence_tasks(
            self.persist_agent_order_execution(
                dry_run,
                intent,
                request,
                result,
                error_message.clone(),
            ),
            self.persist_execution_analysis(dry_run, intent, &analysis_context, config_hash),
            self.persist_live_strategy_evaluation(
                dry_run,
                intent,
                request,
                result,
                &analysis_context,
            ),
            async {
                if !intent.is_buy {
                    self.persist_exit_reason_execution(intent, result, error_message)
                        .await;
                }
            },
        )
        .await;
    }

    async fn persist_agent_order_execution(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        request: &OrderRequest,
        result: Option<&ExecutionResult>,
        error_message: Option<String>,
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

        let persist_result = sqlx::query(
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
        .bind(error_message)
        .bind(intent.created_at)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(error) = persist_result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist agent order execution"
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

        if let Err(error) = result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist exit reason execution"
            );
        }
    }

    async fn persist_execution_analysis(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        analysis_context: &ExecutionAnalysisContext,
        config_hash: Option<String>,
    ) {
        let Some(pool) = self.pool.as_ref() else {
            return;
        };

        let metadata =
            serde_json::to_value(&intent.metadata).unwrap_or_else(|_| serde_json::json!({}));

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
        .bind(analysis_context.expected_price)
        .bind(analysis_context.executed_price)
        .bind(analysis_context.expected_slippage_bps)
        .bind(analysis_context.actual_slippage_bps)
        .bind(analysis_context.queue_delay_ms)
        .bind(analysis_context.execution_latency_ms)
        .bind(analysis_context.total_latency_ms)
        .bind(&analysis_context.status)
        .bind(dry_run)
        .bind(config_hash)
        .bind(sqlx::types::Json(metadata))
        .execute(pool)
        .await;

        if let Err(error) = persist_result {
            tracing::warn!(
                agent_id = %intent.agent_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist execution analysis"
            );
        }
    }

    async fn persist_live_strategy_evaluation(
        &self,
        dry_run: bool,
        intent: &OrderIntent,
        request: &OrderRequest,
        execution_result: Option<&ExecutionResult>,
        analysis_context: &ExecutionAnalysisContext,
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
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let timeframe = intent
            .metadata
            .get("timeframe")
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
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
            "expected_slippage_bps": analysis_context.expected_slippage_bps.map(|value| value.to_string()),
            "actual_slippage_bps": analysis_context.actual_slippage_bps.map(|value| value.to_string()),
            "total_latency_ms": analysis_context.total_latency_ms,
            "dry_run": dry_run,
            "execution": execution_result.map(|result| serde_json::json!({
                "order_id": result.order_id.clone(),
                "status": format!("{:?}", result.status),
                "filled_shares": result.filled_shares,
                "avg_fill_price": result.avg_fill_price.map(|price| price.to_string()),
                "elapsed_ms": result.elapsed_ms
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

        if let Err(error) = insert {
            tracing::warn!(
                account_id = %self.account_id,
                intent_id = %intent.intent_id,
                error = %error,
                "failed to persist live strategy evaluation evidence"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::join_execution_persistence_tasks;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::{
        sync::Barrier,
        time::{sleep, Duration},
    };

    async fn tracked_write(
        barrier: Arc<Barrier>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        completed: Arc<AtomicUsize>,
    ) {
        let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        loop {
            let observed = max_in_flight.load(Ordering::SeqCst);
            if current <= observed {
                break;
            }
            if max_in_flight
                .compare_exchange(observed, current, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }

        barrier.wait().await;
        sleep(Duration::from_millis(5)).await;

        in_flight.fetch_sub(1, Ordering::SeqCst);
        completed.fetch_add(1, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn join_execution_persistence_tasks_polls_independent_writes_concurrently() {
        let barrier = Arc::new(Barrier::new(4));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));

        join_execution_persistence_tasks(
            tracked_write(
                barrier.clone(),
                in_flight.clone(),
                max_in_flight.clone(),
                completed.clone(),
            ),
            tracked_write(
                barrier.clone(),
                in_flight.clone(),
                max_in_flight.clone(),
                completed.clone(),
            ),
            tracked_write(
                barrier.clone(),
                in_flight.clone(),
                max_in_flight.clone(),
                completed.clone(),
            ),
            tracked_write(barrier, in_flight, max_in_flight.clone(), completed.clone()),
        )
        .await;

        assert_eq!(completed.load(Ordering::SeqCst), 4);
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 4);
    }
}
