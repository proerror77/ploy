use std::path::Path;

const RETIRED_SOURCE_PATHS: &[&str] = &[
    "src/CLAUDE.md",
    "src/account",
    "src/adapters",
    "src/agent_runtime.rs",
    "src/agents",
    "src/ai_clients",
    "src/analysis",
    "src/api",
    "src/cli",
    "src/collector",
    "src/config",
    "src/config.rs",
    "src/control_plane",
    "src/control_plane.rs",
    "src/coordination",
    "src/coordinator",
    "src/data_plane",
    "src/domain",
    "src/error.rs",
    "src/exchange",
    "src/main_agent_mode",
    "src/main_agent_mode.rs",
    "src/main_commands",
    "src/main_dispatch.rs",
    "src/main_modes",
    "src/main_modes.rs",
    "src/main_runtime.rs",
    "src/ml",
    "src/persistence",
    "src/platform",
    "src/plugins",
    "src/rl",
    "src/safety",
    "src/services",
    "src/signing",
    "src/strategy",
    "src/supervisor",
    "src/tui",
    "src/validation.rs",
];

const RETIRED_TEST_TARGETS: &[&str] = &[
    "tests/engine_store_pg.rs",
    "tests/legacy_live_gate.rs",
    "tests/strategy_evaluations_and_deployment_gate.rs",
];

#[test]
fn workspace_root_keeps_only_the_shim_surface() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut still_present = Vec::new();
    for relative_path in RETIRED_SOURCE_PATHS
        .iter()
        .chain(RETIRED_TEST_TARGETS.iter())
    {
        if repo_root.join(relative_path).exists() {
            still_present.push(relative_path.to_string());
        }
    }

    assert!(
        still_present.is_empty(),
        "legacy root runtime paths still present:\n{}",
        still_present.join("\n")
    );
}
