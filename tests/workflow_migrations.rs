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

#[test]
fn ci_build_prepares_database_for_sqlx_compile_checks() {
    let content = workflow_contents(".github/workflows/test.yml");
    let mut offenders = Vec::new();

    if !content.contains("sqlx migrate run") {
        offenders.push("test.yml: missing sqlx migrate run before cargo build".to_string());
    }

    let build_step_has_database_url =
        content.contains("- name: Build") && content.contains("DATABASE_URL: postgres://ploy:ploy@localhost:5432/ploy_test");
    if !build_step_has_database_url {
        offenders.push("test.yml: Build step missing DATABASE_URL for sqlx::query! compile checks".to_string());
    }

    assert!(
        offenders.is_empty(),
        "workflow sqlx compile guard failed:\n{}",
        offenders.join("\n")
    );
}
