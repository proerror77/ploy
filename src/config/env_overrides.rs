use super::*;

pub(super) fn apply_env_overrides(cfg: &mut AppConfig) {
    if let Some(v) = env_bool(&["PLOY_DRY_RUN__ENABLED", "PLOY__DRY_RUN__ENABLED"]) {
        cfg.dry_run.enabled = v;
    }

    if let Some(v) = env_string(&["PLOY_ACCOUNT__ID", "PLOY__ACCOUNT__ID", "PLOY_ACCOUNT_ID"]) {
        if !v.trim().is_empty() {
            cfg.account.id = v;
        }
    }

    if let Some(v) = env_string(&[
        "PLOY_ACCOUNT__WALLET_ADDRESS",
        "PLOY__ACCOUNT__WALLET_ADDRESS",
        "PLOY_ACCOUNT_WALLET_ADDRESS",
    ]) {
        if !v.trim().is_empty() {
            cfg.account.wallet_address = Some(v);
        }
    }

    if let Some(v) = env_string(&[
        "PLOY_ACCOUNT__LABEL",
        "PLOY__ACCOUNT__LABEL",
        "PLOY_ACCOUNT_LABEL",
    ]) {
        if !v.trim().is_empty() {
            cfg.account.label = Some(v);
        }
    }

    if let Some(v) = env_string(&["PLOY_MARKET__MARKET_SLUG", "PLOY__MARKET__MARKET_SLUG"]) {
        cfg.market.market_slug = v;
    }

    if let Some(v) = env_string(&[
        "PLOY_EXECUTION__EXCHANGE",
        "PLOY__EXECUTION__EXCHANGE",
        "PLOY_EXECUTION_EXCHANGE",
    ]) {
        let normalized = v.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "polymarket" | "kalshi") {
            cfg.execution.exchange = normalized;
        }
    }

    if let Some(v) = env_string_raw(&[
        "PLOY_KALSHI__BASE_URL",
        "PLOY__KALSHI__BASE_URL",
        "PLOY_KALSHI_BASE_URL",
        "KALSHI_BASE_URL",
    ]) {
        if !v.trim().is_empty() {
            cfg.kalshi.base_url = v;
        }
    }

    if let Some(v) = env_string_raw(&[
        "PLOY_KALSHI__API_KEY",
        "PLOY__KALSHI__API_KEY",
        "PLOY_KALSHI_API_KEY",
        "KALSHI_API_KEY",
        "KALSHI_ACCESS_KEY",
    ]) {
        if !v.trim().is_empty() {
            cfg.kalshi.api_key = Some(v);
        }
    }

    if let Some(v) = env_string_raw(&[
        "PLOY_KALSHI__API_SECRET",
        "PLOY__KALSHI__API_SECRET",
        "PLOY_KALSHI_API_SECRET",
        "KALSHI_API_SECRET",
        "KALSHI_ACCESS_SECRET",
    ]) {
        if !v.trim().is_empty() {
            cfg.kalshi.api_secret = Some(v);
        }
    }

    if let Some(v) = env_u16(&["PLOY_API_PORT", "PLOY__API_PORT"]) {
        cfg.api_port = Some(v);
    }

    if let Some(v) = env_string(&[
        "PLOY_DATABASE__URL",
        "PLOY__DATABASE__URL",
        "PLOY_DATABASE_URL",
        "DATABASE_URL",
    ]) {
        cfg.database.url = v;
    }

    if let Some(v) = env_string(&[
        "PLOY_DATABASE__MAX_CONNECTIONS",
        "PLOY__DATABASE__MAX_CONNECTIONS",
        "PLOY_DATABASE_MAX_CONNECTIONS",
    ])
    .and_then(|raw| raw.parse::<u32>().ok())
    {
        cfg.database.max_connections = v;
    }

    if let Some(v) = env_string(&[
        "PLOY_AGENT_FRAMEWORK__MODE",
        "PLOY__AGENT_FRAMEWORK__MODE",
        "PLOY_AGENT_FRAMEWORK_MODE",
    ]) {
        let normalized = v.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "internal" | "openclaw") {
            cfg.agent_framework.mode = normalized;
        }
    }

    if let Some(v) = env_bool(&[
        "PLOY_AGENT_FRAMEWORK__HARD_DISABLE_INTERNAL_AGENTS",
        "PLOY__AGENT_FRAMEWORK__HARD_DISABLE_INTERNAL_AGENTS",
        "PLOY_AGENT_FRAMEWORK_HARD_DISABLE_INTERNAL_AGENTS",
        "PLOY_OPENCLAW_ONLY",
    ]) {
        cfg.agent_framework.hard_disable_internal_agents = v;
    }

    let ee_enabled = env_bool(&[
        "PLOY_EVENT_EDGE_AGENT__ENABLED",
        "PLOY__EVENT_EDGE_AGENT__ENABLED",
    ]);
    let ee_trade = env_bool(&[
        "PLOY_EVENT_EDGE_AGENT__TRADE",
        "PLOY__EVENT_EDGE_AGENT__TRADE",
    ]);
    let ee_event_ids = env_list(&[
        "PLOY_EVENT_EDGE_AGENT__EVENT_IDS",
        "PLOY__EVENT_EDGE_AGENT__EVENT_IDS",
        "PLOY_EVENT_EDGE_AGENT_EVENT_IDS",
    ]);
    let ee_titles = env_list(&[
        "PLOY_EVENT_EDGE_AGENT__TITLES",
        "PLOY__EVENT_EDGE_AGENT__TITLES",
        "PLOY_EVENT_EDGE_AGENT_TITLES",
    ]);

    if ee_enabled.is_some() || ee_trade.is_some() {
        let ee = cfg
            .event_edge_agent
            .get_or_insert_with(EventEdgeAgentConfig::default);
        if let Some(v) = ee_enabled {
            ee.enabled = v;
        }
        if let Some(v) = ee_trade {
            ee.trade = v;
        }
    }

    if ee_event_ids.is_some() || ee_titles.is_some() {
        let ee = cfg
            .event_edge_agent
            .get_or_insert_with(EventEdgeAgentConfig::default);
        if let Some(v) = ee_event_ids {
            ee.event_ids = v;
        }
        if let Some(v) = ee_titles {
            ee.titles = v;
        }
    }
}

fn env_string(keys: &[&str]) -> Option<String> {
    env_string_raw(keys).map(|s| s.to_ascii_lowercase())
}

fn env_string_raw(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(v) = std::env::var(key) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn env_u16(keys: &[&str]) -> Option<u16> {
    env_string(keys).and_then(|v| v.parse::<u16>().ok())
}

fn env_bool(keys: &[&str]) -> Option<bool> {
    env_string(keys).and_then(|v| parse_bool_like(&v))
}

fn env_list(keys: &[&str]) -> Option<Vec<String>> {
    env_string(keys).map(|raw| parse_string_list(&raw))
}

pub(super) fn parse_string_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_bool_like(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
