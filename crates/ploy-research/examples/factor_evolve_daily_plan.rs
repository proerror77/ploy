use std::path::PathBuf;

use ploy_research::research_os::manager::{
    build_research_manager_plan, summarize_research_trace, validate_evidence_stage,
    ResearchManagerInput,
};

fn main() -> anyhow::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 && args.len() != 4 {
        anyhow::bail!(
            "usage: factor_evolve_daily_plan <input-json> <output-json> [autofactor-research-trace-json]"
        );
    }
    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let mut input: ResearchManagerInput =
        serde_json::from_str(&std::fs::read_to_string(&input_path)?)?;
    let mut trace_summary_output = None;
    if args.len() == 4 {
        let trace_path = PathBuf::from(&args[3]);
        let trace: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&trace_path)?)?;
        input.research_trace_summary = summarize_research_trace(&trace);
        trace_summary_output = Some(input.research_trace_summary.clone());
    }
    let plan = build_research_manager_plan(&input);
    if !validate_evidence_stage(&plan.evidence_stage) {
        anyhow::bail!("unsupported evidence_stage: {}", plan.evidence_stage);
    }
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, serde_json::to_string_pretty(&plan)? + "\n")?;
    if let Some(trace_summary) = trace_summary_output {
        let trace_summary_path =
            PathBuf::from(&args[2]).with_file_name("research-trace-summary.json");
        std::fs::write(
            trace_summary_path,
            serde_json::to_string_pretty(&trace_summary)? + "\n",
        )?;
    }
    Ok(())
}
