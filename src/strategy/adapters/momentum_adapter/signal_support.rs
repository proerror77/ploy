use super::*;

fn database_url_from_env() -> Option<String> {
    std::env::var("PLOY_DATABASE__URL")
        .ok()
        .or_else(|| std::env::var("PLOY__DATABASE__URL").ok())
        .or_else(|| std::env::var("PLOY_DATABASE_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn account_id_from_env() -> String {
    std::env::var("PLOY_ACCOUNT__ID")
        .ok()
        .or_else(|| std::env::var("PLOY__ACCOUNT__ID").ok())
        .or_else(|| std::env::var("PLOY_ACCOUNT_ID").ok())
        .unwrap_or_else(|| "default".to_string())
        .trim()
        .to_string()
}

impl MomentumStrategyAdapter {
    pub(super) async fn get_signal_log_pool(&self) -> Option<Arc<sqlx::PgPool>> {
        let existing = self.signal_log_pool.get();
        if let Some(pool) = existing {
            return Some(pool.clone());
        }

        let db_url = database_url_from_env()?;

        let pool = match sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await
        {
            Ok(p) => Arc::new(p),
            Err(e) => {
                warn!(
                    error = %e,
                    "signal recorder: failed to connect to Postgres (signal logging disabled)"
                );
                return None;
            }
        };

        let _ = self.signal_log_pool.set(pool.clone());
        Some(pool)
    }

    pub(super) async fn ensure_signal_log_ready(&self, pool: &sqlx::PgPool) {
        if self.signal_log_ready.get().is_some() {
            return;
        }

        if let Err(e) = crate::persistence::ensure_strategy_observability_tables(pool).await {
            warn!(error = %e, "signal recorder: failed to ensure observability tables");
            return;
        }

        let _ = self.signal_log_ready.set(());
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_directional_signal(
        &self,
        symbol: &str,
        direction: Direction,
        event_id: &str,
        token_id: &str,
        p_hat: f64,
        effective_p: f64,
        ev_net: f64,
        market_ask: Decimal,
        sigma: f64,
        s0: Decimal,
        st: Decimal,
        time_remaining_secs: f64,
        window_secs: u64,
    ) {
        let Some(pool) = self.get_signal_log_pool().await else {
            return;
        };
        self.ensure_signal_log_ready(&pool).await;
        if self.signal_log_ready.get().is_none() {
            return;
        }

        let account_id = account_id_from_env();
        let agent_id = std::env::var("PLOY_AGENT_ID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.id.clone());

        let strategy_id = self.id.clone();
        let symbol = symbol.to_string();
        let side = match direction {
            Direction::Up => "UP",
            Direction::Down => "DOWN",
        }
        .to_string();

        let context = serde_json::json!({
            "mode": "directional",
            "dry_run": self.dry_run,
            "event_id": event_id,
            "p_hat": p_hat,
            "effective_p": effective_p,
            "ev_net": ev_net,
            "sigma": sigma,
            "s0": s0.to_string(),
            "st": st.to_string(),
            "time_remaining_secs": time_remaining_secs,
            "window_secs": window_secs,
        });

        let token_id = token_id.to_string();
        let market_ask = market_ask;

        tokio::spawn(async move {
            let res = sqlx::query(
                r#"
                INSERT INTO signal_history (
                    account_id, intent_id, agent_id, strategy_id, domain, signal_type,
                    market_slug, token_id, symbol, side, confidence, fair_value, market_price, edge, config_hash, context
                )
                VALUES (
                    $1, NULL, $2, $3, 'crypto', 'directional_entry',
                    NULL, $4, $5, $6, $7, $8, $9, $10, NULL, $11
                )
                "#,
            )
            .bind(account_id)
            .bind(agent_id)
            .bind(strategy_id)
            .bind(token_id)
            .bind(symbol)
            .bind(side)
            .bind(effective_p)
            .bind(p_hat)
            .bind(market_ask)
            .bind(ev_net)
            .bind(sqlx::types::Json(context))
            .execute(&*pool)
            .await;

            if let Err(e) = res {
                warn!(error = %e, "signal recorder: failed to insert directional signal");
            }
        });
    }
}
