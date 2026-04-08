use crate::client::ControlPlaneClient;
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use ploy_operator_contracts::{TradingStateSnapshot, compute_oversight_report};
use ploy_research::run_backtest as replay_backtest;
use ploy_strategy_bundles::config::FullConfig;
use ploy_strategy_bundles::feed::{
    load_from_database_with_options, HistoricalFeed, HistoricalLoadOptions,
};
use ploy_strategy_bundles::strategies::directional::DirectionalStrategy;
use ploy_strategy_bundles::{
    MarketUpdate, NullRecorder, RuntimeMode, SimulatedExecutor, StrategyRuntime,
};
use ploy_trading::{FillRecord, TradeSide};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use sqlx::postgres::PgPoolOptions;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacktestRequest {
    pub config_path: String,
    pub db_url: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
}

pub fn render_replay(client: &ControlPlaneClient, deployment_id: &str) -> Result<String, String> {
    let state = client.inspect_trading_state(deployment_id)?;
    let fills = fill_records_from_snapshot(&state)?;
    let report = replay_backtest(&fills);
    Ok(format!(
        "deployment={} mode=replay fills={} realized_pnl={} fees={} net_pnl={}",
        state.deployment_id,
        report.fill_count,
        report.pnl.realized_pnl,
        report.pnl.total_fees,
        report.pnl.net_pnl()
    ))
}

pub fn run_backtest(request: &BacktestRequest) -> Result<String, String> {
    let config = FullConfig::from_file(&request.config_path)
        .map_err(|err| format!("load config {}: {err}", request.config_path))?;
    let sim_config = config.sim_executor_config();
    let mut runtime_config = config.runtime_config();
    runtime_config.mode = RuntimeMode::Backtest;
    let backtest_options = HistoricalLoadOptions {
        include_reference_prices: config.backtest_data.include_reference_prices,
        reference_symbols: config
            .backtest_data
            .reference_symbols(&config.reference_data),
        include_sports_state: config.backtest_data.include_sports_state,
    };

    let data_source = if request.db_url.is_some() {
        "database"
    } else {
        "synthetic"
    };

    let data = if let Some(db_url) = &request.db_url {
        load_backtest_data_from_db(
            db_url,
            &config,
            &backtest_options,
            request.start_date.as_deref(),
            request.end_date.as_deref(),
        )?
    } else {
        generate_synthetic_data(&config.strategy.symbols, 60)
    };

    let strategy = DirectionalStrategy::new(config.strategy.clone());
    let feed = HistoricalFeed::new(data);
    let executor = SimulatedExecutor::new(sim_config);
    let recorder = Box::new(NullRecorder);
    let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, runtime_config);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("build tokio runtime: {err}"))?;
    let result = rt.block_on(runtime.run());
    let snapshot = runtime.trading().snapshot(&BTreeMap::new());
    let trade_count = result.fills_recorded / 2;

    Ok(format!(
        "config={} source={} updates={} intents={} fills={} trades={} realized_pnl={} fees={} net_pnl={} elapsed_secs={:.2}",
        request.config_path,
        data_source,
        result.updates_processed,
        result.intents_submitted,
        result.fills_recorded,
        trade_count,
        result.pnl.realized_pnl,
        result.pnl.total_fees,
        snapshot.fill_cashflow_summary().net_pnl(),
        result.elapsed_secs,
    ))
}

pub fn compare_configs(left_path: &Path, right_path: &Path) -> Result<String, String> {
    let left = read_toml_file(left_path)?;
    let right = read_toml_file(right_path)?;
    let mut diffs = Vec::new();
    diff_toml_values("", Some(&left), Some(&right), &mut diffs);

    if diffs.is_empty() {
        return Ok("no config differences detected".to_string());
    }

    Ok(diffs.join("\n"))
}

pub fn render_oversight(client: &ControlPlaneClient) -> Result<String, String> {
    let system = client.system_snapshot()?;
    let deployments = client.deployment_summaries()?;
    let trading = client.trading_state()?;
    let report = compute_oversight_report(&system, &deployments, &trading);

    serde_json::to_string_pretty(&report)
        .map_err(|err| format!("serialize oversight report: {err}"))
}

fn fill_records_from_snapshot(state: &TradingStateSnapshot) -> Result<Vec<FillRecord>, String> {
    state
        .fills
        .iter()
        .map(|fill| {
            let side = match fill.side.as_str() {
                "buy" => TradeSide::Buy,
                "sell" => TradeSide::Sell,
                other => {
                    return Err(format!(
                        "unsupported fill side `{other}` in deployment `{}`",
                        state.deployment_id
                    ));
                }
            };
            Ok(FillRecord {
                fill_id: fill.fill_id.clone(),
                order_id: fill.order_id.clone(),
                token_id: fill.token_id.clone(),
                side,
                quantity: fill.quantity,
                price: fill.price,
                fee: fill.fee,
                timestamp: fill.timestamp,
            })
        })
        .collect()
}

fn load_backtest_data_from_db(
    db_url: &str,
    config: &FullConfig,
    options: &HistoricalLoadOptions,
    start_date: Option<&str>,
    end_date: Option<&str>,
) -> Result<Vec<MarketUpdate>, String> {
    let (from_dt, to_dt) = match (start_date, end_date) {
        (Some(from), Some(to)) => (parse_date_start(from)?, parse_date_end(to)?),
        _ => config.backtest_time_range().ok_or_else(|| {
            "database backtest requires --start-date/--end-date or [runtime].from/[runtime].to"
                .to_string()
        })?,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("build tokio runtime: {err}"))?;
    let pool = rt
        .block_on(PgPoolOptions::new().max_connections(5).connect(db_url))
        .map_err(|err| format!("connect database: {err}"))?;
    rt.block_on(load_from_database_with_options(
        &pool,
        &config.strategy.symbols,
        from_dt,
        to_dt,
        options,
    ))
    .map_err(|err| format!("load market updates: {err}"))
}

fn parse_date_start(value: &str) -> Result<chrono::DateTime<Utc>, String> {
    Ok(Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| format!("invalid date `{value}` (use YYYY-MM-DD)"))?
            .and_hms_opt(0, 0, 0)
            .expect("valid midnight"),
    ))
}

fn parse_date_end(value: &str) -> Result<chrono::DateTime<Utc>, String> {
    Ok(Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| format!("invalid date `{value}` (use YYYY-MM-DD)"))?
            .and_hms_opt(23, 59, 59)
            .expect("valid end of day"),
    ))
}

fn generate_synthetic_data(symbols: &[String], duration_mins: u64) -> Vec<MarketUpdate> {
    let mut updates = Vec::new();
    let start = Utc::now() - Duration::minutes(duration_mins as i64);
    let window_secs = 300u64;
    let base_prices = [
        dec!(100000),
        dec!(3500),
        dec!(140),
        dec!(2.25),
        dec!(0.18),
        dec!(40),
        dec!(600),
    ];

    for window_idx in 0..(duration_mins * 60 / window_secs) {
        let window_start = start + Duration::seconds((window_idx * window_secs) as i64);
        let window_end = window_start + Duration::seconds(window_secs as i64);

        for (sym_idx, symbol) in symbols.iter().enumerate() {
            let base = base_prices[sym_idx % base_prices.len()];
            let event_id = format!("evt-{}-{}", symbol.to_lowercase(), window_idx);
            let up_token = format!("up-{}-{}", symbol.to_lowercase(), window_idx);
            let down_token = format!("dn-{}-{}", symbol.to_lowercase(), window_idx);

            updates.push(MarketUpdate::SpotPrice {
                symbol: symbol.clone(),
                price: base,
                ts: window_start,
            });

            updates.push(MarketUpdate::EventDiscovered {
                event_id: event_id.clone(),
                symbol: symbol.clone(),
                up_token: up_token.clone(),
                down_token: down_token.clone(),
                end_time: window_end,
                window_secs,
                price_to_beat: None,
                resolved_up_won: None,
            });

            let drift: Decimal = if window_idx % 3 == 0 {
                base * dec!(0.015)
            } else if window_idx % 3 == 1 {
                -(base * dec!(0.012))
            } else {
                base * dec!(0.001)
            };

            let up_ask = if drift > Decimal::ZERO {
                dec!(0.55)
            } else if drift < -(base * dec!(0.005)) {
                dec!(0.30)
            } else {
                dec!(0.50)
            };

            updates.push(MarketUpdate::Quote {
                token_id: up_token.clone(),
                bid: Some(up_ask - dec!(0.01)),
                ask: Some(up_ask),
                ts: window_start + Duration::seconds(5),
            });
            updates.push(MarketUpdate::Quote {
                token_id: down_token.clone(),
                bid: Some(dec!(1) - up_ask - dec!(0.01)),
                ask: Some(dec!(1) - up_ask),
                ts: window_start + Duration::seconds(5),
            });

            for tick in 1..=5 {
                let ts = window_start + Duration::seconds(tick * 10);
                let pct = Decimal::from(tick) / dec!(5);
                let price = base + drift * pct;
                updates.push(MarketUpdate::SpotPrice {
                    symbol: symbol.clone(),
                    price,
                    ts,
                });
            }

            let final_price = base + drift;
            let up_ask_final = if drift.abs() > base * dec!(0.005) {
                if drift > Decimal::ZERO {
                    dec!(0.70)
                } else {
                    dec!(0.20)
                }
            } else {
                dec!(0.50)
            };

            updates.push(MarketUpdate::Quote {
                token_id: up_token,
                bid: Some(up_ask_final - dec!(0.01)),
                ask: Some(up_ask_final),
                ts: window_start + Duration::seconds(55),
            });
            updates.push(MarketUpdate::Quote {
                token_id: down_token,
                bid: Some(dec!(1) - up_ask_final - dec!(0.01)),
                ask: Some(dec!(1) - up_ask_final),
                ts: window_start + Duration::seconds(55),
            });

            updates.push(MarketUpdate::SpotPrice {
                symbol: symbol.clone(),
                price: final_price,
                ts: window_start + Duration::seconds(120),
            });

            updates.push(MarketUpdate::EventExpired {
                event_id,
                end_time: window_end,
                resolved_up_won: None,
            });
        }
    }

    updates.sort_by_key(|update| match update {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::SportsState { ts, .. }
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered { end_time, .. } => *end_time - Duration::seconds(300),
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    });

    updates
}

fn read_toml_file(path: &Path) -> Result<toml::Value, String> {
    let content =
        fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))?;
    content
        .parse::<toml::Value>()
        .map_err(|err| format!("parse {}: {err}", path.display()))
}

fn diff_toml_values(
    path: &str,
    left: Option<&toml::Value>,
    right: Option<&toml::Value>,
    diffs: &mut Vec<String>,
) {
    match (left, right) {
        (Some(toml::Value::Table(left_table)), Some(toml::Value::Table(right_table))) => {
            let keys: BTreeSet<_> = left_table
                .keys()
                .chain(right_table.keys())
                .cloned()
                .collect();
            for key in keys {
                let next_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                diff_toml_values(
                    &next_path,
                    left_table.get(&key),
                    right_table.get(&key),
                    diffs,
                );
            }
        }
        (Some(left_value), Some(right_value)) if left_value != right_value => {
            diffs.push(format!(
                "changed {} left={} right={}",
                path,
                render_toml_value(left_value),
                render_toml_value(right_value)
            ));
        }
        (None, Some(right_value)) => {
            diffs.push(format!(
                "added {} right={}",
                path,
                render_toml_value(right_value)
            ));
        }
        (Some(left_value), None) => {
            diffs.push(format!(
                "removed {} left={}",
                path,
                render_toml_value(left_value)
            ));
        }
        _ => {}
    }
}

fn render_toml_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => format!("{text:?}"),
        _ => value.to_string(),
    }
}

pub fn default_config_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/strategies/02-pm5d.unified.toml")
}

#[cfg(test)]
mod tests {
    use super::{
        compare_configs, default_config_path, render_oversight, render_replay, run_backtest,
        BacktestRequest,
    };
    use crate::client::ControlPlaneClient;
    use chrono::Utc;
    use ploy_operator_contracts::{
        DeploymentState, DeploymentSummary, DesiredState, FillSnapshot, ObservedState,
        PnlSnapshotResponse, RiskSnapshotResponse, SystemStatus, TradingStateSnapshot,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use serde_json::Value;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-research-{label}-{unique}"))
    }

    #[test]
    fn replay_renders_summary_from_trading_snapshot() {
        let runtime_root = temp_dir("replay");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                fills: vec![
                    FillSnapshot {
                        fill_id: "fill-1".to_string(),
                        order_id: "order-1".to_string(),
                        token_id: "yes-token".to_string(),
                        side: "buy".to_string(),
                        quantity: dec!(2),
                        price: dec!(0.40),
                        fee: dec!(0.05),
                        timestamp: Utc::now(),
                    },
                    FillSnapshot {
                        fill_id: "fill-2".to_string(),
                        order_id: "order-1".to_string(),
                        token_id: "yes-token".to_string(),
                        side: "sell".to_string(),
                        quantity: dec!(2),
                        price: dec!(0.70),
                        fee: dec!(0.05),
                        timestamp: Utc::now(),
                    },
                ],
                ..TradingStateSnapshot::default()
            }])
            .expect("serialize trading state"),
        )
        .expect("write trading state");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_replay(&client, "example.paper").expect("replay output");
        assert!(output.contains("deployment=example.paper"));
        assert!(output.contains("fills=2"));
        assert!(output.contains("net_pnl="));
    }

    #[test]
    fn compare_reports_changed_added_and_removed_keys() {
        let dir = temp_dir("compare");
        fs::create_dir_all(&dir).expect("create compare dir");
        let left = dir.join("left.toml");
        let right = dir.join("right.toml");
        fs::write(
            &left,
            r#"
[runtime]
mode = "backtest"
from = "2026-04-01T00:00:00Z"

[strategy]
min_edge = 0.05
symbols = ["BTCUSDT"]
"#,
        )
        .expect("write left");
        fs::write(
            &right,
            r#"
[runtime]
mode = "backtest"
to = "2026-04-03T23:59:59Z"

[strategy]
min_edge = 0.07
symbols = ["BTCUSDT", "ETHUSDT"]
"#,
        )
        .expect("write right");

        let output = compare_configs(&left, &right).expect("compare output");
        assert!(output.contains("changed strategy.min_edge"));
        assert!(output.contains("removed runtime.from"));
        assert!(output.contains("added runtime.to"));
    }

    #[test]
    fn synthetic_backtest_runs_from_config() {
        let output = run_backtest(&BacktestRequest {
            config_path: default_config_path().display().to_string(),
            db_url: None,
            start_date: None,
            end_date: None,
        })
        .expect("synthetic backtest output");

        assert!(output.contains("source=synthetic"));
        assert!(output.contains("updates="));
        assert!(output.contains("net_pnl="));
    }

    #[test]
    fn oversight_report_flags_state_and_risk_anomalies() {
        let runtime_root = temp_dir("oversight");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::to_string(&SystemStatus {
                status: "degraded".to_string(),
                uptime_seconds: 42,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: None,
                websocket_connected: true,
                database_connected: true,
                error_count_1h: 2,
                live_reconcile_failures: 0,
                next_live_reconcile_at: None,
                last_live_reconcile_error: None,
                active_alert_count: 0,
                stale_source_count: 0,
                last_live_reconcile_success_at: None,
            })
            .expect("serialize system status"),
        )
        .expect("write system status");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::to_string(&vec![DeploymentSummary {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: "paper".to_string(),
                account_id: "default".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Degraded,
            }])
            .expect("serialize deployments"),
        )
        .expect("write deployments");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: "paper".to_string(),
                pnl: PnlSnapshotResponse {
                    realized_pnl: Decimal::ZERO,
                    unrealized_pnl: Decimal::ZERO,
                    total_fees: Decimal::ZERO,
                    net_pnl: dec!(-3),
                },
                risk: RiskSnapshotResponse {
                    pending_intents: 4,
                    active_orders: 5,
                    open_positions: 4,
                    gross_exposure: dec!(6),
                    reserved_order_exposure: Decimal::ZERO,
                    total_gross_exposure: dec!(6),
                },
                ..TradingStateSnapshot::default()
            }])
            .expect("serialize trading state"),
        )
        .expect("write trading state");

        let client = ControlPlaneClient::from_runtime_root(&runtime_root);
        let output = render_oversight(&client).expect("oversight output");
        let value: Value = serde_json::from_str(&output).expect("parse oversight json");
        assert_eq!(value["platform_status"], "degraded");
        assert_eq!(value["deployments_reviewed"], 1);
        assert!(value["signal_count"].as_u64().unwrap_or(0) >= 5);
        let signals = value["signals"].as_array().expect("signals array");
        let actions = value["recommended_actions"]
            .as_array()
            .expect("recommended actions array");
        assert!(signals
            .iter()
            .any(|signal| signal["kind"] == "system_errors"));
        assert!(signals
            .iter()
            .any(|signal| signal["kind"] == "state_mismatch"));
        assert!(signals
            .iter()
            .any(|signal| signal["kind"] == "order_buildup"));
        assert!(signals
            .iter()
            .any(|signal| signal["kind"] == "position_buildup"));
        assert!(signals
            .iter()
            .any(|signal| signal["kind"] == "exposure_watch"));
        assert!(signals
            .iter()
            .any(|signal| signal["kind"] == "pnl_regression"));
        assert!(actions.iter().any(|action| action["kind"] == "replay"));
        assert!(actions
            .iter()
            .any(|action| action["kind"] == "compare_configs"));
        assert!(actions
            .iter()
            .any(|action| action["kind"] == "pause_review"));
        assert!(actions.iter().any(|action| action["kind"] == "backtest"));
        assert!(actions.iter().any(|action| {
            action["kind"] == "replay"
                && action["operator_command"] == "ployctl research replay example.paper"
        }));
        assert!(actions.iter().any(|action| {
            action["kind"] == "pause_review"
                && action["operator_command"] == "ployctl trading inspect example.paper"
        }));
        assert!(actions.iter().any(|action| {
            action["kind"] == "compare_configs"
                && action["operator_command"]
                    == "ployctl research compare config/strategies/02-pm5d.unified.toml <other-config>"
        }));
        assert!(actions.iter().any(|action| {
            action["kind"] == "backtest"
                && action["operator_command"]
                    == "ployctl research backtest --config config/strategies/02-pm5d.unified.toml --db-url <DATABASE_URL> --start-date <YYYY-MM-DD> --end-date <YYYY-MM-DD>"
        }));
    }
}
