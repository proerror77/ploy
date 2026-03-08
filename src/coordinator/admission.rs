use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::domain::{OrderRequest, OrderSide, OrderType, TimeInForce};
use crate::platform::{Domain, MarketSelector, OrderIntent, OrderPriority, StrategyDeployment};

use super::capital::{
    intent_deployment_scope, intent_market_identity, CapitalPolicy, CryptoHorizon,
};
use super::config::{CoordinatorConfig, DuplicateGuardScope};

#[derive(Debug)]
struct IntentDuplicateGuard {
    enabled: bool,
    window: ChronoDuration,
    scope: DuplicateGuardScope,
    recent_buys: HashMap<String, chrono::DateTime<Utc>>,
}

impl IntentDuplicateGuard {
    fn new(window_ms: u64, enabled: bool, scope: DuplicateGuardScope) -> Self {
        let clamped_ms = window_ms.min(i64::MAX as u64) as i64;
        let window = ChronoDuration::milliseconds(clamped_ms.max(1));
        Self {
            enabled,
            window,
            scope,
            recent_buys: HashMap::new(),
        }
    }

    fn deployment_scope(intent: &OrderIntent) -> String {
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

    fn register_or_block(
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

pub(super) struct AdmissionController {
    config: CoordinatorConfig,
    duplicate_guard: RwLock<IntentDuplicateGuard>,
    deployments: Arc<RwLock<HashMap<String, StrategyDeployment>>>,
}

impl AdmissionController {
    pub(super) fn new(config: &CoordinatorConfig) -> Self {
        Self {
            config: config.clone(),
            duplicate_guard: RwLock::new(IntentDuplicateGuard::new(
                config.duplicate_guard_window_ms,
                config.duplicate_guard_enabled,
                config.duplicate_guard_scope,
            )),
            deployments: Arc::new(RwLock::new(Self::load_strategy_deployments())),
        }
    }

    pub(super) fn shared_deployments(&self) -> Arc<RwLock<HashMap<String, StrategyDeployment>>> {
        self.deployments.clone()
    }

    pub(super) async fn check_duplicate_intent(&self, intent: &OrderIntent) -> Option<String> {
        let mut guard = self.duplicate_guard.write().await;
        guard.register_or_block(intent, Utc::now())
    }

    pub(super) async fn enforce_live_buy_deployment_gate(
        &self,
        account_id: &str,
        dry_run: bool,
        allowed_domains: &HashSet<Domain>,
        intent: &mut OrderIntent,
    ) -> std::result::Result<(), String> {
        if !intent.is_buy || dry_run || !Self::deployment_gate_required() {
            return Ok(());
        }
        if !allowed_domains.contains(&intent.domain) {
            return Err(format!(
                "domain {} is not enabled for this runtime",
                intent.domain
            ));
        }

        let explicit_id =
            Self::metadata_value(&intent.metadata, &["deployment_id"]).map(ToString::to_string);
        let should_refresh = {
            let deployments = self.deployments.read().await;
            deployments.is_empty()
                || explicit_id
                    .as_ref()
                    .is_some_and(|id| !deployments.contains_key(id.as_str()))
        };
        if should_refresh {
            self.refresh_strategy_deployments().await;
        }

        let deployments = self.deployments.read().await;
        Self::enforce_deployment_gate_with_snapshot(account_id, dry_run, &deployments, intent)
    }

    pub(super) async fn apply_kelly_sizing(
        &self,
        capital_policy: &CapitalPolicy,
        intent: &mut OrderIntent,
    ) -> Option<String> {
        if !self.config.kelly_sizing_enabled {
            return None;
        }
        if !intent.is_buy {
            return None;
        }
        if intent.priority == OrderPriority::Critical {
            return None;
        }
        if intent.limit_price <= Decimal::ZERO || intent.limit_price >= Decimal::ONE {
            return None;
        }

        let p = intent
            .metadata
            .get("signal_fair_value")
            .or_else(|| intent.metadata.get("signal_win_prob"))
            .and_then(|v| Decimal::from_str(v).ok())?;
        let p = p.max(Decimal::ZERO).min(Decimal::ONE);
        let price = intent.limit_price;
        let edge = p - price;

        if edge < self.config.kelly_min_edge {
            return Some(format!(
                "kelly edge {} below min {}",
                edge, self.config.kelly_min_edge
            ));
        }

        let denom = Decimal::ONE - price;
        if denom <= Decimal::ZERO {
            return Some("kelly denom <= 0".to_string());
        }

        let raw_kelly = ((p - price) / denom).max(Decimal::ZERO).min(Decimal::ONE);
        if raw_kelly <= Decimal::ZERO {
            return Some("kelly fraction <= 0 (no positive edge)".to_string());
        }

        let mut effective_fraction = (raw_kelly * self.config.kelly_fraction_multiplier)
            .max(Decimal::ZERO)
            .min(Decimal::ONE);
        if let Some(conf) = intent
            .metadata
            .get("signal_confidence")
            .and_then(|v| Decimal::from_str(v).ok())
        {
            effective_fraction *= conf.max(Decimal::ZERO).min(Decimal::ONE);
        }

        if effective_fraction <= Decimal::ZERO {
            return Some("kelly effective fraction <= 0".to_string());
        }

        let bankroll = capital_policy
            .available_notional_for(intent)
            .await
            .unwrap_or_else(|| intent.notional_value());

        if bankroll <= Decimal::ZERO {
            return Some("kelly bankroll <= 0".to_string());
        }

        let target_notional = (bankroll * effective_fraction).max(Decimal::ZERO);
        if target_notional <= Decimal::ZERO {
            return Some("kelly target_notional <= 0".to_string());
        }

        let sized_shares = (target_notional / price)
            .floor()
            .to_u64()
            .unwrap_or(0)
            .min(intent.shares);

        let mut final_shares = sized_shares;
        if final_shares == 0 {
            let floor_shares = self.config.kelly_min_shares.min(intent.shares);
            if floor_shares > 0 {
                final_shares = floor_shares;
                intent
                    .metadata
                    .insert("kelly_min_shares_applied".to_string(), "true".to_string());
                intent.metadata.insert(
                    "kelly_min_shares_floor".to_string(),
                    floor_shares.to_string(),
                );
            } else {
                return Some("kelly sizing produced 0 shares".to_string());
            }
        }

        if final_shares < intent.shares {
            intent.shares = final_shares;
        }

        intent
            .metadata
            .insert("kelly_fraction_raw".to_string(), raw_kelly.to_string());
        intent.metadata.insert(
            "kelly_fraction_multiplier".to_string(),
            self.config.kelly_fraction_multiplier.to_string(),
        );
        intent.metadata.insert(
            "kelly_fraction_effective".to_string(),
            effective_fraction.to_string(),
        );
        intent
            .metadata
            .insert("kelly_bankroll_usd".to_string(), bankroll.to_string());
        intent.metadata.insert(
            "kelly_target_notional_usd".to_string(),
            target_notional.to_string(),
        );
        intent
            .metadata
            .insert("kelly_sized_shares".to_string(), sized_shares.to_string());
        if final_shares != sized_shares {
            intent
                .metadata
                .insert("kelly_final_shares".to_string(), final_shares.to_string());
        }

        None
    }

    pub(super) fn apply_min_order_constraints(
        &self,
        intent: &mut OrderIntent,
        strategy_max_shares: u64,
    ) -> Option<String> {
        if !intent.is_buy {
            return None;
        }
        if intent.priority == OrderPriority::Critical {
            return None;
        }
        if intent.limit_price <= Decimal::ZERO {
            return None;
        }

        let min_shares_cfg = self.config.min_order_shares.max(1);
        let min_notional = self.config.min_order_notional_usd.max(Decimal::ZERO);

        let mut required_shares = min_shares_cfg;
        if min_notional > Decimal::ZERO {
            let min_shares_for_notional = (min_notional / intent.limit_price)
                .ceil()
                .to_u64()
                .unwrap_or(u64::MAX);
            required_shares = required_shares.max(min_shares_for_notional);
        }

        if required_shares <= 1 {
            return None;
        }

        if required_shares > strategy_max_shares {
            return Some(format!(
                "venue minimum requires {} shares (min_shares={}, min_notional_usd={}) but strategy_max_shares={}",
                required_shares, min_shares_cfg, min_notional, strategy_max_shares
            ));
        }

        if intent.shares < required_shares {
            let before = intent.shares;
            intent.shares = required_shares;
            intent
                .metadata
                .insert("venue_min_order_applied".to_string(), "true".to_string());
            intent.metadata.insert(
                "venue_min_order_before_shares".to_string(),
                before.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_required_shares".to_string(),
                required_shares.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_min_shares".to_string(),
                min_shares_cfg.to_string(),
            );
            intent.metadata.insert(
                "venue_min_order_min_notional_usd".to_string(),
                min_notional.to_string(),
            );
        }

        None
    }

    pub(super) fn build_order_request(
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

    fn refresh_strategy_deployments(&self) -> impl std::future::Future<Output = ()> + '_ {
        async move {
            let loaded = Self::load_strategy_deployments();
            let mut deployments = self.deployments.write().await;
            *deployments = loaded;
        }
    }

    fn deployment_gate_required() -> bool {
        match std::env::var("PLOY_DEPLOYMENT_GATE_REQUIRED")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
        {
            Some(v) => !matches!(v.as_str(), "0" | "false" | "no" | "off"),
            None => true,
        }
    }

    fn deployments_state_path() -> PathBuf {
        if let Ok(path) = std::env::var("PLOY_DEPLOYMENTS_FILE") {
            return PathBuf::from(path);
        }

        let container_data_root = Path::new("/opt/ploy/data");
        if container_data_root.exists() {
            return container_data_root.join("state/deployments.json");
        }

        let repo_state_deployment = Path::new("data/state/deployments.json");
        if repo_state_deployment.exists() {
            return repo_state_deployment.to_path_buf();
        }

        let repo_root_deployment = Path::new("deployment/deployments.json");
        if repo_root_deployment.exists() {
            return repo_root_deployment.to_path_buf();
        }

        let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
        if container_deployment.exists() {
            return container_deployment.to_path_buf();
        }

        PathBuf::from("data/state/deployments.json")
    }

    fn parse_strategy_deployments(raw: &str) -> HashMap<String, StrategyDeployment> {
        let mut out = HashMap::new();
        if let Ok(items) = serde_json::from_str::<Vec<StrategyDeployment>>(raw) {
            for mut dep in items {
                let id = dep.id.trim().to_string();
                if id.is_empty() {
                    continue;
                }
                dep.id = id.clone();
                dep.normalize_account_ids_in_place();
                out.insert(id, dep);
            }
        }
        out
    }

    fn load_strategy_deployments() -> HashMap<String, StrategyDeployment> {
        let raw = std::env::var("PLOY_STRATEGY_DEPLOYMENTS_JSON")
            .or_else(|_| std::env::var("PLOY_DEPLOYMENTS_JSON"))
            .unwrap_or_default();
        if !raw.trim().is_empty() {
            return Self::parse_strategy_deployments(&raw);
        }

        let repo_state_path = Path::new("data/state/deployments.json");
        let container_data_path = Path::new("/opt/ploy/data/state/deployments.json");
        let candidates = [
            Self::deployments_state_path(),
            repo_state_path.to_path_buf(),
            container_data_path.to_path_buf(),
            Path::new("deployment/deployments.json").to_path_buf(),
            Path::new("/opt/ploy/deployment/deployments.json").to_path_buf(),
        ];

        for path in candidates {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let parsed = Self::parse_strategy_deployments(&contents);
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }

        HashMap::new()
    }

    fn metadata_value<'a>(metadata: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
        keys.iter()
            .find_map(|k| metadata.get(*k))
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }

    fn normalized_token(raw: &str) -> String {
        raw.trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect()
    }

    fn strategy_matches(intent_strategy: &str, deployment_strategy: &str) -> bool {
        let intent = Self::normalized_token(intent_strategy);
        let dep = Self::normalized_token(deployment_strategy);
        if intent.is_empty() || dep.is_empty() {
            return false;
        }
        intent == dep || intent.contains(&dep) || dep.contains(&intent)
    }

    fn selector_matches_intent(
        deployment: &StrategyDeployment,
        market_slug: &str,
        metadata: &HashMap<String, String>,
    ) -> bool {
        match &deployment.market_selector {
            MarketSelector::Static {
                symbol,
                series_id,
                market_slug: expected_market_slug,
            } => {
                if let Some(expected) = expected_market_slug
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    if !market_slug.eq_ignore_ascii_case(expected) {
                        return false;
                    }
                }

                if let Some(expected) = symbol.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    if let Some(actual) = Self::metadata_value(metadata, &["symbol"]) {
                        if !actual.eq_ignore_ascii_case(expected) {
                            return false;
                        }
                    }
                }

                if let Some(expected) = series_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    if let Some(actual) =
                        Self::metadata_value(metadata, &["series_id", "event_series_id"])
                    {
                        if !actual.eq_ignore_ascii_case(expected) {
                            return false;
                        }
                    }
                }

                true
            }
            MarketSelector::Dynamic { domain, .. } => *domain == deployment.domain,
        }
    }

    fn timeframe_hint(intent: &OrderIntent) -> Option<String> {
        if let Some(raw) = Self::metadata_value(&intent.metadata, &["timeframe", "horizon"]) {
            if let Some(h) = CryptoHorizon::from_hint(raw) {
                return Some(h.as_str().to_string());
            }
            return Some(raw.to_ascii_lowercase());
        }

        if let Some(raw) = Self::metadata_value(&intent.metadata, &["series_id", "event_series_id"])
        {
            if let Some(h) = CryptoHorizon::from_hint(raw) {
                return Some(h.as_str().to_string());
            }
        }

        CryptoHorizon::from_hint(&intent.market_slug).map(|h| h.as_str().to_string())
    }

    fn deployment_matches_timeframe(deployment: &StrategyDeployment, intent: &OrderIntent) -> bool {
        let Some(timeframe) = Self::timeframe_hint(intent) else {
            return true;
        };
        timeframe.eq_ignore_ascii_case(deployment.timeframe.as_str())
    }

    fn deployment_runtime_eligible(
        deployment: &StrategyDeployment,
        account_id: &str,
        dry_run: bool,
        intent: &OrderIntent,
    ) -> bool {
        deployment.is_enabled_for_runtime(account_id, dry_run)
            && deployment.domain == intent.domain
            && Self::deployment_matches_timeframe(deployment, intent)
            && Self::selector_matches_intent(deployment, &intent.market_slug, &intent.metadata)
    }

    fn apply_deployment_metadata(intent: &mut OrderIntent, deployment: &StrategyDeployment) {
        intent
            .metadata
            .insert("deployment_id".to_string(), deployment.id.clone());
        intent
            .metadata
            .entry("timeframe".to_string())
            .or_insert_with(|| deployment.timeframe.as_str().to_string());
        intent
            .metadata
            .entry("allocator_profile".to_string())
            .or_insert_with(|| deployment.allocator_profile.clone());
        intent
            .metadata
            .entry("risk_profile".to_string())
            .or_insert_with(|| deployment.risk_profile.clone());
        intent
            .metadata
            .entry("deployment_strategy".to_string())
            .or_insert_with(|| deployment.strategy.clone());
        intent
            .metadata
            .entry("deployment_priority".to_string())
            .or_insert_with(|| deployment.priority.to_string());
        intent
            .metadata
            .entry("deployment_cooldown_secs".to_string())
            .or_insert_with(|| deployment.cooldown_secs.to_string());

        if let MarketSelector::Static {
            symbol,
            series_id,
            market_slug,
        } = &deployment.market_selector
        {
            if let Some(value) = symbol.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                intent
                    .metadata
                    .entry("symbol".to_string())
                    .or_insert_with(|| value.to_string());
            }
            if let Some(value) = series_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                intent
                    .metadata
                    .entry("series_id".to_string())
                    .or_insert_with(|| value.to_string());
                intent
                    .metadata
                    .entry("event_series_id".to_string())
                    .or_insert_with(|| value.to_string());
            }
            if let Some(value) = market_slug
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                intent
                    .metadata
                    .entry("selector_market_slug".to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
    }

    fn enforce_deployment_gate_with_snapshot(
        account_id: &str,
        dry_run: bool,
        deployments: &HashMap<String, StrategyDeployment>,
        intent: &mut OrderIntent,
    ) -> std::result::Result<(), String> {
        if !intent.is_buy || dry_run || !Self::deployment_gate_required() {
            return Ok(());
        }

        if deployments.is_empty() {
            return Err(
                "deployment registry is empty while deployment gate is required".to_string(),
            );
        }

        if let Some(deployment_id) = Self::metadata_value(&intent.metadata, &["deployment_id"]) {
            let Some(deployment) = deployments.get(deployment_id) else {
                return Err(format!("unknown deployment_id: {}", deployment_id));
            };
            if !Self::deployment_runtime_eligible(deployment, account_id, dry_run, intent) {
                return Err(format!(
                    "deployment {} is not eligible for runtime/account/domain/timeframe/selector binding",
                    deployment.id
                ));
            }
            Self::apply_deployment_metadata(intent, deployment);
            return Ok(());
        }

        let strategy = Self::metadata_value(&intent.metadata, &["strategy", "deployment_strategy"])
            .ok_or_else(|| "strategy metadata is required for live BUY intents".to_string())?;

        let mut candidates: Vec<&StrategyDeployment> = deployments
            .values()
            .filter(|deployment| {
                Self::deployment_runtime_eligible(deployment, account_id, dry_run, intent)
                    && Self::strategy_matches(strategy, deployment.strategy.as_str())
            })
            .collect();

        if candidates.is_empty() {
            let mut domain_candidates: Vec<&StrategyDeployment> = deployments
                .values()
                .filter(|deployment| {
                    Self::deployment_runtime_eligible(deployment, account_id, dry_run, intent)
                })
                .collect();
            domain_candidates.sort_by(|a, b| a.id.cmp(&b.id));

            if domain_candidates.len() == 1 {
                let deployment = domain_candidates[0];
                Self::apply_deployment_metadata(intent, deployment);
                intent.metadata.insert(
                    "deployment_resolution".to_string(),
                    "domain_singleton_fallback".to_string(),
                );
                return Ok(());
            }

            return Err(format!(
                "no eligible deployment found for strategy={} domain={} market={}",
                strategy, intent.domain, intent.market_slug
            ));
        }

        candidates.sort_by(|a, b| a.id.cmp(&b.id));

        if candidates.len() > 1 {
            let ids = candidates
                .iter()
                .map(|d| d.id.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "ambiguous deployment resolution for strategy={} market={}: {}",
                strategy, intent.market_slug, ids
            ));
        }

        let deployment = candidates[0];
        Self::apply_deployment_metadata(intent, deployment);
        Ok(())
    }

    fn infer_time_bucket_seconds(intent: &OrderIntent) -> i64 {
        if let Some(raw) = intent.metadata.get("event_window_secs") {
            if let Ok(v) = raw.trim().parse::<i64>() {
                if v > 0 {
                    return v;
                }
            }
        }

        let mut hints: Vec<&str> = Vec::new();
        if let Some(h) = intent.metadata.get("timeframe") {
            hints.push(h.as_str());
        }
        if let Some(h) = intent.metadata.get("horizon") {
            hints.push(h.as_str());
        }
        if let Some(h) = intent.metadata.get("series_id") {
            hints.push(h.as_str());
        }

        for raw in hints {
            if let Some(horizon) = CryptoHorizon::from_hint(raw) {
                return match horizon {
                    CryptoHorizon::M15 => 15 * 60,
                    CryptoHorizon::M5 => 5 * 60,
                    CryptoHorizon::Other => 5 * 60,
                };
            }
        }

        5 * 60
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

        let window_secs = Self::infer_time_bucket_seconds(intent);
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

pub(super) fn buy_intent_missing_deployment_reason(intent: &OrderIntent) -> Option<String> {
    if !intent.is_buy {
        return None;
    }

    let has_deployment_id = intent.deployment_id().is_some();

    if has_deployment_id {
        None
    } else {
        Some("BUY intent missing required metadata field 'deployment_id'".to_string())
    }
}

pub(super) fn sell_reduce_only_violation_reason(
    intent: &OrderIntent,
    tracked_open_shares: u64,
    pending_sell_shares: u64,
) -> Option<String> {
    if intent.is_buy {
        return None;
    }

    if tracked_open_shares == 0 {
        return Some(format!(
            "SELL intent reduce-only violation: no tracked open shares for token_id={} side={} in domain={}",
            intent.token_id,
            intent.side.as_str(),
            intent.domain
        ));
    }

    let available_shares = tracked_open_shares.saturating_sub(pending_sell_shares);
    if available_shares == 0 {
        return Some(format!(
            "SELL intent reduce-only violation: tracked open shares {} are fully reserved by pending SELL intents {} for token_id={} side={}",
            tracked_open_shares,
            pending_sell_shares,
            intent.token_id,
            intent.side.as_str()
        ));
    }

    if intent.shares > available_shares {
        return Some(format!(
            "SELL intent reduce-only violation: requested shares {} exceeds available reduce-only shares {} (tracked={}, pending_sell={}) for token_id={} side={}",
            intent.shares,
            available_shares,
            tracked_open_shares,
            pending_sell_shares,
            intent.token_id,
            intent.side.as_str()
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{
        DeploymentExecutionMode, StrategyLifecycleStage, StrategyProductType, Timeframe,
    };
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

    fn make_deployment(
        id: &str,
        strategy: &str,
        domain: Domain,
        timeframe: Timeframe,
        execution_mode: DeploymentExecutionMode,
    ) -> StrategyDeployment {
        StrategyDeployment {
            id: id.to_string(),
            strategy: strategy.to_string(),
            strategy_version: "test".to_string(),
            domain,
            market_selector: MarketSelector::Dynamic {
                domain,
                query: None,
                min_liquidity_usd: None,
                max_spread_bps: None,
                min_time_remaining_secs: None,
                max_time_remaining_secs: None,
            },
            timeframe,
            enabled: true,
            allocator_profile: "default".to_string(),
            risk_profile: "default".to_string(),
            priority: 50,
            cooldown_secs: 60,
            account_ids: Vec::new(),
            execution_mode,
            lifecycle_stage: StrategyLifecycleStage::Live,
            product_type: StrategyProductType::BinaryOption,
            last_evaluated_at: None,
            last_evaluation_score: None,
        }
    }

    #[test]
    fn test_buy_intent_requires_deployment_id_metadata() {
        let intent = make_intent(true, OrderPriority::Normal);
        let reason = buy_intent_missing_deployment_reason(&intent);
        assert_eq!(
            reason.as_deref(),
            Some("BUY intent missing required metadata field 'deployment_id'")
        );
    }

    #[test]
    fn test_sell_intent_does_not_require_deployment_id_metadata() {
        let intent = make_intent(false, OrderPriority::Normal);
        assert!(buy_intent_missing_deployment_reason(&intent).is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_repeated_buy_within_window() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut guard = controller.duplicate_guard.blocking_write();
        let intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "crypto.pm.btc.5m.momentum");

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_allows_after_window() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut guard = controller.duplicate_guard.blocking_write();
        let intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "crypto.pm.btc.5m.momentum");

        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::seconds(11))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_blocks_same_market_even_if_token_differs() {
        let controller = make_controller(DuplicateGuardScope::Deployment);
        let now = Utc::now();
        let mut first = make_intent(true, OrderPriority::Normal)
            .with_metadata("deployment_id", "crypto.pm.btc.5m.momentum");
        let mut second = first.clone();
        second.token_id = "token-down-456".to_string();
        second.side = crate::domain::Side::Down;

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&first, now).is_none());
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(10))
            .is_some());
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
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(10))
            .is_some());
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
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_none());
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
        assert!(guard
            .register_or_block(&second, now + chrono::Duration::milliseconds(100))
            .is_some());
    }

    #[test]
    fn test_duplicate_guard_does_not_block_sells() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(false, OrderPriority::Normal);

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_duplicate_guard_skips_critical_orders() {
        let controller = make_controller(DuplicateGuardScope::Market);
        let now = Utc::now();
        let intent = make_intent(true, OrderPriority::Critical);

        let mut guard = controller.duplicate_guard.blocking_write();
        assert!(guard.register_or_block(&intent, now).is_none());
        assert!(guard
            .register_or_block(&intent, now + chrono::Duration::milliseconds(10))
            .is_none());
    }

    #[test]
    fn test_deployment_gate_blocks_live_buy_without_strategy_metadata() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::LiveOnly,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal);
        let result = AdmissionController::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("strategy metadata is required"));
    }

    #[test]
    fn test_deployment_gate_accepts_explicit_deployment_and_applies_metadata() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::LiveOnly,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "crypto_momentum")
            .with_metadata("deployment_id", "crypto-momentum-15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = AdmissionController::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_ok());
        assert_eq!(
            intent.metadata.get("deployment_id").map(String::as_str),
            Some("crypto-momentum-15m")
        );
        assert_eq!(
            intent.metadata.get("timeframe").map(String::as_str),
            Some("15m")
        );
    }

    #[test]
    fn test_deployment_gate_blocks_ambiguous_inferred_deployments() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-a".to_string(),
            make_deployment(
                "crypto-momentum-a",
                "momentum",
                Domain::Crypto,
                Timeframe::Other("other".to_string()),
                DeploymentExecutionMode::Any,
            ),
        );
        deployments.insert(
            "crypto-momentum-b".to_string(),
            make_deployment(
                "crypto-momentum-b",
                "momentum",
                Domain::Crypto,
                Timeframe::Other("other".to_string()),
                DeploymentExecutionMode::Any,
            ),
        );

        let mut intent =
            make_intent(true, OrderPriority::Normal).with_metadata("strategy", "momentum");
        intent.market_slug = "btc-updown-unknown".to_string();

        let result = AdmissionController::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("ambiguous deployment resolution"));
    }

    #[test]
    fn test_deployment_gate_blocks_runtime_scope_mismatch() {
        let mut deployment = make_deployment(
            "crypto-momentum-15m",
            "momentum",
            Domain::Crypto,
            Timeframe::M15,
            DeploymentExecutionMode::DryRunOnly,
        );
        deployment.account_ids = vec!["acct-b".to_string()];

        let mut deployments = HashMap::new();
        deployments.insert("crypto-momentum-15m".to_string(), deployment);

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "momentum")
            .with_metadata("deployment_id", "crypto-momentum-15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = AdmissionController::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not eligible"));
    }

    #[test]
    fn test_deployment_gate_infers_unique_by_timeframe_hint() {
        let mut deployments = HashMap::new();
        deployments.insert(
            "crypto-momentum-5m".to_string(),
            make_deployment(
                "crypto-momentum-5m",
                "momentum",
                Domain::Crypto,
                Timeframe::M5,
                DeploymentExecutionMode::Any,
            ),
        );
        deployments.insert(
            "crypto-momentum-15m".to_string(),
            make_deployment(
                "crypto-momentum-15m",
                "momentum",
                Domain::Crypto,
                Timeframe::M15,
                DeploymentExecutionMode::Any,
            ),
        );

        let mut intent = make_intent(true, OrderPriority::Normal)
            .with_metadata("strategy", "crypto_momentum")
            .with_metadata("horizon", "15m");
        intent.market_slug = "btc-updown-15m-xyz".to_string();

        let result = AdmissionController::enforce_deployment_gate_with_snapshot(
            "acct-a",
            false,
            &deployments,
            &mut intent,
        );

        assert!(result.is_ok());
        assert_eq!(
            intent.metadata.get("deployment_id").map(String::as_str),
            Some("crypto-momentum-15m")
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
