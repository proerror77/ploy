use super::{
    Decimal, EntrySignal, EventContext, ExitReason, MarketUpdate, MomentumStrategy, Quote,
    SeriesMapping, StrategyAction, info,
};

impl MomentumStrategy {
    pub(super) fn handle_market_update(&mut self, update: &MarketUpdate) -> Vec<StrategyAction> {
        match update {
            MarketUpdate::BinancePrice {
                symbol,
                price,
                timestamp,
            } => self.on_binance_price(symbol, *price, *timestamp),
            MarketUpdate::PolymarketQuote { token_id, quote, .. } => {
                self.handle_polymarket_quote(token_id, quote)
            }
            MarketUpdate::EventDiscovered {
                event_id,
                series_id,
                up_token,
                down_token,
                end_time,
                ..
            } => {
                self.register_discovered_event(event_id, series_id, up_token, down_token, *end_time);
                Vec::new()
            }
            MarketUpdate::EventExpired { event_id } => {
                self.active_events.remove(event_id);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_polymarket_quote(&mut self, token_id: &str, quote: &Quote) -> Vec<StrategyAction> {
        let mut actions = Vec::new();

        if let Some((symbol, price, reason)) = self.exit_info_for_quote(token_id, quote) {
            actions.extend(self.create_exit_order(&symbol, price, reason));
        }

        for signal in self.pending_entry_signals_for_quote(token_id, quote) {
            actions.extend(self.create_entry_order(signal));
        }

        actions
    }

    fn exit_info_for_quote(
        &mut self,
        token_id: &str,
        quote: &Quote,
    ) -> Option<(String, Decimal, ExitReason)> {
        let take_profit_pct = self.config.take_profit_pct;
        let stop_loss_pct = self.config.stop_loss_pct;
        let trailing_stop_pct = self.config.trailing_stop_pct;
        let exit_before_resolution_secs = self.config.exit_before_resolution_secs as i64;

        if let Some(pos) = self.positions.get_mut(token_id) {
            return Self::exit_info_from_position(
                pos,
                quote,
                take_profit_pct,
                stop_loss_pct,
                trailing_stop_pct,
                exit_before_resolution_secs,
            );
        }

        for pos in self.positions.values_mut() {
            if pos.token_id == token_id {
                return Self::exit_info_from_position(
                    pos,
                    quote,
                    take_profit_pct,
                    stop_loss_pct,
                    trailing_stop_pct,
                    exit_before_resolution_secs,
                );
            }
        }

        None
    }

    fn exit_info_from_position(
        pos: &mut super::ActivePosition,
        quote: &Quote,
        take_profit_pct: Decimal,
        stop_loss_pct: Decimal,
        trailing_stop_pct: Decimal,
        exit_before_resolution_secs: i64,
    ) -> Option<(String, Decimal, ExitReason)> {
        let bid = quote.best_bid?;
        pos.update_high(bid);
        Self::check_exit_with_thresholds(
            pos,
            bid,
            take_profit_pct,
            stop_loss_pct,
            trailing_stop_pct,
            exit_before_resolution_secs,
        )
        .map(|reason| (pos.symbol.clone(), bid, reason))
    }

    fn pending_entry_signals_for_quote(&self, token_id: &str, quote: &Quote) -> Vec<EntrySignal> {
        self.pending_orders
            .values()
            .filter_map(|pending| {
                if !pending.is_entry {
                    return None;
                }

                let signal = pending.signal.as_ref()?;
                if signal.token_id != token_id {
                    return None;
                }

                let ask = quote.best_ask?;
                let mut updated_signal = signal.clone();
                updated_signal.pm_price = ask;
                updated_signal.edge = self.estimate_fair_value(signal.cex_move_pct) - ask;
                Some(updated_signal)
            })
            .collect()
    }

    fn register_discovered_event(
        &mut self,
        event_id: &str,
        series_id: &str,
        up_token: &str,
        down_token: &str,
        end_time: chrono::DateTime<chrono::Utc>,
    ) {
        for mapping in SeriesMapping::standard_mappings() {
            if mapping.series_ids.iter().any(|candidate| candidate == series_id) {
                let event = EventContext {
                    event_id: event_id.to_string(),
                    symbol: mapping.symbol.clone(),
                    up_token_id: up_token.to_string(),
                    down_token_id: down_token.to_string(),
                    end_time,
                };

                self.active_events.insert(event_id.to_string(), event);
                info!("Discovered event for {}: {}", mapping.symbol, event_id);
                break;
            }
        }
    }
}
