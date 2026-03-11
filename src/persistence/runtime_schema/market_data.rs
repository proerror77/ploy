use crate::error::Result;
use sqlx::PgPool;

pub(crate) async fn ensure_clob_quote_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_quote_ticks (
            id BIGSERIAL PRIMARY KEY,
            token_id TEXT NOT NULL,
            side TEXT NOT NULL CHECK (side IN ('UP', 'DOWN')),
            best_bid NUMERIC(10,6),
            best_ask NUMERIC(10,6),
            bid_size NUMERIC(18,8),
            ask_size NUMERIC(18,8),
            source TEXT NOT NULL DEFAULT 'polymarket_ws',
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("ALTER TABLE clob_quote_ticks ADD COLUMN IF NOT EXISTS domain TEXT")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_token_time ON clob_quote_ticks(token_id, received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_time ON clob_quote_ticks(received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_quote_ticks_domain_time ON clob_quote_ticks(domain, received_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_binance_price_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_price_ticks (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            price NUMERIC(20,10) NOT NULL,
            quantity NUMERIC(20,10),
            trade_time TIMESTAMPTZ NOT NULL,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_symbol_time ON binance_price_ticks(symbol, trade_time DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_price_ticks_time ON binance_price_ticks(trade_time DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_binance_lob_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS binance_lob_ticks (
            id BIGSERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            update_id BIGINT,
            best_bid NUMERIC(20,10) NOT NULL,
            best_ask NUMERIC(20,10) NOT NULL,
            mid_price NUMERIC(20,10) NOT NULL,
            spread_bps NUMERIC(12,6) NOT NULL,
            obi_5 NUMERIC(12,8) NOT NULL,
            obi_10 NUMERIC(12,8) NOT NULL,
            bid_volume_5 NUMERIC(20,10) NOT NULL,
            ask_volume_5 NUMERIC(20,10) NOT NULL,
            bids JSONB,
            asks JSONB,
            event_time TIMESTAMPTZ NOT NULL,
            source TEXT NOT NULL DEFAULT 'binance_depth_ws',
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_symbol_time ON binance_lob_ticks(symbol, event_time DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_binance_lob_ticks_time ON binance_lob_ticks(event_time DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_clob_orderbook_snapshots_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_orderbook_snapshots (
            id BIGSERIAL PRIMARY KEY,
            domain TEXT,
            token_id TEXT NOT NULL,
            market TEXT,
            bids JSONB NOT NULL,
            asks JSONB NOT NULL,
            book_timestamp TIMESTAMPTZ,
            hash TEXT,
            source TEXT NOT NULL DEFAULT 'polymarket_ws',
            context JSONB,
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("ALTER TABLE clob_orderbook_snapshots ADD COLUMN IF NOT EXISTS domain TEXT")
        .execute(pool)
        .await?;
    sqlx::query("ALTER TABLE clob_orderbook_snapshots ADD COLUMN IF NOT EXISTS context JSONB")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_token_time ON clob_orderbook_snapshots(token_id, received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_time ON clob_orderbook_snapshots(received_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_orderbook_snapshots_domain_time ON clob_orderbook_snapshots(domain, received_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(crate) async fn ensure_pm_market_metadata_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS pm_market_metadata (
            market_slug TEXT PRIMARY KEY,
            price_to_beat NUMERIC(20,8) NOT NULL,
            start_time TIMESTAMPTZ,
            end_time TIMESTAMPTZ,
            horizon TEXT,
            symbol TEXT,
            raw_market JSONB,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_symbol_horizon ON pm_market_metadata(symbol, horizon)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_end_time ON pm_market_metadata(end_time DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pm_market_metadata_updated_at ON pm_market_metadata(updated_at DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}
