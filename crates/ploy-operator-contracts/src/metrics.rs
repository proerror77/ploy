use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub deployments_total: usize,
    pub deployments_running: usize,
    pub deployments_degraded: usize,
    pub deployments_failed: usize,
    pub live_deployments: usize,
    pub paper_deployments: usize,
    pub claim_accounts_total: usize,
    pub claim_accounts_degraded: usize,
    pub pending_intents: usize,
    pub active_orders: usize,
    pub open_positions: usize,
    pub gross_exposure: Decimal,
    pub reserved_order_exposure: Decimal,
    pub total_gross_exposure: Decimal,
    pub active_alert_count: usize,
    pub warning_alert_count: usize,
    pub critical_alert_count: usize,
}

#[cfg(test)]
mod tests {
    use super::SystemMetrics;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn system_metrics_uses_stable_wire_keys() {
        let value = serde_json::to_value(SystemMetrics {
            deployments_total: 3,
            deployments_running: 1,
            deployments_degraded: 1,
            deployments_failed: 1,
            live_deployments: 2,
            paper_deployments: 1,
            claim_accounts_total: 2,
            claim_accounts_degraded: 1,
            pending_intents: 1,
            active_orders: 2,
            open_positions: 3,
            gross_exposure: Decimal::new(500, 2),
            reserved_order_exposure: Decimal::new(75, 2),
            total_gross_exposure: Decimal::new(575, 2),
            active_alert_count: 2,
            warning_alert_count: 1,
            critical_alert_count: 1,
        })
        .expect("serialize metrics");

        assert_eq!(
            value,
            json!({
                "deployments_total": 3,
                "deployments_running": 1,
                "deployments_degraded": 1,
                "deployments_failed": 1,
                "live_deployments": 2,
                "paper_deployments": 1,
                "claim_accounts_total": 2,
                "claim_accounts_degraded": 1,
                "pending_intents": 1,
                "active_orders": 2,
                "open_positions": 3,
                "gross_exposure": "5.00",
                "reserved_order_exposure": "0.75",
                "total_gross_exposure": "5.75",
                "active_alert_count": 2,
                "warning_alert_count": 1,
                "critical_alert_count": 1,
            })
        );
    }
}
