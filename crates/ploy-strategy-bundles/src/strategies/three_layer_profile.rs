//! Runtime profile selector for the PM5D three-layer strategy.

use serde::{Deserialize, Deserializer, Serialize, de};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreeLayerProfile {
    /// Historical runtime behavior: blended CEX continuation and book confirmation.
    Mixed,
    /// Snapshot Strategy A: contrarian alpha plus executable liquidity/risk gates.
    Champion,
    /// Snapshot Strategy B: Champion plus CEX/PM order-book imbalance soft score.
    ObiSoft,
    /// Snapshot Strategy C: Champion plus CEX continuation soft score.
    ContinuationSoft,
}

impl Default for ThreeLayerProfile {
    fn default() -> Self {
        Self::Mixed
    }
}

impl ThreeLayerProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Champion => "champion",
            Self::ObiSoft => "obi_soft",
            Self::ContinuationSoft => "continuation_soft",
        }
    }

    pub fn uses_snapshot_scoring(self) -> bool {
        !matches!(self, Self::Mixed)
    }
}

impl FromStr for ThreeLayerProfile {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "mixed" | "legacy" => Ok(Self::Mixed),
            "champion" | "a" | "alpha" | "alpha_only" | "contrarian_alpha" => Ok(Self::Champion),
            "obi" | "obi_soft" | "b" | "book_imbalance" | "orderbook" => Ok(Self::ObiSoft),
            "continuation" | "continuation_soft" | "c" | "cex_continuation" => {
                Ok(Self::ContinuationSoft)
            }
            other => Err(format!(
                "unknown three_layer_strategy_profile {other:?}; expected mixed, champion, obi_soft, or continuation_soft"
            )),
        }
    }
}

impl fmt::Display for ThreeLayerProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ThreeLayerProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(de::Error::custom)
    }
}
