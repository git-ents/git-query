//! Where the demand loop's wall clock actually goes.
//!
//! Gate A3 asks for "fine, not fast", but a number is only useful if you know
//! which part of the pipeline it names. This splits load from execute and
//! compares the two fact routes, because the answer changes what the
//! integration has to do: a parse-bound cost is fixable by not writing facts
//! into program text, an execute-bound cost is not fixable by us at all.

use std::time::Instant;

use nemo_spike::{FactRoute, Runner, s};

const N: usize = 10_000;

fn edb() -> Vec<nemo_spike::Tuple> {
    (0..N).map(|i| s!(format!("a{i}"), "r1")).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn cost_split_for_the_inline_route() {
    let runner = Runner::new("p(?A) :- q(?A, ?R) .").facts("q", edb());
    let text = runner.program_text();
    let started = Instant::now();
    let mut session = runner.run().await.expect("run");
    let total = started.elapsed();
    let rows = session.relation("p").await.len();
    println!(
        "inline: {N} facts, {} bytes of program text -> load {:?}, execute {:?}, read+total {:?}, {rows} rows",
        text.len(),
        session.timing.load,
        session.timing.execute,
        total
    );
    assert_eq!(rows, N);
}

#[tokio::test(flavor = "current_thread")]
async fn cost_split_for_the_memory_import_route() {
    let runner = Runner::new("p(?A) :- q(?A, ?R) .")
        .route(FactRoute::MemoryImport)
        .facts("q", edb());
    let started = Instant::now();
    let mut session = runner.run().await.expect("run");
    let total = started.elapsed();
    let rows = session.relation("p").await.len();
    println!(
        "memory-import: {N} facts -> load {:?}, execute {:?}, read+total {:?}, {rows} rows",
        session.timing.load, session.timing.execute, total
    );
    assert_eq!(rows, N);
}

#[tokio::test(flavor = "current_thread")]
async fn cost_of_reading_a_relation_back() {
    // The read is a separate cost from the run, and the CLI pays it per
    // answered predicate.
    let runner = Runner::new("p(?A) :- q(?A, ?R) .")
        .route(FactRoute::MemoryImport)
        .facts("q", edb());
    let mut session = runner.run().await.expect("run");
    let started = Instant::now();
    let rows = session.relation("p").await.len();
    println!("read-back: {rows} rows in {:?}", started.elapsed());
}

#[tokio::test(flavor = "current_thread")]
async fn cost_of_an_empty_program() {
    // The floor: what a query costs before it has done anything. A CLI that
    // runs per invocation pays this every time.
    let started = Instant::now();
    let session = Runner::new("p(?X) :- q(?X) .").run().await.expect("run");
    println!(
        "empty: load {:?}, execute {:?}, total {:?}",
        session.timing.load,
        session.timing.execute,
        started.elapsed()
    );
}
