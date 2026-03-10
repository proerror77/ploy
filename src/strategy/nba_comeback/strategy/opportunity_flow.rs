use crate::ai_clients::{EventDetails, LiveGameMarket};
use crate::domain::{OrderType, Side, TimeInForce};
use crate::error::Result;
use crate::platform::Domain;
use crate::strategy::nba_comeback::core::ComebackOpportunity;
use crate::strategy::nba_comeback::espn::LiveGame;
use crate::strategy::traits::{
    StrategyAction, StrategyEvent, StrategyEventType, StrategyOrderIntent,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

use super::{
    NBA_COMEBACK_PRIORITY, NBA_COMEBACK_STRATEGY_NAME, NbaComebackMarketRegistration,
    NbaComebackStrategy, PendingNbaComebackOrder,
};

impl NbaComebackMarketRegistration {
    pub(super) fn matches_game(&self, game: &LiveGame) -> bool {
        self.game_id
            .as_deref()
            .map(|id| id == game.espn_game_id)
            .unwrap_or(false)
            || (same_text(&self.home_team, &game.home_team)
                && same_text(&self.away_team, &game.away_team))
            || (same_text(&self.home_abbrev, &game.home_abbrev)
                && same_text(&self.away_abbrev, &game.away_abbrev))
    }

    pub(super) fn for_trailing_game(
        &self,
        game: &LiveGame,
    ) -> Option<(String, Decimal, String, Option<String>)> {
        let (_, trailing_abbrev, _) = game.trailing_team()?;
        if same_text(&self.home_abbrev, &trailing_abbrev) {
            return Some((
                self.home_token_id.clone(),
                self.home_price,
                self.market_slug.clone(),
                self.condition_id.clone(),
            ));
        }
        if same_text(&self.away_abbrev, &trailing_abbrev) {
            return Some((
                self.away_token_id.clone(),
                self.away_price,
                self.market_slug.clone(),
                self.condition_id.clone(),
            ));
        }
        None
    }
}

impl NbaComebackStrategy {
    pub(super) fn can_open_notional(&self, amount: Decimal) -> bool {
        self.core.can_open_new_risk()
            && self.core.state.daily_spend_usd + self.reserved_notional_usd + amount
                <= self.core.cfg.max_daily_spend_usd
    }

    pub(super) fn reserve_pending_order(
        &mut self,
        opp: &ComebackOpportunity,
        client_order_id: String,
        condition_id: Option<String>,
    ) -> PendingNbaComebackOrder {
        let reserved_notional_usd = opp.market_price * Decimal::from(self.core.cfg.shares);
        let pending = PendingNbaComebackOrder {
            client_order_id: client_order_id.clone(),
            game_id: opp.game.espn_game_id.clone(),
            trailing_abbrev: opp.trailing_abbrev.clone(),
            token_id: opp.token_id.clone(),
            market_slug: opp.market_slug.clone(),
            condition_id,
            requested_shares: self.core.cfg.shares,
            accounted_filled_shares: 0,
            limit_price: opp.market_price,
            reserved_notional_usd,
        };
        self.reserved_notional_usd += reserved_notional_usd;
        self.pending_orders.insert(client_order_id, pending.clone());
        pending
    }

    pub(super) fn release_pending_order(
        &mut self,
        client_order_id: &str,
    ) -> Option<PendingNbaComebackOrder> {
        let pending = self.pending_orders.remove(client_order_id)?;
        self.reserved_notional_usd =
            (self.reserved_notional_usd - pending.reserved_notional_usd).max(Decimal::ZERO);
        Some(pending)
    }

    async fn ensure_stats_loaded(&mut self) -> Result<()> {
        if self.stats_loaded || self.core.stats.team_count() > 0 {
            self.stats_loaded = true;
            return Ok(());
        }
        self.core.stats.load_all().await?;
        self.stats_loaded = true;
        Ok(())
    }

    async fn resolve_market_registration(
        &self,
        game: &LiveGame,
    ) -> Result<Option<NbaComebackMarketRegistration>> {
        if let Some(registration) = self
            .market_registrations
            .iter()
            .find(|registration| registration.matches_game(game))
        {
            return Ok(Some(registration.clone()));
        }

        let Some(pm_sports) = self.pm_sports.as_ref() else {
            return Ok(None);
        };
        let Some(event) = pm_sports
            .find_live_game(&game.home_team, &game.away_team)
            .await?
        else {
            return Ok(None);
        };
        Ok(registration_from_event(game, &event))
    }

    pub(super) async fn collect_opportunities(
        &mut self,
    ) -> Result<Vec<(ComebackOpportunity, Option<String>)>> {
        self.ensure_stats_loaded().await?;
        let games = self.core.espn.fetch_live_games().await?;
        let candidates = self.core.scan_games(&games);
        let mut out = Vec::new();

        for candidate in candidates {
            if self.core.has_position(&candidate.game.espn_game_id) {
                continue;
            }

            let Some(registration) = self.resolve_market_registration(&candidate.game).await?
            else {
                continue;
            };
            let Some((token_id, market_price, market_slug, condition_id)) =
                registration.for_trailing_game(&candidate.game)
            else {
                continue;
            };

            let notional = market_price * Decimal::from(self.core.cfg.shares);
            if !self.can_open_notional(notional) {
                continue;
            }

            if self.positions.contains_key(&token_id)
                || self
                    .pending_orders
                    .values()
                    .any(|pending| pending.token_id == token_id)
            {
                continue;
            }

            if let Some(opp) =
                self.core
                    .evaluate_opportunity(&candidate, market_price, market_slug, token_id)
            {
                out.push((opp, condition_id));
            }
        }

        Ok(out)
    }

    pub(super) fn build_actions_for_opportunity_inner(
        &mut self,
        opp: &ComebackOpportunity,
        condition_id: Option<String>,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        let client_order_id = format!(
            "nba_comeback_{}_{}_{}",
            sanitize_component(&self.id),
            sanitize_component(&opp.game.espn_game_id),
            now.timestamp_millis()
        );
        let pending =
            self.reserve_pending_order(opp, client_order_id.clone(), condition_id.clone());

        let mut signal_event = StrategyEvent::new(
            StrategyEventType::SignalDetected,
            format!(
                "nba_comeback signal game={} trailing={} edge={:.4} price={:.4}",
                opp.game.espn_game_id, opp.trailing_abbrev, opp.edge, opp.market_price
            ),
        )
        .with_data("game_id", &opp.game.espn_game_id)
        .with_data("trailing_team", &opp.trailing_abbrev)
        .with_data("token_id", &opp.token_id)
        .with_data("market_slug", &opp.market_slug)
        .with_data("edge", opp.edge.to_string())
        .with_data("market_price", opp.market_price.to_string())
        .with_data("fair_value", format!("{:.6}", opp.adjusted_win_prob))
        .with_data("signal_type", "nba_comeback_entry")
        .with_data("client_order_id", &client_order_id);
        if let Some(condition_id) = condition_id {
            signal_event = signal_event.with_data("condition_id", condition_id);
        }

        self.last_scan_at = Some(now);

        vec![
            StrategyAction::LogEvent {
                event: signal_event,
            },
            StrategyAction::SubmitIntent {
                intent: StrategyOrderIntent {
                    client_order_id: pending.client_order_id,
                    domain: Domain::Sports,
                    market_slug: opp.market_slug.clone(),
                    token_id: opp.token_id.clone(),
                    side: Side::Up,
                    is_buy: true,
                    shares: self.core.cfg.shares,
                    limit_price: opp.market_price,
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    priority: NBA_COMEBACK_PRIORITY,
                    metadata: HashMap::from([
                        ("game_id".to_string(), opp.game.espn_game_id.clone()),
                        ("trailing_team".to_string(), opp.trailing_abbrev.clone()),
                        (
                            "strategy".to_string(),
                            NBA_COMEBACK_STRATEGY_NAME.to_string(),
                        ),
                    ]),
                },
            },
        ]
    }
}

fn registration_from_event(
    game: &LiveGame,
    event: &EventDetails,
) -> Option<NbaComebackMarketRegistration> {
    let market = event.moneyline()?;
    let (yes_token_id, no_token_id) = market.get_token_ids()?;
    let (yes_price, no_price) = market.get_prices()?;

    let home_idx = select_team_index(market, &game.home_team, &game.home_abbrev)?;
    let away_idx = select_team_index(market, &game.away_team, &game.away_abbrev)?;
    if home_idx == away_idx {
        return None;
    }

    let (home_token_id, home_price) = if home_idx == 0 {
        (yes_token_id.clone(), yes_price)
    } else {
        (no_token_id.clone(), no_price)
    };
    let (away_token_id, away_price) = if away_idx == 0 {
        (yes_token_id, yes_price)
    } else {
        (no_token_id, no_price)
    };

    Some(NbaComebackMarketRegistration {
        game_id: Some(game.espn_game_id.clone()),
        market_slug: event.slug.clone(),
        condition_id: market.condition_id.clone(),
        home_team: game.home_team.clone(),
        away_team: game.away_team.clone(),
        home_abbrev: game.home_abbrev.clone(),
        away_abbrev: game.away_abbrev.clone(),
        home_token_id,
        away_token_id,
        home_price,
        away_price,
    })
}

fn select_team_index(market: &LiveGameMarket, team_name: &str, team_abbrev: &str) -> Option<usize> {
    let outcomes = market
        .outcomes
        .as_ref()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())?;

    outcomes
        .iter()
        .enumerate()
        .take(2)
        .find_map(|(idx, outcome)| {
            if text_matches_team(outcome, team_name, team_abbrev) {
                Some(idx)
            } else {
                None
            }
        })
}

fn text_matches_team(text: &str, team_name: &str, team_abbrev: &str) -> bool {
    let text_norm = normalize_text(text);
    let name_norm = normalize_text(team_name);
    let abbrev_norm = normalize_text(team_abbrev);

    text_norm.contains(name_norm.trim()) || text_norm.contains(abbrev_norm.trim())
}

fn same_text(left: &str, right: &str) -> bool {
    normalize_text(left).trim() == normalize_text(right).trim()
}

fn normalize_text(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
