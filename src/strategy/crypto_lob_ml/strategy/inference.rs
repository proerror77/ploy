use super::*;

impl CryptoLobMlStrategy {
    fn max_time_remaining_for(&self, horizon: &str) -> u64 {
        match core::normalize_timeframe(horizon).as_str() {
            "15m" => self.cfg.max_time_remaining_secs_15m,
            _ => self.cfg.max_time_remaining_secs_5m,
        }
    }

    fn window_start_price(&self, spot: &SpotPrice, horizon: &str) -> Decimal {
        let window_secs = core::event_window_secs_for_horizon(horizon);
        spot.price_secs_ago(window_secs).unwrap_or(spot.price)
    }

    fn quote_mid(&self, token_id: &str) -> Option<Decimal> {
        self.quotes
            .get(token_id)
            .and_then(|quote| quote.mid_price())
    }

    pub(super) fn should_emit_inference_log(&mut self, event_id: &str, now: DateTime<Utc>) -> bool {
        match self.last_logged_at.get(event_id) {
            None => {
                self.last_logged_at.insert(event_id.to_string(), now);
                true
            }
            Some(last)
                if now.signed_duration_since(*last).num_seconds()
                    >= INFERENCE_LOG_INTERVAL_SECS =>
            {
                self.last_logged_at.insert(event_id.to_string(), now);
                true
            }
            _ => false,
        }
    }

    pub(super) fn evaluate_event(
        &mut self,
        now: DateTime<Utc>,
        event: &LobMlTrackedEvent,
    ) -> Result<Option<LobMlInferenceSummary>> {
        let remaining_secs = event.end_time.signed_duration_since(now).num_seconds();
        if remaining_secs < self.cfg.min_time_remaining_secs as i64 {
            self.last_reason = Some(format!(
                "{}:{} below_min_remaining",
                event.symbol, event.horizon
            ));
            return Ok(None);
        }
        if remaining_secs > self.max_time_remaining_for(&event.horizon) as i64 {
            self.last_reason = Some(format!(
                "{}:{} above_max_remaining",
                event.symbol, event.horizon
            ));
            return Ok(None);
        }
        if self.cfg.require_price_to_beat && event.price_to_beat.is_none() {
            self.last_reason = Some(format!(
                "{}:{} missing_price_to_beat",
                event.symbol, event.horizon
            ));
            return Ok(None);
        }

        let Some(spot) = self.spot_prices.get(&event.symbol) else {
            self.last_reason = Some(format!("{}:{} waiting_spot", event.symbol, event.horizon));
            return Ok(None);
        };
        let Some(l2) = self.l2_by_symbol.get(&event.symbol) else {
            self.last_reason = Some(format!("{}:{} waiting_l2", event.symbol, event.horizon));
            return Ok(None);
        };

        let l2_age_secs = now.signed_duration_since(l2.timestamp).num_seconds();
        if l2_age_secs > self.cfg.max_lob_snapshot_age_secs as i64 {
            self.last_reason = Some(format!("{}:{} stale_l2", event.symbol, event.horizon));
            return Ok(None);
        }

        let price_to_beat = event.price_to_beat.unwrap_or(spot.price);
        let distance_to_beat = if spot.price > Decimal::ZERO {
            (price_to_beat - spot.price) / spot.price
        } else {
            Decimal::ZERO
        };
        let second_bucket = chrono::DateTime::<Utc>::from_timestamp(spot.timestamp.timestamp(), 0)
            .unwrap_or(spot.timestamp);
        let entry_key = format!(
            "{}|{}",
            event.symbol,
            core::normalize_timeframe(&event.horizon)
        );
        core::push_sequence_snapshot(
            &mut self.sequence_cache,
            &entry_key,
            SequenceSnapshot {
                ts: second_bucket,
                obi_5: l2.obi_5,
                obi_10: l2.obi_10,
                spread_bps: l2.spread_bps,
                bid_volume_5: l2.bid_volume_5,
                ask_volume_5: l2.ask_volume_5,
                momentum_1s: spot.momentum(1).unwrap_or(Decimal::ZERO),
                momentum_5s: spot.momentum(5).unwrap_or(Decimal::ZERO),
                spot_price: spot.price,
                remaining_secs: Decimal::from(remaining_secs.max(0)),
                price_to_beat,
                distance_to_beat,
            },
        );

        let Some(sequence_input) = core::build_sequence(
            &self.sequence_cache,
            &entry_key,
            &event.horizon,
            &self.cfg.feature_offsets,
            &self.cfg.feature_scales,
        ) else {
            self.last_reason = Some(format!(
                "{}:{} warming_sequence",
                event.symbol, event.horizon
            ));
            return Ok(None);
        };

        let p_gbm_anchor = core::estimate_p_up_gbm_anchor(
            spot.price,
            self.window_start_price(spot, &event.horizon),
            event.price_to_beat,
            spot.volatility(60),
            remaining_secs,
            self.cfg.oracle_lag_buffer_secs,
        );

        Ok(Some(LobMlInferenceSummary {
            event_id: event.event_id.clone(),
            symbol: event.symbol.clone(),
            horizon: core::normalize_timeframe(&event.horizon),
            p_gbm_anchor,
            remaining_secs,
            sequence_snapshots: sequence_input.len() / core::SEQ_FEATURE_DIM,
            up_mid: self.quote_mid(&event.up_token),
            down_mid: self.quote_mid(&event.down_token),
            at: now,
        }))
    }

    pub(super) fn inference_event(
        &self,
        event: &LobMlTrackedEvent,
        summary: &LobMlInferenceSummary,
    ) -> StrategyEvent {
        let message = format!(
            "crypto_lob_ml {} {} ready p_up_gbm_anchor={}",
            summary.symbol, summary.horizon, summary.p_gbm_anchor
        );
        let mut strategy_event = StrategyEvent::new(
            StrategyEventType::Custom("crypto_lob_ml_inference".to_string()),
            message,
        )
        .with_data("event_id", &summary.event_id)
        .with_data("series_id", &event.series_id)
        .with_data("symbol", &summary.symbol)
        .with_data("horizon", &summary.horizon)
        .with_data("remaining_secs", summary.remaining_secs.to_string())
        .with_data("sequence_snapshots", summary.sequence_snapshots.to_string())
        .with_data("p_gbm_anchor", summary.p_gbm_anchor.to_string());

        if let Some(price_to_beat) = event.price_to_beat {
            strategy_event = strategy_event.with_data("price_to_beat", price_to_beat.to_string());
        }
        if let Some(title) = &event.title {
            strategy_event = strategy_event.with_data("title", title);
        }
        if let Some(up_mid) = summary.up_mid {
            strategy_event = strategy_event.with_data("up_mid", up_mid.to_string());
        }
        if let Some(down_mid) = summary.down_mid {
            strategy_event = strategy_event.with_data("down_mid", down_mid.to_string());
        }

        strategy_event
    }
}
