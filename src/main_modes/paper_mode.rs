use ploy::adapters::PolymarketClient;
use ploy::error::Result;

pub async fn run_paper_trading(
    symbols: String,
    min_vol_edge: f64,
    min_price_edge: f64,
    log_file: String,
    stats_interval: u64,
    reverse_profile_url: Option<String>,
    reverse_poll_secs: u64,
    reverse_min_trade_usdc: f64,
    reverse_max_event_usdc: f64,
    reverse_max_total_usdc: f64,
    reverse_target_assets: String,
) -> Result<()> {
    use ploy::strategy::{
        run_paper_trading, run_reverse_engineered_profile_paper, PaperTradingConfig,
        ReverseEngineeredConfig, VolatilityArbConfig,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    if let Some(profile_url) = reverse_profile_url {
        let cfg = ReverseEngineeredConfig {
            profile_url,
            poll_interval_secs: reverse_poll_secs,
            min_trade_usdc: reverse_min_trade_usdc,
            max_event_usdc: reverse_max_event_usdc,
            max_total_usdc: reverse_max_total_usdc,
            target_assets: reverse_target_assets
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
        };
        run_reverse_engineered_profile_paper(cfg).await?;
        return Ok(());
    }

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
