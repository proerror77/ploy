/// Legacy live execution escape hatches are permanently disabled.
///
/// Live execution must go through the Coordinator/Gateway path (`ploy platform start`).
/// These helpers remain so older call sites can compile without silently enabling
/// a bypass.
#[inline]
pub fn direct_live_allowed() -> bool {
    false
}

/// Legacy `ploy strategy start` live override is also permanently disabled.
#[inline]
pub fn strategy_direct_live_allowed() -> bool {
    false
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
    fn direct_live_allowed_is_always_false() {
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true") };
        assert!(!direct_live_allowed());
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    }

    #[test]
    fn strategy_direct_live_allowed_is_always_false() {
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true") };
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_STRATEGY_LIVE", "true") };
        assert!(!strategy_direct_live_allowed());
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_STRATEGY_LIVE") };
    }

    #[test]
    fn enforce_live_gate_blocks_all_commands() {
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
    fn enforce_live_gate_ignores_env_override() {
        unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true") };
        assert!(enforce_live_gate("test-cmd").is_err());
        unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    }
}
