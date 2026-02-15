//! Comeback Stats Provider
//!
//! Loads historical team comeback rates from the database and provides
//! lookup by team abbreviation. Used by the core scan cycle to determine
//! whether a trailing team has a historically viable comeback rate.

use anyhow::Result;
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use tracing::{debug, info};

/// Internal row type for sqlx query_as
#[derive(Debug, FromRow)]
struct TeamStatsRow {
    team_name: String,
    team_abbrev: String,
    comeback_rate_5pt: f64,
    comeback_rate_10pt: f64,
    comeback_rate_15pt: f64,
    q4_net_rating: f64,
    q4_avg_points: f64,
    elo_rating: Option<f64>,
}

/// Per-team comeback profile loaded from nba_team_stats
#[derive(Debug, Clone)]
pub struct TeamComebackProfile {
    pub team_name: String,
    pub team_abbrev: String,
    pub comeback_rate_5pt: f64,
    pub comeback_rate_10pt: f64,
    pub comeback_rate_15pt: f64,
    pub q4_net_rating: f64,
    pub q4_avg_points: f64,
    pub elo_rating: f64,
}

/// Provides team comeback stats from the database with in-memory cache
pub struct ComebackStatsProvider {
    pool: PgPool,
    cache: HashMap<String, TeamComebackProfile>,
    season: String,
}

impl ComebackStatsProvider {
    pub fn new(pool: PgPool, season: String) -> Self {
        Self {
            pool,
            cache: HashMap::new(),
            season,
        }
    }

    /// Load all team stats for the configured season into cache
    pub async fn load_all(&mut self) -> Result<()> {
        let rows = sqlx::query_as::<_, TeamStatsRow>(
            r#"
            SELECT team_name, team_abbrev,
                   comeback_rate_5pt, comeback_rate_10pt, comeback_rate_15pt,
                   q4_net_rating, q4_avg_points, elo_rating
            FROM nba_team_stats
            WHERE season = $1
            "#,
        )
        .bind(&self.season)
        .fetch_all(&self.pool)
        .await?;

        self.cache.clear();
        for row in rows {
            let profile = TeamComebackProfile {
                team_name: row.team_name.clone(),
                team_abbrev: row.team_abbrev.clone(),
                comeback_rate_5pt: row.comeback_rate_5pt,
                comeback_rate_10pt: row.comeback_rate_10pt,
                comeback_rate_15pt: row.comeback_rate_15pt,
                q4_net_rating: row.q4_net_rating,
                q4_avg_points: row.q4_avg_points,
                elo_rating: row.elo_rating.unwrap_or(1500.0),
            };
            self.cache.insert(row.team_abbrev.clone(), profile);
        }

        info!(
            "Loaded comeback stats for {} teams (season {})",
            self.cache.len(),
            self.season
        );
        Ok(())
    }

    /// Get a team's comeback profile by abbreviation (e.g. "BOS")
    pub fn get_profile(&self, team_abbrev: &str) -> Option<&TeamComebackProfile> {
        self.cache.get(team_abbrev)
    }

    /// Get the comeback rate for a specific deficit bucket.
    ///
    /// Buckets:
    /// - deficit 1-5  → comeback_rate_5pt
    /// - deficit 6-10 → comeback_rate_10pt
    /// - deficit 11-15 → comeback_rate_15pt
    /// - deficit >15  → None (too large to trade)
    pub fn comeback_rate_for_deficit(&self, team_abbrev: &str, deficit: i32) -> Option<f64> {
        let profile = self.cache.get(team_abbrev)?;

        let rate = if deficit <= 5 {
            profile.comeback_rate_5pt
        } else if deficit <= 10 {
            profile.comeback_rate_10pt
        } else if deficit <= 15 {
            profile.comeback_rate_15pt
        } else {
            return None; // Too large
        };

        debug!(
            "{} deficit={} → comeback_rate={:.3}",
            team_abbrev, deficit, rate
        );
        Some(rate)
    }

    pub fn team_count(&self) -> usize {
        self.cache.len()
    }
}
