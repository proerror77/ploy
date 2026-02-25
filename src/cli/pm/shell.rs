//! `ploy pm shell` — Interactive REPL for Polymarket CLI.

use super::GlobalPmArgs;

pub fn run(
    args: &GlobalPmArgs,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + '_>> {
    Box::pin(run_inner(args))
}

async fn run_inner(args: &GlobalPmArgs) -> anyhow::Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::DefaultEditor;

    println!("\x1b[36mPolymarket Interactive Shell\x1b[0m");
    println!("Type commands without 'ploy pm' prefix. E.g.: markets search bitcoin");
    println!("Type 'help' for available commands, 'exit' to quit.");
    println!();

    let history_path = super::config_file::PmConfig::config_dir()
        .ok()
        .map(|d| d.join("history.txt"));

    let mut rl = DefaultEditor::new()?;

    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    }

    loop {
        match rl.readline("\x1b[36mpm>\x1b[0m ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let _ = rl.add_history_entry(line);

                match line {
                    "exit" | "quit" | "q" => break,
                    "help" | "?" => {
                        print_shell_help();
                        continue;
                    }
                    _ => {}
                }

                // Parse as pm subcommand
                let shell_args: Vec<String> = std::iter::once("pm".to_string())
                    .chain(line.split_whitespace().map(String::from))
                    .collect();

                use clap::Parser;
                match ShellCli::try_parse_from(&shell_args) {
                    Ok(parsed) => {
                        let shell_pm_args = GlobalPmArgs {
                            json: args.json,
                            private_key: args.private_key.clone(),
                            dry_run: args.dry_run,
                            yes: args.yes,
                        };
                        if let Err(e) = super::run(parsed.command, &shell_pm_args).await {
                            super::output::print_error(&format!("{e}"));
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                    }
                }
            }
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }

    if let Some(ref path) = history_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = rl.save_history(path);
    }

    Ok(())
}

/// Internal CLI struct for shell parsing.
#[derive(clap::Parser)]
#[command(name = "pm", no_binary_name = true)]
struct ShellCli {
    #[command(subcommand)]
    command: super::PmCommands,
}

fn print_shell_help() {
    println!("Available commands:");
    println!("  markets  {{list, get, get-by-slug, search}}");
    println!("  events   {{list, get, get-by-slug}}");
    println!("  tags     {{list, get, get-by-slug, related}}");
    println!("  series   {{list, get}}");
    println!("  comments {{list, get, by-user}}");
    println!("  profiles {{get}}");
    println!("  sports   {{metadata, market-types, teams}}");
    println!("  clob     {{health, time, midpoint, price, spread, book, ...}}");
    println!("  data     {{positions, trades, activity, leaderboard, ...}}");
    println!("  orders   {{create, market-buy, market-sell, list, cancel, ...}}");
    println!("  wallet   {{address, balance, api-keys, ...}}");
    println!("  ctf      {{split, merge, redeem, condition-id}}");
    println!("  approve  {{check, set}}");
    println!("  bridge   {{deposit, supported-assets}}");
    println!("  setup    (interactive setup wizard)");
    println!("  help     (this message)");
    println!("  exit     (quit shell)");
}
