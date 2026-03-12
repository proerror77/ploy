use std::fs;
use std::path::Path;

fn workflow_contents(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn release_workflows_use_sqlx_migrate_instead_of_raw_psql_files() {
    let workflows = [
        ".github/workflows/release-aliyun.yml",
        ".github/workflows/deploy-prebuilt.yml",
    ];

    let mut offenders = Vec::new();
    for workflow in workflows {
        let content = workflow_contents(workflow);

        let uses_raw_migration_file =
            content.contains("psql \"${DATABASE_URL}\" -v ON_ERROR_STOP=1 -f")
                || content.contains("for file in ~/ploy/migrations/*.sql; do")
                || content.contains("psql -U ploy -d ploy -f \"$file\"");
        if uses_raw_migration_file {
            offenders.push(format!("{workflow}: raw psql migration path still present"));
        }

        let uses_sqlx_migrate_run = content.contains("sqlx migrate run")
            || content.contains("/bin/sqlx\" migrate run")
            || content.contains("/bin/sqlx migrate run");
        if !uses_sqlx_migrate_run {
            offenders.push(format!(
                "{workflow}: missing sqlx migrate run deployment step"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow migration guard failed:\n{}",
        offenders.join("\n")
    );
}
