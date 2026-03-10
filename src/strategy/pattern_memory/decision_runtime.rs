use super::*;
use crate::domain::Quote;
use crate::strategy::multi_outcome::{ExpectedValue, POLYMARKET_FEE_RATE};

impl PatternMemoryStrategy {
    pub(super) fn symbol_for_series(&self, series_id: &str) -> Option<&str> {
        self.symbol_by_series.get(series_id).map(|s| s.as_str())
    }

    fn direction_from_p_up(p_up: f64) -> Side {
        if p_up >= 0.5 { Side::Up } else { Side::Down }
    }

    fn confidence_from_p_up(p_up: f64) -> f64 {
        p_up.max(1.0 - p_up)
    }

    fn required_return(spot: Decimal, price_to_beat: Decimal) -> Option<f64> {
        if spot <= Decimal::ZERO {
            return None;
        }
        let rr = (price_to_beat - spot) / spot;
        rr.to_f64()
    }

    pub(super) fn kline_return(open: Decimal, close: Decimal) -> Option<f64> {
        if open <= Decimal::ZERO {
            return None;
        }
        ((close - open) / open).to_f64()
    }

    fn in_cooldown(&self, symbol: &str, now: DateTime<Utc>) -> bool {
        let Some(last) = self.cooldowns.get(symbol) else {
            return false;
        };
        now.signed_duration_since(*last).num_seconds() < self.cfg.trade.cooldown_secs
    }

    fn pick_event<'a>(&'a self, symbol: &str, now: DateTime<Utc>) -> Option<&'a EventState> {
        let events = self.events.get(symbol)?;
        let mut best: Option<(&EventState, i64)> = None;
        for ev in events.values() {
            let rem = (ev.end_time - now).num_seconds();
            if rem <= 0 {
                continue;
            }
            if rem < self.cfg.timing.min_remaining_secs {
                continue;
            }
            let diff = (rem - self.cfg.timing.target_remaining_secs).abs();
            if diff > self.cfg.timing.tolerance_secs {
                continue;
            }
            match best {
                None => best = Some((ev, diff)),
                Some((_b, best_diff)) if diff < best_diff => best = Some((ev, diff)),
                _ => {}
            }
        }
        best.map(|(ev, _)| ev)
    }

    pub(super) fn update_quote(
        &mut self,
        token_id: &str,
        side: Side,
        quote: &Quote,
        ts: DateTime<Utc>,
    ) {
        self.quotes.insert(
            token_id.to_string(),
            QuoteState {
                side,
                best_bid: quote.best_bid,
                best_ask: quote.best_ask,
                ts,
            },
        );
    }

    fn ev_for_side(entry_price: Decimal, p_win: f64) -> Option<ExpectedValue> {
        let p = Decimal::from_f64(p_win)?;
        Some(ExpectedValue::calculate(
            entry_price,
            p,
            Some(POLYMARKET_FEE_RATE),
        ))
    }

    fn should_trade_posterior(&self, post: &Posterior) -> bool {
        let conf = Self::confidence_from_p_up(post.p_up);
        post.matches >= self.cfg.pattern.min_matches
            && post.n_eff >= self.cfg.pattern.min_n_eff
            && conf >= self.cfg.pattern.min_confidence
    }

    fn filter_15m_ok(&self, symbol: &str, required_return: f64) -> (bool, f64, Posterior) {
        if !self.cfg.filter_15m.enabled {
            return (
                true,
                1.0,
                Posterior {
                    p_up: 0.5,
                    up_w: 0.0,
                    down_w: 0.0,
                    n_eff: 0.0,
                    matches: 0,
                },
            );
        }

        let Some(mem) = self.mem_15m.get(symbol) else {
            return (
                false,
                0.0,
                Posterior {
                    p_up: 0.5,
                    up_w: 0.0,
                    down_w: 0.0,
                    n_eff: 0.0,
                    matches: 0,
                },
            );
        };

        let post = mem.posterior_for_required_return(
            required_return,
            self.cfg.pattern.corr_threshold,
            self.cfg.pattern.alpha,
            self.cfg.pattern.beta,
            self.cfg.pattern.age_decay_lambda,
        );
        let conf = Self::confidence_from_p_up(post.p_up);
        let ok = post.n_eff >= self.cfg.filter_15m.min_n_eff
            && conf >= self.cfg.filter_15m.min_confidence;
        (ok, conf, post)
    }

    pub(super) async fn maybe_trade_on_5m_close(
        &mut self,
        symbol: &str,
        spot: Decimal,
        now: DateTime<Utc>,
    ) -> Option<Vec<StrategyAction>> {
        if !self.enabled {
            return None;
        }
        if self.in_cooldown(symbol, now) {
            return None;
        }

        let has_events = self
            .events
            .get(symbol)
            .map(|m| !m.is_empty())
            .unwrap_or(false);
        if !has_events {
            return None;
        }

        let event = match self.pick_event(symbol, now) {
            Some(ev) => ev.clone(),
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: no matching event (target_rem={}±{}s, min_rem={}s)",
                            symbol,
                            self.cfg.timing.target_remaining_secs,
                            self.cfg.timing.tolerance_secs,
                            self.cfg.timing.min_remaining_secs
                        ),
                    ),
                }]);
            }
        };

        if self.traded_events.contains(&event.event_id) {
            return None;
        }

        let rem = (event.end_time - now).num_seconds();
        let (price_to_beat, required_return) = match event.price_to_beat {
            Some(thr) => (Some(thr), Self::required_return(spot, thr)?),
            None => (None, 0.0),
        };

        let mem5 = match self.mem_5m.get(symbol) {
            Some(m) => m,
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!("{} 5m close: 5m memory not ready (rem={}s)", symbol, rem),
                    ),
                }]);
            }
        };

        let samples5 = mem5.samples_len();
        let post5 = mem5.posterior_for_required_return(
            required_return,
            self.cfg.pattern.corr_threshold,
            self.cfg.pattern.alpha,
            self.cfg.pattern.beta,
            self.cfg.pattern.age_decay_lambda,
        );

        let dir5 = Self::direction_from_p_up(post5.p_up);
        let conf5 = Self::confidence_from_p_up(post5.p_up);
        let p_win_dir5 = match dir5 {
            Side::Up => post5.p_up,
            Side::Down => 1.0 - post5.p_up,
        };

        let (filter_ok, conf15, post15) = self.filter_15m_ok(symbol, required_return);
        let dir15 = Self::direction_from_p_up(post15.p_up);
        let dir_ok = if self.cfg.filter_15m.enabled {
            dir15 == dir5
        } else {
            true
        };

        self.last_decision.insert(
            symbol.to_string(),
            LastDecision {
                event_id: event.event_id.clone(),
                symbol: symbol.to_string(),
                p_up: post5.p_up,
                conf: conf5,
                required_return,
                matches: post5.matches,
                n_eff: post5.n_eff,
                tf15_conf: if self.cfg.filter_15m.enabled {
                    Some(conf15)
                } else {
                    None
                },
                tf15_dir_ok: if self.cfg.filter_15m.enabled {
                    Some(dir_ok)
                } else {
                    None
                },
                at: now,
            },
        );

        let filter_desc = if self.cfg.filter_15m.enabled {
            format!(
                "15m_ok={} 15m_conf={:.1}% 15m_dir={} 15m_dir_ok={}",
                filter_ok,
                conf15 * 100.0,
                dir15,
                dir_ok
            )
        } else {
            "15m_filter=off".to_string()
        };

        if !self.should_trade_posterior(&post5) {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} p_win={:.1}% conf5={:.1}% n_eff5={:.2} matches5={} samples5={} r_req={:.3}% => SKIP: evidence (need matches>={} n_eff>={:.2} conf>={:.1}%)",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        p_win_dir5 * 100.0,
                        conf5 * 100.0,
                        post5.n_eff,
                        post5.matches,
                        samples5,
                        required_return * 100.0,
                        self.cfg.pattern.min_matches,
                        self.cfg.pattern.min_n_eff,
                        self.cfg.pattern.min_confidence * 100.0,
                    ),
                ),
            }]);
        }

        if self.cfg.filter_15m.enabled {
            if !filter_ok {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} => SKIP: 15m filter (need n_eff>={:.2} conf>={:.1}%, got n_eff={:.2} conf={:.1}%)",
                            symbol,
                            event.event_id,
                            rem,
                            filter_desc,
                            dir5,
                            self.cfg.filter_15m.min_n_eff,
                            self.cfg.filter_15m.min_confidence * 100.0,
                            post15.n_eff,
                            conf15 * 100.0,
                        ),
                    ),
                }]);
            }
            if !dir_ok {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} => SKIP: 15m dir mismatch",
                            symbol, event.event_id, rem, filter_desc, dir5
                        ),
                    ),
                }]);
            }
        }

        let (token_id, p_win) = match dir5 {
            Side::Up => (event.up_token.clone(), post5.p_up),
            Side::Down => (event.down_token.clone(), 1.0 - post5.p_up),
        };

        let ask = match self.quotes.get(&token_id).and_then(|q| q.best_ask) {
            Some(a) => a,
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} => SKIP: no quote for token={}",
                            symbol,
                            event.event_id,
                            rem,
                            filter_desc,
                            dir5,
                            &token_id[..8.min(token_id.len())]
                        ),
                    ),
                }]);
            }
        };

        if ask > self.cfg.trade.max_entry_price {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c => SKIP: ask too high (max {:.1}c)",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        ask * dec!(100),
                        self.cfg.trade.max_entry_price * dec!(100),
                    ),
                ),
            }]);
        }

        let ev = match Self::ev_for_side(ask, p_win) {
            Some(v) => v,
            None => {
                return Some(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Custom("Decision".to_string()),
                        format!(
                            "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c => SKIP: EV calc failed",
                            symbol,
                            event.event_id,
                            rem,
                            filter_desc,
                            dir5,
                            ask * dec!(100),
                        ),
                    ),
                }]);
            }
        };

        if ev.net_ev < self.cfg.trade.min_net_ev {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c net_ev={:.4} => SKIP: net_ev < min_net_ev ({:.4})",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        ask * dec!(100),
                        ev.net_ev,
                        self.cfg.trade.min_net_ev,
                    ),
                ),
            }]);
        }

        if !ev.is_positive_ev {
            return Some(vec![StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::Custom("Decision".to_string()),
                    format!(
                        "{} 5m close: event={} rem={}s {} dir5={} ask={:.1}c net_ev={:.4} => SKIP: negative EV",
                        symbol,
                        event.event_id,
                        rem,
                        filter_desc,
                        dir5,
                        ask * dec!(100),
                        ev.net_ev,
                    ),
                ),
            }]);
        }

        let client_order_id = format!(
            "{}_{}_{}_{}_{}",
            self.id,
            symbol,
            event.event_id,
            dir5.as_str().to_lowercase(),
            now.timestamp_millis()
        );

        let mut actions: Vec<StrategyAction> = Vec::new();
        let thr_display = price_to_beat.unwrap_or(spot);
        let thr_src = if price_to_beat.is_some() {
            "fixed"
        } else {
            "dynamic"
        };
        let tf15_conf_display = if self.cfg.filter_15m.enabled {
            format!("{:.1}", conf15 * 100.0)
        } else {
            "NA".to_string()
        };
        let tf15_dir_display = if self.cfg.filter_15m.enabled {
            dir15.to_string()
        } else {
            "NA".to_string()
        };

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::SignalDetected,
                format!(
                    "{} pattern_memory {} event={} rem={}s p_win={:.1}% conf={:.1}% n_eff={:.2} matches={} samples5={} r_req={:.3}% spot={:.2} thr_{}={:.2} ask={:.1}c net_ev={:.4} 15m_conf={} 15m_dir={}",
                    symbol,
                    dir5,
                    event.event_id,
                    rem,
                    p_win * 100.0,
                    conf5 * 100.0,
                    post5.n_eff,
                    post5.matches,
                    samples5,
                    required_return * 100.0,
                    spot,
                    thr_src,
                    thr_display,
                    ask * dec!(100),
                    ev.net_ev,
                    tf15_conf_display,
                    tf15_dir_display,
                ),
            ),
        });

        actions.push(StrategyAction::SubmitIntent {
            intent: StrategyOrderIntent {
                client_order_id,
                domain: Domain::Crypto,
                market_slug: event.event_id.clone(),
                token_id,
                side: dir5,
                is_buy: true,
                shares: self.cfg.trade.shares,
                limit_price: ask,
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                priority: 7,
                metadata: HashMap::new(),
            },
        });

        self.traded_events.insert(event.event_id.clone());
        self.cooldowns.insert(symbol.to_string(), now);

        Some(actions)
    }
}
