use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{CryptoRlPolicyStrategy, core};
use crate::error::Result;
use crate::strategy::crypto::series_info;
use crate::strategy::traits::{MarketUpdate, StrategyEvent, StrategyEventType};

const SIGNAL_LOG_INTERVAL_SECS: i64 = 30;

#[derive(Debug, Clone)]
pub(super) struct RlTrackedEvent {
    pub(super) event_id: String,
    pub(super) series_id: String,
    pub(super) symbol: String,
    pub(super) up_token: String,
    pub(super) down_token: String,
    pub(super) end_time: DateTime<Utc>,
    pub(super) price_to_beat: Option<Decimal>,
    pub(super) title: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct RlSignalSummary {
    pub(super) event_id: String,
    pub(super) symbol: String,
    pub(super) series_id: String,
    pub(super) action: core::DiscreteAction,
    pub(super) policy_source: String,
    pub(super) desired_shares: u64,
    pub(super) up_ask: Decimal,
    pub(super) down_ask: Decimal,
    pub(super) remaining_secs: i64,
    pub(super) obs_version: u32,
    pub(super) momentum_1s: Decimal,
    pub(super) momentum_5s: Decimal,
    pub(super) at: DateTime<Utc>,
}

impl CryptoRlPolicyStrategy {
    pub(super) fn track_event(&mut self, update: &MarketUpdate) {
        let MarketUpdate::EventDiscovered {
            event_id,
            series_id,
            up_token,
            down_token,
            end_time,
            price_to_beat,
            title,
            ..
        } = update
        else {
            return;
        };

        let Some(info) = series_info(series_id) else {
            return;
        };
        if !self.symbols.iter().any(|symbol| symbol == info.symbol) {
            return;
        }

        self.active_events.insert(
            event_id.clone(),
            RlTrackedEvent {
                event_id: event_id.clone(),
                series_id: series_id.clone(),
                symbol: info.symbol.to_string(),
                up_token: up_token.clone(),
                down_token: down_token.clone(),
                end_time: *end_time,
                price_to_beat: *price_to_beat,
                title: title.clone(),
            },
        );
    }

    pub(super) fn should_emit_signal_log(&mut self, event_id: &str, now: DateTime<Utc>) -> bool {
        match self.last_logged_at.get(event_id) {
            None => {
                self.last_logged_at.insert(event_id.to_string(), now);
                true
            }
            Some(last)
                if now.signed_duration_since(*last).num_seconds() >= SIGNAL_LOG_INTERVAL_SECS =>
            {
                self.last_logged_at.insert(event_id.to_string(), now);
                true
            }
            _ => false,
        }
    }

    pub(super) fn action_label(action: core::DiscreteAction) -> &'static str {
        match action {
            core::DiscreteAction::Hold => "hold",
            core::DiscreteAction::BuyUp => "buy_up",
            core::DiscreteAction::BuyDown => "buy_down",
            core::DiscreteAction::SellPosition => "sell_position",
            core::DiscreteAction::EnterHedge => "enter_hedge",
        }
    }

    pub(super) fn evaluate_event(
        &mut self,
        now: DateTime<Utc>,
        event: &RlTrackedEvent,
    ) -> Result<Option<RlSignalSummary>> {
        let remaining_secs = event.end_time.signed_duration_since(now).num_seconds();
        if remaining_secs < self.cfg.min_time_remaining_secs as i64 {
            self.last_reason = Some(format!("{} below_min_remaining", event.symbol));
            return Ok(None);
        }
        if remaining_secs > self.cfg.max_time_remaining_secs as i64 {
            self.last_reason = Some(format!("{} above_max_remaining", event.symbol));
            return Ok(None);
        }

        let Some(spot) = self.spot_prices.get(&event.symbol) else {
            self.last_reason = Some(format!("{} waiting_spot", event.symbol));
            return Ok(None);
        };
        let Some(l2) = self.l2_by_symbol.get(&event.symbol) else {
            self.last_reason = Some(format!("{} waiting_l2", event.symbol));
            return Ok(None);
        };
        if now.signed_duration_since(l2.timestamp).num_seconds()
            > self.cfg.max_lob_snapshot_age_secs as i64
        {
            self.last_reason = Some(format!("{} stale_l2", event.symbol));
            return Ok(None);
        }

        let Some(up_quote) = self.quotes.get(&event.up_token) else {
            self.last_reason = Some(format!("{} waiting_up_quote", event.symbol));
            return Ok(None);
        };
        let Some(down_quote) = self.quotes.get(&event.down_token) else {
            self.last_reason = Some(format!("{} waiting_down_quote", event.symbol));
            return Ok(None);
        };

        #[cfg(feature = "onnx")]
        let (up_bid, up_ask, down_bid, down_ask) = match (
            up_quote.best_bid,
            up_quote.best_ask,
            down_quote.best_bid,
            down_quote.best_ask,
        ) {
            (Some(up_bid), Some(up_ask), Some(down_bid), Some(down_ask))
                if up_ask > Decimal::ZERO && down_ask > Decimal::ZERO =>
            {
                (up_bid, up_ask, down_bid, down_ask)
            }
            _ => {
                self.last_reason = Some(format!("{} incomplete_quotes", event.symbol));
                return Ok(None);
            }
        };
        #[cfg(not(feature = "onnx"))]
        let (_up_bid, up_ask, _down_bid, down_ask) = match (
            up_quote.best_bid,
            up_quote.best_ask,
            down_quote.best_bid,
            down_quote.best_ask,
        ) {
            (Some(up_bid), Some(up_ask), Some(down_bid), Some(down_ask))
                if up_ask > Decimal::ZERO && down_ask > Decimal::ZERO =>
            {
                (up_bid, up_ask, down_bid, down_ask)
            }
            _ => {
                self.last_reason = Some(format!("{} incomplete_quotes", event.symbol));
                return Ok(None);
            }
        };

        let momentum_1s = spot.momentum(1).unwrap_or(Decimal::ZERO);
        let momentum_5s = spot.momentum(5).unwrap_or(Decimal::ZERO);

        #[cfg(feature = "onnx")]
        let mut policy_source = "rule_based".to_string();
        #[cfg(not(feature = "onnx"))]
        let policy_source = "rule_based".to_string();

        #[cfg(feature = "onnx")]
        let action = if let Some(model) = &self.policy_model {
            let obs = if self.cfg.observation_version == 2 {
                core::build_observation_v2(
                    self.cfg.default_shares,
                    self.cfg.max_time_remaining_secs,
                    now,
                    spot.price,
                    momentum_1s,
                    momentum_5s,
                    l2,
                    up_bid,
                    up_ask,
                    down_bid,
                    down_ask,
                    None,
                    remaining_secs,
                    l2.obi_1,
                    l2.obi_2,
                    l2.obi_3,
                    l2.obi_20,
                )
            } else {
                core::build_observation_v1(
                    self.cfg.default_shares,
                    self.cfg.max_time_remaining_secs,
                    now,
                    spot.price,
                    momentum_1s,
                    momentum_5s,
                    l2,
                    up_bid,
                    up_ask,
                    down_bid,
                    down_ask,
                    None,
                    remaining_secs,
                )
            };

            match model.predict(&obs).ok().and_then(|output| {
                core::action_from_policy_output(self.cfg.policy_output.as_str(), &output)
            }) {
                Some(action) => {
                    policy_source = "onnx".to_string();
                    action
                }
                None => core::rule_based_policy(false, Some(up_ask + down_ask), momentum_1s, None),
            }
        } else {
            core::rule_based_policy(false, Some(up_ask + down_ask), momentum_1s, None)
        };

        #[cfg(not(feature = "onnx"))]
        let action = core::rule_based_policy(false, Some(up_ask + down_ask), momentum_1s, None);

        let discrete = action.to_discrete();
        if matches!(
            discrete,
            core::DiscreteAction::Hold | core::DiscreteAction::SellPosition
        ) {
            self.last_reason = Some(format!("{} {}", event.symbol, Self::action_label(discrete)));
            return Ok(None);
        }

        match discrete {
            core::DiscreteAction::BuyUp if up_ask > self.cfg.max_entry_price => {
                self.last_reason = Some(format!("{} buy_up_above_max_entry", event.symbol));
                return Ok(None);
            }
            core::DiscreteAction::BuyDown if down_ask > self.cfg.max_entry_price => {
                self.last_reason = Some(format!("{} buy_down_above_max_entry", event.symbol));
                return Ok(None);
            }
            core::DiscreteAction::EnterHedge
                if up_ask > self.cfg.max_entry_price
                    || down_ask > self.cfg.max_entry_price
                    || up_ask + down_ask >= dec!(1.0) =>
            {
                self.last_reason = Some(format!("{} hedge_gate_reject", event.symbol));
                return Ok(None);
            }
            _ => {}
        }

        Ok(Some(RlSignalSummary {
            event_id: event.event_id.clone(),
            symbol: event.symbol.clone(),
            series_id: event.series_id.clone(),
            action: discrete,
            policy_source,
            desired_shares: core::compute_shares(&action, self.cfg.default_shares),
            up_ask,
            down_ask,
            remaining_secs,
            obs_version: self.cfg.observation_version,
            momentum_1s,
            momentum_5s,
            at: now,
        }))
    }

    pub(super) fn signal_event(
        &self,
        event: &RlTrackedEvent,
        signal: &RlSignalSummary,
    ) -> StrategyEvent {
        StrategyEvent::new(
            StrategyEventType::Custom("crypto_rl_policy_signal".to_string()),
            format!(
                "crypto_rl_policy {} {}",
                signal.symbol,
                Self::action_label(signal.action)
            ),
        )
        .with_data("event_id", &signal.event_id)
        .with_data("series_id", &signal.series_id)
        .with_data("symbol", &signal.symbol)
        .with_data("action", Self::action_label(signal.action))
        .with_data("policy_source", &signal.policy_source)
        .with_data("desired_shares", signal.desired_shares.to_string())
        .with_data("up_ask", signal.up_ask.to_string())
        .with_data("down_ask", signal.down_ask.to_string())
        .with_data("remaining_secs", signal.remaining_secs.to_string())
        .with_data("obs_version", signal.obs_version.to_string())
        .with_data("momentum_1s", signal.momentum_1s.to_string())
        .with_data("momentum_5s", signal.momentum_5s.to_string())
        .with_data(
            "policy_model_version",
            self.cfg.policy_model_version.clone().unwrap_or_default(),
        )
        .with_data("title", event.title.clone().unwrap_or_default())
        .with_data("at", signal.at.to_rfc3339())
        .with_data(
            "price_to_beat",
            event
                .price_to_beat
                .map(|price| price.to_string())
                .unwrap_or_default(),
        )
    }
}
