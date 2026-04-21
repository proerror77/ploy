use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueKind {
    Polymarket,
    Kalshi,
    Sportsbook,
}

impl Default for VenueKind {
    fn default() -> Self {
        Self::Polymarket
    }
}
