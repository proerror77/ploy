use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::warn;

use crate::ai_clients::{EventDetails, LiveGameMarket, PolymarketSportsClient, NBA_SERIES_ID};
use crate::config::NbaComebackConfig;
use crate::domain::{OrderRequest, OrderStatus, Side};
use crate::error::Result;
use crate::strategy::nba_comeback::core::ComebackCandidate;
use crate::strategy::nba_comeback::{
    ComebackOpportunity, ComebackStatsProvider, EspnClient, NbaComebackCore,
};
use crate::strategy::traits::{
    DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction, StrategyEvent,
    StrategyEventType, StrategyStateInfo,
};

const NBA_TEAM_ABBREVS: &[&str] = &[
    "ATL", "BOS", "BKN", "CHA", "CHI", "CLE", "DAL", "DEN", "DET", "GSW", "HOU", "IND", "LAC",
    "LAL", "MEM", "MIA", "MIL", "MIN", "NOP", "NYK", "OKC", "ORL", "PHI", "PHX", "POR", "SAC",
    "SAS", "TOR", "UTA", "WAS",
];

#[derive(Debug, Clone)]
struct PendingNbaOrder {
    opportunity: ComebackOpportunity,
    requested_shares: u64,
    entry_recorded: bool,
    recorded_filled_qty: u64,
}

pub struct NbaComebackStrategy {
    id: String,
    core: NbaComebackCore,
    enabled: bool,
    poll_interval_secs: u64,
    pm_sports: Option<PolymarketSportsClient>,
    pending_orders: HashMap<String, PendingNbaOrder>,
    positions: HashMap<String, PositionInfo>,
    last_update: DateTime<Utc>,
}

impl NbaComebackStrategy {
    pub fn from_toml(id: String, config_str: &str, _dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow::anyhow!("Invalid TOML: {}", e))?;

        let empty_table = Value::Table(Default::default());
        let strategy = config.get("strategy").unwrap_or(&empty_table);
        let entry = config.get("entry").unwrap_or(&empty_table);
        let timing = config.get("timing").unwrap_or(&empty_table);
        let risk = config.get("risk").unwrap_or(&empty_table);
        let scan = config.get("scan").unwrap_or(&empty_table);
        let database = config.get("database").unwrap_or(&empty_table);
        let grok = config.get("grok").unwrap_or(&empty_table);
        let performance = config.get("performance").unwrap_or(&empty_table);
        let scaling = config.get("scaling").unwrap_or(&empty_table);
        let exit = config.get("exit").unwrap_or(&empty_table);

        let cfg = NbaComebackConfig {
            enabled: bool_value(strategy.get("enabled"), true),
            min_edge: decimal_value(entry.get("min_edge"), Decimal::new(5, 2)),
            max_entry_price: decimal_value(entry.get("max_entry_price"), Decimal::new(75, 2)),
            shares: u64_value(entry.get("shares"), 50),
            cooldown_secs: u64_value(risk.get("cooldown_secs"), 300),
            max_daily_spend_usd: decimal_value(
                risk.get("max_daily_spend_usd"),
                Decimal::new(100, 0),
            ),
            min_deficit: i32_value(scan.get("min_deficit"), 1),
            max_deficit: i32_value(scan.get("max_deficit"), 15),
            target_quarter: u8_value(scan.get("target_quarter"), 3),
            espn_poll_interval_secs: u64_value(timing.get("poll_interval_secs"), 30),
            min_comeback_rate: f64_value(scan.get("min_comeback_rate"), 0.15),
            season: string_value(scan.get("season"), "2025-26"),
            grok_enabled: bool_value(grok.get("enabled"), false),
            grok_interval_secs: u64_value(grok.get("interval_secs"), 300),
            grok_min_edge: decimal_value(grok.get("min_edge"), Decimal::new(8, 2)),
            grok_min_confidence: f64_value(grok.get("min_confidence"), 0.6),
            grok_decision_cooldown_secs: u64_value(grok.get("decision_cooldown_secs"), 60),
            grok_fallback_enabled: bool_value(grok.get("fallback_enabled"), true),
            min_reward_risk_ratio: f64_value(risk.get("min_reward_risk_ratio"), 4.0),
            min_expected_value: f64_value(risk.get("min_expected_value"), 0.05),
            kelly_fraction_cap: f64_value(risk.get("kelly_fraction_cap"), 0.25),
            performance_daily_loss_limit_usd: decimal_value(
                performance.get("daily_loss_limit_usd"),
                Decimal::new(30, 0),
            ),
            performance_min_settled_trades: u64_value(performance.get("min_settled_trades"), 10),
            performance_min_win_rate: f64_value(performance.get("min_win_rate"), 0.45),
            performance_low_winrate_multiplier: f64_value(
                performance.get("low_winrate_multiplier"),
                0.60,
            ),
            performance_loss_streak_threshold: u32_value(
                performance.get("loss_streak_threshold"),
                3,
            ),
            performance_loss_streak_multiplier: f64_value(
                performance.get("loss_streak_multiplier"),
                0.50,
            ),
            scaling_enabled: bool_value(scaling.get("enabled"), false),
            scaling_max_adds: u32_value(scaling.get("max_adds"), 3),
            scaling_min_price_drop_pct: f64_value(scaling.get("min_price_drop_pct"), 5.0),
            scaling_max_game_exposure_usd: decimal_value(
                scaling.get("max_game_exposure_usd"),
                Decimal::new(50, 0),
            ),
            scaling_min_comeback_retention: f64_value(scaling.get("min_comeback_retention"), 0.70),
            scaling_min_time_remaining_mins: f64_value(scaling.get("min_time_remaining_mins"), 8.0),
            early_exit_enabled: bool_value(exit.get("enabled"), true),
            early_exit_take_profit_pct: f64_value(exit.get("take_profit_pct"), 15.0),
            early_exit_stop_loss_pct: f64_value(exit.get("stop_loss_pct"), 20.0),
        };

        let database_url = database
            .get("url")
            .and_then(|v| v.as_str())
            .map(ToString::to_string)
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .ok_or_else(|| {
                anyhow::anyhow!("nba_comeback strategy requires [database].url or DATABASE_URL")
            })?;
        let espn = EspnClient::new();
        let stats = ComebackStatsProvider::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy(&database_url)
                .map_err(|e| anyhow::anyhow!("failed to create lazy stats pool: {}", e))?,
            cfg.season.clone(),
        );
        let pm_sports = Some(PolymarketSportsClient::new()?);

        Ok(Self {
            id,
            enabled: cfg.enabled,
            poll_interval_secs: cfg.espn_poll_interval_secs,
            core: NbaComebackCore::new(espn, stats, cfg),
            pm_sports,
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            last_update: Utc::now(),
        })
    }

    #[cfg(test)]
    fn new_for_tests(id: &str, core: NbaComebackCore) -> Self {
        let enabled = core.cfg.enabled;
        let poll_interval_secs = core.cfg.espn_poll_interval_secs;
        Self {
            id: id.to_string(),
            enabled,
            poll_interval_secs,
            core,
            pm_sports: None,
            pending_orders: HashMap::new(),
            positions: HashMap::new(),
            last_update: Utc::now(),
        }
    }

    fn normalize_text(value: &str) -> String {
        value
            .to_ascii_lowercase()
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
            .collect::<String>()
    }

    fn is_valid_nba_abbrev(abbrev: &str) -> bool {
        NBA_TEAM_ABBREVS.contains(&abbrev.to_ascii_uppercase().as_str())
    }

    fn is_valid_nba_game(game: &crate::strategy::nba_comeback::LiveGame) -> bool {
        Self::is_valid_nba_abbrev(&game.home_abbrev) && Self::is_valid_nba_abbrev(&game.away_abbrev)
    }

    fn event_matches_game(
        event: &EventDetails,
        game: &crate::strategy::nba_comeback::LiveGame,
    ) -> bool {
        if let Some(game_id) = event.game_id {
            if game_id.to_string() == game.espn_game_id {
                return true;
            }
        }

        let title_norm = Self::normalize_text(&event.title);
        let home_team = Self::normalize_text(&game.home_team);
        let away_team = Self::normalize_text(&game.away_team);
        let home_abbrev = Self::normalize_text(&game.home_abbrev);
        let away_abbrev = Self::normalize_text(&game.away_abbrev);

        (title_norm.contains(home_team.trim()) && title_norm.contains(away_team.trim()))
            || (title_norm.contains(home_abbrev.trim()) && title_norm.contains(away_abbrev.trim()))
    }

    fn find_matching_pm_event<'a>(
        game: &crate::strategy::nba_comeback::LiveGame,
        pm_events: &'a [EventDetails],
    ) -> Option<&'a EventDetails> {
        pm_events
            .iter()
            .find(|event| Self::event_matches_game(event, game))
    }

    fn text_matches_team(text: &str, team_name: &str, team_abbrev: &str) -> bool {
        let text_norm = Self::normalize_text(text);
        let name_norm = Self::normalize_text(team_name);
        let abbrev_norm = Self::normalize_text(team_abbrev);

        text_norm.contains(name_norm.trim()) || text_norm.contains(abbrev_norm.trim())
    }

    fn select_trailing_market(
        market: &LiveGameMarket,
        trailing_team: &str,
        trailing_abbrev: &str,
    ) -> Option<(String, Decimal)> {
        let outcomes = market
            .outcomes
            .as_ref()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())?;
        let (price_a, price_b) = market.get_prices()?;
        let (token_a, token_b) = market.get_token_ids()?;

        outcomes
            .iter()
            .enumerate()
            .take(2)
            .find_map(|(idx, outcome)| {
                if !Self::text_matches_team(outcome, trailing_team, trailing_abbrev) {
                    return None;
                }
                if idx == 0 {
                    Some((token_a.clone(), price_a))
                } else {
                    Some((token_b.clone(), price_b))
                }
            })
    }

    fn resolve_opportunity(
        &self,
        candidate: &ComebackCandidate,
        pm_events: &[EventDetails],
    ) -> Option<ComebackOpportunity> {
        let event = Self::find_matching_pm_event(&candidate.game, pm_events)?;
        let moneyline = event.moneyline()?;
        let (token_id, market_price) = Self::select_trailing_market(
            moneyline,
            &candidate.trailing_team,
            &candidate.trailing_abbrev,
        )?;

        self.core
            .evaluate_opportunity(candidate, market_price, event.slug.clone(), token_id)
    }

    fn build_submit_action(
        &mut self,
        opportunity: ComebackOpportunity,
        requested_shares: u64,
    ) -> StrategyAction {
        let client_order_id = format!(
            "nba_comeback_{}_{}_{}",
            opportunity.game.espn_game_id,
            opportunity.token_id,
            Utc::now().timestamp_millis()
        );
        let mut order = OrderRequest::buy_limit(
            opportunity.token_id.clone(),
            Side::Up,
            requested_shares,
            opportunity.market_price,
        );
        order.client_order_id = client_order_id.clone();
        order.idempotency_key = Some(client_order_id.clone());

        self.pending_orders.insert(
            client_order_id.clone(),
            PendingNbaOrder {
                opportunity,
                requested_shares,
                entry_recorded: false,
                recorded_filled_qty: 0,
            },
        );

        StrategyAction::SubmitOrder {
            client_order_id,
            purpose: crate::strategy::OrderPurpose::from_order_request(&order),
            order,
            priority: 5,
        }
    }

    fn sync_position_from_core(&mut self, pending: &PendingNbaOrder) {
        let game_id = &pending.opportunity.game.espn_game_id;
        let Some(pos) = self.core.state.game_positions.get(game_id) else {
            return;
        };
        if pos.total_shares == 0 {
            self.positions.remove(game_id);
            return;
        }

        let avg_entry_price = pos.total_cost / Decimal::from(pos.total_shares);
        let mut info = PositionInfo::new(
            pending.opportunity.token_id.clone(),
            Side::Up,
            pos.total_shares,
            avg_entry_price,
            self.id.clone(),
        );
        info.current_price = Some(pending.opportunity.market_price);
        info.metadata.insert("game_id".to_string(), game_id.clone());
        info.metadata.insert(
            "trailing_team".to_string(),
            pending.opportunity.trailing_abbrev.clone(),
        );
        info.metadata.insert(
            "market_slug".to_string(),
            pending.opportunity.market_slug.clone(),
        );
        self.positions.insert(game_id.clone(), info);
    }

    fn build_actions_from_candidates(
        &mut self,
        candidates: &[ComebackCandidate],
        pm_events: &[EventDetails],
    ) -> Vec<StrategyAction> {
        let mut actions = Vec::new();
        for candidate in candidates {
            let Some(opportunity) = self.resolve_opportunity(candidate, pm_events) else {
                continue;
            };

            if self
                .core
                .is_duplicate_initial_entry(&opportunity.game.espn_game_id, &opportunity.token_id)
            {
                continue;
            }
            if self.pending_orders.values().any(|pending| {
                pending.opportunity.game.espn_game_id == opportunity.game.espn_game_id
            }) {
                continue;
            }

            let requested_shares = self.core.adjusted_shares(self.core.cfg.shares);
            actions.push(StrategyAction::LogEvent {
                event: StrategyEvent::new(
                    StrategyEventType::SignalDetected,
                    format!(
                        "nba_comeback signal game={} team={} edge={:.3}",
                        opportunity.game.espn_game_id,
                        opportunity.trailing_abbrev,
                        opportunity.edge
                    ),
                ),
            });
            actions.push(self.build_submit_action(opportunity, requested_shares));
        }
        actions
    }
}

#[async_trait]
impl Strategy for NbaComebackStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "NBA Comeback Strategy"
    }

    fn description(&self) -> &str {
        "Q3-to-Q4 NBA comeback scanner with canonical Strategy-plane output"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.poll_interval_secs.saturating_mul(1000),
        }]
    }

    async fn on_market_update(&mut self, _update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        self.last_update = update.timestamp;
        let Some(client_order_id) = update.client_order_id.as_deref() else {
            return Ok(Vec::new());
        };

        match update.status {
            OrderStatus::PartiallyFilled | OrderStatus::Filled => {
                let maybe_pending = self.pending_orders.get(client_order_id).cloned();
                if let Some(mut pending) = maybe_pending {
                    let cumulative_filled = if update.filled_qty > 0 {
                        update.filled_qty
                    } else if matches!(update.status, OrderStatus::Filled) {
                        pending.requested_shares
                    } else {
                        0
                    };
                    let delta = cumulative_filled.saturating_sub(pending.recorded_filled_qty);
                    if delta > 0 {
                        let fill_price = update
                            .avg_fill_price
                            .unwrap_or(pending.opportunity.market_price);
                        if !pending.entry_recorded {
                            self.core.state.record_initial_entry(
                                &pending.opportunity.game.espn_game_id,
                                &pending.opportunity.token_id,
                            );
                            pending.entry_recorded = true;
                        }
                        self.core.record_trade(
                            &pending.opportunity.game.espn_game_id,
                            fill_price * Decimal::from(delta),
                        );
                        self.core.record_position_entry_with_market_and_team(
                            &pending.opportunity.game.espn_game_id,
                            &pending.opportunity.trailing_abbrev,
                            &pending.opportunity.market_slug,
                            &pending.opportunity.token_id,
                            fill_price,
                            delta,
                            pending.opportunity.comeback_rate,
                        );
                        pending.recorded_filled_qty = cumulative_filled;
                        self.pending_orders
                            .insert(client_order_id.to_string(), pending.clone());
                        self.sync_position_from_core(&pending);
                    }
                    if matches!(update.status, OrderStatus::Filled) {
                        self.pending_orders.remove(client_order_id);
                    }
                }
            }
            OrderStatus::Rejected
            | OrderStatus::Cancelled
            | OrderStatus::Expired
            | OrderStatus::Failed => {
                self.pending_orders.remove(client_order_id);
            }
            OrderStatus::Pending | OrderStatus::Submitted => {}
        }

        Ok(Vec::new())
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        self.last_update = now;
        if !self.enabled {
            return Ok(Vec::new());
        }

        self.core.reset_daily_if_needed();
        let Some(pm_sports) = self.pm_sports.as_ref() else {
            return Ok(Vec::new());
        };
        if self.core.stats.team_count() == 0 {
            if let Err(error) = self.core.stats.load_all().await {
                warn!(
                    strategy = self.id.as_str(),
                    season = self.core.cfg.season.as_str(),
                    error = %error,
                    "nba_comeback strategy failed to load comeback stats"
                );
                return Ok(Vec::new());
            }
        }

        let mut live_games = self.core.espn.fetch_live_games().await?;
        live_games.retain(Self::is_valid_nba_game);
        if live_games.is_empty() {
            return Ok(Vec::new());
        }

        let candidates = self.core.scan_games(&live_games);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let pm_events = pm_sports
            .fetch_todays_games_with_details(NBA_SERIES_ID)
            .await?;
        Ok(self.build_actions_from_candidates(&candidates, &pm_events))
    }

    fn state(&self) -> StrategyStateInfo {
        let settled_trades = self.core.state.settled_trades;
        let win_rate = self.core.settled_win_rate().unwrap_or(0.0);
        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: "monitoring".to_string(),
            enabled: self.enabled,
            active: self.is_active(),
            position_count: self.positions.len(),
            pending_order_count: self.pending_orders.len(),
            total_exposure: self.positions.values().fold(Decimal::ZERO, |acc, pos| {
                acc + Decimal::from(pos.shares) * pos.entry_price
            }),
            unrealized_pnl: self
                .positions
                .values()
                .fold(Decimal::ZERO, |acc, pos| acc + pos.unrealized_pnl),
            realized_pnl_today: self.core.state.daily_realized_pnl_usd,
            last_update: self.last_update,
            metrics: HashMap::from([
                (
                    "daily_spend_usd".to_string(),
                    self.core.state.daily_spend_usd.to_string(),
                ),
                ("settled_trades".to_string(), settled_trades.to_string()),
                ("win_rate".to_string(), format!("{:.3}", win_rate)),
            ]),
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        self.positions.values().cloned().collect()
    }

    fn is_active(&self) -> bool {
        !self.positions.is_empty() || !self.pending_orders.is_empty()
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        let mut actions = Vec::new();
        for (game_id, position) in &self.positions {
            if position.shares == 0 {
                continue;
            }
            let limit_price = position.current_price.unwrap_or(position.entry_price);
            let client_order_id = format!(
                "nba_comeback_shutdown_{}_{}_{}",
                game_id,
                position.token_id,
                Utc::now().timestamp_millis()
            );
            let mut order = OrderRequest::sell_limit(
                position.token_id.clone(),
                Side::Up,
                position.shares,
                limit_price,
            );
            order.client_order_id = client_order_id.clone();
            order.idempotency_key = Some(client_order_id.clone());
            actions.push(StrategyAction::SubmitOrder {
                client_order_id,
                purpose: crate::strategy::OrderPurpose::Exit,
                order,
                priority: 10,
            });
        }
        Ok(actions)
    }

    fn reset(&mut self) {
        self.pending_orders.clear();
        self.positions.clear();
        self.core.state = Default::default();
        self.last_update = Utc::now();
        self.enabled = true;
    }
}

fn bool_value(value: Option<&toml::Value>, default: bool) -> bool {
    value.and_then(|v| v.as_bool()).unwrap_or(default)
}

fn string_value(value: Option<&toml::Value>, default: &str) -> String {
    value
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| default.to_string())
}

fn i32_value(value: Option<&toml::Value>, default: i32) -> i32 {
    value
        .and_then(|v| v.as_integer())
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(default)
}

fn u8_value(value: Option<&toml::Value>, default: u8) -> u8 {
    value
        .and_then(|v| v.as_integer())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(default)
}

fn u32_value(value: Option<&toml::Value>, default: u32) -> u32 {
    value
        .and_then(|v| v.as_integer())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(default)
}

fn u64_value(value: Option<&toml::Value>, default: u64) -> u64 {
    value
        .and_then(|v| v.as_integer())
        .and_then(|v| u64::try_from(v).ok())
        .unwrap_or(default)
}

fn f64_value(value: Option<&toml::Value>, default: f64) -> f64 {
    value
        .and_then(|v| match v {
            toml::Value::Float(inner) => Some(*inner),
            toml::Value::Integer(inner) => Some(*inner as f64),
            toml::Value::String(inner) => inner.parse::<f64>().ok(),
            _ => None,
        })
        .unwrap_or(default)
}

fn decimal_value(value: Option<&toml::Value>, default: Decimal) -> Decimal {
    value
        .and_then(|v| match v {
            toml::Value::Float(inner) => inner.to_string().parse::<Decimal>().ok(),
            toml::Value::Integer(inner) => Some(Decimal::from(*inner)),
            toml::Value::String(inner) => inner.parse::<Decimal>().ok(),
            _ => None,
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame, QuarterScore};
    use rust_decimal_macros::dec;

    fn test_cfg() -> NbaComebackConfig {
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

    fn test_core() -> NbaComebackCore {
        let cfg = test_cfg();
        let stats = ComebackStatsProvider::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/unused")
                .expect("lazy pool"),
            cfg.season.clone(),
        );
        NbaComebackCore::new(EspnClient::new(), stats, cfg)
    }

    fn sample_game() -> LiveGame {
        LiveGame {
            espn_game_id: "401".to_string(),
            home_team: "Boston Celtics".to_string(),
            away_team: "Los Angeles Lakers".to_string(),
            home_abbrev: "BOS".to_string(),
            away_abbrev: "LAL".to_string(),
            home_score: 92,
            away_score: 84,
            quarter: 3,
            clock: "01:30".to_string(),
            time_remaining_mins: 13.5,
            status: GameStatus::InProgress,
            home_quarter_scores: vec![QuarterScore {
                period: 1,
                points: 30.0,
            }],
            away_quarter_scores: vec![QuarterScore {
                period: 1,
                points: 26.0,
            }],
        }
    }

    fn sample_candidate() -> ComebackCandidate {
        ComebackCandidate {
            game: sample_game(),
            trailing_team: "Los Angeles Lakers".to_string(),
            trailing_abbrev: "LAL".to_string(),
            deficit: 8,
            comeback_rate: 0.24,
            adjusted_win_prob: 0.42,
        }
    }

    fn sample_event() -> EventDetails {
        EventDetails {
            id: "evt-1".to_string(),
            title: "Los Angeles Lakers vs Boston Celtics".to_string(),
            slug: "lakers-vs-celtics".to_string(),
            closed: false,
            markets: vec![LiveGameMarket {
                question: "Los Angeles Lakers vs Boston Celtics".to_string(),
                condition_id: Some("cond-1".to_string()),
                outcome_prices: Some("[\"0.31\",\"0.69\"]".to_string()),
                clob_token_ids: Some("[\"token-lal\",\"token-bos\"]".to_string()),
                volume: Some(1234.0),
                outcomes: Some("[\"Los Angeles Lakers\",\"Boston Celtics\"]".to_string()),
            }],
            score: Some("92-84".to_string()),
            live: true,
            period: Some("Q3".to_string()),
            elapsed: Some("01:30".to_string()),
            ended: false,
            game_id: Some(401),
            event_date: Some("2026-03-06".to_string()),
            start_time: None,
            volume: Some(1234.0),
        }
    }

    #[tokio::test]
    async fn from_toml_parses_runtime_shape() {
        let toml = r#"
[strategy]
name = "nba_comeback"
plugin_id = "sports.nba_comeback.v1"
enabled = true

[entry]
min_edge = 0.07
max_entry_price = 0.68
shares = 25

[timing]
poll_interval_secs = 45

[risk]
cooldown_secs = 180
max_daily_spend_usd = 55.0

[scan]
min_deficit = 4
max_deficit = 12
target_quarter = 3
season = "2026-27"

[database]
url = "postgres://localhost/unused"
"#;

        let strategy = NbaComebackStrategy::from_toml("nba_test".to_string(), toml, true)
            .expect("strategy should parse");

        assert_eq!(
            strategy.required_feeds(),
            vec![DataFeed::Tick {
                interval_ms: 45_000
            }]
        );
        assert_eq!(strategy.core.cfg.min_edge, dec!(0.07));
        assert_eq!(strategy.core.cfg.max_entry_price, dec!(0.68));
        assert_eq!(strategy.core.cfg.shares, 25);
        assert_eq!(strategy.core.cfg.cooldown_secs, 180);
        assert_eq!(strategy.core.cfg.min_deficit, 4);
        assert_eq!(strategy.core.cfg.max_deficit, 12);
        assert_eq!(strategy.core.cfg.season, "2026-27");
    }

    #[tokio::test]
    async fn build_actions_matches_trailing_market() {
        let mut strategy = NbaComebackStrategy::new_for_tests("nba_test", test_core());
        let actions =
            strategy.build_actions_from_candidates(&[sample_candidate()], &[sample_event()]);

        assert_eq!(actions.len(), 2);
        match &actions[1] {
            StrategyAction::SubmitOrder {
                client_order_id,
                order,
                ..
            } => {
                assert!(client_order_id.starts_with("nba_comeback_401_token-lal_"));
                assert_eq!(order.token_id, "token-lal");
                assert_eq!(order.market_side, Side::Up);
                assert_eq!(order.shares, 50);
                assert_eq!(order.limit_price, dec!(0.31));
            }
            other => panic!("expected submit order action, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn order_update_filled_records_position() {
        let mut strategy = NbaComebackStrategy::new_for_tests("nba_test", test_core());
        let opportunity = strategy
            .resolve_opportunity(&sample_candidate(), &[sample_event()])
            .expect("opportunity");
        let action = strategy.build_submit_action(opportunity, 25);

        let StrategyAction::SubmitOrder {
            client_order_id, ..
        } = action
        else {
            panic!("expected submit order");
        };

        strategy
            .on_order_update(&OrderUpdate {
                order_id: client_order_id.clone(),
                client_order_id: Some(client_order_id),
                status: OrderStatus::Filled,
                filled_qty: 25,
                avg_fill_price: Some(dec!(0.30)),
                timestamp: Utc::now(),
                error: None,
            })
            .await
            .expect("filled order should update strategy");

        assert_eq!(strategy.positions.len(), 1);
        let position = strategy
            .positions
            .get("401")
            .expect("position should be keyed by game id");
        assert_eq!(position.token_id, "token-lal");
        assert_eq!(position.shares, 25);
        assert_eq!(position.entry_price, dec!(0.30));
        assert_eq!(
            position.metadata.get("market_slug"),
            Some(&"lakers-vs-celtics".to_string())
        );
        assert!(strategy
            .core
            .state
            .is_initial_entry_recorded("401", "token-lal"));
    }

    #[tokio::test]
    async fn rejected_order_does_not_consume_entry_budget() {
        let mut strategy = NbaComebackStrategy::new_for_tests("nba_test", test_core());
        let opportunity = strategy
            .resolve_opportunity(&sample_candidate(), &[sample_event()])
            .expect("opportunity");
        let action = strategy.build_submit_action(opportunity, 25);

        let StrategyAction::SubmitOrder {
            client_order_id, ..
        } = action
        else {
            panic!("expected submit order");
        };

        strategy
            .on_order_update(&OrderUpdate {
                order_id: client_order_id.clone(),
                client_order_id: Some(client_order_id),
                status: OrderStatus::Rejected,
                filled_qty: 0,
                avg_fill_price: None,
                timestamp: Utc::now(),
                error: Some("rejected".to_string()),
            })
            .await
            .expect("rejected order should update strategy");

        assert_eq!(strategy.core.state.daily_spend_usd, Decimal::ZERO);
        assert!(!strategy
            .core
            .state
            .is_initial_entry_recorded("401", "token-lal"));
        assert!(strategy.pending_orders.is_empty());
    }

    #[tokio::test]
    async fn shutdown_emits_exit_orders_for_open_positions() {
        let mut strategy = NbaComebackStrategy::new_for_tests("nba_test", test_core());
        let opportunity = strategy
            .resolve_opportunity(&sample_candidate(), &[sample_event()])
            .expect("opportunity");
        let action = strategy.build_submit_action(opportunity, 25);

        let StrategyAction::SubmitOrder {
            client_order_id, ..
        } = action
        else {
            panic!("expected submit order");
        };

        strategy
            .on_order_update(&OrderUpdate {
                order_id: client_order_id.clone(),
                client_order_id: Some(client_order_id),
                status: OrderStatus::Filled,
                filled_qty: 25,
                avg_fill_price: Some(dec!(0.30)),
                timestamp: Utc::now(),
                error: None,
            })
            .await
            .expect("filled order should update strategy");

        let actions = strategy.shutdown().await.expect("shutdown actions");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            StrategyAction::SubmitOrder { order, .. } => {
                assert_eq!(order.order_side, crate::domain::OrderSide::Sell);
                assert_eq!(order.shares, 25);
                assert_eq!(order.limit_price, dec!(0.31));
            }
            other => panic!("expected submit order action, got {:?}", other),
        }
    }
}
