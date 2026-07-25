//! A Datalog query and rules layer over git.
//!
//! [`checked_program`] assembles the whole program -- every rule module
//! [`gix_query_rules::RuleStore`] currently publishes, parsed, qualified, and
//! validated together -- and [`run_goal`] is the facade for tier 3 against
//! it: parse a goal, resolve it against the host registry, validate it,
//! magic-set rewrite it, lower it to Nemo, audit the emission, and run it to
//! a demand fixpoint against a real repository.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use gix_query_host::{AllBuiltins, RepoFacts};
use gix_query_ir::{CheckedProgram, Registry};

pub use gix_query_ir::{Registry as HostRegistry, Value};
pub use gix_query_rules::RuleStore;

/// Join a list of diagnostics into one message, one per line.
fn join<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything that can go wrong running a goal.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The goal failed to parse.
    #[error("goal did not parse:\n{0}")]
    Parse(String),
    /// Loading or assembling the stored rule modules failed -- storage, a
    /// module's own syntax, or the whole program's own validation.
    #[error(transparent)]
    Rules(#[from] gix_query_rules::Error),
    /// The goal itself failed validation against the checked program.
    #[error("goal failed validation:\n{0}")]
    CheckGoal(String),
    /// Tier 1 named a predicate the checked program does not declare.
    #[error("no predicate named `{0}`")]
    UnknownPredicate(String),
    /// Tier 1 bound a position outside the predicate's declared arity.
    #[error("position {position} is outside `{predicate}`'s arity {arity}")]
    BindOutOfRange {
        /// The predicate named.
        predicate: String,
        /// The position given.
        position: usize,
        /// The predicate's actual arity.
        arity: usize,
    },
    /// Lowering the rewritten program to Nemo, or auditing that emission,
    /// failed.
    #[error(transparent)]
    Eval(#[from] gix_query_eval::EvalError),
}

/// Assemble the whole program: every rule module currently published at
/// `refs/meta/rules/*` in `repo`, parsed, qualified into one namespace, and
/// checked together against the host registry.
///
/// A repository with no rule modules produces an empty, trivially valid
/// program, so a goal can still call bare host predicates and builtins with
/// no rules published at all.
///
/// # Errors
/// See [`gix_query_rules::Error`].
pub fn checked_program(repo: &gix::Repository) -> Result<CheckedProgram, QueryError> {
    let registry = Registry::host();
    let store = RuleStore::open(repo)?;
    Ok(gix_query_rules::assemble(&store, &registry)?)
}

/// Parse `goal_text`, projecting `select`, and run it against `repo`.
///
/// Assembles [`checked_program`] first, so a goal may call any `pub`
/// predicate any currently published rule module defines, in addition to
/// the host EDB relations and builtins `gix_query_ir::Registry::host`
/// declares. Rows come back in `select`'s column order, deduplicated --
/// Datalog's answer relation is a set, and the demand loop's final round
/// already dedups internally, but a caller composing several partial runs
/// should not have to re-derive that guarantee.
///
/// # Errors
/// See [`QueryError`].
pub fn run_goal(
    repo: &gix::Repository,
    goal_text: &str,
    select: &[&str],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let checked = checked_program(repo)?;

    let mut goal = gix_query_parse::parse_goal_with_select(goal_text, select)
        .map_err(|e| QueryError::Parse(join(&e)))?;
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    run_checked_goal(repo, &checked, &goal)
}

/// Tier 1: apply `predicate` with `bound` values at the given argument
/// positions. Every other position becomes an output column, in signature
/// order -- "bound args filter, free args become columns."
///
/// `predicate` is looked up by name against [`checked_program`]'s registry,
/// so it may be a bare host predicate or builtin name, or a rule module's
/// qualified `pub` name (e.g. `review.reviewed`), exactly as tier 3 resolves
/// names in goal text.
///
/// # Errors
/// See [`QueryError`]. [`QueryError::UnknownPredicate`] and
/// [`QueryError::BindOutOfRange`] are tier 1's own; the rest are shared with
/// [`run_goal`].
pub fn run_predicate(
    repo: &gix::Repository,
    predicate: &str,
    bound: &[(usize, Value)],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let checked = checked_program(repo)?;

    let key = checked
        .registry()
        .predicates()
        .find(|entry| entry.key.name.as_ref() == predicate)
        .map(|entry| entry.key.clone())
        .ok_or_else(|| QueryError::UnknownPredicate(predicate.to_owned()))?;

    for (position, _) in bound {
        if *position >= key.arity {
            return Err(QueryError::BindOutOfRange {
                predicate: predicate.to_owned(),
                position: *position,
                arity: key.arity,
            });
        }
    }

    let mut goal = gix_query_parse::goal_from_predicate(key.name.clone(), key.arity, bound);
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    run_checked_goal(repo, &checked, &goal)
}

/// Shared tail of tiers 1 and 3: validate a resolved [`gix_query_ir::Goal`]
/// against `checked`, magic-set rewrite it, lower it to Nemo, audit the
/// emission, and run it to a demand fixpoint against `repo`.
fn run_checked_goal(
    repo: &gix::Repository,
    checked: &CheckedProgram,
    goal: &gix_query_ir::Goal,
) -> Result<Vec<Vec<Value>>, QueryError> {
    gix_query_check::check_goal(goal, checked).map_err(|e| QueryError::CheckGoal(join(&e)))?;

    let rewritten = gix_query_eval::rewrite_program(checked, goal);
    let emitted = gix_query_eval::NemoProgram::emit(&rewritten)?;
    emitted.audit()?;

    let facts = RepoFacts::new(repo);
    let builtins = AllBuiltins::new(repo);
    let result = gix_query_eval::run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 64)?;

    Ok(result.answer)
}
