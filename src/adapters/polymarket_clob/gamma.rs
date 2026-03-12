use super::{MarketResponse, MarketSummary, PolymarketClient, Result, GAMMA_API_URL};
use crate::error::PloyError;
use alloy::primitives::U256;
use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::types::request::{
    EventByIdRequest, MarketsRequest, SearchRequest, SeriesByIdRequest,
};
use polymarket_client_sdk::gamma::types::response::Event as SdkEvent;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tokio::time::{timeout, Duration};
use tracing::{debug, instrument};

const GAMMA_REQUEST_TIMEOUT_SECS: u64 = 15;

impl PolymarketClient {
    /// Get the raw Gamma market (SDK type) by CLOB token id.
    ///
    /// This is useful for official settlement/outcome checks without relying on
    /// undocumented endpoints.
    #[instrument(skip(self))]
    pub async fn get_gamma_market_by_token_id(
        &self,
        token_id: &str,
    ) -> Result<polymarket_client_sdk::gamma::types::response::Market> {
        let token_u256 = U256::from_str(token_id)
            .map_err(|e| PloyError::Internal(format!("Invalid token_id '{}': {}", token_id, e)))?;

        let req = MarketsRequest::builder()
            .clob_token_ids(vec![token_u256])
            .limit(1)
            .build();

        let markets = self
            .gamma_client
            .markets(&req);
        let markets = timeout(Duration::from_secs(GAMMA_REQUEST_TIMEOUT_SECS), markets)
            .await
            .map_err(|_| {
                PloyError::Internal(format!(
                    "Gamma markets request timed out after {}s",
                    GAMMA_REQUEST_TIMEOUT_SECS
                ))
            })?
            .map_err(|e| PloyError::Internal(format!("Failed to get market: {}", e)))?;

        markets.into_iter().next().ok_or_else(|| {
            PloyError::MarketDataUnavailable(format!("Market not found for token_id={}", token_id))
        })
    }

    /// Search for markets
    #[instrument(skip(self))]
    pub async fn search_markets(&self, query: &str) -> Result<Vec<MarketSummary>> {
        let req = SearchRequest::builder().q(query).build();

        let results = self
            .gamma_client
            .search(&req);
        let results = timeout(Duration::from_secs(GAMMA_REQUEST_TIMEOUT_SECS), results)
            .await
            .map_err(|_| {
                PloyError::Internal(format!(
                    "Gamma search timed out after {}s",
                    GAMMA_REQUEST_TIMEOUT_SECS
                ))
            })?
            .map_err(|e| PloyError::Internal(format!("Failed to search markets: {}", e)))?;

        let mut summaries = Vec::new();
        for event in results.events.unwrap_or_default() {
            if let Some(markets) = event.markets {
                for market in markets {
                    summaries.push(MarketSummary {
                        condition_id: market
                            .condition_id
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                        question: market.question,
                        slug: market.slug,
                        active: market.active.unwrap_or(true),
                        clob_token_ids: market.clob_token_ids.map(|ids| {
                            serde_json::to_string(
                                &ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                            )
                            .unwrap_or_default()
                        }),
                        outcome_prices: market.outcome_prices.map(|prices| {
                            serde_json::to_string(
                                &prices
                                    .iter()
                                    .map(|value| value.to_string())
                                    .collect::<Vec<_>>(),
                            )
                            .unwrap_or_default()
                        }),
                    });
                }
            }
        }

        Ok(summaries)
    }

    /// Get series by ID
    #[instrument(skip(self))]
    pub async fn get_series(&self, series_id: &str) -> Result<GammaSeriesResponse> {
        let req = SeriesByIdRequest::builder().id(series_id).build();

        let series = self
            .gamma_client
            .series_by_id(&req);
        let series = timeout(Duration::from_secs(GAMMA_REQUEST_TIMEOUT_SECS), series)
            .await
            .map_err(|_| {
                PloyError::Internal(format!(
                    "Gamma series fetch timed out after {}s",
                    GAMMA_REQUEST_TIMEOUT_SECS
                ))
            })?
            .map_err(|e| PloyError::Internal(format!("Failed to get series: {}", e)))?;

        Ok(GammaSeriesResponse {
            id: series.id,
            ticker: series.ticker,
            slug: series.slug,
            title: series.title,
            recurrence: series.recurrence,
            events: vec![],
            volume: series
                .volume
                .map(|value| value.to_string().parse().unwrap_or(0.0)),
            liquidity: series
                .liquidity
                .map(|value| value.to_string().parse().unwrap_or(0.0)),
        })
    }

    /// Get current (active, not closed) event from a series
    #[instrument(skip(self))]
    pub async fn get_current_event(&self, series_id: &str) -> Result<Option<GammaEventInfo>> {
        let events = self.get_all_active_events(series_id).await?;
        let now = Utc::now();

        let mut best: Option<(DateTime<Utc>, GammaEventInfo)> = None;
        for event in events {
            let Some(end_str) = &event.end_date else {
                continue;
            };
            let Ok(end) = DateTime::parse_from_rfc3339(end_str).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if end <= now {
                continue;
            }

            match best.as_ref() {
                None => best = Some((end, event)),
                Some((best_end, _)) if end < *best_end => best = Some((end, event)),
                _ => {}
            }
        }

        Ok(best.map(|(_, event)| event))
    }

    /// Get event details by ID
    #[instrument(skip(self))]
    pub async fn get_event_details(&self, event_id: &str) -> Result<GammaEventInfo> {
        let req = EventByIdRequest::builder().id(event_id).build();

        let event = self
            .gamma_client
            .event_by_id(&req);
        let event = timeout(Duration::from_secs(GAMMA_REQUEST_TIMEOUT_SECS), event)
            .await
            .map_err(|_| {
                PloyError::Internal(format!(
                    "Gamma event fetch timed out after {}s",
                    GAMMA_REQUEST_TIMEOUT_SECS
                ))
            })?
            .map_err(|e| PloyError::Internal(format!("Failed to get event: {}", e)))?;

        Ok(self.convert_sdk_event(&event))
    }

    /// Get current market tokens from a series
    #[instrument(skip(self))]
    pub async fn get_current_market_tokens(
        &self,
        series_id: &str,
    ) -> Result<Option<(String, MarketResponse)>> {
        let Some(event) = self.get_current_event(series_id).await? else {
            return Ok(None);
        };

        let details = self.get_event_details(&event.id).await?;
        let market = match details.markets.first() {
            Some(market) => market,
            None => return Ok(None),
        };

        let Some(condition_id) = &market.condition_id else {
            return Ok(None);
        };

        let market_resp = self.get_market(condition_id).await?;
        Ok(Some((details.id, market_resp)))
    }

    /// Get all active events from a series
    #[instrument(skip(self))]
    pub async fn get_all_active_events(&self, series_id: &str) -> Result<Vec<GammaEventInfo>> {
        let req = SeriesByIdRequest::builder().id(series_id).build();
        let series = self
            .gamma_client
            .series_by_id(&req);
        let series = timeout(Duration::from_secs(GAMMA_REQUEST_TIMEOUT_SECS), series)
            .await
            .map_err(|_| {
                PloyError::Internal(format!(
                    "Gamma active-events fetch timed out after {}s for series {}",
                    GAMMA_REQUEST_TIMEOUT_SECS, series_id
                ))
            })?
            .map_err(|e| PloyError::Internal(format!("Failed to fetch series: {}", e)))?;

        let active_events: Vec<GammaEventInfo> = series
            .events
            .unwrap_or_default()
            .into_iter()
            .filter(|event| !event.closed.unwrap_or(false))
            .map(|event| self.convert_sdk_event(&event))
            .collect();

        debug!(
            "Found {} active events in series {}",
            active_events.len(),
            series_id
        );
        Ok(active_events)
    }

    /// Get all events from a series (includes closed events with full market data).
    ///
    /// Uses the Gamma events API directly (not SDK series_by_id) because the
    /// series endpoint omits nested market data (outcomes, prices, token IDs).
    #[instrument(skip(self))]
    pub async fn get_all_events_in_series(&self, series_id: &str) -> Result<Vec<GammaEventInfo>> {
        let client = reqwest::Client::new();
        let mut all_events: Vec<GammaEventInfo> = Vec::new();
        let page_size = 200;
        let mut offset = 0;

        loop {
            let url = format!(
                "{}/events?series_id={}&closed=true&limit={}&offset={}&order=endDate&ascending=false",
                GAMMA_API_URL, series_id, page_size, offset
            );

            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| PloyError::Internal(format!("Gamma events API error: {e}")))?;

            if !resp.status().is_success() {
                return Err(PloyError::Internal(format!(
                    "Gamma events API returned {}",
                    resp.status()
                )));
            }

            let page: Vec<GammaEventInfo> = resp
                .json()
                .await
                .map_err(|e| PloyError::Internal(format!("Gamma events parse error: {e}")))?;

            let page_len = page.len();
            all_events.extend(page);

            if page_len < page_size {
                break;
            }
            offset += page_size;
        }

        debug!(
            "Found {} closed events in series {}",
            all_events.len(),
            series_id
        );
        Ok(all_events)
    }

    /// Get active sports events matching a keyword
    #[instrument(skip(self))]
    pub async fn get_active_sports_events(&self, keyword: &str) -> Result<Vec<GammaEventInfo>> {
        let req = SearchRequest::builder().q(keyword).build();

        let results = self
            .gamma_client
            .search(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Failed to search: {}", e)))?;

        Ok(results
            .events
            .unwrap_or_default()
            .into_iter()
            .filter(|event| !event.closed.unwrap_or(false))
            .map(|event| self.convert_sdk_event(&event))
            .collect())
    }

    /// Get all tokens from all active events in a series
    /// Returns (event, up_token_id, down_token_id) for each event
    #[instrument(skip(self))]
    pub async fn get_series_all_tokens(
        &self,
        series_id: &str,
    ) -> Result<Vec<(GammaEventInfo, String, String)>> {
        let events = self.get_all_active_events(series_id).await?;
        let mut result = Vec::new();

        for event in events {
            for market in &event.markets {
                if let Some(clob_ids) = &market.clob_token_ids {
                    if let Ok(ids) = serde_json::from_str::<Vec<String>>(clob_ids) {
                        if ids.len() >= 2 {
                            result.push((event.clone(), ids[0].clone(), ids[1].clone()));
                            break;
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    /// Convert SDK Event to our GammaEventInfo
    pub(super) fn convert_sdk_event(&self, event: &SdkEvent) -> GammaEventInfo {
        GammaEventInfo {
            id: event.id.clone(),
            slug: event.slug.clone(),
            title: event.title.clone(),
            start_time: event
                .start_time
                .or(event.start_date)
                .map(|value| value.to_rfc3339()),
            end_date: event.end_date.map(|value| value.to_rfc3339()),
            closed: event.closed.unwrap_or(false),
            markets: event
                .markets
                .as_ref()
                .map(|markets| {
                    markets
                        .iter()
                        .map(|market| GammaMarketInfo {
                            condition_id: market.condition_id.map(|value| value.to_string()),
                            question: market.question.clone(),
                            tokens: None,
                            group_item_title: market.group_item_title.clone(),
                            outcomes: market.outcomes.as_ref().map(|outcomes| {
                                serde_json::to_string(outcomes).unwrap_or_default()
                            }),
                            clob_token_ids: market.clob_token_ids.as_ref().map(|ids| {
                                serde_json::to_string(
                                    &ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                                )
                                .unwrap_or_default()
                            }),
                            outcome_prices: market.outcome_prices.as_ref().map(|prices| {
                                serde_json::to_string(
                                    &prices
                                        .iter()
                                        .map(|value| value.to_string())
                                        .collect::<Vec<_>>(),
                                )
                                .unwrap_or_default()
                            }),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GammaSeriesResponse {
    pub id: String,
    pub ticker: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
    pub recurrence: Option<String>,
    #[serde(default)]
    pub events: Vec<GammaEventInfo>,
    pub volume: Option<f64>,
    pub liquidity: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GammaEventInfo {
    pub id: String,
    pub slug: Option<String>,
    pub title: Option<String>,
    #[serde(rename = "startTime")]
    pub start_time: Option<String>,
    #[serde(rename = "endDate")]
    pub end_date: Option<String>,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub markets: Vec<GammaMarketInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GammaMarketInfo {
    #[serde(rename = "conditionId")]
    pub condition_id: Option<String>,
    pub question: Option<String>,
    #[serde(default)]
    pub tokens: Option<Vec<GammaTokenInfo>>,
    #[serde(rename = "groupItemTitle")]
    pub group_item_title: Option<String>,
    #[serde(default)]
    pub outcomes: Option<String>,
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>,
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GammaTokenInfo {
    pub token_id: String,
    pub outcome: String,
}
