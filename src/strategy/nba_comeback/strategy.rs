use std::collections::HashMap;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use toml::Value;
use tracing::warn;

use super::comeback_stats::ComebackStatsProvider;
use super::core::{ComebackCandidate, ComebackOpportunity, NbaComebackCore};
use super::espn::{EspnClient, GameStatus, LiveGame};
use crate::config::NbaComebackConfig;
use crate::domain::{OrderRequest, OrderStatus, Quote, Side};
use crate::error::{PloyError, Result};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NbaComebackMarketRegistration {
    pub game_id: String,
    pub trailing_abbrev: String,
    pub market_slug: String,
    pub token_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOrderKind {
    Entry,
    ScaleIn,
    Exit,
}

#[derive(Debug, Clone)]
struct PendingOrderTrack {
    kind: PendingOrderKind,
    game_id: String,
    trailing_abbrev: String,
    market_slug: String,
    token_id: String,
    comeback_rate: f64,
    shares: u64,
    price: Decimal,
}

#[derive(Debug, Clone)]
struct ManagedPosition {
    game_id: String,
    trailing_abbrev: String,
    market_slug: String,
    token_id: String,
    shares: u64,
    entry_price: Decimal,
    current_price: Option<Decimal>,
    opened_at: DateTime<Utc>,
}

pub struct NbaComebackStrategy {
    id: String,
    core: NbaComebackCore,
    dry_run: bool,
    enabled: bool,
    registered_markets: HashMap<String, NbaComebackMarketRegistration>,
    manual_live_games: HashMap<String, LiveGame>,
    quotes: HashMap<String, Quote>,
    positions: HashMap<String, ManagedPosition>,
    pending_orders: HashMap<String, PendingOrderTrack>,
    stats_load_attempted: bool,
    last_update: DateTime<Utc>,
    #[cfg(test)]
    pub(crate) test_candidates: Option<Vec<ComebackCandidate>>,
}

pub type NbaComebackAdapter = NbaComebackStrategy;

impl NbaComebackStrategy {
    pub fn new(id: String, core: NbaComebackCore, dry_run: bool) -> Self {
        Self {
            id,
            core,
            dry_run,
            enabled: true,
            registered_markets: HashMap::new(),
            manual_live_games: HashMap::new(),
            quotes: HashMap::new(),
            positions: HashMap::new(),
            pending_orders: HashMap::new(),
            stats_load_attempted: false,
            last_update: Utc::now(),
            #[cfg(test)]
            test_candidates: None,
        }
    }

    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        let config: Value = toml::from_str(config_str)
            .map_err(|e| PloyError::Internal(format!("Invalid TOML: {e}")))?;

        let strategy = config
            .get("strategy")
            .and_then(Value::as_table)
            .ok_or_else(|| PloyError::Internal("Missing [strategy] section".to_string()))?;
        let strategy_name = strategy
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| PloyError::Internal("Missing [strategy].name".to_string()))?;
        if strategy_name != "nba_comeback" {
            return Err(PloyError::Internal(format!(
                "Expected [strategy].name = \"nba_comeback\", got \"{strategy_name}\""
            )));
        }

        let mut cfg = default_nba_comeback_config();
        if let Some(section) = config.get("nba_comeback").and_then(Value::as_table) {
            apply_nba_comeback_overrides(&mut cfg, section);
        }

        let database_url = database_url_from_env();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&database_url)
            .map_err(PloyError::Database)?;
        let stats = ComebackStatsProvider::new(pool, cfg.season.clone());
        let core = NbaComebackCore::new(EspnClient::new(), stats, cfg);

        Ok(Self::new(id, core, dry_run))
    }

    pub fn config(&self) -> &NbaComebackConfig {
        &self.core.cfg
    }

    pub fn register_market(&mut self, registration: NbaComebackMarketRegistration) {
        self.registered_markets
            .insert(registration.game_id.clone(), registration);
        self.last_update = Utc::now();
    }

    pub fn upsert_live_game(&mut self, game: LiveGame) {
        self.manual_live_games.insert(game.espn_game_id.clone(), game);
        self.last_update = Utc::now();
    }

    pub fn replace_live_games<I>(&mut self, games: I)
    where
        I: IntoIterator<Item = LiveGame>,
    {
        self.manual_live_games.clear();
        for game in games {
            self.upsert_live_game(game);
        }
    }

    async fn ensure_stats_loaded(&mut self) {
        if self.stats_load_attempted {
            return;
        }

        self.stats_load_attempted = true;
        if let Err(error) = self.core.stats.load_all().await {
            warn!(
                strategy = self.id,
                error = %error,
                "nba_comeback: failed to load team stats; candidate scans may be empty"
            );
        }
    }

    async fn scan_candidates(&mut self) -> Vec<ComebackCandidate> {
        #[cfg(test)]
        if let Some(candidates) = self.test_candidates.clone() {
            self.core.reset_daily_if_needed();
            return candidates;
        }

        self.ensure_stats_loaded().await;
        if !self.manual_live_games.is_empty() {
            let games: Vec<LiveGame> = self.manual_live_games.values().cloned().collect();
            return self.core.scan_games(&games);
        }

        self.core.scan_espn().await
    }

    fn market_for_candidate(
        &self,
        candidate: &ComebackCandidate,
    ) -> Option<&NbaComebackMarketRegistration> {
        let registration = self.registered_markets.get(&candidate.game.espn_game_id)?;
        if registration
            .trailing_abbrev
            .eq_ignore_ascii_case(&candidate.trailing_abbrev)
        {
            Some(registration)
        } else {
            None
        }
    }

    fn best_quote_price(&self, token_id: &str, is_buy: bool) -> Option<Decimal> {
        let quote = self.quotes.get(token_id)?;
        if is_buy {
            quote.best_ask.or_else(|| quote.mid_price()).or(quote.best_bid)
        } else {
            quote.best_bid.or_else(|| quote.mid_price()).or(quote.best_ask)
        }
    }

    fn has_pending_order_for_game(&self, game_id: &str) -> bool {
        self.pending_orders.values().any(|track| track.game_id == game_id)
    }

    fn submit_entry_action(
        &mut self,
        opportunity: &ComebackOpportunity,
        shares: u64,
        kind: PendingOrderKind,
        now: DateTime<Utc>,
    ) -> StrategyAction {
        let order_kind = match kind {
            PendingOrderKind::Entry => "entry",
            PendingOrderKind::ScaleIn => "scale_in",
            PendingOrderKind::Exit => "exit",
        };
        let client_order_id = format!(
            "{}_{}_{}_{}",
            self.id,
            order_kind,
            opportunity.game.espn_game_id,
            now.timestamp_millis()
        );
        let mut order = OrderRequest::buy_limit(
            opportunity.token_id.clone(),
            Side::Up,
            shares,
            opportunity.market_price,
        );
        order.client_order_id = client_order_id.clone();
        order.idempotency_key = Some(client_order_id.clone());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrderTrack {
                kind,
                game_id: opportunity.game.espn_game_id.clone(),
                trailing_abbrev: opportunity.trailing_abbrev.clone(),
                market_slug: opportunity.market_slug.clone(),
                token_id: opportunity.token_id.clone(),
                comeback_rate: opportunity.comeback_rate,
                shares,
                price: opportunity.market_price,
            },
        );

        StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: if kind == PendingOrderKind::ScaleIn { 7 } else { 6 },
        }
    }

    fn submit_exit_action(
        &mut self,
        position: &ManagedPosition,
        exit_price: Decimal,
        now: DateTime<Utc>,
    ) -> StrategyAction {
        let client_order_id = format!(
            "{}_exit_{}_{}",
            self.id,
            position.game_id,
            now.timestamp_millis()
        );
        let mut order = OrderRequest::sell_limit(
            position.token_id.clone(),
            Side::Up,
            position.shares,
            exit_price,
        );
        order.client_order_id = client_order_id.clone();
        order.idempotency_key = Some(client_order_id.clone());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingOrderTrack {
                kind: PendingOrderKind::Exit,
                game_id: position.game_id.clone(),
                trailing_abbrev: position.trailing_abbrev.clone(),
                market_slug: position.market_slug.clone(),
                token_id: position.token_id.clone(),
                comeback_rate: 0.0,
                shares: position.shares,
                price: exit_price,
            },
        );

        StrategyAction::SubmitOrder {
            client_order_id,
            order,
            priority: 8,
        }
    }

    fn position_total_exposure(&self) -> Decimal {
        self.positions
            .values()
            .map(|position| position.entry_price * Decimal::from(position.shares))
            .sum()
    }

    fn current_phase(&self) -> &'static str {
        if !self.enabled {
            return "paused";
        }
        if self
            .pending_orders
            .values()
            .any(|track| track.kind == PendingOrderKind::Exit)
        {
            return "exiting";
        }
        if !self.positions.is_empty() {
            return "managing";
        }
        if !self.pending_orders.is_empty() {
            return "entering";
        }
        "watch"
    }

    fn refresh_mark_to_market_prices(&mut self) {
        for position in self.positions.values_mut() {
            let next_price = self
                .quotes
                .get(&position.token_id)
                .and_then(|quote| quote.best_bid.or_else(|| quote.mid_price()).or(quote.best_ask));
            position.current_price = next_price;
        }
    }

    fn classify_early_exit(
        avg_entry_price: Decimal,
        current_price: Decimal,
        cfg: &NbaComebackConfig,
    ) -> Option<&'static str> {
        if avg_entry_price <= Decimal::ZERO || current_price <= Decimal::ZERO {
            return None;
        }
        let pnl_pct = ((current_price - avg_entry_price) * dec!(100) / avg_entry_price)
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);

        if pnl_pct >= cfg.early_exit_take_profit_pct {
            return Some("take_profit");
        }
        if pnl_pct <= -cfg.early_exit_stop_loss_pct {
            return Some("stop_loss");
        }
        None
    }

    fn settle_final_position(
        &mut self,
        game: &LiveGame,
        position: ManagedPosition,
        actions: &mut Vec<StrategyAction>,
    ) {
        let settled_price = if game.home_score == game.away_score {
            None
        } else if position.trailing_abbrev.eq_ignore_ascii_case(&game.home_abbrev) {
            Some(if game.home_score > game.away_score {
                Decimal::ONE
            } else {
                Decimal::ZERO
            })
        } else if position.trailing_abbrev.eq_ignore_ascii_case(&game.away_abbrev) {
            Some(if game.away_score > game.home_score {
                Decimal::ONE
            } else {
                Decimal::ZERO
            })
        } else {
            None
        };

        if let Some(price) = settled_price {
            let pnl = (price - position.entry_price) * Decimal::from(position.shares);
            self.core.record_realized_pnl(pnl);
            self.core.close_position(&position.game_id);

            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::CycleCompleted,
                    format!(
                        "Settled NBA comeback position {} at {}",
                        position.game_id, price
                    ),
                )
                .with_data("game_id", position.game_id)
                .with_data("settlement_price", price.to_string())
                .with_data("pnl", pnl.to_string()),
            });
        }
    }
}

#[async_trait]
impl Strategy for NbaComebackStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "NBA Comeback"
    }

    fn description(&self) -> &str {
        "NBA Q3->Q4 comeback strategy wrapped behind the canonical Strategy trait"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.core.cfg.espn_poll_interval_secs * 1000,
        }]
    }

    async fn on_market_update(&mut self, update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        self.last_update = Utc::now();

        if let MarketUpdate::PolymarketQuote {
            token_id, quote, ..
        } = update
        {
            self.quotes.insert(token_id.clone(), *quote);
            if let Some(position) = self
                .positions
                .values_mut()
                .find(|position| position.token_id == *token_id)
            {
                position.current_price = quote
                    .best_bid
                    .or_else(|| quote.mid_price())
                    .or(quote.best_ask);
            }
        }

        Ok(vec![])
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.last_update = update.timestamp;
        let order_key = update
            .client_order_id
            .clone()
            .unwrap_or_else(|| update.order_id.clone());
        let Some(track) = self.pending_orders.get(&order_key).cloned() else {
            return Ok(vec![]);
        };

        match update.status {
            OrderStatus::Filled => {
                let fill_price = update.avg_fill_price.unwrap_or(track.price);
                let filled_qty = if update.filled_qty > 0 {
                    update.filled_qty
                } else {
                    track.shares
                };

                match track.kind {
                    PendingOrderKind::Entry | PendingOrderKind::ScaleIn => {
                        let spend = fill_price * Decimal::from(filled_qty);
                        if track.kind == PendingOrderKind::Entry {
                            self.core.record_initial_entry_submission(
                                &track.game_id,
                                &track.token_id,
                                spend,
                            );
                        } else {
                            self.core.record_trade(&track.game_id, spend);
                        }
                        self.core.record_position_entry_with_market_and_team(
                            &track.game_id,
                            &track.trailing_abbrev,
                            &track.market_slug,
                            &track.token_id,
                            fill_price,
                            filled_qty,
                            track.comeback_rate,
                        );

                        let position = self
                            .positions
                            .entry(track.game_id.clone())
                            .or_insert_with(|| ManagedPosition {
                                game_id: track.game_id.clone(),
                                trailing_abbrev: track.trailing_abbrev.clone(),
                                market_slug: track.market_slug.clone(),
                                token_id: track.token_id.clone(),
                                shares: 0,
                                entry_price: fill_price,
                                current_price: Some(fill_price),
                                opened_at: update.timestamp,
                            });
                        let existing_cost = position.entry_price * Decimal::from(position.shares);
                        let new_cost = fill_price * Decimal::from(filled_qty);
                        position.shares += filled_qty;
                        let total_cost = existing_cost + new_cost;
                        position.entry_price = total_cost / Decimal::from(position.shares);
                        position.current_price = Some(fill_price);
                        position.opened_at = position.opened_at.min(update.timestamp);
                    }
                    PendingOrderKind::Exit => {
                        if let Some(position) = self.positions.remove(&track.game_id) {
                            let pnl =
                                (fill_price - position.entry_price) * Decimal::from(position.shares);
                            self.core.record_realized_pnl(pnl);
                            self.core.close_position(&track.game_id);
                        }
                    }
                }

                self.pending_orders.remove(&order_key);
            }
            OrderStatus::Cancelled
            | OrderStatus::Rejected
            | OrderStatus::Expired
            | OrderStatus::Failed => {
                self.pending_orders.remove(&order_key);
            }
            OrderStatus::Pending | OrderStatus::Submitted | OrderStatus::PartiallyFilled => {}
        }

        Ok(vec![])
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.last_update = now;
        if !self.enabled {
            return Ok(vec![]);
        }

        self.refresh_mark_to_market_prices();

        let mut actions = Vec::new();

        let final_games: Vec<String> = self
            .positions
            .keys()
            .filter(|game_id| {
                self.manual_live_games
                    .get(*game_id)
                    .map(|game| game.status == GameStatus::Final)
                    .unwrap_or(false)
                    && !self.has_pending_order_for_game(game_id)
            })
            .cloned()
            .collect();
        for game_id in final_games {
            let Some(game) = self.manual_live_games.get(&game_id).cloned() else {
                continue;
            };
            if let Some(position) = self.positions.remove(&game_id) {
                self.settle_final_position(&game, position, &mut actions);
            }
        }

        if self.core.cfg.early_exit_enabled {
            let exits: Vec<ManagedPosition> = self
                .positions
                .values()
                .filter(|position| !self.has_pending_order_for_game(&position.game_id))
                .filter_map(|position| {
                    let current_price = self.best_quote_price(&position.token_id, false)?;
                    if Self::classify_early_exit(position.entry_price, current_price, &self.core.cfg)
                        .is_some()
                    {
                        Some(position.clone())
                    } else {
                        None
                    }
                })
                .collect();

            for position in exits {
                if let Some(exit_price) = self.best_quote_price(&position.token_id, false) {
                    actions.push(self.submit_exit_action(&position, exit_price, now));
                }
            }
        }

        let candidates = self.scan_candidates().await;
        let mut best_entry: Option<ComebackOpportunity> = None;

        for candidate in candidates {
            let Some(registration) = self.market_for_candidate(&candidate).cloned() else {
                continue;
            };
            let Some(entry_price) = self.best_quote_price(&registration.token_id, true) else {
                continue;
            };

            if self.positions.contains_key(&candidate.game.espn_game_id) {
                if !self.core.cfg.scaling_enabled
                    || self.has_pending_order_for_game(&candidate.game.espn_game_id)
                    || !self.core.can_open_new_risk()
                {
                    continue;
                }
                if !self.core.can_scale_in(
                    &candidate.game.espn_game_id,
                    entry_price,
                    candidate.comeback_rate,
                    candidate.game.time_remaining_mins,
                ) {
                    continue;
                }
                let Some(shares) = self.core.kelly_scaling_shares(
                    &candidate.game.espn_game_id,
                    entry_price,
                    candidate.adjusted_win_prob,
                ) else {
                    continue;
                };
                if shares == 0 {
                    continue;
                }
                if let Some(opportunity) = self.core.evaluate_opportunity(
                    &candidate,
                    entry_price,
                    registration.market_slug.clone(),
                    registration.token_id.clone(),
                ) {
                    actions.push(self.submit_entry_action(
                        &opportunity,
                        shares,
                        PendingOrderKind::ScaleIn,
                        now,
                    ));
                }
                continue;
            }

            if self.has_pending_order_for_game(&candidate.game.espn_game_id)
                || self.core.is_duplicate_initial_entry(
                    &candidate.game.espn_game_id,
                    &registration.token_id,
                )
                || !self.core.can_open_new_risk()
            {
                continue;
            }

            if let Some(opportunity) = self.core.evaluate_opportunity(
                &candidate,
                entry_price,
                registration.market_slug.clone(),
                registration.token_id.clone(),
            ) {
                match &best_entry {
                    Some(current) if current.edge >= opportunity.edge => {}
                    _ => best_entry = Some(opportunity),
                }
            }
        }

        if let Some(opportunity) = best_entry {
            let shares = self.core.adjusted_shares(self.core.cfg.shares);
            if shares > 0 {
                actions.push(self.submit_entry_action(
                    &opportunity,
                    shares,
                    PendingOrderKind::Entry,
                    now,
                ));
            }
        }

        Ok(actions)
    }

    fn state(&self) -> StrategyStateInfo {
        let mut metrics = HashMap::new();
        metrics.insert("dry_run".to_string(), self.dry_run.to_string());
        metrics.insert(
            "registered_markets".to_string(),
            self.registered_markets.len().to_string(),
        );
        metrics.insert(
            "cached_games".to_string(),
            self.manual_live_games.len().to_string(),
        );
        metrics.insert(
            "daily_spend_usd".to_string(),
            self.core.state.daily_spend_usd.to_string(),
        );
        metrics.insert(
            "daily_realized_pnl_usd".to_string(),
            self.core.state.daily_realized_pnl_usd.to_string(),
        );
        metrics.insert(
            "risk_size_multiplier".to_string(),
            format!("{:.6}", self.core.risk_size_multiplier()),
        );

        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: self.current_phase().to_string(),
            enabled: self.enabled,
            active: self.enabled || !self.positions.is_empty() || !self.pending_orders.is_empty(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: self.position_total_exposure(),
            unrealized_pnl: self
                .positions
                .values()
                .filter_map(|position| {
                    position.current_price.map(|price| {
                        (price - position.entry_price) * Decimal::from(position.shares)
                    })
                })
                .sum(),
            realized_pnl_today: self.core.state.daily_realized_pnl_usd,
            last_update: self.last_update,
            metrics,
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions
            .values()
            .map(|position| {
                let mut info = PositionInfo::new(
                    position.token_id.clone(),
                    Side::Up,
                    position.shares,
                    position.entry_price,
                    self.id.clone(),
                );
                if let Some(current_price) = position.current_price {
                    info.update_price(current_price);
                }
                info.opened_at = position.opened_at;
                info.metadata
                    .insert("game_id".to_string(), position.game_id.clone());
                info.metadata.insert(
                    "trailing_abbrev".to_string(),
                    position.trailing_abbrev.clone(),
                );
                info.metadata
                    .insert("market_slug".to_string(), position.market_slug.clone());
                info
            })
            .collect()
    }

    fn is_active(&self) -> bool {
        self.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        let mut actions = Vec::new();
        for client_order_id in self.pending_orders.keys().cloned().collect::<Vec<_>>() {
            actions.push(StrategyAction::CancelOrder {
                order_id: client_order_id,
            });
        }
        if !self.positions.is_empty() {
            actions.push(StrategyAction::Alert {
                level: AlertLevel::Warning,
                message: format!(
                    "nba_comeback shutdown with {} open positions",
                    self.positions.len()
                ),
            });
        }
        actions.push(StrategyAction::LogEvent {
            event: StrategyEvent::new(
                StrategyEventType::StateChanged,
                "NBA comeback strategy shutdown",
            ),
        });
        Ok(actions)
    }

    fn reset(&mut self) {
        self.positions.clear();
        self.pending_orders.clear();
        self.quotes.clear();
        self.manual_live_games.clear();
        self.core.state = Default::default();
        self.enabled = true;
        self.last_update = Utc::now();
    }
}

fn database_url_from_env() -> String {
    std::env::var("PLOY_DATABASE__URL")
        .ok()
        .or_else(|| std::env::var("PLOY__DATABASE__URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "postgres://localhost/unused".to_string())
}

fn default_nba_comeback_config() -> NbaComebackConfig {
    NbaComebackConfig {
        enabled: true,
        min_edge: dec!(0.05),
        max_entry_price: dec!(0.75),
        shares: 50,
        cooldown_secs: 300,
        max_daily_spend_usd: dec!(100),
        min_deficit: 1,
        max_deficit: 15,
        target_quarter: 3,
        espn_poll_interval_secs: 30,
        min_comeback_rate: 0.15,
        season: "2025-26".to_string(),
        grok_enabled: false,
        grok_interval_secs: 300,
        grok_min_edge: dec!(0.08),
        grok_min_confidence: 0.6,
        grok_decision_cooldown_secs: 60,
        grok_fallback_enabled: true,
        min_reward_risk_ratio: 4.0,
        min_expected_value: 0.05,
        kelly_fraction_cap: 0.25,
        performance_daily_loss_limit_usd: dec!(30),
        performance_min_settled_trades: 10,
        performance_min_win_rate: 0.45,
        performance_low_winrate_multiplier: 0.60,
        performance_loss_streak_threshold: 3,
        performance_loss_streak_multiplier: 0.50,
        scaling_enabled: false,
        scaling_max_adds: 3,
        scaling_min_price_drop_pct: 5.0,
        scaling_max_game_exposure_usd: dec!(50),
        scaling_min_comeback_retention: 0.70,
        scaling_min_time_remaining_mins: 8.0,
        early_exit_enabled: true,
        early_exit_take_profit_pct: 15.0,
        early_exit_stop_loss_pct: 20.0,
    }
}

fn apply_nba_comeback_overrides(cfg: &mut NbaComebackConfig, section: &toml::value::Table) {
    if let Some(value) = section.get("enabled").and_then(Value::as_bool) {
        cfg.enabled = value;
    }
    if let Some(value) = section.get("min_edge").and_then(value_to_decimal) {
        cfg.min_edge = value;
    }
    if let Some(value) = section.get("max_entry_price").and_then(value_to_decimal) {
        cfg.max_entry_price = value;
    }
    if let Some(value) = section.get("shares").and_then(value_to_u64) {
        cfg.shares = value;
    }
    if let Some(value) = section.get("cooldown_secs").and_then(value_to_u64) {
        cfg.cooldown_secs = value;
    }
    if let Some(value) = section.get("max_daily_spend_usd").and_then(value_to_decimal) {
        cfg.max_daily_spend_usd = value;
    }
    if let Some(value) = section.get("min_deficit").and_then(value_to_i32) {
        cfg.min_deficit = value;
    }
    if let Some(value) = section.get("max_deficit").and_then(value_to_i32) {
        cfg.max_deficit = value;
    }
    if let Some(value) = section.get("target_quarter").and_then(value_to_u64) {
        cfg.target_quarter = value as u8;
    }
    if let Some(value) = section
        .get("espn_poll_interval_secs")
        .and_then(value_to_u64)
    {
        cfg.espn_poll_interval_secs = value;
    }
    if let Some(value) = section.get("min_comeback_rate").and_then(value_to_f64) {
        cfg.min_comeback_rate = value;
    }
    if let Some(value) = section.get("season").and_then(Value::as_str) {
        cfg.season = value.to_string();
    }
    if let Some(value) = section.get("scaling_enabled").and_then(Value::as_bool) {
        cfg.scaling_enabled = value;
    }
    if let Some(value) = section.get("scaling_max_adds").and_then(value_to_u64) {
        cfg.scaling_max_adds = value as u32;
    }
    if let Some(value) = section
        .get("scaling_min_price_drop_pct")
        .and_then(value_to_f64)
    {
        cfg.scaling_min_price_drop_pct = value;
    }
    if let Some(value) = section
        .get("scaling_max_game_exposure_usd")
        .and_then(value_to_decimal)
    {
        cfg.scaling_max_game_exposure_usd = value;
    }
    if let Some(value) = section
        .get("scaling_min_comeback_retention")
        .and_then(value_to_f64)
    {
        cfg.scaling_min_comeback_retention = value;
    }
    if let Some(value) = section
        .get("scaling_min_time_remaining_mins")
        .and_then(value_to_f64)
    {
        cfg.scaling_min_time_remaining_mins = value;
    }
    if let Some(value) = section.get("early_exit_enabled").and_then(Value::as_bool) {
        cfg.early_exit_enabled = value;
    }
    if let Some(value) = section
        .get("early_exit_take_profit_pct")
        .and_then(value_to_f64)
    {
        cfg.early_exit_take_profit_pct = value;
    }
    if let Some(value) = section
        .get("early_exit_stop_loss_pct")
        .and_then(value_to_f64)
    {
        cfg.early_exit_stop_loss_pct = value;
    }
}

fn value_to_decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::Float(v) => Decimal::try_from(*v).ok(),
        Value::Integer(v) => Some(Decimal::from(*v)),
        Value::String(v) => Decimal::from_str(v).ok(),
        _ => None,
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::Float(v) if *v >= 0.0 => Some(*v as u64),
        Value::String(v) => v.parse::<u64>().ok(),
        _ => None,
    }
}

fn value_to_i32(value: &Value) -> Option<i32> {
    match value {
        Value::Integer(v) => i32::try_from(*v).ok(),
        Value::Float(v) => Some(*v as i32),
        Value::String(v) => v.parse::<i32>().ok(),
        _ => None,
    }
}

fn value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(v) => Some(*v as f64),
        Value::Float(v) => Some(*v),
        Value::String(v) => v.parse::<f64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::domain::{OrderStatus, Quote, Side};
    use crate::strategy::traits::{DataFeed, MarketUpdate, OrderUpdate, Strategy, StrategyAction};

    fn test_candidate(
        game_id: &str,
        trailing_abbrev: &str,
        deficit: i32,
        adjusted_win_prob: f64,
    ) -> ComebackCandidate {
        ComebackCandidate {
            game: LiveGame {
                espn_game_id: game_id.to_string(),
                home_team: "Los Angeles Lakers".to_string(),
                away_team: "Boston Celtics".to_string(),
                home_abbrev: "LAL".to_string(),
                away_abbrev: "BOS".to_string(),
                home_score: 80,
                away_score: 88,
                quarter: 3,
                clock: "5:00".to_string(),
                time_remaining_mins: 17.0,
                status: GameStatus::InProgress,
                home_quarter_scores: vec![],
                away_quarter_scores: vec![],
            },
            trailing_team: if trailing_abbrev == "LAL" {
                "Los Angeles Lakers".to_string()
            } else {
                "Boston Celtics".to_string()
            },
            trailing_abbrev: trailing_abbrev.to_string(),
            deficit,
            comeback_rate: 0.22,
            adjusted_win_prob,
        }
    }

    fn test_quote(token_id: &str, bid: Decimal, ask: Decimal) -> MarketUpdate {
        MarketUpdate::PolymarketQuote {
            token_id: token_id.to_string(),
            side: Side::Up,
            quote: Quote {
                side: Side::Up,
                best_bid: Some(bid),
                best_ask: Some(ask),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(220)),
                timestamp: Utc::now(),
            },
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_from_toml_builds_nba_strategy_with_expected_name() {
        let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
shares = 25
espn_poll_interval_secs = 45
min_edge = 0.09
max_entry_price = 0.60
season = "2025-26"
"#;

        let strategy = NbaComebackStrategy::from_toml("test".into(), toml, true).unwrap();
        assert_eq!(strategy.name(), "NBA Comeback");
        assert_eq!(strategy.config().shares, 25);
        assert_eq!(strategy.config().espn_poll_interval_secs, 45);
        assert_eq!(strategy.config().min_edge, dec!(0.09));
        assert_eq!(strategy.config().max_entry_price, dec!(0.60));
        assert_eq!(
            strategy.required_feeds(),
            vec![DataFeed::Tick { interval_ms: 45_000 }]
        );
    }

    #[tokio::test]
    async fn test_on_tick_emits_entry_order_for_registered_market() {
        let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
shares = 25
"#;

        let mut strategy = NbaComebackStrategy::from_toml("test".into(), toml, true).unwrap();
        strategy.register_market(NbaComebackMarketRegistration {
            game_id: "game-1".to_string(),
            trailing_abbrev: "LAL".to_string(),
            market_slug: "lakers-comeback".to_string(),
            token_id: "token-lal".to_string(),
        });
        strategy.test_candidates = Some(vec![test_candidate("game-1", "LAL", 8, 0.42)]);
        strategy
            .on_market_update(&test_quote("token-lal", dec!(0.24), dec!(0.25)))
            .await
            .unwrap();

        let actions = strategy.on_tick(Utc::now()).await.unwrap();
        assert_eq!(actions.len(), 1);

        match &actions[0] {
            StrategyAction::SubmitOrder { order, .. } => {
                assert_eq!(order.token_id, "token-lal");
                assert_eq!(order.shares, 25);
                assert_eq!(order.limit_price, dec!(0.25));
            }
            other => panic!("expected submit order, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_filled_entry_projects_position_info() {
        let toml = r#"
[strategy]
name = "nba_comeback"

[nba_comeback]
shares = 30
"#;

        let mut strategy = NbaComebackStrategy::from_toml("test".into(), toml, true).unwrap();
        strategy.register_market(NbaComebackMarketRegistration {
            game_id: "game-1".to_string(),
            trailing_abbrev: "LAL".to_string(),
            market_slug: "lakers-comeback".to_string(),
            token_id: "token-lal".to_string(),
        });
        strategy.test_candidates = Some(vec![test_candidate("game-1", "LAL", 8, 0.42)]);
        strategy
            .on_market_update(&test_quote("token-lal", dec!(0.24), dec!(0.25)))
            .await
            .unwrap();

        let actions = strategy.on_tick(Utc::now()).await.unwrap();
        let client_order_id = match &actions[0] {
            StrategyAction::SubmitOrder {
                client_order_id, ..
            } => client_order_id.clone(),
            other => panic!("expected submit order, got {other:?}"),
        };

        strategy
            .on_order_update(&OrderUpdate {
                order_id: "order-1".to_string(),
                client_order_id: Some(client_order_id),
                status: OrderStatus::Filled,
                filled_qty: 30,
                avg_fill_price: Some(dec!(0.25)),
                timestamp: Utc::now(),
                error: None,
            })
            .await
            .unwrap();

        let positions = strategy.positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].token_id, "token-lal");
        assert_eq!(positions[0].shares, 30);
        assert_eq!(positions[0].entry_price, dec!(0.25));
        assert_eq!(strategy.state().position_count, 1);
        assert_eq!(strategy.state().pending_order_count, 0);
    }
}
