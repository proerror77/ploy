use super::{
    PoliticalEvent, PoliticalEventDetails, PolymarketPoliticsClient, TRUMP_APPROVAL_SERIES,
    TRUMP_FAVORABILITY_SERIES,
};
use crate::error::{PloyError, Result};
use polymarket_client_sdk::gamma::types::request::{EventByIdRequest, SeriesByIdRequest};
use tracing::{debug, info};

impl PolymarketPoliticsClient {
    pub async fn fetch_series_events(&self, series_id: &str) -> Result<Vec<PoliticalEvent>> {
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
            .map(Self::map_political_event)
            .filter(|event| !event.closed)
            .collect::<Vec<_>>();

        info!(
            "Found {} open events in series {}",
            open_events.len(),
            series_id
        );
        Ok(open_events)
    }

    pub async fn fetch_trump_favorability_events(&self) -> Result<Vec<PoliticalEvent>> {
        self.fetch_series_events(TRUMP_FAVORABILITY_SERIES).await
    }

    pub async fn fetch_trump_approval_events(&self) -> Result<Vec<PoliticalEvent>> {
        self.fetch_series_events(TRUMP_APPROVAL_SERIES).await
    }

    pub async fn get_event_details(&self, event_id: &str) -> Result<PoliticalEventDetails> {
        let req = EventByIdRequest::builder().id(event_id).build();
        let event = self
            .gamma_client
            .event_by_id(&req)
            .await
            .map_err(|e| PloyError::Internal(format!("Gamma event fetch failed: {}", e)))?;
        let event = Self::map_political_event_details(event);

        debug!("Event {} has {} markets", event.title, event.markets.len());
        Ok(event)
    }
}
