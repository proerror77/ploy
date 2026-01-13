---
name: sports-bet
description: AI-powered sports betting analysis using Grok data collection and Claude Opus prediction
version: 1.0.0
author: Ploy Team
---

# Sports Betting Analysis

Analyze sports betting opportunities on Polymarket using multi-source AI analysis.

## Usage

This skill analyzes a sports event from Polymarket and provides:
- AI-powered win probability predictions (via Claude Opus)
- Multi-source data collection (via Grok)
- Edge calculation vs market odds
- Trade recommendations with position sizing

## Parameters

- `url` (required): Polymarket event URL
  - Example: `https://polymarket.com/event/nba-phi-dal-2026-01-11`
- `compare_draftkings` (optional): Include DraftKings odds comparison
  - Default: `false`
- `min_edge` (optional): Minimum edge percentage to recommend
  - Default: `5.0`

## Example Invocations

### Basic Analysis
```
Analyze this NBA game: https://polymarket.com/event/nba-phi-dal-2026-01-11
```

### With DraftKings Comparison
```
Analyze this game with DraftKings comparison: https://polymarket.com/event/nba-phi-dal-2026-01-11
```

### Custom Edge Threshold
```
Analyze this game with minimum 8% edge: https://polymarket.com/event/nba-phi-dal-2026-01-11
```

## What This Skill Does

1. **Parses Event URL**: Extracts team names and league from Polymarket URL
2. **Collects Data** (via Grok):
   - Player status and injuries
   - Betting lines from major sportsbooks
   - Expert picks and public sentiment
   - Breaking news and lineup changes
   - Head-to-head history
   - Team statistics and trends
   - Advanced analytics (ATS records, situational factors)
3. **AI Analysis** (via Claude Opus):
   - Analyzes all structured data
   - Predicts win probabilities
   - Compares with market odds
   - Calculates edge opportunities
4. **Generates Recommendation**:
   - Action: Buy/Sell/Hold/Avoid
   - Edge percentage
   - Confidence level
   - Position size (Kelly Criterion)

## Output Format

The skill returns a structured analysis including:

```
Game: Philadelphia 76ers vs Dallas Mavericks
League: NBA

Market Odds (Polymarket):
- 76ers YES: 0.450 (45.0%)
- Mavericks YES: 0.550 (55.0%)

AI Prediction (Claude Opus):
- 76ers Win Probability: 58.5%
- Mavericks Win Probability: 41.5%
- Confidence: 78%

Reasoning: Embiid upgraded to probable with strong recent form.
76ers are 8-2 ATS at home. Mavericks on 3rd game of road trip.

Key Factors:
- Embiid return from injury (32.5/11.2/5.8 last 5 games)
- Home court advantage (76ers 15-5 at home)
- Mavericks fatigue factor (3rd road game)
- Sharp money on 76ers (52% vs 48% public)

Trade Recommendation:
- Action: BUY
- Side: 76ers YES
- Edge: +13.5%
- Suggested Size: 8.2% of bankroll
- Reasoning: Predicted 58.5% vs market 45.0% = 13.5% edge
```

## Decision Rules

- **Minimum Edge**: 5% difference from market odds (configurable)
- **Minimum Confidence**: 70% AI confidence
- **Buy Signal**: Predicted probability > Market price + min_edge
- **Avoid Signal**: Confidence < 70%
- **Position Sizing**: Kelly Criterion (max 10% of bankroll)

## Supported Leagues

- NBA (30 teams)
- NFL (32 teams)
- More leagues coming soon

## Requirements

This skill requires the following environment variables:

- `GROK_API_KEY`: For data collection from X/Twitter and sports sources
- `ANTHROPIC_API_KEY`: For Claude Opus analysis
- `THE_ODDS_API_KEY` (optional): For DraftKings odds comparison

## Implementation

The skill is implemented using:
- `SportsAnalyst` from `src/agent/sports_analyst.rs`
- `SportsDataFetcher` from `src/agent/sports_data.rs`
- `PolymarketSportsClient` from `src/agent/polymarket_sports.rs`

## Error Handling

The skill gracefully handles:
- Invalid URLs
- Missing environment variables
- API failures (falls back to partial analysis)
- Market not found on Polymarket
- Data collection failures

## Performance

- Typical analysis time: 30-60 seconds
- Data collection: 7 parallel API calls
- Claude Opus timeout: 5 minutes
- Caches market data for 30 seconds

## Related Skills

- `sports-live`: List all live games today
- `sports-markets`: Browse available sports markets
- `sports-history`: View past betting performance

## Notes

- Analysis uses Claude Opus for complex reasoning
- Grok provides real-time data from X/Twitter
- Automatically handles URL format variations
- Can detect arbitrage opportunities across platforms
- Includes data quality metrics in output
