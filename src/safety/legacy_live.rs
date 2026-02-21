/// Legacy live execution escape hatches are permanently disabled.
///
/// These helpers are kept only for backward-compatible call sites.
/// Live execution must go through the Coordinator path (`ploy platform start`).
#[inline]
pub fn legacy_live_allowed() -> bool {
    false
}

/// Legacy strategy live override is also permanently disabled.
#[inline]
pub fn legacy_strategy_live_allowed() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_live_allowed_is_always_false() {
        std::env::set_var("PLOY_ALLOW_LEGACY_LIVE", "true");
        assert!(!legacy_live_allowed());
        std::env::remove_var("PLOY_ALLOW_LEGACY_LIVE");
    }

    #[test]
    fn legacy_strategy_live_allowed_is_always_false() {
        std::env::set_var("PLOY_ALLOW_LEGACY_LIVE", "true");
        std::env::set_var("PLOY_ALLOW_LEGACY_STRATEGY_LIVE", "true");
        assert!(!legacy_strategy_live_allowed());
        std::env::remove_var("PLOY_ALLOW_LEGACY_LIVE");
        std::env::remove_var("PLOY_ALLOW_LEGACY_STRATEGY_LIVE");
    }
}
