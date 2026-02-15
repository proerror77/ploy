//! SportsTradingAgent — pull-based agent for NBA comeback strategy
//!
//! Polls ESPN on a 30s interval, runs NbaComebackCore logic,
//! and submits OrderIntents via the coordinator.

use async_trait::async_trait;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::agent::polymarket_sports::{OrderBookLevel as SportsOrderBookLevel, SportsOrderBook};
use crate::agent::{EventDetails, LiveGameMarket, PolymarketSportsClient, NBA_SERIES_ID};
use crate::agents::{AgentContext, TradingAgent};
use crate::coordinator::CoordinatorCommand;
use crate::domain::Side;
use crate::error::Result;
use crate::platform::{AgentRiskParams, AgentStatus, Domain, OrderIntent, OrderPriority};
use crate::strategy::nba_comeback::core::{ComebackCandidate, ComebackOpportunity, NbaComebackCore};
use crate::strategy::nba_comeback::espn::{GameStatus, LiveGame};

/// Configuration for the SportsTradingAgent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SportsTradingConfig {
    pub agent_id: String,
    pub name: String,
    pub poll_interval_secs: u64,
    pub heartbeat_interval_secs: u64,
    pub risk_params: AgentRiskParams,
}

impl Default for SportsTradingConfig {
    fn default() -> Self {
        Self {
            agent_id: "sports".into(),
            name: "NBA Comeback".into(),
            poll_interval_secs: 30,
            heartbeat_interval_secs: 5,
            risk_params: AgentRiskParams::conservative(),
        }
    }
}

/// Pull-based sports trading agent wrapping NbaComebackCore
pub struct SportsTradingAgent {
    config: SportsTradingConfig,
    core: NbaComebackCore,
    observation_pool: Option<PgPool>,
    pm_sports: Option<PolymarketSportsClient>,
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

const NBA_TEAM_ABBREVS: &[&str] = &[
    "ATL", "BOS", "BKN", "CHA", "CHI", "CLE", "DAL", "DEN", "DET", "GSW", "HOU", "IND",
    "LAC", "LAL", "MEM", "MIA", "MIL", "MIN", "NOP", "NYK", "OKC", "ORL", "PHI", "PHX",
    "POR", "SAC", "SAS", "TOR", "UTA", "WAS",
];

impl SportsTradingAgent {
    pub fn new(config: SportsTradingConfig, core: NbaComebackCore) -> Self {
        Self {
            config,
            core,
            observation_pool: None,
            pm_sports: None,
        }
    }

    pub fn with_observability(mut self, observation_pool: PgPool, pm_sports: PolymarketSportsClient) -> Self {
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

    fn find_matching_pm_event<'a>(game: &LiveGame, pm_events: &'a [EventDetails]) -> Option<&'a EventDetails> {
        pm_events
            .iter()
            .find(|event| Self::event_matches_game(event, game))
    }

    fn select_trailing_side(market: &LiveGameMarket, trailing_team: &str, trailing_abbrev: &str) -> Option<usize> {
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

        if let (Some(pm_client), Some((yes_token, no_token))) = (self.pm_sports.as_ref(), tokens.as_ref()) {
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

    async fn ensure_observation_table(pool: &PgPool) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS nba_live_observations (
                id BIGSERIAL PRIMARY KEY,
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
                agent_id, espn_game_id, home_team, away_team, home_abbrev, away_abbrev,
                home_score, away_score, quarter, clock, time_remaining_mins, game_status,
                trailing_team, trailing_abbrev, deficit, comeback_rate, adjusted_win_prob,
                pm_event_id, pm_event_title, pm_event_slug, pm_live_status,
                pm_yes_token_id, pm_no_token_id, pm_yes_mid, pm_no_mid,
                pm_yes_best_bid, pm_yes_best_ask, pm_no_best_bid, pm_no_best_ask,
                pm_trailing_token_id, pm_trailing_price, pm_trailing_price_source,
                edge, is_trade_candidate
            )
            VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11, $12,
                $13, $14, $15, $16, $17,
                $18, $19, $20, $21,
                $22, $23, $24, $25,
                $26, $27, $28, $29,
                $30, $31, $32,
                $33, $34
            )
            "#,
        )
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

    fn parse_depth_levels(levels: &[SportsOrderBookLevel], is_bid: bool, max_levels: usize) -> Vec<DepthLevelJson> {
        use rust_decimal::Decimal;

        let mut parsed: Vec<(Decimal, Decimal)> = Vec::with_capacity(levels.len());
        for lvl in levels {
            let Ok(price) = lvl.price.parse::<Decimal>() else { continue };
            let Ok(size) = lvl.size.parse::<Decimal>() else { continue };
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
            self.persist_one_orderbook_snapshot(
                pool,
                book,
                "YES",
                game,
                market_obs,
                max_levels,
            )
            .await;
        }

        // Persist NO book snapshot (if available)
        if let Some(book) = market_obs.pm_no_book.as_ref() {
            self.persist_one_orderbook_snapshot(
                pool,
                book,
                "NO",
                game,
                market_obs,
                max_levels,
            )
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
        .with_metadata("game_id", &opp.game.espn_game_id)
        .with_metadata("trailing_team", &opp.trailing_abbrev)
        .with_metadata("deficit", &opp.deficit.to_string())
        .with_metadata("comeback_rate", &format!("{:.3}", opp.comeback_rate))
        .with_metadata("edge", &format!("{:.3}", opp.edge))
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

        // Load historical stats
        if let Err(e) = self.core.stats.load_all().await {
            warn!(agent = self.config.agent_id, error = %e, "failed to load NBA stats, continuing");
        }
        if let Some(pool) = self.observation_pool.as_ref() {
            if let Err(e) = Self::ensure_calendar_table(pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure nba_schedule_calendar table");
            }
            if let Err(e) = Self::ensure_observation_table(pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure nba_live_observations table");
            }
            if let Err(e) = crate::coordinator::bootstrap::ensure_clob_orderbook_snapshots_table(pool).await {
                warn!(agent = self.config.agent_id, error = %e, "failed to ensure clob_orderbook_snapshots table");
            }
        } else {
            warn!(
                agent = self.config.agent_id,
                "observation DB not configured; calendar-gated sports trading disabled"
            );
        }

        let mut status = AgentStatus::Running;
        let mut pending_intents: HashMap<Uuid, ComebackOpportunity> = HashMap::new();
        let position_count: usize = 0;
        let total_exposure = Decimal::ZERO;
        let daily_pnl = Decimal::ZERO;
        let mut last_calendar_sync_at: Option<chrono::DateTime<Utc>> = None;
        // Persist sports observations at a bounded cadence to keep DB volume sane.
        // Key: espn_game_id, Value: UTC minute bucket (unix_ts / 60).
        let mut last_observation_minute: HashMap<String, i64> = HashMap::new();

        let poll_dur = tokio::time::Duration::from_secs(self.config.poll_interval_secs);
        let heartbeat_dur = tokio::time::Duration::from_secs(self.config.heartbeat_interval_secs);
        let mut poll_tick = tokio::time::interval(poll_dur);
        let mut heartbeat_tick = tokio::time::interval(heartbeat_dur);
        poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                        continue;
                    }
                    let candidates = self.core.scan_games(&live_games);
                    let candidates_by_game: HashMap<String, ComebackCandidate> = candidates
                        .iter()
                        .map(|c| (c.game.espn_game_id.clone(), c.clone()))
                        .collect();

                    let pm_events = if let Some(pm_client) = self.pm_sports.as_ref() {
                        match pm_client.fetch_todays_games_with_details(NBA_SERIES_ID).await {
                            Ok(events) => events,
                            Err(e) => {
                                warn!(agent = self.config.agent_id, error = %e, "failed to fetch PM NBA game details");
                                Vec::new()
                            }
                        }
                    } else {
                        Vec::new()
                    };

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
                            market_slug,
                            token_id,
                        ) {
                            let intent = Self::opportunity_to_intent(
                                &self.config.agent_id,
                                &opp,
                                self.core.cfg.shares,
                            );
                            let intent_id = intent.intent_id;

                            if let Err(e) = ctx.submit_order(intent).await {
                                warn!(agent = self.config.agent_id, error = %e, "failed to submit");
                            } else {
                                pending_intents.insert(intent_id, opp);
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
                            warn!(agent = self.config.agent_id, "force close (no exit logic for sports)");
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
                    let _ = ctx.report_state(
                        &self.config.name,
                        status,
                        position_count,
                        total_exposure,
                        daily_pnl,
                        Decimal::ZERO,
                        None,
                    ).await;
                }
            }
        }

        info!(agent = self.config.agent_id, "sports agent stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = SportsTradingConfig::default();
        assert_eq!(cfg.agent_id, "sports");
        assert_eq!(cfg.poll_interval_secs, 30);
    }
}
