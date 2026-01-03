//! Simulated Market for Price and Quote Generation
//!
//! Generates realistic market data for training RL agents.

use rand::Rng;

/// Market simulation configuration
#[derive(Debug, Clone)]
pub struct MarketConfig {
    /// Initial spot price
    pub initial_price: f64,
    /// Price volatility (std dev of returns)
    pub volatility: f64,
    /// Mean reversion strength (0 = random walk, 1 = strong reversion)
    pub mean_reversion: f64,
    /// Long-term mean price
    pub mean_price: f64,
    /// Bid-ask spread as percentage
    pub spread_pct: f64,
    /// Quote update frequency (steps between quote changes)
    pub quote_update_freq: usize,
    /// Trend strength (-1 to 1)
    pub trend: f64,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            initial_price: 0.50,
            volatility: 0.02,
            mean_reversion: 0.1,
            mean_price: 0.50,
            spread_pct: 0.02,
            quote_update_freq: 5,
            trend: 0.0,
        }
    }
}

/// Current market state
#[derive(Debug, Clone)]
pub struct MarketState {
    /// Current spot price
    pub spot_price: f64,
    /// UP token bid price
    pub up_bid: f64,
    /// UP token ask price
    pub up_ask: f64,
    /// DOWN token bid price
    pub down_bid: f64,
    /// DOWN token ask price
    pub down_ask: f64,
    /// Sum of asks (up_ask + down_ask)
    pub sum_of_asks: f64,
    /// Price history (most recent last)
    pub price_history: Vec<f64>,
    /// Current step
    pub step: usize,
}

impl MarketState {
    /// Get momentum over n steps
    pub fn momentum(&self, n: usize) -> Option<f64> {
        if self.price_history.len() < n + 1 {
            return None;
        }
        let current = self.price_history.last()?;
        let past = self.price_history.get(self.price_history.len() - n - 1)?;
        Some(current - past)
    }

    /// Get UP spread
    pub fn up_spread(&self) -> f64 {
        self.up_ask - self.up_bid
    }

    /// Get DOWN spread
    pub fn down_spread(&self) -> f64 {
        self.down_ask - self.down_bid
    }
}

/// Simulated market for generating price and quote data
pub struct SimulatedMarket {
    config: MarketConfig,
    state: MarketState,
    rng: rand::rngs::ThreadRng,
}

impl SimulatedMarket {
    /// Create a new simulated market
    pub fn new(config: MarketConfig) -> Self {
        let initial_price = config.initial_price;

        // Initialize quotes based on initial price
        let half_spread = config.spread_pct / 2.0;
        let up_mid = initial_price;
        let down_mid = 1.0 - initial_price;

        let state = MarketState {
            spot_price: initial_price,
            up_bid: (up_mid * (1.0 - half_spread)).max(0.01),
            up_ask: (up_mid * (1.0 + half_spread)).min(0.99),
            down_bid: (down_mid * (1.0 - half_spread)).max(0.01),
            down_ask: (down_mid * (1.0 + half_spread)).min(0.99),
            sum_of_asks: 0.0, // Will be calculated
            price_history: vec![initial_price],
            step: 0,
        };

        let mut market = Self {
            config,
            state,
            rng: rand::thread_rng(),
        };

        market.update_sum_of_asks();
        market
    }

    /// Generate a sample from standard normal distribution (Box-Muller transform)
    fn sample_normal(&mut self) -> f64 {
        let u1: f64 = self.rng.gen_range(0.0001..1.0);
        let u2: f64 = self.rng.gen();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Reset market to initial state
    pub fn reset(&mut self) -> &MarketState {
        let initial_price = self.config.initial_price;
        let half_spread = self.config.spread_pct / 2.0;
        let up_mid = initial_price;
        let down_mid = 1.0 - initial_price;

        self.state = MarketState {
            spot_price: initial_price,
            up_bid: (up_mid * (1.0 - half_spread)).max(0.01),
            up_ask: (up_mid * (1.0 + half_spread)).min(0.99),
            down_bid: (down_mid * (1.0 - half_spread)).max(0.01),
            down_ask: (down_mid * (1.0 + half_spread)).min(0.99),
            sum_of_asks: 0.0,
            price_history: vec![initial_price],
            step: 0,
        };

        self.update_sum_of_asks();
        &self.state
    }

    /// Step the market forward one time step
    pub fn step(&mut self) -> &MarketState {
        self.state.step += 1;

        // Generate price return with mean reversion and trend
        let random_return = self.sample_normal() * self.config.volatility;
        let reversion = self.config.mean_reversion * (self.config.mean_price - self.state.spot_price);
        let trend_component = self.config.trend * self.config.volatility;

        let total_return = random_return + reversion + trend_component;

        // Update spot price (clamped to valid range)
        self.state.spot_price = (self.state.spot_price + total_return).clamp(0.01, 0.99);

        // Update price history (keep last 60 prices)
        self.state.price_history.push(self.state.spot_price);
        if self.state.price_history.len() > 60 {
            self.state.price_history.remove(0);
        }

        // Update quotes periodically
        if self.state.step % self.config.quote_update_freq == 0 {
            self.update_quotes();
        }

        &self.state
    }

    /// Update quotes based on current spot price
    fn update_quotes(&mut self) {
        let half_spread = self.config.spread_pct / 2.0;
        let noise: f64 = self.rng.gen_range(-0.005..0.005);

        let up_mid = self.state.spot_price + noise;
        let down_mid = 1.0 - self.state.spot_price + noise;

        // Add some randomness to spreads
        let spread_noise: f64 = self.rng.gen_range(0.8..1.2);
        let adjusted_spread = half_spread * spread_noise;

        self.state.up_bid = (up_mid * (1.0 - adjusted_spread)).clamp(0.01, 0.99);
        self.state.up_ask = (up_mid * (1.0 + adjusted_spread)).clamp(0.01, 0.99);
        self.state.down_bid = (down_mid * (1.0 - adjusted_spread)).clamp(0.01, 0.99);
        self.state.down_ask = (down_mid * (1.0 + adjusted_spread)).clamp(0.01, 0.99);

        self.update_sum_of_asks();
    }

    fn update_sum_of_asks(&mut self) {
        self.state.sum_of_asks = self.state.up_ask + self.state.down_ask;
    }

    /// Get current market state
    pub fn state(&self) -> &MarketState {
        &self.state
    }

    /// Get configuration
    pub fn config(&self) -> &MarketConfig {
        &self.config
    }

    /// Set trend direction (-1 to 1)
    pub fn set_trend(&mut self, trend: f64) {
        self.config.trend = trend.clamp(-1.0, 1.0);
    }

    /// Introduce a price shock
    pub fn shock(&mut self, magnitude: f64) {
        self.state.spot_price = (self.state.spot_price + magnitude).clamp(0.01, 0.99);
        self.update_quotes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_creation() {
        let config = MarketConfig::default();
        let market = SimulatedMarket::new(config);

        assert!((market.state().spot_price - 0.50).abs() < 0.01);
        assert!(market.state().up_ask > market.state().up_bid);
        assert!(market.state().down_ask > market.state().down_bid);
    }

    #[test]
    fn test_market_step() {
        let config = MarketConfig::default();
        let mut market = SimulatedMarket::new(config);

        let initial_price = market.state().spot_price;

        // Step multiple times
        for _ in 0..100 {
            market.step();
        }

        // Price should have moved
        assert!(market.state().price_history.len() > 1);
        // Price should still be valid
        assert!(market.state().spot_price >= 0.01);
        assert!(market.state().spot_price <= 0.99);
    }

    #[test]
    fn test_market_reset() {
        let config = MarketConfig::default();
        let mut market = SimulatedMarket::new(config);

        // Step and change state
        for _ in 0..50 {
            market.step();
        }

        // Reset
        market.reset();

        assert!((market.state().spot_price - 0.50).abs() < 0.01);
        assert_eq!(market.state().step, 0);
        assert_eq!(market.state().price_history.len(), 1);
    }

    #[test]
    fn test_momentum_calculation() {
        let config = MarketConfig {
            trend: 0.5, // Upward trend
            ..Default::default()
        };
        let mut market = SimulatedMarket::new(config);

        for _ in 0..20 {
            market.step();
        }

        // Should have momentum data
        let momentum = market.state().momentum(5);
        assert!(momentum.is_some());
    }

    #[test]
    fn test_sum_of_asks() {
        let config = MarketConfig::default();
        let market = SimulatedMarket::new(config);

        let state = market.state();
        let expected_sum = state.up_ask + state.down_ask;
        assert!((state.sum_of_asks - expected_sum).abs() < 0.0001);
    }
}
