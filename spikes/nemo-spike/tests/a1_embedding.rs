//! Gate A1 -- programmatic embedding.
//!
//! Can we, from Rust and without touching disk or shelling out to `nmo`, load
//! a program from a string, assert typed tuples into named EDB predicates, run
//! to fixpoint, and iterate a named predicate's tuples with stable types?
//!
//! Run these single-threaded. Nemo's `execute` drives a process-global
//! `TimedCode` singleton that asserts it is only entered from one thread, so
//! two engines running concurrently in one process panic -- and the panic
//! poisons the mutex, so every later run in that process fails too. That is
//! recorded as a finding, not worked around.

use nemo_spike::{FactRoute, Runner, Value, s};

#[tokio::test(flavor = "current_thread")]
async fn a1_a_program_loads_from_a_string_and_runs() {
    let mut session = Runner::new("b(?X) :- a(?X) .")
        .facts("a", [s!("x"), s!("y")])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("b").await, vec![s!("x"), s!("y")]);
}

#[tokio::test(flavor = "current_thread")]
async fn a1_facts_assert_inline_with_both_value_types() {
    let mut session = Runner::new("out(?S, ?N) :- in(?S, ?N) .")
        .facts("in", [s!("deadbeef", 1), s!("cafe", 42)])
        .run()
        .await
        .expect("run");
    let rows = session.relation("out").await;
    assert_eq!(rows, vec![s!("cafe", 42), s!("deadbeef", 1)]);
    // Types survive: the integer came back an integer, not the string "42".
    assert!(matches!(rows[0].0[1], Value::Int(42)));
    assert!(matches!(rows[0].0[0], Value::Sym(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn a1_facts_assert_from_memory_without_touching_disk() {
    // The second route: a custom ResourceProvider serving CSV out of a
    // String. No temp file, no working-directory dependence.
    let mut session = Runner::new("out(?S) :- in(?S) .")
        .route(FactRoute::MemoryImport)
        .facts("in", [s!("a"), s!("b"), s!("c")])
        .run()
        .await
        .expect("run");
    assert_eq!(
        session.relation("out").await,
        vec![s!("a"), s!("b"), s!("c")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a1_an_underived_predicate_reads_back_empty_rather_than_erroring() {
    // The CLI needs "no rows" to be a value, not an error: exit code 1 is a
    // successful query with an empty relation.
    let mut session = Runner::new("b(?X) :- a(?X), c(?X) .")
        .facts("a", [s!("x")])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("b").await, Vec::new());
    assert_eq!(session.relation("nonexistent").await, Vec::new());
}

#[tokio::test(flavor = "current_thread")]
async fn a1_a_malformed_program_is_an_error_not_a_panic() {
    let result = Runner::new("b(?X) :- .").run().await;
    assert!(result.is_err(), "expected a parse error");
}

#[tokio::test(flavor = "current_thread")]
async fn a1_ordinary_git_symbols_survive_a_round_trip() {
    let ordinary = [
        "refs/meta/rules/review",
        "src/lib.rs",
        "9a3f21c4deadbeefcafe0123456789abcdef0123",
        "a b c",
        "unicode-\u{e9}\u{4e2d}",
        "",
    ];
    let mut runner = Runner::new("out(?X) :- in(?X) .");
    for s in ordinary {
        runner = runner.facts("in", [s!(s)]);
    }
    let mut session = runner.run().await.expect("run");
    let got: Vec<String> = session
        .relation("out")
        .await
        .into_iter()
        .map(|t| match &t.0[0] {
            Value::Sym(s) => s.clone(),
            other => panic!("not a symbol: {other:?}"),
        })
        .collect();
    let mut expected: Vec<String> = ordinary.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    expected.dedup();
    assert_eq!(got, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn a1_a_double_quote_in_a_symbol_cannot_be_written_as_a_fact() {
    // Nemo's string literal is `is_not("\"")` -- everything up to the next
    // quote, with no escape processing whatsoever. A double quote therefore
    // cannot appear in a program-text symbol at all, escaped or not. Git
    // permits `"` in path names and in author names, so this is reachable
    // from real data, not a contrived byte. The lowering has to refuse it
    // rather than escape it, which is what this asserts.
    let attempt = std::panic::catch_unwind(|| {
        Runner::new("out(?X) :- in(?X) .")
            .facts("in", [s!("weird\"name.rs")])
            .program_text()
    });
    assert!(attempt.is_err(), "the quote should have been refused");
}

#[tokio::test(flavor = "current_thread")]
async fn a1_a_backslash_in_a_symbol_survives_because_nothing_escapes_it() {
    // The complement of the previous test: since Nemo does no unescaping, the
    // right lowering escapes nothing, and a literal backslash is then just a
    // byte like any other.
    let mut session = Runner::new("out(?X) :- in(?X) .")
        .facts("in", [s!("a\\tb")])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("out").await, vec![s!("a\\tb")]);
}

#[tokio::test(flavor = "current_thread")]
async fn a1_the_memory_import_route_carries_symbols_program_text_cannot() {
    // CSV has its own quoting, so the bulk-fact route is not subject to the
    // string-literal limitation. This matters: it means the restriction binds
    // constants written into rules, not tuples asserted from Rust.
    let awkward = ["weird\"name.rs", "a\tb", "comma,separated"];
    let mut runner = Runner::new("out(?X) :- in(?X) .").route(FactRoute::MemoryImport);
    for s in awkward {
        runner = runner.facts("in", [s!(s)]);
    }
    let mut session = runner.run().await.expect("run");
    let got: Vec<String> = session
        .relation("out")
        .await
        .into_iter()
        .map(|t| match &t.0[0] {
            Value::Sym(s) => s.clone(),
            other => panic!("not a symbol: {other:?}"),
        })
        .collect();
    let mut expected: Vec<String> = awkward.iter().map(|s| (*s).to_owned()).collect();
    expected.sort();
    assert_eq!(got, expected);
}
