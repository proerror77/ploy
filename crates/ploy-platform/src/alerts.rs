use chrono::Utc;
use ploy_operator_contracts::{AlertRecord, AlertSeverity};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct AlertSignal {
    pub alert_id: String,
    pub severity: AlertSeverity,
    pub kind: String,
    pub source: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct AlertRegistry {
    active: BTreeMap<String, AlertRecord>,
}

impl AlertRegistry {
    pub fn reconcile(&mut self, next: Vec<AlertSignal>) {
        let now = Utc::now();
        let next_ids: BTreeSet<String> =
            next.iter().map(|signal| signal.alert_id.clone()).collect();

        self.active
            .retain(|alert_id, _| next_ids.contains(alert_id));

        for signal in next {
            self.active
                .entry(signal.alert_id.clone())
                .and_modify(|record| {
                    record.severity = signal.severity;
                    record.kind = signal.kind.clone();
                    record.source = signal.source.clone();
                    record.resource_type = signal.resource_type.clone();
                    record.resource_id = signal.resource_id.clone();
                    record.message = signal.message.clone();
                    record.last_seen_at = now;
                })
                .or_insert(AlertRecord {
                    alert_id: signal.alert_id,
                    severity: signal.severity,
                    kind: signal.kind,
                    source: signal.source,
                    resource_type: signal.resource_type,
                    resource_id: signal.resource_id,
                    message: signal.message,
                    first_seen_at: now,
                    last_seen_at: now,
                });
        }
    }

    pub fn active(&self) -> Vec<AlertRecord> {
        let mut alerts: Vec<_> = self.active.values().cloned().collect();
        alerts.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then_with(|| right.last_seen_at.cmp(&left.last_seen_at))
                .then_with(|| left.alert_id.cmp(&right.alert_id))
        });
        alerts
    }
}

#[cfg(test)]
mod tests {
    use super::{AlertRegistry, AlertSignal};
    use ploy_operator_contracts::AlertSeverity;

    #[test]
    fn reconcile_preserves_first_seen_for_existing_alerts() {
        let mut registry = AlertRegistry::default();
        registry.reconcile(vec![AlertSignal {
            alert_id: "claim".to_string(),
            severity: AlertSeverity::Warning,
            kind: "claim_loop_degraded".to_string(),
            source: "ployd".to_string(),
            resource_type: "account".to_string(),
            resource_id: Some("acct-live".to_string()),
            message: "claim loop degraded".to_string(),
        }]);
        let first_seen = registry.active()[0].first_seen_at;

        registry.reconcile(vec![AlertSignal {
            alert_id: "claim".to_string(),
            severity: AlertSeverity::Critical,
            kind: "claim_loop_degraded".to_string(),
            source: "ployd".to_string(),
            resource_type: "account".to_string(),
            resource_id: Some("acct-live".to_string()),
            message: "claim loop still degraded".to_string(),
        }]);

        let alert = &registry.active()[0];
        assert_eq!(alert.first_seen_at, first_seen);
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert_eq!(alert.message, "claim loop still degraded");
    }

    #[test]
    fn reconcile_drops_alerts_missing_from_next_set() {
        let mut registry = AlertRegistry::default();
        registry.reconcile(vec![AlertSignal {
            alert_id: "claim".to_string(),
            severity: AlertSeverity::Warning,
            kind: "claim_loop_degraded".to_string(),
            source: "ployd".to_string(),
            resource_type: "account".to_string(),
            resource_id: Some("acct-live".to_string()),
            message: "claim loop degraded".to_string(),
        }]);
        registry.reconcile(Vec::new());
        assert!(registry.active().is_empty());
    }
}
