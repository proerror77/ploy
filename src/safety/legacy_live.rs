fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "y" | "on")
    )
}

/// Global escape hatch for legacy "direct live trading" paths.
///
/// Ploy is migrating to a Coordinator-only live execution plane (via `ploy platform start`).
/// Legacy entry points remain available for dry-run, but require explicit opt-in for live mode.
pub fn legacy_live_allowed() -> bool {
    env_truthy("PLOY_ALLOW_LEGACY_LIVE")
}

/// Legacy `ploy strategy start` live runtime gate.
///
/// This accepts the per-command override and the global legacy-live override.
pub fn legacy_strategy_live_allowed() -> bool {
    legacy_live_allowed() || env_truthy("PLOY_ALLOW_LEGACY_STRATEGY_LIVE")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn legacy_live_allowed_parses_truthy_values() {
        let _guard = ENV_LOCK.lock().unwrap();

        let key = "PLOY_ALLOW_LEGACY_LIVE";
        let prev = std::env::var(key).ok();

        for v in ["1", "true", "yes", "y", "on", "TrUe"] {
            set_env(key, Some(v));
            assert!(legacy_live_allowed(), "value {v} should be truthy");
        }

        set_env(key, Some("0"));
        assert!(!legacy_live_allowed());

        set_env(key, Some("false"));
        assert!(!legacy_live_allowed());

        set_env(key, Some("no"));
        assert!(!legacy_live_allowed());

        match prev.as_deref() {
            Some(v) => set_env(key, Some(v)),
            None => set_env(key, None),
        }
    }

    #[test]
    fn legacy_strategy_live_allowed_accepts_global_override() {
        let _guard = ENV_LOCK.lock().unwrap();

        let global_key = "PLOY_ALLOW_LEGACY_LIVE";
        let strategy_key = "PLOY_ALLOW_LEGACY_STRATEGY_LIVE";
        let prev_global = std::env::var(global_key).ok();
        let prev_strategy = std::env::var(strategy_key).ok();

        set_env(global_key, None);
        set_env(strategy_key, None);
        assert!(!legacy_strategy_live_allowed());

        set_env(strategy_key, Some("true"));
        assert!(legacy_strategy_live_allowed());

        set_env(strategy_key, None);
        set_env(global_key, Some("true"));
        assert!(legacy_strategy_live_allowed());

        match prev_global.as_deref() {
            Some(v) => set_env(global_key, Some(v)),
            None => set_env(global_key, None),
        }
        match prev_strategy.as_deref() {
            Some(v) => set_env(strategy_key, Some(v)),
            None => set_env(strategy_key, None),
        }
    }
}
