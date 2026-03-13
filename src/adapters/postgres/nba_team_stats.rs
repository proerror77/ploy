use sqlx::Row;

use crate::error::Result;
use crate::strategy::nba_comeback::nba_data_collector::TeamStats;

use super::PostgresStore;

impl PostgresStore {
    /// Load all team stats for a given season
    pub async fn load_nba_team_stats(&self, season: &str) -> Result<Vec<TeamStats>> {
        let rows = sqlx::query(
            r#"
            SELECT team_name, team_abbrev, season,
                   wins, losses, win_rate, avg_points,
                   q1_avg_points, q2_avg_points, q3_avg_points, q4_avg_points,
                   comeback_rate_5pt, comeback_rate_10pt, comeback_rate_15pt,
                   q4_net_rating, q4_pace,
                   elo_rating, offensive_rating, defensive_rating
            FROM nba_team_stats
            WHERE season = $1
            "#,
        )
        .bind(season)
        .fetch_all(&self.pool)
        .await?;

        let stats = rows
            .iter()
            .map(|r| TeamStats {
                team_name: r.get("team_name"),
                season: r.get("season"),
                wins: r.get("wins"),
                losses: r.get("losses"),
                win_rate: r.get("win_rate"),
                avg_points: r.get("avg_points"),
                q1_avg_points: r.get("q1_avg_points"),
                q2_avg_points: r.get("q2_avg_points"),
                q3_avg_points: r.get("q3_avg_points"),
                q4_avg_points: r.get("q4_avg_points"),
                comeback_rate_5pt: r.get("comeback_rate_5pt"),
                comeback_rate_10pt: r.get("comeback_rate_10pt"),
                comeback_rate_15pt: r.get("comeback_rate_15pt"),
                elo_rating: r.get("elo_rating"),
                offensive_rating: r.get("offensive_rating"),
                defensive_rating: r.get("defensive_rating"),
            })
            .collect();

        Ok(stats)
    }

    /// Upsert a single team's stats (insert or update on conflict)
    pub async fn upsert_nba_team_stats(
        &self,
        team_name: &str,
        team_abbrev: &str,
        season: &str,
        stats: &TeamStats,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO nba_team_stats (
                team_name, team_abbrev, season,
                wins, losses, win_rate, avg_points,
                q1_avg_points, q2_avg_points, q3_avg_points, q4_avg_points,
                comeback_rate_5pt, comeback_rate_10pt, comeback_rate_15pt,
                q4_net_rating,
                elo_rating, offensive_rating, defensive_rating,
                updated_at
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18, NOW())
            ON CONFLICT (team_abbrev, season) DO UPDATE SET
                team_name = EXCLUDED.team_name,
                wins = EXCLUDED.wins,
                losses = EXCLUDED.losses,
                win_rate = EXCLUDED.win_rate,
                avg_points = EXCLUDED.avg_points,
                q1_avg_points = EXCLUDED.q1_avg_points,
                q2_avg_points = EXCLUDED.q2_avg_points,
                q3_avg_points = EXCLUDED.q3_avg_points,
                q4_avg_points = EXCLUDED.q4_avg_points,
                comeback_rate_5pt = EXCLUDED.comeback_rate_5pt,
                comeback_rate_10pt = EXCLUDED.comeback_rate_10pt,
                comeback_rate_15pt = EXCLUDED.comeback_rate_15pt,
                q4_net_rating = EXCLUDED.q4_net_rating,
                elo_rating = EXCLUDED.elo_rating,
                offensive_rating = EXCLUDED.offensive_rating,
                defensive_rating = EXCLUDED.defensive_rating,
                updated_at = NOW()
            "#,
        )
        .bind(team_name)
        .bind(team_abbrev)
        .bind(season)
        .bind(stats.wins)
        .bind(stats.losses)
        .bind(stats.win_rate)
        .bind(stats.avg_points)
        .bind(stats.q1_avg_points)
        .bind(stats.q2_avg_points)
        .bind(stats.q3_avg_points)
        .bind(stats.q4_avg_points)
        .bind(stats.comeback_rate_5pt)
        .bind(stats.comeback_rate_10pt)
        .bind(stats.comeback_rate_15pt)
        .bind(0.0f64)
        .bind(stats.elo_rating)
        .bind(stats.offensive_rating)
        .bind(stats.defensive_rating)
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
