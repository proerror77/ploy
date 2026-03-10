use super::*;

mod pm_backfill;

pub(super) use pm_backfill::{backfill_pm_replay_tables, backfill_pm_token_settlements};

/// Seed NBA team comeback stats into the database
pub(super) async fn seed_nba_stats(season: &str, database_url: Option<String>) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::nba_comeback::nba_data_collector::TeamStats;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });

    println!("\n\x1b[36m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!(
        "\x1b[36m║  NBA Team Stats Seeder (season: {:<27})║\x1b[0m",
        season
    );
    println!("\x1b[36m╚══════════════════════════════════════════════════════════════╝\x1b[0m\n");

    let store = PostgresStore::new(&db_url, 5)
        .await
        .context("Failed to connect to database")?;

    // Pre-computed comeback rates for all 30 NBA teams (2025-26 season estimates)
    // Format: (name, abbrev, comeback_5pt, comeback_10pt, comeback_15pt, q4_avg_pts, elo)
    let teams: &[(&str, &str, f64, f64, f64, f64, f64)] = &[
        ("Atlanta Hawks", "ATL", 0.38, 0.18, 0.06, 28.5, 1490.0),
        ("Boston Celtics", "BOS", 0.52, 0.30, 0.14, 30.2, 1620.0),
        ("Brooklyn Nets", "BKN", 0.32, 0.14, 0.04, 27.0, 1430.0),
        ("Charlotte Hornets", "CHA", 0.30, 0.12, 0.03, 26.8, 1420.0),
        ("Chicago Bulls", "CHI", 0.35, 0.16, 0.05, 27.5, 1470.0),
        ("Cleveland Cavaliers", "CLE", 0.48, 0.27, 0.12, 29.8, 1590.0),
        ("Dallas Mavericks", "DAL", 0.44, 0.24, 0.10, 29.2, 1560.0),
        ("Denver Nuggets", "DEN", 0.46, 0.26, 0.11, 29.5, 1580.0),
        ("Detroit Pistons", "DET", 0.28, 0.10, 0.03, 26.2, 1400.0),
        (
            "Golden State Warriors",
            "GSW",
            0.42,
            0.22,
            0.09,
            28.8,
            1540.0,
        ),
        ("Houston Rockets", "HOU", 0.40, 0.20, 0.08, 28.2, 1510.0),
        ("Indiana Pacers", "IND", 0.43, 0.23, 0.10, 29.5, 1530.0),
        ("LA Clippers", "LAC", 0.39, 0.19, 0.07, 28.0, 1500.0),
        ("Los Angeles Lakers", "LAL", 0.41, 0.21, 0.08, 28.5, 1520.0),
        ("Memphis Grizzlies", "MEM", 0.42, 0.22, 0.09, 28.8, 1530.0),
        ("Miami Heat", "MIA", 0.40, 0.20, 0.08, 28.0, 1510.0),
        ("Milwaukee Bucks", "MIL", 0.45, 0.25, 0.11, 29.5, 1570.0),
        (
            "Minnesota Timberwolves",
            "MIN",
            0.46,
            0.26,
            0.11,
            29.2,
            1580.0,
        ),
        (
            "New Orleans Pelicans",
            "NOP",
            0.36,
            0.17,
            0.06,
            27.8,
            1480.0,
        ),
        ("New York Knicks", "NYK", 0.44, 0.24, 0.10, 29.0, 1560.0),
        (
            "Oklahoma City Thunder",
            "OKC",
            0.50,
            0.29,
            0.13,
            30.0,
            1610.0,
        ),
        ("Orlando Magic", "ORL", 0.37, 0.18, 0.07, 27.5, 1490.0),
        ("Philadelphia 76ers", "PHI", 0.41, 0.21, 0.08, 28.5, 1520.0),
        ("Phoenix Suns", "PHX", 0.43, 0.23, 0.09, 29.0, 1540.0),
        (
            "Portland Trail Blazers",
            "POR",
            0.29,
            0.11,
            0.03,
            26.5,
            1410.0,
        ),
        ("Sacramento Kings", "SAC", 0.39, 0.19, 0.07, 28.2, 1500.0),
        ("San Antonio Spurs", "SAS", 0.31, 0.13, 0.04, 26.8, 1430.0),
        ("Toronto Raptors", "TOR", 0.33, 0.15, 0.05, 27.2, 1450.0),
        ("Utah Jazz", "UTA", 0.30, 0.12, 0.03, 26.5, 1420.0),
        ("Washington Wizards", "WAS", 0.27, 0.09, 0.02, 25.8, 1390.0),
    ];

    let mut count = 0;
    for &(name, abbrev, cr5, cr10, cr15, q4_avg, elo) in teams {
        let stats = TeamStats {
            team_name: name.to_string(),
            season: season.to_string(),
            wins: 0,
            losses: 0,
            win_rate: 0.0,
            avg_points: 0.0,
            q1_avg_points: 0.0,
            q2_avg_points: 0.0,
            q3_avg_points: 0.0,
            q4_avg_points: q4_avg,
            comeback_rate_5pt: cr5,
            comeback_rate_10pt: cr10,
            comeback_rate_15pt: cr15,
            elo_rating: Some(elo),
            offensive_rating: None,
            defensive_rating: None,
        };

        store
            .upsert_nba_team_stats(name, abbrev, season, &stats)
            .await
            .context(format!("Failed to upsert {}", abbrev))?;

        println!(
            "  \x1b[32m✓\x1b[0m {} ({}) — 5pt:{:.0}% 10pt:{:.0}% 15pt:{:.0}%",
            name,
            abbrev,
            cr5 * 100.0,
            cr10 * 100.0,
            cr15 * 100.0
        );
        count += 1;
    }

    println!(
        "\n\x1b[32m✓ Seeded {} teams for season {}\x1b[0m\n",
        count, season
    );
    Ok(())
}

/// The standalone NBA comeback runtime has been retired in favor of managed deployments.
pub(super) async fn run_nba_comeback(_config: Option<PathBuf>, _dry_run: bool) -> Result<()> {
    anyhow::bail!(
        "standalone `ploy strategy nba-comeback` runtime is retired; use canonical managed strategy deployments via `ploy platform start`"
    )
}

// ─────────────────────────────────────────────────────────────
// Integrity Check handler
// ─────────────────────────────────────────────────────────────

pub(super) async fn run_integrity_check(
    json_output: bool,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::strategy::integrity::IntegrityChecker;

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });

    let store = PostgresStore::new(&db_url, 5).await?;
    let checker = IntegrityChecker::new(store.pool().clone());
    let report = checker.run_full_check().await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report);
    }

    if !report.healthy {
        std::process::exit(1);
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Backfill handlers
// ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub(super) async fn backfill_klines(
    symbols: &str,
    from: &str,
    to: &str,
    interval: &str,
    database_url: Option<String>,
) -> Result<()> {
    use crate::adapters::PostgresStore;
    use crate::collector::BinanceKlineClient;
    use chrono::DateTime;

    let symbol_list: Vec<String> = symbols.split(',').map(|s| s.trim().to_string()).collect();
    if symbol_list.is_empty() {
        anyhow::bail!("No symbols provided");
    }

    let from_dt = DateTime::parse_from_rfc3339(from)
        .or_else(|_| DateTime::parse_from_str(from, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --from date (expected ISO 8601, e.g. 2026-02-20T00:00:00Z)")?;

    let to_dt = DateTime::parse_from_rfc3339(to)
        .or_else(|_| DateTime::parse_from_str(to, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map(|d| d.with_timezone(&chrono::Utc))
        .context("Invalid --to date (expected ISO 8601, e.g. 2026-02-28T00:00:00Z)")?;

    if to_dt <= from_dt {
        anyhow::bail!("--to must be after --from");
    }

    let db_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/ploy".to_string())
    });
    let store = PostgresStore::new(&db_url, 5).await?;
    let pool = store.pool();

    // Ensure binance_klines table exists
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_klines (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            interval TEXT NOT NULL,
            open_time TIMESTAMPTZ NOT NULL,
            close_time TIMESTAMPTZ NOT NULL,
            open NUMERIC(20,10) NOT NULL,
            high NUMERIC(20,10) NOT NULL,
            low NUMERIC(20,10) NOT NULL,
            close NUMERIC(20,10) NOT NULL,
            volume NUMERIC(20,10) NOT NULL,
            quote_volume NUMERIC(20,10) NOT NULL,
            trades BIGINT NOT NULL DEFAULT 0,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE (symbol, interval, open_time)
        )
        "#,
    )
    .execute(pool)
    .await
    .context("Failed to ensure binance_klines table")?;

    let client = BinanceKlineClient::new();

    println!(
        "\nBackfilling klines: {} symbols, interval={}, {} → {}",
        symbol_list.len(),
        interval,
        from_dt.format("%Y-%m-%d"),
        to_dt.format("%Y-%m-%d")
    );

    let mut grand_total = 0usize;
    for sym in &symbol_list {
        print!("  {} ... ", sym);
        std::io::stdout().flush().ok();

        let klines = client
            .fetch_klines_range(sym, interval, from_dt, to_dt)
            .await
            .with_context(|| format!("Failed to fetch klines for {}", sym))?;

        let fetched = klines.len();
        let saved = BinanceKlineClient::save_klines_to_db(pool, sym, interval, &klines)
            .await
            .with_context(|| format!("Failed to save klines for {}", sym))?;

        println!("{} fetched, {} new", fetched, saved);
        grand_total += saved;
    }

    println!("\nDone. {} new klines inserted total.\n", grand_total);
    Ok(())
}
