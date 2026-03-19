use super::*;
use polymarket_client_sdk::gamma::types::request::{EventByIdRequest, SeriesByIdRequest};
use tracing::{debug, info, warn};

impl PolymarketSportsClient {
    /// Fetch all events from a sports series
    pub async fn fetch_series_events(&self, series_id: &str) -> Result<Vec<LiveGameEvent>> {
        let req = SeriesByIdRequest::builder().id(series_id).build();
        let series = self
            .gamma_client
            .series_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma series fetch failed: {}", e)))?;

        let open_events = series
            .events
            .unwrap_or_default()
            .into_iter()
            .map(Self::map_live_game_event)
            .filter(|e| !e.closed)
            .collect::<Vec<_>>();

        info!(
            "Found {} open events in series {}",
            open_events.len(),
            series_id
        );
        Ok(open_events)
    }

    /// Fetch NBA live game events
    pub async fn fetch_nba_live_games(&self) -> Result<Vec<LiveGameEvent>> {
        self.fetch_series_events(NBA_SERIES_ID).await
    }

    /// Filter games by date (format: "2026-01-03")
    pub async fn fetch_games_by_date(
        &self,
        series_id: &str,
        date: &str,
    ) -> Result<Vec<LiveGameEvent>> {
        let events = self.fetch_series_events(series_id).await?;

        let dated_events = events
            .into_iter()
            .filter(|e| e.slug.contains(date))
            .collect::<Vec<_>>();

        info!("Found {} games on {}", dated_events.len(), date);
        Ok(dated_events)
    }

    /// Fetch today's NBA games
    pub async fn fetch_todays_nba_games(&self) -> Result<Vec<LiveGameEvent>> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.fetch_games_by_date(NBA_SERIES_ID, &today).await
    }

    /// Get full event details with markets
    pub async fn get_event_details(&self, event_id: &str) -> Result<EventDetails> {
        let req = EventByIdRequest::builder().id(event_id).build();
        let event = self
            .gamma_client
            .event_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma event fetch failed: {}", e)))?;
        let event = Self::map_event_details(event);

        debug!("Event {} has {} markets", event.title, event.markets.len());
        Ok(event)
    }

    /// Find a live game by team names
    pub async fn find_live_game(&self, team1: &str, team2: &str) -> Result<Option<EventDetails>> {
        let team1_lower = team1.to_lowercase();
        let team2_lower = team2.to_lowercase();

        let events = self.fetch_nba_live_games().await?;

        for event in events {
            let title_lower = event.title.to_lowercase();
            if title_lower.contains(&team1_lower) && title_lower.contains(&team2_lower) {
                info!("Found live game: {}", event.title);
                return self.get_event_details(&event.id).await.map(Some);
            }

            let slug_lower = event.slug.to_lowercase();
            if slug_lower.contains(&team1_lower) || slug_lower.contains(&team2_lower) {
                let details = self.get_event_details(&event.id).await?;
                let detail_title = details.title.to_lowercase();
                if detail_title.contains(&team1_lower) || detail_title.contains(&team2_lower) {
                    info!("Found live game via slug: {}", details.title);
                    return Ok(Some(details));
                }
            }
        }

        warn!("No live game found for {} vs {}", team1, team2);
        Ok(None)
    }

    /// Fetch currently live games (in-play)
    pub async fn fetch_live_games(&self, series_id: &str) -> Result<Vec<EventDetails>> {
        let events = self.fetch_series_events(series_id).await?;
        let mut live_games = Vec::new();

        for event in events {
            let details = self.get_event_details(&event.id).await?;
            if details.live && !details.ended {
                live_games.push(details);
            }
        }

        info!("Found {} live games", live_games.len());
        Ok(live_games)
    }

    /// Fetch live NBA games
    pub async fn fetch_nba_live_in_play(&self) -> Result<Vec<EventDetails>> {
        self.fetch_live_games(NBA_SERIES_ID).await
    }

    /// Fetch all today's games with full details
    pub async fn fetch_todays_games_with_details(
        &self,
        series_id: &str,
    ) -> Result<Vec<EventDetails>> {
        let now = chrono::Utc::now();
        let today = now.date_naive();
        let window_start = now - chrono::Duration::hours(18);
        let window_end = now + chrono::Duration::hours(36);

        let events = self.fetch_series_events(series_id).await?;
        let mut games = Vec::new();

        for event in events {
            if let Some(slug_date) = parse_trailing_slug_date(&event.slug) {
                if slug_date < today - chrono::Duration::days(2)
                    || slug_date > today + chrono::Duration::days(2)
                {
                    continue;
                }
            }

            let details = match self.get_event_details(&event.id).await {
                Ok(v) => v,
                Err(e) => {
                    debug!(event_id = %event.id, error = %e, "failed to fetch PM event details");
                    continue;
                }
            };

            let start_ts = details
                .start_time
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));

            let include = if details.live && !details.ended {
                true
            } else if let Some(start_ts) = start_ts {
                start_ts >= window_start && start_ts <= window_end
            } else {
                details
                    .event_date
                    .as_deref()
                    .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                    .map(|d| d == today || d == (today - chrono::Duration::days(1)))
                    .unwrap_or(false)
            };

            if include {
                games.push(details);
            }
        }

        games.sort_by_key(|g| {
            g.start_time
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.timestamp())
                .unwrap_or(i64::MAX)
        });

        info!("Found {} games for today/live", games.len());
        Ok(games)
    }
}
