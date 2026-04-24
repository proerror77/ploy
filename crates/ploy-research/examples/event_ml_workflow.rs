use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Coverage,
    Attribution,
    Baseline,
}

impl Phase {
    fn example(self) -> &'static str {
        match self {
            Phase::Coverage => "event_dataset_coverage",
            Phase::Attribution => "event_factor_attribution",
            Phase::Baseline => "event_dataset_baseline",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Phase::Coverage => "coverage diagnostics",
            Phase::Attribution => "AutoML-style factor attribution",
            Phase::Baseline => "fixed supervised baseline",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Phase::Coverage => "coverage",
            Phase::Attribution => "attribution",
            Phase::Baseline => "baseline",
        })
    }
}

#[derive(Debug, Clone)]
struct Config {
    dataset: String,
    entry_secs: i64,
    tolerance_secs: i64,
    top_n: usize,
    min_edge: f64,
    features: Option<String>,
    output_dir: Option<PathBuf>,
    phases: Vec<Phase>,
    dry_run: bool,
}

#[derive(Debug)]
struct WorkflowState {
    run_dir: PathBuf,
    governed_features: Option<String>,
    records: Vec<PhaseRecord>,
}

#[derive(Debug, Serialize)]
struct WorkflowReport<'a> {
    dataset: &'a str,
    run_dir: String,
    entry_secs: i64,
    tolerance_secs: i64,
    phases: Vec<String>,
    feature_source: &'a str,
    governed_features: Vec<&'a str>,
    records: &'a [PhaseRecord],
    next_gates: &'a [&'a str],
}

#[derive(Debug, Serialize)]
struct PhaseRecord {
    phase: String,
    label: &'static str,
    command: String,
    status: String,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if has_flag(&args, "--help") || has_flag(&args, "-h") {
        print_usage();
        return Ok(());
    }

    let config = parse_config(&args)?;
    let mut state = WorkflowState {
        run_dir: workflow_run_dir(&config)?,
        governed_features: config.features.clone(),
        records: Vec::new(),
    };
    fs::create_dir_all(&state.run_dir)
        .with_context(|| format!("create workflow run dir {}", state.run_dir.display()))?;
    print_workflow_header(&config, &state);

    for phase in &config.phases {
        run_phase(*phase, &config, &mut state)?;
    }
    write_workflow_report(&config, &state)?;

    eprintln!();
    eprintln!("workflow_status=completed");
    eprintln!("workflow_run_dir={}", state.run_dir.display());
    eprintln!(
        "next_gates=model_family_selection -> hyperparameter_search -> walk_forward_backtest -> DL/RL only if justified"
    );
    Ok(())
}

fn parse_config(args: &[String]) -> Result<Config> {
    let dataset = flag_value(args, "--dataset")
        .or_else(|| flag_value(args, "--dataset-root"))
        .context("--dataset <event-root-dataset-dir> is required")?;
    let entry_secs = parse_i64_flag(args, "--entry-secs", 60)?;
    let tolerance_secs = parse_i64_flag(args, "--tolerance-secs", 30)?;
    let top_n = parse_usize_flag(args, "--top-n", 20)?;
    let min_edge = parse_f64_flag(args, "--min-edge", 0.0)?;
    let features = flag_value(args, "--features");
    let output_dir = flag_value(args, "--output-dir").map(PathBuf::from);
    let phases = flag_value(args, "--phases")
        .map(|raw| parse_phases(&raw))
        .transpose()?
        .unwrap_or_else(|| vec![Phase::Coverage, Phase::Attribution, Phase::Baseline]);
    let dry_run = has_flag(args, "--dry-run");

    if entry_secs < 0 {
        bail!("--entry-secs must be non-negative");
    }
    if tolerance_secs < 0 {
        bail!("--tolerance-secs must be non-negative");
    }
    if top_n == 0 {
        bail!("--top-n must be positive");
    }

    Ok(Config {
        dataset,
        entry_secs,
        tolerance_secs,
        top_n,
        min_edge,
        features,
        output_dir,
        phases,
        dry_run,
    })
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn parse_i64_flag(args: &[String], flag: &str, default: i64) -> Result<i64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<i64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<usize>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_f64_flag(args: &[String], flag: &str, default: f64) -> Result<f64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<f64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_phases(raw: &str) -> Result<Vec<Phase>> {
    let mut phases = Vec::new();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let phase = match item {
            "coverage" => Phase::Coverage,
            "attribution" | "factor-attribution" | "automl" => Phase::Attribution,
            "baseline" | "fixed-baseline" => Phase::Baseline,
            other => bail!("unknown workflow phase: {other}"),
        };
        phases.push(phase);
    }
    if phases.is_empty() {
        bail!("--phases must include at least one phase");
    }
    Ok(phases)
}

fn workflow_run_dir(config: &Config) -> Result<PathBuf> {
    if let Some(output_dir) = &config.output_dir {
        return Ok(output_dir.clone());
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    Ok(PathBuf::from(&config.dataset)
        .join("workflow_runs")
        .join(format!("event_ml_{seconds}")))
}

fn print_workflow_header(config: &Config, state: &WorkflowState) {
    eprintln!("=== Event ML AutoML Workflow ===");
    eprintln!("dataset={}", config.dataset);
    eprintln!("run_dir={}", state.run_dir.display());
    eprintln!(
        "entry_secs={} tolerance_secs={} phases={} dry_run={}",
        config.entry_secs,
        config.tolerance_secs,
        format_phases(&config.phases),
        config.dry_run
    );
    eprintln!(
        "order=coverage -> AutoML factor attribution -> governed feature set -> fixed baseline -> model family -> hyperparameter search -> walk-forward/backtest -> DL/RL gates"
    );
}

fn format_phases(phases: &[Phase]) -> String {
    phases
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn run_phase(phase: Phase, config: &Config, state: &mut WorkflowState) -> Result<()> {
    let args = phase_args(phase, config, state);
    let command = shell_command(phase.example(), &args);
    eprintln!();
    eprintln!("--- phase={} label=\"{}\" ---", phase, phase.label());
    eprintln!("command={command}");

    if config.dry_run {
        state.records.push(PhaseRecord {
            phase: phase.to_string(),
            label: phase.label(),
            command,
            status: "dry_run".to_string(),
        });
        return Ok(());
    }

    let status = Command::new("cargo")
        .args(["run", "-p", "ploy-research", "--example", phase.example()])
        .args(["--features", "polars-export", "--"])
        .args(&args)
        .status()
        .with_context(|| format!("spawn phase {phase}"))?;
    ensure_success(phase, status)?;
    if phase == Phase::Attribution && config.features.is_none() {
        let whitelist = read_feature_whitelist(&attribution_output_dir(state))?;
        if !whitelist.is_empty() {
            let features = whitelist.join(",");
            eprintln!(
                "governed_feature_source={} governed_feature_count={}",
                attribution_output_dir(state)
                    .join("feature_whitelist.txt")
                    .display(),
                whitelist.len()
            );
            state.governed_features = Some(features);
        }
    }
    state.records.push(PhaseRecord {
        phase: phase.to_string(),
        label: phase.label(),
        command,
        status: "passed".to_string(),
    });
    Ok(())
}

fn phase_args(phase: Phase, config: &Config, state: &WorkflowState) -> Vec<String> {
    let mut args = vec![
        "--dataset".to_string(),
        config.dataset.clone(),
        "--entry-secs".to_string(),
        config.entry_secs.to_string(),
    ];

    match phase {
        Phase::Coverage => {
            args.push("--tolerances".to_string());
            args.push(config.tolerance_secs.to_string());
        }
        Phase::Attribution => {
            args.push("--tolerance-secs".to_string());
            args.push(config.tolerance_secs.to_string());
            args.push("--top-n".to_string());
            args.push(config.top_n.to_string());
            args.push("--output-dir".to_string());
            args.push(attribution_output_dir(state).display().to_string());
            args.push("--whitelist-max-features".to_string());
            args.push(config.top_n.to_string());
        }
        Phase::Baseline => {
            args.push("--tolerance-secs".to_string());
            args.push(config.tolerance_secs.to_string());
            args.push("--min-edge".to_string());
            args.push(config.min_edge.to_string());
        }
    }

    let features = match phase {
        Phase::Baseline => state.governed_features.as_ref(),
        _ => config.features.as_ref(),
    };
    if let Some(features) = features {
        args.push("--features".to_string());
        args.push(features.clone());
    }

    args
}

fn attribution_output_dir(state: &WorkflowState) -> PathBuf {
    state.run_dir.join("attribution")
}

fn read_feature_whitelist(output_dir: &Path) -> Result<Vec<String>> {
    let path = output_dir.join("feature_whitelist.txt");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn write_workflow_report(config: &Config, state: &WorkflowState) -> Result<()> {
    let governed_features = state
        .governed_features
        .as_deref()
        .map(split_features)
        .unwrap_or_default();
    let next_gates = [
        "model_family_selection",
        "hyperparameter_search",
        "walk_forward_backtest",
        "dl_gate_if_justified",
        "rl_gate_if_justified",
    ];
    let report = WorkflowReport {
        dataset: &config.dataset,
        run_dir: state.run_dir.display().to_string(),
        entry_secs: config.entry_secs,
        tolerance_secs: config.tolerance_secs,
        phases: config.phases.iter().map(ToString::to_string).collect(),
        feature_source: if config.features.is_some() {
            "user_supplied"
        } else {
            "automl_attribution_whitelist"
        },
        governed_features,
        records: &state.records,
        next_gates: &next_gates,
    };

    let json_path = state.run_dir.join("workflow_report.json");
    let json_file =
        File::create(&json_path).with_context(|| format!("create {}", json_path.display()))?;
    serde_json::to_writer_pretty(json_file, &report)
        .with_context(|| format!("write {}", json_path.display()))?;

    let markdown_path = state.run_dir.join("workflow_report.md");
    let mut markdown_file = File::create(&markdown_path)
        .with_context(|| format!("create {}", markdown_path.display()))?;
    writeln!(markdown_file, "# Event ML Workflow Report")?;
    writeln!(markdown_file)?;
    writeln!(markdown_file, "- dataset: `{}`", config.dataset)?;
    writeln!(markdown_file, "- run_dir: `{}`", state.run_dir.display())?;
    writeln!(markdown_file, "- entry_secs: `{}`", config.entry_secs)?;
    writeln!(
        markdown_file,
        "- tolerance_secs: `{}`",
        config.tolerance_secs
    )?;
    writeln!(markdown_file)?;
    writeln!(markdown_file, "## Phases")?;
    writeln!(markdown_file)?;
    writeln!(markdown_file, "| phase | status | command |")?;
    writeln!(markdown_file, "| --- | --- | --- |")?;
    for record in &state.records {
        writeln!(
            markdown_file,
            "| `{}` | `{}` | `{}` |",
            record.phase, record.status, record.command
        )?;
    }
    writeln!(markdown_file)?;
    writeln!(markdown_file, "## Governed Features")?;
    writeln!(markdown_file)?;
    for feature in split_features(state.governed_features.as_deref().unwrap_or_default()) {
        writeln!(markdown_file, "- `{feature}`")?;
    }
    writeln!(markdown_file)?;
    writeln!(markdown_file, "## Next Gates")?;
    writeln!(markdown_file)?;
    for gate in next_gates {
        writeln!(markdown_file, "- `{gate}`")?;
    }

    eprintln!("artifact_workflow_report={}", json_path.display());
    Ok(())
}

fn split_features(raw: &str) -> Vec<&str> {
    raw.split(',')
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .collect()
}

fn ensure_success(phase: Phase, status: ExitStatus) -> Result<()> {
    if status.success() {
        eprintln!("phase_status={} result=passed", phase);
        Ok(())
    } else {
        bail!("phase {phase} failed with status {status}");
    }
}

fn shell_command(example: &str, args: &[String]) -> String {
    let mut parts = vec![
        "cargo".to_string(),
        "run".to_string(),
        "-p".to_string(),
        "ploy-research".to_string(),
        "--example".to_string(),
        example.to_string(),
        "--features".to_string(),
        "polars-export".to_string(),
        "--".to_string(),
    ];
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=,".contains(ch))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn print_usage() {
    println!(
        "\
Event ML AutoML workflow runner

Usage:
  cargo run -p ploy-research --example event_ml_workflow --features polars-export -- \\
    --dataset <event-root-dir> [--entry-secs 60] [--tolerance-secs 30]

Options:
  --dataset <dir>          Event-root dataset directory.
  --entry-secs <secs>      Entry row target before settlement. Default: 60.
  --tolerance-secs <secs>  Entry row tolerance. Default: 30.
  --top-n <n>              Attribution rows to print. Default: 20.
  --min-edge <value>       Baseline trading edge threshold. Default: 0.0.
  --features <csv>         Override default feature list.
  --output-dir <dir>       Workflow artifact directory. Default: <dataset>/workflow_runs/event_ml_<timestamp>.
  --phases <csv>           coverage,attribution,baseline. Default: all three.
  --dry-run                Print commands without running phases.
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        std::iter::once("event_ml_workflow".to_string())
            .chain(items.iter().map(|item| (*item).to_string()))
            .collect()
    }

    #[test]
    fn parses_default_phase_order() {
        let config = parse_config(&args(&["--dataset", "/tmp/events"])).unwrap();

        assert_eq!(
            config.phases,
            vec![Phase::Coverage, Phase::Attribution, Phase::Baseline]
        );
        assert_eq!(config.entry_secs, 60);
        assert_eq!(config.tolerance_secs, 30);
    }

    #[test]
    fn parses_selected_phase_aliases() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--phases",
            "coverage,automl,fixed-baseline",
            "--dry-run",
        ]))
        .unwrap();

        assert_eq!(
            config.phases,
            vec![Phase::Coverage, Phase::Attribution, Phase::Baseline]
        );
        assert!(config.dry_run);
    }

    #[test]
    fn coverage_phase_uses_tolerances_flag() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--entry-secs",
            "90",
            "--tolerance-secs",
            "15",
        ]))
        .unwrap();

        let state = WorkflowState {
            run_dir: PathBuf::from("/tmp/run"),
            governed_features: config.features.clone(),
            records: Vec::new(),
        };
        let phase_args = phase_args(Phase::Coverage, &config, &state);

        assert!(
            phase_args
                .windows(2)
                .any(|pair| pair == ["--tolerances", "15"])
        );
        assert!(
            !phase_args
                .windows(2)
                .any(|pair| pair == ["--tolerance-secs", "15"])
        );
    }

    #[test]
    fn rejects_unknown_phase() {
        let error = parse_config(&args(&["--dataset", "/tmp/events", "--phases", "rl"]))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unknown workflow phase"));
    }

    #[test]
    fn baseline_consumes_governed_features_after_attribution() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--output-dir",
            "/tmp/run",
        ]))
        .unwrap();
        let state = WorkflowState {
            run_dir: PathBuf::from("/tmp/run"),
            governed_features: Some("fair_prob_up,model_edge_up".to_string()),
            records: Vec::new(),
        };

        let phase_args = phase_args(Phase::Baseline, &config, &state);

        assert!(
            phase_args
                .windows(2)
                .any(|pair| pair == ["--features", "fair_prob_up,model_edge_up"])
        );
    }
}
