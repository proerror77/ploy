/// Additive historical-loader flags for non-crypto datasets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalLoadOptions {
    pub include_reference_prices: bool,
    pub reference_symbols: Vec<String>,
    pub include_sports_state: bool,
    pub require_official_settlement: bool,
    /// Downsample `binance_lob_ticks` to one snapshot per N seconds per symbol.
    /// Defaults to 30 (one row per 30-second bucket). Set to 1 to disable downsampling.
    pub lob_sample_secs: u32,
}

impl Default for HistoricalLoadOptions {
    fn default() -> Self {
        Self {
            include_reference_prices: false,
            reference_symbols: Vec::new(),
            include_sports_state: false,
            require_official_settlement: false,
            lob_sample_secs: 30,
        }
    }
}

impl HistoricalLoadOptions {
    #[must_use]
    pub fn normalized_reference_symbols(&self) -> Vec<String> {
        self.reference_symbols
            .iter()
            .map(|symbol| symbol.trim().to_lowercase())
            .filter(|symbol| !symbol.is_empty())
            .collect()
    }
}
