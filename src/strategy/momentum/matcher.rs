use super::*;

/// Maps CEX symbols to Polymarket event series
pub struct EventMatcher {
    client: PolymarketClient,
    /// Map symbol to series ID
    pub(super) symbol_to_series: HashMap<String, Vec<String>>,
    /// Cache of active events per series
    pub(super) active_events: Arc<RwLock<HashMap<String, Vec<EventInfo>>>>,
}

/// Event information for trading
#[derive(Debug, Clone)]
pub struct EventInfo {
    pub slug: String,
    pub title: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub condition_id: String,
    pub series_id: String,
    pub horizon: String,
    pub price_to_beat: Option<Decimal>,
}

impl EventInfo {
    pub fn time_remaining(&self) -> ChronoDuration {
        self.end_time - Utc::now()
    }

    pub fn seconds_since_start(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.start_time)
            .num_seconds()
            .max(0)
    }

    pub fn is_tradeable(&self, min_seconds: i64) -> bool {
        self.time_remaining().num_seconds() > min_seconds
    }

    pub fn parse_price_from_question(question: &str) -> Option<Decimal> {
        let cleaned: String = question
            .chars()
            .skip_while(|c| !c.is_ascii_digit() && *c != '$')
            .skip_while(|c| *c == '$')
            .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
            .filter(|c| *c != ',')
            .collect();

        if cleaned.is_empty() {
            return None;
        }

        Decimal::from_str(&cleaned).ok()
    }
}

impl EventMatcher {
    pub fn new(client: PolymarketClient) -> Self {
        let mut symbol_to_series = HashMap::new();

        for symbol in known_binance_symbols() {
            let series_ids = crypto_series_ids_for_symbol(symbol);
            if !series_ids.is_empty() {
                symbol_to_series.insert((*symbol).to_string(), series_ids);
            }
        }

        Self {
            client,
            symbol_to_series,
            active_events: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn horizon_for_series(series_id: &str) -> &'static str {
        crypto_horizon_for_series(series_id)
    }

    fn window_secs_for_horizon(horizon: &str) -> i64 {
        match horizon {
            "15m" => 15 * 60,
            "5m" => 5 * 60,
            _ => 5 * 60,
        }
    }

    pub async fn find_event(&self, symbol: &str) -> Option<EventInfo> {
        self.find_event_with_timing(symbol, 60, i64::MAX, false)
            .await
    }

    pub async fn find_event_with_timing(
        &self,
        symbol: &str,
        min_secs: u64,
        max_secs: i64,
        prefer_close_to_end: bool,
    ) -> Option<EventInfo> {
        let series_ids = self.symbol_to_series.get(symbol)?;
        let events = self.active_events.read().await;
        let mut best: Option<(i64, EventInfo)> = None;

        for series_id in series_ids {
            let Some(series_events) = events.get(series_id) else {
                continue;
            };

            for event in series_events {
                let remaining = event.time_remaining().num_seconds();
                if remaining < min_secs as i64 || remaining > max_secs {
                    continue;
                }

                let is_better = match best.as_ref() {
                    None => true,
                    Some((best_remaining, _)) => {
                        if prefer_close_to_end {
                            remaining < *best_remaining
                        } else {
                            remaining > *best_remaining
                        }
                    }
                };

                if is_better {
                    best = Some((remaining, event.clone()));
                }
            }
        }

        best.map(|(_, event)| event)
    }

    pub async fn get_events(&self, symbol: &str) -> Vec<EventInfo> {
        self.get_events_with_min_remaining(symbol, 60).await
    }

    pub async fn get_events_with_min_remaining(
        &self,
        symbol: &str,
        min_remaining_secs: i64,
    ) -> Vec<EventInfo> {
        let series_ids = match self.symbol_to_series.get(symbol) {
            Some(ids) => ids,
            None => return vec![],
        };

        let events = self.active_events.read().await;
        let mut result = vec![];

        for series_id in series_ids {
            if let Some(series_events) = events.get(series_id) {
                for event in series_events {
                    if event.time_remaining().num_seconds() > min_remaining_secs {
                        result.push(event.clone());
                    }
                }
            }
        }

        result
    }

    pub async fn refresh(&self) -> Result<()> {
        let mut series_ids: Vec<String> =
            self.symbol_to_series.values().flatten().cloned().collect();
        series_ids.sort();
        series_ids.dedup();

        let mut updates: Vec<(String, Vec<EventInfo>)> = Vec::with_capacity(series_ids.len());
        for series_id in series_ids {
            match self.fetch_series_events(&series_id).await {
                Ok(series_events) => updates.push((series_id, series_events)),
                Err(e) => warn!("Failed to fetch events for {}: {}", series_id, e),
            }
        }

        if updates.is_empty() {
            return Ok(());
        }

        let mut events = self.active_events.write().await;
        for (series_id, series_events) in updates {
            events.insert(series_id, series_events);
        }

        Ok(())
    }

    async fn fetch_series_events(&self, series_id: &str) -> Result<Vec<EventInfo>> {
        let gamma_events = self.client.get_all_active_events(series_id).await?;
        let now = Utc::now();
        let max_end_time = now + ChronoDuration::minutes(60);
        let min_end_time = now + ChronoDuration::seconds(30);

        let mut sorted_events: Vec<_> = gamma_events
            .into_iter()
            .filter(|e| {
                if let Some(end_str) = &e.end_date {
                    if let Ok(end) = DateTime::parse_from_rfc3339(end_str) {
                        let end_utc = end.with_timezone(&Utc);
                        return end_utc > min_end_time && end_utc <= max_end_time;
                    }
                }
                false
            })
            .collect();

        sorted_events.sort_by(|a, b| {
            let a_end = a
                .end_date
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
            let b_end = b
                .end_date
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
            a_end.cmp(&b_end)
        });

        info!(
            "Series {}: {} events ending in next 60 minutes",
            series_id,
            sorted_events.len()
        );

        let mut events = vec![];
        for gamma_event in sorted_events.into_iter().take(5) {
            let event_details = match self.client.get_event_details(&gamma_event.id).await {
                Ok(details) => details,
                Err(e) => {
                    debug!("Failed to get details for event {}: {}", gamma_event.id, e);
                    continue;
                }
            };

            let market = match event_details.markets.first() {
                Some(m) => m,
                None => continue,
            };
            let condition_id = market.condition_id.clone().unwrap_or_default();

            let is_up_label = |label: &str| -> bool {
                let o = label.trim().to_ascii_lowercase();
                o == "up" || o.contains("up") || o == "yes" || o.starts_with('↑')
            };
            let is_down_label = |label: &str| -> bool {
                let o = label.trim().to_ascii_lowercase();
                o == "down" || o.contains("down") || o == "no" || o.starts_with('↓')
            };

            let mut up_token_id: Option<String> = None;
            let mut down_token_id: Option<String> = None;
            if let Some(token_infos) = market.tokens.as_ref() {
                for t in token_infos {
                    if up_token_id.is_none() && is_up_label(&t.outcome) {
                        up_token_id = Some(t.token_id.clone());
                    }
                    if down_token_id.is_none() && is_down_label(&t.outcome) {
                        down_token_id = Some(t.token_id.clone());
                    }
                }
            }

            let mut token_ids: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .and_then(|ids_str| serde_json::from_str::<Vec<String>>(ids_str).ok())
                .unwrap_or_default();

            if token_ids.len() < 2 {
                if let Some(token_infos) = market.tokens.as_ref() {
                    token_ids = token_infos.iter().map(|t| t.token_id.clone()).collect();
                }
            }

            if token_ids.len() < 2 {
                debug!(
                    "Market {} has insufficient tokens (clobTokenIds/tokens missing)",
                    condition_id
                );
                continue;
            }

            if up_token_id.is_none() || down_token_id.is_none() {
                if let Some(outcomes_raw) = market.outcomes.as_ref() {
                    if let Ok(outcomes) = serde_json::from_str::<Vec<String>>(outcomes_raw) {
                        if outcomes.len() == token_ids.len() {
                            let mut up_idx: Option<usize> = None;
                            let mut down_idx: Option<usize> = None;
                            for (idx, outcome) in outcomes.iter().enumerate() {
                                if up_idx.is_none() && is_up_label(outcome) {
                                    up_idx = Some(idx);
                                }
                                if down_idx.is_none() && is_down_label(outcome) {
                                    down_idx = Some(idx);
                                }
                            }

                            if let Some(u) = up_idx {
                                up_token_id = Some(token_ids[u].clone());
                            }
                            if let Some(d) = down_idx {
                                down_token_id = Some(token_ids[d].clone());
                            }
                        }
                    }
                }
            }

            let up_token_id = up_token_id.unwrap_or_else(|| token_ids[0].clone());
            let down_token_id = down_token_id.unwrap_or_else(|| token_ids[1].clone());

            let end_time = match event_details.end_date.as_ref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .ok()
            }) {
                Some(t) => t,
                None => continue,
            };

            let horizon = Self::horizon_for_series(series_id).to_string();
            let window_secs = Self::window_secs_for_horizon(&horizon);
            let start_time = event_details
                .start_time
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|| end_time - ChronoDuration::seconds(window_secs));

            let price_to_beat = EventInfo::parse_price_from_question(
                &event_details.title.clone().unwrap_or_default(),
            );

            let event_info = EventInfo {
                slug: event_details.slug.clone().unwrap_or_default(),
                title: event_details.title.clone().unwrap_or_default(),
                up_token_id,
                down_token_id,
                start_time,
                end_time,
                condition_id,
                series_id: series_id.to_string(),
                horizon,
                price_to_beat,
            };

            debug!(
                "Found event: {} (UP={}, DOWN={})",
                event_info.title,
                &event_info.up_token_id[..20.min(event_info.up_token_id.len())],
                &event_info.down_token_id[..20.min(event_info.down_token_id.len())]
            );

            events.push(event_info);
        }

        Ok(events)
    }

    #[allow(dead_code)]
    fn convert_gamma_event(&self, gamma: &GammaEventInfo) -> Option<EventInfo> {
        debug!(
            "Converting event: id={}, markets={}",
            gamma.id,
            gamma.markets.len()
        );

        let market = gamma.markets.first()?;
        let tokens = match market.tokens.as_ref() {
            Some(t) => t,
            None => {
                debug!("Event {} has no tokens", gamma.id);
                return None;
            }
        };

        debug!("Event {} has {} tokens", gamma.id, tokens.len());
        if tokens.len() < 2 {
            return None;
        }

        for t in tokens {
            debug!("  Token: {} = {}", t.token_id, t.outcome);
        }

        let up_token = tokens.iter().find(|t| {
            let outcome = t.outcome.to_lowercase();
            outcome.contains("up") || outcome == "yes" || outcome.starts_with("↑")
        })?;
        let down_token = tokens.iter().find(|t| {
            let outcome = t.outcome.to_lowercase();
            outcome.contains("down") || outcome == "no" || outcome.starts_with("↓")
        })?;

        let end_time = gamma.end_date.as_ref().and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .ok()
        })?;

        let start_time = gamma
            .start_time
            .as_ref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| {
                end_time - ChronoDuration::seconds(Self::window_secs_for_horizon("other"))
            });

        let title = gamma.title.clone().unwrap_or_default();
        let price_to_beat = EventInfo::parse_price_from_question(&title);

        Some(EventInfo {
            slug: gamma.slug.clone().unwrap_or_default(),
            title,
            up_token_id: up_token.token_id.clone(),
            down_token_id: down_token.token_id.clone(),
            start_time,
            end_time,
            condition_id: market.condition_id.clone().unwrap_or_default(),
            series_id: String::new(),
            horizon: "other".to_string(),
            price_to_beat,
        })
    }

    pub async fn get_all_token_ids(&self) -> Vec<String> {
        let events = self.active_events.read().await;
        let mut token_ids = vec![];

        for series_events in events.values() {
            for event in series_events {
                token_ids.push(event.up_token_id.clone());
                token_ids.push(event.down_token_id.clone());
            }
        }

        token_ids
    }

    pub async fn get_token_mappings(&self) -> Vec<(String, Side)> {
        let events = self.active_events.read().await;
        let mut mappings = vec![];

        for series_events in events.values() {
            for event in series_events {
                mappings.push((event.up_token_id.clone(), Side::Up));
                mappings.push((event.down_token_id.clone(), Side::Down));
            }
        }

        mappings
    }

    pub fn client(&self) -> &PolymarketClient {
        &self.client
    }
}
