use anyhow::anyhow;
use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::collections::HashMap;

use crate::adapters::PolymarketClient;
use crate::domain::Domain;
use crate::error::{PloyError, Result};
use crate::strategy::traits::{
    AlertLevel, DataFeed, MarketUpdate, OrderUpdate, PositionInfo, Strategy, StrategyAction,
    StrategyEvent, StrategyEventType, StrategyStateInfo,
};

const STRATEGY_NAME: &str = "weather_market";
const DEFAULT_PM_REST_URL: &str = "https://clob.polymarket.com";
const NWS_BASE_URL: &str = "https://api.weather.gov";
const OPEN_METEO_BASE_URL: &str = "https://api.open-meteo.com/v1/forecast";

#[derive(Debug, Clone, Deserialize)]
struct StrategySection {
    name: String,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TemperatureUnit {
    Fahrenheit,
    Celsius,
}

impl Default for TemperatureUnit {
    fn default() -> Self {
        Self::Fahrenheit
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SettlementRounding {
    None,
    NearestInteger,
    FloorInteger,
    CeilInteger,
    Tenth,
}

impl Default for SettlementRounding {
    fn default() -> Self {
        Self::NearestInteger
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct WeatherBucketConfig {
    label: String,
    token_id: String,
    market_slug: Option<String>,
    min_temp: Option<f64>,
    max_temp: Option<f64>,
}

impl Default for WeatherBucketConfig {
    fn default() -> Self {
        Self {
            label: String::new(),
            token_id: String::new(),
            market_slug: None,
            min_temp: None,
            max_temp: None,
        }
    }
}

impl WeatherBucketConfig {
    fn contains(&self, value: f64) -> bool {
        let lower_ok = self.min_temp.map(|min| value >= min).unwrap_or(true);
        let upper_ok = self.max_temp.map(|max| value < max).unwrap_or(true);
        lower_ok && upper_ok
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct WeatherMarketConfig {
    station_id: String,
    station_name: String,
    contract_date: String,
    domain: Option<String>,
    latitude: f64,
    longitude: f64,
    station_utc_offset_hours: i32,
    settlement_unit: TemperatureUnit,
    settlement_rounding: SettlementRounding,
    tick_interval_ms: u64,
    evaluation_cooldown_secs: u64,
    observation_window_hours: u64,
    peak_window_start_hour: u32,
    peak_window_end_hour: u32,
    late_peak_hour: u32,
    regime_neutral_band: f64,
    recommendation_min_edge: f64,
    recommendation_min_confidence: f64,
    sigma_floor: f64,
    sigma_spread_multiplier: f64,
    open_meteo_weight: f64,
    nws_weight: f64,
    use_open_meteo: bool,
    use_nws_hourly: bool,
    use_nws_observations: bool,
    previous_day_max_temp: Option<f64>,
    nws_station_id: Option<String>,
    nws_grid_office: Option<String>,
    nws_grid_x: Option<u32>,
    nws_grid_y: Option<u32>,
    alert_cooldown_secs: u64,
    market_slug: Option<String>,
    observe_only: bool,
    emit_all_bucket_views: bool,
    buckets: Vec<WeatherBucketConfig>,
}

impl Default for WeatherMarketConfig {
    fn default() -> Self {
        Self {
            station_id: "KJFK".to_string(),
            station_name: "John F. Kennedy International Airport".to_string(),
            contract_date: Utc::now().date_naive().to_string(),
            domain: Some("economics".to_string()),
            latitude: 40.6413,
            longitude: -73.7781,
            station_utc_offset_hours: -4,
            settlement_unit: TemperatureUnit::Fahrenheit,
            settlement_rounding: SettlementRounding::NearestInteger,
            tick_interval_ms: 300_000,
            evaluation_cooldown_secs: 300,
            observation_window_hours: 18,
            peak_window_start_hour: 13,
            peak_window_end_hour: 17,
            late_peak_hour: 18,
            regime_neutral_band: 1.0,
            recommendation_min_edge: 0.05,
            recommendation_min_confidence: 0.55,
            sigma_floor: 1.2,
            sigma_spread_multiplier: 0.85,
            open_meteo_weight: 0.55,
            nws_weight: 0.45,
            use_open_meteo: true,
            use_nws_hourly: true,
            use_nws_observations: true,
            previous_day_max_temp: None,
            nws_station_id: Some("KJFK".to_string()),
            nws_grid_office: Some("OKX".to_string()),
            nws_grid_x: Some(33),
            nws_grid_y: Some(35),
            alert_cooldown_secs: 900,
            market_slug: None,
            observe_only: true,
            emit_all_bucket_views: true,
            buckets: Vec::new(),
        }
    }
}

impl WeatherMarketConfig {
    fn validate(&self) -> Result<()> {
        if self.station_id.trim().is_empty() {
            return Err(PloyError::Validation("weather_market.station_id is required".into()));
        }
        if self.contract_date.trim().is_empty() {
            return Err(PloyError::Validation(
                "weather_market.contract_date is required".into(),
            ));
        }
        NaiveDate::parse_from_str(&self.contract_date, "%Y-%m-%d").map_err(|err| {
            PloyError::Validation(format!(
                "weather_market.contract_date must be YYYY-MM-DD: {err}"
            ))
        })?;
        if self.buckets.is_empty() {
            return Err(PloyError::Validation(
                "weather_market requires at least one [[weather_market.buckets]] entry".into(),
            ));
        }
        for bucket in &self.buckets {
            if bucket.label.trim().is_empty() || bucket.token_id.trim().is_empty() {
                return Err(PloyError::Validation(
                    "weather_market buckets require label and token_id".into(),
                ));
            }
            if let (Some(min), Some(max)) = (bucket.min_temp, bucket.max_temp) {
                if min >= max {
                    return Err(PloyError::Validation(format!(
                        "bucket {} has invalid min/max bounds",
                        bucket.label
                    )));
                }
            }
        }
        Ok(())
    }

    fn contract_date(&self) -> Result<NaiveDate> {
        NaiveDate::parse_from_str(&self.contract_date, "%Y-%m-%d")
            .map_err(|e| anyhow!("invalid weather_market.contract_date: {e}").into())
    }

    fn domain(&self) -> Domain {
        Domain::parse_optional(self.domain.as_deref(), Domain::Economics)
            .unwrap_or(Domain::Economics)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct WeatherMarketToml {
    strategy: StrategySection,
    #[serde(default)]
    weather_market: WeatherMarketConfig,
}

#[derive(Debug, Clone)]
struct SourceEstimate {
    name: String,
    normalized_max_temp: f64,
    expected_peak_hour_local: Option<f64>,
    confidence: f64,
}

#[derive(Debug, Clone, Default)]
struct ObservationSnapshot {
    current_temp: Option<f64>,
    observed_max_temp: Option<f64>,
    observed_peak_hour_local: Option<f64>,
}

#[derive(Debug, Clone)]
struct BucketQuote {
    token_id: String,
    market_slug: String,
    best_bid: Option<Decimal>,
    best_ask: Option<Decimal>,
    mid: Option<Decimal>,
}

#[derive(Debug, Clone)]
struct BucketView {
    label: String,
    token_id: String,
    market_slug: String,
    model_probability: f64,
    target_price: f64,
    market_reference_price: Option<f64>,
    edge: Option<f64>,
}

#[derive(Debug, Clone)]
struct EntrySuggestion {
    label: String,
    token_id: String,
    market_slug: String,
    model_probability: f64,
    market_reference_price: f64,
    edge: f64,
    confidence: f64,
    rationale: String,
}

#[derive(Debug, Clone)]
struct WeatherSnapshot {
    station_id: String,
    contract_date: NaiveDate,
    base_max_temp: f64,
    corrected_max_temp: f64,
    sigma: f64,
    observed_max_temp: Option<f64>,
    current_temp: Option<f64>,
    regime: String,
    regime_confidence: f64,
    peak_anomaly: String,
    peak_anomaly_confidence: f64,
    expected_peak_hour_local: Option<f64>,
    previous_day_max_temp: Option<f64>,
    bucket_views: Vec<BucketView>,
    recommendations: Vec<EntrySuggestion>,
    source_estimates: Vec<SourceEstimate>,
}

pub struct WeatherMarketStrategy {
    id: String,
    dry_run: bool,
    enabled: bool,
    cfg: WeatherMarketConfig,
    pm_client: PolymarketClient,
    http: reqwest::Client,
    last_evaluated_at: Option<DateTime<Utc>>,
    last_snapshot: Option<WeatherSnapshot>,
    last_error: Option<String>,
    last_alert_at: Option<DateTime<Utc>>,
}

impl WeatherMarketStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        let parsed: WeatherMarketToml =
            toml::from_str(config_str).map_err(|e| anyhow!("Invalid TOML: {e}"))?;
        if parsed.strategy.name != STRATEGY_NAME {
            return Err(anyhow!(
                "strategy.name must be \"{}\", got \"{}\"",
                STRATEGY_NAME,
                parsed.strategy.name
            )
            .into());
        }

        let cfg = WeatherMarketConfig {
            observe_only: true,
            ..parsed.weather_market
        };
        cfg.validate()?;

        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/geo+json, application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("ploy-weather-market/0.1 (+https://github.com/proerror77/ploy)"),
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| anyhow!("failed to build weather_market http client: {e}"))?;
        let pm_client = PolymarketClient::new(DEFAULT_PM_REST_URL, true)?;

        Ok(Self {
            id,
            dry_run,
            enabled: parsed.strategy.enabled.unwrap_or(true),
            cfg,
            pm_client,
            http,
            last_evaluated_at: None,
            last_snapshot: None,
            last_error: None,
            last_alert_at: None,
        })
    }

    fn should_evaluate(&self, now: DateTime<Utc>) -> bool {
        match self.last_evaluated_at {
            None => true,
            Some(last) => {
                now.signed_duration_since(last).num_seconds()
                    >= self.cfg.evaluation_cooldown_secs as i64
            }
        }
    }

    async fn evaluate(&self, now: DateTime<Utc>) -> Result<WeatherSnapshot> {
        let contract_date = self.cfg.contract_date()?;
        let open_meteo = if self.cfg.use_open_meteo {
            Some(self.fetch_open_meteo(contract_date).await?)
        } else {
            None
        };
        let observation = if self.cfg.use_nws_observations {
            self.fetch_nws_observations(contract_date).await.unwrap_or_default()
        } else {
            ObservationSnapshot::default()
        };
        let nws_hourly = if self.cfg.use_nws_hourly {
            self.fetch_nws_hourly_forecast(contract_date).await.ok()
        } else {
            None
        };

        let mut source_estimates = Vec::new();
        if let Some(open_meteo) = open_meteo.as_ref() {
            source_estimates.push(SourceEstimate {
                name: "open_meteo".to_string(),
                normalized_max_temp: open_meteo.daily_max_temp,
                expected_peak_hour_local: open_meteo.hourly_peak_hour_local,
                confidence: open_meteo.confidence,
            });
        }
        if let Some(nws_hourly) = nws_hourly.as_ref() {
            source_estimates.push(SourceEstimate {
                name: "nws_hourly".to_string(),
                normalized_max_temp: nws_hourly.forecast_max_temp,
                expected_peak_hour_local: nws_hourly.peak_hour_local,
                confidence: nws_hourly.confidence,
            });
        }
        if source_estimates.is_empty() {
            return Err(PloyError::MarketDataUnavailable(
                "weather_market did not produce any forecast source estimates".into(),
            ));
        }

        let base_max_temp = fuse_forecast_estimates(
            &source_estimates,
            self.cfg.open_meteo_weight,
            self.cfg.nws_weight,
        );
        let previous_day_max = self
            .cfg
            .previous_day_max_temp
            .or_else(|| open_meteo.as_ref().and_then(|snapshot| snapshot.previous_day_max_temp));
        let expected_peak_hour_local = expected_peak_hour(
            &source_estimates,
            observation.observed_peak_hour_local,
        );
        let corrected_max_temp = intraday_corrected_max(
            base_max_temp,
            observation.current_temp,
            observation.observed_max_temp,
            local_hour(now, self.cfg.station_utc_offset_hours),
            self.cfg.peak_window_start_hour,
            self.cfg.peak_window_end_hour,
        );
        let sigma = estimate_sigma(
            &source_estimates,
            corrected_max_temp,
            observation.observed_max_temp,
            self.cfg.sigma_floor,
            self.cfg.sigma_spread_multiplier,
        );
        let (regime, regime_confidence) = classify_regime(
            corrected_max_temp,
            previous_day_max,
            self.cfg.regime_neutral_band,
            sigma,
        );
        let (peak_anomaly, peak_anomaly_confidence) = classify_peak_anomaly(
            expected_peak_hour_local,
            self.cfg.peak_window_start_hour,
            self.cfg.peak_window_end_hour,
            self.cfg.late_peak_hour,
        );

        let bucket_quotes = self.fetch_bucket_quotes().await?;
        let bucket_views = build_bucket_views(
            &self.cfg,
            &bucket_quotes,
            corrected_max_temp,
            sigma,
        );
        let overall_confidence = overall_confidence(
            &source_estimates,
            regime_confidence,
            peak_anomaly_confidence,
            observation.current_temp.is_some(),
        );
        let recommendations = build_recommendations(
            &bucket_views,
            overall_confidence,
            self.cfg.recommendation_min_edge,
            self.cfg.recommendation_min_confidence,
            &regime,
            &peak_anomaly,
        );

        Ok(WeatherSnapshot {
            station_id: self.cfg.station_id.clone(),
            contract_date,
            base_max_temp,
            corrected_max_temp,
            sigma,
            observed_max_temp: observation.observed_max_temp,
            current_temp: observation.current_temp,
            regime,
            regime_confidence,
            peak_anomaly,
            peak_anomaly_confidence,
            expected_peak_hour_local,
            previous_day_max_temp: previous_day_max,
            bucket_views,
            recommendations,
            source_estimates,
        })
    }

    async fn fetch_bucket_quotes(&self) -> Result<Vec<BucketQuote>> {
        let mut quotes = Vec::with_capacity(self.cfg.buckets.len());
        for bucket in &self.cfg.buckets {
            let (best_bid, best_ask) = self.pm_client.get_best_prices(&bucket.token_id).await?;
            let mid = match (best_bid, best_ask) {
                (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::from(2)),
                (Some(bid), None) => Some(bid),
                (None, Some(ask)) => Some(ask),
                _ => None,
            };
            quotes.push(BucketQuote {
                token_id: bucket.token_id.clone(),
                market_slug: bucket
                    .market_slug
                    .clone()
                    .or_else(|| self.cfg.market_slug.clone())
                    .unwrap_or_else(|| bucket.label.clone()),
                best_bid,
                best_ask,
                mid,
            });
        }
        Ok(quotes)
    }

    async fn fetch_open_meteo(&self, contract_date: NaiveDate) -> Result<OpenMeteoSnapshot> {
        let unit_param = match self.cfg.settlement_unit {
            TemperatureUnit::Fahrenheit => "fahrenheit",
            TemperatureUnit::Celsius => "celsius",
        };
        let url = format!(
            "{OPEN_METEO_BASE_URL}?latitude={}&longitude={}&daily=temperature_2m_max&hourly=temperature_2m,cloud_cover,wind_speed_10m,precipitation_probability&current=temperature_2m&temperature_unit={unit_param}&timezone=GMT&forecast_days=2&past_days=2",
            self.cfg.latitude, self.cfg.longitude
        );
        let response: OpenMeteoResponse = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let target = contract_date.to_string();
        let daily_idx = response
            .daily
            .time
            .iter()
            .position(|day| day == &target)
            .ok_or_else(|| anyhow!("open-meteo missing contract date {target}"))?;
        let daily_max_temp = normalize_temp(
            response.daily.temperature_2m_max[daily_idx],
            self.cfg.settlement_unit.clone(),
            self.cfg.settlement_rounding.clone(),
        );
        let previous_day_max_temp = contract_date
            .pred_opt()
            .and_then(|prev_day| {
                response
                    .daily
                    .time
                    .iter()
                    .position(|day| day == &prev_day.to_string())
                    .map(|idx| {
                        normalize_temp(
                            response.daily.temperature_2m_max[idx],
                            self.cfg.settlement_unit.clone(),
                            self.cfg.settlement_rounding.clone(),
                        )
                    })
            });

        let hourly_peak_hour_local = response
            .hourly
            .peak_for_date(contract_date, self.cfg.station_utc_offset_hours)
            .map(|sample| sample.0);
        let cloud_cover = response.hourly.mean_for_date(
            contract_date,
            self.cfg.station_utc_offset_hours,
            &response.hourly.cloud_cover,
        );
        let precip_prob = response.hourly.mean_for_date(
            contract_date,
            self.cfg.station_utc_offset_hours,
            &response.hourly.precipitation_probability,
        );
        let confidence = (0.82
            - cloud_cover.unwrap_or(0.0) / 400.0
            - precip_prob.unwrap_or(0.0) / 350.0)
            .clamp(0.25, 0.95);

        Ok(OpenMeteoSnapshot {
            daily_max_temp,
            previous_day_max_temp,
            hourly_peak_hour_local,
            confidence,
        })
    }

    async fn fetch_nws_hourly_forecast(&self, contract_date: NaiveDate) -> Result<NwsHourlySnapshot> {
        let office = self
            .cfg
            .nws_grid_office
            .as_deref()
            .ok_or_else(|| anyhow!("weather_market.nws_grid_office missing"))?;
        let grid_x = self
            .cfg
            .nws_grid_x
            .ok_or_else(|| anyhow!("weather_market.nws_grid_x missing"))?;
        let grid_y = self
            .cfg
            .nws_grid_y
            .ok_or_else(|| anyhow!("weather_market.nws_grid_y missing"))?;
        let url = format!(
            "{NWS_BASE_URL}/gridpoints/{office}/{grid_x},{grid_y}/forecast/hourly"
        );
        let response: NwsHourlyForecastResponse = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut best_temp: Option<f64> = None;
        let mut peak_hour: Option<f64> = None;
        let mut precip_sum = 0.0;
        let mut count = 0.0;
        for period in response.properties.periods {
            let start = DateTime::parse_from_rfc3339(&period.start_time)
                .map_err(|e| anyhow!("invalid NWS period start: {e}"))?
                .with_timezone(&Utc);
            let local_date = local_date(start, self.cfg.station_utc_offset_hours);
            if local_date != contract_date {
                continue;
            }
            let normalized = normalize_temp(
                convert_temperature(period.temperature as f64, &period.temperature_unit, &self.cfg.settlement_unit),
                self.cfg.settlement_unit.clone(),
                self.cfg.settlement_rounding.clone(),
            );
            if best_temp.map(|current| normalized > current).unwrap_or(true) {
                best_temp = Some(normalized);
                peak_hour = Some(local_hour(start, self.cfg.station_utc_offset_hours));
            }
            if let Some(prob) = period.probability_of_precipitation.value {
                precip_sum += prob;
                count += 1.0;
            }
        }

        let forecast_max_temp =
            best_temp.ok_or_else(|| anyhow!("NWS hourly forecast missing contract date"))?;
        let mean_precip = if count > 0.0 { precip_sum / count } else { 0.0 };
        let confidence = (0.8 - mean_precip / 300.0).clamp(0.25, 0.95);
        Ok(NwsHourlySnapshot {
            forecast_max_temp,
            peak_hour_local: peak_hour,
            confidence,
        })
    }

    async fn fetch_nws_observations(&self, contract_date: NaiveDate) -> Result<ObservationSnapshot> {
        let station_id = self
            .cfg
            .nws_station_id
            .as_deref()
            .ok_or_else(|| anyhow!("weather_market.nws_station_id missing"))?;
        let end = Utc::now();
        let start = end - Duration::hours(self.cfg.observation_window_hours as i64);
        let url = format!(
            "{NWS_BASE_URL}/stations/{station_id}/observations?start={}&end={}&limit=200",
            start.to_rfc3339(),
            end.to_rfc3339()
        );
        let response: NwsObservationsResponse = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut current_temp = None;
        let mut latest_observation_at = None;
        let mut observed_max_temp = None;
        let mut peak_hour_local = None;
        for feature in response.features {
            let ts = DateTime::parse_from_rfc3339(&feature.properties.timestamp)
                .map_err(|e| anyhow!("invalid NWS observation timestamp: {e}"))?
                .with_timezone(&Utc);
            let local_date = local_date(ts, self.cfg.station_utc_offset_hours);
            let raw_c = match feature.properties.temperature.value {
                Some(value) => value,
                None => continue,
            };
            let normalized = normalize_temp(
                celsius_to_unit(raw_c, self.cfg.settlement_unit.clone()),
                self.cfg.settlement_unit.clone(),
                self.cfg.settlement_rounding.clone(),
            );
            if latest_observation_at.map(|latest| ts >= latest).unwrap_or(true) {
                current_temp = Some(normalized);
                latest_observation_at = Some(ts);
            }
            if local_date != contract_date {
                continue;
            }
            if observed_max_temp
                .map(|current| normalized > current)
                .unwrap_or(true)
            {
                observed_max_temp = Some(normalized);
                peak_hour_local = Some(local_hour(ts, self.cfg.station_utc_offset_hours));
            }
        }

        Ok(ObservationSnapshot {
            current_temp,
            observed_max_temp,
            observed_peak_hour_local: peak_hour_local,
        })
    }

    fn snapshot_event(&self, snapshot: &WeatherSnapshot) -> StrategyEvent {
        let best_bucket = snapshot
            .bucket_views
            .iter()
            .max_by(|a, b| a.model_probability.total_cmp(&b.model_probability))
            .map(|bucket| bucket.label.clone())
            .unwrap_or_else(|| "unknown".to_string());
        StrategyEvent::new(
            StrategyEventType::Custom("weather_market_snapshot".to_string()),
            format!(
                "weather_market {} {} corrected_max={} sigma={:.2} regime={} peak={}",
                snapshot.station_id,
                snapshot.contract_date,
                snapshot.corrected_max_temp,
                snapshot.sigma,
                snapshot.regime,
                snapshot.peak_anomaly
            ),
        )
        .with_data("station_id", &snapshot.station_id)
        .with_data("contract_date", snapshot.contract_date.to_string())
        .with_data("base_max_temp", format!("{:.2}", snapshot.base_max_temp))
        .with_data("corrected_max_temp", format!("{:.2}", snapshot.corrected_max_temp))
        .with_data("sigma", format!("{:.2}", snapshot.sigma))
        .with_data("regime", &snapshot.regime)
        .with_data(
            "regime_confidence",
            format!("{:.3}", snapshot.regime_confidence),
        )
        .with_data("peak_anomaly", &snapshot.peak_anomaly)
        .with_data(
            "peak_anomaly_confidence",
            format!("{:.3}", snapshot.peak_anomaly_confidence),
        )
        .with_data("best_bucket", best_bucket)
    }

    fn recommendation_actions(
        &mut self,
        snapshot: &WeatherSnapshot,
        now: DateTime<Utc>,
    ) -> Vec<StrategyAction> {
        let mut actions = vec![StrategyAction::LogEvent {
            event: self.snapshot_event(snapshot),
        }];
        let Some(best) = snapshot.recommendations.first() else {
            return actions;
        };
        let cooldown_ok = self
            .last_alert_at
            .map(|last| now.signed_duration_since(last).num_seconds() >= self.cfg.alert_cooldown_secs as i64)
            .unwrap_or(true);
        let recommendation_event = StrategyEvent::new(
            StrategyEventType::SignalDetected,
            format!(
                "weather_market recommendation {} edge={:.3} model={:.3} market={:.3}",
                best.label, best.edge, best.model_probability, best.market_reference_price
            ),
        )
        .with_data("label", &best.label)
        .with_data("token_id", &best.token_id)
        .with_data("market_slug", &best.market_slug)
        .with_data("edge", format!("{:.4}", best.edge))
        .with_data("model_probability", format!("{:.4}", best.model_probability))
        .with_data(
            "market_reference_price",
            format!("{:.4}", best.market_reference_price),
        )
        .with_data("confidence", format!("{:.4}", best.confidence))
        .with_data("rationale", &best.rationale);

        actions.push(StrategyAction::LogEvent {
            event: recommendation_event,
        });
        if cooldown_ok {
            self.last_alert_at = Some(now);
            actions.push(StrategyAction::Alert {
                level: AlertLevel::Info,
                message: format!(
                    "observe-only {} suggests {} at {:.3} vs model {:.3} (edge {:.3})",
                    self.id,
                    best.label,
                    best.market_reference_price,
                    best.model_probability,
                    best.edge
                ),
            });
        }
        actions
    }

    fn state_metrics(&self) -> HashMap<String, String> {
        let mut metrics = HashMap::new();
        metrics.insert("observe_only".to_string(), "true".to_string());
        metrics.insert("station_id".to_string(), self.cfg.station_id.clone());
        metrics.insert("contract_date".to_string(), self.cfg.contract_date.clone());
        metrics.insert(
            "bucket_count".to_string(),
            self.cfg.buckets.len().to_string(),
        );
        metrics.insert(
            "tick_interval_ms".to_string(),
            self.cfg.tick_interval_ms.to_string(),
        );
        if let Some(last) = self.last_evaluated_at {
            metrics.insert("last_evaluated_at".to_string(), last.to_rfc3339());
        }
        if let Some(err) = self.last_error.as_ref() {
            metrics.insert("last_error".to_string(), err.clone());
        }
        if let Some(snapshot) = self.last_snapshot.as_ref() {
            metrics.insert(
                "base_max_temp".to_string(),
                format!("{:.2}", snapshot.base_max_temp),
            );
            metrics.insert(
                "corrected_max_temp".to_string(),
                format!("{:.2}", snapshot.corrected_max_temp),
            );
            metrics.insert("sigma".to_string(), format!("{:.2}", snapshot.sigma));
            metrics.insert("regime".to_string(), snapshot.regime.clone());
            metrics.insert("peak_anomaly".to_string(), snapshot.peak_anomaly.clone());
            metrics.insert(
                "recommendation_count".to_string(),
                snapshot.recommendations.len().to_string(),
            );
            if let Some(best) = snapshot.recommendations.first() {
                metrics.insert("best_bucket".to_string(), best.label.clone());
                metrics.insert("best_edge".to_string(), format!("{:.4}", best.edge));
                metrics.insert(
                    "best_market_price".to_string(),
                    format!("{:.4}", best.market_reference_price),
                );
                metrics.insert(
                    "best_model_probability".to_string(),
                    format!("{:.4}", best.model_probability),
                );
            }
        }
        metrics
    }
}

#[async_trait]
impl Strategy for WeatherMarketStrategy {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        STRATEGY_NAME
    }

    fn description(&self) -> &str {
        "Observe-only weather-market strategy using public station and forecast data"
    }

    fn required_feeds(&self) -> Vec<DataFeed> {
        vec![DataFeed::Tick {
            interval_ms: self.cfg.tick_interval_ms,
        }]
    }

    async fn on_market_update(&mut self, _update: &MarketUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_order_update(&mut self, _update: &OrderUpdate) -> Result<Vec<StrategyAction>> {
        Ok(Vec::new())
    }

    async fn on_tick(&mut self, now: DateTime<Utc>) -> Result<Vec<StrategyAction>> {
        if !self.enabled || !self.should_evaluate(now) {
            return Ok(Vec::new());
        }

        match self.evaluate(now).await {
            Ok(snapshot) => {
                self.last_evaluated_at = Some(now);
                self.last_error = None;
                self.last_snapshot = Some(snapshot.clone());
                Ok(self.recommendation_actions(&snapshot, now))
            }
            Err(err) => {
                self.last_evaluated_at = Some(now);
                self.last_error = Some(err.to_string());
                Ok(vec![StrategyAction::LogEvent {
                    event: StrategyEvent::new(
                        StrategyEventType::Error,
                        format!("weather_market evaluation failed: {err}"),
                    ),
                }])
            }
        }
    }

    fn state(&self) -> StrategyStateInfo {
        StrategyStateInfo {
            strategy_id: self.id.clone(),
            phase: if self.enabled {
                "observing".to_string()
            } else {
                "disabled".to_string()
            },
            enabled: self.enabled,
            active: self.enabled,
            position_count: 0,
            pending_order_count: 0,
            total_exposure: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl_today: Decimal::ZERO,
            last_update: self.last_evaluated_at.unwrap_or_else(Utc::now),
            metrics: self.state_metrics(),
        }
    }

    fn positions(&self) -> Vec<PositionInfo> {
        Vec::new()
    }

    fn is_active(&self) -> bool {
        self.enabled
    }

    async fn shutdown(&mut self) -> Result<Vec<StrategyAction>> {
        self.enabled = false;
        Ok(vec![StrategyAction::Alert {
            level: AlertLevel::Info,
            message: format!("{} shutdown (observe_only=true, dry_run={})", self.id, self.dry_run),
        }])
    }

    fn reset(&mut self) {
        self.last_evaluated_at = None;
        self.last_snapshot = None;
        self.last_error = None;
        self.last_alert_at = None;
    }
}

#[derive(Debug, Deserialize)]
struct OpenMeteoResponse {
    daily: OpenMeteoDaily,
    hourly: OpenMeteoHourly,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoDaily {
    time: Vec<String>,
    temperature_2m_max: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct OpenMeteoHourly {
    time: Vec<String>,
    temperature_2m: Vec<f64>,
    #[serde(default)]
    cloud_cover: Vec<f64>,
    #[serde(default)]
    wind_speed_10m: Vec<f64>,
    #[serde(default)]
    precipitation_probability: Vec<f64>,
}

impl OpenMeteoHourly {
    fn peak_for_date(&self, date: NaiveDate, utc_offset_hours: i32) -> Option<(f64, f64)> {
        let mut best: Option<(f64, f64)> = None;
        for (idx, ts_raw) in self.time.iter().enumerate() {
            let ts = DateTime::parse_from_rfc3339(&format!("{ts_raw}:00+00:00"))
                .ok()?
                .with_timezone(&Utc);
            if local_date(ts, utc_offset_hours) != date {
                continue;
            }
            let temp = *self.temperature_2m.get(idx)?;
            let hour = local_hour(ts, utc_offset_hours);
            match best {
                None => best = Some((hour, temp)),
                Some((_, current)) if temp > current => best = Some((hour, temp)),
                _ => {}
            }
        }
        best
    }

    fn mean_for_date(
        &self,
        date: NaiveDate,
        utc_offset_hours: i32,
        values: &[f64],
    ) -> Option<f64> {
        let mut sum = 0.0;
        let mut count = 0.0;
        for (idx, ts_raw) in self.time.iter().enumerate() {
            let ts = DateTime::parse_from_rfc3339(&format!("{ts_raw}:00+00:00"))
                .ok()?
                .with_timezone(&Utc);
            if local_date(ts, utc_offset_hours) != date {
                continue;
            }
            if let Some(value) = values.get(idx) {
                sum += *value;
                count += 1.0;
            }
        }
        if count > 0.0 {
            Some(sum / count)
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct OpenMeteoSnapshot {
    daily_max_temp: f64,
    previous_day_max_temp: Option<f64>,
    hourly_peak_hour_local: Option<f64>,
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct NwsHourlyForecastResponse {
    properties: NwsHourlyProperties,
}

#[derive(Debug, Deserialize)]
struct NwsHourlyProperties {
    periods: Vec<NwsHourlyPeriod>,
}

#[derive(Debug, Deserialize)]
struct NwsHourlyPeriod {
    start_time: String,
    temperature: i64,
    temperature_unit: String,
    probability_of_precipitation: NwsProbabilityValue,
}

#[derive(Debug, Deserialize)]
struct NwsProbabilityValue {
    value: Option<f64>,
}

#[derive(Debug)]
struct NwsHourlySnapshot {
    forecast_max_temp: f64,
    peak_hour_local: Option<f64>,
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct NwsObservationsResponse {
    features: Vec<NwsObservationFeature>,
}

#[derive(Debug, Deserialize)]
struct NwsObservationFeature {
    properties: NwsObservationProperties,
}

#[derive(Debug, Deserialize)]
struct NwsObservationProperties {
    timestamp: String,
    temperature: NwsObservationValue,
}

#[derive(Debug, Deserialize)]
struct NwsObservationValue {
    value: Option<f64>,
}

fn build_bucket_views(
    cfg: &WeatherMarketConfig,
    quotes: &[BucketQuote],
    corrected_max_temp: f64,
    sigma: f64,
) -> Vec<BucketView> {
    cfg.buckets
        .iter()
        .zip(quotes.iter())
        .map(|(bucket, quote)| {
            let probability = bucket_probability(corrected_max_temp, sigma, bucket);
            let market_reference_price = quote
                .best_ask
                .or(quote.mid)
                .or(quote.best_bid)
                .and_then(|value| value.to_f64());
            BucketView {
                label: bucket.label.clone(),
                token_id: quote.token_id.clone(),
                market_slug: quote.market_slug.clone(),
                model_probability: probability,
                target_price: probability,
                market_reference_price,
                edge: market_reference_price.map(|market| probability - market),
            }
        })
        .collect()
}

fn build_recommendations(
    bucket_views: &[BucketView],
    confidence: f64,
    min_edge: f64,
    min_confidence: f64,
    regime: &str,
    peak_anomaly: &str,
) -> Vec<EntrySuggestion> {
    if confidence < min_confidence {
        return Vec::new();
    }

    let mut suggestions: Vec<EntrySuggestion> = bucket_views
        .iter()
        .filter_map(|bucket| {
            let market_reference_price = bucket.market_reference_price?;
            let edge = bucket.edge?;
            (edge >= min_edge).then(|| EntrySuggestion {
                label: bucket.label.clone(),
                token_id: bucket.token_id.clone(),
                market_slug: bucket.market_slug.clone(),
                model_probability: bucket.model_probability,
                market_reference_price,
                edge,
                confidence,
                rationale: format!("regime={regime}, peak_anomaly={peak_anomaly}, observe_only=true"),
            })
        })
        .collect();
    suggestions.sort_by(|a, b| b.edge.total_cmp(&a.edge));
    suggestions
}

fn fuse_forecast_estimates(
    estimates: &[SourceEstimate],
    open_meteo_weight: f64,
    nws_weight: f64,
) -> f64 {
    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for estimate in estimates {
        let base_weight = match estimate.name.as_str() {
            "open_meteo" => open_meteo_weight,
            "nws_hourly" => nws_weight,
            _ => 0.5,
        };
        let weight = (base_weight * estimate.confidence).max(0.05);
        weighted_sum += estimate.normalized_max_temp * weight;
        weight_sum += weight;
    }
    if weight_sum <= f64::EPSILON {
        estimates[0].normalized_max_temp
    } else {
        weighted_sum / weight_sum
    }
}

fn intraday_corrected_max(
    base_max_temp: f64,
    current_temp: Option<f64>,
    observed_max_temp: Option<f64>,
    local_hour: f64,
    peak_window_start_hour: u32,
    peak_window_end_hour: u32,
) -> f64 {
    let observed_max = observed_max_temp.unwrap_or(f64::NEG_INFINITY);
    let current = current_temp.unwrap_or(base_max_temp);
    let heating_start = (peak_window_start_hour.saturating_sub(5)) as f64;
    let peak_end = peak_window_end_hour as f64;
    let progress = ((local_hour - heating_start) / (peak_end - heating_start).max(1.0))
        .clamp(0.0, 1.0);
    let remaining_heat = (base_max_temp - current).max(0.0);
    let forward_component = if local_hour <= peak_end {
        current + remaining_heat * (1.0 - progress.powf(1.35))
    } else {
        base_max_temp - (local_hour - peak_end) * 0.2
    };
    base_max_temp
        .max(forward_component)
        .max(observed_max)
}

fn classify_regime(
    corrected_max_temp: f64,
    previous_day_max_temp: Option<f64>,
    neutral_band: f64,
    sigma: f64,
) -> (String, f64) {
    let Some(previous_day_max_temp) = previous_day_max_temp else {
        return ("unknown".to_string(), 0.35);
    };
    let delta = corrected_max_temp - previous_day_max_temp;
    let label = if delta > neutral_band {
        "warming"
    } else if delta < -neutral_band {
        "cooling"
    } else {
        "flat"
    };
    let confidence = (delta.abs() / sigma.max(0.75) / 2.0).clamp(0.25, 0.95);
    (label.to_string(), confidence)
}

fn classify_peak_anomaly(
    expected_peak_hour_local: Option<f64>,
    peak_window_start_hour: u32,
    peak_window_end_hour: u32,
    late_peak_hour: u32,
) -> (String, f64) {
    let Some(expected_peak_hour_local) = expected_peak_hour_local else {
        return ("unknown".to_string(), 0.3);
    };
    if expected_peak_hour_local >= late_peak_hour as f64 {
        return (
            "late_peak".to_string(),
            ((expected_peak_hour_local - late_peak_hour as f64) / 3.0).clamp(0.3, 0.95),
        );
    }
    if expected_peak_hour_local < peak_window_start_hour as f64 - 1.0 {
        return (
            "early_peak".to_string(),
            ((peak_window_start_hour as f64 - expected_peak_hour_local) / 3.0).clamp(0.3, 0.95),
        );
    }
    if expected_peak_hour_local > peak_window_end_hour as f64 {
        return (
            "extended_peak".to_string(),
            ((expected_peak_hour_local - peak_window_end_hour as f64) / 3.0).clamp(0.25, 0.8),
        );
    }
    ("normal_peak".to_string(), 0.65)
}

fn estimate_sigma(
    estimates: &[SourceEstimate],
    corrected_max_temp: f64,
    observed_max_temp: Option<f64>,
    sigma_floor: f64,
    sigma_spread_multiplier: f64,
) -> f64 {
    let mut spread = 0.0_f64;
    for estimate in estimates {
        spread = spread.max((estimate.normalized_max_temp - corrected_max_temp).abs());
    }
    if let Some(observed_max_temp) = observed_max_temp {
        spread = spread.max((corrected_max_temp - observed_max_temp).abs() * 0.35);
    }
    (sigma_floor + spread * sigma_spread_multiplier).clamp(sigma_floor, 8.0)
}

fn overall_confidence(
    estimates: &[SourceEstimate],
    regime_confidence: f64,
    peak_anomaly_confidence: f64,
    has_observation: bool,
) -> f64 {
    let source_conf = if estimates.is_empty() {
        0.3
    } else {
        estimates.iter().map(|estimate| estimate.confidence).sum::<f64>() / estimates.len() as f64
    };
    let observation_bonus = if has_observation { 0.08 } else { 0.0 };
    (source_conf * 0.55 + regime_confidence * 0.25 + peak_anomaly_confidence * 0.20 + observation_bonus)
        .clamp(0.2, 0.98)
}

fn expected_peak_hour(
    estimates: &[SourceEstimate],
    observed_peak_hour_local: Option<f64>,
) -> Option<f64> {
    if let Some(observed_peak_hour_local) = observed_peak_hour_local {
        return Some(observed_peak_hour_local);
    }
    let mut weighted = 0.0;
    let mut total = 0.0;
    for estimate in estimates {
        let Some(hour) = estimate.expected_peak_hour_local else {
            continue;
        };
        weighted += hour * estimate.confidence;
        total += estimate.confidence;
    }
    (total > 0.0).then_some(weighted / total)
}

fn bucket_probability(mean: f64, sigma: f64, bucket: &WeatherBucketConfig) -> f64 {
    let sigma = sigma.max(0.5);
    let lower = bucket.min_temp.map(|value| normal_cdf((value - mean) / sigma)).unwrap_or(0.0);
    let upper = bucket.max_temp.map(|value| normal_cdf((value - mean) / sigma)).unwrap_or(1.0);
    (upper - lower).clamp(0.0, 1.0)
}

fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();
    0.5 * (1.0 + sign * y)
}

fn local_date(ts: DateTime<Utc>, utc_offset_hours: i32) -> NaiveDate {
    (ts + Duration::hours(utc_offset_hours as i64)).date_naive()
}

fn local_hour(ts: DateTime<Utc>, utc_offset_hours: i32) -> f64 {
    let shifted = ts + Duration::hours(utc_offset_hours as i64);
    shifted.hour() as f64 + shifted.minute() as f64 / 60.0
}

fn normalize_temp(value: f64, unit: TemperatureUnit, rounding: SettlementRounding) -> f64 {
    let base = match rounding {
        SettlementRounding::None => value,
        SettlementRounding::NearestInteger => value.round(),
        SettlementRounding::FloorInteger => value.floor(),
        SettlementRounding::CeilInteger => value.ceil(),
        SettlementRounding::Tenth => (value * 10.0).round() / 10.0,
    };
    match unit {
        TemperatureUnit::Fahrenheit | TemperatureUnit::Celsius => base,
    }
}

fn celsius_to_unit(value_c: f64, unit: TemperatureUnit) -> f64 {
    match unit {
        TemperatureUnit::Celsius => value_c,
        TemperatureUnit::Fahrenheit => value_c * 9.0 / 5.0 + 32.0,
    }
}

fn convert_temperature(value: f64, source_unit: &str, target: &TemperatureUnit) -> f64 {
    match (source_unit.trim().to_ascii_uppercase().as_str(), target) {
        ("F", TemperatureUnit::Fahrenheit) => value,
        ("F", TemperatureUnit::Celsius) => (value - 32.0) * 5.0 / 9.0,
        ("C", TemperatureUnit::Celsius) => value,
        ("C", TemperatureUnit::Fahrenheit) => value * 9.0 / 5.0 + 32.0,
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_toml() -> &'static str {
        r#"
[strategy]
name = "weather_market"
enabled = true

[weather_market]
station_id = "KJFK"
station_name = "JFK"
contract_date = "2026-03-20"
latitude = 40.64
longitude = -73.78
station_utc_offset_hours = -4
recommendation_min_edge = 0.04

[[weather_market.buckets]]
label = "70-72F"
token_id = "token-a"
min_temp = 70.0
max_temp = 73.0

[[weather_market.buckets]]
label = "73-75F"
token_id = "token-b"
min_temp = 73.0
max_temp = 76.0
"#
    }

    #[test]
    fn from_toml_builds_weather_strategy() {
        let strategy =
            WeatherMarketStrategy::from_toml("wx-test".to_string(), minimal_toml(), true)
                .expect("strategy");
        assert_eq!(strategy.name(), "weather_market");
        assert!(matches!(
            strategy.required_feeds().as_slice(),
            [DataFeed::Tick { interval_ms: 300000 }]
        ));
        assert_eq!(strategy.cfg.buckets.len(), 2);
        assert!(strategy.cfg.observe_only);
    }

    #[test]
    fn bucket_probabilities_sum_close_to_one_for_covering_ranges() {
        let buckets = vec![
            WeatherBucketConfig {
                label: "<70F".to_string(),
                token_id: "a".to_string(),
                market_slug: None,
                min_temp: None,
                max_temp: Some(70.0),
            },
            WeatherBucketConfig {
                label: "70-72F".to_string(),
                token_id: "b".to_string(),
                market_slug: None,
                min_temp: Some(70.0),
                max_temp: Some(73.0),
            },
            WeatherBucketConfig {
                label: ">=73F".to_string(),
                token_id: "c".to_string(),
                market_slug: None,
                min_temp: Some(73.0),
                max_temp: None,
            },
        ];
        let total = buckets
            .iter()
            .map(|bucket| bucket_probability(72.0, 1.5, bucket))
            .sum::<f64>();
        assert!((total - 1.0).abs() < 0.02);
    }

    #[test]
    fn classification_helpers_detect_regime_and_peak_anomaly() {
        let (regime, confidence) = classify_regime(78.0, Some(72.0), 1.0, 2.0);
        assert_eq!(regime, "warming");
        assert!(confidence > 0.5);

        let (peak, peak_confidence) = classify_peak_anomaly(Some(19.5), 13, 17, 18);
        assert_eq!(peak, "late_peak");
        assert!(peak_confidence >= 0.5);
    }

    #[test]
    fn recommendations_require_edge_and_confidence() {
        let bucket_views = vec![BucketView {
            label: "73-75F".to_string(),
            token_id: "token-b".to_string(),
            market_slug: "jfk-73-75".to_string(),
            model_probability: 0.42,
            target_price: 0.42,
            market_reference_price: Some(0.31),
            edge: Some(0.11),
        }];
        let suggestions = build_recommendations(
            &bucket_views,
            0.72,
            0.05,
            0.55,
            "warming",
            "late_peak",
        );
        assert_eq!(suggestions.len(), 1);
        assert!(suggestions[0].rationale.contains("warming"));
    }
}
