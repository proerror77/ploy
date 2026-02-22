use ploy::adapters::PolymarketClient;
use ploy::error::Result;

pub async fn run_paper_trading(
    symbols: String,
    min_vol_edge: f64,
    min_price_edge: f64,
    log_file: String,
    stats_interval: u64,
) -> Result<()> {
    use ploy::strategy::{run_paper_trading, PaperTradingConfig, VolatilityArbConfig};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    let symbols: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .collect();

    let series_ids: Vec<String> = symbols
        .iter()
        .filter_map(|s| match s.trim_end_matches("USDT") {
            "BTC" => Some("btc-price-series-15m".into()),
            "ETH" => Some("eth-price-series-15m".into()),
            "SOL" => Some("sol-price-series-15m".into()),
            _ => None,
        })
        .collect();

    let mut vol_arb_config = VolatilityArbConfig::default();
    vol_arb_config.min_vol_edge_pct = min_vol_edge / 100.0;
    vol_arb_config.min_price_edge =
        Decimal::from_f64_retain(min_price_edge / 100.0).unwrap_or(dec!(0.02));
    vol_arb_config.symbols = symbols.clone();

    let config = PaperTradingConfig {
        vol_arb_config,
        symbols,
        series_ids,
        kline_update_interval_secs: 60,
        stats_interval_secs: stats_interval,
        log_file: Some(log_file),
    };

    let pm_client = PolymarketClient::new("https://clob.polymarket.com", true)?;
    run_paper_trading(pm_client, Some(config)).await?;

    Ok(())
}
