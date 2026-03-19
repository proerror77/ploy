use super::*;

impl SportsAnalyst {
    pub(super) async fn fetch_market_odds(
        &self,
        event_slug: &str,
        team1: &str,
        team2: &str,
    ) -> Result<MarketOdds> {
        let client = PolymarketClient::new(CLOB_BASE_URL, true)?;

        let search_slug = if event_slug.contains('/') {
            event_slug.split('/').last().unwrap_or(event_slug)
        } else {
            event_slug
        };

        debug!("Searching Polymarket for slug: {}", search_slug);
        if let Some(odds) = self.try_fetch_odds(&client, search_slug).await {
            info!("Found market data via slug query");
            return Ok(odds);
        }

        let team1_short = self.get_team_short_name(team1);
        let team2_short = self.get_team_short_name(team2);
        debug!("Searching for teams: {} vs {}", team1_short, team2_short);

        let team_events = client
            .get_active_sports_events(&team1_short)
            .await
            .unwrap_or_default();
        if let Some(odds) = self
            .try_search_team_matchup(&client, &team_events, team1, team2)
            .await
        {
            info!("Found market data via team search");
            return Ok(odds);
        }

        let markets = client
            .search_markets(&team1_short)
            .await
            .unwrap_or_default();
        if let Some(odds) = self.try_search_markets(&markets, team1, team2) {
            info!("Found market data via markets search");
            return Ok(odds);
        }

        warn!(
            "Could not fetch market data for {} vs {} - market may not exist on Polymarket",
            team1, team2
        );
        warn!("Analysis will proceed with Grok data only (no Polymarket odds comparison)");

        Ok(MarketOdds {
            team1_yes_price: Decimal::new(50, 2),
            team1_no_price: Decimal::new(50, 2),
            team2_yes_price: Some(Decimal::new(50, 2)),
            team2_no_price: Some(Decimal::new(50, 2)),
            spread: None,
        })
    }

    pub(super) async fn try_fetch_odds(
        &self,
        client: &PolymarketClient,
        slug: &str,
    ) -> Option<MarketOdds> {
        let events = client.get_active_sports_events(slug).await.ok()?;
        let normalized = slug.trim_matches('/');
        let candidate = events
            .iter()
            .find(|event| {
                event.slug.as_deref().is_some_and(|event_slug| {
                    let event_slug = event_slug.trim_matches('/');
                    event_slug == normalized || event_slug.ends_with(&format!("/{}", normalized))
                })
            })
            .cloned()
            .or_else(|| events.into_iter().next())?;

        let event = self.ensure_event_has_markets(client, &candidate).await?;
        self.parse_event_odds(&event)
    }

    pub(super) async fn try_search_team_matchup(
        &self,
        client: &PolymarketClient,
        events: &[GammaEventInfo],
        team1: &str,
        team2: &str,
    ) -> Option<MarketOdds> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        for event in events {
            if let Some(title) = event.title.as_deref() {
                let title_lower = title.to_lowercase();
                if title_lower.contains(&team1_lower) || title_lower.contains(&team2_lower) {
                    let Some(hydrated) = self.ensure_event_has_markets(client, event).await else {
                        continue;
                    };
                    if let Some(odds) = self.parse_event_odds(&hydrated) {
                        return Some(odds);
                    }
                }
            }
        }

        None
    }

    pub(super) fn try_search_markets(
        &self,
        markets: &[GammaMarketSummary],
        team1: &str,
        team2: &str,
    ) -> Option<MarketOdds> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        for market in markets {
            if let Some(question) = market.question.as_deref() {
                let question_lower = question.to_lowercase();
                if (question_lower.contains(&team1_lower) || question_lower.contains(&team2_lower))
                    && (question_lower.contains("win") || question_lower.contains("beat"))
                {
                    return self.parse_market_summary_odds(market);
                }
            }
        }

        None
    }

    pub(super) fn parse_event_odds(&self, event: &GammaEventInfo) -> Option<MarketOdds> {
        let market = event.markets.first()?;
        self.parse_market_odds(market)
    }

    pub(super) fn parse_market_odds(&self, market: &GammaMarketInfo) -> Option<MarketOdds> {
        let yes_price = self
            .parse_yes_price(market.outcome_prices.as_deref())
            .unwrap_or(0.5);
        self.build_odds_from_yes_price(yes_price)
    }

    pub(super) fn parse_market_summary_odds(
        &self,
        market: &GammaMarketSummary,
    ) -> Option<MarketOdds> {
        let yes_price = self
            .parse_yes_price(market.outcome_prices.as_deref())
            .unwrap_or(0.5);
        self.build_odds_from_yes_price(yes_price)
    }

    pub(super) fn parse_yes_price(&self, outcome_prices: Option<&str>) -> Option<f64> {
        let raw = outcome_prices?;
        if let Ok(arr) = serde_json::from_str::<Vec<String>>(raw) {
            return arr.first().and_then(|v| v.parse::<f64>().ok());
        }
        if let Ok(arr) = serde_json::from_str::<Vec<f64>>(raw) {
            return arr.first().copied();
        }
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
            return arr.first().and_then(|v| {
                v.as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
            });
        }
        None
    }

    pub(super) fn build_odds_from_yes_price(&self, yes_price: f64) -> Option<MarketOdds> {
        Some(MarketOdds {
            team1_yes_price: Decimal::from_f64_retain(yes_price).unwrap_or(Decimal::new(50, 2)),
            team1_no_price: Decimal::from_f64_retain(1.0 - yes_price)
                .unwrap_or(Decimal::new(50, 2)),
            team2_yes_price: Some(
                Decimal::from_f64_retain(1.0 - yes_price).unwrap_or(Decimal::new(50, 2)),
            ),
            team2_no_price: Some(
                Decimal::from_f64_retain(yes_price).unwrap_or(Decimal::new(50, 2)),
            ),
            spread: None,
        })
    }

    pub(super) async fn ensure_event_has_markets(
        &self,
        client: &PolymarketClient,
        event: &GammaEventInfo,
    ) -> Option<GammaEventInfo> {
        if !event.markets.is_empty() {
            return Some(event.clone());
        }
        client.get_event_details(&event.id).await.ok()
    }

    pub(super) fn get_team_short_name(&self, full_name: &str) -> String {
        let parts: Vec<&str> = full_name.split_whitespace().collect();
        if parts.len() > 1 {
            parts.last().unwrap_or(&full_name).to_string()
        } else {
            full_name.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_analyst() -> SportsAnalyst {
        let grok = GrokClient::new(crate::ai_clients::grok::GrokConfig::default()).unwrap();
        let claude = ClaudeAgentClient::new();
        SportsAnalyst::new(grok, claude)
    }

    #[test]
    fn test_parse_yes_price_accepts_string_arrays() {
        let analyst = create_test_analyst();
        let parsed = analyst.parse_yes_price(Some(r#"["0.61","0.39"]"#));
        assert_eq!(parsed, Some(0.61));
    }

    #[test]
    fn test_build_odds_from_yes_price_stays_symmetric() {
        let analyst = create_test_analyst();
        let odds = analyst.build_odds_from_yes_price(0.625).unwrap();

        assert_eq!(odds.team1_yes_price, Decimal::new(625, 3));
        assert_eq!(odds.team1_no_price, Decimal::new(375, 3));
        assert_eq!(odds.team2_yes_price, Some(Decimal::new(375, 3)));
        assert_eq!(odds.team2_no_price, Some(Decimal::new(625, 3)));
    }
}
