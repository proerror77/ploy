use super::*;

/// Daily trade counter for rate limiting.
#[derive(Debug, Default)]
pub(super) struct DailyTradeCounter {
    count: u32,
    reset_date: Option<chrono::NaiveDate>,
}

impl DailyTradeCounter {
    pub(super) fn increment(&mut self) -> u32 {
        let today = Utc::now().date_naive();
        if self.reset_date != Some(today) {
            self.count = 0;
            self.reset_date = Some(today);
        }
        self.count += 1;
        self.count
    }

    pub(super) fn current(&mut self) -> u32 {
        let today = Utc::now().date_naive();
        if self.reset_date != Some(today) {
            self.count = 0;
            self.reset_date = Some(today);
        }
        self.count
    }
}

impl MomentumEngine {
    pub(super) async fn daily_limit_reached(&self) -> bool {
        if self.config.max_daily_trades == 0 {
            return false;
        }
        let mut counter = self.daily_trades.write().await;
        counter.current() >= self.config.max_daily_trades
    }

    pub(super) async fn record_trade(&self) -> u32 {
        let mut counter = self.daily_trades.write().await;
        counter.increment()
    }

    fn estimated_win_probability(&self, signal: &MomentumSignal) -> Decimal {
        (signal.pm_price + signal.edge)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    fn signal_kelly_fraction(&self, signal: &MomentumSignal) -> Decimal {
        if signal.pm_price <= Decimal::ZERO || signal.pm_price >= Decimal::ONE {
            return Decimal::ZERO;
        }

        let p = self.estimated_win_probability(signal);
        let denom = Decimal::ONE - signal.pm_price;
        if denom <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        ((p - signal.pm_price) / denom)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    pub(super) fn apply_signal_position_sizing(
        &self,
        base_shares: u64,
        signal: &MomentumSignal,
    ) -> u64 {
        if base_shares == 0 {
            return 0;
        }

        let mut multiplier = Decimal::ONE;

        if self.config.dynamic_position_sizing {
            let conf = Decimal::from_f64(signal.confidence.clamp(0.0, 1.0)).unwrap_or(Decimal::ONE);
            multiplier *= conf;
        }

        if self.config.use_kelly_sizing {
            let kelly = self.signal_kelly_fraction(signal);
            let cap = self.config.kelly_fraction_cap.max(dec!(0.0001));
            let normalized = (kelly / cap).min(Decimal::ONE);
            multiplier *= normalized;
        }

        let scaled = (Decimal::from(base_shares) * multiplier)
            .floor()
            .to_u64()
            .unwrap_or(0);

        if scaled == 0 {
            debug!(
                "Position size scaled to 0 (base_shares={}, multiplier={:.4})",
                base_shares, multiplier
            );
        }

        scaled
    }

    pub async fn positions_count(&self) -> usize {
        self.positions.read().await.len()
    }
}
