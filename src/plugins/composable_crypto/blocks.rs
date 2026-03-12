use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! block_kind {
    ($name:ident, $label:literal, { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                match raw.trim().to_ascii_lowercase().as_str() {
                    $($wire => Ok(Self::$variant),)+
                    other => Err(format!("unknown {} block: {}", $label, other)),
                }
            }
        }
    };
}

block_kind!(SignalBlockKind, "signal", {
    Momentum => "momentum",
    MeanReversion => "mean_reversion",
    SpreadDislocation => "spread_dislocation",
    PatternMemory => "pattern_memory",
    SplitArb => "split_arb",
});

block_kind!(FilterBlockKind, "filter", {
    TimeWindow => "time_window",
    VolatilityGate => "volatility_gate",
    LiquidityGate => "liquidity_gate",
});

block_kind!(EntryBlockKind, "entry", {
    MarketableLimit => "marketable_limit",
    LadderLimit => "ladder_limit",
});

block_kind!(ExitBlockKind, "exit", {
    TrailingStop => "trailing_stop",
    EdgeDecay => "edge_decay",
    TimeStop => "time_stop",
});

block_kind!(SizingBlockKind, "sizing", {
    FixedShares => "fixed_shares",
    FixedUsdRisk => "fixed_usd_risk",
    BudgetFraction => "budget_fraction",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterBlockSpec {
    #[serde(rename = "type")]
    pub kind: FilterBlockKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryBlockSpec {
    #[serde(rename = "type")]
    pub kind: EntryBlockKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitBlockSpec {
    #[serde(rename = "type")]
    pub kind: ExitBlockKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizingBlockSpec {
    #[serde(rename = "type")]
    pub kind: SizingBlockKind,
}
