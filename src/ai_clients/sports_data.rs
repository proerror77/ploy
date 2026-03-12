use crate::ai_clients::grok::GrokClient;
use crate::error::Result;
use tracing::info;

mod fetch_queries;
mod formatting;
mod parsing;
mod types;

pub use formatting::format_for_claude;
pub use types::{
    AdvancedAnalytics, BettingLines, BettingTrends, DataQuality, GameInfo, HeadToHeadData,
    HistoricalGame, InjuryStatus, NewsData, NewsItem, PlayerStatus, SentimentData,
    StructuredGameData, TeamPerformance, TeamStats,
};

/// Sports Data Fetcher - Gets structured data from Grok
pub struct SportsDataFetcher {
    grok: GrokClient,
}

impl SportsDataFetcher {
    pub fn new(grok: GrokClient) -> Self {
        Self { grok }
    }

    /// Fetch structured game data for a matchup
    pub async fn fetch_game_data(
        &self,
        team1: &str,
        team2: &str,
        league: &str,
    ) -> Result<StructuredGameData> {
        info!(
            "Fetching comprehensive structured data for {} vs {}",
            team1, team2
        );

        info!("📊 Step 1/7: Fetching player status and injuries...");
        let players = self.fetch_player_status(team1, team2, league).await?;

        info!("💰 Step 2/7: Fetching betting lines and odds...");
        let betting = self.fetch_betting_lines(team1, team2).await?;

        info!("📈 Step 3/7: Analyzing market sentiment...");
        let sentiment = self.fetch_sentiment(team1, team2).await?;

        info!("📰 Step 4/7: Fetching breaking news and updates...");
        let news = self.fetch_news(team1, team2, league).await?;

        info!("🔄 Step 5/7: Analyzing head-to-head history...");
        let head_to_head = self.fetch_head_to_head(team1, team2, league).await?;

        info!("📊 Step 6/7: Fetching team statistics and trends...");
        let team_stats = self.fetch_team_stats(team1, team2, league).await?;

        info!("🎯 Step 7/7: Analyzing advanced metrics and trends...");
        let advanced_analytics = self.fetch_advanced_analytics(team1, team2, league).await?;

        info!("✅ Data collection complete!");

        Ok(StructuredGameData {
            game_info: GameInfo {
                team1: team1.to_string(),
                team2: team2.to_string(),
                game_time: "TBD".to_string(),
                venue: "TBD".to_string(),
                league: league.to_string(),
            },
            team1_players: players.0,
            team2_players: players.1,
            betting_lines: betting,
            sentiment,
            news,
            head_to_head,
            team_stats,
            advanced_analytics,
            data_quality: DataQuality {
                sources_count: 7,
                data_freshness: "< 1 hour".to_string(),
                confidence: 0.90,
            },
        })
    }
}

#[cfg(test)]
mod tests;
