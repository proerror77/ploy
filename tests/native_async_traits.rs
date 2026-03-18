use std::fs;
use std::path::Path;

fn repo_file(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn governance_agent_path_does_not_use_async_trait_macro() {
    let files = [
        "src/agents/governance_agent.rs",
        "src/agents/openclaw/agent.rs",
    ];

    let offenders: Vec<_> = files
        .into_iter()
        .filter_map(|relative_path| {
            let content = repo_file(relative_path);
            let uses_async_trait = content.contains("#[async_trait]")
                || content.contains("use async_trait::async_trait;");
            uses_async_trait.then(|| relative_path.to_string())
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "governance async-trait compatibility shim still present in:\n{}",
        offenders.join("\n")
    );
}
