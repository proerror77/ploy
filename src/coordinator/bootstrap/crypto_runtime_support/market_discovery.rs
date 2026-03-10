use super::*;

pub(super) async fn initialize_crypto_market_discovery(
    shared_pool: Option<&PgPool>,
    pm_client_ref: &PolymarketClient,
    crypto_cfg: &crate::strategy::CryptoTradingConfig,
    all_coins: &[String],
    use_data_plane: bool,
    pm_ws: Arc<PolymarketWebSocket>,
) -> Arc<EventMatcher> {
    let event_matcher = Arc::new(EventMatcher::new(pm_client_ref.clone()));
    if let Err(e) = event_matcher.refresh().await {
        warn!(error = %e, "crypto event matcher refresh failed (continuing)");
    }

    let collector_min_remaining_secs = env_i64("PM_COLLECTOR_MIN_REMAINING_SECS", 0)
        .max(-86400)
        .min(86400);
    let mut desired: HashMap<String, Side> = HashMap::new();
    let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
    for coin in all_coins {
        let symbol = format!("{}USDT", coin.to_uppercase());
        for ev in event_matcher
            .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
            .await
        {
            desired.insert(ev.up_token_id.clone(), Side::Up);
            desired.insert(ev.down_token_id.clone(), Side::Down);

            let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
            collector_targets.push(
                crate::collector::CollectorTokenTarget::new(ev.up_token_id.clone(), "CRYPTO")
                    .with_expires_at(expires_at)
                    .with_metadata(serde_json::json!({
                        "symbol": symbol.as_str(),
                        "side": "UP",
                        "condition_id": ev.condition_id.as_str(),
                        "slug": ev.slug.as_str(),
                        "title": ev.title.as_str(),
                        "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                    })),
            );
            collector_targets.push(
                crate::collector::CollectorTokenTarget::new(ev.down_token_id.clone(), "CRYPTO")
                    .with_expires_at(expires_at)
                    .with_metadata(serde_json::json!({
                        "symbol": symbol.as_str(),
                        "side": "DOWN",
                        "condition_id": ev.condition_id.as_str(),
                        "slug": ev.slug.as_str(),
                        "title": ev.title.as_str(),
                        "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                    })),
            );
        }
    }
    if use_data_plane {
        for (token, side) in &desired {
            pm_ws.register_token(token, *side).await;
        }
        pm_ws.request_resubscribe();
        info!(
            agent = %crypto_cfg.agent_id,
            token_count = desired.len(),
            "seeded PM token mappings for crypto data collection"
        );
    } else {
        let (_added, _removed, _updated, total) = pm_ws.reconcile_token_sides(&desired).await;
        info!(
            agent = %crypto_cfg.agent_id,
            token_count = total,
            "seeded PM token mappings for crypto data collection"
        );
    }

    if let Some(pool) = shared_pool {
        if let Err(e) = crate::collector::ensure_collector_token_targets_table(pool).await {
            warn!(
                agent = %crypto_cfg.agent_id,
                error = %e,
                "failed to ensure collector_token_targets table"
            );
        }

        if let Err(e) =
            crate::collector::upsert_collector_token_targets(pool, &collector_targets).await
        {
            warn!(
                agent = %crypto_cfg.agent_id,
                error = %e,
                "failed to upsert collector token targets (crypto)"
            );
        }
    }

    let pm_ws_collector = pm_ws.clone();
    let matcher_collector = event_matcher.clone();
    let coins_collector = all_coins.to_vec();
    let agent_id_collector = crypto_cfg.agent_id.clone();
    let pool_collector = shared_pool.cloned();
    let use_data_plane_collector = use_data_plane;
    let initial_last_desired = if use_data_plane_collector {
        desired.clone()
    } else {
        HashMap::new()
    };
    tokio::spawn(async move {
        let refresh_secs = env_u64("PM_COLLECTOR_REFRESH_SECS", PM_COLLECTOR_REFRESH_SECS).max(10);
        let mut tick = tokio::time::interval(Duration::from_secs(refresh_secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_desired = initial_last_desired;

        loop {
            tick.tick().await;

            if let Err(e) = matcher_collector.refresh().await {
                warn!(agent = %agent_id_collector, error = %e, "pm token collector refresh failed");
                continue;
            }

            let mut desired: HashMap<String, Side> = HashMap::new();
            let mut collector_targets: Vec<crate::collector::CollectorTokenTarget> = Vec::new();
            for coin in &coins_collector {
                let symbol = format!("{}USDT", coin.to_uppercase());
                for ev in matcher_collector
                    .get_events_with_min_remaining(&symbol, collector_min_remaining_secs)
                    .await
                {
                    desired.insert(ev.up_token_id.clone(), Side::Up);
                    desired.insert(ev.down_token_id.clone(), Side::Down);

                    let expires_at = Some(ev.end_time + chrono::Duration::hours(1));
                    collector_targets.push(
                        crate::collector::CollectorTokenTarget::new(
                            ev.up_token_id.clone(),
                            "CRYPTO",
                        )
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "UP",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                    );
                    collector_targets.push(
                        crate::collector::CollectorTokenTarget::new(
                            ev.down_token_id.clone(),
                            "CRYPTO",
                        )
                        .with_expires_at(expires_at)
                        .with_metadata(serde_json::json!({
                            "symbol": symbol.as_str(),
                            "side": "DOWN",
                            "condition_id": ev.condition_id.as_str(),
                            "slug": ev.slug.as_str(),
                            "title": ev.title.as_str(),
                            "price_to_beat": ev.price_to_beat.as_ref().map(ToString::to_string),
                        })),
                    );
                }
            }

            if use_data_plane_collector {
                if desired != last_desired {
                    let previous_token_count = last_desired.len();
                    for (token, side) in &desired {
                        pm_ws_collector.register_token(token, *side).await;
                    }
                    pm_ws_collector.request_resubscribe();
                    info!(
                        agent = %agent_id_collector,
                        previous_token_count,
                        token_count = desired.len(),
                        "pm token collector refreshed token set on shared data-plane ws; resubscribe requested"
                    );
                    last_desired = desired;
                }
            } else {
                let (added, removed, updated, total) =
                    pm_ws_collector.reconcile_token_sides(&desired).await;
                if added > 0 || removed > 0 {
                    pm_ws_collector.request_resubscribe();
                    info!(
                        agent = %agent_id_collector,
                        added,
                        removed,
                        updated,
                        token_count = total,
                        "pm token collector reconciled token set; resubscribe requested"
                    );
                }
            }

            if let Some(pool) = pool_collector.as_ref() {
                let ensured = crate::collector::ensure_collector_token_targets_table(pool).await;
                if let Err(e) = ensured {
                    warn!(
                        agent = %agent_id_collector,
                        error = %e,
                        "failed to ensure collector_token_targets table"
                    );
                }

                if let Err(e) =
                    crate::collector::upsert_collector_token_targets(pool, &collector_targets).await
                {
                    warn!(
                        agent = %agent_id_collector,
                        error = %e,
                        "failed to upsert collector token targets (crypto)"
                    );
                }
            }
        }
    });

    event_matcher
}
