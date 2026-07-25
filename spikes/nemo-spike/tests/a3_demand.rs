//! Gate A3 -- demand loop viability.
//!
//! Nemo is bottom-up; our host predicates only answer bound calls. The bridge
//! is an outer loop: run to fixpoint, read the `demand_*` relations the
//! magic-set rewrite produced, answer the calls, feed answers back as EDB, run
//! again. The gate is whether that converges and stays correct when a round's
//! answers create demand the previous round could not have known about.

use std::time::Instant;

use nemo_spike::builtin::{Builtin, TableBuiltin};
use nemo_spike::demand::run_to_demand_fixpoint;
use nemo_spike::{FactRoute, Runner, Value, s};

/// The lowered form of `p(B) :- q(A, Rev), anchor_at(A, Rev, B)`.
///
/// Two rules come out of one: a demand rule over the builtin's bound
/// positions, guarded by everything to its left in author order, and the
/// original rule with the builtin call replaced by a read of the answers.
const SIMPLE: &str = "\
demand_anchor_at(?A, ?R) :- q(?A, ?R) .
p(?B) :- q(?A, ?R), anchor_at_ext(?A, ?R, ?B) .";

/// The same, but an answer feeds a rule that derives a new `q`, so round two
/// asks for a call round one could not have predicted.
const CHAINED: &str = "\
q(?A, ?R) :- seed(?A, ?R) .
q(?A, ?R) :- p(?B), follows(?B, ?A, ?R) .
demand_anchor_at(?A, ?R) :- q(?A, ?R) .
p(?B) :- q(?A, ?R), anchor_at_ext(?A, ?R, ?B) .";

#[tokio::test(flavor = "current_thread")]
async fn a3_one_round_answers_a_flat_query() {
    let anchor_at = TableBuiltin::new(
        "anchor_at",
        2,
        [s!("a1", "r1", "blob1"), s!("a2", "r1", "blob2")],
    );
    let runner = Runner::new(SIMPLE).facts("q", [s!("a1", "r1"), s!("a2", "r1")]);
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 8)
        .await
        .expect("loop");

    let mut session = result.session;
    assert_eq!(session.relation("p").await, vec![s!("blob1"), s!("blob2")]);
    // Two rounds: the first asks and answers, the second confirms nothing new
    // is asked. A one-round loop would be one that never checked.
    assert_eq!(result.rounds, 2);
    assert_eq!(result.calls.len(), 2);
    assert!(result.calls.iter().all(|c| c.round == 1));
}

#[tokio::test(flavor = "current_thread")]
async fn a3_a_call_with_no_answers_is_not_a_failure() {
    let anchor_at = TableBuiltin::new("anchor_at", 2, [s!("a1", "r1", "blob1")]);
    let runner = Runner::new(SIMPLE).facts("q", [s!("a1", "r1"), s!("missing", "r1")]);
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 8)
        .await
        .expect("loop");

    let mut session = result.session;
    assert_eq!(session.relation("p").await, vec![s!("blob1")]);
    // The unanswerable call is still logged -- a footprint audit needs to know
    // it was made.
    let missing = result
        .calls
        .iter()
        .find(|c| c.bound[0] == Value::sym("missing"))
        .expect("the empty call was logged");
    assert_eq!(missing.answers, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a3_a_second_round_of_demand_converges_and_is_correct() {
    let anchor_at = TableBuiltin::new(
        "anchor_at",
        2,
        [
            s!("a1", "r1", "blob1"),
            s!("a2", "r2", "blob2"),
            s!("a3", "r3", "blob3"),
        ],
    );
    let runner = Runner::new(CHAINED).facts("seed", [s!("a1", "r1")]).facts(
        "follows",
        [s!("blob1", "a2", "r2"), s!("blob2", "a3", "r3")],
    );
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 16)
        .await
        .expect("loop");

    let mut session = result.session;
    assert_eq!(
        session.relation("p").await,
        vec![s!("blob1"), s!("blob2"), s!("blob3")]
    );
    // Three chained calls, each discovered by the round before it, and a
    // fourth round to observe that demand stopped growing.
    assert_eq!(result.rounds, 4);
    let rounds_asked: Vec<usize> = result.calls.iter().map(|c| c.round).collect();
    assert_eq!(rounds_asked, vec![1, 2, 3]);
}

#[tokio::test(flavor = "current_thread")]
async fn a3_a_cycle_in_the_chain_still_terminates() {
    // If an answer can lead back to a call already made, the loop must notice
    // rather than re-ask forever. The call log is the memo.
    let anchor_at = TableBuiltin::new("anchor_at", 2, [s!("a1", "r1", "b1"), s!("a2", "r2", "b2")]);
    let runner = Runner::new(CHAINED)
        .facts("seed", [s!("a1", "r1")])
        .facts("follows", [s!("b1", "a2", "r2"), s!("b2", "a1", "r1")]);
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 16)
        .await
        .expect("loop");

    let mut session = result.session;
    assert_eq!(session.relation("p").await, vec![s!("b1"), s!("b2")]);
    assert_eq!(result.calls.len(), 2, "a call was asked twice");
}

#[tokio::test(flavor = "current_thread")]
async fn a3_the_builtin_is_never_enumerated() {
    // The whole point of the mode. The stub's table holds ten thousand rows;
    // if the loop ever enumerated it, the call count would not be one.
    let rows: Vec<_> = (0..10_000)
        .map(|i| s!(format!("a{i}"), "r1", format!("blob{i}")))
        .collect();
    let anchor_at = TableBuiltin::new("anchor_at", 2, rows);
    let runner = Runner::new(SIMPLE).facts("q", [s!("a7", "r1")]);
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 8)
        .await
        .expect("loop");

    let mut session = result.session;
    assert_eq!(session.relation("p").await, vec![s!("blob7")]);
    assert_eq!(result.calls.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a3_timing_at_ten_thousand_facts() {
    // "Fine", not "fast". The number that matters is per round, because the
    // loop re-runs the whole program each round from cold -- Nemo has no
    // incremental entry point, so a k-round query pays k times.
    const N: usize = 10_000;
    let rows: Vec<_> = (0..N)
        .map(|i| s!(format!("a{i}"), "r1", format!("blob{i}")))
        .collect();
    let anchor_at = TableBuiltin::new("anchor_at", 2, rows);
    let q: Vec<_> = (0..N).map(|i| s!(format!("a{i}"), "r1")).collect();
    // The memory-import route, because the inline route is parse-bound: see
    // `a3_cost_breakdown`, where the same ten thousand facts cost 1.7s as
    // program text and 6ms as an in-memory CSV.
    let runner = Runner::new(SIMPLE)
        .route(FactRoute::MemoryImport)
        .facts("q", q);

    let started = Instant::now();
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 8)
        .await
        .expect("loop");
    let total = started.elapsed();

    let mut session = result.session;
    assert_eq!(session.relation("p").await.len(), N);
    assert_eq!(result.calls.len(), N);

    println!(
        "A3 timing: {N} facts, {} rounds, {total:?} total",
        result.rounds
    );
    for (i, d) in result.round_times.iter().enumerate() {
        println!("  round {}: {d:?}", i + 1);
    }
    assert!(
        total.as_secs() < 120,
        "ten thousand facts took {total:?}, which is not fine"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a3_the_call_log_records_the_mode_it_was_called_in() {
    // `explain` re-annotates trace leaves against this log, so the log has to
    // carry what the trace cannot: which builtin, which bound arguments.
    let anchor_at = TableBuiltin::new("anchor_at", 2, [s!("a1", "r1", "blob1")]);
    let runner = Runner::new(SIMPLE).facts("q", [s!("a1", "r1")]);
    let result = run_to_demand_fixpoint(&runner, &[&anchor_at], 8)
        .await
        .expect("loop");

    assert_eq!(result.calls.len(), 1);
    let call = &result.calls[0];
    assert_eq!(call.builtin, "anchor_at");
    assert_eq!(call.bound, vec![Value::sym("a1"), Value::sym("r1")]);
    assert_eq!(call.bound.len(), anchor_at.bound());
    assert_eq!(call.answers, 1);
}
