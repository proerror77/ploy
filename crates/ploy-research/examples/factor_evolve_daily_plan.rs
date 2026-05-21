use std::path::PathBuf;

use anyhow::{Context, Result};
use ploy_research::research_os::manager::{plan_next_research, ResearchManagerInput};

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let input = flag_value(&args, "--input")
        .or_else(|| args.first().cloned())
        .context("--input <path> is required")?;
    let output = flag_value(&args, "--output");

    let raw = std::fs::read_to_string(&input).with_context(|| format!("read input {input}"))?;
    let request: ResearchManagerInput =
        serde_json::from_str(&raw).with_context(|| format!("parse input {input}"))?;
    let plan = plan_next_research(&request).map_err(anyhow::Error::msg)?;
    let rendered = serde_json::to_string_pretty(&plan)?;

    if let Some(output) = output {
        let output = PathBuf::from(output);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, rendered + "\n")
            .with_context(|| format!("write output {}", output.display()))?;
    } else {
        println!("{rendered}");
    }
    Ok(())
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find_map(|pair| (pair[0] == name).then(|| pair[1].clone()))
}
