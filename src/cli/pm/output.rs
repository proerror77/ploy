//! Output formatting for `ploy pm` commands.
//!
//! Supports two modes: human-readable tables (default) and JSON (--json).

use serde::Serialize;
use tabled::{Table, Tabled};

/// Output mode for command results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Table,
    Json,
}

impl OutputMode {
    pub fn from_json_flag(json: bool) -> Self {
        if json {
            OutputMode::Json
        } else {
            OutputMode::Table
        }
    }
}

/// Print a vec of Tabled + Serialize items in the chosen mode.
pub fn print_items<T: Tabled + Serialize>(items: &[T], mode: OutputMode) -> anyhow::Result<()> {
    match mode {
        OutputMode::Table => {
            if items.is_empty() {
                println!("(no results)");
            } else {
                let table = Table::new(items).to_string();
                println!("{table}");
            }
        }
        OutputMode::Json => {
            let json = serde_json::to_string_pretty(items)?;
            println!("{json}");
        }
    }
    Ok(())
}

/// Print a single Serialize item. Falls back to JSON for table mode.
pub fn print_item<T: Serialize>(item: &T, mode: OutputMode) -> anyhow::Result<()> {
    // Both modes use JSON pretty-print since SDK types don't implement Tabled
    let json = serde_json::to_string_pretty(item)?;
    match mode {
        OutputMode::Table | OutputMode::Json => {
            println!("{json}");
        }
    }
    Ok(())
}

/// Print a single Debug-only item (for SDK types that lack Serialize).
pub fn print_debug<T: std::fmt::Debug>(item: &T, _mode: OutputMode) -> anyhow::Result<()> {
    println!("{item:#?}");
    Ok(())
}

/// Print a vec of Debug-only items (for SDK types that lack Serialize).
pub fn print_debug_items<T: std::fmt::Debug>(items: &[T], _mode: OutputMode) -> anyhow::Result<()> {
    if items.is_empty() {
        println!("(no results)");
    } else {
        for item in items {
            println!("{item:#?}");
        }
    }
    Ok(())
}

/// Print raw JSON value.
pub fn print_json_value(value: &serde_json::Value, _mode: OutputMode) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Print a simple key-value pair.
pub fn print_kv(key: &str, value: &str) {
    println!("{key}: {value}");
}

/// Print a success message.
pub fn print_success(msg: &str) {
    println!("\x1b[32m{msg}\x1b[0m");
}

/// Print a warning message.
pub fn print_warn(msg: &str) {
    println!("\x1b[33m{msg}\x1b[0m");
}

/// Print an error message.
pub fn print_error(msg: &str) {
    eprintln!("\x1b[31m{msg}\x1b[0m");
}

/// Prompt user for confirmation. Returns true if confirmed.
pub fn confirm(prompt: &str) -> bool {
    use std::io::{self, Write};
    print!("{prompt} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
