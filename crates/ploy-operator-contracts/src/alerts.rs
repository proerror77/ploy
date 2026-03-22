use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertRecord {
    pub alert_id: String,
    pub severity: AlertSeverity,
    pub kind: String,
    pub source: String,
    pub resource_type: String,
    #[serde(default)]
    pub resource_id: Option<String>,
    pub message: String,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::{AlertRecord, AlertSeverity};
    use chrono::Utc;
    use serde_json::json;

    #[test]
    fn alert_record_uses_stable_wire_keys() {
        let now = Utc::now();
        let value = serde_json::to_value(AlertRecord {
            alert_id: "live_reconcile_degraded".to_string(),
            severity: AlertSeverity::Critical,
            kind: "live_reconcile_degraded".to_string(),
            source: "ployd".to_string(),
            resource_type: "system".to_string(),
            resource_id: None,
            message: "live reconcile is backing off".to_string(),
            first_seen_at: now,
            last_seen_at: now,
        })
        .expect("serialize alert");

        assert_eq!(
            value,
            json!({
                "alert_id": "live_reconcile_degraded",
                "severity": "critical",
                "kind": "live_reconcile_degraded",
                "source": "ployd",
                "resource_type": "system",
                "resource_id": null,
                "message": "live reconcile is backing off",
                "first_seen_at": now,
                "last_seen_at": now,
            })
        );
    }
}
