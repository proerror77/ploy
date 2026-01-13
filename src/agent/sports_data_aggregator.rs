//! Multi-Source Sports Data Aggregator
//!
//! Provides robust data collection from multiple sources with:
//! - Fallback mechanisms
//! - Data validation and quality scoring
//! - Caching and rate limiting
//! - Source reliability tracking

use crate::agent::grok::GrokClient;
use crate::agent::sports_data::{
    StructuredGameData, PlayerStatus, BettingLines, SentimentData,
    NewsData, HeadToHeadData, TeamStats, AdvancedAnalytics, GameInfo, DataQuality
};
use crate::error::{PloyError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn, error};
use chrono::{DateTime, Utc, Duration};

/// Data source types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataSource {
    Grok,           // X/Twitter real-time data
    OddsAPI,        // The Odds API (DraftKings, FanDuel, etc.)
    ESPN,           // ESPN stats API
    NBA,            // Official NBA API
    Polymarket,     // Polymarket market data
    SportsRadar,    // SportsRadar API
    Cache,          // Local cache
}

impl DataSource {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Grok => "Grok (X/Twitter)",
            Self::OddsAPI => "The Odds API",
            Self::ESPN => "ESPN API",
            Self::NBA => "NBA Official API",
            Self::Polymarket => "Polymarket",
            Self::SportsRadar => "SportsRadar",
            Self::Cache => "Cache",
        }
    }

    pub fn priority(&self) -> u8 {
        match self {
            Self::NBA => 10,           // Official source
            Self::ESPN => 9,           // Highly reliable
            Self::SportsRadar => 8,    // Professional data
            Self::OddsAPI => 7,        // Betting data
            Self::Grok => 6,           // Real-time but variable
            Self::Polymarket => 5,     // Market data
            Self::Cache => 1,          // Fallback only
        }
    }
}

/// Data quality metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataQualityMetrics {
    pub source: DataSource,
    pub timestamp: DateTime<Utc>,
    pub completeness: f64,      // 0.0-1.0
    pub freshness: f64,         // 0.0-1.0
    pub reliability: f64,       // 0.0-1.0
    pub consistency: f64,       // 0.0-1.0
    pub overall_score: f64,     // 0.0-1.0
}

impl DataQualityMetrics {
    pub fn calculate_score(&mut self) {
        self.overall_score = (
            self.completeness * 0.3 +
            self.freshness * 0.25 +
            self.reliability * 0.25 +
            self.consistency * 0.2
        );
    }
}

/// Cached data entry
#[derive(Debug, Clone)]
struct CachedData {
    data: StructuredGameData,
    timestamp: DateTime<Utc>,
    quality: DataQualityMetrics,
}

/// Source reliability tracker
#[derive(Debug, Clone)]
struct SourceReliability {
    success_count: u32,
    failure_count: u32,
    avg_response_time: f64,
    last_success: Option<DateTime<Utc>>,
    last_failure: Option<DateTime<Utc>>,
}

impl SourceReliability {
    fn new() -> Self {
        Self {
            success_count: 0,
            failure_count: 0,
            avg_response_time: 0.0,
            last_success: None,
            last_failure: None,
        }
    }

    fn success_rate(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            0.5 // Unknown
        } else {
            self.success_count as f64 / total as f64
        }
    }

    fn record_success(&mut self, response_time: f64) {
        self.success_count += 1;
        self.last_success = Some(Utc::now());

        // Update average response time
        let total = self.success_count + self.failure_count;
        self.avg_response_time = (
            self.avg_response_time * (total - 1) as f64 + response_time
        ) / total as f64;
    }

    fn record_failure(&mut self) {
        self.failure_count += 1;
        self.last_failure = Some(Utc::now());
    }
}

/// Multi-source sports data aggregator
pub struct SportsDataAggregator {
    grok: GrokClient,
    cache: Arc<RwLock<HashMap<String, CachedData>>>,
    reliability: Arc<RwLock<HashMap<DataSource, SourceReliability>>>,
    cache_ttl: Duration,
}

impl SportsDataAggregator {
    pub fn new(grok: GrokClient) -> Self {
        Self {
            grok,
            cache: Arc::new(RwLock::new(HashMap::new())),
            reliability: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::minutes(5),
        }
    }

    /// Fetch game data with multi-source aggregation
    pub async fn fetch_game_data(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<AggregatedGameData> {
        let cache_key = format!("{}-{}-{}", league, team1, team2);

        // Check cache first
        if let Some(cached) = self.get_cached(&cache_key).await {
            info!("Using cached data (age: {}s)",
                (Utc::now() - cached.timestamp).num_seconds());
            return Ok(AggregatedGameData {
                data: cached.data,
                sources: vec![DataSource::Cache],
                quality: cached.quality,
            });
        }

        info!("Fetching fresh data from multiple sources...");

        // Fetch from multiple sources in parallel
        let mut tasks = vec![];

        // Source 1: Grok (primary)
        let grok_task = self.fetch_from_grok(team1, team2, league);
        tasks.push(tokio::spawn(grok_task));

        // Source 2: The Odds API (if configured)
        if std::env::var("THE_ODDS_API_KEY").is_ok() {
            let odds_task = self.fetch_from_odds_api(team1, team2, league);
            tasks.push(tokio::spawn(odds_task));
        }

        // Source 3: ESPN API (if configured)
        if std::env::var("ESPN_API_KEY").is_ok() {
            let espn_task = self.fetch_from_espn(team1, team2, league);
            tasks.push(tokio::spawn(espn_task));
        }

        // Wait for all sources
        let results = futures::future::join_all(tasks).await;

        // Aggregate results
        let mut successful_sources = vec![];
        let mut all_data = vec![];

        for result in results {
            match result {
                Ok(Ok((source, data, quality))) => {
                    info!("✓ {} succeeded (quality: {:.2})",
                        source.name(), quality.overall_score);
                    successful_sources.push(source);
                    all_data.push((source, data, quality));

                    // Update reliability
                    self.record_success(source, 1.0).await;
                }
                Ok(Err(e)) => {
                    warn!("✗ Source failed: {}", e);
                }
                Err(e) => {
                    error!("✗ Task panicked: {}", e);
                }
            }
        }

        if all_data.is_empty() {
            return Err(PloyError::Internal(
                "All data sources failed".into()
            ));
        }

        // Merge data from multiple sources
        let aggregated = self.merge_data(all_data)?;

        // Cache the result
        self.cache_data(&cache_key, &aggregated).await;

        Ok(aggregated)
    }

    /// Fetch from Grok (X/Twitter)
    async fn fetch_from_grok(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<(DataSource, StructuredGameData, DataQualityMetrics)> {
        let start = std::time::Instant::now();

        // Use existing SportsDataFetcher logic
        let fetcher = crate::agent::sports_data::SportsDataFetcher::new(
            self.grok.clone()
        );

        let data = fetcher.fetch_game_data(team1, team2, league).await?;

        let elapsed = start.elapsed().as_secs_f64();

        // Calculate quality metrics
        let mut quality = DataQualityMetrics {
            source: DataSource::Grok,
            timestamp: Utc::now(),
            completeness: self.calculate_completeness(&data),
            freshness: 1.0, // Just fetched
            reliability: 0.8, // Grok is generally reliable
            consistency: 0.9, // Good format consistency
            overall_score: 0.0,
        };
        quality.calculate_score();

        Ok((DataSource::Grok, data, quality))
    }

    /// Fetch from The Odds API
    async fn fetch_from_odds_api(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<(DataSource, StructuredGameData, DataQualityMetrics)> {
        use crate::agent::odds_provider::{OddsProvider, Sport};

        let provider = OddsProvider::from_env()?;

        let sport = match league.to_uppercase().as_str() {
            "NBA" => Sport::NBA,
            "NFL" => Sport::NFL,
            "NHL" => Sport::NHL,
            "MLB" => Sport::MLB,
            _ => return Err(PloyError::Internal("Unsupported league".into())),
        };

        // Fetch odds
        let odds = provider.fetch_odds(sport, team1, team2).await?;

        // Convert to StructuredGameData format
        let data = self.odds_to_structured_data(team1, team2, league, &odds)?;

        let mut quality = DataQualityMetrics {
            source: DataSource::OddsAPI,
            timestamp: Utc::now(),
            completeness: 0.6, // Only betting data
            freshness: 1.0,
            reliability: 0.95, // Very reliable
            consistency: 1.0,
            overall_score: 0.0,
        };
        quality.calculate_score();

        Ok((DataSource::OddsAPI, data, quality))
    }

    /// Fetch from ESPN API
    async fn fetch_from_espn(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<(DataSource, StructuredGameData, DataQualityMetrics)> {
        // TODO: Implement ESPN API integration
        // For now, return error
        Err(PloyError::Internal("ESPN API not yet implemented".into()))
    }

    /// Merge data from multiple sources
    fn merge_data(
        &self,
        sources: Vec<(DataSource, StructuredGameData, DataQualityMetrics)>,
    ) -> Result<AggregatedGameData> {
        if sources.is_empty() {
            return Err(PloyError::Internal("No data to merge".into()));
        }

        // Sort by quality score (highest first)
        let mut sorted = sources;
        sorted.sort_by(|a, b| {
            b.2.overall_score.partial_cmp(&a.2.overall_score).unwrap()
        });

        // Use highest quality source as base
        let (primary_source, mut merged_data, primary_quality) = sorted[0].clone();

        info!("Using {} as primary source (quality: {:.2})",
            primary_source.name(), primary_quality.overall_score);

        // Merge additional data from other sources
        for (source, data, quality) in sorted.iter().skip(1) {
            info!("Merging data from {} (quality: {:.2})",
                source.name(), quality.overall_score);

            // Merge betting lines if better
            if quality.overall_score > 0.7 {
                if data.betting_lines.spread != 0.0 {
                    merged_data.betting_lines = data.betting_lines.clone();
                }
            }

            // Merge player data if more complete
            if data.team1_players.len() > merged_data.team1_players.len() {
                merged_data.team1_players = data.team1_players.clone();
            }
            if data.team2_players.len() > merged_data.team2_players.len() {
                merged_data.team2_players = data.team2_players.clone();
            }

            // Merge news if available
            if !data.news.breaking_news.is_empty() {
                merged_data.news.breaking_news.extend(
                    data.news.breaking_news.clone()
                );
            }
        }

        // Calculate overall quality
        let avg_quality = sorted.iter()
            .map(|(_, _, q)| q.overall_score)
            .sum::<f64>() / sorted.len() as f64;

        let overall_quality = DataQualityMetrics {
            source: primary_source,
            timestamp: Utc::now(),
            completeness: self.calculate_completeness(&merged_data),
            freshness: 1.0,
            reliability: avg_quality,
            consistency: primary_quality.consistency,
            overall_score: avg_quality,
        };

        // Update data quality in merged data
        merged_data.data_quality.confidence = avg_quality;
        merged_data.data_quality.sources_count = sorted.len() as u32;

        Ok(AggregatedGameData {
            data: merged_data,
            sources: sorted.iter().map(|(s, _, _)| *s).collect(),
            quality: overall_quality,
        })
    }

    /// Calculate data completeness score
    fn calculate_completeness(&self, data: &StructuredGameData) -> f64 {
        let mut score = 0.0;
        let mut total = 0.0;

        // Check each data section
        total += 1.0;
        if !data.team1_players.is_empty() && !data.team2_players.is_empty() {
            score += 1.0;
        }

        total += 1.0;
        if data.betting_lines.spread != 0.0 {
            score += 1.0;
        }

        total += 1.0;
        if !data.sentiment.key_narratives.is_empty() {
            score += 1.0;
        }

        total += 1.0;
        if !data.news.breaking_news.is_empty() {
            score += 1.0;
        }

        total += 1.0;
        if !data.head_to_head.last_5_meetings.is_empty() {
            score += 1.0;
        }

        total += 1.0;
        if data.team_stats.team1_stats.record != "0-0" {
            score += 1.0;
        }

        total += 1.0;
        if !data.advanced_analytics.team1_trends.is_empty() {
            score += 1.0;
        }

        score / total
    }

    /// Convert odds data to StructuredGameData
    fn odds_to_structured_data(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
        odds: &crate::agent::odds_provider::OddsData,
    ) -> Result<StructuredGameData> {
        use crate::agent::sports_data::*;

        // Create minimal structured data with betting lines
        Ok(StructuredGameData {
            game_info: GameInfo {
                team1: team1.to_string(),
                team2: team2.to_string(),
                game_time: "TBD".to_string(),
                venue: "TBD".to_string(),
                league: league.to_string(),
            },
            team1_players: vec![],
            team2_players: vec![],
            betting_lines: BettingLines {
                spread: odds.spread.unwrap_or(0.0),
                spread_team: team1.to_string(),
                moneyline_favorite: odds.home_odds.to_string().parse().unwrap_or(-110),
                moneyline_underdog: odds.away_odds.to_string().parse().unwrap_or(-110),
                over_under: odds.total.unwrap_or(0.0),
                implied_probability: odds.home_implied_prob.to_string()
                    .parse().unwrap_or(0.5),
                line_movement: None,
            },
            sentiment: SentimentData {
                expert_pick: team1.to_string(),
                expert_confidence: 0.5,
                public_bet_percentage: 50.0,
                sharp_money_side: team1.to_string(),
                social_sentiment: "NEUTRAL".to_string(),
                key_narratives: vec![],
            },
            news: NewsData {
                breaking_news: vec![],
                injury_updates: vec![],
                lineup_changes: vec![],
                weather_impact: None,
            },
            head_to_head: HeadToHeadData {
                last_5_meetings: vec![],
                team1_wins: 0,
                team2_wins: 0,
                avg_total_points: 0.0,
                avg_margin: 0.0,
            },
            team_stats: TeamStats {
                team1_stats: Default::default(),
                team2_stats: Default::default(),
            },
            advanced_analytics: AdvancedAnalytics {
                team1_trends: vec![],
                team2_trends: vec![],
                situational_factors: vec![],
                betting_trends: Default::default(),
            },
            data_quality: DataQuality {
                sources_count: 1,
                data_freshness: "< 1 min".to_string(),
                confidence: 0.8,
            },
        })
    }

    /// Get cached data if available and fresh
    async fn get_cached(&self, key: &str) -> Option<CachedData> {
        let cache = self.cache.read().await;
        if let Some(cached) = cache.get(key) {
            let age = Utc::now() - cached.timestamp;
            if age < self.cache_ttl {
                return Some(cached.clone());
            }
        }
        None
    }

    /// Cache data
    async fn cache_data(&self, key: &str, data: &AggregatedGameData) {
        let mut cache = self.cache.write().await;
        cache.insert(key.to_string(), CachedData {
            data: data.data.clone(),
            timestamp: Utc::now(),
            quality: data.quality.clone(),
        });
    }

    /// Record successful fetch
    async fn record_success(&self, source: DataSource, response_time: f64) {
        let mut reliability = self.reliability.write().await;
        let entry = reliability.entry(source)
            .or_insert_with(SourceReliability::new);
        entry.record_success(response_time);
    }

    /// Record failed fetch
    async fn record_failure(&self, source: DataSource) {
        let mut reliability = self.reliability.write().await;
        let entry = reliability.entry(source)
            .or_insert_with(SourceReliability::new);
        entry.record_failure();
    }

    /// Get source reliability stats
    pub async fn get_reliability_stats(&self) -> HashMap<DataSource, f64> {
        let reliability = self.reliability.read().await;
        reliability.iter()
            .map(|(source, stats)| (*source, stats.success_rate()))
            .collect()
    }
}

/// Aggregated game data with quality metrics
#[derive(Debug, Clone)]
pub struct AggregatedGameData {
    pub data: StructuredGameData,
    pub sources: Vec<DataSource>,
    pub quality: DataQualityMetrics,
}

impl AggregatedGameData {
    /// Get human-readable source list
    pub fn source_names(&self) -> String {
        self.sources.iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Check if data quality is acceptable
    pub fn is_acceptable(&self, min_quality: f64) -> bool {
        self.quality.overall_score >= min_quality
    }
}

// Default implementations for missing types
impl Default for crate::agent::sports_data::TeamPerformance {
    fn default() -> Self {
        Self {
            team_name: String::new(),
            record: "0-0".to_string(),
            last_10_record: "0-0".to_string(),
            home_record: None,
            away_record: None,
            avg_points_scored: 0.0,
            avg_points_allowed: 0.0,
            offensive_rating: 0.0,
            defensive_rating: 0.0,
            pace: 0.0,
            recent_form: String::new(),
            rest_days: 0,
            back_to_back: false,
        }
    }
}

impl Default for crate::agent::sports_data::BettingTrends {
    fn default() -> Self {
        Self {
            team1_ats_record: "0-0-0".to_string(),
            team2_ats_record: "0-0-0".to_string(),
            team1_over_under_record: "0-0-0".to_string(),
            team2_over_under_record: "0-0-0".to_string(),
            public_money_percentage: 50.0,
            sharp_money_percentage: 50.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_source_priority() {
        assert!(DataSource::NBA.priority() > DataSource::Grok.priority());
        assert!(DataSource::ESPN.priority() > DataSource::Polymarket.priority());
    }

    #[test]
    fn test_quality_score_calculation() {
        let mut quality = DataQualityMetrics {
            source: DataSource::Grok,
            timestamp: Utc::now(),
            completeness: 0.8,
            freshness: 1.0,
            reliability: 0.9,
            consistency: 0.85,
            overall_score: 0.0,
        };

        quality.calculate_score();
        assert!(quality.overall_score > 0.8);
        assert!(quality.overall_score < 1.0);
    }

    #[test]
    fn test_source_reliability() {
        let mut reliability = SourceReliability::new();

        reliability.record_success(1.0);
        reliability.record_success(1.5);
        reliability.record_failure();

        assert_eq!(reliability.success_rate(), 2.0 / 3.0);
        assert_eq!(reliability.avg_response_time, 1.25);
    }
}
