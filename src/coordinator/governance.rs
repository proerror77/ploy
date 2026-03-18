use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};

use chrono::Utc;
use tokio::sync::RwLock;

use crate::domain::Domain;

use super::command::{
    DomainIngressSnapshot, GovernancePolicyHistoryEntry, GovernancePolicySnapshot,
    GovernancePolicyUpdate,
};
use super::config::CoordinatorConfig;
use super::OrderIntent;

#[path = "governance/persistence.rs"]
mod persistence;

pub(super) use persistence::{
    load_governance_policy, load_governance_policy_history, persist_governance_policy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IngressMode {
    Running,
    Paused,
    Halted,
}

impl IngressMode {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Halted => "halted",
        }
    }
}

fn parse_ingress_mode(raw: &str) -> Option<IngressMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "running" => Some(IngressMode::Running),
        "paused" => Some(IngressMode::Paused),
        "halted" => Some(IngressMode::Halted),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub(super) struct GovernancePolicy {
    pub(super) block_new_intents: bool,
    pub(super) blocked_domains: HashSet<Domain>,
    pub(super) max_intent_notional_usd: Option<Decimal>,
    pub(super) max_total_notional_usd: Option<Decimal>,
    pub(super) updated_at: chrono::DateTime<Utc>,
    pub(super) updated_by: String,
    pub(super) reason: Option<String>,
    pub(super) metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GovernanceRuntimeStateSnapshot {
    pub(super) ingress_mode: IngressMode,
    pub(super) domain_ingress_modes: HashMap<Domain, IngressMode>,
    pub(super) paused_agent_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct GovernanceIntentSnapshot {
    pub(super) global_mode: IngressMode,
    pub(super) domain_mode: IngressMode,
    pub(super) agent_paused: bool,
    pub(super) policy: GovernancePolicy,
}

#[derive(Debug, Clone)]
pub(super) struct PersistedGovernanceState {
    pub(super) policy: GovernancePolicy,
    pub(super) runtime_state: GovernanceRuntimeStateSnapshot,
}

#[derive(Debug, Clone)]
struct GovernanceState {
    ingress_mode: IngressMode,
    domain_ingress_modes: HashMap<Domain, IngressMode>,
    policy: GovernancePolicy,
    paused_agent_ids: HashSet<String>,
}

impl GovernancePolicy {
    pub(super) fn from_config(config: &CoordinatorConfig) -> Self {
        let blocked_domains = config
            .governance_blocked_domains
            .iter()
            .filter_map(|raw| parse_governance_domain(raw))
            .collect::<HashSet<_>>();

        Self {
            block_new_intents: config.governance_block_new_intents,
            blocked_domains,
            max_intent_notional_usd: config.governance_max_intent_notional_usd,
            max_total_notional_usd: config.governance_max_total_notional_usd,
            updated_at: Utc::now(),
            updated_by: "boot".to_string(),
            reason: Some("loaded from coordinator config".to_string()),
            metadata: HashMap::new(),
        }
    }

    pub(super) fn try_from_update(
        update: GovernancePolicyUpdate,
    ) -> std::result::Result<Self, String> {
        let mut blocked_domains = HashSet::new();
        for raw in &update.blocked_domains {
            let Some(domain) = parse_governance_domain(raw) else {
                return Err(format!("unknown blocked domain '{}'", raw));
            };
            blocked_domains.insert(domain);
        }

        if update.updated_by.trim().is_empty() {
            return Err("updated_by is required".to_string());
        }

        if let Some(v) = update.max_intent_notional_usd {
            if v <= Decimal::ZERO {
                return Err("max_intent_notional_usd must be > 0".to_string());
            }
        }
        if let Some(v) = update.max_total_notional_usd {
            if v <= Decimal::ZERO {
                return Err("max_total_notional_usd must be > 0".to_string());
            }
        }

        Ok(Self {
            block_new_intents: update.block_new_intents,
            blocked_domains,
            max_intent_notional_usd: update.max_intent_notional_usd,
            max_total_notional_usd: update.max_total_notional_usd,
            updated_at: Utc::now(),
            updated_by: update.updated_by.trim().to_string(),
            reason: update.reason.and_then(|v| {
                let trimmed = v.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }),
            metadata: update.metadata,
        })
    }

    pub(super) fn to_snapshot(&self) -> GovernancePolicySnapshot {
        let mut blocked_domains = self
            .blocked_domains
            .iter()
            .map(|d| governance_domain_label(*d).to_string())
            .collect::<Vec<_>>();
        blocked_domains.sort();
        GovernancePolicySnapshot {
            block_new_intents: self.block_new_intents,
            blocked_domains,
            max_intent_notional_usd: self.max_intent_notional_usd,
            max_total_notional_usd: self.max_total_notional_usd,
            updated_at: self.updated_at,
            updated_by: self.updated_by.clone(),
            reason: self.reason.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

pub(super) fn governance_block_reason(
    policy: &GovernancePolicy,
    intent: &crate::coordinator::OrderIntent,
    current_account_notional: Decimal,
) -> Option<String> {
    if !intent.is_buy {
        return None;
    }

    if policy.block_new_intents {
        return Some("global governance policy blocks new intents".to_string());
    }

    if policy.blocked_domains.contains(&intent.domain) {
        return Some(format!(
            "domain '{}' is blocked by global governance policy",
            governance_domain_label(intent.domain)
        ));
    }

    let intent_notional = intent.notional_value();
    if let Some(max_intent) = policy.max_intent_notional_usd {
        if intent_notional > max_intent {
            return Some(format!(
                "intent notional {} exceeds governance max_intent_notional_usd {}",
                intent_notional, max_intent
            ));
        }
    }

    if let Some(max_total) = policy.max_total_notional_usd {
        let projected = current_account_notional + intent_notional;
        if projected > max_total {
            return Some(format!(
                "projected account notional {} exceeds governance max_total_notional_usd {}",
                projected, max_total
            ));
        }
    }

    None
}

pub(super) struct GovernanceController {
    state: RwLock<GovernanceState>,
}

impl GovernanceController {
    pub(super) fn new(config: &CoordinatorConfig) -> Self {
        Self {
            state: RwLock::new(GovernanceState {
                ingress_mode: IngressMode::Running,
                domain_ingress_modes: HashMap::new(),
                policy: GovernancePolicy::from_config(config),
                paused_agent_ids: HashSet::new(),
            }),
        }
    }

    pub(super) async fn intent_snapshot(&self, intent: &OrderIntent) -> GovernanceIntentSnapshot {
        let state = self.state.read().await;
        GovernanceIntentSnapshot {
            global_mode: state.ingress_mode,
            domain_mode: state
                .domain_ingress_modes
                .get(&intent.domain)
                .copied()
                .unwrap_or(IngressMode::Running),
            agent_paused: state.paused_agent_ids.contains(&intent.agent_id),
            policy: state.policy.clone(),
        }
    }

    pub(super) async fn ingress_mode_label(&self) -> String {
        self.state.read().await.ingress_mode.as_str().to_string()
    }

    pub(super) async fn domain_ingress_snapshot_rows(&self) -> Vec<DomainIngressSnapshot> {
        let state = self.state.read().await;
        let mut rows = state
            .domain_ingress_modes
            .iter()
            .map(|(domain, mode)| DomainIngressSnapshot {
                domain: governance_domain_snapshot_label(*domain),
                mode: mode.as_str().to_string(),
            })
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| a.domain.cmp(&b.domain));
        rows
    }

    pub(super) async fn set_global_mode(&self, mode: IngressMode) {
        let mut state = self.state.write().await;
        state.ingress_mode = mode;
        state.domain_ingress_modes.clear();
    }

    pub(super) async fn set_domain_mode(&self, domain: Domain, mode: IngressMode) {
        let mut state = self.state.write().await;
        match mode {
            IngressMode::Running => {
                state.domain_ingress_modes.remove(&domain);
            }
            _ => {
                state.domain_ingress_modes.insert(domain, mode);
            }
        }
    }

    pub(super) async fn pause_agent(&self, agent_id: &str) {
        self.state
            .write()
            .await
            .paused_agent_ids
            .insert(agent_id.to_string());
    }

    pub(super) async fn resume_agent(&self, agent_id: &str) {
        self.state.write().await.paused_agent_ids.remove(agent_id);
    }

    pub(super) async fn paused_agent_ids_sorted(&self) -> Vec<String> {
        let state = self.state.read().await;
        let mut paused = state.paused_agent_ids.iter().cloned().collect::<Vec<_>>();
        paused.sort();
        paused
    }

    pub(super) async fn policy_snapshot(&self) -> GovernancePolicySnapshot {
        self.state.read().await.policy.to_snapshot()
    }

    pub(super) async fn current_policy(&self) -> GovernancePolicy {
        self.state.read().await.policy.clone()
    }

    pub(super) async fn replace_policy(&self, next: GovernancePolicy) -> GovernancePolicySnapshot {
        let snapshot = next.to_snapshot();
        self.state.write().await.policy = next;
        snapshot
    }

    pub(super) async fn runtime_state_snapshot(&self) -> GovernanceRuntimeStateSnapshot {
        let state = self.state.read().await;
        GovernanceRuntimeStateSnapshot {
            ingress_mode: state.ingress_mode,
            domain_ingress_modes: state.domain_ingress_modes.clone(),
            paused_agent_ids: state.paused_agent_ids.clone(),
        }
    }

    pub(super) async fn restore_runtime_state(&self, snapshot: &GovernanceRuntimeStateSnapshot) {
        let mut state = self.state.write().await;
        state.ingress_mode = snapshot.ingress_mode;
        state.domain_ingress_modes = snapshot.domain_ingress_modes.clone();
        state.paused_agent_ids = snapshot.paused_agent_ids.clone();
    }
}

fn parse_governance_domain(raw: &str) -> Option<Domain> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "sports" => Some(Domain::Sports),
        "crypto" => Some(Domain::Crypto),
        "politics" => Some(Domain::Politics),
        "economics" => Some(Domain::Economics),
        _ => None,
    }
}

pub(super) fn governance_domain_label(domain: Domain) -> &'static str {
    match domain {
        Domain::Sports => "sports",
        Domain::Crypto => "crypto",
        Domain::Politics => "politics",
        Domain::Economics => "economics",
        Domain::Custom(_) => "custom",
    }
}

pub(super) fn governance_domain_snapshot_label(domain: Domain) -> String {
    match domain {
        Domain::Custom(id) => format!("custom:{}", id),
        _ => governance_domain_label(domain).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::persistence::clamp_governance_history_limit;
    use super::*;
    use crate::coordinator::OrderIntent;
    use crate::persistence::{
        ensure_coordinator_governance_policies_table,
        ensure_coordinator_governance_policy_history_table,
    };
    use rust_decimal_macros::dec;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn test_governance_policy_update_rejects_unknown_domain() {
        let update = GovernancePolicyUpdate {
            block_new_intents: false,
            blocked_domains: vec!["unknown".to_string()],
            max_intent_notional_usd: None,
            max_total_notional_usd: None,
            updated_by: "openclaw".to_string(),
            reason: None,
            metadata: HashMap::new(),
        };

        let parsed = GovernancePolicy::try_from_update(update);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_governance_policy_blocks_domain() {
        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: false,
            blocked_domains: vec!["sports".to_string()],
            max_intent_notional_usd: None,
            max_total_notional_usd: None,
            updated_by: "openclaw".to_string(),
            reason: Some("maintenance".to_string()),
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );
        let reason = governance_block_reason(&policy, &intent, dec!(0));
        assert!(reason.is_some());
    }

    #[test]
    fn test_governance_policy_blocks_projected_total_notional() {
        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: false,
            blocked_domains: vec![],
            max_intent_notional_usd: Some(dec!(50)),
            max_total_notional_usd: Some(dec!(100)),
            updated_by: "openclaw".to_string(),
            reason: None,
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let intent = OrderIntent::new(
            "crypto",
            Domain::Crypto,
            "btc-updown-5m-1",
            "token-up",
            crate::domain::Side::Up,
            true,
            50,
            dec!(0.50),
        );
        let reason = governance_block_reason(&policy, &intent, dec!(90));
        assert!(reason
            .unwrap_or_default()
            .contains("max_total_notional_usd"));
    }

    #[test]
    fn test_governance_policy_allows_sell_when_new_intents_blocked() {
        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: true,
            blocked_domains: vec!["sports".to_string()],
            max_intent_notional_usd: Some(dec!(1)),
            max_total_notional_usd: Some(dec!(1)),
            updated_by: "openclaw".to_string(),
            reason: Some("circuit".to_string()),
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let intent = OrderIntent::new(
            "sports",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            false,
            10,
            dec!(0.45),
        );
        let reason = governance_block_reason(&policy, &intent, dec!(999));
        assert!(reason.is_none(), "sell intent should remain allowed");
    }

    #[test]
    fn test_clamp_governance_history_limit_bounds() {
        assert_eq!(clamp_governance_history_limit(0), 1);
        assert_eq!(clamp_governance_history_limit(25), 25);
        assert_eq!(clamp_governance_history_limit(999), 500);
    }

    #[tokio::test]
    async fn test_governance_runtime_state_snapshot_roundtrip() {
        let controller = GovernanceController::new(&CoordinatorConfig::default());
        controller.set_global_mode(IngressMode::Paused).await;
        controller
            .set_domain_mode(Domain::Sports, IngressMode::Halted)
            .await;
        controller.pause_agent("sports_agent").await;

        let snapshot = controller.runtime_state_snapshot().await;
        assert_eq!(snapshot.ingress_mode, IngressMode::Paused);
        assert_eq!(
            snapshot.domain_ingress_modes.get(&Domain::Sports),
            Some(&IngressMode::Halted)
        );
        assert!(snapshot.paused_agent_ids.contains("sports_agent"));

        let restored = GovernanceController::new(&CoordinatorConfig::default());
        restored.restore_runtime_state(&snapshot).await;

        let restored_snapshot = restored.runtime_state_snapshot().await;
        assert_eq!(restored_snapshot, snapshot);
    }

    #[tokio::test]
    async fn test_set_global_mode_clears_domain_overrides() {
        let controller = GovernanceController::new(&CoordinatorConfig::default());
        controller
            .set_domain_mode(Domain::Sports, IngressMode::Halted)
            .await;
        controller
            .set_domain_mode(Domain::Politics, IngressMode::Paused)
            .await;

        controller.set_global_mode(IngressMode::Paused).await;

        let snapshot = controller.runtime_state_snapshot().await;
        assert_eq!(snapshot.ingress_mode, IngressMode::Paused);
        assert!(
            snapshot.domain_ingress_modes.is_empty(),
            "global mode changes should clear per-domain overrides"
        );
    }

    #[tokio::test]
    async fn test_governance_intent_snapshot_reads_runtime_controls_in_one_view() {
        let controller = GovernanceController::new(&CoordinatorConfig::default());
        controller.set_global_mode(IngressMode::Paused).await;
        controller
            .set_domain_mode(Domain::Sports, IngressMode::Halted)
            .await;
        controller.pause_agent("sports_agent").await;

        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: true,
            blocked_domains: vec!["sports".to_string()],
            max_intent_notional_usd: Some(dec!(10)),
            max_total_notional_usd: Some(dec!(50)),
            updated_by: "test".to_string(),
            reason: Some("maintenance".to_string()),
            metadata: HashMap::new(),
        })
        .expect("valid policy");
        controller.replace_policy(policy.clone()).await;

        let intent = OrderIntent::new(
            "sports_agent",
            Domain::Sports,
            "nba-game-1",
            "sports-token-yes",
            crate::domain::Side::Up,
            true,
            10,
            dec!(0.45),
        );

        let snapshot = controller.intent_snapshot(&intent).await;
        assert_eq!(snapshot.global_mode, IngressMode::Paused);
        assert_eq!(snapshot.domain_mode, IngressMode::Halted);
        assert!(snapshot.agent_paused);
        assert!(snapshot.policy.block_new_intents);
        assert!(snapshot.policy.blocked_domains.contains(&Domain::Sports));
        assert_eq!(
            snapshot.policy.max_total_notional_usd,
            policy.max_total_notional_usd
        );
    }

    #[tokio::test]
    async fn test_governance_policy_persistence_roundtrips_runtime_state() {
        let db_url = std::env::var("PLOY_TEST_DATABASE_URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok());
        let Some(db_url) = db_url else {
            return;
        };

        let pool = match PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
        {
            Ok(pool) => pool,
            Err(_) => return,
        };

        ensure_coordinator_governance_policies_table(&pool)
            .await
            .expect("ensure governance policies table");
        ensure_coordinator_governance_policy_history_table(&pool)
            .await
            .expect("ensure governance policy history table");

        let account_id = format!(
            "gov-persist-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let controller = GovernanceController::new(&CoordinatorConfig::default());
        controller.set_global_mode(IngressMode::Paused).await;
        controller
            .set_domain_mode(Domain::Sports, IngressMode::Halted)
            .await;
        controller.pause_agent("sports_agent").await;

        let policy = GovernancePolicy::try_from_update(GovernancePolicyUpdate {
            block_new_intents: true,
            blocked_domains: vec!["sports".to_string()],
            max_intent_notional_usd: Some(dec!(10)),
            max_total_notional_usd: Some(dec!(50)),
            updated_by: "test".to_string(),
            reason: Some("maintenance".to_string()),
            metadata: HashMap::new(),
        })
        .expect("valid policy");

        let runtime_state = controller.runtime_state_snapshot().await;
        persist_governance_policy(&pool, &account_id, &policy, &runtime_state)
            .await
            .expect("persist governance policy");

        let restored = load_governance_policy(&pool, &account_id)
            .await
            .expect("load governance policy")
            .expect("persisted governance policy");

        assert!(restored.policy.block_new_intents);
        assert!(restored.policy.blocked_domains.contains(&Domain::Sports));
        assert_eq!(restored.runtime_state.ingress_mode, IngressMode::Paused);
        assert_eq!(
            restored
                .runtime_state
                .domain_ingress_modes
                .get(&Domain::Sports),
            Some(&IngressMode::Halted)
        );
        assert!(restored
            .runtime_state
            .paused_agent_ids
            .contains("sports_agent"));
    }
}
