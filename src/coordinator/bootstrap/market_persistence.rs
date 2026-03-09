use super::support::env_decimal;
use super::*;

async fn ensure_clob_trade_ticks_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_trade_ticks (
            id BIGSERIAL PRIMARY KEY,
            domain TEXT,
            condition_id TEXT NOT NULL,
            token_id TEXT NOT NULL,
            side TEXT NOT NULL CHECK (side IN ('BUY','SELL')),
            size NUMERIC(20,10) NOT NULL,
            price NUMERIC(10,6) NOT NULL,
            trade_ts TIMESTAMPTZ NOT NULL,
            trade_ts_unix BIGINT NOT NULL,
            transaction_hash TEXT NOT NULL,
            proxy_wallet TEXT,
            title TEXT,
            slug TEXT,
            outcome TEXT,
            outcome_index INTEGER,
            source TEXT NOT NULL DEFAULT 'polymarket_data_api',
            received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            UNIQUE (transaction_hash, token_id, side, size, price, trade_ts_unix)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_token_time ON clob_trade_ticks(token_id, trade_ts DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_market_time ON clob_trade_ticks(condition_id, trade_ts DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_ticks_time ON clob_trade_ticks(trade_ts DESC)",
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub(super) async fn ensure_clob_trade_alerts_table(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clob_trade_alerts (
            id BIGSERIAL PRIMARY KEY,
            alert_type TEXT NOT NULL CHECK (alert_type IN ('LARGE_TRADE','BURST')),
            domain TEXT,
            condition_id TEXT NOT NULL,
            token_id TEXT NOT NULL,
            side TEXT CHECK (side IN ('BUY','SELL')),
            size NUMERIC(20,10),
            notional NUMERIC(20,10),
            trade_ts TIMESTAMPTZ,
            trade_ts_unix BIGINT,
            transaction_hash TEXT,
            window_start TIMESTAMPTZ,
            window_end TIMESTAMPTZ,
            burst_bucket_unix BIGINT,
            metadata JSONB,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_alerts_time ON clob_trade_alerts(created_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_clob_trade_alerts_token_time ON clob_trade_alerts(token_id, created_at DESC)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_clob_trade_alerts_large_unique
        ON clob_trade_alerts(alert_type, transaction_hash, token_id)
        WHERE alert_type = 'LARGE_TRADE'
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_clob_trade_alerts_burst_unique
        ON clob_trade_alerts(alert_type, token_id, burst_bucket_unix)
        WHERE alert_type = 'BURST'
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

type InsertedTradeTickRow = (
    String,
    String,
    rust_decimal::Decimal,
    rust_decimal::Decimal,
    chrono::DateTime<Utc>,
    i64,
    String,
);

#[derive(Debug, Clone)]
struct TradeAlertConfig {
    min_size: rust_decimal::Decimal,
    min_notional: rust_decimal::Decimal,
    burst_window_secs: i64,
    burst_min_size: rust_decimal::Decimal,
    burst_min_notional: rust_decimal::Decimal,
    burst_min_trades: usize,
    burst_cooldown_secs: i64,
}

impl TradeAlertConfig {
    fn from_env() -> Self {
        let min_size = env_decimal("PM_TRADE_ALERT_MIN_SIZE", rust_decimal::Decimal::ZERO);
        let min_notional = env_decimal("PM_TRADE_ALERT_MIN_NOTIONAL", rust_decimal::Decimal::ZERO);
        let burst_window_secs = env_i64("PM_TRADE_BURST_WINDOW_SECS", 60).max(1);
        let burst_min_size = env_decimal("PM_TRADE_BURST_MIN_SIZE", rust_decimal::Decimal::ZERO);
        let burst_min_notional =
            env_decimal("PM_TRADE_BURST_MIN_NOTIONAL", rust_decimal::Decimal::ZERO);
        let burst_min_trades = env_usize("PM_TRADE_BURST_MIN_TRADES", 0);
        let burst_cooldown_secs = env_i64("PM_TRADE_BURST_COOLDOWN_SECS", burst_window_secs).max(1);

        Self {
            min_size,
            min_notional,
            burst_window_secs,
            burst_min_size,
            burst_min_notional,
            burst_min_trades,
            burst_cooldown_secs,
        }
    }

    fn disabled() -> Self {
        Self {
            min_size: rust_decimal::Decimal::ZERO,
            min_notional: rust_decimal::Decimal::ZERO,
            burst_window_secs: 60,
            burst_min_size: rust_decimal::Decimal::ZERO,
            burst_min_notional: rust_decimal::Decimal::ZERO,
            burst_min_trades: 0,
            burst_cooldown_secs: 60,
        }
    }

    fn enabled(&self) -> bool {
        self.min_size > rust_decimal::Decimal::ZERO
            || self.min_notional > rust_decimal::Decimal::ZERO
            || self.burst_enabled()
    }

    fn burst_enabled(&self) -> bool {
        self.burst_min_size > rust_decimal::Decimal::ZERO
            || self.burst_min_notional > rust_decimal::Decimal::ZERO
    }
}

#[derive(Debug, Default)]
struct TradeAlertState {
    by_token: HashMap<String, TokenBurstState>,
}

#[derive(Debug, Default)]
struct TokenBurstState {
    trades: VecDeque<(i64, rust_decimal::Decimal, rust_decimal::Decimal)>,
    sum_size: rust_decimal::Decimal,
    sum_notional: rust_decimal::Decimal,
    last_burst_bucket_unix: Option<i64>,
}

#[derive(Debug, Clone)]
struct TradeBurstAlert {
    token_id: String,
    condition_id: String,
    window_start_unix: i64,
    window_end_unix: i64,
    burst_bucket_unix: i64,
    sum_size: rust_decimal::Decimal,
    sum_notional: rust_decimal::Decimal,
    n_trades: usize,
}

#[instrument(skip(data_client, pool, last_seen_by_market))]
async fn collect_trades_for_market(
    data_client: &DataApiClient,
    pool: &PgPool,
    condition_id: &str,
    domain: &str,
    page_limit: usize,
    max_pages: usize,
    overlap_secs: i64,
    last_seen_by_market: &tokio::sync::RwLock<HashMap<String, i64>>,
    alert_cfg: TradeAlertConfig,
    alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>>,
) {
    use chrono::TimeZone as _;

    let last_seen_ts = {
        let map = last_seen_by_market.read().await;
        map.get(condition_id).copied()
    };

    let last_seen_ts: i64 = match last_seen_ts {
        Some(ts) => ts,
        None => {
            let seeded = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(trade_ts_unix), 0) FROM clob_trade_ticks WHERE condition_id = $1",
            )
            .bind(condition_id)
            .fetch_one(pool)
            .await
            .unwrap_or(0);

            let seeded = if seeded <= 0 {
                Utc::now().timestamp()
            } else {
                seeded
            };

            let mut map = last_seen_by_market.write().await;
            *map.entry(condition_id.to_string()).or_insert(seeded)
        }
    };
    let target_min_ts = last_seen_ts.saturating_sub(overlap_secs.max(0));

    let mut max_ts_seen: i64 = last_seen_ts;
    let page_limit_i32 = i32::try_from(page_limit).unwrap_or(1000);

    for page in 0..max_pages {
        let offset = page.saturating_mul(page_limit);
        if offset > 10_000 {
            debug!(
                condition_id,
                offset, "stopping data-api trades pagination at offset > 10000 (SDK bound)"
            );
            break;
        }
        let offset_i32 = match i32::try_from(offset) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    offset,
                    "failed to convert pagination offset for data-api trades"
                );
                return;
            }
        };

        let cid_b256: alloy::primitives::B256 = condition_id.parse().unwrap_or_default();
        let req_builder =
            DataTradesRequest::builder().filter(DataMarketFilter::markets([cid_b256]));
        let req_builder = match req_builder.limit(page_limit_i32) {
            Ok(builder) => builder,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    limit = page_limit_i32,
                    "invalid data-api trades limit"
                );
                return;
            }
        };
        let req_builder = match req_builder.offset(offset_i32) {
            Ok(builder) => builder,
            Err(e) => {
                warn!(
                    condition_id,
                    error = %e,
                    offset = offset_i32,
                    "invalid data-api trades offset"
                );
                return;
            }
        };
        let req = req_builder.build();

        let trades =
            match tokio::time::timeout(Duration::from_secs(15), data_client.trades(&req)).await {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    warn!(
                        condition_id,
                        error = %e,
                        "failed to fetch polymarket data-api trades via SDK"
                    );
                    return;
                }
                Err(_) => {
                    warn!(
                        condition_id,
                        "timed out fetching polymarket data-api trades via SDK"
                    );
                    return;
                }
            };

        if trades.is_empty() {
            break;
        }

        let mut min_ts_in_page: i64 = i64::MAX;
        let mut max_ts_in_page: i64 = i64::MIN;
        let mut rows: Vec<&polymarket_client_sdk::data::types::response::Trade> =
            Vec::with_capacity(trades.len());
        for t in &trades {
            min_ts_in_page = min_ts_in_page.min(t.timestamp);
            max_ts_in_page = max_ts_in_page.max(t.timestamp);

            if t.timestamp >= target_min_ts {
                rows.push(t);
            }
        }

        max_ts_seen = max_ts_seen.max(max_ts_in_page);

        if !rows.is_empty() {
            let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
                r#"
                INSERT INTO clob_trade_ticks (
                    domain,
                    condition_id,
                    token_id,
                    side,
                    size,
                    price,
                    trade_ts,
                    trade_ts_unix,
                    transaction_hash,
                    proxy_wallet,
                    title,
                    slug,
                    outcome,
                    outcome_index,
                    source
                )
                "#,
            );

            qb.push_values(rows.into_iter(), |mut b, t| {
                let trade_ts = Utc.timestamp_opt(t.timestamp, 0).single();
                let side = t.side.to_string();
                let proxy_wallet = format!("{:#x}", t.proxy_wallet);
                let cond_id_str = t.condition_id.to_string();
                let asset_str = t.asset.to_string();
                let tx_hash_str = t.transaction_hash.to_string();

                b.push_bind(domain)
                    .push_bind(cond_id_str)
                    .push_bind(asset_str)
                    .push_bind(side)
                    .push_bind(t.size)
                    .push_bind(t.price)
                    .push_bind(trade_ts.unwrap_or_else(Utc::now))
                    .push_bind(t.timestamp)
                    .push_bind(tx_hash_str)
                    .push_bind(proxy_wallet)
                    .push_bind(&t.title)
                    .push_bind(&t.slug)
                    .push_bind(&t.outcome)
                    .push_bind(t.outcome_index)
                    .push_bind("polymarket_data_api");
            });

            if alert_cfg.enabled() {
                qb.push(
                    " ON CONFLICT DO NOTHING RETURNING token_id, side, size, price, trade_ts, trade_ts_unix, transaction_hash",
                );

                match qb
                    .build_query_as::<InsertedTradeTickRow>()
                    .fetch_all(pool)
                    .await
                {
                    Ok(mut inserted) => {
                        if !inserted.is_empty() {
                            inserted.sort_by_key(|r| r.5);
                            maybe_emit_trade_alerts(
                                pool,
                                domain,
                                condition_id,
                                &inserted,
                                &alert_cfg,
                                alert_state.as_ref(),
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        warn!(
                            condition_id,
                            error = %e,
                            "failed to persist polymarket trade ticks (returning)"
                        );
                    }
                }
            } else {
                qb.push(" ON CONFLICT DO NOTHING");

                if let Err(e) = qb.build().execute(pool).await {
                    warn!(
                        condition_id,
                        error = %e,
                        "failed to persist polymarket trade ticks"
                    );
                }
            }
        }

        if min_ts_in_page < target_min_ts {
            break;
        }

        if trades.len() < page_limit {
            break;
        }
    }

    if max_ts_seen > last_seen_ts {
        let mut map = last_seen_by_market.write().await;
        map.insert(condition_id.to_string(), max_ts_seen);
    }
}

#[instrument(skip(pool, inserted, alert_state))]
async fn maybe_emit_trade_alerts(
    pool: &PgPool,
    domain: &str,
    condition_id: &str,
    inserted: &[InsertedTradeTickRow],
    alert_cfg: &TradeAlertConfig,
    alert_state: Option<&Arc<tokio::sync::Mutex<TradeAlertState>>>,
) {
    use rust_decimal::Decimal;

    if inserted.is_empty() || !alert_cfg.enabled() {
        return;
    }

    for (token_id, side, size, price, trade_ts, trade_ts_unix, tx_hash) in inserted {
        let notional: Decimal = *size * *price;
        let size_trigger = alert_cfg.min_size > Decimal::ZERO && *size >= alert_cfg.min_size;
        let notional_trigger =
            alert_cfg.min_notional > Decimal::ZERO && notional >= alert_cfg.min_notional;

        if !(size_trigger || notional_trigger) {
            continue;
        }

        warn!(
            condition_id,
            token_id,
            side,
            size = %size,
            price = %price,
            notional = %notional,
            trade_ts = %trade_ts,
            trade_ts_unix,
            transaction_hash = %tx_hash,
            "large trade tick detected"
        );

        let meta = json!({
            "min_size": alert_cfg.min_size.to_string(),
            "min_notional": alert_cfg.min_notional.to_string(),
        });

        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO clob_trade_alerts (
                alert_type,
                domain,
                condition_id,
                token_id,
                side,
                size,
                notional,
                trade_ts,
                trade_ts_unix,
                transaction_hash,
                metadata
            )
            VALUES (
                'LARGE_TRADE',
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10
            )
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(domain)
        .bind(condition_id)
        .bind(token_id)
        .bind(side)
        .bind(*size)
        .bind(notional)
        .bind(*trade_ts)
        .bind(*trade_ts_unix)
        .bind(tx_hash)
        .bind(sqlx::types::Json(meta))
        .execute(pool)
        .await
        {
            warn!(
                condition_id,
                token_id,
                error = %e,
                "failed to persist large trade alert"
            );
        }
    }

    if !alert_cfg.burst_enabled() {
        return;
    }
    let Some(state) = alert_state else {
        return;
    };

    let mut burst_events: Vec<TradeBurstAlert> = Vec::new();
    {
        let mut guard = state.lock().await;

        for (token_id, _side, size, price, _trade_ts, trade_ts_unix, _tx_hash) in inserted {
            let notional: Decimal = *size * *price;

            let token_state = guard.by_token.entry(token_id.clone()).or_default();
            token_state
                .trades
                .push_back((*trade_ts_unix, *size, notional));
            token_state.sum_size += *size;
            token_state.sum_notional += notional;

            let cutoff = trade_ts_unix.saturating_sub(alert_cfg.burst_window_secs.max(1));
            while let Some((front_ts, front_size, front_notional)) =
                token_state.trades.front().cloned()
            {
                if front_ts < cutoff {
                    token_state.trades.pop_front();
                    token_state.sum_size -= front_size;
                    token_state.sum_notional -= front_notional;
                } else {
                    break;
                }
            }

            let n = token_state.trades.len();
            let enough_trades = alert_cfg.burst_min_trades == 0 || n >= alert_cfg.burst_min_trades;
            if !enough_trades {
                continue;
            }

            let size_trigger = alert_cfg.burst_min_size > Decimal::ZERO
                && token_state.sum_size >= alert_cfg.burst_min_size;
            let notional_trigger = alert_cfg.burst_min_notional > Decimal::ZERO
                && token_state.sum_notional >= alert_cfg.burst_min_notional;

            if !(size_trigger || notional_trigger) {
                continue;
            }

            let bucket_unix =
                (*trade_ts_unix / alert_cfg.burst_cooldown_secs) * alert_cfg.burst_cooldown_secs;
            if token_state.last_burst_bucket_unix == Some(bucket_unix) {
                continue;
            }
            token_state.last_burst_bucket_unix = Some(bucket_unix);

            let window_start_unix = token_state
                .trades
                .front()
                .map(|(ts, _, _)| *ts)
                .unwrap_or(*trade_ts_unix);

            burst_events.push(TradeBurstAlert {
                token_id: token_id.clone(),
                condition_id: condition_id.to_string(),
                window_start_unix,
                window_end_unix: *trade_ts_unix,
                burst_bucket_unix: bucket_unix,
                sum_size: token_state.sum_size,
                sum_notional: token_state.sum_notional,
                n_trades: n,
            });
        }
    }

    if burst_events.is_empty() {
        return;
    }

    use chrono::TimeZone as _;
    for ev in burst_events {
        let window_start_ts = Utc.timestamp_opt(ev.window_start_unix, 0).single();
        let window_end_ts = Utc.timestamp_opt(ev.window_end_unix, 0).single();

        warn!(
            condition_id = %ev.condition_id,
            token_id = %ev.token_id,
            n_trades = ev.n_trades,
            sum_size = %ev.sum_size,
            sum_notional = %ev.sum_notional,
            window_start_unix = ev.window_start_unix,
            window_end_unix = ev.window_end_unix,
            burst_bucket_unix = ev.burst_bucket_unix,
            "trade burst detected"
        );

        let meta = json!({
            "window_secs": alert_cfg.burst_window_secs,
            "min_size": alert_cfg.burst_min_size.to_string(),
            "min_notional": alert_cfg.burst_min_notional.to_string(),
            "min_trades": alert_cfg.burst_min_trades,
        });

        if let Err(e) = sqlx::query(
            r#"
            INSERT INTO clob_trade_alerts (
                alert_type,
                domain,
                condition_id,
                token_id,
                size,
                notional,
                trade_ts,
                trade_ts_unix,
                window_start,
                window_end,
                burst_bucket_unix,
                metadata
            )
            VALUES (
                'BURST',
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11
            )
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(domain)
        .bind(&ev.condition_id)
        .bind(&ev.token_id)
        .bind(ev.sum_size)
        .bind(ev.sum_notional)
        .bind(window_end_ts.unwrap_or_else(Utc::now))
        .bind(ev.window_end_unix)
        .bind(window_start_ts)
        .bind(window_end_ts)
        .bind(ev.burst_bucket_unix)
        .bind(sqlx::types::Json(meta))
        .execute(pool)
        .await
        {
            warn!(
                condition_id = %ev.condition_id,
                token_id = %ev.token_id,
                error = %e,
                "failed to persist trade burst alert"
            );
        }
    }
}

pub(super) fn spawn_polymarket_trade_persistence(
    event_matcher: Arc<EventMatcher>,
    pool: PgPool,
    agent_id: String,
    coins: Vec<String>,
    domain: Domain,
) {
    tokio::spawn(async move {
        let agent_label = agent_id.clone();

        if let Err(e) = ensure_clob_trade_ticks_table(&pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_trade_ticks table; trade persistence disabled"
            );
            return;
        }

        let data_client = Arc::new(DataApiClient::default());
        let poll_secs = env_u64("PM_TRADES_POLL_SECS", 10).max(1);
        let page_limit = env_usize("PM_TRADES_PAGE_LIMIT", 200).clamp(1, 1000);
        let max_pages = env_usize("PM_TRADES_MAX_PAGES", 10).clamp(1, 100);
        let overlap_secs = env_i64("PM_TRADES_OVERLAP_SECS", 120).max(0);
        let max_concurrency = env_usize("PM_TRADES_CONCURRENCY", 4).clamp(1, 32);

        let mut alert_cfg = TradeAlertConfig::from_env();
        let mut alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>> =
            if alert_cfg.burst_enabled() {
                Some(Arc::new(
                    tokio::sync::Mutex::new(TradeAlertState::default()),
                ))
            } else {
                None
            };

        if alert_cfg.enabled() {
            if let Err(e) = ensure_clob_trade_alerts_table(&pool).await {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to ensure clob_trade_alerts table; trade alerting disabled"
                );
                alert_cfg = TradeAlertConfig::disabled();
                alert_state = None;
            }
        }

        let last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let end_grace_secs = env_i64("PM_TRADES_END_GRACE_SECS", 600).max(0);
        let min_remaining_for_collection = env_i64("PM_TRADES_MIN_REMAINING_SECS", 0)
            .max(-86400)
            .min(86400);
        let mut tracked_markets: HashMap<String, i64> = HashMap::new();

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;
            let now_unix = Utc::now().timestamp();
            for coin in &coins {
                let symbol = format!("{}USDT", coin.to_uppercase());
                for ev in event_matcher
                    .get_events_with_min_remaining(&symbol, min_remaining_for_collection)
                    .await
                {
                    let cid = ev.condition_id.trim();
                    if cid.is_empty() {
                        continue;
                    }
                    let expires_at = ev.end_time.timestamp().saturating_add(end_grace_secs);
                    tracked_markets.insert(cid.to_string(), expires_at);
                }
            }

            tracked_markets.retain(|_, expires_at| *expires_at >= now_unix);
            let mut markets: Vec<String> = tracked_markets.keys().cloned().collect();
            markets.sort();
            if markets.is_empty() {
                continue;
            }

            let domain_str = domain.to_string();
            let pool_ref = pool.clone();
            let data_client_ref = data_client.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let data_client = data_client_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            data_client.as_ref(),
                            &pool,
                            &condition_id,
                            &domain,
                            page_limit,
                            max_pages,
                            overlap_secs,
                            &last_seen,
                            alert_cfg,
                            alert_state,
                        )
                        .await;
                    }
                })
                .await;
        }
    });
}

pub(super) fn spawn_polymarket_trade_persistence_from_collector_targets(
    pool: PgPool,
    agent_id: String,
    domain: Domain,
) {
    tokio::spawn(async move {
        let agent_label = agent_id.clone();

        if let Err(e) = ensure_clob_trade_ticks_table(&pool).await {
            warn!(
                agent = agent_label,
                error = %e,
                "failed to ensure clob_trade_ticks table; trade persistence disabled"
            );
            return;
        }

        let data_client = Arc::new(DataApiClient::default());
        let poll_secs = env_u64("PM_TRADES_POLL_SECS", 10).max(1);
        let page_limit = env_usize("PM_TRADES_PAGE_LIMIT", 200).clamp(1, 1000);
        let max_pages = env_usize("PM_TRADES_MAX_PAGES", 10).clamp(1, 100);
        let overlap_secs = env_i64("PM_TRADES_OVERLAP_SECS", 120).max(0);
        let max_concurrency = env_usize("PM_TRADES_CONCURRENCY", 4).clamp(1, 32);
        let targets_limit = env_usize("PM_TRADES_TARGETS_LIMIT", 200).clamp(1, 5000);

        let mut alert_cfg = TradeAlertConfig::from_env();
        let mut alert_state: Option<Arc<tokio::sync::Mutex<TradeAlertState>>> =
            if alert_cfg.burst_enabled() {
                Some(Arc::new(
                    tokio::sync::Mutex::new(TradeAlertState::default()),
                ))
            } else {
                None
            };

        if alert_cfg.enabled() {
            if let Err(e) = ensure_clob_trade_alerts_table(&pool).await {
                warn!(
                    agent = agent_label,
                    error = %e,
                    "failed to ensure clob_trade_alerts table; trade alerting disabled"
                );
                alert_cfg = TradeAlertConfig::disabled();
                alert_state = None;
            }
        }

        let last_seen_by_market: Arc<tokio::sync::RwLock<HashMap<String, i64>>> =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            let markets: Vec<String> = match sqlx::query_scalar::<_, String>(
                r#"
                SELECT DISTINCT metadata->>'condition_id'
                FROM collector_token_targets
                WHERE domain = 'SPORTS_NBA'
                  AND target_date BETWEEN (CURRENT_DATE - 1) AND (CURRENT_DATE + 1)
                  AND (expires_at IS NULL OR expires_at > NOW())
                  AND (metadata ? 'condition_id')
                  AND COALESCE(metadata->>'condition_id','') <> ''
                ORDER BY 1
                LIMIT $1
                "#,
            )
            .bind(targets_limit as i64)
            .fetch_all(&pool)
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        agent = agent_label,
                        error = %e,
                        "failed to query sports trade targets from collector_token_targets"
                    );
                    continue;
                }
            };

            if markets.is_empty() {
                continue;
            }

            let domain_str = domain.to_string();
            let pool_ref = pool.clone();
            let data_client_ref = data_client.clone();
            let last_seen = last_seen_by_market.clone();
            let alert_cfg_ref = alert_cfg.clone();
            let alert_state_ref = alert_state.clone();

            futures_util::stream::iter(markets)
                .for_each_concurrent(max_concurrency, |condition_id| {
                    let pool = pool_ref.clone();
                    let data_client = data_client_ref.clone();
                    let domain = domain_str.clone();
                    let last_seen = last_seen.clone();
                    let alert_cfg = alert_cfg_ref.clone();
                    let alert_state = alert_state_ref.clone();
                    async move {
                        collect_trades_for_market(
                            data_client.as_ref(),
                            &pool,
                            &condition_id,
                            &domain,
                            page_limit,
                            max_pages,
                            overlap_secs,
                            &last_seen,
                            alert_cfg,
                            alert_state,
                        )
                        .await;
                    }
                })
                .await;
        }
    });
}

#[derive(Debug, Default, Clone, Copy)]
struct SettlementRefreshStats {
    targeted_tokens: usize,
    refreshed_markets: usize,
    upserted_rows: usize,
    resolved_markets: usize,
}

pub(super) fn spawn_pm_token_settlement_persistence(
    pm_client: PolymarketClient,
    pool: PgPool,
    agent_id: String,
    collector_domains: Vec<&'static str>,
) {
    tokio::spawn(async move {
        if let Err(e) = ensure_pm_token_settlements_table(&pool).await {
            warn!(
                agent = %agent_id,
                error = %e,
                "failed to ensure pm_token_settlements table; settlement persistence disabled"
            );
            return;
        }

        let poll_secs = env_u64("PM_SETTLEMENT_POLL_SECS", 120).max(10);
        let targets_limit = env_usize("PM_SETTLEMENT_TARGETS_LIMIT", 1000).clamp(1, 10000);
        let unresolved_limit = env_usize("PM_SETTLEMENT_UNRESOLVED_LIMIT", 1000).clamp(1, 10000);
        let lookback_secs = env_i64("PM_SETTLEMENT_TARGET_LOOKBACK_SECS", 86400).max(0);
        let max_tokens_per_cycle =
            env_usize("PM_SETTLEMENT_MAX_TOKENS_PER_CYCLE", 200).clamp(1, 5000);
        let max_concurrency = env_usize("PM_SETTLEMENT_CONCURRENCY", 2).clamp(1, 32);

        let collector_domains_label = collector_domains.join(",");
        let mut tick = tokio::time::interval(Duration::from_secs(poll_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            match refresh_pm_token_settlements_for_domains(
                &pm_client,
                &pool,
                &collector_domains,
                targets_limit,
                unresolved_limit,
                lookback_secs,
                max_tokens_per_cycle,
                max_concurrency,
            )
            .await
            {
                Ok(stats) => {
                    if stats.targeted_tokens > 0
                        && (stats.resolved_markets > 0 || stats.upserted_rows > 0)
                    {
                        info!(
                            agent = %agent_id,
                            collector_domains = %collector_domains_label,
                            targeted_tokens = stats.targeted_tokens,
                            refreshed_markets = stats.refreshed_markets,
                            upserted_rows = stats.upserted_rows,
                            resolved_markets = stats.resolved_markets,
                            "pm settlement persistence cycle complete"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        agent = %agent_id,
                        collector_domains = %collector_domains_label,
                        error = %e,
                        "pm settlement persistence cycle failed"
                    );
                }
            }
        }
    });
}

async fn refresh_pm_token_settlements_for_domains(
    pm_client: &PolymarketClient,
    pool: &PgPool,
    collector_domains: &[&str],
    targets_limit: usize,
    unresolved_limit: usize,
    lookback_secs: i64,
    max_tokens_per_cycle: usize,
    max_concurrency: usize,
) -> Result<SettlementRefreshStats> {
    use std::collections::BTreeSet;

    let mut token_ids: BTreeSet<String> = BTreeSet::new();

    for domain in collector_domains {
        let scoped_targets = sqlx::query_scalar::<_, String>(
            r#"
            SELECT token_id
            FROM collector_token_targets
            WHERE domain = $1
              AND (
                    expires_at IS NULL
                 OR expires_at > NOW() - ($2::bigint * INTERVAL '1 second')
              )
            ORDER BY updated_at DESC
            LIMIT $3
            "#,
        )
        .bind(*domain)
        .bind(lookback_secs)
        .bind(targets_limit as i64)
        .fetch_all(pool)
        .await?;
        for token_id in scoped_targets {
            if !token_id.trim().is_empty() {
                token_ids.insert(token_id);
            }
        }
    }

    let unresolved_targets = sqlx::query_scalar::<_, String>(
        r#"
        SELECT token_id
        FROM pm_token_settlements
        WHERE resolved = FALSE
        ORDER BY fetched_at DESC
        LIMIT $1
        "#,
    )
    .bind(unresolved_limit as i64)
    .fetch_all(pool)
    .await?;
    for token_id in unresolved_targets {
        if !token_id.trim().is_empty() {
            token_ids.insert(token_id);
        }
    }

    let mut token_ids: Vec<String> = token_ids.into_iter().collect();
    if token_ids.is_empty() {
        return Ok(SettlementRefreshStats::default());
    }
    if token_ids.len() > max_tokens_per_cycle {
        token_ids.truncate(max_tokens_per_cycle);
    }

    let seen_conditions: Arc<tokio::sync::Mutex<HashSet<String>>> =
        Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    let stats: Arc<tokio::sync::Mutex<SettlementRefreshStats>> =
        Arc::new(tokio::sync::Mutex::new(SettlementRefreshStats {
            targeted_tokens: token_ids.len(),
            ..SettlementRefreshStats::default()
        }));

    futures_util::stream::iter(token_ids)
        .for_each_concurrent(max_concurrency, |token_id| {
            let seen_conditions = seen_conditions.clone();
            let stats = stats.clone();
            async move {
                let market = match pm_client.get_gamma_market_by_token_id(&token_id).await {
                    Ok(market) => market,
                    Err(e) => {
                        warn!(
                            token_id = %token_id,
                            error = %e,
                            "failed to fetch gamma market for settlement refresh"
                        );
                        return;
                    }
                };

                let condition_key = market
                    .condition_id
                    .map(|b| b.to_string())
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| format!("market:{}", market.id));

                {
                    let mut seen = seen_conditions.lock().await;
                    if !seen.insert(condition_key) {
                        return;
                    }
                }

                match upsert_pm_token_settlement_rows(pool, &market).await {
                    Ok((upserted_rows, resolved_market)) => {
                        let mut guard = stats.lock().await;
                        guard.refreshed_markets += 1;
                        guard.upserted_rows += upserted_rows;
                        if resolved_market {
                            guard.resolved_markets += 1;
                        }
                        drop(guard);

                        if let Err(e) =
                            backfill_pm_market_metadata_from_settlement(pool, &market).await
                        {
                            debug!(
                                market_id = %market.id,
                                error = %e,
                                "pm_market_metadata backfill skipped"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            token_id = %token_id,
                            market_id = %market.id,
                            error = %e,
                            "failed to upsert pm settlement rows"
                        );
                    }
                }
            }
        })
        .await;

    let snapshot = { *stats.lock().await };
    Ok(snapshot)
}

async fn upsert_pm_token_settlement_rows(
    pool: &PgPool,
    market: &polymarket_client_sdk::gamma::types::response::Market,
) -> Result<(usize, bool)> {
    let clob_token_ids: Vec<String> = market
        .clob_token_ids
        .as_ref()
        .map(|ids| ids.iter().map(|id| id.to_string()).collect())
        .unwrap_or_default();
    let outcomes: Vec<String> = market.outcomes.clone().unwrap_or_default();
    let outcome_prices: Vec<String> = market
        .outcome_prices
        .as_ref()
        .map(|ps| ps.iter().map(|d| d.to_string()).collect())
        .unwrap_or_default();

    if clob_token_ids.is_empty() || outcome_prices.is_empty() {
        return Ok((0, false));
    }

    let parsed_prices: Vec<rust_decimal::Decimal> = outcome_prices
        .iter()
        .filter_map(|v| v.parse::<rust_decimal::Decimal>().ok())
        .collect();
    let resolved_market = market.closed.unwrap_or(false) && is_market_resolved(&parsed_prices);
    let resolved_at: Option<chrono::DateTime<Utc>> = resolved_market.then(Utc::now);
    let raw_market = serde_json::to_value(market).unwrap_or_else(|_| serde_json::json!({}));

    let mut upserted_rows = 0usize;
    for (idx, token_id) in clob_token_ids.iter().enumerate() {
        let outcome = outcomes.get(idx).cloned();
        let settled_price = outcome_prices
            .get(idx)
            .and_then(|v| v.parse::<rust_decimal::Decimal>().ok());

        sqlx::query(
            r#"
            INSERT INTO pm_token_settlements (
                token_id,
                condition_id,
                market_id,
                market_slug,
                outcome,
                settled_price,
                resolved,
                resolved_at,
                fetched_at,
                raw_market
            )
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,NOW(),$9)
            ON CONFLICT (token_id) DO UPDATE SET
                condition_id = EXCLUDED.condition_id,
                market_id = EXCLUDED.market_id,
                market_slug = EXCLUDED.market_slug,
                outcome = EXCLUDED.outcome,
                settled_price = EXCLUDED.settled_price,
                resolved = EXCLUDED.resolved,
                resolved_at = COALESCE(pm_token_settlements.resolved_at, EXCLUDED.resolved_at),
                fetched_at = NOW(),
                raw_market = EXCLUDED.raw_market
            "#,
        )
        .bind(token_id)
        .bind(market.condition_id.map(|b| b.to_string()))
        .bind(&market.id)
        .bind(market.slug.as_deref())
        .bind(outcome.as_deref())
        .bind(settled_price)
        .bind(resolved_market)
        .bind(resolved_at)
        .bind(sqlx::types::Json(raw_market.clone()))
        .execute(pool)
        .await?;

        upserted_rows += 1;
    }

    Ok((upserted_rows, resolved_market))
}

async fn backfill_pm_market_metadata_from_settlement(
    pool: &PgPool,
    market: &polymarket_client_sdk::gamma::types::response::Market,
) -> Result<()> {
    let slug = match market.slug.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };

    let raw_market = serde_json::to_value(market).unwrap_or_else(|_| serde_json::json!({}));

    let end_time: Option<chrono::DateTime<Utc>> = market
        .end_date_iso
        .map(|d| d.and_hms_opt(23, 59, 59).unwrap_or_default())
        .map(|dt| chrono::DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
    let start_time: Option<chrono::DateTime<Utc>> = raw_market
        .get("eventStartTime")
        .or_else(|| raw_market.get("startDate"))
        .or_else(|| raw_market.get("start_date"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    let symbol = if slug.starts_with("btc-") {
        Some("BTCUSDT")
    } else if slug.starts_with("eth-") {
        Some("ETHUSDT")
    } else if slug.starts_with("sol-") {
        Some("SOLUSDT")
    } else {
        None
    };

    let horizon = if slug.contains("-5m-") {
        Some("5m")
    } else if slug.contains("-15m-") {
        Some("15m")
    } else if slug.contains("-60m-") {
        Some("60m")
    } else {
        match (start_time, end_time) {
            (Some(s), Some(e)) => {
                let secs = (e - s).num_seconds();
                if secs <= 360 {
                    Some("5m")
                } else if secs <= 1080 {
                    Some("15m")
                } else {
                    Some("60m")
                }
            }
            _ => None,
        }
    };

    let threshold: Option<rust_decimal::Decimal> = market
        .group_item_threshold
        .as_deref()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            let upper: Option<f64> = raw_market
                .get("upperBound")
                .or_else(|| raw_market.get("upper_bound"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
            let lower: Option<f64> = raw_market
                .get("lowerBound")
                .or_else(|| raw_market.get("lower_bound"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok());
            match (upper, lower) {
                (Some(u), Some(l)) => rust_decimal::Decimal::try_from((u + l) / 2.0).ok(),
                _ => None,
            }
        });

    let price_to_beat = match threshold {
        Some(p) if !p.is_zero() => p,
        _ => match (symbol, start_time) {
            (Some(sym), Some(st)) => {
                let row = sqlx::query_scalar::<_, rust_decimal::Decimal>(
                    "SELECT price FROM binance_price_ticks WHERE symbol = $1 AND trade_time <= $2 ORDER BY trade_time DESC LIMIT 1"
                )
                .bind(sym)
                .bind(st)
                .fetch_optional(pool)
                .await
                .unwrap_or(None);
                row.unwrap_or(rust_decimal::Decimal::ZERO)
            }
            _ => rust_decimal::Decimal::ZERO,
        },
    };

    sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = EXCLUDED.price_to_beat,
            start_time    = COALESCE(EXCLUDED.start_time, pm_market_metadata.start_time),
            end_time      = COALESCE(EXCLUDED.end_time, pm_market_metadata.end_time),
            horizon       = COALESCE(EXCLUDED.horizon, pm_market_metadata.horizon),
            symbol        = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
            raw_market    = EXCLUDED.raw_market,
            updated_at    = NOW()
        "#,
    )
    .bind(slug)
    .bind(price_to_beat)
    .bind(start_time)
    .bind(end_time)
    .bind(horizon)
    .bind(symbol)
    .bind(sqlx::types::Json(raw_market))
    .execute(pool)
    .await?;

    Ok(())
}

#[allow(dead_code)]
fn parse_json_array_strings_relaxed(
    input: &str,
) -> std::result::Result<Vec<String>, serde_json::Error> {
    let s = input.trim();
    if s.is_empty() || s == "null" {
        return Ok(Vec::new());
    }

    if let Ok(v) = serde_json::from_str::<Vec<String>>(s) {
        return Ok(v);
    }

    let vals = serde_json::from_str::<Vec<serde_json::Value>>(s)?;
    Ok(vals
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect())
}

fn is_market_resolved(prices: &[rust_decimal::Decimal]) -> bool {
    if prices.is_empty() {
        return false;
    }

    let winners = prices
        .iter()
        .filter(|p| **p >= rust_decimal_macros::dec!(0.99))
        .count();
    let losers = prices
        .iter()
        .filter(|p| **p <= rust_decimal_macros::dec!(0.01))
        .count();

    winners == 1 && losers == prices.len().saturating_sub(1)
}
