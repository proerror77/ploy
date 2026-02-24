//! NBA Data Collection System
//!
//! Collects and synchronizes data from multiple sources:
//! 1. Polymarket LOB (orderbook snapshots)
//! 2. NBA live scores (game state)
//! 3. Team statistics (historical data)
//!
//! Critical: All data must be timestamped and synchronized

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Complete market snapshot with synchronized data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub game_id: String,

    // Market data
    pub orderbook: OrderbookData,

    // Game data
    pub game_state: GameState,

    // Metadata
    pub data_latency_ms: u64,
    pub sources_synced: bool,
}

/// Orderbook data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookData {
    pub timestamp: DateTime<Utc>,
    pub token_id: String,
    pub team_name: String,

    // Best prices
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub mid_price: Option<Decimal>,
    pub spread_bps: Option<i32>,

    // Depth
    pub bid_depth: Decimal,
    pub ask_depth: Decimal,
    pub bid_levels: Vec<OrderLevel>,
    pub ask_levels: Vec<OrderLevel>,

    // Recent trades
    pub recent_trades: Vec<Trade>,
}

/// Order level in LOB
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLevel {
    pub price: Decimal,
    pub size: Decimal,
}

/// Trade record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: String,
    pub timestamp: DateTime<Utc>,
    pub price: Decimal,
    pub size: Decimal,
    pub side: String, // "buy" or "sell"
}

/// Game state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameState {
    pub timestamp: DateTime<Utc>,
    pub game_id: String,
    pub home_team: String,
    pub away_team: String,

    // Score
    pub home_score: i32,
    pub away_score: i32,

    // Time
    pub quarter: u8,
    pub time_remaining: f64, // minutes
    pub game_status: String, // "live", "final", "scheduled"

    // Additional context
    pub possession: Option<String>, // "home" or "away"
    pub last_play: Option<String>,
}

/// Team statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamStats {
    pub team_name: String,
    pub season: String,

    // Record
    pub wins: i32,
    pub losses: i32,
    pub win_rate: f64,

    // Scoring
    pub avg_points: f64,
    pub q1_avg_points: f64,
    pub q2_avg_points: f64,
    pub q3_avg_points: f64,
    pub q4_avg_points: f64,

    // Comeback stats
    pub comeback_rate_5pt: f64, // Win rate when down 5 points
    pub comeback_rate_10pt: f64,
    pub comeback_rate_15pt: f64,

    // Strength ratings
    pub elo_rating: Option<f64>,
    pub offensive_rating: Option<f64>,
    pub defensive_rating: Option<f64>,
}

/// Data collector configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorConfig {
    // Collection intervals
    pub lob_interval_ms: u64, // How often to collect LOB (e.g., 10000 = 10s)
    pub game_interval_ms: u64, // How often to collect game state (e.g., 30000 = 30s)

    // LOB settings
    pub max_lob_levels: usize, // Max orderbook levels to record (e.g., 20)
    pub max_recent_trades: usize, // Max recent trades to keep (e.g., 100)

    // Data quality
    pub max_data_age_ms: u64, // Max acceptable data age (e.g., 5000 = 5s)
    pub require_sync: bool,   // Require all sources synced
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            lob_interval_ms: 10000,  // 10 seconds
            game_interval_ms: 30000, // 30 seconds
            max_lob_levels: 20,
            max_recent_trades: 100,
            max_data_age_ms: 5000, // 5 seconds
            require_sync: true,
        }
    }
}

/// Data collector (placeholder for actual implementation)
///
/// In production, this would:
/// 1. Connect to Polymarket WebSocket for LOB updates
/// 2. Poll NBA API for live scores
/// 3. Cache team statistics
/// 4. Synchronize timestamps across sources
/// 5. Store snapshots to database
pub struct DataCollector {
    config: CollectorConfig,

    // Caches
    orderbook_cache: HashMap<String, OrderbookData>,
    game_state_cache: HashMap<String, GameState>,
    team_stats_cache: HashMap<String, TeamStats>,
}

impl DataCollector {
    pub fn new(config: CollectorConfig) -> Self {
        Self {
            config,
            orderbook_cache: HashMap::new(),
            game_state_cache: HashMap::new(),
            team_stats_cache: HashMap::new(),
        }
    }

    /// Get synchronized market snapshot
    ///
    /// This is the main entry point for the strategy.
    /// Returns None if data is stale or not synchronized.
    pub fn get_snapshot(&self, market_id: &str, game_id: &str) -> Option<MarketSnapshot> {
        let orderbook = self.orderbook_cache.get(market_id)?;
        let game_state = self.game_state_cache.get(game_id)?;

        let now = Utc::now();

        // Check data freshness
        let lob_age = (now - orderbook.timestamp).num_milliseconds() as u64;
        let game_age = (now - game_state.timestamp).num_milliseconds() as u64;

        if lob_age > self.config.max_data_age_ms || game_age > self.config.max_data_age_ms {
            return None; // Data too stale
        }

        // Check synchronization
        let time_diff = (orderbook.timestamp - game_state.timestamp)
            .num_milliseconds()
            .abs() as u64;
        let sources_synced = time_diff < 1000; // Within 1 second

        if self.config.require_sync && !sources_synced {
            return None; // Sources not synchronized
        }

        let data_latency_ms = lob_age.max(game_age);

        Some(MarketSnapshot {
            timestamp: now,
            market_id: market_id.to_string(),
            game_id: game_id.to_string(),
            orderbook: orderbook.clone(),
            game_state: game_state.clone(),
            data_latency_ms,
            sources_synced,
        })
    }

    /// Get team statistics
    pub fn get_team_stats(&self, team_name: &str) -> Option<&TeamStats> {
        self.team_stats_cache.get(team_name)
    }

    /// Update orderbook cache (called by LOB collector)
    pub fn update_orderbook(&mut self, market_id: String, data: OrderbookData) {
        self.orderbook_cache.insert(market_id, data);
    }

    /// Update game state cache (called by game collector)
    pub fn update_game_state(&mut self, game_id: String, state: GameState) {
        self.game_state_cache.insert(game_id, state);
    }

    /// Update team stats cache (called by stats loader)
    pub fn update_team_stats(&mut self, team_name: String, stats: TeamStats) {
        self.team_stats_cache.insert(team_name, stats);
    }

    /// Check if data is fresh enough for trading
    pub fn is_data_fresh(&self, market_id: &str, game_id: &str) -> bool {
        self.get_snapshot(market_id, game_id).is_some()
    }
}

/// Helper to calculate spread in basis points
pub fn calculate_spread_bps(bid: Decimal, ask: Decimal) -> i32 {
    let mid = (bid + ask) / Decimal::from(2);
    let spread = ask - bid;

    if mid > Decimal::ZERO {
        let bps = (spread / mid) * Decimal::from(10000);
        bps.to_i32().unwrap_or(0)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_orderbook() -> OrderbookData {
        OrderbookData {
            timestamp: Utc::now(),
            token_id: "test_token".to_string(),
            team_name: "Lakers".to_string(),
            best_bid: Some(Decimal::new(45, 2)),
            best_ask: Some(Decimal::new(46, 2)),
            mid_price: Some(Decimal::new(455, 3)),
            spread_bps: Some(22),
            bid_depth: Decimal::new(2000, 0),
            ask_depth: Decimal::new(1800, 0),
            bid_levels: vec![],
            ask_levels: vec![],
            recent_trades: vec![],
        }
    }

    fn create_test_game_state() -> GameState {
        GameState {
            timestamp: Utc::now(),
            game_id: "test_game".to_string(),
            home_team: "Lakers".to_string(),
            away_team: "Warriors".to_string(),
            home_score: 85,
            away_score: 90,
            quarter: 3,
            time_remaining: 8.5,
            game_status: "live".to_string(),
            possession: Some("home".to_string()),
            last_play: None,
        }
    }

    #[test]
    fn test_snapshot_creation() {
        let mut collector = DataCollector::new(CollectorConfig::default());

        let orderbook = create_test_orderbook();
        let game_state = create_test_game_state();

        collector.update_orderbook("market1".to_string(), orderbook);
        collector.update_game_state("game1".to_string(), game_state);

        let snapshot = collector.get_snapshot("market1", "game1");
        assert!(snapshot.is_some());

        let snap = snapshot.unwrap();
        assert_eq!(snap.market_id, "market1");
        assert_eq!(snap.game_id, "game1");
        assert!(snap.sources_synced);
    }

    #[test]
    fn test_stale_data_rejected() {
        let mut collector = DataCollector::new(CollectorConfig {
            max_data_age_ms: 100, // Very short timeout
            ..Default::default()
        });

        let orderbook = create_test_orderbook();
        let game_state = create_test_game_state();

        collector.update_orderbook("market1".to_string(), orderbook);
        collector.update_game_state("game1".to_string(), game_state);

        // Wait for data to become stale
        std::thread::sleep(std::time::Duration::from_millis(150));

        let snapshot = collector.get_snapshot("market1", "game1");
        assert!(snapshot.is_none(), "Stale data should be rejected");
    }

    #[test]
    fn test_spread_calculation() {
        let bid = Decimal::new(45, 2); // 0.45
        let ask = Decimal::new(46, 2); // 0.46

        let spread_bps = calculate_spread_bps(bid, ask);

        // Spread = 0.01, Mid = 0.455, BPS = (0.01 / 0.455) * 10000 ≈ 220
        assert!(spread_bps > 200 && spread_bps < 230);
    }
}
