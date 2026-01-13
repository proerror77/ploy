# Sports Betting Analysis Skill - 完整实现范例

这是一个完整的 skill 实现示例，展示如何将现有的 `SportsAnalyst` 功能封装成可调用的 skill。

## 第一步：在 CLI 中添加 Skill 命令

在 `src/cli/legacy.rs` 的 `SportsCommands` 枚举中添加新命令：

```rust
#[derive(Subcommand, Debug)]
pub enum SportsCommands {
    // ... 现有命令 ...

    /// AI-powered sports betting analysis (Grok + Claude)
    Bet {
        /// Polymarket event URL
        #[arg(short, long)]
        url: String,

        /// Include live game data
        #[arg(long)]
        live: bool,

        /// Compare with DraftKings odds
        #[arg(long)]
        compare_dk: bool,

        /// Minimum edge percentage to show recommendation
        #[arg(long, default_value = "5.0")]
        min_edge: f64,

        /// Output format (text, json)
        #[arg(long, default_value = "text")]
        format: String,
    },
}
```

## 第二步：在 main.rs 中实现 Handler

在 `src/main.rs` 的 `run_sports_command()` 函数中添加处理逻辑：

```rust
async fn run_sports_command(cmd: &SportsCommands) -> Result<()> {
    match cmd {
        // ... 现有命令处理 ...

        SportsCommands::Bet { url, live, compare_dk, min_edge, format } => {
            run_sports_bet_analysis(url, *live, *compare_dk, *min_edge, format).await?;
        }
    }
    Ok(())
}

/// Run AI-powered sports betting analysis
async fn run_sports_bet_analysis(
    url: &str,
    live: bool,
    compare_dk: bool,
    min_edge: f64,
    format: &str,
) -> Result<()> {
    use ploy::agent::{SportsAnalyst, SportsAnalysisWithDK};
    use rust_decimal::Decimal;

    // Print header
    println!("\n{}", "═".repeat(70));
    println!("{}  AI-POWERED SPORTS BETTING ANALYSIS  {}",
        "\x1b[33m", "\x1b[0m");
    println!("{}", "═".repeat(70));
    println!();

    // Create analyst
    let analyst = match SportsAnalyst::from_env() {
        Ok(a) => a,
        Err(e) => {
            println!("\x1b[31m✗ Error: {}\x1b[0m", e);
            println!("\nRequired environment variables:");
            println!("  - GROK_API_KEY: For data collection");
            println!("  - ANTHROPIC_API_KEY: For Claude Opus analysis");
            return Ok(());
        }
    };

    println!("\x1b[36m→ Analyzing event: {}\x1b[0m\n", url);

    // Run analysis
    let analysis = if compare_dk {
        // With DraftKings comparison
        match analyst.analyze_with_draftkings(url).await {
            Ok(a) => a,
            Err(e) => {
                println!("\x1b[31m✗ Analysis failed: {}\x1b[0m", e);
                return Ok(());
            }
        }
    } else {
        // Basic analysis
        match analyst.analyze_event(url).await {
            Ok(a) => SportsAnalysisWithDK {
                base: a,
                draftkings: None,
            },
            Err(e) => {
                println!("\x1b[31m✗ Analysis failed: {}\x1b[0m", e);
                return Ok(());
            }
        }
    };

    // Output based on format
    match format {
        "json" => print_analysis_json(&analysis),
        _ => print_analysis_text(&analysis, min_edge, live),
    }

    Ok(())
}

/// Print analysis in human-readable format
fn print_analysis_text(
    analysis: &SportsAnalysisWithDK,
    min_edge: f64,
    _live: bool,
) {
    let base = &analysis.base;

    // Game info
    println!("\x1b[1m📊 GAME INFORMATION\x1b[0m");
    println!("─".repeat(70));
    println!("  League: {}", base.league);
    println!("  Teams:  {} vs {}", base.teams.0, base.teams.1);
    println!();

    // Market odds
    println!("\x1b[1m💰 MARKET ODDS (Polymarket)\x1b[0m");
    println!("─".repeat(70));
    println!("  {} YES: {:.3} ({:.1}%)",
        base.teams.0,
        base.market_odds.team1_yes_price,
        base.market_odds.team1_yes_price.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
    );
    println!("  {} YES: {:.3} ({:.1}%)",
        base.teams.1,
        base.market_odds.team2_yes_price.unwrap_or(Decimal::ZERO),
        base.market_odds.team2_yes_price
            .map(|p| p.to_string().parse::<f64>().unwrap_or(0.0) * 100.0)
            .unwrap_or(0.0)
    );
    if let Some(ref spread) = base.market_odds.spread {
        println!("  Spread: {}", spread);
    }
    println!();

    // AI prediction
    println!("\x1b[1m🤖 AI PREDICTION (Claude Opus)\x1b[0m");
    println!("─".repeat(70));
    println!("  {} Win Probability: \x1b[36m{:.1}%\x1b[0m",
        base.teams.0,
        base.prediction.team1_win_prob * 100.0
    );
    println!("  {} Win Probability: \x1b[36m{:.1}%\x1b[0m",
        base.teams.1,
        base.prediction.team2_win_prob * 100.0
    );
    println!("  Confidence: \x1b[33m{:.0}%\x1b[0m",
        base.prediction.confidence * 100.0
    );
    println!();
    println!("  Reasoning:");
    println!("  {}", base.prediction.reasoning);
    println!();

    // Key factors
    if !base.prediction.key_factors.is_empty() {
        println!("  Key Factors:");
        for factor in &base.prediction.key_factors {
            println!("    • {}", factor);
        }
        println!();
    }

    // DraftKings comparison
    if let Some(ref dk) = analysis.draftkings {
        println!("\x1b[1m🎲 DRAFTKINGS COMPARISON\x1b[0m");
        println!("─".repeat(70));
        println!("  DK Odds: {:.3} / {:.3}", dk.dk_odds.home_odds, dk.dk_odds.away_odds);
        println!("  DK Implied: {:.1}% / {:.1}%",
            dk.dk_odds.home_implied_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0,
            dk.dk_odds.away_implied_prob.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
        );
        println!("  Edge: \x1b[36m{:.1}%\x1b[0m",
            dk.edge.to_string().parse::<f64>().unwrap_or(0.0) * 100.0
        );
        println!();
    }

    // Trade recommendation
    println!("\x1b[1m📈 TRADE RECOMMENDATION\x1b[0m");
    println!("─".repeat(70));

    let action_color = match base.recommendation.action {
        ploy::agent::sports_analyst::TradeAction::Buy => "\x1b[32m", // Green
        ploy::agent::sports_analyst::TradeAction::Sell => "\x1b[31m", // Red
        ploy::agent::sports_analyst::TradeAction::Hold => "\x1b[33m", // Yellow
        ploy::agent::sports_analyst::TradeAction::Avoid => "\x1b[90m", // Gray
    };

    println!("  Action: {}{:?}\x1b[0m", action_color, base.recommendation.action);
    println!("  Side: {}", base.recommendation.side);
    println!("  Edge: \x1b[36m{:+.1}%\x1b[0m", base.recommendation.edge);
    println!("  Suggested Size: {:.1}% of bankroll",
        base.recommendation.suggested_size.to_string().parse::<f64>().unwrap_or(0.0)
    );
    println!();
    println!("  Reasoning:");
    println!("  {}", base.recommendation.reasoning);
    println!();

    // Edge warning
    if base.recommendation.edge.abs() < min_edge {
        println!("\x1b[33m⚠ Warning: Edge ({:.1}%) is below minimum threshold ({:.1}%)\x1b[0m",
            base.recommendation.edge, min_edge);
        println!();
    }

    // Best edge across sources
    if analysis.draftkings.is_some() {
        let (source, edge) = analysis.best_edge();
        println!("\x1b[1m🎯 BEST OPPORTUNITY\x1b[0m");
        println!("─".repeat(70));
        println!("  Source: {}", source);
        println!("  Edge: \x1b[36m{:+.1}%\x1b[0m", edge);

        if analysis.has_arbitrage() {
            println!();
            println!("\x1b[32m💎 ARBITRAGE OPPORTUNITY DETECTED!\x1b[0m");
            println!("  Polymarket and DraftKings have opposing signals.");
            println!("  Consider hedging across both platforms.");
        }
        println!();
    }

    // Structured data summary
    if let Some(ref data) = base.structured_data {
        println!("\x1b[1m📋 DATA SOURCES\x1b[0m");
        println!("─".repeat(70));
        println!("  {} Players: {}", base.teams.0, data.team1_players.len());
        println!("  {} Players: {}", base.teams.1, data.team2_players.len());
        println!("  Betting Lines: {} spread, O/U {}",
            data.betting_lines.spread_team,
            data.betting_lines.over_under
        );
        println!("  Expert Pick: {} ({:.0}% confidence)",
            data.sentiment.expert_pick,
            data.sentiment.expert_confidence * 100.0
        );
        println!("  Data Quality: {:.0}% confidence, {} sources",
            data.data_quality.confidence * 100.0,
            data.data_quality.sources_count
        );
        println!();
    }

    println!("{}", "═".repeat(70));
}

/// Print analysis in JSON format
fn print_analysis_json(analysis: &SportsAnalysisWithDK) {
    use serde_json::json;

    let output = json!({
        "game": {
            "league": analysis.base.league,
            "team1": analysis.base.teams.0,
            "team2": analysis.base.teams.1,
        },
        "market_odds": {
            "team1_yes": analysis.base.market_odds.team1_yes_price.to_string(),
            "team1_no": analysis.base.market_odds.team1_no_price.to_string(),
            "team2_yes": analysis.base.market_odds.team2_yes_price.map(|p| p.to_string()),
            "team2_no": analysis.base.market_odds.team2_no_price.map(|p| p.to_string()),
            "spread": analysis.base.market_odds.spread,
        },
        "prediction": {
            "team1_win_prob": analysis.base.prediction.team1_win_prob,
            "team2_win_prob": analysis.base.prediction.team2_win_prob,
            "confidence": analysis.base.prediction.confidence,
            "reasoning": analysis.base.prediction.reasoning,
            "key_factors": analysis.base.prediction.key_factors,
        },
        "recommendation": {
            "action": format!("{:?}", analysis.base.recommendation.action),
            "side": analysis.base.recommendation.side,
            "edge": analysis.base.recommendation.edge,
            "suggested_size": analysis.base.recommendation.suggested_size.to_string(),
            "reasoning": analysis.base.recommendation.reasoning,
        },
        "draftkings": analysis.draftkings.as_ref().map(|dk| json!({
            "edge": dk.edge.to_string(),
            "recommended_side": dk.recommended_side,
            "home_edge": dk.home_edge.to_string(),
            "away_edge": dk.away_edge.to_string(),
        })),
        "best_edge": {
            let (source, edge) = analysis.best_edge();
            json!({
                "source": source,
                "edge": edge,
            })
        },
        "has_arbitrage": analysis.has_arbitrage(),
    });

    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}
```

## 第三步：使用示例

### 基础分析
```bash
ploy sports bet --url "https://polymarket.com/event/nba-phi-dal-2026-01-11"
```

### 包含 DraftKings 对比
```bash
ploy sports bet \
  --url "https://polymarket.com/event/nba-phi-dal-2026-01-11" \
  --compare-dk
```

### JSON 输出
```bash
ploy sports bet \
  --url "https://polymarket.com/event/nba-phi-dal-2026-01-11" \
  --format json
```

### 自定义最小边缘
```bash
ploy sports bet \
  --url "https://polymarket.com/event/nba-phi-dal-2026-01-11" \
  --min-edge 8.0
```

## 输出示例

```
══════════════════════════════════════════════════════════════════════
  AI-POWERED SPORTS BETTING ANALYSIS
══════════════════════════════════════════════════════════════════════

→ Analyzing event: https://polymarket.com/event/nba-phi-dal-2026-01-11

📊 GAME INFORMATION
──────────────────────────────────────────────────────────────────────
  League: NBA
  Teams:  Philadelphia 76ers vs Dallas Mavericks

💰 MARKET ODDS (Polymarket)
──────────────────────────────────────────────────────────────────────
  Philadelphia 76ers YES: 0.450 (45.0%)
  Dallas Mavericks YES: 0.550 (55.0%)

🤖 AI PREDICTION (Claude Opus)
──────────────────────────────────────────────────────────────────────
  Philadelphia 76ers Win Probability: 58.5%
  Dallas Mavericks Win Probability: 41.5%
  Confidence: 78%

  Reasoning:
  Embiid upgraded to probable with strong recent form (32.5 PPG).
  76ers are 8-2 ATS at home. Mavericks on 3rd game of road trip
  with only 1 day rest, historically struggle in this spot.

  Key Factors:
    • Embiid return from injury (32.5/11.2/5.8 last 5 games)
    • Home court advantage (76ers 15-5 at home)
    • Mavericks fatigue factor (3rd road game, 1 day rest)
    • Sharp money on 76ers (52% vs 48% public)

📈 TRADE RECOMMENDATION
──────────────────────────────────────────────────────────────────────
  Action: Buy
  Side: Philadelphia 76ers YES
  Edge: +13.5%
  Suggested Size: 8.2% of bankroll

  Reasoning:
  Predicted 58.5% vs market 45.0% = 13.5% edge

📋 DATA SOURCES
──────────────────────────────────────────────────────────────────────
  Philadelphia 76ers Players: 5
  Dallas Mavericks Players: 5
  Betting Lines: Dallas Mavericks -3.5, O/U 225.5
  Expert Pick: Dallas Mavericks (72% confidence)
  Data Quality: 90% confidence, 7 sources

══════════════════════════════════════════════════════════════════════
```

## 关键特性

1. **彩色输出**: 使用 ANSI 颜色代码提升可读性
2. **错误处理**: 友好的错误消息和环境变量提示
3. **多格式支持**: 文本和 JSON 输出
4. **DraftKings 集成**: 可选的跨平台对比
5. **套利检测**: 自动识别对冲机会
6. **数据透明**: 显示数据来源和质量指标

## 环境变量

```bash
export GROK_API_KEY="your-grok-api-key"
export ANTHROPIC_API_KEY="your-claude-api-key"
export THE_ODDS_API_KEY="your-odds-api-key"  # 可选，用于 DraftKings 对比
```

## 扩展建议

1. **批量分析**: 添加 `--batch` 参数分析多个事件
2. **历史追踪**: 保存分析结果到数据库
3. **实时监控**: 添加 `--watch` 模式持续监控赔率变化
4. **Webhook 通知**: 发现高边缘机会时发送通知
5. **回测模式**: 使用历史数据验证策略表现
