//! factor_scan — regime-aware factor IC scanner
//!
//! Usage:
//!   cargo run -p ploy-research --example factor_scan -- \
//!     --symbols BTC,ETH --days 7 --db-url postgres://...
//!
//! This is a minimal entry point. Wire up the DB loader from
//! factor_research.rs to populate the registry with real data.

use ploy_research::FactorRegistry;

fn main() {
    let registry = FactorRegistry::new();
    eprintln!("factor_scan: registry ready, {} factors loaded", registry.all().len());
    eprintln!("Next step: wire up DB loader from factor_research.rs to populate registry.");
    eprintln!("See: crates/ploy-research/examples/factor_research.rs for the loader pattern.");
}
