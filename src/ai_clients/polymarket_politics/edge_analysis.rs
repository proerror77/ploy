use super::PoliticsMarketDetails;
use rust_decimal::Decimal;

/// Edge analysis comparing Polymarket with polling data
#[derive(Debug, Clone)]
pub struct PoliticsEdgeAnalysis {
    pub market: String,
    pub polymarket_yes_prob: Decimal,
    pub polymarket_no_prob: Decimal,
    pub poll_yes_prob: Decimal,
    pub poll_no_prob: Decimal,
    pub yes_edge: Decimal,
    pub no_edge: Decimal,
    pub recommended_side: String,
    pub edge: Decimal,
    pub yes_token_id: String,
    pub no_token_id: String,
}

impl PoliticsEdgeAnalysis {
    pub fn calculate(details: &PoliticsMarketDetails, poll_yes_prob: Decimal) -> Option<Self> {
        let poly_yes = details.yes_price()?;
        let poly_no = details.no_price()?;
        let poll_no = Decimal::ONE - poll_yes_prob;

        let yes_edge = poll_yes_prob - poly_yes;
        let no_edge = poll_no - poly_no;

        let (recommended_side, edge) = if yes_edge > no_edge {
            ("YES".to_string(), yes_edge)
        } else {
            ("NO".to_string(), no_edge)
        };

        Some(Self {
            market: details.market.question.clone().unwrap_or_default(),
            polymarket_yes_prob: poly_yes,
            polymarket_no_prob: poly_no,
            poll_yes_prob,
            poll_no_prob: poll_no,
            yes_edge,
            no_edge,
            recommended_side,
            edge,
            yes_token_id: details.yes_token_id.clone(),
            no_token_id: details.no_token_id.clone(),
        })
    }

    pub fn is_significant(&self) -> bool {
        self.edge > Decimal::from_str_exact("0.05").unwrap_or(Decimal::ZERO)
    }

    pub fn recommended_token(&self) -> &str {
        if self.recommended_side == "YES" {
            &self.yes_token_id
        } else {
            &self.no_token_id
        }
    }

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
