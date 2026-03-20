// Temporary compatibility entrypoint while the dedicated `ployd` and `ployctl`
// app crates take over runtime ownership.
#![allow(
    async_fn_in_trait,
    dead_code,
    clippy::clone_on_copy,
    clippy::collapsible_else_if,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::assign_op_pattern,
    clippy::cast_abs_to_unsigned,
    clippy::derivable_impls,
    clippy::doc_overindented_list_items,
    clippy::double_ended_iterator_last,
    clippy::excessive_precision,
    clippy::explicit_auto_deref,
    clippy::field_reassign_with_default,
    clippy::for_kv_map,
    clippy::format_in_format_args,
    clippy::if_same_then_else,
    clippy::iter_cloned_collect,
    clippy::large_enum_variant,
    clippy::manual_contains,
    clippy::manual_async_fn,
    clippy::manual_clamp,
    clippy::manual_is_multiple_of,
    clippy::map_flatten,
    clippy::module_inception,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_lifetimes,
    clippy::needless_range_loop,
    clippy::new_without_default,
    clippy::print_literal,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::redundant_locals,
    clippy::result_large_err,
    clippy::search_is_some,
    clippy::should_implement_trait,
    clippy::to_string_in_format_args,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_map_or,
    clippy::unnecessary_min_or_max,
    clippy::unwrap_or_default,
    clippy::useless_conversion,
    clippy::useless_asref,
    clippy::write_literal,
    clippy::write_with_newline,
    clippy::wrong_self_convention
)]

use clap::Parser;
use ploy::cli::runtime::Cli;
use ploy::error::Result;

mod main_agent_mode;
mod main_commands;
mod main_dispatch;
mod main_modes;
mod main_runtime;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = ploy::workspace::crate_markers();
    let cli = Cli::parse();
    main_dispatch::run(&cli).await
}
