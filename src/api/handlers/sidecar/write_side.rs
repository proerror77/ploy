use super::*;

/// POST /api/sidecar/orders
///
/// Submit an order through the Coordinator pipeline (risk gate -> queue -> execution).
pub async fn sidecar_submit_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SidecarOrderRequest>,
) -> std::result::Result<Json<SidecarOrderResponse>, (StatusCode, String)> {
    ensure_sidecar_authorized(&headers)?;
    validate_account_scope(&state, req.account_id.as_deref())?;

    let dry_run = req.dry_run.unwrap_or(true);
    let price = Decimal::from_str(&format!("{:.4}", req.price))
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid price format".to_string()))?;

    if price <= Decimal::ZERO || price >= Decimal::ONE {
        return Err((
            StatusCode::BAD_REQUEST,
            "Price must be between 0 and 1 (exclusive)".to_string(),
        ));
    }

    let order_cost = req.shares as f64 * req.price;
    if order_cost > 50.0 {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Order cost ${:.2} exceeds sidecar limit $50", order_cost),
        ));
    }

    if dry_run {
        info!(
            market = %req.market_slug,
            shares = req.shares,
            price = %price,
            strategy = %req.strategy,
            "sidecar dry-run order (not submitted)"
        );

        let _ = sqlx::query(
            r#"
            INSERT INTO security_audit_log (event_type, severity, details, metadata)
            VALUES ('SIDECAR_DRY_RUN', 'LOW', $1, $2)
            "#,
        )
        .bind(format!(
            "Sidecar dry-run: {} shares @ {} on {}",
            req.shares, price, req.market_slug
        ))
        .bind(serde_json::json!({
            "strategy": req.strategy,
            "market_slug": req.market_slug,
            "shares": req.shares,
            "price": req.price,
            "decision_request_id": req.decision_request_id,
        }))
        .execute(state.store.pool())
        .await;

        return Ok(Json(SidecarOrderResponse {
            success: true,
            intent_id: None,
            message: format!(
                "Dry-run: would buy {} shares @ ${} on {}",
                req.shares, price, req.market_slug
            ),
            dry_run: true,
        }));
    }

    if !sidecar_orders_live_enabled() {
        return Err((
            StatusCode::CONFLICT,
            "live /api/sidecar/orders is disabled; route live intents to /api/sidecar/intents"
                .to_string(),
        ));
    }

    let coordinator = state.coordinator.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Coordinator not running (platform not started)".to_string(),
        )
    })?;

    let deployment_id = req
        .deployment_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "deployment_id is required for live /api/sidecar/orders".to_string(),
            )
        })?
        .to_string();
    let deployment = resolve_intent_deployment(&state, &deployment_id).await?;

    let domain_default = deployment
        .as_ref()
        .map(|d| d.domain)
        .unwrap_or(Domain::Sports);
    let domain = parse_sidecar_domain(req.domain.as_deref(), domain_default)?;
    ensure_domain_allowed(&state, domain, "sidecar order domain")?;
    let side = parse_binary_side(req.side.as_deref())?;
    let is_buy = parse_is_buy(None, req.is_buy)?;

    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "sidecar".to_string());
    metadata.insert("strategy".to_string(), req.strategy.clone());
    metadata.insert("deployment_id".to_string(), deployment_id);
    if let Some(ref dec_id) = req.decision_request_id {
        metadata.insert("decision_request_id".to_string(), dec_id.clone());
    }
    if let Some(ref reasoning) = req.decision_reasoning {
        metadata.insert("decision_reasoning".to_string(), reasoning.clone());
    }
    if let Some(edge) = req.edge {
        metadata.insert("edge".to_string(), format!("{:.4}", edge));
    }
    if let Some(conf) = req.confidence {
        metadata.insert("confidence".to_string(), format!("{:.2}", conf));
    }
    if let Some(idem) = req
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        metadata.insert("idempotency_key".to_string(), idem.to_string());
    }
    metadata.insert("domain".to_string(), domain.to_string());
    metadata.insert("account_id".to_string(), state.account_id.clone());
    if let Some(dep) = deployment.as_ref() {
        validate_deployment_binding(dep, domain, &req.market_slug, &metadata)?;
        apply_deployment_metadata(&mut metadata, dep);
    }

    let mut intent = OrderIntent::new(
        "sidecar",
        domain,
        &req.market_slug,
        &req.token_id,
        side,
        is_buy,
        req.shares,
        price,
    );
    intent.priority = clamp_external_priority(
        deployment
            .as_ref()
            .map(deployment_default_priority)
            .unwrap_or(OrderPriority::Normal),
    );
    intent.metadata = metadata;

    let intent_id = intent.intent_id.to_string();

    ensure_agent_authorized(&state, "sidecar")?;
    coordinator
        .submit_order(intent)
        .await
        .map_err(|e| map_coordinator_submit_error("Failed to submit order", e))?;

    broadcast_sidecar_activity(
        &state,
        &intent_id,
        &req.market_slug,
        &req.token_id,
        side,
        req.shares,
        price,
    );

    info!(
        intent_id = %intent_id,
        market = %req.market_slug,
        shares = req.shares,
        price = %price,
        "sidecar order submitted to coordinator"
    );

    Ok(Json(SidecarOrderResponse {
        success: true,
        intent_id: Some(intent_id),
        message: "Order submitted to coordinator pipeline".to_string(),
        dry_run: false,
    }))
}

/// POST /api/sidecar/intents
///
/// Unified ingestion endpoint for external runtimes (OpenClaw/RPC/scripts).
/// Always routes through Coordinator (risk gate -> duplicate guard -> allocator -> execution).
pub async fn sidecar_submit_intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SidecarIntentRequest>,
) -> std::result::Result<Json<SidecarIntentResponse>, (StatusCode, String)> {
    ensure_sidecar_authorized(&headers)?;
    let requested_account =
        resolve_request_account_scope(req.account_id.as_deref(), Some(&req.metadata))?;
    validate_account_scope(&state, requested_account.as_deref())?;

    let dry_run = req.dry_run.unwrap_or(false);
    let price = Decimal::from_str(&format!("{:.6}", req.price_limit)).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Invalid price_limit format".to_string(),
        )
    })?;
    if price <= Decimal::ZERO || price >= Decimal::ONE {
        return Err((
            StatusCode::BAD_REQUEST,
            "price_limit must be between 0 and 1 (exclusive)".to_string(),
        ));
    }
    if req.size == 0 {
        return Err((StatusCode::BAD_REQUEST, "size must be > 0".to_string()));
    }
    if req.deployment_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "deployment_id is required".to_string(),
        ));
    }
    if req.market_slug.trim().is_empty() || req.token_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "market_slug and token_id are required".to_string(),
        ));
    }

    let deployment = resolve_intent_deployment(&state, &req.deployment_id).await?;

    let domain_default = deployment
        .as_ref()
        .map(|d| d.domain)
        .unwrap_or(Domain::Crypto);
    let domain = parse_sidecar_domain(req.domain.as_deref(), domain_default)?;
    ensure_domain_allowed(&state, domain, "sidecar intent domain")?;
    let side = parse_binary_side(req.side.as_deref())?;
    let is_buy = parse_is_buy(req.order_side.as_deref(), req.is_buy)?;
    let priority = if req.priority.as_deref().is_some() {
        parse_order_priority(req.priority.as_deref())?
    } else {
        deployment
            .as_ref()
            .map(deployment_default_priority)
            .unwrap_or(OrderPriority::Normal)
    };
    let priority = clamp_external_priority(priority);
    let agent_id = req
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("openclaw_rpc")
        .to_string();

    let mut metadata = req.metadata;
    metadata
        .entry("source".to_string())
        .or_insert_with(|| "sidecar.intent_ingress".to_string());
    metadata.insert("deployment_id".to_string(), req.deployment_id.clone());
    metadata
        .entry("domain".to_string())
        .or_insert_with(|| domain.to_string());
    metadata.insert("account_id".to_string(), state.account_id.clone());
    if let Some(idem) = req
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        metadata.insert("idempotency_key".to_string(), idem.to_string());
    }
    if let Some(reason) = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        metadata.insert("intent_reason".to_string(), reason.to_string());
    }
    if let Some(edge) = req.edge {
        metadata.insert("signal_edge".to_string(), format!("{:.6}", edge));
    }
    if let Some(conf) = req.confidence {
        metadata.insert("signal_confidence".to_string(), format!("{:.6}", conf));
    }
    if let Some(dep) = deployment.as_ref() {
        validate_deployment_binding(dep, domain, &req.market_slug, &metadata)?;
        apply_deployment_metadata(&mut metadata, dep);
    }

    let mut intent = OrderIntent::new(
        &agent_id,
        domain,
        &req.market_slug,
        &req.token_id,
        side,
        is_buy,
        req.size,
        price,
    );
    intent.priority = priority;
    intent.metadata = metadata;
    if let Some(raw) = req
        .intent_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let parsed = Uuid::parse_str(raw).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "intent_id must be a UUID".to_string(),
            )
        })?;
        intent.intent_id = parsed;
    }
    let intent_id = intent.intent_id.to_string();

    if dry_run {
        return Ok(Json(SidecarIntentResponse {
            success: true,
            intent_id,
            message: "Dry-run: intent validated and skipped".to_string(),
            dry_run: true,
        }));
    }

    ensure_agent_authorized(&state, &agent_id)?;

    let coordinator = state.coordinator.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Coordinator not running (platform not started)".to_string(),
        )
    })?;
    coordinator
        .submit_order(intent)
        .await
        .map_err(|e| map_coordinator_submit_error("Failed to submit intent", e))?;

    broadcast_sidecar_activity(
        &state,
        &intent_id,
        &req.market_slug,
        &req.token_id,
        side,
        req.size,
        price,
    );

    Ok(Json(SidecarIntentResponse {
        success: true,
        intent_id,
        message: "Intent submitted to coordinator pipeline".to_string(),
        dry_run: false,
    }))
}
