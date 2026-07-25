//! `git query` -- the one binary.
//!
//! Tier 1 (`run <predicate> --bind …`), tier 3 (`run --goal … --select …`),
//! `predicates`, and `rules add|list|check` are wired up. Tier 2 and
//! `explain` are not implemented yet.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use gix_query::{HostRegistry, RuleStore, Value};

#[derive(Parser)]
#[command(name = "git-query", bin_name = "git query")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a query.
    ///
    /// Tier 1: `git query run <predicate> --bind <position>=<value>`. Bound
    /// positions filter; every other position becomes an output column, in
    /// signature order.
    ///
    /// Tier 3: `git query run --goal '<datalog>' --select Var1,Var2`.
    Run {
        /// Tier 1: the predicate to query, e.g. `reviewed` or
        /// `review.reviewed`. Omit and use `--goal` for tier 3 instead.
        #[arg(conflicts_with = "goal")]
        predicate: Option<String>,
        /// Tier 1: bind an argument position to a value, `<position>=<value>`.
        /// Repeatable. Positions are 0-indexed into the predicate's declared
        /// signature.
        #[arg(long = "bind", value_name = "POSITION=VALUE")]
        bind: Vec<String>,
        /// Tier 3: the goal body, e.g. `commit(Rev), parent(Rev, P)`.
        #[arg(long)]
        goal: Option<String>,
        /// Tier 3: which variables to project, in output-column order.
        #[arg(long, value_delimiter = ',')]
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
        Command::Run {
            predicate,
            bind,
            goal,
            select,
            repo,
        } => match (predicate, goal) {
            (Some(predicate), None) => run_predicate(&predicate, &bind, repo),
            (None, Some(goal)) => {
                if select.is_empty() {
                    bail!("--goal requires --select");
                }
                run_goal(&goal, &select, repo)
            }
            (None, None) => bail!("run requires either a predicate or --goal"),
            (Some(_), Some(_)) => unreachable!("clap rejects predicate together with --goal"),
        },
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
    print_rows(&rows);
    Ok(!rows.is_empty())
}

fn run_predicate(
    predicate: &str,
    bind: &[String],
    repo: Option<std::path::PathBuf>,
) -> anyhow::Result<bool> {
    let repo = open_repo(repo)?;
    let bound: Vec<(usize, Value)> = bind
        .iter()
        .map(|s| parse_bind(s))
        .collect::<Result<_, _>>()?;
    let rows = gix_query::run_predicate(&repo, predicate, &bound)?;
    print_rows(&rows);
    Ok(!rows.is_empty())
}

/// Parse one `--bind` argument, `<position>=<value>`.
fn parse_bind(arg: &str) -> anyhow::Result<(usize, Value)> {
    let (position, value) = arg
        .split_once('=')
        .with_context(|| format!("`--bind {arg}` is not `<position>=<value>`"))?;
    let position: usize = position
        .parse()
        .with_context(|| format!("`--bind {arg}`: `{position}` is not a position"))?;
    Ok((position, parse_value(value)))
}

/// A bare CLI argument is an integer if it parses as one, otherwise a
/// symbol -- an oid, a refname, a path, a member handle, or a closed-domain
/// atom such as `approve`. There is no separate quoting on the command
/// line, unlike in goal text.
fn parse_value(s: &str) -> Value {
    match s.parse::<i64>() {
        Ok(n) => Value::Int(n),
        Err(_) => Value::Sym(s.into()),
    }
}

fn print_rows(rows: &[Vec<Value>]) {
    for row in rows {
        let cols: Vec<String> = row.iter().map(ToString::to_string).collect();
        println!("{}", cols.join("\t"));
    }
}

fn print_predicates() {
    let registry = HostRegistry::host();
    for entry in registry.predicates() {
        println!("{}/{}\t{}", entry.key.name, entry.key.arity, entry.doc);
    }
}
