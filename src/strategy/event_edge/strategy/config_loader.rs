use super::{
    DEFAULT_PM_REST_URL, EVENT_EDGE_STRATEGY_NAME, EventEdgeStrategy, PolymarketClient, Result,
};
use crate::config::EventEdgeAgentConfig;
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::traits::{DataFeed, Strategy};
use anyhow::anyhow;
use rust_decimal::Decimal;

impl EventEdgeStrategy {
    pub fn from_toml(id: String, config_str: &str, dry_run: bool) -> Result<Self> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_str).map_err(|e| anyhow!("Invalid TOML: {}", e))?;

        let strategy_section = config
            .get("strategy")
            .ok_or_else(|| anyhow!("Missing [strategy] section"))?;
        let strategy_name = strategy_section
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing strategy.name"))?;
        if strategy_name != EVENT_EDGE_STRATEGY_NAME {
            return Err(anyhow!(
                "strategy.name must be \"{}\", got \"{}\"",
                EVENT_EDGE_STRATEGY_NAME,
                strategy_name
            )
            .into());
        }

        let event_edge = config
            .get("event_edge")
            .ok_or_else(|| anyhow!("Missing [event_edge] section"))?;

        let mut cfg = EventEdgeAgentConfig {
            enabled: strategy_section
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            ..EventEdgeAgentConfig::default()
        };

        if let Some(enabled) = event_edge.get("enabled").and_then(|v| v.as_bool()) {
            cfg.enabled = enabled;
        }
        if let Some(framework) = event_edge.get("framework").and_then(|v| v.as_str()) {
            cfg.framework = framework.trim().to_string();
        }
        if let Some(event_ids) = string_list_from_toml(event_edge, "event_ids") {
            cfg.event_ids = event_ids;
        }
        if let Some(titles) = string_list_from_toml(event_edge, "titles") {
            cfg.titles = titles;
        }
        if let Some(interval_secs) = event_edge.get("interval_secs").and_then(|v| v.as_integer()) {
            cfg.interval_secs = interval_secs.max(1) as u64;
        }
        if let Some(min_edge) = decimal_from_toml(event_edge, "min_edge") {
            cfg.min_edge = min_edge;
        }
        if let Some(max_entry) = decimal_from_toml(event_edge, "max_entry") {
            cfg.max_entry = max_entry;
        }
        if let Some(shares) = event_edge.get("shares").and_then(|v| v.as_integer()) {
            cfg.shares = shares.max(0) as u64;
        }
        if let Some(trade) = event_edge.get("trade").and_then(|v| v.as_bool()) {
            cfg.trade = trade;
        }
        if let Some(cooldown_secs) = event_edge.get("cooldown_secs").and_then(|v| v.as_integer()) {
            cfg.cooldown_secs = cooldown_secs.max(0) as u64;
        }
        if let Some(max_daily_spend_usd) = decimal_from_toml(event_edge, "max_daily_spend_usd") {
            cfg.max_daily_spend_usd = max_daily_spend_usd;
        }
        if let Some(model) = event_edge.get("model").and_then(|v| v.as_str()) {
            cfg.model = Some(model.trim().to_string());
        }
        if let Some(turns) = event_edge
            .get("claude_max_turns")
            .and_then(|v| v.as_integer())
        {
            cfg.claude_max_turns = turns.max(0) as u32;
        }

        let problems = cfg.validate();
        if !problems.is_empty() {
            return Err(anyhow!("Invalid [event_edge] config: {}", problems.join("; ")).into());
        }

        let client = PolymarketClient::new(DEFAULT_PM_REST_URL, dry_run)?;
        Ok(Self::new(id, EventEdgeCore::new(client, cfg), dry_run))
    }
}

fn string_list_from_toml(config: &toml::Value, key: &str) -> Option<Vec<String>> {
    config.get(key).and_then(|value| {
        value.as_array().map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|raw| raw.trim().to_string()))
                .filter(|raw| !raw.is_empty())
                .collect::<Vec<_>>()
        })
    })
}

fn decimal_from_toml(config: &toml::Value, key: &str) -> Option<Decimal> {
    let value = config.get(key)?;
    if let Some(raw) = value.as_float() {
        Decimal::try_from(raw).ok()
    } else if let Some(raw) = value.as_integer() {
        Some(Decimal::from(raw))
    } else if let Some(raw) = value.as_str() {
        raw.parse::<Decimal>().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn from_toml_builds_event_edge_strategy_and_overrides_config() {
        let toml = r#"
[strategy]
name = "event_edge"

[event_edge]
event_ids = ["event-1"]
titles = ["Who wins?"]
interval_secs = 45
min_edge = 0.12
max_entry = 0.63
shares = 55
trade = true
cooldown_secs = 900
max_daily_spend_usd = 125
"#;

        let strategy =
            EventEdgeStrategy::from_toml("ee-test".to_string(), toml, true).expect("strategy");

        assert_eq!(strategy.name(), "event_edge");
        assert!(matches!(
            strategy.required_feeds().as_slice(),
            [DataFeed::Tick {
                interval_ms: 45_000
            }]
        ));
        assert_eq!(strategy.core.cfg.event_ids, vec!["event-1"]);
        assert_eq!(strategy.core.cfg.titles, vec!["Who wins?"]);
        assert_eq!(strategy.core.cfg.min_edge, dec!(0.12));
        assert_eq!(strategy.core.cfg.max_entry, dec!(0.63));
        assert_eq!(strategy.core.cfg.shares, 55);
        assert!(strategy.core.cfg.trade);
        assert_eq!(strategy.core.cfg.cooldown_secs, 900);
        assert_eq!(strategy.core.cfg.max_daily_spend_usd, dec!(125));
    }

    #[test]
    fn from_toml_rejects_non_event_edge_strategy_name() {
        let toml = r#"
[strategy]
name = "momentum"

[event_edge]
event_ids = ["event-1"]
"#;

        let err = EventEdgeStrategy::from_toml("ee-test".to_string(), toml, true)
            .err()
            .expect("wrong strategy name should fail");
        assert!(err.to_string().contains("event_edge"));
    }

    #[test]
    fn from_toml_rejects_missing_event_edge_section() {
        let toml = r#"
[strategy]
name = "event_edge"
"#;

        let err = EventEdgeStrategy::from_toml("ee-test".to_string(), toml, true)
            .err()
            .expect("missing event_edge section should fail");
        assert!(err.to_string().contains("Missing [event_edge] section"));
    }
}
