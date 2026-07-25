//! `git query` -- the one binary.
//!
//! Only tier 3 (`run --goal … --select …`) and `predicates` are wired up so
//! far. Tiers 1 and 2, `explain`, and `rules` are not implemented; no rule
//! module is loaded, so a goal can only call bare host EDB predicates and
//! builtins.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use gix_query::HostRegistry;

#[derive(Parser)]
#[command(name = "git-query", bin_name = "git query")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a query.
    Run {
        /// The goal body, e.g. `commit(Rev), parent(Rev, P)`.
        #[arg(long)]
        goal: String,
        /// Which variables to project, in output-column order.
        #[arg(long, value_delimiter = ',', required = true)]
        select: Vec<String>,
        /// Which repository to query. Defaults to discovering one from the
        /// current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
    /// List every predicate the host registry declares.
    Predicates,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(true) => ExitCode::from(0),
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(2)
        }
    }
}

/// Runs the requested command. `Ok(true)` means rows came back (or the
/// command has no row concept, e.g. `predicates`); `Ok(false)` means a query
/// ran cleanly and found nothing.
fn run(command: Command) -> anyhow::Result<bool> {
    match command {
        Command::Run { goal, select, repo } => run_goal(&goal, &select, repo),
        Command::Predicates => {
            print_predicates();
            Ok(true)
        }
    }
}

fn run_goal(
    goal: &str,
    select: &[String],
    repo: Option<std::path::PathBuf>,
) -> anyhow::Result<bool> {
    let repo = match repo {
        Some(path) => gix::open(path)?,
        None => gix::discover(".")?,
    };
    let select: Vec<&str> = select.iter().map(String::as_str).collect();
    let rows = gix_query::run_goal(&repo, goal, &select)?;

    for row in &rows {
        let cols: Vec<String> = row.iter().map(ToString::to_string).collect();
        println!("{}", cols.join("\t"));
    }

    Ok(!rows.is_empty())
}

fn print_predicates() {
    let registry = HostRegistry::host();
    for entry in registry.predicates() {
        println!("{}/{}\t{}", entry.key.name, entry.key.arity, entry.doc);
    }
}
