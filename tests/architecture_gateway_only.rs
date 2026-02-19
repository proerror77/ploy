use std::fs;
use std::path::{Path, PathBuf};

const ALLOWED_DIRECT_SUBMIT_CALLERS: &[&str] = &[
    "src/strategy/executor.rs",
    "src/strategy/core/executor.rs",
];

fn collect_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn direct_exchange_submit_calls_are_limited_to_executors() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_root = repo_root.join("src");
    let mut files = Vec::new();
    collect_rust_files(&src_root, &mut files);

    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(repo_root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let content = fs::read_to_string(&file).unwrap_or_default();
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let looks_like_direct_submit = trimmed.contains("client.submit_order(")
                || trimmed.contains("pm_client.submit_order(")
                || trimmed.contains(".core.client.submit_order(");
            if !looks_like_direct_submit {
                continue;
            }
            if ALLOWED_DIRECT_SUBMIT_CALLERS
                .iter()
                .any(|allowed| *allowed == rel)
            {
                continue;
            }
            offenders.push(format!("{rel}:{}: {}", idx + 1, trimmed));
        }
    }

    assert!(
        offenders.is_empty(),
        "direct exchange submit path detected outside executors:\n{}",
        offenders.join("\n")
    );
}
