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

/// Global escape hatch for direct live trading entrypoints.
///
/// Preferred production path is `ploy platform start` (Coordinator-only live).
pub fn direct_live_allowed() -> bool {
    env_truthy("PLOY_ALLOW_DIRECT_LIVE")
}

/// `ploy strategy start` live runtime gate.
///
/// Accepts the strategy-specific override and global direct-live override.
pub fn strategy_direct_live_allowed() -> bool {
    direct_live_allowed() || env_truthy("PLOY_ALLOW_DIRECT_STRATEGY_LIVE")
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
    fn direct_live_allowed_parses_truthy_values() {
        let _guard = ENV_LOCK.lock().unwrap();

        let key = "PLOY_ALLOW_DIRECT_LIVE";
        let prev = std::env::var(key).ok();

        for v in ["1", "true", "yes", "y", "on", "TrUe"] {
            set_env(key, Some(v));
            assert!(direct_live_allowed(), "value {v} should be truthy");
        }

        set_env(key, Some("0"));
        assert!(!direct_live_allowed());

        set_env(key, Some("false"));
        assert!(!direct_live_allowed());

        set_env(key, Some("no"));
        assert!(!direct_live_allowed());

        match prev.as_deref() {
            Some(v) => set_env(key, Some(v)),
            None => set_env(key, None),
        }
    }

    #[test]
    fn strategy_direct_live_allowed_accepts_global_override() {
        let _guard = ENV_LOCK.lock().unwrap();

        let global_key = "PLOY_ALLOW_DIRECT_LIVE";
        let strategy_key = "PLOY_ALLOW_DIRECT_STRATEGY_LIVE";
        let prev_global = std::env::var(global_key).ok();
        let prev_strategy = std::env::var(strategy_key).ok();

        set_env(global_key, None);
        set_env(strategy_key, None);
        assert!(!strategy_direct_live_allowed());

        set_env(strategy_key, Some("true"));
        assert!(strategy_direct_live_allowed());

        set_env(strategy_key, None);
        set_env(global_key, Some("true"));
        assert!(strategy_direct_live_allowed());

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
