//! Prompt construction for unified NBA Grok trade decisions.

use super::{DecisionTrigger, MomentumDirection, UnifiedDecisionRequest};

/// Build the unified decision prompt with all available context.
pub fn build_unified_prompt(req: &UnifiedDecisionRequest) -> String {
    let mut prompt = format!(
        r#"You are a sports trading analyst. Decide whether to BUY YES shares for the trailing team.

GAME STATE:
- {away} {away_score} vs {home} {home_score} (Q{quarter} {clock})
- Trailing team: {trailing} (down {deficit} pts)
"#,
        away = req.game.away_team,
        home = req.game.home_team,
        away_score = req.game.away_score,
        home_score = req.game.home_score,
        quarter = req.game.quarter,
        clock = req.game.clock,
        trailing = req.trailing_team,
        deficit = req.deficit,
    );

    if let Some(ref comeback) = req.comeback {
        prompt.push_str(&format!(
            r#"
STATISTICAL MODEL:
- Historical comeback rate: {:.1}%
- Adjusted win probability: {:.1}%
- Statistical edge vs market: {:.1}%
"#,
            comeback.comeback_rate * 100.0,
            comeback.adjusted_win_prob * 100.0,
            comeback.statistical_edge * 100.0,
        ));
    } else {
        prompt.push_str("\nSTATISTICAL MODEL: Not available for this trigger.\n");
    }

    if let Some(ref intel) = req.grok_intel {
        let momentum_str = match intel.momentum_direction {
            MomentumDirection::HomeTeamSurge => "Home team surge",
            MomentumDirection::AwayTeamSurge => "Away team surge",
            MomentumDirection::Neutral => "Neutral",
        };

        let injuries_summary = if intel.injury_updates.is_empty() {
            "None detected".to_string()
        } else {
            intel
                .injury_updates
                .iter()
                .map(|inj| format!("{} ({}) — {}", inj.player_name, inj.team_abbrev, inj.status))
                .collect::<Vec<_>>()
                .join("; ")
        };

        let grok_prob_str = intel
            .grok_home_win_prob
            .map(|p| format!("{:.1}%", p * 100.0))
            .unwrap_or_else(|| "N/A".to_string());

        prompt.push_str(&format!(
            r#"
X.COM INTELLIGENCE:
- Momentum: {momentum} — {narrative}
- Injuries since game start: {injuries}
- Home sentiment: {home_sent:.2}, Away sentiment: {away_sent:.2}
- Grok estimated home win prob: {grok_prob}
- Intel confidence: {confidence:.2}
"#,
            momentum = momentum_str,
            narrative = intel.momentum_narrative,
            injuries = injuries_summary,
            home_sent = intel.home_sentiment_score,
            away_sent = intel.away_sentiment_score,
            grok_prob = grok_prob_str,
            confidence = intel.grok_confidence,
        ));
    } else {
        prompt.push_str("\nX.COM INTELLIGENCE: Not yet available (first poll pending).\n");
    }

    let best_bid_str = req
        .market
        .yes_best_bid
        .map(|d| d.to_string())
        .unwrap_or_else(|| "N/A".to_string());
    let best_ask_str = req
        .market
        .yes_best_ask
        .map(|d| d.to_string())
        .unwrap_or_else(|| "N/A".to_string());

    prompt.push_str(&format!(
        r#"
MARKET:
- Current price for {trailing} YES: ${market_price}
- Best bid: ${best_bid}, Best ask: ${best_ask}

RISK METRICS (pre-computed):
- Reward-to-risk ratio: {rr:.1}x (gain ${gain:.2} / risk ${risk:.2})
- Expected value: {ev:+.1}%
- Kelly fraction: {kelly:.1}%
"#,
        trailing = req.trailing_team,
        market_price = req.market.market_price,
        best_bid = best_bid_str,
        best_ask = best_ask_str,
        rr = req.risk_metrics.reward_risk_ratio,
        gain = 1.0
            - req
                .market
                .market_price
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0),
        risk = req
            .market
            .market_price
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0),
        ev = req.risk_metrics.expected_value * 100.0,
        kelly = req.risk_metrics.kelly_fraction * 100.0,
    ));

    if let DecisionTrigger::EspnScaleIn {
        add_number,
        existing_shares,
        existing_cost_usd,
    } = &req.trigger
    {
        prompt.push_str(&format!(
            r#"EXISTING POSITION (scale-in #{add_number}):
- Already holding {existing_shares} shares (cost: ${existing_cost:.2})
- This would be add #{add_number} to the position
- Consider whether adding increases or concentrates risk

"#,
            add_number = add_number,
            existing_shares = existing_shares,
            existing_cost = existing_cost_usd,
        ));
    }

    prompt.push_str(&format!(
        r#"TRIGGER: {trigger}

Decide: should we BUY YES shares on {trailing} winning?

IMPORTANT: Also provide your OWN independent win probability estimate (own_fair_value)
based on your X.com search. If it disagrees with the statistical model by >5%, explain why.

Respond ONLY in JSON:
{{
  "decision": "trade" or "pass",
  "fair_value": 0.0-1.0 (statistical model estimate),
  "own_fair_value": 0.0-1.0 (YOUR independent estimate from X.com intel),
  "edge": fair_value minus market_price,
  "confidence": 0.0-1.0,
  "reasoning": "2-3 sentences",
  "risk_factors": ["factor1", "factor2"]
}}"#,
        trailing = req.trailing_team,
        trigger = req.trigger,
    ));

    prompt
}
