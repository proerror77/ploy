//! Subscription Planner — computes a deduplicated, ref-counted subscription plan
//! across all active strategy deployments.
//!
//! The planner merges `DataFeed` requirements from every strategy into a single
//! `SubscriptionPlan` that the Data Plane can execute.  When deployments change
//! at runtime, `diff()` produces the minimal `PlanDelta` (subscribe / unsubscribe).

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::Domain;
use crate::strategy::traits::DataFeed;

// ---------------------------------------------------------------------------
// Subscription key — uniquely identifies one atomic subscription unit
// ---------------------------------------------------------------------------

/// A source-level subscription key.  Two feeds that resolve to the same key
/// share a single upstream connection / channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubscriptionKey {
    /// Polymarket WS quote stream for a specific token.
    PolymarketQuote { token_id: String },
    /// Binance spot trade stream for a symbol (e.g. "BTCUSDT").
    BinanceSpot { symbol: String },
    /// Binance kline stream for a (symbol, interval) pair.
    BinanceKline { symbol: String, interval: String },
    /// Polymarket series / event metadata refresh.
    PolymarketSeries { series_id: String },
    /// Periodic tick (keyed by interval to dedup identical ticks).
    Tick { interval_ms: u64 },
}

impl fmt::Display for SubscriptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PolymarketQuote { token_id } => write!(f, "pm_quote:{}", token_id),
            Self::BinanceSpot { symbol } => write!(f, "bn_spot:{}", symbol),
            Self::BinanceKline { symbol, interval } => {
                write!(f, "bn_kline:{}:{}", symbol, interval)
            }
            Self::PolymarketSeries { series_id } => write!(f, "pm_series:{}", series_id),
            Self::Tick { interval_ms } => write!(f, "tick:{}ms", interval_ms),
        }
    }
}
// ---------------------------------------------------------------------------
// Consumer — who needs a subscription
// ---------------------------------------------------------------------------

/// Identifies a consumer of a subscription (strategy deployment or agent).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConsumerId(pub String);

impl fmt::Display for ConsumerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<S: Into<String>> From<S> for ConsumerId {
    fn from(s: S) -> Self {
        Self(s.into())
    }
}

// ---------------------------------------------------------------------------
// SubscriptionPlan — the output of the planner
// ---------------------------------------------------------------------------

/// A fully deduplicated subscription plan.
///
/// Each `SubscriptionKey` maps to the set of consumers that need it.
/// The Data Plane subscribes once per key; consumers are ref-counted.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionPlan {
    /// key → set of consumers that require this subscription
    entries: HashMap<SubscriptionKey, HashSet<ConsumerId>>,
    /// consumer → domain (for filtering / isolation)
    consumer_domains: HashMap<ConsumerId, Domain>,
}

impl SubscriptionPlan {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total unique subscription keys.
    pub fn key_count(&self) -> usize {
        self.entries.len()
    }

    /// Total (key, consumer) pairs.
    pub fn ref_count(&self) -> usize {
        self.entries.values().map(|c| c.len()).sum()
    }

    /// All unique Polymarket token IDs in the plan.
    pub fn polymarket_tokens(&self) -> HashSet<&str> {
        self.entries
            .keys()
            .filter_map(|k| match k {
                SubscriptionKey::PolymarketQuote { token_id } => Some(token_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All unique Binance spot symbols in the plan.
    pub fn binance_symbols(&self) -> HashSet<&str> {
        self.entries
            .keys()
            .filter_map(|k| match k {
                SubscriptionKey::BinanceSpot { symbol } => Some(symbol.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All unique Binance kline (symbol, interval) pairs.
    pub fn binance_klines(&self) -> HashSet<(&str, &str)> {
        self.entries
            .keys()
            .filter_map(|k| match k {
                SubscriptionKey::BinanceKline { symbol, interval } => {
                    Some((symbol.as_str(), interval.as_str()))
                }
                _ => None,
            })
            .collect()
    }

    /// All unique Polymarket series IDs.
    pub fn polymarket_series(&self) -> HashSet<&str> {
        self.entries
            .keys()
            .filter_map(|k| match k {
                SubscriptionKey::PolymarketSeries { series_id } => Some(series_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Consumers for a given key.
    pub fn consumers_of(&self, key: &SubscriptionKey) -> Option<&HashSet<ConsumerId>> {
        self.entries.get(key)
    }

    /// Domain of a consumer.
    pub fn consumer_domain(&self, consumer: &ConsumerId) -> Option<&Domain> {
        self.consumer_domains.get(consumer)
    }

    /// Iterate all entries.
    pub fn iter(&self) -> impl Iterator<Item = (&SubscriptionKey, &HashSet<ConsumerId>)> {
        self.entries.iter()
    }
}

// ---------------------------------------------------------------------------
// PlanDelta — diff between two plans
// ---------------------------------------------------------------------------

/// The minimal set of changes to move from one plan to another.
#[derive(Debug, Clone, Default)]
pub struct PlanDelta {
    /// Keys that need to be subscribed (not present in old plan).
    pub subscribe: HashSet<SubscriptionKey>,
    /// Keys that can be unsubscribed (no consumers left).
    pub unsubscribe: HashSet<SubscriptionKey>,
    /// Keys where the consumer set changed but the key remains active.
    pub updated: HashSet<SubscriptionKey>,
}

impl PlanDelta {
    pub fn is_empty(&self) -> bool {
        self.subscribe.is_empty() && self.unsubscribe.is_empty() && self.updated.is_empty()
    }
}

// ---------------------------------------------------------------------------
// SubscriptionPlanner — builds and diffs plans
// ---------------------------------------------------------------------------

/// Builds a deduplicated `SubscriptionPlan` from strategy feed requirements.
pub struct SubscriptionPlanner;

impl SubscriptionPlanner {
    /// Expand a single `DataFeed` into its constituent `SubscriptionKey`s.
    pub fn expand_feed(feed: &DataFeed) -> Vec<SubscriptionKey> {
        match feed {
            DataFeed::PolymarketQuotes { tokens } => tokens
                .iter()
                .map(|t| SubscriptionKey::PolymarketQuote {
                    token_id: t.clone(),
                })
                .collect(),
            DataFeed::BinanceSpot { symbols } => symbols
                .iter()
                .map(|s| SubscriptionKey::BinanceSpot { symbol: s.clone() })
                .collect(),
            DataFeed::BinanceKlines {
                symbols, intervals, ..
            } => symbols
                .iter()
                .flat_map(|s| {
                    intervals
                        .iter()
                        .map(move |i| SubscriptionKey::BinanceKline {
                            symbol: s.clone(),
                            interval: i.clone(),
                        })
                })
                .collect(),
            DataFeed::PolymarketEvents { series_ids } => series_ids
                .iter()
                .map(|s| SubscriptionKey::PolymarketSeries {
                    series_id: s.clone(),
                })
                .collect(),
            DataFeed::Tick { interval_ms } => {
                vec![SubscriptionKey::Tick {
                    interval_ms: *interval_ms,
                }]
            }
        }
    }

    /// Build a plan from a list of (consumer, domain, feeds) tuples.
    ///
    /// Each consumer declares the feeds it needs; the planner merges them
    /// into a single deduplicated plan with ref-counting.
    pub fn build_plan(
        requirements: impl IntoIterator<Item = (ConsumerId, Domain, Vec<DataFeed>)>,
    ) -> SubscriptionPlan {
        let mut plan = SubscriptionPlan::new();
        for (consumer, domain, feeds) in requirements {
            plan.consumer_domains.insert(consumer.clone(), domain);
            for feed in &feeds {
                for key in Self::expand_feed(feed) {
                    plan.entries
                        .entry(key)
                        .or_default()
                        .insert(consumer.clone());
                }
            }
        }
        plan
    }

    /// Compute the delta between an old plan and a new plan.
    pub fn diff(old: &SubscriptionPlan, new: &SubscriptionPlan) -> PlanDelta {
        let old_keys: HashSet<&SubscriptionKey> = old.entries.keys().collect();
        let new_keys: HashSet<&SubscriptionKey> = new.entries.keys().collect();

        let subscribe = new_keys
            .difference(&old_keys)
            .map(|k| (*k).clone())
            .collect();
        let unsubscribe = old_keys
            .difference(&new_keys)
            .map(|k| (*k).clone())
            .collect();
        let updated = old_keys
            .intersection(&new_keys)
            .filter(|k| old.entries.get(**k) != new.entries.get(**k))
            .map(|k| (*k).clone())
            .collect();

        PlanDelta {
            subscribe,
            unsubscribe,
            updated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto_consumer() -> (ConsumerId, Domain, Vec<DataFeed>) {
        (
            ConsumerId::from("momentum-btc-15m"),
            Domain::Crypto,
            vec![
                DataFeed::BinanceSpot {
                    symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()],
                },
                DataFeed::PolymarketQuotes {
                    tokens: vec!["tok-up-1".into(), "tok-down-1".into()],
                },
                DataFeed::Tick { interval_ms: 1000 },
            ],
        )
    }

    fn overlapping_consumer() -> (ConsumerId, Domain, Vec<DataFeed>) {
        (
            ConsumerId::from("split-arb-btc"),
            Domain::Crypto,
            vec![
                DataFeed::BinanceSpot {
                    symbols: vec!["BTCUSDT".into()], // overlaps with momentum
                },
                DataFeed::PolymarketQuotes {
                    tokens: vec!["tok-up-1".into(), "tok-up-2".into()], // partial overlap
                },
            ],
        )
    }

    fn sports_consumer() -> (ConsumerId, Domain, Vec<DataFeed>) {
        (
            ConsumerId::from("nba-comeback"),
            Domain::Sports,
            vec![DataFeed::PolymarketQuotes {
                tokens: vec!["tok-nba-1".into(), "tok-nba-2".into()],
            }],
        )
    }

    fn kline_consumer() -> (ConsumerId, Domain, Vec<DataFeed>) {
        (
            ConsumerId::from("pattern-memory"),
            Domain::Crypto,
            vec![DataFeed::BinanceKlines {
                symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()],
                intervals: vec!["5m".into(), "15m".into()],
                closed_only: true,
            }],
        )
    }

    #[test]
    fn build_plan_deduplicates_overlapping_tokens() {
        let plan = SubscriptionPlanner::build_plan(vec![crypto_consumer(), overlapping_consumer()]);

        // BTCUSDT appears in both consumers but should be one key
        let bn_symbols = plan.binance_symbols();
        assert_eq!(bn_symbols.len(), 2); // BTCUSDT, ETHUSDT
        assert!(bn_symbols.contains("BTCUSDT"));
        assert!(bn_symbols.contains("ETHUSDT"));

        // tok-up-1 appears in both, tok-down-1 in first, tok-up-2 in second = 3 unique
        let pm_tokens = plan.polymarket_tokens();
        assert_eq!(pm_tokens.len(), 3);

        // BTCUSDT key should have 2 consumers
        let btc_key = SubscriptionKey::BinanceSpot {
            symbol: "BTCUSDT".into(),
        };
        let consumers = plan.consumers_of(&btc_key).unwrap();
        assert_eq!(consumers.len(), 2);
        assert!(consumers.contains(&ConsumerId::from("momentum-btc-15m")));
        assert!(consumers.contains(&ConsumerId::from("split-arb-btc")));
    }

    #[test]
    fn build_plan_isolates_domains() {
        let plan = SubscriptionPlanner::build_plan(vec![crypto_consumer(), sports_consumer()]);

        assert_eq!(
            plan.consumer_domain(&ConsumerId::from("momentum-btc-15m")),
            Some(&Domain::Crypto)
        );
        assert_eq!(
            plan.consumer_domain(&ConsumerId::from("nba-comeback")),
            Some(&Domain::Sports)
        );

        // 2 crypto tokens + 2 sports tokens = 4 PM tokens total
        assert_eq!(plan.polymarket_tokens().len(), 4);
    }

    #[test]
    fn build_plan_expands_klines_as_cross_product() {
        let plan = SubscriptionPlanner::build_plan(vec![kline_consumer()]);

        // 2 symbols × 2 intervals = 4 kline keys
        let klines = plan.binance_klines();
        assert_eq!(klines.len(), 4);
        assert!(klines.contains(&("BTCUSDT", "5m")));
        assert!(klines.contains(&("BTCUSDT", "15m")));
        assert!(klines.contains(&("ETHUSDT", "5m")));
        assert!(klines.contains(&("ETHUSDT", "15m")));
    }

    #[test]
    fn diff_detects_subscribe_and_unsubscribe() {
        let old = SubscriptionPlanner::build_plan(vec![crypto_consumer()]);
        let new = SubscriptionPlanner::build_plan(vec![
            overlapping_consumer(), // drops ETHUSDT, tok-down-1; adds tok-up-2
            sports_consumer(),      // entirely new
        ]);

        let delta = SubscriptionPlanner::diff(&old, &new);

        // New keys: tok-up-2, tok-nba-1, tok-nba-2
        assert!(delta.subscribe.contains(&SubscriptionKey::PolymarketQuote {
            token_id: "tok-up-2".into()
        }));
        assert!(delta.subscribe.contains(&SubscriptionKey::PolymarketQuote {
            token_id: "tok-nba-1".into()
        }));

        // Removed keys: ETHUSDT, tok-down-1, Tick(1000)
        assert!(delta.unsubscribe.contains(&SubscriptionKey::BinanceSpot {
            symbol: "ETHUSDT".into()
        }));
        assert!(delta
            .unsubscribe
            .contains(&SubscriptionKey::PolymarketQuote {
                token_id: "tok-down-1".into()
            }));
        assert!(delta
            .unsubscribe
            .contains(&SubscriptionKey::Tick { interval_ms: 1000 }));

        // Updated keys: BTCUSDT (consumer set changed), tok-up-1 (consumer set changed)
        assert!(delta.updated.contains(&SubscriptionKey::BinanceSpot {
            symbol: "BTCUSDT".into()
        }));
        assert!(delta.updated.contains(&SubscriptionKey::PolymarketQuote {
            token_id: "tok-up-1".into()
        }));
    }

    #[test]
    fn diff_empty_plans_produces_empty_delta() {
        let empty = SubscriptionPlan::new();
        let delta = SubscriptionPlanner::diff(&empty, &empty);
        assert!(delta.is_empty());
    }

    #[test]
    fn ref_count_tracks_total_consumer_key_pairs() {
        let plan = SubscriptionPlanner::build_plan(vec![
            crypto_consumer(),      // 2 BN + 2 PM + 1 Tick = 5 refs
            overlapping_consumer(), // 1 BN + 2 PM = 3 refs (but 2 overlap)
        ]);

        // Unique keys: BTCUSDT, ETHUSDT, tok-up-1, tok-down-1, tok-up-2, Tick(1000) = 6
        assert_eq!(plan.key_count(), 6);
        // Total refs: BTCUSDT(2) + ETHUSDT(1) + tok-up-1(2) + tok-down-1(1) + tok-up-2(1) + Tick(1) = 8
        assert_eq!(plan.ref_count(), 8);
    }

    #[test]
    fn expand_feed_handles_empty_inputs() {
        assert!(
            SubscriptionPlanner::expand_feed(&DataFeed::PolymarketQuotes { tokens: vec![] })
                .is_empty()
        );
        assert!(
            SubscriptionPlanner::expand_feed(&DataFeed::BinanceSpot { symbols: vec![] }).is_empty()
        );
        assert!(SubscriptionPlanner::expand_feed(&DataFeed::BinanceKlines {
            symbols: vec![],
            intervals: vec!["1m".into()],
            closed_only: false,
        })
        .is_empty());
    }
}
