use rust_decimal::Decimal;
use std::str::FromStr;

use super::SportsMarketDetails;

/// Edge analysis comparing Polymarket with sportsbook odds
#[derive(Debug, Clone)]
pub struct PolymarketEdgeAnalysis {
    pub market: String,
    pub polymarket_yes_prob: Decimal,
    pub polymarket_no_prob: Decimal,
    pub sportsbook_yes_prob: Decimal,
    pub sportsbook_no_prob: Decimal,
    pub yes_edge: Decimal,
    pub no_edge: Decimal,
    pub recommended_side: String,
    pub edge: Decimal,
    pub yes_token_id: String,
    pub no_token_id: String,
}

impl PolymarketEdgeAnalysis {
    /// Calculate edge between Polymarket and sportsbook
    pub fn calculate(details: &SportsMarketDetails, sportsbook_yes_prob: Decimal) -> Option<Self> {
        let poly_yes = details.yes_price()?;
        let poly_no = details.no_price()?;
        let sb_no = Decimal::ONE - sportsbook_yes_prob;

        let yes_edge = sportsbook_yes_prob - poly_yes;
        let no_edge = sb_no - poly_no;

        let (recommended_side, edge) = if yes_edge > no_edge {
            ("YES".to_string(), yes_edge)
        } else {
            ("NO".to_string(), no_edge)
        };

        Some(Self {
            market: details.market.question.clone().unwrap_or_default(),
            polymarket_yes_prob: poly_yes,
            polymarket_no_prob: poly_no,
            sportsbook_yes_prob,
            sportsbook_no_prob: sb_no,
            yes_edge,
            no_edge,
            recommended_side,
            edge,
            yes_token_id: details.yes_token_id.clone(),
            no_token_id: details.no_token_id.clone(),
        })
    }

    /// Check if edge is significant (> 5%)
    pub fn is_significant(&self) -> bool {
        self.edge > Decimal::from_str_exact("0.05").unwrap_or(Decimal::ZERO)
    }

    /// Get recommended token ID for betting
    pub fn recommended_token(&self) -> &str {
        if self.recommended_side == "YES" {
            &self.yes_token_id
        } else {
            &self.no_token_id
        }
    }

    /// Calculate Kelly criterion bet fraction
    pub fn kelly_fraction(&self) -> Decimal {
        if self.edge <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        let odds = if self.recommended_side == "YES" {
            Decimal::ONE / self.polymarket_yes_prob - Decimal::ONE
        } else {
            Decimal::ONE / self.polymarket_no_prob - Decimal::ONE
        };

        if odds > Decimal::ZERO {
            self.edge / odds
        } else {
            Decimal::ZERO
        }
    }
}
