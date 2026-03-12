use super::*;

impl MomentumStrategyAdapter {
    pub(super) async fn handle_order_update(
        &mut self,
        update: &OrderUpdate,
    ) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        let order_key = update
            .client_order_id
            .clone()
            .unwrap_or_else(|| update.order_id.clone());
        let track = self.pending_orders.get(&order_key).cloned();

        match update.status {
            crate::domain::OrderStatus::Filled => {
                info!(
                    "[{}] Order filled: {} @ {:?}",
                    self.id, update.order_id, update.avg_fill_price
                );

                if let Some(track) = track {
                    match track.kind {
                        MomentumOrderKind::Entry => {
                            let fill_price = update.avg_fill_price.unwrap_or(track.price);
                            let filled_shares = if update.filled_qty > 0 {
                                update.filled_qty
                            } else {
                                track.shares
                            };

                            self.positions.insert(
                                track.token_id.clone(),
                                MomentumPosition {
                                    token_id: track.token_id.clone(),
                                    symbol: track.symbol.clone(),
                                    direction: track.direction,
                                    side: track.side,
                                    shares: filled_shares,
                                    entry_price: fill_price,
                                    current_price: Some(fill_price),
                                    opened_at: update.timestamp,
                                    order_id: Some(update.order_id.clone()),
                                },
                            );

                            self.daily_trades += 1;
                        }
                        MomentumOrderKind::Exit => {
                            self.positions.remove(&track.token_id);
                        }
                    }

                    self.pending_orders.remove(&order_key);
                }

                actions.push(StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::OrderFilled,
                        format!("Order {} filled", update.order_id),
                    ),
                });
            }
            crate::domain::OrderStatus::Cancelled => {
                warn!("[{}] Order cancelled: {}", self.id, update.order_id);
                self.pending_orders.remove(&order_key);
            }
            crate::domain::OrderStatus::Failed => {
                warn!(
                    "[{}] Order failed: {} - {:?}",
                    self.id, update.order_id, update.error
                );

                self.pending_orders.remove(&order_key);

                actions.push(StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: format!("Order failed: {:?}", update.error),
                });
            }
            _ => {}
        }

        Ok(actions)
    }

    pub(super) fn state_snapshot(&self) -> StrategyStateInfo {
        let position_count = self.positions.len();
        let pending_count = self.pending_orders.len();
        let total_exposure = self
            .positions
            .values()
            .map(|pos| pos.entry_price * Decimal::from(pos.shares))
            .sum::<Decimal>();

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled { "running" } else { "paused" }.to_string(),
            enabled: self.enabled,
            active: self.enabled,
            position_count,
            pending_order_count: pending_count,
            total_exposure,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: Utc::now(),
            metrics: {
                let mut m = HashMap::new();
                m.insert(
                    "mode".into(),
                    if self.config.hold_to_resolution {
                        "confirmatory"
                    } else {
                        "predictive"
                    }
                    .into(),
                );
                m.insert("dry_run".into(), self.dry_run.to_string());
                m
            },
        }
    }

    pub(super) fn position_infos(&self) -> Vec<PositionInfo> {
        self.positions
            .values()
            .map(|p| {
                PositionInfo::new(
                    p.token_id.clone(),
                    p.side,
                    p.shares,
                    p.entry_price,
                    self.id.clone(),
                )
            })
            .collect()
    }

    pub(super) async fn shutdown_actions(&mut self) -> Result<Vec<StrategyAction>> {
        info!("[{}] Shutting down momentum strategy", self.id);
        self.enabled = false;

        let mut actions = Vec::new();

        if !self.config.hold_to_resolution {
            for pos in self.positions.values() {
                info!(
                    "[{}] Closing position: {} {} shares @ {:?}",
                    self.id, pos.token_id, pos.shares, pos.current_price
                );
            }
        }

        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                "Strategy shutdown initiated",
            ),
        });

        Ok(actions)
    }

    pub(super) fn reset_state(&mut self) {
        self.positions = HashMap::new();
        self.cex_prices = HashMap::new();
        self.pm_quotes = HashMap::new();
        self.events = HashMap::new();
        self.cooldowns = HashMap::new();
        self.daily_trades = 0;
        self.last_reset = Utc::now();
        self.pending_orders = HashMap::new();
    }
}
