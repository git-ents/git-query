//! `git query` -- the one binary.
//!
//! Tier 3 (`run --goal … --select …`), `predicates`, and `rules add|list` are
//! wired up. Tiers 1 and 2 and `explain` are not implemented yet.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use gix_query::{HostRegistry, RuleStore};

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
    /// Publish, inspect, or list stored rule modules.
    Rules {
        #[command(subcommand)]
        command: RulesCommand,
    },
}

#[derive(Subcommand)]
enum RulesCommand {
    /// Publish a module's source at `refs/meta/rules/<name>`.
    ///
    /// Validates the whole program -- every module currently published, plus
    /// this one -- before storing, so a push that would break another
    /// module's composition (e.g. close a cross-module negation cycle) is
    /// rejected rather than silently accepted.
    Add {
        /// The module's name, e.g. `review`.
        name: String,
        /// Path to its `.dl` source file.
        path: std::path::PathBuf,
        /// Which repository to publish into. Defaults to discovering one
        /// from the current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
    /// List every published module's name.
    List {
        /// Which repository to list from. Defaults to discovering one from
        /// the current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
    /// Assemble and validate the whole program: every published module,
    /// together.
    Check {
        /// Which repository to check. Defaults to discovering one from the
        /// current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
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
        Command::Rules { command } => run_rules(command),
    }
}

fn open_repo(repo: Option<std::path::PathBuf>) -> anyhow::Result<gix::Repository> {
    Ok(match repo {
        Some(path) => gix::open(path)?,
        None => gix::discover(".")?,
    })
}

fn run_rules(command: RulesCommand) -> anyhow::Result<bool> {
    match command {
        RulesCommand::Add { name, path, repo } => {
            let repo = open_repo(repo)?;
            let source = std::fs::read_to_string(&path)?;
            let store = RuleStore::open(&repo)?;
            store.put(&name, &source)?;
            // Validate the whole program -- this module plus every other one
            // already published -- now, so a caller finds out immediately
            // rather than at the next query.
            gix_query::checked_program(&repo)?;
            println!("stored {name} at refs/meta/rules/{name}");
            Ok(true)
        }
        RulesCommand::List { repo } => {
            let repo = open_repo(repo)?;
            let store = RuleStore::open(&repo)?;
            let names = store.list()?;
            for name in &names {
                println!("{name}");
            }
            Ok(!names.is_empty())
        }
        RulesCommand::Check { repo } => {
            let repo = open_repo(repo)?;
            gix_query::checked_program(&repo)?;
            println!("ok");
            Ok(true)
        }
    }
}

fn run_goal(
    goal: &str,
    select: &[String],
    repo: Option<std::path::PathBuf>,
) -> anyhow::Result<bool> {
    let repo = open_repo(repo)?;
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
