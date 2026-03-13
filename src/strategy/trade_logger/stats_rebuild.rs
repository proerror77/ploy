use super::{BucketStats, SymbolStats, TradeOutcome, TradeRecord, TradingStats};
use rust_decimal::Decimal;

pub(super) fn rebuild_stats(trades: &[TradeRecord]) -> TradingStats {
    let mut stats = TradingStats::default();

    for trade in trades {
        stats.total_trades += 1;
        stats.total_cost += trade.cost_usd;

        let pnl = trade.pnl_usd.unwrap_or(Decimal::ZERO);
        let is_closed = matches!(trade.outcome, TradeOutcome::Won | TradeOutcome::Lost);

        match &trade.outcome {
            TradeOutcome::Open => stats.open += 1,
            TradeOutcome::Won => {
                stats.wins += 1;
                stats.total_payout += trade.payout_usd.unwrap_or(Decimal::ZERO);
                stats.total_pnl += pnl;
            }
            TradeOutcome::Lost => {
                stats.losses += 1;
                stats.total_pnl += pnl;
            }
            TradeOutcome::ExitedEarly { .. } | TradeOutcome::Cancelled => {}
        }

        let symbol_stats = stats
            .by_symbol
            .entry(trade.symbol.clone())
            .or_insert_with(|| SymbolStats {
                symbol: trade.symbol.clone(),
                ..Default::default()
            });

        symbol_stats.total_trades += 1;
        symbol_stats.total_cost += trade.cost_usd;

        match &trade.outcome {
            TradeOutcome::Open => symbol_stats.open += 1,
            TradeOutcome::Won => {
                symbol_stats.wins += 1;
                symbol_stats.total_payout += trade.payout_usd.unwrap_or(Decimal::ZERO);
                symbol_stats.total_pnl += pnl;
            }
            TradeOutcome::Lost => {
                symbol_stats.losses += 1;
                symbol_stats.total_pnl += pnl;
            }
            TradeOutcome::ExitedEarly { .. } | TradeOutcome::Cancelled => {}
        }

        if symbol_stats
            .last_trade
            .is_none_or(|last| trade.timestamp > last)
        {
            symbol_stats.last_trade = Some(trade.timestamp);
        }

        update_bucket_stats(
            &mut stats.by_time_bucket,
            trade.context.time_bucket.as_ref(),
            trade.cost_usd,
            pnl,
            &trade.outcome,
            is_closed,
        );
        update_bucket_stats(
            &mut stats.by_strategy_mode,
            trade.context.strategy_mode.as_ref(),
            trade.cost_usd,
            pnl,
            &trade.outcome,
            is_closed,
        );
    }

    stats
}

fn update_bucket_stats(
    buckets: &mut std::collections::HashMap<String, BucketStats>,
    key: Option<&String>,
    cost_usd: Decimal,
    pnl: Decimal,
    outcome: &TradeOutcome,
    is_closed: bool,
) {
    let Some(key) = key else {
        return;
    };

    let bucket_stats = buckets.entry(key.clone()).or_default();
    bucket_stats.trades += 1;
    bucket_stats.cost += cost_usd;

    if is_closed {
        bucket_stats.pnl += pnl;
        match outcome {
            TradeOutcome::Won => bucket_stats.wins += 1,
            TradeOutcome::Lost => bucket_stats.losses += 1,
            TradeOutcome::Open | TradeOutcome::ExitedEarly { .. } | TradeOutcome::Cancelled => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rebuild_stats;
    use crate::strategy::trade_logger::{TradeContext, TradeOutcome, TradeRecord};
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;

    #[test]
    fn rebuild_stats_tracks_symbol_buckets_and_modes() {
        let trades = vec![
            TradeRecord {
                id: "t1".to_string(),
                timestamp: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
                symbol: "BTCUSDT".to_string(),
                event_slug: "btc-up".to_string(),
                condition_id: "cond-1".to_string(),
                direction: "up".to_string(),
                entry_price: dec!(0.45),
                shares: 10,
                cost_usd: dec!(4.5),
                momentum_pct: dec!(0.1),
                edge_pct: dec!(0.08),
                outcome: TradeOutcome::Won,
                payout_usd: Some(dec!(10)),
                pnl_usd: Some(dec!(5.5)),
                resolved_at: Some(Utc.timestamp_opt(1_700_000_060, 0).unwrap()),
                context: TradeContext {
                    time_bucket: Some("0-2".to_string()),
                    strategy_mode: Some("early_mispricing".to_string()),
                    ..Default::default()
                },
            },
            TradeRecord {
                id: "t2".to_string(),
                timestamp: Utc.timestamp_opt(1_700_000_120, 0).unwrap(),
                symbol: "BTCUSDT".to_string(),
                event_slug: "btc-down".to_string(),
                condition_id: "cond-2".to_string(),
                direction: "down".to_string(),
                entry_price: dec!(0.40),
                shares: 5,
                cost_usd: dec!(2),
                momentum_pct: dec!(-0.05),
                edge_pct: dec!(0.03),
                outcome: TradeOutcome::Open,
                payout_usd: None,
                pnl_usd: None,
                resolved_at: None,
                context: TradeContext {
                    time_bucket: Some("5-10".to_string()),
                    strategy_mode: Some("late_reversal".to_string()),
                    ..Default::default()
                },
            },
        ];

        let stats = rebuild_stats(&trades);

        assert_eq!(stats.total_trades, 2);
        assert_eq!(stats.wins, 1);
        assert_eq!(stats.open, 1);
        assert_eq!(stats.total_pnl, dec!(5.5));
        assert_eq!(stats.by_symbol["BTCUSDT"].total_trades, 2);
        assert_eq!(stats.by_time_bucket["0-2"].wins, 1);
        assert_eq!(stats.by_time_bucket["5-10"].trades, 1);
        assert_eq!(stats.by_strategy_mode["early_mispricing"].pnl, dec!(5.5));
        assert_eq!(stats.by_strategy_mode["late_reversal"].trades, 1);
    }
}
