use ploy::safety::direct_live::{direct_live_allowed, enforce_live_gate, strategy_direct_live_allowed};

/// Every legacy live entry point must be blocked by `enforce_live_gate`.
#[test]
fn enforce_live_gate_blocks_all_known_commands() {
    let commands = [
        "ploy strategy start",
        "ploy crypto split-arb",
        "ploy sports split-arb",
        "ploy agent --enable-trading",
    ];

    for cmd in commands {
        let err = enforce_live_gate(cmd)
            .expect_err(&format!("enforce_live_gate should block `{cmd}`"));
        let msg = err.to_string();
        assert!(
            msg.contains(cmd),
            "error for `{cmd}` should include the command name, got: {msg}"
        );
        assert!(
            msg.contains("ploy platform start"),
            "error for `{cmd}` should mention coordinator path, got: {msg}"
        );
    }
}

/// `direct_live_allowed` must remain false regardless of env-var overrides.
#[test]
fn direct_live_allowed_ignores_env_vars() {
    unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true") };
    assert!(
        !direct_live_allowed(),
        "direct_live_allowed must be hardcoded false"
    );
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
}

/// `strategy_direct_live_allowed` must remain false regardless of env-var overrides.
#[test]
fn strategy_direct_live_allowed_ignores_env_vars() {
    unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "true") };
    unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_STRATEGY_LIVE", "true") };
    assert!(
        !strategy_direct_live_allowed(),
        "strategy_direct_live_allowed must be hardcoded false"
    );
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_STRATEGY_LIVE") };
}

/// The gate error should be a `PloyError::Validation` variant.
#[test]
fn enforce_live_gate_returns_validation_error() {
    let err = enforce_live_gate("test-cmd").unwrap_err();
    let debug = format!("{:?}", err);
    assert!(
        debug.contains("Validation"),
        "expected PloyError::Validation, got: {debug}"
    );
}
