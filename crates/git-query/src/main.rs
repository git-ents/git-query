//! `git query` -- the one binary.
//!
//! Tier 1 (`run <predicate> --bind …`), tier 3 (`run --goal … --select …`),
//! `predicates`, `rule add|list|check`, and tier 1's `explain` are wired up.
//! Tier 2 and tier 3's `explain` are not implemented yet -- see
//! [`explain_predicate`](gix_query::explain_predicate)'s doc for why a
//! partially bound or goal-shaped explain is an open product question rather
//! than missing plumbing.
//!
//! `ARCHITECTURE.md`'s git-query section names the CLI shape `run`, `explain`,
//! `rule`; `rules` (this crate's original name) is kept as a hidden alias so
//! existing scripts do not break, and `predicates` stays as an addition the
//! doc does not forbid.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use gix_query::{DerivationTree, HostRegistry, NegationWitness, RuleStore, Value};

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
    /// Explain a query result's derivation.
    ///
    /// Tier 1 only: `git query explain <predicate> --bind <position>=<value>`,
    /// with every position bound -- explain traces one concrete tuple, so
    /// there is no free-position case the way `run` has one.
    Explain {
        /// The predicate to explain, e.g. `reviewed` or `review.reviewed`.
        predicate: String,
        /// Bind an argument position to a value, `<position>=<value>`.
        /// Repeatable, and required for every position: explain has no
        /// free-position case.
        #[arg(long = "bind", value_name = "POSITION=VALUE")]
        bind: Vec<String>,
        /// Which repository to query. Defaults to discovering one from the
        /// current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
    /// List every predicate the host registry declares.
    Predicates,
    /// Publish, inspect, or list stored rule modules.
    #[command(alias = "rules")]
    Rule {
        #[command(subcommand)]
        command: RuleCommand,
    },
}

#[derive(Subcommand)]
enum RuleCommand {
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
        Command::Explain {
            predicate,
            bind,
            repo,
        } => run_explain(&predicate, &bind, repo),
        Command::Predicates => {
            print_predicates();
            Ok(true)
        }
        Command::Rule { command } => run_rule(command),
    }
}

fn open_repo(repo: Option<std::path::PathBuf>) -> anyhow::Result<gix::Repository> {
    Ok(match repo {
        Some(path) => gix::open(path)?,
        None => gix::discover(".")?,
    })
}

fn run_rule(command: RuleCommand) -> anyhow::Result<bool> {
    match command {
        RuleCommand::Add { name, path, repo } => {
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
        RuleCommand::List { repo } => {
            let repo = open_repo(repo)?;
            let store = RuleStore::open(&repo)?;
            let names = store.list()?;
            for name in &names {
                println!("{name}");
            }
            Ok(!names.is_empty())
        }
        RuleCommand::Check { repo } => {
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

fn run_explain(
    predicate: &str,
    bind: &[String],
    repo: Option<std::path::PathBuf>,
) -> anyhow::Result<bool> {
    let repo = open_repo(repo)?;
    let bound: Vec<(usize, Value)> = bind
        .iter()
        .map(|s| parse_bind(s))
        .collect::<Result<_, _>>()?;
    match gix_query::explain_predicate(&repo, predicate, &bound)? {
        Some(tree) => {
            print_derivation(&tree, 0);
            Ok(true)
        }
        None => {
            println!("not derivable");
            Ok(false)
        }
    }
}

/// Render a [`DerivationTree`] as an indented outline: one node per line,
/// two spaces per level. Not `ARCHITECTURE.md`'s format -- the doc does not
/// specify one -- but the library's own [`DerivationTree`] shape rendered
/// plainly, so a reader can trace exactly what [`gix_query::explain_predicate`]
/// returned without a second, invented vocabulary in between.
fn print_derivation(tree: &DerivationTree, depth: usize) {
    let pad = "  ".repeat(depth);
    match tree {
        DerivationTree::Fact { predicate, tuple } => {
            println!("{pad}{}({})", predicate.name, format_tuple(tuple));
        }
        DerivationTree::Rule {
            rule,
            head,
            support,
            negations,
        } => {
            let rule_label = rule.map_or_else(|| "goal".to_owned(), |r| r.to_string());
            println!("{pad}({}) :- [{rule_label}]", format_tuple(head));
            for lit in support {
                print_derivation(lit, depth + 1);
            }
            for NegationWitness { predicate, tuple } in negations {
                println!("{}  !{}({})", pad, predicate.name, format_tuple(tuple));
            }
        }
    }
}

fn format_tuple(values: &[Value]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("git-query").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?} should parse: {e}"))
    }

    /// `ARCHITECTURE.md`'s git-query section names the shape `rule
    /// <subcommands>`; the CLI predates the doc and calls it `rules`, kept as
    /// a hidden alias so nothing already scripted against it breaks.
    #[test]
    fn rule_and_its_rules_alias_both_parse_to_the_same_command() {
        let Command::Rule {
            command: RuleCommand::List { .. },
        } = parse(&["rule", "list"]).command
        else {
            panic!("expected Rule(List)");
        };
        let Command::Rule {
            command: RuleCommand::List { .. },
        } = parse(&["rules", "list"]).command
        else {
            panic!("expected the 'rules' alias to parse the same way");
        };
    }

    #[test]
    fn explain_requires_a_predicate_and_accepts_repeated_binds() {
        let Command::Explain {
            predicate, bind, ..
        } = parse(&["explain", "reviewed", "--bind", "0=abc123"]).command
        else {
            panic!("expected Explain");
        };
        assert_eq!(predicate, "reviewed");
        assert_eq!(bind, vec!["0=abc123".to_owned()]);
    }

    #[test]
    fn explain_with_no_predicate_is_rejected() {
        assert!(Cli::try_parse_from(["git-query", "explain"]).is_err());
    }

    #[test]
    fn rules_is_hidden_from_help_while_rule_is_listed() {
        let help = <Cli as clap::CommandFactory>::command()
            .render_help()
            .to_string();
        assert!(help.contains("rule"), "{help}");
        assert!(
            !help.contains("rules"),
            "the 'rules' alias should stay hidden from help text: {help}"
        );
    }
}
