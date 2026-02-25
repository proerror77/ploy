use crate::domain::{Cycle, DumpSignal, Order, OrderStatus, Round, Side, StrategyState, Tick};
use crate::error::{PloyError, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use tracing::{debug, info, instrument};

/// PostgreSQL storage adapter
#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Create a new PostgreSQL store
    pub async fn new(database_url: &str, max_connections: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;

        info!("Connected to PostgreSQL");
        Ok(Self { pool })
    }

    /// Create a PostgreSQL store from an existing connection pool (zero-cost reuse)
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run migrations
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        info!("Database migrations completed");
        Ok(())
    }

    /// Get the connection pool
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // ==================== Rounds ====================

    /// Insert or update a round
    #[instrument(skip(self))]
    pub async fn upsert_round(&self, round: &Round) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO rounds (slug, up_token_id, down_token_id, start_time, end_time, outcome)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (slug) DO UPDATE SET
                up_token_id = EXCLUDED.up_token_id,
                down_token_id = EXCLUDED.down_token_id,
                start_time = EXCLUDED.start_time,
                end_time = EXCLUDED.end_time,
                outcome = EXCLUDED.outcome
            RETURNING id
            "#,
        )
        .bind(&round.slug)
        .bind(&round.up_token_id)
        .bind(&round.down_token_id)
        .bind(round.start_time)
        .bind(round.end_time)
        .bind(round.outcome.map(|s| s.as_str()))
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Get a round by slug
    pub async fn get_round_by_slug(&self, slug: &str) -> Result<Option<Round>> {
        let row = sqlx::query(
            r#"
            SELECT id, slug, up_token_id, down_token_id, start_time, end_time, outcome
            FROM rounds WHERE slug = $1
            "#,
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Round {
            id: Some(r.get("id")),
            slug: r.get("slug"),
            up_token_id: r.get("up_token_id"),
            down_token_id: r.get("down_token_id"),
            start_time: r.get("start_time"),
            end_time: r.get("end_time"),
            outcome: r
                .get::<Option<String>, _>("outcome")
                .and_then(|s| Side::try_from(s.as_str()).ok()),
        }))
    }

    /// Get active round (current time between start and end)
    pub async fn get_active_round(&self) -> Result<Option<Round>> {
        let row = sqlx::query(
            r#"
            SELECT id, slug, up_token_id, down_token_id, start_time, end_time, outcome
            FROM rounds
            WHERE start_time <= NOW() AND end_time > NOW()
            ORDER BY start_time DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Round {
            id: Some(r.get("id")),
            slug: r.get("slug"),
            up_token_id: r.get("up_token_id"),
            down_token_id: r.get("down_token_id"),
            start_time: r.get("start_time"),
            end_time: r.get("end_time"),
            outcome: r
                .get::<Option<String>, _>("outcome")
                .and_then(|s| Side::try_from(s.as_str()).ok()),
        }))
    }

    // ==================== Ticks ====================

    /// Insert a tick
    pub async fn insert_tick(&self, tick: &Tick) -> Result<i64> {
        let row = sqlx::query(
            r#"
            INSERT INTO ticks (round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING id
            "#,
        )
        .bind(tick.round_id)
        .bind(tick.timestamp)
        .bind(tick.side.as_str())
        .bind(tick.best_bid)
        .bind(tick.best_ask)
        .bind(tick.bid_size)
        .bind(tick.ask_size)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Batch insert ticks
    pub async fn insert_ticks(&self, ticks: &[Tick]) -> Result<()> {
        if ticks.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;

        for tick in ticks {
            sqlx::query(
                r#"
                INSERT INTO ticks (round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(tick.round_id)
            .bind(tick.timestamp)
            .bind(tick.side.as_str())
            .bind(tick.best_bid)
            .bind(tick.best_ask)
            .bind(tick.bid_size)
            .bind(tick.ask_size)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        debug!("Inserted {} ticks", ticks.len());
        Ok(())
    }

    /// Get historical ticks for a round
    pub async fn get_ticks_for_round(&self, round_id: i32) -> Result<Vec<Tick>> {
        let rows = sqlx::query(
            r#"
            SELECT id, round_id, timestamp, side, best_bid, best_ask, bid_size, ask_size
            FROM ticks
            WHERE round_id = $1
            ORDER BY timestamp ASC
            "#,
        )
        .bind(round_id)
        .fetch_all(&self.pool)
        .await?;

        let ticks = rows
            .iter()
            .map(|row| {
                let side_str: String = row.get("side");
                let side = match side_str.to_uppercase().as_str() {
                    "UP" => Side::Up,
                    "DOWN" => Side::Down,
                    _ => Side::Up, // Default to Up if unknown
                };
                Tick {
                    id: Some(row.get("id")),
                    round_id: row.get("round_id"),
                    timestamp: row.get("timestamp"),
                    side,
                    best_bid: row.get("best_bid"),
                    best_ask: row.get("best_ask"),
                    bid_size: row.get("bid_size"),
                    ask_size: row.get("ask_size"),
                }
            })
            .collect();

        Ok(ticks)
    }

    /// Get all rounds with tick data
    pub async fn get_rounds_with_ticks(&self) -> Result<Vec<Round>> {
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT r.id, r.slug, r.up_token_id, r.down_token_id,
                   r.start_time, r.end_time, r.outcome
            FROM rounds r
            INNER JOIN ticks t ON t.round_id = r.id
            ORDER BY r.start_time DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let rounds = rows
            .iter()
            .map(|row| {
                let outcome_str: Option<String> = row.get("outcome");
                let outcome = outcome_str.map(|s| match s.to_uppercase().as_str() {
                    "UP" => Side::Up,
                    "DOWN" => Side::Down,
                    _ => Side::Up,
                });
                Round {
                    id: Some(row.get("id")),
                    slug: row.get("slug"),
                    up_token_id: row.get("up_token_id"),
                    down_token_id: row.get("down_token_id"),
                    start_time: row.get("start_time"),
                    end_time: row.get("end_time"),
                    outcome,
                }
            })
            .collect();

        Ok(rounds)
    }

    /// Get tick count for a round
    pub async fn get_tick_count(&self, round_id: i32) -> Result<i64> {
        let row = sqlx::query(r#"SELECT COUNT(*) as count FROM ticks WHERE round_id = $1"#)
            .bind(round_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get("count"))
    }

    // ==================== Cycles ====================

    /// Create a new cycle
    pub async fn create_cycle(&self, round_id: i32, state: StrategyState) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO cycles (round_id, state)
            VALUES ($1, $2)
            RETURNING id
            "#,
        )
        .bind(round_id)
        .bind(state.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Update cycle state
    pub async fn update_cycle_state(&self, cycle_id: i32, state: StrategyState) -> Result<()> {
        sqlx::query("UPDATE cycles SET state = $1 WHERE id = $2")
            .bind(state.as_str())
            .bind(cycle_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Update cycle with Leg1 fill
    pub async fn update_cycle_leg1(
        &self,
        cycle_id: i32,
        side: Side,
        entry_price: Decimal,
        shares: u64,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE cycles SET
                state = 'LEG1_FILLED',
                leg1_side = $1,
                leg1_entry_price = $2,
                leg1_shares = $3,
                leg1_filled_at = NOW()
            WHERE id = $4
            "#,
        )
        .bind(side.as_str())
        .bind(entry_price)
        .bind(shares as i32)
        .bind(cycle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update cycle with Leg2 fill and PnL
    pub async fn update_cycle_leg2(
        &self,
        cycle_id: i32,
        entry_price: Decimal,
        shares: u64,
        pnl: Decimal,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE cycles SET
                state = 'CYCLE_COMPLETE',
                leg2_entry_price = $1,
                leg2_shares = $2,
                leg2_filled_at = NOW(),
                pnl = $3
            WHERE id = $4
            "#,
        )
        .bind(entry_price)
        .bind(shares as i32)
        .bind(pnl)
        .bind(cycle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Abort a cycle
    pub async fn abort_cycle(&self, cycle_id: i32, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE cycles SET state = 'ABORT', abort_reason = $1 WHERE id = $2
            "#,
        )
        .bind(reason)
        .bind(cycle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Get cycle by ID
    pub async fn get_cycle(&self, cycle_id: i32) -> Result<Option<Cycle>> {
        let row = sqlx::query(
            r#"
            SELECT id, round_id, state, leg1_side, leg1_entry_price, leg1_shares, leg1_filled_at,
                   leg2_entry_price, leg2_shares, leg2_filled_at, pnl, created_at, updated_at
            FROM cycles WHERE id = $1
            "#,
        )
        .bind(cycle_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Cycle {
            id: Some(r.get("id")),
            round_id: r.get("round_id"),
            state: r
                .get::<String, _>("state")
                .as_str()
                .try_into()
                .unwrap_or(StrategyState::Idle),
            leg1_side: r
                .get::<Option<String>, _>("leg1_side")
                .and_then(|s| Side::try_from(s.as_str()).ok()),
            leg1_entry_price: r.get("leg1_entry_price"),
            leg1_shares: r.get::<Option<i32>, _>("leg1_shares").map(|s| s as u64),
            leg1_filled_at: r.get("leg1_filled_at"),
            leg2_entry_price: r.get("leg2_entry_price"),
            leg2_shares: r.get::<Option<i32>, _>("leg2_shares").map(|s| s as u64),
            leg2_filled_at: r.get("leg2_filled_at"),
            pnl: r.get("pnl"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
        }))
    }

    // ==================== Orders ====================

    /// Insert a new order
    pub async fn insert_order(&self, order: &Order) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO orders (
                cycle_id, leg, client_order_id, exchange_order_id, market_side, order_side,
                token_id, shares, limit_price, avg_fill_price, filled_shares, status,
                submitted_at, filled_at, error
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            RETURNING id
            "#,
        )
        .bind(order.cycle_id)
        .bind(order.leg as i32)
        .bind(&order.client_order_id)
        .bind(&order.exchange_order_id)
        .bind(order.market_side.as_str())
        .bind(order.order_side.to_string())
        .bind(&order.token_id)
        .bind(order.shares as i32)
        .bind(order.limit_price)
        .bind(order.avg_fill_price)
        .bind(order.filled_shares as i32)
        .bind(format!("{:?}", order.status))
        .bind(order.submitted_at)
        .bind(order.filled_at)
        .bind(&order.error)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// Update order status
    pub async fn update_order_status(
        &self,
        client_order_id: &str,
        status: OrderStatus,
        exchange_order_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE orders SET
                status = $1,
                exchange_order_id = COALESCE($2, exchange_order_id),
                submitted_at = CASE
                    WHEN $1 = 'Submitted' AND submitted_at IS NULL THEN NOW()
                    ELSE submitted_at
                END,
                cancelled_at = CASE
                    WHEN $1 = 'Cancelled' AND cancelled_at IS NULL THEN NOW()
                    ELSE cancelled_at
                END,
                updated_at = NOW()
            WHERE client_order_id = $3
            "#,
        )
        .bind(format!("{:?}", status))
        .bind(exchange_order_id)
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update order fill
    pub async fn update_order_fill(
        &self,
        client_order_id: &str,
        filled_shares: u64,
        avg_fill_price: Decimal,
        status: OrderStatus,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE orders SET
                filled_shares = $1,
                avg_fill_price = $2,
                status = $3,
                filled_at = CASE
                    WHEN $3 = 'Filled' AND filled_at IS NULL THEN NOW()
                    ELSE filled_at
                END,
                cancelled_at = CASE
                    WHEN $3 = 'Cancelled' AND cancelled_at IS NULL THEN NOW()
                    ELSE cancelled_at
                END,
                updated_at = NOW()
            WHERE client_order_id = $4
            "#,
        )
        .bind(filled_shares as i32)
        .bind(avg_fill_price)
        .bind(format!("{:?}", status))
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ==================== Daily Metrics ====================

    /// Get or create today's metrics
    pub async fn get_or_create_daily_metrics(&self, date: NaiveDate) -> Result<DailyMetrics> {
        sqlx::query(
            r#"
            INSERT INTO daily_metrics (date)
            VALUES ($1)
            ON CONFLICT (date) DO NOTHING
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;

        let row = sqlx::query(
            r#"
            SELECT date, total_cycles, completed_cycles, aborted_cycles, leg2_completions,
                   total_pnl, max_drawdown, consecutive_failures, halted, halt_reason
            FROM daily_metrics WHERE date = $1
            "#,
        )
        .bind(date)
        .fetch_one(&self.pool)
        .await?;

        Ok(DailyMetrics {
            date: row.get("date"),
            total_cycles: row.get("total_cycles"),
            completed_cycles: row.get("completed_cycles"),
            aborted_cycles: row.get("aborted_cycles"),
            leg2_completions: row.get("leg2_completions"),
            total_pnl: row.get("total_pnl"),
            max_drawdown: row.get("max_drawdown"),
            consecutive_failures: row.get("consecutive_failures"),
            halted: row.get("halted"),
            halt_reason: row.get("halt_reason"),
        })
    }

    /// Increment cycle count
    pub async fn increment_cycle_count(&self, date: NaiveDate) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query("UPDATE daily_metrics SET total_cycles = total_cycles + 1 WHERE date = $1")
            .bind(date)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record cycle completion
    pub async fn record_cycle_completion(&self, date: NaiveDate, pnl: Decimal) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query(
            r#"
            UPDATE daily_metrics SET
                completed_cycles = completed_cycles + 1,
                leg2_completions = leg2_completions + 1,
                total_pnl = total_pnl + $1,
                consecutive_failures = 0
            WHERE date = $2
            "#,
        )
        .bind(pnl)
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record cycle abort
    pub async fn record_cycle_abort(&self, date: NaiveDate) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query(
            r#"
            UPDATE daily_metrics SET
                aborted_cycles = aborted_cycles + 1,
                consecutive_failures = consecutive_failures + 1
            WHERE date = $1
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record cycle abort without counting as a failure.
    ///
    /// Useful for expected/neutral aborts (e.g. IOC order got 0 fill) where we should track
    /// the abort rate but not trip consecutive-failure logic.
    pub async fn record_cycle_abort_neutral(&self, date: NaiveDate) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query(
            r#"
            UPDATE daily_metrics SET
                aborted_cycles = aborted_cycles + 1
            WHERE date = $1
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Halt trading
    pub async fn halt_trading(&self, date: NaiveDate, reason: &str) -> Result<()> {
        self.ensure_daily_metrics_row(date).await?;
        sqlx::query("UPDATE daily_metrics SET halted = TRUE, halt_reason = $1 WHERE date = $2")
            .bind(reason)
            .bind(date)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn ensure_daily_metrics_row(&self, date: NaiveDate) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO daily_metrics (date)
            VALUES ($1)
            ON CONFLICT (date) DO NOTHING
            "#,
        )
        .bind(date)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ==================== Strategy State ====================

    /// Get current strategy state
    pub async fn get_strategy_state(&self) -> Result<PersistedState> {
        let row = sqlx::query(
            r#"
            SELECT current_state, current_round_id, current_cycle_id, risk_state, last_updated
            FROM strategy_state WHERE id = 1
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(PersistedState {
            current_state: StrategyState::try_from(row.get::<&str, _>("current_state"))
                .map_err(|e| PloyError::Internal(e))?,
            current_round_id: row.get("current_round_id"),
            current_cycle_id: row.get("current_cycle_id"),
            risk_state: row.get("risk_state"),
            last_updated: row.get("last_updated"),
        })
    }

    /// Update strategy state
    pub async fn update_strategy_state(
        &self,
        state: StrategyState,
        round_id: Option<i32>,
        cycle_id: Option<i32>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE strategy_state SET
                current_state = $1,
                current_round_id = $2,
                current_cycle_id = $3,
                last_updated = NOW()
            WHERE id = 1
            "#,
        )
        .bind(state.as_str())
        .bind(round_id)
        .bind(cycle_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ==================== Dump Signals ====================

    // ==================== Crash Recovery ====================

    /// Get all incomplete cycles (for crash recovery)
    /// Returns cycles that are in LEG1_PENDING, LEG1_FILLED, or LEG2_PENDING states
    pub async fn get_incomplete_cycles(&self) -> Result<Vec<IncompleteCycle>> {
        let rows = sqlx::query(
            r#"
            SELECT c.id, c.round_id, c.state, c.leg1_side, c.leg1_entry_price, c.leg1_shares,
                   c.leg1_filled_at, c.created_at,
                   r.slug, r.up_token_id, r.down_token_id, r.end_time
            FROM cycles c
            JOIN rounds r ON c.round_id = r.id
            WHERE c.state IN ('LEG1_PENDING', 'LEG1_FILLED', 'LEG2_PENDING')
            ORDER BY c.created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let cycles = rows
            .into_iter()
            .map(|r| IncompleteCycle {
                cycle_id: r.get("id"),
                round_id: r.get("round_id"),
                state: r
                    .get::<String, _>("state")
                    .as_str()
                    .try_into()
                    .unwrap_or(StrategyState::Idle),
                leg1_side: r
                    .get::<Option<String>, _>("leg1_side")
                    .and_then(|s| Side::try_from(s.as_str()).ok()),
                leg1_entry_price: r.get("leg1_entry_price"),
                leg1_shares: r.get::<Option<i32>, _>("leg1_shares").map(|s| s as u64),
                leg1_filled_at: r.get("leg1_filled_at"),
                created_at: r.get("created_at"),
                round_slug: r.get("slug"),
                up_token_id: r.get("up_token_id"),
                down_token_id: r.get("down_token_id"),
                round_end_time: r.get("end_time"),
            })
            .collect();

        Ok(cycles)
    }

    /// Get orphaned orders (submitted but not filled/cancelled for too long)
    pub async fn get_orphaned_orders(&self, age_minutes: i32) -> Result<Vec<OrphanedOrder>> {
        let rows = sqlx::query(
            r#"
            SELECT o.id, o.client_order_id, o.exchange_order_id, o.token_id,
                   o.shares, o.limit_price, o.status, o.submitted_at, o.leg,
                   c.id as cycle_id, c.state as cycle_state
            FROM orders o
            LEFT JOIN cycles c ON o.cycle_id = c.id
            WHERE o.status IN ('Submitted', 'Pending', 'PartiallyFilled')
              AND o.submitted_at < NOW() - INTERVAL '1 minute' * $1
            ORDER BY o.submitted_at ASC
            "#,
        )
        .bind(age_minutes)
        .fetch_all(&self.pool)
        .await?;

        let orders = rows
            .into_iter()
            .map(|r| OrphanedOrder {
                order_id: r.get("id"),
                client_order_id: r.get("client_order_id"),
                exchange_order_id: r.get("exchange_order_id"),
                token_id: r.get("token_id"),
                shares: r.get::<i32, _>("shares") as u64,
                limit_price: r.get("limit_price"),
                status: r.get("status"),
                submitted_at: r.get("submitted_at"),
                leg: r.get::<i32, _>("leg") as u8,
                cycle_id: r.get("cycle_id"),
                cycle_state: r.get("cycle_state"),
            })
            .collect();

        Ok(orders)
    }

    /// Mark an order as cancelled (for orphan cleanup)
    pub async fn mark_order_cancelled(&self, client_order_id: &str, reason: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE orders SET
                status = 'Cancelled',
                error = $1,
                cancelled_at = NOW(),
                updated_at = NOW()
            WHERE client_order_id = $2
            "#,
        )
        .bind(reason)
        .bind(client_order_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Check if trading was halted
    pub async fn is_trading_halted(&self, date: NaiveDate) -> Result<bool> {
        let row = sqlx::query("SELECT halted FROM daily_metrics WHERE date = $1")
            .bind(date)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| r.get::<bool, _>("halted")).unwrap_or(false))
    }

    /// Get recovery summary for startup logging
    pub async fn get_recovery_summary(&self) -> Result<RecoverySummary> {
        let incomplete_cycles = self.get_incomplete_cycles().await?;
        let orphaned_orders = self.get_orphaned_orders(5).await?; // 5 minutes threshold

        let persisted_state = self.get_strategy_state().await.ok();

        Ok(RecoverySummary {
            incomplete_cycle_count: incomplete_cycles.len(),
            orphaned_order_count: orphaned_orders.len(),
            last_state: persisted_state.as_ref().map(|s| s.current_state.clone()),
            last_cycle_id: persisted_state.and_then(|s| s.current_cycle_id),
            incomplete_cycles,
            orphaned_orders,
        })
    }

    /// Insert dump signal
    pub async fn insert_dump_signal(&self, signal: &DumpSignal, round_id: i32) -> Result<i32> {
        let row = sqlx::query(
            r#"
            INSERT INTO dump_signals (
                round_id, side, trigger_price, reference_price, drop_pct,
                spread_bps, was_valid, timestamp
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id
            "#,
        )
        .bind(round_id)
        .bind(signal.side.as_str())
        .bind(signal.trigger_price)
        .bind(signal.reference_price)
        .bind(signal.drop_pct)
        .bind(signal.spread_bps as i32)
        .bind(signal.is_valid(500)) // TODO: use config
        .bind(signal.timestamp)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }
}

/// Daily metrics structure
#[derive(Debug, Clone)]
pub struct DailyMetrics {
    pub date: NaiveDate,
    pub total_cycles: i32,
    pub completed_cycles: i32,
    pub aborted_cycles: i32,
    pub leg2_completions: i32,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
    pub consecutive_failures: i32,
    pub halted: bool,
    pub halt_reason: Option<String>,
}

/// Persisted strategy state
#[derive(Debug, Clone)]
pub struct PersistedState {
    pub current_state: StrategyState,
    pub current_round_id: Option<i32>,
    pub current_cycle_id: Option<i32>,
    pub risk_state: String,
    pub last_updated: DateTime<Utc>,
}

/// Incomplete cycle for crash recovery
#[derive(Debug, Clone)]
pub struct IncompleteCycle {
    pub cycle_id: i32,
    pub round_id: i32,
    pub state: StrategyState,
    pub leg1_side: Option<Side>,
    pub leg1_entry_price: Option<Decimal>,
    pub leg1_shares: Option<u64>,
    pub leg1_filled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub round_slug: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub round_end_time: DateTime<Utc>,
}

impl IncompleteCycle {
    /// Check if the round has already ended
    pub fn is_round_expired(&self) -> bool {
        Utc::now() > self.round_end_time
    }

    /// Get time remaining until round ends
    pub fn time_remaining(&self) -> chrono::Duration {
        self.round_end_time - Utc::now()
    }
}

/// Orphaned order for cleanup
#[derive(Debug, Clone)]
pub struct OrphanedOrder {
    pub order_id: i32,
    pub client_order_id: String,
    pub exchange_order_id: Option<String>,
    pub token_id: String,
    pub shares: u64,
    pub limit_price: Option<Decimal>,
    pub status: String,
    pub submitted_at: Option<DateTime<Utc>>,
    pub leg: u8,
    pub cycle_id: Option<i32>,
    pub cycle_state: Option<String>,
}

impl OrphanedOrder {
    /// Check if this order can be cancelled on the exchange
    pub fn can_cancel_on_exchange(&self) -> bool {
        self.exchange_order_id.is_some() && self.status != "Cancelled" && self.status != "Filled"
    }
}

/// Recovery summary for startup
#[derive(Debug, Clone)]
pub struct RecoverySummary {
    pub incomplete_cycle_count: usize,
    pub orphaned_order_count: usize,
    pub last_state: Option<StrategyState>,
    pub last_cycle_id: Option<i32>,
    pub incomplete_cycles: Vec<IncompleteCycle>,
    pub orphaned_orders: Vec<OrphanedOrder>,
}

impl RecoverySummary {
    /// Check if recovery is needed
    pub fn needs_recovery(&self) -> bool {
        self.incomplete_cycle_count > 0 || self.orphaned_order_count > 0
    }

    /// Log recovery summary
    pub fn log_summary(&self) {
        if !self.needs_recovery() {
            info!("No crash recovery needed - clean startup");
            return;
        }

        info!(
            "Crash recovery summary: {} incomplete cycles, {} orphaned orders",
            self.incomplete_cycle_count, self.orphaned_order_count
        );

        for cycle in &self.incomplete_cycles {
            let expired = if cycle.is_round_expired() {
                " [EXPIRED]"
            } else {
                ""
            };
            info!(
                "  - Cycle {} in state {} (round: {}){}",
                cycle.cycle_id, cycle.state, cycle.round_slug, expired
            );
        }

        for order in &self.orphaned_orders {
            info!(
                "  - Order {} ({}) status={} token={}",
                order.client_order_id,
                if order.leg == 1 { "Leg1" } else { "Leg2" },
                order.status,
                &order.token_id[..8]
            );
        }
    }
}

impl PostgresStore {
    // ==================== NBA Comeback Stats ====================

    /// Load all team stats for a given season
    pub async fn load_nba_team_stats(
        &self,
        season: &str,
    ) -> Result<Vec<crate::strategy::nba_comeback::nba_data_collector::TeamStats>> {
        let rows = sqlx::query(
            r#"
            SELECT team_name, team_abbrev, season,
                   wins, losses, win_rate, avg_points,
                   q1_avg_points, q2_avg_points, q3_avg_points, q4_avg_points,
                   comeback_rate_5pt, comeback_rate_10pt, comeback_rate_15pt,
                   q4_net_rating, q4_pace,
                   elo_rating, offensive_rating, defensive_rating
            FROM nba_team_stats
            WHERE season = $1
            "#,
        )
        .bind(season)
        .fetch_all(&self.pool)
        .await?;

        let stats = rows
            .iter()
            .map(
                |r| crate::strategy::nba_comeback::nba_data_collector::TeamStats {
                    team_name: r.get("team_name"),
                    season: r.get("season"),
                    wins: r.get("wins"),
                    losses: r.get("losses"),
                    win_rate: r.get("win_rate"),
                    avg_points: r.get("avg_points"),
                    q1_avg_points: r.get("q1_avg_points"),
                    q2_avg_points: r.get("q2_avg_points"),
                    q3_avg_points: r.get("q3_avg_points"),
                    q4_avg_points: r.get("q4_avg_points"),
                    comeback_rate_5pt: r.get("comeback_rate_5pt"),
                    comeback_rate_10pt: r.get("comeback_rate_10pt"),
                    comeback_rate_15pt: r.get("comeback_rate_15pt"),
                    elo_rating: r.get("elo_rating"),
                    offensive_rating: r.get("offensive_rating"),
                    defensive_rating: r.get("defensive_rating"),
                },
            )
            .collect();

        Ok(stats)
    }

    /// Upsert a single team's stats (insert or update on conflict)
    pub async fn upsert_nba_team_stats(
        &self,
        team_name: &str,
        team_abbrev: &str,
        season: &str,
        stats: &crate::strategy::nba_comeback::nba_data_collector::TeamStats,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO nba_team_stats (
                team_name, team_abbrev, season,
                wins, losses, win_rate, avg_points,
                q1_avg_points, q2_avg_points, q3_avg_points, q4_avg_points,
                comeback_rate_5pt, comeback_rate_10pt, comeback_rate_15pt,
                q4_net_rating,
                elo_rating, offensive_rating, defensive_rating,
                updated_at
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18, NOW())
            ON CONFLICT (team_abbrev, season) DO UPDATE SET
                team_name = EXCLUDED.team_name,
                wins = EXCLUDED.wins,
                losses = EXCLUDED.losses,
                win_rate = EXCLUDED.win_rate,
                avg_points = EXCLUDED.avg_points,
                q1_avg_points = EXCLUDED.q1_avg_points,
                q2_avg_points = EXCLUDED.q2_avg_points,
                q3_avg_points = EXCLUDED.q3_avg_points,
                q4_avg_points = EXCLUDED.q4_avg_points,
                comeback_rate_5pt = EXCLUDED.comeback_rate_5pt,
                comeback_rate_10pt = EXCLUDED.comeback_rate_10pt,
                comeback_rate_15pt = EXCLUDED.comeback_rate_15pt,
                q4_net_rating = EXCLUDED.q4_net_rating,
                elo_rating = EXCLUDED.elo_rating,
                offensive_rating = EXCLUDED.offensive_rating,
                defensive_rating = EXCLUDED.defensive_rating,
                updated_at = NOW()
            "#,
        )
        .bind(team_name)
        .bind(team_abbrev)
        .bind(season)
        .bind(stats.wins)
        .bind(stats.losses)
        .bind(stats.win_rate)
        .bind(stats.avg_points)
        .bind(stats.q1_avg_points)
        .bind(stats.q2_avg_points)
        .bind(stats.q3_avg_points)
        .bind(stats.q4_avg_points)
        .bind(stats.comeback_rate_5pt)
        .bind(stats.comeback_rate_10pt)
        .bind(stats.comeback_rate_15pt)
        .bind(0.0f64) // q4_net_rating placeholder
        .bind(stats.elo_rating)
        .bind(stats.offensive_rating)
        .bind(stats.defensive_rating)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // ==================== Event Registry ====================

    /// Insert or update an event in the registry.
    /// Deduplicates on (title, source); uses COALESCE to preserve existing data.
    pub async fn upsert_event(
        &self,
        req: &crate::strategy::registry::EventUpsertRequest,
    ) -> Result<i32> {
        let status = req.status.as_deref().unwrap_or("discovered");
        let metadata = req
            .metadata
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));

        let row = sqlx::query(
            r#"
            INSERT INTO event_registry (
                title, source, event_id, slug, domain, strategy_hint,
                status, confidence, settlement_rule, end_time,
                market_slug, condition_id, token_ids, outcome_prices,
                metadata, last_scanned_at
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, NOW())
            ON CONFLICT (title, source) DO UPDATE SET
                event_id       = COALESCE(EXCLUDED.event_id, event_registry.event_id),
                slug           = COALESCE(EXCLUDED.slug, event_registry.slug),
                domain         = EXCLUDED.domain,
                strategy_hint  = COALESCE(EXCLUDED.strategy_hint, event_registry.strategy_hint),
                confidence     = COALESCE(EXCLUDED.confidence, event_registry.confidence),
                settlement_rule= COALESCE(EXCLUDED.settlement_rule, event_registry.settlement_rule),
                end_time       = COALESCE(EXCLUDED.end_time, event_registry.end_time),
                market_slug    = COALESCE(EXCLUDED.market_slug, event_registry.market_slug),
                condition_id   = COALESCE(EXCLUDED.condition_id, event_registry.condition_id),
                token_ids      = COALESCE(EXCLUDED.token_ids, event_registry.token_ids),
                outcome_prices = COALESCE(EXCLUDED.outcome_prices, event_registry.outcome_prices),
                metadata       = event_registry.metadata || EXCLUDED.metadata,
                last_scanned_at= NOW(),
                updated_at     = NOW()
            RETURNING id
            "#,
        )
        .bind(&req.title)
        .bind(&req.source)
        .bind(&req.event_id)
        .bind(&req.slug)
        .bind(&req.domain)
        .bind(&req.strategy_hint)
        .bind(status)
        .bind(req.confidence)
        .bind(&req.settlement_rule)
        .bind(req.end_time)
        .bind(&req.market_slug)
        .bind(&req.condition_id)
        .bind(&req.token_ids)
        .bind(&req.outcome_prices)
        .bind(&metadata)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.get("id"))
    }

    /// List events matching the given filter criteria.
    pub async fn list_events(
        &self,
        filter: &crate::strategy::registry::EventFilter,
    ) -> Result<Vec<crate::strategy::registry::RegisteredEvent>> {
        let limit = filter.limit.unwrap_or(100);

        // Build dynamic WHERE clauses
        let mut conditions = Vec::new();
        let mut idx = 1u32;

        if filter.status.is_some() {
            conditions.push(format!("status = ${idx}"));
            idx += 1;
        }
        if filter.domain.is_some() {
            conditions.push(format!("domain = ${idx}"));
            idx += 1;
        }
        if filter.strategy_hint.is_some() {
            conditions.push(format!("strategy_hint = ${idx}"));
            idx += 1;
        }
        if filter.source.is_some() {
            conditions.push(format!("source = ${idx}"));
            idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            r#"
            SELECT id, event_id, title, slug, source, domain, strategy_hint,
                   status, confidence, settlement_rule, end_time,
                   market_slug, condition_id, token_ids, outcome_prices,
                   metadata, last_scanned_at, created_at, updated_at
            FROM event_registry
            {where_clause}
            ORDER BY updated_at DESC
            LIMIT ${idx}
            "#,
        );

        let mut query = sqlx::query(&sql);

        if let Some(ref s) = filter.status {
            query = query.bind(s);
        }
        if let Some(ref d) = filter.domain {
            query = query.bind(d);
        }
        if let Some(ref sh) = filter.strategy_hint {
            query = query.bind(sh);
        }
        if let Some(ref src) = filter.source {
            query = query.bind(src);
        }
        query = query.bind(limit);

        let rows = query.fetch_all(&self.pool).await?;

        let events = rows
            .iter()
            .map(|r| crate::strategy::registry::RegisteredEvent {
                id: r.get("id"),
                event_id: r.get("event_id"),
                title: r.get("title"),
                slug: r.get("slug"),
                source: r.get("source"),
                domain: r.get("domain"),
                strategy_hint: r.get("strategy_hint"),
                status: r.get("status"),
                confidence: r.get("confidence"),
                settlement_rule: r.get("settlement_rule"),
                end_time: r.get("end_time"),
                market_slug: r.get("market_slug"),
                condition_id: r.get("condition_id"),
                token_ids: r.get("token_ids"),
                outcome_prices: r.get("outcome_prices"),
                metadata: r.get("metadata"),
                last_scanned_at: r.get("last_scanned_at"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            })
            .collect();

        Ok(events)
    }

    /// Transition an event to a new status (validates the state machine).
    pub async fn update_event_status(
        &self,
        id: i32,
        new_status: crate::strategy::registry::EventStatus,
    ) -> Result<()> {
        let row = sqlx::query("SELECT status FROM event_registry WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        let row =
            row.ok_or_else(|| PloyError::Validation(format!("event_registry id={id} not found")))?;

        let current_str: String = row.get("status");
        let current = crate::strategy::registry::EventStatus::from_str(&current_str)
            .ok_or_else(|| PloyError::Validation(format!("unknown status in DB: {current_str}")))?;

        if !current.can_transition_to(new_status) {
            return Err(PloyError::InvalidStateTransition {
                from: current_str,
                to: new_status.to_string(),
            });
        }

        sqlx::query("UPDATE event_registry SET status = $1, updated_at = NOW() WHERE id = $2")
            .bind(new_status.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get events with status=monitoring for a given strategy.
    pub async fn get_monitoring_events(
        &self,
        strategy_hint: &str,
    ) -> Result<Vec<crate::strategy::registry::RegisteredEvent>> {
        let filter = crate::strategy::registry::EventFilter {
            status: Some("monitoring".to_string()),
            strategy_hint: Some(strategy_hint.to_string()),
            ..Default::default()
        };
        self.list_events(&filter).await
    }

    /// Expire events whose end_time has passed (from non-terminal states).
    pub async fn expire_stale_events(&self) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE event_registry
            SET status = 'expired', updated_at = NOW()
            WHERE end_time < NOW()
              AND status NOT IN ('settled', 'expired')
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

// Implement Side::try_from for database strings
impl TryFrom<&str> for Side {
    type Error = String;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        match s.to_uppercase().as_str() {
            "UP" => Ok(Side::Up),
            "DOWN" => Ok(Side::Down),
            _ => Err(format!("Unknown side: {}", s)),
        }
    }
}
