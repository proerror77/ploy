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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_live_allowed_is_always_false() {
        std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true");
        assert!(!direct_live_allowed());
        std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE");
    }

    #[test]
    fn strategy_direct_live_allowed_is_always_false() {
        std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true");
        std::env::set_var("PLOY_ALLOW_DIRECT_STRATEGY_LIVE", "true");
        assert!(!strategy_direct_live_allowed());
        std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE");
        std::env::remove_var("PLOY_ALLOW_DIRECT_STRATEGY_LIVE");
    }
}

