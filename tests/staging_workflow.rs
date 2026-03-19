use std::fs;
use std::path::Path;

fn repo_file(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn release_staging_workflow_is_first_class_and_artifact_based() {
    let content = repo_file(".github/workflows/release-staging.yml");
    let deploy_section = content
        .split("deploy-staging:")
        .nth(1)
        .unwrap_or("");
    let mut offenders = Vec::new();

    if !content.contains("environment: staging") {
        offenders.push("release-staging.yml: missing environment: staging".to_string());
    }
    if !content.contains("tango-2-1") {
        offenders.push("release-staging.yml: missing tango-2-1 target context".to_string());
    }
    if !deploy_section.contains("sqlx") || !deploy_section.contains("migrate run") {
        offenders.push("release-staging.yml: missing tracked sqlx migration step".to_string());
    }
    if !content.contains("actions/upload-artifact@v4") || !content.contains("actions/download-artifact@v4") {
        offenders.push("release-staging.yml: missing artifact bundle upload/download flow".to_string());
    }
    if deploy_section.contains("cargo build --release")
        || deploy_section.contains("cargo install")
    {
        offenders.push("release-staging.yml: deploy path still builds on host".to_string());
    }

    assert!(
        offenders.is_empty(),
        "staging workflow guard failed:\n{}",
        offenders.join("\n")
    );
}
