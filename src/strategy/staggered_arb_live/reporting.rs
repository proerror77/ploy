use super::*;

impl StaggeredArbAdapter {
    fn summarize_gate_counts(
        counts: &HashMap<String, u64>,
        include_reasons: Option<&[&str]>,
        exclude_reasons: &[&str],
        limit: usize,
    ) -> String {
        let mut ranked: Vec<_> = counts
            .iter()
            .filter(|(reason, count)| {
                let reason = reason.as_str();
                let include_match =
                    include_reasons.map_or(true, |included| included.contains(&reason));
                let exclude_match = exclude_reasons.contains(&reason);
                **count > 0 && include_match && !exclude_match
            })
            .map(|(reason, count)| (reason.as_str(), *count))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

        if ranked.is_empty() {
            return "none".to_string();
        }

        ranked
            .into_iter()
            .take(limit)
            .map(|(reason, count)| format!("{}:{}", reason, count))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn summarize_symbol_gate_counts(
        counts_by_symbol: &HashMap<String, HashMap<String, u64>>,
        symbols: &[String],
        include_reasons: Option<&[&str]>,
        exclude_reasons: &[&str],
        per_symbol_limit: usize,
    ) -> String {
        let mut parts = Vec::new();
        for symbol in symbols {
            let Some(counts) = counts_by_symbol.get(symbol) else {
                continue;
            };
            let summary = Self::summarize_gate_counts(
                counts,
                include_reasons,
                exclude_reasons,
                per_symbol_limit,
            );
            if !summary.is_empty() {
                parts.push(format!("{}:[{}]", symbol, summary));
            }
        }

        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(";")
        }
    }

    pub(super) fn build_summary(&self) -> String {
        let total = self.closed_trades.len();
        let wins = self
            .closed_trades
            .iter()
            .filter(|t| t.pnl > Decimal::ZERO)
            .count();
        let win_rate = if total > 0 {
            wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let avg_pnl = if total > 0 {
            self.closed_trades.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(total as u64)
        } else {
            Decimal::ZERO
        };
        let open = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .count();
        let entry_timing_reasons = [
            "before_event_start",
            "entry_window_expired",
            "time_remaining_too_low",
        ];
        let entry_timing_gates = Self::summarize_gate_counts(
            &self.entry_reject_counts,
            Some(&entry_timing_reasons),
            &["entry_accepted"],
            3,
        );
        let entry_signal_gates = Self::summarize_gate_counts(
            &self.entry_reject_counts,
            None,
            &[
                "entry_accepted",
                "before_event_start",
                "entry_window_expired",
                "time_remaining_too_low",
            ],
            3,
        );
        let entry_signal_by_symbol = Self::summarize_symbol_gate_counts(
            &self.entry_reject_counts_by_symbol,
            &self.config.backtest_config.symbols,
            None,
            &[
                "entry_accepted",
                "before_event_start",
                "entry_window_expired",
                "time_remaining_too_low",
            ],
            1,
        );
        let leg2_gates = Self::summarize_gate_counts(&self.leg2_skip_counts, None, &[], 3);
        let leg2_by_symbol = Self::summarize_symbol_gate_counts(
            &self.leg2_skip_counts_by_symbol,
            &self.config.backtest_config.symbols,
            None,
            &[],
            1,
        );

        format!(
            "[STAG-ARB] equity=${:.2} trades={} win_rate={:.0}% avg_pnl=${:.4} open={} entry_timing_gates={} entry_signal_gates={} entry_signal_by_symbol={} leg2_gates={} leg2_by_symbol={}",
            self.equity,
            total,
            win_rate,
            avg_pnl,
            open,
            entry_timing_gates,
            entry_signal_gates,
            entry_signal_by_symbol,
            leg2_gates,
            leg2_by_symbol,
        )
    }

    pub(super) fn strategy_state(&self) -> StrategyStateInfo {
        let open_count = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .count();
        let realized_pnl: Decimal = self.closed_trades.iter().map(|t| t.pnl).sum();
        let total_exposure: Decimal = self
            .positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .map(|p| p.leg1_price * Decimal::from(p.leg1_shares))
            .sum();

        let mut metrics = HashMap::new();
        metrics.insert("equity".to_string(), format!("{:.2}", self.equity));
        metrics.insert(
            "total_trades".to_string(),
            self.closed_trades.len().to_string(),
        );
        let merges = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason.contains("merge"))
            .count();
        let forced = self
            .closed_trades
            .iter()
            .filter(|t| t.exit_reason.contains("forced"))
            .count();
        metrics.insert("merge_count".to_string(), merges.to_string());
        metrics.insert("forced_count".to_string(), forced.to_string());
        metrics.insert("dry_run".to_string(), self.dry_run.to_string());
        for (k, v) in self.entry_reject_counts.iter() {
            metrics.insert(format!("entry_gate_{}", k), v.to_string());
        }
        for (symbol, counts) in self.entry_reject_counts_by_symbol.iter() {
            for (reason, count) in counts {
                metrics.insert(
                    format!("entry_gate_{}_{}", symbol, reason),
                    count.to_string(),
                );
            }
        }
        for (k, v) in self.leg2_skip_counts.iter() {
            metrics.insert(format!("leg2_gate_{}", k), v.to_string());
        }
        for (symbol, counts) in self.leg2_skip_counts_by_symbol.iter() {
            for (reason, count) in counts {
                metrics.insert(
                    format!("leg2_gate_{}_{}", symbol, reason),
                    count.to_string(),
                );
            }
        }

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if open_count > 0 {
                "trading".to_string()
            } else {
                "monitoring".to_string()
            },
            enabled: true,
            active: open_count > 0,
            position_count: open_count,
            pending_order_count: self.live_orders.len(),
            total_exposure,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: realized_pnl,
            last_update: Utc::now(),
            metrics,
        }
    }

    pub(super) fn exported_positions(&self) -> Vec<PositionInfo> {
        self.positions
            .iter()
            .filter(|p| p.state == PaperPositionState::Leg1Filled)
            .map(|p| {
                let token_id = match p.leg1_direction {
                    Direction::Up => format!("{}_up", p.symbol),
                    Direction::Down => format!("{}_down", p.symbol),
                };
                let side = match p.leg1_direction {
                    Direction::Up => Side::Up,
                    Direction::Down => Side::Down,
                };
                PositionInfo::new(token_id, side, p.leg1_shares, p.leg1_price, self.id.clone())
            })
            .collect()
    }

    pub(super) fn is_strategy_active(&self) -> bool {
        self.positions
            .iter()
            .any(|p| p.state == PaperPositionState::Leg1Filled)
    }

    pub(super) fn shutdown_actions(&self) -> Vec<StrategyAction> {
        let summary = self.build_summary();
        info!("[STAG-ARB] Shutdown: {}", summary);
        vec![StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                format!("Shutdown: {}", summary),
            ),
        }]
    }

    pub(super) fn reset_runtime_state(&mut self) {
        self.positions.clear();
        self.closed_trades.clear();
        self.equity = self.initial_capital;
        self.cooldowns.clear();
        self.event_trade_counts.clear();
        self.active_windows.clear();
        self.spot_prices.clear();
        self.pm_asks_by_event.clear();
        self.pm_quote_state_by_event.clear();
        self.binance_l2_obi_5.clear();
        self.binance_l2_obi_prev_5.clear();
        self.binance_l2_obi_ts.clear();
        self.token_to_quote_route.clear();
        self.last_summary = None;
        self.fixed_amount_overage_warned = false;
    }
}
