//! A Datalog query and rules layer over git.
//!
//! [`run_goal`] is the facade for tier 3: parse a goal, resolve it against
//! the host registry, validate it, magic-set rewrite it, lower it to Nemo,
//! audit the emission, and run it to a demand fixpoint against a real
//! repository. No rule module is loaded, so a goal can only call bare host
//! predicates and builtins -- `gix-query-rules` is not wired in yet.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use gix_query_host::{AllBuiltins, RepoFacts};
use gix_query_ir::{Program, Registry, Value};

pub use gix_query_ir::Registry as HostRegistry;

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
    /// The (empty) program failed validation. Unreachable in practice, since
    /// [`run_goal`] checks no rules -- kept as a distinct variant so a future
    /// caller that does load rule modules gets a precise error rather than
    /// this crate having to guess which check produced it.
    #[error("program failed validation:\n{0}")]
    CheckProgram(String),
    /// The goal itself failed validation against the checked program.
    #[error("goal failed validation:\n{0}")]
    CheckGoal(String),
    /// Lowering the rewritten program to Nemo, or auditing that emission,
    /// failed.
    #[error(transparent)]
    Eval(#[from] gix_query_eval::EvalError),
}

/// Parse `goal_text`, projecting `select`, and run it against `repo`.
///
/// No rule module is loaded: the only predicates a goal can call are the
/// host EDB relations and builtins `gix_query_ir::Registry::host` declares.
/// Rows come back in `select`'s column order, deduplicated -- Datalog's
/// answer relation is a set, and the demand loop's final round already
/// dedups internally, but a caller composing several partial runs should
/// not have to re-derive that guarantee.
///
/// # Errors
/// See [`QueryError`].
pub fn run_goal(
    repo: &gix::Repository,
    goal_text: &str,
    select: &[&str],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let registry = Registry::host();

    let mut goal = gix_query_parse::parse_goal_with_select(goal_text, select)
        .map_err(|e| QueryError::Parse(join(&e)))?;
    gix_query_parse::resolve_goal(&mut goal, &registry);

    let checked = gix_query_check::check(&Program::new(), &registry)
        .map_err(|e| QueryError::CheckProgram(join(&e)))?;
    gix_query_check::check_goal(&goal, &checked).map_err(|e| QueryError::CheckGoal(join(&e)))?;

    let rewritten = gix_query_eval::rewrite_program(&checked, &goal);
    let emitted = gix_query_eval::NemoProgram::emit(&rewritten)?;
    emitted.audit()?;

    let facts = RepoFacts::new(repo);
    let builtins = AllBuiltins::new(repo);
    let result = gix_query_eval::run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 64)?;

    Ok(result.answer)
}
