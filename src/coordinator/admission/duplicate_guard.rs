use super::*;

#[derive(Debug)]
pub(super) struct IntentDuplicateGuard {
    enabled: bool,
    window: ChronoDuration,
    scope: DuplicateGuardScope,
    recent_buys: HashMap<String, chrono::DateTime<Utc>>,
}

impl IntentDuplicateGuard {
    pub(super) fn new(window_ms: u64, enabled: bool, scope: DuplicateGuardScope) -> Self {
        let clamped_ms = window_ms.min(i64::MAX as u64) as i64;
        let window = ChronoDuration::milliseconds(clamped_ms.max(1));
        Self {
            enabled,
            window,
            scope,
            recent_buys: HashMap::new(),
        }
    }

    pub(super) fn deployment_scope(intent: &OrderIntent) -> String {
        intent_deployment_scope(intent)
    }

    fn buy_key(&self, intent: &OrderIntent) -> Option<String> {
        if !intent.is_buy || intent.priority == OrderPriority::Critical {
            return None;
        }

        let scope = match intent
            .metadata
            .get("duplicate_guard_scope")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("deployment") | Some("dep") => DuplicateGuardScope::Deployment,
            Some("market") | Some("global") => DuplicateGuardScope::Market,
            _ => self.scope,
        };

        let market = intent_market_identity(intent);
        let base = format!("{}|{}", intent.domain, market);

        match scope {
            DuplicateGuardScope::Market => Some(base),
            DuplicateGuardScope::Deployment => {
                Some(format!("{}|{}", base, Self::deployment_scope(intent)))
            }
        }
    }

    fn prune(&mut self, now: chrono::DateTime<Utc>) {
        self.recent_buys
            .retain(|_, ts| now.signed_duration_since(*ts) < self.window);
    }

    pub(super) fn register_or_block(
        &mut self,
        intent: &OrderIntent,
        now: chrono::DateTime<Utc>,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let key = self.buy_key(intent)?;
        self.prune(now);

        if let Some(last) = self.recent_buys.get(&key) {
            let elapsed_ms = now.signed_duration_since(*last).num_milliseconds().max(0);
            return Some(format!(
                "Duplicate buy intent blocked (elapsed={}ms, guard_window={}ms, key={})",
                elapsed_ms,
                self.window.num_milliseconds(),
                key
            ));
        }

        self.recent_buys.insert(key, now);
        None
    }
}

impl AdmissionController {
    pub(in crate::coordinator) async fn check_duplicate_intent(
        &self,
        intent: &OrderIntent,
    ) -> Option<String> {
        let mut guard = self.duplicate_guard.write().await;
        guard.register_or_block(intent, Utc::now())
    }

    pub(in crate::coordinator) fn build_order_request(
        &self,
        account_id: &str,
        intent: &OrderIntent,
    ) -> OrderRequest {
        let order_side = if intent.is_buy {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };

        let idempotency_key = self.stable_idempotency_key(account_id, intent);
        OrderRequest {
            client_order_id: format!("intent:{}", intent.intent_id),
            idempotency_key: Some(idempotency_key),
            token_id: intent.token_id.clone(),
            market_side: intent.side.clone(),
            order_side,
            shares: intent.shares,
            limit_price: intent.limit_price,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
        }
    }

    fn sanitize_idempotency_component(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        for ch in input.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.' | '|') {
                out.push(ch);
            } else {
                out.push('_');
            }
        }
        out
    }

    fn stable_idempotency_key(&self, account_id: &str, intent: &OrderIntent) -> String {
        if let Some(key) = intent
            .metadata
            .get("idempotency_key")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
        {
            return Self::sanitize_idempotency_component(key);
        }

        let scope = match intent
            .metadata
            .get("duplicate_guard_scope")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("deployment") | Some("dep") => DuplicateGuardScope::Deployment,
            Some("market") | Some("global") => DuplicateGuardScope::Market,
            _ => self.config.duplicate_guard_scope,
        };
        let scope_label = match scope {
            DuplicateGuardScope::Market => "market",
            DuplicateGuardScope::Deployment => "deployment",
        };
        let dep_label = match scope {
            DuplicateGuardScope::Market => "market".to_string(),
            DuplicateGuardScope::Deployment => IntentDuplicateGuard::deployment_scope(intent),
        };

        let window_secs = deployments::infer_time_bucket_seconds(intent);
        let ts = intent
            .metadata
            .get("event_time")
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(intent.created_at)
            .timestamp();
        let bucket = ts.div_euclid(window_secs);
        let side = intent.side.as_str();
        let order_kind = if intent.is_buy { "buy" } else { "sell" };

        Self::sanitize_idempotency_component(&format!(
            "acct:{account}|scope:{scope}|dep:{dep}|dom:{dom}|mkt:{mkt}|side:{side}|kind:{kind}|bucket:{bucket}",
            account = account_id,
            scope = scope_label,
            dep = dep_label,
            dom = intent.domain.to_string().to_ascii_lowercase(),
            mkt = intent_market_identity(intent),
            side = side.to_ascii_lowercase(),
            kind = order_kind,
            bucket = bucket,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn make_controller(scope: DuplicateGuardScope) -> AdmissionController {
        let mut config = CoordinatorConfig::default();
        config.duplicate_guard_enabled = true;
        config.duplicate_guard_window_ms = 10_000;
        config.duplicate_guard_scope = scope;
        AdmissionController::new(&config)
    }

    fn make_intent(is_buy: bool, priority: OrderPriority) -> OrderIntent {
        let mut intent = OrderIntent::new(
            "crypto_lob_ml",
            Domain::Crypto,
            "btc-updown-5m-123",
            "token-up-123",
            crate::domain::Side::Up,
            is_buy,
            100,
            dec!(0.42),
        );
        intent.priority = priority;
        intent
    }

    #[test]
    fn test_duplicate_guard_blocks_repeated_buy_within_window() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut guard = controller.duplicate_guard.blocking_write();
        let intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "crypto.pm.btc.5m.momentum");

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(
            guard
                .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
                .is_some()
        );
    }

    #[test]
    fn test_duplicate_guard_allows_after_window() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut guard = controller.duplicate_guard.blocking_write();
        let intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "crypto.pm.btc.5m.momentum");

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(
            guard
                .register_or_block(&intent, now + chrono::Duration::seconds(11))
                .is_none()
        );
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_even_if_token_differs() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let first = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "crypto.pm.btc.5m.momentum");
        let mut second = first.clone();
        second.token_id = "token-down-456".to_string();
        second.side = crate::domain::Side::Down;

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&first, now).is_none());
        assert!(
            guard
                .register_or_block(&second, now + chrono::Duration::milliseconds(10))
                .is_some()
        );
    }

    #[test]
    fn test_duplicate_guard_blocks_same_condition_with_different_slugs() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "sports.pm.nba.comeback");
        first.market_slug = "nba-lakers-celtics-v1".to_string();
        first.metadata.insert(
            "condition_id".to_string(),
            "0x1111000000000000000000000000000000000000000000000000000000000000".to_string(),
        );

        let mut second = first.clone();
        second.market_slug = "nba-lakers-celtics-v2".to_string();

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&first, now).is_none());
        assert!(
            guard
                .register_or_block(&second, now + chrono::Duration::milliseconds(10))
                .is_some()
        );
    }

    #[test]
    fn test_duplicate_guard_allows_same_market_for_different_deployments() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);

        first.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.momentum".to_string(),
        );
        second.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&first, now).is_none());
        assert!(
            guard
                .register_or_block(&second, now + chrono::Duration::milliseconds(100))
                .is_none()
        );
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_for_different_deployments_in_market_scope() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal);
        let mut second = make_intent(true, OrderPriority::Normal);

        first.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.momentum".to_string(),
        );
        second.metadata.insert(
            "deployment_id".to_string(),
            "crypto.pm.btc.15m.patternmem".to_string(),
        );

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&first, now).is_none());
        assert!(
            guard
                .register_or_block(&second, now + chrono::Duration::milliseconds(100))
                .is_some()
        );
    }

    #[test]
    fn test_duplicate_guard_does_not_block_sells() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(false, OrderPriority::Normal);

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(
            guard
                .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
                .is_none()
        );
    }

    #[test]
    fn test_duplicate_guard_skips_critical_orders() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Critical);

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(
            guard
                .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
                .is_none()
        );
    }

    #[test]
    fn test_build_order_request_uses_stable_idempotency_key_by_window() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let mut first = OrderIntent::new(
            "openclaw",
            Domain::Crypto,
            "btc-updown-15m-20260219-1200",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        first.metadata.insert(
            "condition_id".to_string(),
            "0x1111000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        first
            .metadata
            .insert("event_time".to_string(), "2026-02-20T12:00:00Z".to_string());

        let mut second = first.clone();
        second.market_slug = "nba-lakers-celtics-v2".to_string();

        let first_key = controller
            .build_order_request("acct-main", &first)
            .idempotency_key
            .expect("stable key");
        let second_key = controller
            .build_order_request("acct-main", &second)
            .idempotency_key
            .expect("stable key");
        assert_eq!(first_key, second_key);
    }

    #[test]
    fn test_build_order_request_fallback_uses_intent_created_at() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let mut first = OrderIntent::new(
            "openclaw",
            Domain::Crypto,
            "btc-updown-15m",
            "token-up-1",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        let mut second = first.clone();
        first.created_at = chrono::DateTime::parse_from_rfc3339("2026-02-19T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);
        second.created_at = chrono::DateTime::parse_from_rfc3339("2026-02-19T13:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        let first_key = controller
            .build_order_request("acct-main", &first)
            .idempotency_key
            .expect("stable key");
        let second_key = controller
            .build_order_request("acct-main", &second)
            .idempotency_key
            .expect("stable key");
        assert_ne!(first_key, second_key);
    }
}
