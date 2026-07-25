//! The outer demand loop.
//!
//! Nemo evaluates bottom-up and our host predicates only answer bound calls,
//! so the two do not meet without a loop on our side: run to fixpoint, read
//! which calls the program asked for, answer them, feed the answers back as
//! ordinary EDB facts, and run again. It terminates when a round asks for no
//! call it has not already answered.
//!
//! The magic-set rewrite that produces the `demand_*` predicates is assumed
//! here rather than implemented -- the spike's question is whether the loop
//! converges and stays correct, not whether we can write the rewrite, which is
//! ours either way.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use nemo::error::Error as NemoError;

use crate::builtin::Builtin;
use crate::runner::{Runner, Session};
use crate::value::{Tuple, Value};

/// One answered host call, recorded as it happens.
///
/// This log is the reason the loop is worth building rather than inlining: it
/// is what `explain` later joins against to say which mode a builtin was
/// called in, and what a footprint audit reads to say which refs an answer
/// depended on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedCall {
    /// Which round asked.
    pub round: usize,
    /// Which builtin.
    pub builtin: String,
    /// The bound arguments.
    pub bound: Vec<Value>,
    /// How many completions came back.
    pub answers: usize,
}

/// What the loop did.
#[derive(Debug)]
pub struct LoopResult {
    /// The final run, still queryable.
    pub session: Session,
    /// Rounds executed, including the last one that added nothing.
    pub rounds: usize,
    /// Every host call, in the order asked.
    pub calls: Vec<LoggedCall>,
    /// Wall clock per round.
    pub round_times: Vec<Duration>,
}

/// Run `runner` to a demand fixpoint against `builtins`.
///
/// For each builtin `b`, the program is expected to derive `demand_b/n` over
/// `b`'s bound positions, and to read answers from `b_ext/arity`.
pub async fn run_to_demand_fixpoint(
    runner: &Runner,
    builtins: &[&dyn Builtin],
    max_rounds: usize,
) -> Result<LoopResult, NemoError> {
    let mut answered: BTreeMap<&str, BTreeSet<Vec<Value>>> = BTreeMap::new();
    let mut ext: BTreeMap<String, Vec<Tuple>> = BTreeMap::new();
    let mut calls = Vec::new();
    let mut round_times = Vec::new();

    for round in 1..=max_rounds {
        let mut this = runner.clone();
        for (predicate, tuples) in &ext {
            this = this.facts(predicate, tuples.iter().cloned());
        }
        let mut session = this.run().await?;
        round_times.push(session.timing.load + session.timing.execute);

        let mut asked_anything_new = false;
        for builtin in builtins {
            let demanded = session
                .relation(&format!("demand_{}", builtin.name()))
                .await;
            for tuple in demanded {
                assert_eq!(
                    tuple.arity(),
                    builtin.bound(),
                    "demand_{} has the wrong arity for its mode",
                    builtin.name()
                );
                let seen = answered.entry(builtin.name()).or_default();
                if !seen.insert(tuple.0.clone()) {
                    continue;
                }
                asked_anything_new = true;
                let answers = builtin.call(&tuple.0);
                calls.push(LoggedCall {
                    round,
                    builtin: builtin.name().to_owned(),
                    bound: tuple.0.clone(),
                    answers: answers.len(),
                });
                let table = ext.entry(format!("{}_ext", builtin.name())).or_default();
                for answer in answers {
                    let mut row = tuple.0.clone();
                    row.extend(answer);
                    table.push(Tuple(row));
                }
            }
        }

        if !asked_anything_new {
            return Ok(LoopResult {
                session,
                rounds: round,
                calls,
                round_times,
            });
        }
    }

    panic!("demand loop did not converge within {max_rounds} rounds");
}
