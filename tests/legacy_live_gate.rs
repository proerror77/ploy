use ploy::safety::direct_live::{
    direct_live_allowed, enforce_live_gate, strategy_direct_live_allowed,
};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Every legacy live entry point must be blocked by `enforce_live_gate`.
#[test]
fn enforce_live_gate_blocks_all_known_commands() {
    let _guard = ENV_LOCK.lock().expect("failed to lock env");
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };

    let commands = [
        "ploy strategy start",
        "ploy crypto split-arb",
        "ploy sports split-arb",
        "ploy agent --enable-trading",
    ];

    for cmd in commands {
        let err =
            enforce_live_gate(cmd).expect_err(&format!("enforce_live_gate should block `{cmd}`"));
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

/// `direct_live_allowed` should be blocked by default and enabled by explicit env override.
#[test]
fn direct_live_allowed_respects_env_override() {
    let _guard = ENV_LOCK.lock().expect("failed to lock env");
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    assert!(!direct_live_allowed(), "default must remain blocked");

    unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "1") };
    assert!(
        direct_live_allowed(),
        "env override should enable direct live"
    );
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
}

/// `strategy_direct_live_allowed` should follow the same direct-live env override.
#[test]
fn strategy_direct_live_allowed_respects_env_override() {
    let _guard = ENV_LOCK.lock().expect("failed to lock env");
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    assert!(
        !strategy_direct_live_allowed(),
        "default must remain blocked"
    );

    unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "1") };
    assert!(
        strategy_direct_live_allowed(),
        "env override should enable strategy direct live"
    );
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
}

/// The gate error should be a `PloyError::Validation` variant.
#[test]
fn enforce_live_gate_returns_validation_error() {
    let _guard = ENV_LOCK.lock().expect("failed to lock env");
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    let err = enforce_live_gate("test-cmd").unwrap_err();
    let debug = format!("{:?}", err);
    assert!(
        debug.contains("Validation"),
        "expected PloyError::Validation, got: {debug}"
    );
}

#[test]
fn enforce_live_gate_allows_with_explicit_override() {
    let _guard = ENV_LOCK.lock().expect("failed to lock env");
    unsafe { std::env::set_var("PLOY_ALLOW_DIRECT_LIVE", "1") };
    let result = enforce_live_gate("test-cmd");
    unsafe { std::env::remove_var("PLOY_ALLOW_DIRECT_LIVE") };
    assert!(result.is_ok(), "override should allow legacy live gate");
}
