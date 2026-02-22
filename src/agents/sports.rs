//! SportsTradingAgent — pull-based agent for NBA comeback strategy
//!
//! Polls ESPN on a 30s interval, runs NbaComebackCore logic,
//! and submits OrderIntents via the coordinator.

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::agent::grok::GrokClient;
use crate::agent::polymarket_sports::{OrderBookLevel as SportsOrderBookLevel, SportsOrderBook};
use crate::agent::{EventDetails, LiveGameMarket, PolymarketSportsClient, NBA_SERIES_ID};
use crate::agents::{AgentContext, TradingAgent};
use crate::collector::{
    ensure_collector_token_targets_table, upsert_collector_token_targets, CollectorTokenTarget,
};
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::Result;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::nba_comeback::core::{
    ComebackCandidate, ComebackOpportunity, GamePosition, NbaComebackCore, NbaComebackState,
};
use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame};
use crate::strategy::nba_comeback::grok_decision::{
    self, ComebackSnapshot, DecisionTrigger, GrokDecision, MarketSnapshot, RiskMetrics,
    UnifiedDecisionRequest,
};
use crate::strategy::nba_comeback::grok_intel::{
    self, GrokGameIntel, GrokSignalEvaluator, GrokTradeSignal,
};

/// Configuration for the SportsTradingAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportsTradingConfig {
    /// DB account scope (single DB multi-account).
    #[serde(default = "default_account_id")]
    pub account_id: String,
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for SportsTradingConfig {
    fn default() -> Self {
        Self {
            account_id: default_account_id(),
            agent_id: "sports".into(),
            name: "NBA Comeback".into(),
            poll_interval_secs: 30,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

fn default_account_id() -> String {
    "default".to_string()
}

/// Pull-based sports trading agent wrapping NbaComebackCore
pub struct SportsTradingAgent {
    config: SportsTradingConfig,
    core: NbaComebackCore,
    observation_pool: Option<PgPool>,
    pm_sports: Option<PolymarketSportsClient>,
    grok: Option<GrokClient>,
    grok_cache: HashMap<String, GrokGameIntel>,
    /// Per-game cooldown for unified Grok decision requests (game_id → last decision time)
    decision_cooldown: HashMap<String, std::time::Instant>,
}

#[derive(Debug, Clone, Default)]
struct MarketObservation {
    pm_event_id: Option<String>,
    pm_event_title: Option<String>,
    pm_event_slug: Option<String>,
    pm_live_status: Option<String>,
    pm_yes_token_id: Option<String>,
    pm_no_token_id: Option<String>,
    pm_yes_mid: Option<Decimal>,
    pm_no_mid: Option<Decimal>,
    pm_yes_best_bid: Option<Decimal>,
    pm_yes_best_ask: Option<Decimal>,
    pm_no_best_bid: Option<Decimal>,
    pm_no_best_ask: Option<Decimal>,
    pm_trailing_token_id: Option<String>,
    pm_trailing_price: Option<Decimal>,
    pm_trailing_price_source: Option<String>,
    pm_yes_book: Option<SportsOrderBook>,
    pm_no_book: Option<SportsOrderBook>,
}

#[derive(Debug, Clone, Default)]
struct MarketInput {
    market_slug: Option<String>,
    trailing_token_id: Option<String>,
    trailing_price: Option<Decimal>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct DepthLevelJson {
    price: String,
    size: String,
}

const CALENDAR_LOOKBACK_DAYS: i64 = 1;
const CALENDAR_LOOKAHEAD_DAYS: i64 = 7;
const CALENDAR_SYNC_INTERVAL_SECS: i64 = 30 * 60; // 30 minutes
const DEPLOYMENT_ID_NBA_COMEBACK: &str = "sports.pm.nba.comeback";
const DEPLOYMENT_ID_NBA_GROK_UNIFIED: &str = "sports.pm.nba.grok_unified";

const NBA_TEAM_ABBREVS: &[&str] = &[
    "ATL", "BOS", "BKN", "CHA", "CHI", "CLE", "DAL", "DEN", "DET", "GSW", "HOU", "IND", "LAC",
    "LAL", "MEM", "MIA", "MIL", "MIN", "NOP", "NYK", "OKC", "ORL", "PHI", "PHX", "POR", "SAC",
    "SAS", "TOR", "UTA", "WAS",
];

impl SportsTradingAgent {
    pub fn new(config: SportsTradingConfig, core: NbaComebackCore) -> Self {
        Self {
            config,
            core,
            observation_pool: None,
            pm_sports: None,
            grok: None,
            grok_cache: HashMap::new(),
            decision_cooldown: HashMap::new(),
        }
    }

    pub fn with_observability(
        mut self,
        observation_pool: PgPool,
        pm_sports: PolymarketSportsClient,
    ) -> Self {
        self.observation_pool = Some(observation_pool);
        self.pm_sports = Some(pm_sports);
        self
    }

    pub fn with_observation_pool(mut self, observation_pool: PgPool) -> Self {
        self.observation_pool = Some(observation_pool);
        self
    }

    pub fn with_pm_sports(mut self, pm_sports: PolymarketSportsClient) -> Self {
        self.pm_sports = Some(pm_sports);
        self
    }

    pub fn with_grok(mut self, grok: GrokClient) -> Self {
        self.grok = Some(grok);
        self
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

    fn is_valid_nba_game(game: &LiveGame) -> bool {
        Self::is_valid_nba_abbrev(&game.home_abbrev) && Self::is_valid_nba_abbrev(&game.away_abbrev)
    }

    fn text_matches_team(text: &str, team_name: &str, team_abbrev: &str) -> bool {
        let text_norm = Self::normalize_text(text);
        let name_norm = Self::normalize_text(team_name);
        let abbrev_norm = Self::normalize_text(team_abbrev);

        text_norm.contains(name_norm.trim()) || text_norm.contains(abbrev_norm.trim())
    }

    fn event_matches_game(event: &EventDetails, game: &LiveGame) -> bool {
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
        game: &LiveGame,
        pm_events: &'a [EventDetails],
    ) -> Option<&'a EventDetails> {
        pm_events
            .iter()
            .find(|event| Self::event_matches_game(event, game))
    }

    fn select_trailing_side(
        market: &LiveGameMarket,
        trailing_team: &str,
        trailing_abbrev: &str,
    ) -> Option<usize> {
        let outcomes = market
            .outcomes
            .as_ref()
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())?;

        outcomes
            .iter()
            .enumerate()
            .take(2)
            .find_map(|(idx, outcome)| {
                if Self::text_matches_team(outcome, trailing_team, trailing_abbrev) {
                    Some(idx)
                } else {
                    None
                }
            })
    }

    async fn collect_market_observation(
        &self,
        game: &LiveGame,
        pm_events: &[EventDetails],
    ) -> MarketObservation {
        let mut obs = MarketObservation::default();

        let Some(event) = Self::find_matching_pm_event(game, pm_events) else {
            return obs;
        };

        obs.pm_event_id = Some(event.id.clone());
        obs.pm_event_title = Some(event.title.clone());
        obs.pm_event_slug = Some(event.slug.clone());
        obs.pm_live_status = Some(event.live_status());

        let Some(moneyline) = event.moneyline() else {
            return obs;
        };

        if let Some((yes_mid, no_mid)) = moneyline.get_prices() {
            obs.pm_yes_mid = Some(yes_mid);
            obs.pm_no_mid = Some(no_mid);
        }

        let mut tokens: Option<(String, String)> = None;
        if let Some((yes_token, no_token)) = moneyline.get_token_ids() {
            obs.pm_yes_token_id = Some(yes_token.clone());
            obs.pm_no_token_id = Some(no_token.clone());
            tokens = Some((yes_token, no_token));
        }

        if let (Some(pm_client), Some((yes_token, no_token))) =
            (self.pm_sports.as_ref(), tokens.as_ref())
        {
            if let Ok(book) = pm_client.get_order_book(yes_token).await {
                obs.pm_yes_best_bid = book.best_bid();
                obs.pm_yes_best_ask = book.best_ask();
                if obs.pm_yes_mid.is_none() {
                    obs.pm_yes_mid = book.mid_price();
                }
                obs.pm_yes_book = Some(book);
            }
            if let Ok(book) = pm_client.get_order_book(no_token).await {
                obs.pm_no_best_bid = book.best_bid();
                obs.pm_no_best_ask = book.best_ask();
                if obs.pm_no_mid.is_none() {
                    obs.pm_no_mid = book.mid_price();
                }
                obs.pm_no_book = Some(book);
            }
        }

        if let Some((trailing_team, trailing_abbrev, _)) = game.trailing_team() {
            if let (Some((yes_mid, no_mid)), Some((yes_token, no_token)), Some(idx)) = (
                moneyline.get_prices(),
                moneyline.get_token_ids(),
                Self::select_trailing_side(moneyline, &trailing_team, &trailing_abbrev),
            ) {
                if idx == 0 {
                    obs.pm_trailing_token_id = Some(yes_token);
                    obs.pm_trailing_price = Some(yes_mid);
                    obs.pm_trailing_price_source = Some("moneyline_outcome_prices".to_string());
                } else if idx == 1 {
                    obs.pm_trailing_token_id = Some(no_token);
                    obs.pm_trailing_price = Some(no_mid);
                    obs.pm_trailing_price_source = Some("moneyline_outcome_prices".to_string());
                }
            }
        }

        obs
    }

    async fn upsert_today_nba_token_targets(&self, pm_events: &[EventDetails]) {
        let Some(pool) = self.observation_pool.as_ref() else {
            return;
        };

        let today = Utc::now().date_naive();
        let mut targets: Vec<CollectorTokenTarget> = Vec::new();
        targets.reserve(pm_events.len().saturating_mul(2));

        for ev in pm_events {
            let Some(moneyline) = ev.moneyline() else {
                continue;
            };
            let Some((yes_token, no_token)) = moneyline.get_token_ids() else {
                continue;
            };

            let parsed_date = ev
                .event_date
                .as_deref()
                .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                .unwrap_or(today);

            let start_ts = ev
                .start_time
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            let expires_at = start_ts
                .map(|dt| dt + chrono::Duration::hours(8))
                .unwrap_or_else(|| Utc::now() + chrono::Duration::hours(24));

            let mut common = serde_json::json!({
                "event_id": ev.id,
                "slug": ev.slug,
                "title": ev.title,
                "source": "pm_gamma",
                "market_type": "moneyline"
            });
            if let Some(cid) = moneyline.condition_id.as_deref() {
                if let Some(obj) = common.as_object_mut() {
                    obj.insert(
                        "condition_id".to_string(),
                        serde_json::Value::String(cid.to_string()),
                    );
                }
            }

            targets.push(
                CollectorTokenTarget::new(yes_token, "SPORTS_NBA")
                    .with_target_date(Some(parsed_date))
                    .with_expires_at(Some(expires_at))
                    .with_metadata({
                        let mut v = common.clone();
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "outcome".to_string(),
                                serde_json::Value::String("YES".to_string()),
                            );
                        }
                        v
                    }),
            );
            targets.push(
                CollectorTokenTarget::new(no_token, "SPORTS_NBA")
                    .with_target_date(Some(parsed_date))
                    .with_expires_at(Some(expires_at))
                    .with_metadata({
                        let mut v = common;
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert(
                                "outcome".to_string(),
                                serde_json::Value::String("NO".to_string()),
                            );
                        }
                        v
                    }),
            );
        }

        if targets.is_empty() {
            return;
        }

        if let Err(e) = upsert_collector_token_targets(pool, &targets).await {
            warn!(
                agent = self.config.agent_id,
                error = %e,
                "failed to upsert collector token targets (NBA)"
            );
        }
    }

    fn status_text(status: GameStatus) -> &'static str {
        match status {
            GameStatus::Scheduled => "scheduled",
            GameStatus::InProgress => "in_progress",
            GameStatus::Final => "final",
            GameStatus::Unknown => "unknown",
        }
    }

    async fn ensure_calendar_table(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS nba_schedule_calendar (
                espn_game_id TEXT PRIMARY KEY,
                season TEXT NOT NULL,
                game_date DATE NOT NULL,
                home_team TEXT NOT NULL,
                away_team TEXT NOT NULL,
                home_abbrev TEXT NOT NULL,
                away_abbrev TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'scheduled',
                first_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nba_schedule_calendar_game_date ON nba_schedule_calendar(game_date)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nba_schedule_calendar_season ON nba_schedule_calendar(season, game_date)",
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn sync_calendar_from_espn(&self, pool: &PgPool) -> Result<usize> {
        let today = Utc::now().date_naive();
        let mut upserted = 0usize;

        for day_offset in -CALENDAR_LOOKBACK_DAYS..=CALENDAR_LOOKAHEAD_DAYS {
            let target_date = today + chrono::Duration::days(day_offset);
            let games = self.core.espn.fetch_games_for_date(target_date).await?;

            for game in games {
                if !Self::is_valid_nba_game(&game) {
                    continue;
                }

                sqlx::query(
                    r#"
                    INSERT INTO nba_schedule_calendar (
                        espn_game_id, season, game_date, home_team, away_team,
                        home_abbrev, away_abbrev, status, first_seen_at, last_seen_at, updated_at
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW(), NOW())
                    ON CONFLICT (espn_game_id) DO UPDATE SET
                        season = EXCLUDED.season,
                        game_date = EXCLUDED.game_date,
                        home_team = EXCLUDED.home_team,
                        away_team = EXCLUDED.away_team,
                        home_abbrev = EXCLUDED.home_abbrev,
                        away_abbrev = EXCLUDED.away_abbrev,
                        status = EXCLUDED.status,
                        last_seen_at = NOW(),
                        updated_at = NOW()
                    "#,
                )
                .bind(&game.espn_game_id)
                .bind(&self.core.cfg.season)
                .bind(target_date)
                .bind(&game.home_team)
                .bind(&game.away_team)
                .bind(&game.home_abbrev)
                .bind(&game.away_abbrev)
                .bind(Self::status_text(game.status))
                .execute(pool)
                .await?;

                upserted = upserted.saturating_add(1);
            }
        }

        Ok(upserted)
    }

    async fn load_near_term_calendar_ids(&self, pool: &PgPool) -> Result<HashSet<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            SELECT espn_game_id
            FROM nba_schedule_calendar
            WHERE season = $1
              AND game_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
            "#,
        )
        .bind(&self.core.cfg.season)
        .fetch_all(pool)
        .await?;

        Ok(rows.into_iter().collect())
    }

    async fn ensure_state_table(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS nba_comeback_agent_state (
                account_id TEXT NOT NULL DEFAULT 'default',
                agent_id TEXT NOT NULL,
                state_json JSONB NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                PRIMARY KEY (account_id, agent_id)
            )
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nba_comeback_agent_state_updated_at ON nba_comeback_agent_state(updated_at DESC)",
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn load_persisted_state(&mut self, pool: &PgPool) -> Result<()> {
        let state = sqlx::query_scalar::<_, sqlx::types::Json<NbaComebackState>>(
            r#"
            SELECT state_json
            FROM nba_comeback_agent_state
            WHERE account_id = $1
              AND agent_id = $2
            "#,
        )
        .bind(&self.config.account_id)
        .bind(&self.config.agent_id)
        .fetch_optional(pool)
        .await?;

        if let Some(sqlx::types::Json(state)) = state {
            self.core.state = state;
            self.core.reset_daily_if_needed();
            info!(
                agent = self.config.agent_id,
                tracked_positions = self.core.state.game_positions.len(),
                initial_entries = self.core.state.initial_entries.len(),
                daily_spend = %self.core.state.daily_spend_usd,
                daily_realized_pnl = %self.core.state.daily_realized_pnl_usd,
                settled_trades = self.core.state.settled_trades,
                winning_trades = self.core.state.winning_trades,
                loss_streak = self.core.state.loss_streak,
                daily_spend_day = %self.core.state.daily_spend_day,
                "restored nba comeback state from persistence"
            );
        } else {
            debug!(
                agent = self.config.agent_id,
                "no persisted nba comeback state found"
            );
        }

        Ok(())
    }

    async fn persist_state(&self, pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO nba_comeback_agent_state (
                account_id, agent_id, state_json, updated_at
            )
            VALUES ($1, $2, $3, NOW())
            ON CONFLICT (account_id, agent_id) DO UPDATE SET
                state_json = EXCLUDED.state_json,
                updated_at = NOW()
            "#,
        )
        .bind(&self.config.account_id)
        .bind(&self.config.agent_id)
        .bind(sqlx::types::Json(&self.core.state))
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn persist_state_best_effort(&self, reason: &'static str) {
        let Some(pool) = self.observation_pool.as_ref() else {
            return;
        };
        if let Err(e) = self.persist_state(pool).await {
            warn!(
                agent = self.config.agent_id,
                reason,
                error = %e,
                "failed to persist nba comeback state"
            );
        }
    }

    async fn ensure_observation_table(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS nba_live_observations (
                id BIGSERIAL PRIMARY KEY,
                account_id TEXT NOT NULL DEFAULT 'default',
                agent_id TEXT NOT NULL,
                espn_game_id TEXT NOT NULL,
                home_team TEXT NOT NULL,
                away_team TEXT NOT NULL,
                home_abbrev TEXT NOT NULL,
                away_abbrev TEXT NOT NULL,
                home_score INTEGER NOT NULL,
                away_score INTEGER NOT NULL,
                quarter INTEGER NOT NULL,
                clock TEXT NOT NULL,
                time_remaining_mins DOUBLE PRECISION NOT NULL,
                game_status TEXT NOT NULL,
                trailing_team TEXT,
                trailing_abbrev TEXT,
                deficit INTEGER,
                comeback_rate DOUBLE PRECISION,
                adjusted_win_prob DOUBLE PRECISION,
                pm_event_id TEXT,
                pm_event_title TEXT,
                pm_event_slug TEXT,
                pm_live_status TEXT,
                pm_yes_token_id TEXT,
                pm_no_token_id TEXT,
                pm_yes_mid NUMERIC(10,6),
                pm_no_mid NUMERIC(10,6),
                pm_yes_best_bid NUMERIC(10,6),
                pm_yes_best_ask NUMERIC(10,6),
                pm_no_best_bid NUMERIC(10,6),
                pm_no_best_ask NUMERIC(10,6),
                pm_trailing_token_id TEXT,
                pm_trailing_price NUMERIC(10,6),
                pm_trailing_price_source TEXT,
                edge DOUBLE PRECISION,
                is_trade_candidate BOOLEAN NOT NULL DEFAULT FALSE,
                recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "ALTER TABLE nba_live_observations ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nba_live_observations_game_time ON nba_live_observations(espn_game_id, recorded_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nba_live_observations_time ON nba_live_observations(recorded_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_nba_live_observations_tokens ON nba_live_observations(pm_trailing_token_id, recorded_at DESC)",
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn persist_observation(
        &self,
        game: &LiveGame,
        trailing: Option<(String, String, i32)>,
        candidate: Option<&ComebackCandidate>,
        market_obs: &MarketObservation,
        edge: Option<f64>,
        is_trade_candidate: bool,
    ) {
        let Some(pool) = self.observation_pool.as_ref() else {
            return;
        };

        let game_status = match game.status {
            GameStatus::Scheduled => "scheduled",
            GameStatus::InProgress => "in_progress",
            GameStatus::Final => "final",
            GameStatus::Unknown => "unknown",
        };

        let (trailing_team, trailing_abbrev, deficit) = match trailing {
            Some((team, abbrev, deficit)) => (Some(team), Some(abbrev), Some(deficit)),
            None => (None, None, None),
        };

        let comeback_rate = candidate.map(|c| c.comeback_rate);
        let adjusted_win_prob = candidate.map(|c| c.adjusted_win_prob);

        let result = sqlx::query(
            r#"
            INSERT INTO nba_live_observations (
                account_id, agent_id, espn_game_id, home_team, away_team, home_abbrev, away_abbrev,
                home_score, away_score, quarter, clock, time_remaining_mins, game_status,
                trailing_team, trailing_abbrev, deficit, comeback_rate, adjusted_win_prob,
                pm_event_id, pm_event_title, pm_event_slug, pm_live_status,
                pm_yes_token_id, pm_no_token_id, pm_yes_mid, pm_no_mid,
                pm_yes_best_bid, pm_yes_best_ask, pm_no_best_bid, pm_no_best_ask,
                pm_trailing_token_id, pm_trailing_price, pm_trailing_price_source,
                edge, is_trade_candidate
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7,
                $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $17, $18,
                $19, $20, $21, $22,
                $23, $24, $25, $26,
                $27, $28, $29, $30,
                $31, $32, $33,
                $34, $35
            )
            "#,
        )
        .bind(&self.config.account_id)
        .bind(&self.config.agent_id)
        .bind(&game.espn_game_id)
        .bind(&game.home_team)
        .bind(&game.away_team)
        .bind(&game.home_abbrev)
        .bind(&game.away_abbrev)
        .bind(game.home_score)
        .bind(game.away_score)
        .bind(game.quarter as i32)
        .bind(&game.clock)
        .bind(game.time_remaining_mins)
        .bind(game_status)
        .bind(trailing_team)
        .bind(trailing_abbrev)
        .bind(deficit)
        .bind(comeback_rate)
        .bind(adjusted_win_prob)
        .bind(&market_obs.pm_event_id)
        .bind(&market_obs.pm_event_title)
        .bind(&market_obs.pm_event_slug)
        .bind(&market_obs.pm_live_status)
        .bind(&market_obs.pm_yes_token_id)
        .bind(&market_obs.pm_no_token_id)
        .bind(market_obs.pm_yes_mid)
        .bind(market_obs.pm_no_mid)
        .bind(market_obs.pm_yes_best_bid)
        .bind(market_obs.pm_yes_best_ask)
        .bind(market_obs.pm_no_best_bid)
        .bind(market_obs.pm_no_best_ask)
        .bind(&market_obs.pm_trailing_token_id)
        .bind(market_obs.pm_trailing_price)
        .bind(&market_obs.pm_trailing_price_source)
        .bind(edge)
        .bind(is_trade_candidate)
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent = self.config.agent_id,
                game_id = %game.espn_game_id,
                error = %e,
                "failed to persist nba live observation"
            );
        }

        self.persist_orderbook_snapshots(game, market_obs).await;
    }

    fn env_usize(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(default)
    }

    fn parse_depth_levels(
        levels: &[SportsOrderBookLevel],
        is_bid: bool,
        max_levels: usize,
    ) -> Vec<DepthLevelJson> {
        use rust_decimal::Decimal;

        let mut parsed: Vec<(Decimal, Decimal)> = Vec::with_capacity(levels.len());
        for lvl in levels {
            let Ok(price) = lvl.price.parse::<Decimal>() else {
                continue;
            };
            let Ok(size) = lvl.size.parse::<Decimal>() else {
                continue;
            };
            parsed.push((price, size));
        }

        if is_bid {
            parsed.sort_by(|a, b| b.0.cmp(&a.0));
        } else {
            parsed.sort_by(|a, b| a.0.cmp(&b.0));
        }

        parsed
            .into_iter()
            .take(max_levels)
            .map(|(price, size)| DepthLevelJson {
                price: price.to_string(),
                size: size.to_string(),
            })
            .collect()
    }

    fn parse_book_timestamp(ts: &Option<String>) -> Option<chrono::DateTime<Utc>> {
        let raw = ts.as_ref()?;
        let parsed = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
        Some(parsed.with_timezone(&Utc))
    }

    async fn persist_orderbook_snapshots(&self, game: &LiveGame, market_obs: &MarketObservation) {
        let Some(pool) = self.observation_pool.as_ref() else {
            return;
        };

        let max_levels = Self::env_usize("PM_ORDERBOOK_LEVELS", 20).clamp(1, 200);

        // Persist YES book snapshot (if available)
        if let Some(book) = market_obs.pm_yes_book.as_ref() {
            self.persist_one_orderbook_snapshot(pool, book, "YES", game, market_obs, max_levels)
                .await;
        }

        // Persist NO book snapshot (if available)
        if let Some(book) = market_obs.pm_no_book.as_ref() {
            self.persist_one_orderbook_snapshot(pool, book, "NO", game, market_obs, max_levels)
                .await;
        }
    }

    async fn persist_one_orderbook_snapshot(
        &self,
        pool: &PgPool,
        book: &SportsOrderBook,
        outcome: &'static str,
        game: &LiveGame,
        market_obs: &MarketObservation,
        max_levels: usize,
    ) {
        #[derive(Debug, Clone, serde::Serialize)]
        struct Context<'a> {
            agent_id: &'a str,
            espn_game_id: &'a str,
            quarter: u8,
            clock: &'a str,
            outcome: &'a str,
            pm_event_id: Option<&'a str>,
            pm_event_slug: Option<&'a str>,
        }

        let bids = Self::parse_depth_levels(&book.bids, true, max_levels);
        let asks = Self::parse_depth_levels(&book.asks, false, max_levels);
        let book_ts = Self::parse_book_timestamp(&book.timestamp);

        let context = Context {
            agent_id: &self.config.agent_id,
            espn_game_id: &game.espn_game_id,
            quarter: game.quarter,
            clock: &game.clock,
            outcome,
            pm_event_id: market_obs.pm_event_id.as_deref(),
            pm_event_slug: market_obs.pm_event_slug.as_deref(),
        };

        let result = sqlx::query(
            r#"
            INSERT INTO clob_orderbook_snapshots
                (domain, token_id, market, bids, asks, book_timestamp, hash, source, context)
            VALUES
                ($1, $2, $3, $4, $5, $6, NULL, 'polymarket_http', $7)
            "#,
        )
        .bind(Domain::Sports.to_string())
        .bind(&book.asset_id)
        .bind(book.market.clone())
        .bind(sqlx::types::Json(&bids))
        .bind(sqlx::types::Json(&asks))
        .bind(book_ts)
        .bind(sqlx::types::Json(&context))
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent = self.config.agent_id,
                token_id = %book.asset_id,
                error = %e,
                "failed to persist sports clob orderbook snapshot"
            );
        }
    }

    /// Convert a ComebackOpportunity into an OrderIntent
    fn opportunity_to_intent(
        agent_id: &str,
        opp: &ComebackOpportunity,
        shares: u64,
        config_hash: &str,
    ) -> OrderIntent {
        OrderIntent::new(
            agent_id,
            Domain::Sports,
            &opp.market_slug,
            &opp.token_id,
            Side::Up,
            true,
            shares,
            opp.market_price,
        )
        .with_priority(OrderPriority::Normal)
        .with_metadata("strategy", "nba_comeback")
        .with_deployment_id(DEPLOYMENT_ID_NBA_COMEBACK)
        .with_metadata("game_id", &opp.game.espn_game_id)
        .with_metadata("trailing_team", &opp.trailing_abbrev)
        .with_metadata("deficit", &opp.deficit.to_string())
        .with_metadata("comeback_rate", &format!("{:.3}", opp.comeback_rate))
        .with_metadata("edge", &format!("{:.3}", opp.edge))
        .with_metadata("signal_type", "nba_comeback_entry")
        .with_metadata("signal_confidence", &format!("{:.6}", opp.comeback_rate))
        .with_metadata(
            "signal_fair_value",
            &format!("{:.6}", opp.adjusted_win_prob),
        )
        .with_metadata("signal_market_price", &opp.market_price.to_string())
        .with_metadata("signal_edge", &format!("{:.6}", opp.edge))
        .with_metadata("config_hash", config_hash)
    }

    fn exit_intent(
        agent_id: &str,
        game_id: &str,
        trailing_team: &str,
        market_slug: &str,
        token_id: &str,
        shares: u64,
        limit_price: Decimal,
        exit_reason: &str,
        config_hash: &str,
    ) -> OrderIntent {
        let priority = if exit_reason == "stop_loss" {
            OrderPriority::Critical
        } else {
            OrderPriority::High
        };

        OrderIntent::new(
            agent_id,
            Domain::Sports,
            market_slug,
            token_id,
            Side::Up,
            false,
            shares,
            limit_price,
        )
        .with_priority(priority)
        .with_metadata("strategy", "nba_comeback")
        .with_deployment_id(DEPLOYMENT_ID_NBA_COMEBACK)
        .with_metadata("game_id", game_id)
        .with_metadata("trailing_team", trailing_team)
        .with_metadata("exit_reason", exit_reason)
        .with_metadata("signal_type", "nba_comeback_exit")
        .with_metadata("signal_market_price", &limit_price.to_string())
        .with_metadata("config_hash", config_hash)
    }

    async fn ensure_grok_intel_table(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS grok_game_intel (
                id BIGSERIAL PRIMARY KEY,
                account_id TEXT NOT NULL DEFAULT 'default',
                agent_id TEXT NOT NULL,
                espn_game_id TEXT NOT NULL,
                home_team TEXT NOT NULL,
                away_team TEXT NOT NULL,
                quarter INTEGER NOT NULL,
                clock TEXT NOT NULL,
                score TEXT NOT NULL,
                momentum_direction TEXT NOT NULL,
                home_sentiment_score DOUBLE PRECISION,
                away_sentiment_score DOUBLE PRECISION,
                grok_home_win_prob DOUBLE PRECISION,
                grok_confidence DOUBLE PRECISION,
                injury_updates JSONB DEFAULT '[]',
                key_factors JSONB DEFAULT '[]',
                signal_type TEXT,
                signal_edge DOUBLE PRECISION,
                signal_acted_on BOOLEAN NOT NULL DEFAULT FALSE,
                raw_response TEXT,
                query_duration_ms INTEGER,
                recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "ALTER TABLE grok_game_intel ADD COLUMN IF NOT EXISTS account_id TEXT NOT NULL DEFAULT 'default'",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_grok_game_intel_game_time ON grok_game_intel(espn_game_id, recorded_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_grok_game_intel_signals ON grok_game_intel(signal_type, recorded_at DESC) WHERE signal_type IS NOT NULL",
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn persist_grok_intel(
        pool: &PgPool,
        account_id: &str,
        agent_id: &str,
        game: &LiveGame,
        intel: &GrokGameIntel,
        signal: Option<&GrokTradeSignal>,
        acted_on: bool,
    ) {
        let score_text = format!(
            "{} {}-{} {}",
            game.away_abbrev, game.away_score, game.home_score, game.home_abbrev
        );
        let momentum_str = match intel.momentum_direction {
            grok_intel::MomentumDirection::HomeTeamSurge => "home_surge",
            grok_intel::MomentumDirection::AwayTeamSurge => "away_surge",
            grok_intel::MomentumDirection::Neutral => "neutral",
        };

        let result = sqlx::query(
            r#"
            INSERT INTO grok_game_intel (
                account_id, agent_id, espn_game_id, home_team, away_team,
                quarter, clock, score,
                momentum_direction, home_sentiment_score, away_sentiment_score,
                grok_home_win_prob, grok_confidence,
                injury_updates, key_factors,
                signal_type, signal_edge, signal_acted_on,
                raw_response
            )
            VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8,
                $9, $10, $11,
                $12, $13,
                $14, $15,
                $16, $17, $18,
                $19
            )
            "#,
        )
        .bind(account_id)
        .bind(agent_id)
        .bind(&game.espn_game_id)
        .bind(&game.home_team)
        .bind(&game.away_team)
        .bind(game.quarter as i32)
        .bind(&game.clock)
        .bind(&score_text)
        .bind(momentum_str)
        .bind(intel.home_sentiment_score)
        .bind(intel.away_sentiment_score)
        .bind(intel.grok_home_win_prob)
        .bind(intel.grok_confidence)
        .bind(sqlx::types::Json(&intel.injury_updates))
        .bind(sqlx::types::Json(&intel.key_factors))
        .bind(signal.map(|s| s.signal_type.to_string()))
        .bind(signal.map(|s| s.edge))
        .bind(acted_on)
        .bind(&intel.raw_response)
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent = agent_id,
                game_id = %game.espn_game_id,
                error = %e,
                "failed to persist grok game intel"
            );
        }
    }

    /// Check if a game is still on decision cooldown
    fn is_on_cooldown(&self, game_id: &str) -> bool {
        self.decision_cooldown
            .get(game_id)
            .map(|t| t.elapsed().as_secs() < self.core.cfg.grok_decision_cooldown_secs)
            .unwrap_or(false)
    }

    /// Mark a game as having just received a decision
    fn set_cooldown(&mut self, game_id: &str) {
        self.decision_cooldown
            .insert(game_id.to_string(), std::time::Instant::now());
    }

    fn initial_entry_notional(entry_price: Decimal, shares: u64) -> Decimal {
        entry_price * Decimal::from(shares)
    }

    fn record_initial_espn_entry(
        &mut self,
        game_id: &str,
        trailing_abbrev: &str,
        market_slug: &str,
        token_id: &str,
        entry_price: Decimal,
        shares: u64,
        comeback_rate: f64,
    ) {
        self.core.record_position_entry_with_market_and_team(
            game_id,
            trailing_abbrev,
            market_slug,
            token_id,
            entry_price,
            shares,
            comeback_rate,
        );
        let spend = Self::initial_entry_notional(entry_price, shares);
        self.core
            .record_initial_entry_submission(game_id, token_id, spend);
    }

    fn record_initial_grok_signal_entry(
        &mut self,
        game_id: &str,
        trailing_abbrev: &str,
        market_slug: &str,
        token_id: &str,
        entry_price: Decimal,
        shares: u64,
    ) {
        self.core.record_position_entry_with_market_and_team(
            game_id,
            trailing_abbrev,
            market_slug,
            token_id,
            entry_price,
            shares,
            0.0,
        );
        let spend = Self::initial_entry_notional(entry_price, shares);
        self.core
            .record_initial_entry_submission(game_id, token_id, spend);
    }

    fn position_avg_entry_price(pos: &GamePosition) -> Option<Decimal> {
        if pos.total_shares == 0 {
            return None;
        }
        Some(pos.total_cost / Decimal::from(pos.total_shares))
    }

    fn position_realized_pnl(pos: &GamePosition, exit_price: Decimal) -> Option<Decimal> {
        let avg_entry = Self::position_avg_entry_price(pos)?;
        Some((exit_price - avg_entry) * Decimal::from(pos.total_shares))
    }

    fn settlement_price_for_position(pos: &GamePosition, game: &LiveGame) -> Option<Decimal> {
        if game.status != GameStatus::Final {
            return None;
        }
        if game.home_score == game.away_score {
            return None;
        }
        let team = pos.trailing_abbrev.as_ref()?;
        let winner = if game.home_score > game.away_score {
            &game.home_abbrev
        } else {
            &game.away_abbrev
        };
        if winner.eq_ignore_ascii_case(team) {
            Some(Decimal::ONE)
        } else {
            Some(Decimal::ZERO)
        }
    }

    fn performance_metrics(&self, daily_pnl: Decimal) -> HashMap<String, String> {
        let mut metrics = HashMap::new();
        let win_rate = self.core.settled_win_rate().unwrap_or(0.0);
        metrics.insert("sports_win_rate".to_string(), format!("{:.6}", win_rate));
        metrics.insert(
            "sports_settled_trades".to_string(),
            self.core.state.settled_trades.to_string(),
        );
        metrics.insert(
            "sports_winning_trades".to_string(),
            self.core.state.winning_trades.to_string(),
        );
        metrics.insert(
            "sports_loss_streak".to_string(),
            self.core.state.loss_streak.to_string(),
        );
        metrics.insert(
            "sports_daily_realized_pnl_usd".to_string(),
            daily_pnl.to_string(),
        );
        metrics.insert(
            "sports_size_multiplier".to_string(),
            format!("{:.6}", self.core.risk_size_multiplier()),
        );
        metrics.insert(
            "sports_daily_loss_limit_usd".to_string(),
            self.core.cfg.performance_daily_loss_limit_usd.to_string(),
        );
        metrics.insert(
            "sports_can_open_new_risk".to_string(),
            self.core.can_open_new_risk().to_string(),
        );
        metrics
    }

    async fn submit_force_close_exits(&self, ctx: &AgentContext) {
        let global = ctx.read_global_state().await;
        let positions = global
            .positions
            .into_iter()
            .filter(|p| p.agent_id == self.config.agent_id && p.shares > 0)
            .collect::<Vec<_>>();

        if positions.is_empty() {
            info!(
                agent = self.config.agent_id,
                "force close: no open positions"
            );
            return;
        }

        info!(
            agent = self.config.agent_id,
            positions = positions.len(),
            "force close: submitting reduce-only exits"
        );

        for pos in positions {
            let mut intent = OrderIntent::new(
                &self.config.agent_id,
                pos.domain,
                &pos.market_slug,
                &pos.token_id,
                pos.side,
                false,
                pos.shares,
                dec!(0.01),
            )
            .with_priority(OrderPriority::Critical)
            .with_metadata("intent_reason", "force_close")
            .with_metadata("position_id", &pos.position_id)
            .with_metadata("force_close_price_floor", "0.01");
            if let Some(deployment_id) = pos
                .metadata
                .get("deployment_id")
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
            {
                intent = intent.with_deployment_id(deployment_id);
            } else if let Some(strategy) = pos
                .metadata
                .get("strategy")
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
            {
                intent = intent.with_metadata("strategy", strategy);
            }

            if let Err(e) = ctx.submit_order(intent).await {
                warn!(
                    agent = self.config.agent_id,
                    position_id = %pos.position_id,
                    error = %e,
                    "force close exit submit failed"
                );
            }
        }
    }

    fn classify_early_exit(
        avg_entry_price: Decimal,
        current_price: Decimal,
        take_profit_pct: f64,
        stop_loss_pct: f64,
    ) -> Option<&'static str> {
        if avg_entry_price <= Decimal::ZERO || current_price <= Decimal::ZERO {
            return None;
        }
        let pnl_pct = ((current_price - avg_entry_price) * dec!(100) / avg_entry_price)
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);

        if pnl_pct >= take_profit_pct {
            return Some("take_profit");
        }
        if pnl_pct <= -stop_loss_pct {
            return Some("stop_loss");
        }
        None
    }

    /// Convert a GrokDecision::Trade into an OrderIntent
    fn decision_to_intent(
        agent_id: &str,
        req: &UnifiedDecisionRequest,
        decision: &GrokDecision,
        shares: u64,
        config_hash: &str,
    ) -> OrderIntent {
        let (fair_value, edge, confidence) = match decision {
            GrokDecision::Trade {
                fair_value,
                edge,
                confidence,
                ..
            } => (*fair_value, *edge, *confidence),
            _ => (0.0, 0.0, 0.0),
        };

        OrderIntent::new(
            agent_id,
            Domain::Sports,
            &req.market.market_slug,
            &req.market.token_id,
            Side::Up,
            true,
            shares,
            req.market.market_price,
        )
        .with_priority(OrderPriority::Normal)
        .with_metadata("strategy", "grok_unified_decision")
        .with_deployment_id(DEPLOYMENT_ID_NBA_GROK_UNIFIED)
        .with_metadata("game_id", &req.game.espn_game_id)
        .with_metadata("trailing_team", &req.trailing_abbrev)
        .with_metadata("deficit", &req.deficit.to_string())
        .with_metadata("trigger", &req.trigger.to_string())
        .with_metadata("signal_confidence", &format!("{:.6}", confidence))
        .with_metadata("signal_fair_value", &format!("{:.6}", fair_value))
        .with_metadata("signal_market_price", &req.market.market_price.to_string())
        .with_metadata("signal_edge", &format!("{:.6}", edge))
        .with_metadata(
            "reward_risk_ratio",
            &format!("{:.2}", req.risk_metrics.reward_risk_ratio),
        )
        .with_metadata(
            "expected_value",
            &format!("{:.6}", req.risk_metrics.expected_value),
        )
        .with_metadata(
            "kelly_fraction",
            &format!("{:.6}", req.risk_metrics.kelly_fraction),
        )
        .with_metadata("config_hash", config_hash)
    }

    async fn ensure_grok_unified_decisions_table(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS grok_unified_decisions (
                id BIGSERIAL PRIMARY KEY,
                request_id UUID NOT NULL UNIQUE,
                account_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                espn_game_id TEXT NOT NULL,
                home_team TEXT NOT NULL,
                away_team TEXT NOT NULL,
                trailing_team TEXT NOT NULL,
                trailing_abbrev TEXT NOT NULL,
                deficit INTEGER NOT NULL,
                quarter INTEGER NOT NULL,
                clock TEXT NOT NULL,
                score TEXT NOT NULL,
                trigger_type TEXT NOT NULL,
                comeback_rate DOUBLE PRECISION,
                adjusted_win_prob DOUBLE PRECISION,
                statistical_edge DOUBLE PRECISION,
                grok_momentum TEXT,
                grok_home_win_prob DOUBLE PRECISION,
                grok_confidence DOUBLE PRECISION,
                grok_sentiment_home DOUBLE PRECISION,
                grok_sentiment_away DOUBLE PRECISION,
                injury_updates JSONB,
                market_slug TEXT NOT NULL,
                token_id TEXT NOT NULL,
                market_price DOUBLE PRECISION NOT NULL,
                best_bid DOUBLE PRECISION,
                best_ask DOUBLE PRECISION,
                decision TEXT NOT NULL,
                decision_fair_value DOUBLE PRECISION,
                decision_own_fair_value DOUBLE PRECISION,
                decision_edge DOUBLE PRECISION,
                decision_confidence DOUBLE PRECISION,
                decision_reasoning TEXT,
                decision_risk_factors JSONB,
                reward_risk_ratio DOUBLE PRECISION,
                expected_value DOUBLE PRECISION,
                kelly_fraction DOUBLE PRECISION,
                raw_prompt TEXT,
                raw_response TEXT,
                query_duration_ms INTEGER,
                order_submitted BOOLEAN NOT NULL DEFAULT FALSE,
                recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_grok_decisions_game_time ON grok_unified_decisions(espn_game_id, recorded_at DESC)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_grok_decisions_trades ON grok_unified_decisions(decision, recorded_at DESC) WHERE decision = 'trade'",
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    async fn persist_unified_decision(
        pool: &PgPool,
        account_id: &str,
        agent_id: &str,
        req: &UnifiedDecisionRequest,
        decision: &GrokDecision,
        raw_prompt: &str,
        raw_response: &str,
        order_submitted: bool,
    ) {
        let score_text = format!(
            "{} {}-{} {}",
            req.game.away_abbrev, req.game.away_score, req.game.home_score, req.game.home_abbrev
        );

        let (
            decision_str,
            d_fair_value,
            d_own_fair_value,
            d_edge,
            d_confidence,
            d_reasoning,
            d_risk_factors,
        ) = match decision {
            GrokDecision::Trade {
                fair_value,
                own_fair_value,
                edge,
                confidence,
                reasoning,
                risk_factors,
                ..
            } => (
                "trade",
                Some(*fair_value),
                Some(*own_fair_value),
                Some(*edge),
                Some(*confidence),
                Some(reasoning.as_str()),
                Some(risk_factors.clone()),
            ),
            GrokDecision::Pass { reasoning, .. } => (
                "pass",
                None,
                None,
                None,
                None,
                Some(reasoning.as_str()),
                None,
            ),
        };

        let momentum_str = req
            .grok_intel
            .as_ref()
            .map(|intel| match intel.momentum_direction {
                grok_intel::MomentumDirection::HomeTeamSurge => "home_surge",
                grok_intel::MomentumDirection::AwayTeamSurge => "away_surge",
                grok_intel::MomentumDirection::Neutral => "neutral",
            });

        let result = sqlx::query(
            r#"
            INSERT INTO grok_unified_decisions (
                request_id, account_id, agent_id,
                espn_game_id, home_team, away_team,
                trailing_team, trailing_abbrev, deficit,
                quarter, clock, score,
                trigger_type,
                comeback_rate, adjusted_win_prob, statistical_edge,
                grok_momentum, grok_home_win_prob, grok_confidence,
                grok_sentiment_home, grok_sentiment_away,
                injury_updates,
                market_slug, token_id, market_price,
                best_bid, best_ask,
                decision, decision_fair_value, decision_own_fair_value, decision_edge,
                decision_confidence, decision_reasoning, decision_risk_factors,
                reward_risk_ratio, expected_value, kelly_fraction,
                raw_prompt, raw_response,
                order_submitted
            )
            VALUES (
                $1, $2, $3,
                $4, $5, $6,
                $7, $8, $9,
                $10, $11, $12,
                $13,
                $14, $15, $16,
                $17, $18, $19,
                $20, $21,
                $22,
                $23, $24, $25,
                $26, $27,
                $28, $29, $30, $31,
                $32, $33, $34,
                $35, $36, $37,
                $38, $39,
                $40
            )
            "#,
        )
        .bind(req.request_id)
        .bind(account_id)
        .bind(agent_id)
        .bind(&req.game.espn_game_id)
        .bind(&req.game.home_team)
        .bind(&req.game.away_team)
        .bind(&req.trailing_team)
        .bind(&req.trailing_abbrev)
        .bind(req.deficit)
        .bind(req.game.quarter as i32)
        .bind(&req.game.clock)
        .bind(&score_text)
        .bind(req.trigger.to_string())
        .bind(req.comeback.as_ref().map(|c| c.comeback_rate))
        .bind(req.comeback.as_ref().map(|c| c.adjusted_win_prob))
        .bind(req.comeback.as_ref().map(|c| c.statistical_edge))
        .bind(momentum_str)
        .bind(req.grok_intel.as_ref().and_then(|i| i.grok_home_win_prob))
        .bind(req.grok_intel.as_ref().map(|i| i.grok_confidence))
        .bind(req.grok_intel.as_ref().map(|i| i.home_sentiment_score))
        .bind(req.grok_intel.as_ref().map(|i| i.away_sentiment_score))
        .bind(
            req.grok_intel
                .as_ref()
                .map(|i| sqlx::types::Json(&i.injury_updates)),
        )
        .bind(&req.market.market_slug)
        .bind(&req.market.token_id)
        .bind(
            req.market
                .market_price
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0),
        )
        .bind(
            req.market
                .yes_best_bid
                .map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)),
        )
        .bind(
            req.market
                .yes_best_ask
                .map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)),
        )
        .bind(decision_str)
        .bind(d_fair_value)
        .bind(d_own_fair_value)
        .bind(d_edge)
        .bind(d_confidence)
        .bind(d_reasoning)
        .bind(d_risk_factors.map(|rf| sqlx::types::Json(rf)))
        .bind(req.risk_metrics.reward_risk_ratio)
        .bind(req.risk_metrics.expected_value)
        .bind(req.risk_metrics.kelly_fraction)
        .bind(raw_prompt)
        .bind(raw_response)
        .bind(order_submitted)
        .execute(pool)
        .await;

        if let Err(e) = result {
            warn!(
                agent = agent_id,
                request_id = %req.request_id,
                game_id = %req.game.espn_game_id,
                error = %e,
                "failed to persist grok unified decision"
            );
        }
    }
}

#[async_trait]
impl TradingAgent for SportsTradingAgent {
    fn id(&self) -> &str {
        &self.config.agent_id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn domain(&self) -> Domain {
        Domain::Sports
    }

    fn risk_params(&self) -> AgentRiskParams {
        self.config.risk_params.clone()
    }

    async fn run(mut self, mut ctx: AgentContext) -> Result<()> {
        info!(agent = self.config.agent_id, "sports agent starting");
        let config_hash = {
            let payload = serde_json::to_vec(&self.config).unwrap_or_default();
            let mut hasher = Sha256::new();
            hasher.update(payload);
            format!("{:x}", hasher.finalize())
        };

        // Load historical stats
        if let Err(e) = self.core.stats.load_all().await {
            warn!(agent = self.config.agent_id, error = %e, "failed to load NBA stats, continuing");
        }
        if let Some(pool) = self.observation_pool.clone() {
            if let Err(e) = Self::ensure_calendar_table(&pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure nba_schedule_calendar table");
            }
            if let Err(e) = Self::ensure_observation_table(&pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure nba_live_observations table");
            }
            if let Err(e) = Self::ensure_state_table(&pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure nba_comeback_agent_state table");
            }
            if let Err(e) = ensure_collector_token_targets_table(&pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure collector_token_targets table");
            }
            if let Err(e) =
                crate::coordinator::bootstrap::ensure_clob_orderbook_snapshots_table(&pool).await
            {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure clob_orderbook_snapshots table");
            }
            if self.grok.is_some() {
                if let Err(e) = Self::ensure_grok_intel_table(&pool).await {
                    warn!(agent = self.config.agent_id, error = %e, "failed to ensure grok_game_intel table");
                }
                if let Err(e) = Self::ensure_grok_unified_decisions_table(&pool).await {
                    warn!(agent = self.config.agent_id, error = %e, "failed to ensure grok_unified_decisions table");
                }
            }
            if let Err(e) = self.load_persisted_state(&pool).await {
                warn!(
                    agent = self.config.agent_id,
                    error = %e,
                    "failed to load persisted nba comeback state"
                );
            }
        } else {
            warn!(
                agent = self.config.agent_id,
                "observation DB not configured; calendar-gated sports trading disabled"
            );
        }

        let mut status = AgentStatus::Running;
        let mut force_close_requested = false;
        let mut pending_intents: HashMap<Uuid, ComebackOpportunity> = HashMap::new();
        let position_count: usize = 0;
        let total_exposure = Decimal::ZERO;
        let mut daily_pnl = self.core.state.daily_realized_pnl_usd;
        let mut last_calendar_sync_at: Option<chrono::DateTime<Utc>> = None;
        let pm_events_refresh_secs: u64 = std::env::var("PM_SPORTS_EVENTS_REFRESH_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300)
            .max(30);
        let mut last_pm_sync_at: Option<chrono::DateTime<Utc>> = None;
        let mut pm_events_cache: Vec<EventDetails> = Vec::new();
        // Persist sports observations at a bounded cadence to keep DB volume sane.
        // Key: espn_game_id, Value: UTC minute bucket (unix_ts / 60).
        let mut last_observation_minute: HashMap<String, i64> = HashMap::new();

        let poll_dur = tokio::time::Duration::from_secs(self.config.poll_interval_secs);
        let heartbeat_dur = tokio::time::Duration::from_secs(self.config.heartbeat_interval_secs);
        let grok_interval_secs = self.core.cfg.grok_interval_secs;
        let grok_dur = tokio::time::Duration::from_secs(grok_interval_secs);
        let mut poll_tick = tokio::time::interval(poll_dur);
        let mut heartbeat_tick = tokio::time::interval(heartbeat_dur);
        let mut grok_tick = tokio::time::interval(grok_dur);
        poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        grok_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Cached state shared between ESPN poll and Grok tick
        let mut live_games_cache: Vec<LiveGame> = Vec::new();
        let mut market_inputs_cache: HashMap<String, MarketInput> = HashMap::new();

        loop {
            tokio::select! {
                // --- ESPN poll cycle ---
                _ = poll_tick.tick() => {
                    if !matches!(status, AgentStatus::Running) {
                        continue;
                    }

                    if let Some(pool) = self.observation_pool.as_ref() {
                        let needs_sync = last_calendar_sync_at
                            .map(|ts| Utc::now().signed_duration_since(ts).num_seconds() >= CALENDAR_SYNC_INTERVAL_SECS)
                            .unwrap_or(true);
                        if needs_sync {
                            match self.sync_calendar_from_espn(pool).await {
                                Ok(upserted) => {
                                    info!(
                                        agent = self.config.agent_id,
                                        upserted,
                                        lookback_days = CALENDAR_LOOKBACK_DAYS,
                                        lookahead_days = CALENDAR_LOOKAHEAD_DAYS,
                                        "nba schedule calendar synced"
                                    );
                                    last_calendar_sync_at = Some(Utc::now());
                                }
                                Err(e) => {
                                    warn!(
                                        agent = self.config.agent_id,
                                        error = %e,
                                        "failed to sync nba schedule calendar"
                                    );
                                }
                            }
                        }
                    }

                    let mut live_games = match self.core.espn.fetch_live_games().await {
                        Ok(games) => games,
                        Err(e) => {
                            warn!(agent = self.config.agent_id, error = %e, "failed to fetch ESPN games");
                            continue;
                        }
                    };
                    live_games.retain(Self::is_valid_nba_game);

                    // Calendar gating is for *trading* safety, not for observability.
                    // We still persist observations for valid NBA-team games even if the calendar is empty/unavailable.
                    let mut trade_gate_open = false;
                    let mut calendar_ids_for_trade: HashSet<String> = HashSet::new();
                    if let Some(pool) = self.observation_pool.as_ref() {
                        match self.load_near_term_calendar_ids(pool).await {
                            Ok(ids) if !ids.is_empty() => {
                                trade_gate_open = true;
                                calendar_ids_for_trade = ids;
                            }
                            Ok(_) => {
                                debug!(
                                    agent = self.config.agent_id,
                                    "trade gate closed: nba calendar has no near-term ids"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    agent = self.config.agent_id,
                                    error = %e,
                                    "trade gate closed: failed to load nba calendar ids"
                                );
                            }
                        }
                    } else {
                        debug!(
                            agent = self.config.agent_id,
                            "trade gate closed: observation DB not configured"
                        );
                    }

                    if live_games.is_empty() {
                        debug!(agent = self.config.agent_id, "no valid NBA games in live feed this cycle");
                    }
                    let candidates = self.core.scan_games(&live_games);
                    let candidates_by_game: HashMap<String, ComebackCandidate> = candidates
                        .iter()
                        .map(|c| (c.game.espn_game_id.clone(), c.clone()))
                        .collect();

                    // Keep the orderbook-history collector scoped to "today NBA" without relying on
                    // live-game state. Refresh PM event details on a slower cadence than ESPN polls.
                    let now = Utc::now();
                    let should_refresh_pm = last_pm_sync_at
                        .map(|t| (now - t).num_seconds() >= pm_events_refresh_secs as i64)
                        .unwrap_or(true);
                    if should_refresh_pm {
                        pm_events_cache = if let Some(pm_client) = self.pm_sports.as_ref() {
                            match pm_client.fetch_todays_games_with_details(NBA_SERIES_ID).await {
                                Ok(events) => events,
                                Err(e) => {
                                    warn!(
                                        agent = self.config.agent_id,
                                        error = %e,
                                        "failed to fetch PM NBA game details"
                                    );
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        };
                        last_pm_sync_at = Some(now);
                        self.upsert_today_nba_token_targets(&pm_events_cache).await;
                    }
                    let pm_events: &[EventDetails] = pm_events_cache.as_slice();

                    if live_games.is_empty() {
                        continue;
                    }

                    let mut market_inputs: HashMap<String, MarketInput> = HashMap::new();
                    let min_edge_f64 = self.core.cfg.min_edge.to_string().parse::<f64>().unwrap_or(0.05);

                    for game in &live_games {
                        let candidate = candidates_by_game.get(&game.espn_game_id);
                        let trailing = game.trailing_team();

                        // For data collection: start recording from Q3 and sample at most once per minute.
                        let now_minute_bucket = Utc::now().timestamp() / 60;
                        let eligible_for_observation = matches!(game.status, GameStatus::InProgress)
                            && game.quarter >= self.core.cfg.target_quarter;
                        let should_persist = eligible_for_observation
                            && last_observation_minute
                                .get(&game.espn_game_id)
                                .copied()
                                != Some(now_minute_bucket);

                        let market_obs = if eligible_for_observation {
                            self.collect_market_observation(game, &pm_events).await
                        } else {
                            MarketObservation::default()
                        };

                        let edge = match (candidate, market_obs.pm_trailing_price) {
                            (Some(c), Some(price)) => price
                                .to_string()
                                .parse::<f64>()
                                .ok()
                                .map(|p| c.adjusted_win_prob - p),
                            _ => None,
                        };
                        let mut is_trade_candidate = matches!(
                            (edge, market_obs.pm_trailing_price),
                            (Some(e), Some(price))
                                if e >= min_edge_f64 && price <= self.core.cfg.max_entry_price
                        );
                        if !trade_gate_open || !calendar_ids_for_trade.contains(&game.espn_game_id) {
                            is_trade_candidate = false;
                        }

                        if should_persist {
                            self.persist_observation(
                                game,
                                trailing.clone(),
                                candidate,
                                &market_obs,
                                edge,
                                is_trade_candidate,
                            )
                            .await;
                            last_observation_minute.insert(game.espn_game_id.clone(), now_minute_bucket);
                        }

                        market_inputs.insert(
                            game.espn_game_id.clone(),
                            MarketInput {
                                market_slug: market_obs.pm_event_slug.clone(),
                                trailing_token_id: market_obs.pm_trailing_token_id.clone(),
                                trailing_price: market_obs.pm_trailing_price,
                            },
                        );
                    }

                    // Update caches for Grok tick to use
                    live_games_cache = live_games.clone();
                    market_inputs_cache = market_inputs.clone();

                    // --- Exit management: settle at final, or early TP/SL sell ---
                    let mut game_status_by_id: HashMap<String, GameStatus> = HashMap::new();
                    let mut game_by_id: HashMap<String, LiveGame> = HashMap::new();
                    let mut trailing_by_game: HashMap<String, String> = HashMap::new();
                    for game in &live_games {
                        game_status_by_id.insert(game.espn_game_id.clone(), game.status);
                        game_by_id.insert(game.espn_game_id.clone(), game.clone());
                        if let Some((_, trailing_abbrev, _)) = game.trailing_team() {
                            trailing_by_game.insert(game.espn_game_id.clone(), trailing_abbrev);
                        }
                    }

                    let tracked_game_ids: Vec<String> =
                        self.core.state.game_positions.keys().cloned().collect();
                    for game_id in tracked_game_ids {
                        let Some(pos) = self.core.state.game_positions.get(&game_id).cloned() else {
                            continue;
                        };

                        if matches!(game_status_by_id.get(&game_id), Some(GameStatus::Final)) {
                            if let Some(game) = game_by_id.get(&game_id) {
                                if let Some(settle_price) =
                                    Self::settlement_price_for_position(&pos, game)
                                {
                                    if let Some(realized_pnl) =
                                        Self::position_realized_pnl(&pos, settle_price)
                                    {
                                        self.core.record_realized_pnl(realized_pnl);
                                        daily_pnl = self.core.state.daily_realized_pnl_usd;
                                        info!(
                                            agent = self.config.agent_id,
                                            game_id = %game_id,
                                            settle_price = %settle_price,
                                            realized_pnl = %realized_pnl,
                                            daily_realized_pnl = %daily_pnl,
                                            "recorded final settlement pnl"
                                        );
                                    }
                                }
                            }
                            self.core.close_position(&game_id);
                            self.persist_state_best_effort("final_settlement").await;
                            info!(
                                agent = self.config.agent_id,
                                game_id = %game_id,
                                "position settled at final status; closed local state"
                            );
                            continue;
                        }

                        if !self.core.cfg.early_exit_enabled || !trade_gate_open {
                            continue;
                        }
                        if self.is_on_cooldown(&game_id) {
                            continue;
                        }

                        let Some(market_input) = market_inputs.get(&game_id) else {
                            continue;
                        };
                        let Some(current_price) = market_input.trailing_price else {
                            continue;
                        };
                        let Some(avg_entry_price) = Self::position_avg_entry_price(&pos) else {
                            continue;
                        };

                        let Some(exit_reason) = Self::classify_early_exit(
                            avg_entry_price,
                            current_price,
                            self.core.cfg.early_exit_take_profit_pct,
                            self.core.cfg.early_exit_stop_loss_pct,
                        ) else {
                            continue;
                        };

                        let market_slug = pos
                            .market_slug
                            .clone()
                            .or_else(|| market_input.market_slug.clone());
                        let token_id = pos
                            .token_id
                            .clone()
                            .or_else(|| market_input.trailing_token_id.clone());
                        let (Some(market_slug), Some(token_id)) = (market_slug, token_id) else {
                            continue;
                        };

                        if pos.total_shares == 0 {
                            continue;
                        }

                        let trailing_team = trailing_by_game
                            .get(&game_id)
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());
                        let intent = Self::exit_intent(
                            &self.config.agent_id,
                            &game_id,
                            &trailing_team,
                            &market_slug,
                            &token_id,
                            pos.total_shares,
                            current_price,
                            exit_reason,
                            &config_hash,
                        );

                        match ctx.submit_order(intent).await {
                            Ok(()) => {
                                if let Some(realized_pnl) =
                                    Self::position_realized_pnl(&pos, current_price)
                                {
                                    self.core.record_realized_pnl(realized_pnl);
                                    daily_pnl = self.core.state.daily_realized_pnl_usd;
                                }
                                self.core.close_position(&game_id);
                                self.set_cooldown(&game_id);
                                self.persist_state_best_effort("early_exit").await;
                                info!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    reason = exit_reason,
                                    shares = pos.total_shares,
                                    price = %current_price,
                                    avg_entry = %avg_entry_price,
                                    daily_realized_pnl = %daily_pnl,
                                    "submitted early exit"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    reason = exit_reason,
                                    error = %e,
                                    "failed to submit early exit order"
                                );
                            }
                        }
                    }

                    if candidates.is_empty() {
                        debug!(agent = self.config.agent_id, games = live_games.len(), "no NBA candidates this cycle");
                        continue;
                    }

                    if !trade_gate_open {
                        debug!(
                            agent = self.config.agent_id,
                            "skipping order submission: trade gate closed"
                        );
                        continue;
                    }

                    for candidate in &candidates {
                        if !calendar_ids_for_trade.contains(&candidate.game.espn_game_id) {
                            continue;
                        }
                        let market_input = market_inputs.get(&candidate.game.espn_game_id);
                        let market_slug = market_input
                            .and_then(|m| m.market_slug.clone())
                            .unwrap_or_else(|| {
                                format!(
                                    "nba-{}-vs-{}",
                                    candidate.game.away_abbrev.to_lowercase(),
                                    candidate.game.home_abbrev.to_lowercase()
                                )
                            });
                        let token_id = market_input
                            .and_then(|m| m.trailing_token_id.clone())
                            .unwrap_or_else(|| format!("{}-win-yes", candidate.trailing_abbrev.to_lowercase()));
                        let market_price = market_input
                            .and_then(|m| m.trailing_price)
                            .unwrap_or_else(|| {
                                Decimal::from_f64_retain(candidate.adjusted_win_prob * 0.85)
                                    .unwrap_or(dec!(0.50))
                            });

                        if let Some(opp) = self.core.evaluate_opportunity(
                            candidate,
                            market_price,
                            market_slug.clone(),
                            token_id.clone(),
                        ) {
                            let game_id = opp.game.espn_game_id.clone();
                            let market_price_f64 = opp.market_price.to_string().parse::<f64>().unwrap_or(0.0);
                            if self.core.is_duplicate_initial_entry(&game_id, &opp.token_id) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    token_id = %opp.token_id,
                                    "skipping ESPN entry: duplicate initial entry"
                                );
                                continue;
                            }

                            // Calculate risk metrics and pre-filter on reward-to-risk ratio
                            let risk_metrics = RiskMetrics::calculate(opp.adjusted_win_prob, market_price_f64);
                            if !risk_metrics.passes_filter(
                                self.core.cfg.min_reward_risk_ratio,
                                self.core.cfg.min_expected_value,
                            ) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    rr = format!("{:.1}x", risk_metrics.reward_risk_ratio),
                                    ev = format!("{:.1}%", risk_metrics.expected_value * 100.0),
                                    min_rr = self.core.cfg.min_reward_risk_ratio,
                                    "skipping: reward-to-risk ratio below threshold"
                                );
                                continue;
                            }

                            if !self.core.can_open_new_risk() {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    daily_realized_pnl = %self.core.state.daily_realized_pnl_usd,
                                    daily_loss_limit = %self.core.cfg.performance_daily_loss_limit_usd,
                                    "skipping ESPN entry: daily loss limit reached"
                                );
                                continue;
                            }

                            let entry_shares = self.core.adjusted_shares(self.core.cfg.shares);

                            // Build unified decision request with all available context
                            let req = UnifiedDecisionRequest {
                                request_id: Uuid::new_v4(),
                                trigger: DecisionTrigger::EspnComeback,
                                game: opp.game.clone(),
                                trailing_team: opp.trailing_team.clone(),
                                trailing_abbrev: opp.trailing_abbrev.clone(),
                                deficit: opp.deficit,
                                comeback: Some(ComebackSnapshot {
                                    comeback_rate: opp.comeback_rate,
                                    adjusted_win_prob: opp.adjusted_win_prob,
                                    statistical_edge: opp.edge,
                                }),
                                grok_intel: self.grok_cache.get(&game_id).cloned(),
                                market: MarketSnapshot {
                                    market_slug: opp.market_slug.clone(),
                                    token_id: opp.token_id.clone(),
                                    market_price: opp.market_price,
                                    yes_best_bid: market_input.and_then(|_| None),
                                    yes_best_ask: market_input.and_then(|_| None),
                                },
                                risk_metrics,
                            };

                            // Check decision cooldown
                            if self.is_on_cooldown(&game_id) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    "skipping ESPN decision: on cooldown"
                                );
                                continue;
                            }

                            // Route through Grok unified decision
                            if let Some(grok) = self.grok.as_ref() {
                                match grok_decision::request_unified_decision(grok, &req).await {
                                    Ok((decision, raw_prompt, raw_response)) => {
                                        let mut order_submitted = false;

                                        match &decision {
                                            GrokDecision::Trade { edge, confidence, .. } => {
                                                info!(
                                                    agent = self.config.agent_id,
                                                    game_id = %game_id,
                                                    edge = format!("{:.3}", edge),
                                                    confidence = format!("{:.2}", confidence),
                                                    "grok approved ESPN comeback trade"
                                                );
                                                let intent = Self::decision_to_intent(
                                                    &self.config.agent_id,
                                                    &req,
                                                    &decision,
                                                    entry_shares,
                                                    &config_hash,
                                                );
                                                let intent_id = intent.intent_id;
                                                order_submitted = ctx.submit_order(intent).await.is_ok();
                                                if order_submitted {
                                                    pending_intents.insert(intent_id, opp.clone());
                                                    self.record_initial_espn_entry(
                                                        &game_id,
                                                        &opp.trailing_abbrev,
                                                        &opp.market_slug,
                                                        &opp.token_id,
                                                        opp.market_price,
                                                        entry_shares,
                                                        opp.comeback_rate,
                                                    );
                                                    self.persist_state_best_effort("espn_initial_entry")
                                                        .await;
                                                }
                                            }
                                            GrokDecision::Pass { reasoning, .. } => {
                                                info!(
                                                    agent = self.config.agent_id,
                                                    game_id = %game_id,
                                                    "grok passed on ESPN comeback: {}", reasoning
                                                );
                                            }
                                        }

                                        self.set_cooldown(&game_id);

                                        if let Some(pool) = self.observation_pool.as_ref() {
                                            Self::persist_unified_decision(
                                                pool,
                                                &self.config.account_id,
                                                &self.config.agent_id,
                                                &req,
                                                &decision,
                                                &raw_prompt,
                                                &raw_response,
                                                order_submitted,
                                            )
                                            .await;
                                        }
                                    }
                                    Err(e) => {
                                        // FALLBACK: ESPN comeback has its own statistical model
                                        if self.core.cfg.grok_fallback_enabled {
                                            warn!(
                                                agent = self.config.agent_id,
                                                game_id = %game_id,
                                                error = %e,
                                                "grok unavailable, falling back to rule-based for ESPN signal"
                                            );
                                            let intent = Self::opportunity_to_intent(
                                                &self.config.agent_id,
                                                &opp,
                                                entry_shares,
                                                &config_hash,
                                            );
                                            let intent_id = intent.intent_id;
                                            if let Err(e) = ctx.submit_order(intent).await {
                                                warn!(agent = self.config.agent_id, error = %e, "failed to submit fallback order");
                                            } else {
                                                self.record_initial_espn_entry(
                                                    &game_id,
                                                    &opp.trailing_abbrev,
                                                    &opp.market_slug,
                                                    &opp.token_id,
                                                    opp.market_price,
                                                    entry_shares,
                                                    opp.comeback_rate,
                                                );
                                                self.persist_state_best_effort("espn_fallback_entry")
                                                    .await;
                                                pending_intents.insert(intent_id, opp);
                                            }
                                        } else {
                                            warn!(
                                                agent = self.config.agent_id,
                                                game_id = %game_id,
                                                error = %e,
                                                "grok unavailable and fallback disabled, skipping"
                                            );
                                        }
                                    }
                                }
                            } else {
                                // No Grok configured — fall back to rule-based
                                let intent = Self::opportunity_to_intent(
                                    &self.config.agent_id,
                                    &opp,
                                    entry_shares,
                                    &config_hash,
                                );
                                let intent_id = intent.intent_id;
                                if let Err(e) = ctx.submit_order(intent).await {
                                    warn!(agent = self.config.agent_id, error = %e, "failed to submit");
                                } else {
                                    self.record_initial_espn_entry(
                                        &game_id,
                                        &opp.trailing_abbrev,
                                        &opp.market_slug,
                                        &opp.token_id,
                                        opp.market_price,
                                        entry_shares,
                                        opp.comeback_rate,
                                    );
                                    self.persist_state_best_effort("espn_rule_entry").await;
                                    pending_intents.insert(intent_id, opp);
                                }
                            }
                        }
                    }

                    // --- Kelly scaling-in check for existing positions ---
                    if self.core.cfg.scaling_enabled && trade_gate_open {
                        for candidate in &candidates {
                            let game_id = &candidate.game.espn_game_id;

                            if !calendar_ids_for_trade.contains(game_id) {
                                continue;
                            }

                            // Only scale into games we already have a position in
                            let has_position = self.core.state.game_positions.contains_key(game_id);
                            if !has_position {
                                continue;
                            }
                            if !self.core.can_open_new_risk() {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    daily_realized_pnl = %self.core.state.daily_realized_pnl_usd,
                                    daily_loss_limit = %self.core.cfg.performance_daily_loss_limit_usd,
                                    "scaling blocked: daily loss limit reached"
                                );
                                continue;
                            }

                            // Get current market data
                            let market_input = market_inputs.get(game_id);
                            let current_price = match market_input.and_then(|m| m.trailing_price) {
                                Some(p) => p,
                                None => continue,
                            };

                            // Check all scaling guards
                            if !self.core.can_scale_in(
                                game_id,
                                current_price,
                                candidate.comeback_rate,
                                candidate.game.time_remaining_mins,
                            ) {
                                continue;
                            }

                            // Calculate Kelly optimal shares to add
                            let raw_delta_shares = match self.core.kelly_scaling_shares(
                                game_id,
                                current_price,
                                candidate.adjusted_win_prob,
                            ) {
                                Some(s) => s,
                                None => continue,
                            };
                            let delta_shares = self.core.adjusted_shares(raw_delta_shares);

                            // Check daily spend limit
                            let add_cost = current_price * Decimal::from(delta_shares);
                            if !self.core.can_spend(add_cost) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    "scaling: daily spend limit would be exceeded"
                                );
                                continue;
                            }

                            let market_price_f64 = current_price.to_string().parse::<f64>().unwrap_or(0.0);
                            let risk_metrics = RiskMetrics::calculate(candidate.adjusted_win_prob, market_price_f64);

                            // Risk metrics must still pass filter for scale-in
                            if !risk_metrics.passes_filter(
                                self.core.cfg.min_reward_risk_ratio,
                                self.core.cfg.min_expected_value,
                            ) {
                                continue;
                            }

                            // Check decision cooldown
                            if self.is_on_cooldown(game_id) {
                                continue;
                            }

                            let pos = self.core.state.game_positions.get(game_id).unwrap();
                            let add_number = pos.entries.len() as u32; // 1-based (entry 0 was initial)
                            let existing_shares = pos.total_shares;
                            let existing_cost = pos.total_cost.to_string().parse::<f64>().unwrap_or(0.0);

                            let market_slug = market_input
                                .and_then(|m| m.market_slug.clone())
                                .unwrap_or_else(|| {
                                    format!(
                                        "nba-{}-vs-{}",
                                        candidate.game.away_abbrev.to_lowercase(),
                                        candidate.game.home_abbrev.to_lowercase()
                                    )
                                });
                            let token_id = market_input
                                .and_then(|m| m.trailing_token_id.clone())
                                .unwrap_or_else(|| {
                                    format!("{}-win-yes", candidate.trailing_abbrev.to_lowercase())
                                });

                            let req = UnifiedDecisionRequest {
                                request_id: Uuid::new_v4(),
                                trigger: DecisionTrigger::EspnScaleIn {
                                    add_number,
                                    existing_shares,
                                    existing_cost_usd: existing_cost,
                                },
                                game: candidate.game.clone(),
                                trailing_team: candidate.trailing_team.clone(),
                                trailing_abbrev: candidate.trailing_abbrev.clone(),
                                deficit: candidate.deficit,
                                comeback: Some(ComebackSnapshot {
                                    comeback_rate: candidate.comeback_rate,
                                    adjusted_win_prob: candidate.adjusted_win_prob,
                                    statistical_edge: candidate.adjusted_win_prob - market_price_f64,
                                }),
                                grok_intel: self.grok_cache.get(game_id).cloned(),
                                market: MarketSnapshot {
                                    market_slug,
                                    token_id,
                                    market_price: current_price,
                                    yes_best_bid: None,
                                    yes_best_ask: None,
                                },
                                risk_metrics,
                            };

                            info!(
                                agent = self.config.agent_id,
                                game_id = %game_id,
                                add_number,
                                delta_shares,
                                existing_shares,
                                price = %current_price,
                                "scaling: Kelly recommends adding to position"
                            );

                            // Route through Grok unified decision (or fallback)
                            if let Some(grok) = self.grok.as_ref() {
                                match grok_decision::request_unified_decision(grok, &req).await {
                                    Ok((decision, raw_prompt, raw_response)) => {
                                        let mut order_submitted = false;

                                        match &decision {
                                            GrokDecision::Trade { edge, confidence, .. } => {
                                                info!(
                                                    agent = self.config.agent_id,
                                                    game_id = %game_id,
                                                    add_number,
                                                    delta_shares,
                                                    edge = format!("{:.3}", edge),
                                                    confidence = format!("{:.2}", confidence),
                                                    "grok approved scale-in"
                                                );
                                                let intent = Self::decision_to_intent(
                                                    &self.config.agent_id,
                                                    &req,
                                                    &decision,
                                                    delta_shares,
                                                    &config_hash,
                                                );
                                                order_submitted = ctx.submit_order(intent).await.is_ok();
                                                if order_submitted {
                                                    self.core.record_position_entry(
                                                        game_id,
                                                        current_price,
                                                        delta_shares,
                                                        candidate.comeback_rate,
                                                    );
                                                    self.core.record_trade(game_id, add_cost);
                                                    self.persist_state_best_effort("scale_in").await;
                                                }
                                            }
                                            GrokDecision::Pass { reasoning, .. } => {
                                                info!(
                                                    agent = self.config.agent_id,
                                                    game_id = %game_id,
                                                    "grok passed on scale-in: {}", reasoning
                                                );
                                            }
                                        }

                                        self.set_cooldown(game_id);

                                        if let Some(pool) = self.observation_pool.as_ref() {
                                            Self::persist_unified_decision(
                                                pool,
                                                &self.config.account_id,
                                                &self.config.agent_id,
                                                &req,
                                                &decision,
                                                &raw_prompt,
                                                &raw_response,
                                                order_submitted,
                                            )
                                            .await;
                                        }
                                    }
                                    Err(e) => {
                                        // For scale-in, we do NOT fall back to rule-based.
                                        // Adding to an existing position is higher risk than
                                        // the initial entry, so we require LLM confirmation.
                                        warn!(
                                            agent = self.config.agent_id,
                                            game_id = %game_id,
                                            error = %e,
                                            "grok unavailable for scale-in, skipping (no fallback)"
                                        );
                                    }
                                }
                            }
                            // No fallback for scale-in when Grok is not configured
                        }
                    }
                }

                // --- Grok live search tick ---
                _ = grok_tick.tick() => {
                    if self.grok.is_none() || !self.core.cfg.grok_enabled {
                        continue;
                    }
                    if !matches!(status, AgentStatus::Running) {
                        continue;
                    }
                    if live_games_cache.is_empty() {
                        continue;
                    }

                    let grok_min_edge = self.core.cfg.grok_min_edge.to_string().parse::<f64>().unwrap_or(0.08);
                    let grok_min_confidence = self.core.cfg.grok_min_confidence;

                    for game in &live_games_cache {
                        if game.status != GameStatus::InProgress {
                            continue;
                        }

                        // Query Grok for this game (borrow grok ref in limited scope)
                        let intel = {
                            let grok = self.grok.as_ref().unwrap();
                            match grok_intel::query_grok_for_game(grok, game).await {
                                Ok(intel) => intel,
                                Err(e) => {
                                    warn!(
                                        agent = self.config.agent_id,
                                        game_id = %game.espn_game_id,
                                        error = %e,
                                        "grok query failed, skipping game"
                                    );
                                    continue;
                                }
                            }
                        };
                        self.grok_cache.insert(game.espn_game_id.clone(), intel.clone());

                        // Determine trailing team for signal evaluation
                        let trailing = match game.trailing_team() {
                            Some((_, abbrev, _)) => abbrev,
                            None => continue, // Tied — skip
                        };

                        // Look up market data from cached ESPN poll
                        let market_input = market_inputs_cache.get(&game.espn_game_id);
                        let trailing_price = market_input.and_then(|m| m.trailing_price);

                        // Evaluate for independent trading signal
                        let signal = trailing_price.and_then(|price| {
                            GrokSignalEvaluator::evaluate(
                                &intel,
                                game,
                                &trailing,
                                price,
                                grok_min_edge,
                                grok_min_confidence,
                            )
                        });

                        // Persist to DB
                        if let Some(pool) = self.observation_pool.as_ref() {
                            Self::persist_grok_intel(
                                pool,
                                &self.config.account_id,
                                &self.config.agent_id,
                                game,
                                &intel,
                                signal.as_ref(),
                                signal.is_some(),
                            )
                            .await;
                        }

                        // If signal found, route through unified Grok decision
                        if let Some(ref sig) = signal {
                            let game_id = game.espn_game_id.clone();
                            let market_slug = market_input
                                .and_then(|m| m.market_slug.clone())
                                .unwrap_or_else(|| {
                                    format!(
                                        "nba-{}-vs-{}",
                                        game.away_abbrev.to_lowercase(),
                                        game.home_abbrev.to_lowercase()
                                    )
                                });
                            let token_id = market_input
                                .and_then(|m| m.trailing_token_id.clone())
                                .unwrap_or_else(|| {
                                    format!("{}-win-yes", sig.target_team_abbrev.to_lowercase())
                                });
                            if self.core.is_duplicate_initial_entry(&game_id, &token_id) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    token_id = %token_id,
                                    "skipping grok signal entry: duplicate initial entry"
                                );
                                continue;
                            }
                            if !self.core.can_open_new_risk() {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    daily_realized_pnl = %self.core.state.daily_realized_pnl_usd,
                                    daily_loss_limit = %self.core.cfg.performance_daily_loss_limit_usd,
                                    "skipping grok signal entry: daily loss limit reached"
                                );
                                continue;
                            }
                            let entry_shares = self.core.adjusted_shares(self.core.cfg.shares);
                            let entry_notional =
                                Self::initial_entry_notional(sig.market_price, entry_shares);
                            if !self.core.can_spend(entry_notional) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    spend = %entry_notional,
                                    "skipping grok signal entry: daily spend limit"
                                );
                                continue;
                            }

                            let (trailing_team, trailing_abbrev, deficit) = match game.trailing_team() {
                                Some(t) => t,
                                None => continue,
                            };

                            // Check decision cooldown
                            if self.is_on_cooldown(&game_id) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    "skipping Grok signal decision: on cooldown"
                                );
                                continue;
                            }

                            let sig_price_f64 = sig.market_price.to_string().parse::<f64>().unwrap_or(0.0);
                            // For Grok signal path, use grok_home_win_prob as fair value estimate
                            let grok_fair_value = if trailing == game.home_abbrev {
                                intel.grok_home_win_prob.unwrap_or(sig_price_f64)
                            } else {
                                intel.grok_home_win_prob.map(|p| 1.0 - p).unwrap_or(sig_price_f64)
                            };
                            let risk_metrics = RiskMetrics::calculate(grok_fair_value, sig_price_f64);

                            if !risk_metrics.passes_filter(
                                self.core.cfg.min_reward_risk_ratio,
                                self.core.cfg.min_expected_value,
                            ) {
                                debug!(
                                    agent = self.config.agent_id,
                                    game_id = %game_id,
                                    rr = format!("{:.1}x", risk_metrics.reward_risk_ratio),
                                    "skipping grok signal: reward-to-risk ratio below threshold"
                                );
                                continue;
                            }

                            let req = UnifiedDecisionRequest {
                                request_id: Uuid::new_v4(),
                                trigger: DecisionTrigger::GrokSignal(sig.signal_type),
                                game: game.clone(),
                                trailing_team,
                                trailing_abbrev,
                                deficit,
                                comeback: None, // no statistical model for this trigger
                                grok_intel: Some(intel.clone()),
                                market: MarketSnapshot {
                                    market_slug,
                                    token_id,
                                    market_price: sig.market_price,
                                    yes_best_bid: None,
                                    yes_best_ask: None,
                                },
                                risk_metrics,
                            };

                            match grok_decision::request_unified_decision(self.grok.as_ref().unwrap(), &req).await {
                                Ok((decision, raw_prompt, raw_response)) => {
                                    let mut order_submitted = false;

                                    match &decision {
                                        GrokDecision::Trade { edge, confidence, .. } => {
                                            info!(
                                                agent = self.config.agent_id,
                                                game_id = %game_id,
                                                signal_type = %sig.signal_type,
                                                edge = format!("{:.3}", edge),
                                                confidence = format!("{:.2}", confidence),
                                                "grok approved grok-signal trade"
                                            );
                                            let intent = Self::decision_to_intent(
                                                &self.config.agent_id,
                                                &req,
                                                &decision,
                                                entry_shares,
                                                &config_hash,
                                            );
                                            let intent_id = intent.intent_id;
                                            order_submitted = ctx.submit_order(intent).await.is_ok();
                                            if order_submitted {
                                                self.record_initial_grok_signal_entry(
                                                    &game_id,
                                                    &req.trailing_abbrev,
                                                    &req.market.market_slug,
                                                    &req.market.token_id,
                                                    req.market.market_price,
                                                    entry_shares,
                                                );
                                                self.persist_state_best_effort("grok_signal_entry")
                                                    .await;
                                                info!(
                                                    agent = self.config.agent_id,
                                                    intent_id = %intent_id,
                                                    "grok-signal unified order submitted"
                                                );
                                            }
                                        }
                                        GrokDecision::Pass { reasoning, .. } => {
                                            info!(
                                                agent = self.config.agent_id,
                                                game_id = %game_id,
                                                "grok passed on grok signal: {}", reasoning
                                            );
                                        }
                                    }

                                    self.set_cooldown(&game_id);

                                    if let Some(pool) = self.observation_pool.as_ref() {
                                        Self::persist_unified_decision(
                                            pool,
                                            &self.config.account_id,
                                            &self.config.agent_id,
                                            &req,
                                            &decision,
                                            &raw_prompt,
                                            &raw_response,
                                            order_submitted,
                                        )
                                        .await;
                                    }
                                }
                                Err(e) => {
                                    // NO FALLBACK: Grok signal path has no independent model
                                    warn!(
                                        agent = self.config.agent_id,
                                        game_id = %game_id,
                                        error = %e,
                                        "grok unavailable for grok signal, skipping (no fallback)"
                                    );
                                }
                            }
                        }
                    }
                }

                // --- Coordinator commands ---
                cmd = ctx.command_rx().recv() => {
                    match cmd {
                        Some(CoordinatorCommand::Pause) => {
                            info!(agent = self.config.agent_id, "pausing");
                            status = AgentStatus::Paused;
                        }
                        Some(CoordinatorCommand::Resume) => {
                            info!(agent = self.config.agent_id, "resuming");
                            status = AgentStatus::Running;
                        }
                        Some(CoordinatorCommand::Shutdown) | None => {
                            info!(agent = self.config.agent_id, "shutting down");
                            break;
                        }
                        Some(CoordinatorCommand::ForceClose) => {
                            warn!(agent = self.config.agent_id, "force close requested");
                            force_close_requested = true;
                            break;
                        }
                        Some(CoordinatorCommand::HealthCheck(tx)) => {
                            let snapshot = crate::coordinator::AgentSnapshot {
                                agent_id: self.config.agent_id.clone(),
                                name: self.config.name.clone(),
                                domain: Domain::Sports,
                                status,
                                position_count,
                                exposure: total_exposure,
                                daily_pnl,
                                unrealized_pnl: Decimal::ZERO,
                                metrics: self.performance_metrics(daily_pnl),
                                last_heartbeat: Utc::now(),
                                error_message: None,
                            };
                            let _ = tx.send(crate::coordinator::AgentHealthResponse {
                                snapshot,
                                is_healthy: matches!(status, AgentStatus::Running),
                                uptime_secs: 0,
                                orders_submitted: pending_intents.len() as u64,
                                orders_filled: position_count as u64,
                            });
                        }
                    }
                }

                // --- Heartbeat ---
                _ = heartbeat_tick.tick() => {
                    let _ = ctx.report_state_with_metrics(
                        &self.config.name,
                        status,
                        position_count,
                        total_exposure,
                        daily_pnl,
                        Decimal::ZERO,
                        self.performance_metrics(daily_pnl),
                        None,
                    ).await;
                }
            }
        }

        if force_close_requested {
            self.submit_force_close_exits(&ctx).await;
        }

        info!(agent = self.config.agent_id, "sports agent stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_config_defaults() {
        let cfg = SportsTradingConfig::default();
        assert_eq!(cfg.agent_id, "sports");
        assert_eq!(cfg.poll_interval_secs, 30);
    }

    #[test]
    fn test_classify_early_exit_take_profit_and_stop_loss() {
        let tp = SportsTradingAgent::classify_early_exit(dec!(0.30), dec!(0.36), 15.0, 20.0);
        assert_eq!(tp, Some("take_profit"));

        let sl = SportsTradingAgent::classify_early_exit(dec!(0.30), dec!(0.23), 15.0, 20.0);
        assert_eq!(sl, Some("stop_loss"));

        let hold = SportsTradingAgent::classify_early_exit(dec!(0.30), dec!(0.31), 15.0, 20.0);
        assert_eq!(hold, None);
    }
}
