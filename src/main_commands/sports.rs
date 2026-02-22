use crate::cli;
use crate::enforce_coordinator_only_live;
use crate::OrderExecutor;
use crate::PolymarketClient;
use ploy::cli::legacy::SportsCommands;
use ploy::error::Result;
use tracing::info;

pub(crate) async fn run_sports_command(cmd: &SportsCommands) -> Result<()> {
    use ploy::adapters::polymarket_clob::POLYGON_CHAIN_ID;
    use ploy::signing::Wallet;
    use ploy::strategy::{
        core::SplitArbConfig, run_sports_split_arb, SportsLeague, SportsSplitArbConfig,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    match cmd {
        SportsCommands::SplitArb {
            max_entry,
            target_cost,
            min_profit,
            max_wait,
            shares,
            max_unhedged,
            stop_loss,
            leagues,
            dry_run,
        } => {
            info!("Starting sports split-arb strategy");
            if !*dry_run {
                enforce_coordinator_only_live("ploy sports split-arb")?;
            }

            // Parse leagues
            let league_list: Vec<SportsLeague> = leagues
                .split(',')
                .filter_map(|l| match l.trim().to_uppercase().as_str() {
                    "NBA" => Some(SportsLeague::NBA),
                    "NFL" => Some(SportsLeague::NFL),
                    "MLB" => Some(SportsLeague::MLB),
                    "NHL" => Some(SportsLeague::NHL),
                    "SOCCER" => Some(SportsLeague::Soccer),
                    "UFC" => Some(SportsLeague::UFC),
                    _ => None,
                })
                .collect();

            // Create config
            let config = SportsSplitArbConfig {
                base: SplitArbConfig {
                    max_entry_price: Decimal::from_str(&format!("{:.6}", max_entry / 100.0))
                        .unwrap_or(dec!(0.45)),
                    target_total_cost: Decimal::from_str(&format!("{:.6}", target_cost / 100.0))
                        .unwrap_or(dec!(0.92)),
                    min_profit_margin: Decimal::from_str(&format!("{:.6}", min_profit / 100.0))
                        .unwrap_or(dec!(0.03)),
                    max_hedge_wait_secs: *max_wait,
                    shares_per_trade: *shares,
                    max_unhedged_positions: *max_unhedged,
                    unhedged_stop_loss: Decimal::from_str(&format!("{:.6}", stop_loss / 100.0))
                        .unwrap_or(dec!(0.20)),
                },
                leagues: league_list,
            };

            // Initialize client
            let client = if *dry_run {
                PolymarketClient::new("https://clob.polymarket.com", true)?
            } else {
                let wallet = Wallet::from_env(POLYGON_CHAIN_ID)?;
                PolymarketClient::new_authenticated(
                    "https://clob.polymarket.com",
                    wallet,
                    true, // neg_risk
                )
                .await?
            };

            // Initialize executor with default config
            let executor = OrderExecutor::new(client.clone(), Default::default());

            // Run strategy
            run_sports_split_arb(client, executor, config, *dry_run).await?;
        }
        SportsCommands::Monitor { leagues } => {
            info!("Monitoring sports markets: {}", leagues);
            // TODO: Implement monitoring mode
            println!("Sports monitoring mode not yet implemented");
        }
        SportsCommands::Draftkings {
            sport,
            min_edge,
            all,
        } => {
            use ploy::ai_agents::{Market, OddsProvider, Sport};

            println!("\n\x1b[33m{}\x1b[0m", "═".repeat(63));
            println!("\x1b[33m           DRAFTKINGS ODDS SCANNER\x1b[0m");
            println!("\x1b[33m{}\x1b[0m", "═".repeat(63));

            // Parse sport
            let sport_enum = match sport.to_lowercase().as_str() {
                "nba" => Sport::NBA,
                "nfl" => Sport::NFL,
                "nhl" => Sport::NHL,
                "mlb" => Sport::MLB,
                _ => {
                    println!("\x1b[31mInvalid sport. Use: nba, nfl, nhl, mlb\x1b[0m");
                    return Ok(());
                }
            };

            // Create odds provider
            let provider = match OddsProvider::from_env() {
                Ok(p) => p,
                Err(_e) => {
                    println!("\x1b[31mError: THE_ODDS_API_KEY not configured\x1b[0m");
                    println!("Get a free API key at: https://the-odds-api.com/");
                    return Ok(());
                }
            };

            println!(
                "\nFetching {} odds from DraftKings...\n",
                sport.to_uppercase()
            );

            // Fetch odds
            match provider.get_odds(sport_enum, Market::Moneyline).await {
                Ok(events) => {
                    if events.is_empty() {
                        println!("No upcoming games found for {}", sport.to_uppercase());
                        return Ok(());
                    }

                    println!("Found {} upcoming games:\n", events.len());

                    for event in &events {
                        if let Some(best) = event.best_odds() {
                            let edge_pct = (rust_decimal::Decimal::ONE - best.total_implied)
                                .to_string()
                                .parse::<f64>()
                                .unwrap_or(0.0)
                                * 100.0;

                            // Filter by min_edge unless --all
                            if !*all && edge_pct.abs() < *min_edge {
                                continue;
                            }

                            println!("\x1b[36m{} vs {}\x1b[0m", event.home_team, event.away_team);
                            println!(
                                "  \x1b[32m{}\x1b[0m @ {} ({:.1}%)",
                                event.home_team,
                                format!("{:+.0}", best.home_american_odds),
                                best.home_implied_prob
                                    .to_string()
                                    .parse::<f64>()
                                    .unwrap_or(0.0)
                                    * 100.0
                            );
                            println!(
                                "  \x1b[32m{}\x1b[0m @ {} ({:.1}%)",
                                event.away_team,
                                format!("{:+.0}", best.away_american_odds),
                                best.away_implied_prob
                                    .to_string()
                                    .parse::<f64>()
                                    .unwrap_or(0.0)
                                    * 100.0
                            );

                            if best.has_arbitrage() {
                                println!(
                                    "  \x1b[32m🎯 Arbitrage: {:.2}% profit!\x1b[0m",
                                    best.arbitrage_profit()
                                );
                            }

                            println!();
                        }
                    }
                }
                Err(e) => {
                    println!("\x1b[31mError fetching odds: {}\x1b[0m", e);
                }
            }
        }
        SportsCommands::Analyze { url, team1, team2 } => {
            use ploy::ai_agents::{SportsAnalysisWithDK, SportsAnalyst};

            println!("\n\x1b[33m{}\x1b[0m", "═".repeat(63));
            println!("\x1b[33m        SPORTS ANALYSIS WITH DRAFTKINGS COMPARISON\x1b[0m");
            println!("\x1b[33m{}\x1b[0m", "═".repeat(63));

            // Need either URL or both team names
            if url.is_none() && (team1.is_none() || team2.is_none()) {
                println!("\x1b[31mPlease provide --url or both --team1 and --team2\x1b[0m");
                return Ok(());
            }

            // Create analyst
            let analyst = match SportsAnalyst::from_env() {
                Ok(a) => a,
                Err(e) => {
                    println!("\x1b[31mError: {}\x1b[0m", e);
                    return Ok(());
                }
            };

            // Build URL or use provided
            let event_url = match (&url, &team1, &team2) {
                (Some(u), _, _) => u.clone(),
                (None, Some(t1), Some(t2)) => {
                    // Build URL from team names
                    let t1_slug = t1.to_lowercase().replace(' ', "-");
                    let t2_slug = t2.to_lowercase().replace(' ', "-");
                    format!(
                        "https://polymarket.com/event/nba-{}-vs-{}",
                        t1_slug, t2_slug
                    )
                }
                _ => unreachable!("Validated by earlier check"),
            };

            println!("\nAnalyzing: \x1b[36m{}\x1b[0m\n", event_url);

            // Run analysis with DraftKings
            match analyst.analyze_with_draftkings(&event_url).await {
                Ok(analysis) => {
                    let base = &analysis.base;

                    println!(
                        "\x1b[36mMatchup: {} vs {}\x1b[0m",
                        base.teams.0, base.teams.1
                    );
                    println!();

                    // Claude prediction
                    println!("\x1b[33mClaude Opus Prediction:\x1b[0m");
                    println!(
                        "  {} win: {:.1}%",
                        base.teams.0,
                        base.prediction.team1_win_prob * 100.0
                    );
                    println!(
                        "  {} win: {:.1}%",
                        base.teams.1,
                        base.prediction.team2_win_prob * 100.0
                    );
                    println!("  Confidence: {:.0}%", base.prediction.confidence * 100.0);
                    println!();

                    // DraftKings comparison
                    if let Some(ref dk) = analysis.draftkings {
                        println!("\x1b[33mDraftKings Comparison:\x1b[0m");
                        println!(
                            "  DK {} implied: {:.1}%",
                            dk.home_team,
                            dk.dk_home_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                        println!(
                            "  DK {} implied: {:.1}%",
                            dk.away_team,
                            dk.dk_away_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                        println!(
                            "  Edge on {}: {:.1}%",
                            dk.recommended_side,
                            dk.edge.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
                        );
                        println!();
                    } else {
                        println!("\x1b[33mDraftKings odds not available for this game\x1b[0m");
                        println!();
                    }

                    // Best opportunity
                    let (best_side, best_edge) = analysis.best_edge();
                    println!("\x1b[32mRecommendation:\x1b[0m");
                    println!(
                        "  Best bet: \x1b[32m{}\x1b[0m ({:+.1}% edge)",
                        best_side, best_edge
                    );

                    if analysis.has_arbitrage() {
                        println!("  \x1b[32m🎯 Potential arbitrage detected!\x1b[0m");
                    }
                }
                Err(e) => {
                    println!("\x1b[31mAnalysis failed: {}\x1b[0m", e);
                }
            }
        }
        SportsCommands::Polymarket {
            league,
            search,
            compare_dk,
            min_edge,
            live,
        } => {
            cli::show_polymarket_sports(league, search.as_deref(), *compare_dk, *min_edge, *live)
                .await?;
        }
        SportsCommands::Chain {
            team1,
            team2,
            sport,
            execute,
            amount,
        } => {
            cli::run_sports_chain(team1, team2, sport, *execute, *amount).await?;
        }
        SportsCommands::LiveScan {
            sport,
            min_edge,
            interval,
            spreads,
            moneyline,
            props,
            alert,
        } => {
            cli::run_live_edge_scanner(
                sport, *min_edge, *interval, *spreads, *moneyline, *props, *alert,
            )
            .await?;
        }
    }

    Ok(())
}
