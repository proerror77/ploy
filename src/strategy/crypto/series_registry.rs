#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoSeriesInfo {
    pub series_id: &'static str,
    pub symbol: &'static str,
    pub horizon: &'static str,
    pub window_secs: u64,
}

const CRYPTO_UPDOWN_SERIES: [CryptoSeriesInfo; 8] = [
    CryptoSeriesInfo {
        series_id: "10684",
        symbol: "BTCUSDT",
        horizon: "5m",
        window_secs: 300,
    },
    CryptoSeriesInfo {
        series_id: "10683",
        symbol: "ETHUSDT",
        horizon: "5m",
        window_secs: 300,
    },
    CryptoSeriesInfo {
        series_id: "10686",
        symbol: "SOLUSDT",
        horizon: "5m",
        window_secs: 300,
    },
    CryptoSeriesInfo {
        series_id: "10685",
        symbol: "XRPUSDT",
        horizon: "5m",
        window_secs: 300,
    },
    CryptoSeriesInfo {
        series_id: "10192",
        symbol: "BTCUSDT",
        horizon: "15m",
        window_secs: 900,
    },
    CryptoSeriesInfo {
        series_id: "10191",
        symbol: "ETHUSDT",
        horizon: "15m",
        window_secs: 900,
    },
    CryptoSeriesInfo {
        series_id: "10423",
        symbol: "SOLUSDT",
        horizon: "15m",
        window_secs: 900,
    },
    CryptoSeriesInfo {
        series_id: "10422",
        symbol: "XRPUSDT",
        horizon: "15m",
        window_secs: 900,
    },
];

const KNOWN_BINANCE_SYMBOLS: [&str; 4] = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"];

fn normalize_symbol_or_coin(input: &str) -> Option<&'static str> {
    match input.trim().to_ascii_uppercase().as_str() {
        "BTC" | "BTCUSDT" => Some("BTCUSDT"),
        "ETH" | "ETHUSDT" => Some("ETHUSDT"),
        "SOL" | "SOLUSDT" => Some("SOLUSDT"),
        "XRP" | "XRPUSDT" => Some("XRPUSDT"),
        _ => None,
    }
}

pub fn known_binance_symbols() -> &'static [&'static str] {
    &KNOWN_BINANCE_SYMBOLS
}

pub fn all_updown_series_ids() -> Vec<String> {
    CRYPTO_UPDOWN_SERIES
        .iter()
        .map(|entry| entry.series_id.to_string())
        .collect()
}

pub fn series_ids_for_symbol(symbol_or_coin: &str) -> Vec<String> {
    let Some(symbol) = normalize_symbol_or_coin(symbol_or_coin) else {
        return Vec::new();
    };

    CRYPTO_UPDOWN_SERIES
        .iter()
        .filter(|entry| entry.symbol == symbol)
        .map(|entry| entry.series_id.to_string())
        .collect()
}

pub fn series_info(series_id: &str) -> Option<&'static CryptoSeriesInfo> {
    CRYPTO_UPDOWN_SERIES
        .iter()
        .find(|entry| entry.series_id == series_id)
}

pub fn symbol_and_window_for_series(series_id: &str) -> Option<(&'static str, u64)> {
    series_info(series_id).map(|entry| (entry.symbol, entry.window_secs))
}

pub fn horizon_for_series(series_id: &str) -> &'static str {
    series_info(series_id).map_or("other", |entry| entry.horizon)
}

#[cfg(test)]
mod tests {
    use super::{
        all_updown_series_ids, horizon_for_series, series_ids_for_symbol,
        symbol_and_window_for_series,
    };

    #[test]
    fn all_updown_series_ids_preserves_expected_order() {
        assert_eq!(
            all_updown_series_ids(),
            vec![
                "10684".to_string(),
                "10683".to_string(),
                "10686".to_string(),
                "10685".to_string(),
                "10192".to_string(),
                "10191".to_string(),
                "10423".to_string(),
                "10422".to_string(),
            ]
        );
    }

    #[test]
    fn series_ids_for_symbol_supports_coin_or_symbol_inputs() {
        assert_eq!(
            series_ids_for_symbol("BTC"),
            vec!["10684".to_string(), "10192".to_string()]
        );
        assert_eq!(
            series_ids_for_symbol("ethusdt"),
            vec!["10683".to_string(), "10191".to_string()]
        );
        assert!(series_ids_for_symbol("DOGE").is_empty());
    }

    #[test]
    fn series_lookup_maps_known_ids() {
        assert_eq!(
            symbol_and_window_for_series("10684"),
            Some(("BTCUSDT", 300))
        );
        assert_eq!(
            symbol_and_window_for_series("10422"),
            Some(("XRPUSDT", 900))
        );
        assert_eq!(symbol_and_window_for_series("99999"), None);
        assert_eq!(horizon_for_series("10684"), "5m");
        assert_eq!(horizon_for_series("10192"), "15m");
        assert_eq!(horizon_for_series("99999"), "other");
    }
}
