/// Live execution defaults to the Coordinator/Gateway path (`ploy platform start`).
///
/// For strategies not yet wired into the Coordinator (e.g. staggered_arb),
/// set `PLOY_ALLOW_DIRECT_LIVE=1` to enable the legacy `ploy strategy start` path.
/// The env var must be explicitly set — default is disabled.
#[inline]
pub fn direct_live_allowed() -> bool {
    std::env::var("PLOY_ALLOW_DIRECT_LIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Strategy-specific direct live override — follows the same env gate.
#[inline]
pub fn strategy_direct_live_allowed() -> bool {
    direct_live_allowed()
}

/// Single enforcement gate for all legacy live entry points.
///
/// Returns `Err(PloyError::Validation)` when `direct_live_allowed()` is false
/// (which is always, by design). Every CLI path that can run live orders
/// should call this with its command name so the error message is actionable.
pub fn enforce_live_gate(cmd: &str) -> crate::error::Result<()> {
    if direct_live_allowed() {
        return Ok(());
    }
    Err(crate::error::PloyError::Validation(format!(
        "direct `{cmd}` live runtime is disabled; use `ploy platform start` (Coordinator-only live)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_live_disabled_by_default() {
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
        assert!(!direct_live_allowed());
    }

    #[test]
    fn direct_live_enabled_by_env() {
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "1") };
        assert!(direct_live_allowed());
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true") };
        assert!(direct_live_allowed());
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    }

    #[test]
    fn strategy_direct_live_follows_env() {
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "1") };
        assert!(strategy_direct_live_allowed());
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
        assert!(!strategy_direct_live_allowed());
    }

    #[test]
    fn enforce_live_gate_blocks_without_env() {
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
        for cmd in [
            "ploy strategy start",
            "ploy crypto split-arb",
            "ploy sports split-arb",
            "ploy agent --enable-trading",
        ] {
            let err = enforce_live_gate(cmd).unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(cmd), "error should mention command: {msg}");
            assert!(
                msg.contains("ploy platform start"),
                "error should mention coordinator path: {msg}"
            );
        }
    }

    #[test]
    fn enforce_live_gate_allows_with_env() {
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "1") };
        assert!(enforce_live_gate("test-cmd").is_ok());
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    }
}
