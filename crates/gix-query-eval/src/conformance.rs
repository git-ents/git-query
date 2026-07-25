//! Conformance suite, pinned to the Nemo commit `spike-report.md` records.
//!
//! Every test here asserts something about the *pinned Nemo version*, not
//! about any one program of ours -- this is what `CONTRIBUTING.md`'s engine
//! upgrade audit runs after bumping the pin, and a failure here means the
//! fragment boundary moved under us, not that a query is wrong. Promoted
//! from the spike's gate A6 (`spikes/nemo-spike/tests/a6_fragment.rs`).
//!
//! Nemo's `execute()` enters a process-global timing mutex; run this module
//! with `--test-threads=1`, as every other test module in this crate must.
#![cfg(test)]

use nemo::datavalues::{AnyDataValue, DataValue};

use crate::engine::{relation, run, value_from_nemo};
use crate::lower::{NemoProgram, NemoTables};
use crate::rewrite::RewrittenProgram;
use gix_query_ir::{Atom, Literal, PredicateKey, Program, Rule, Term, Value};

#[test]
fn every_symbol_we_emit_is_quoted_never_a_bare_identifier() {
    // The trap the spike lost an afternoon to: a bare identifier is a Nemo
    // constant that lands in the `Iri` domain, not a plain string. Our
    // emitter quotes every symbol, so a value we round-trip through it must
    // always come back `PlainString`.
    let mut program = Program::new();
    program.push_rule(
        Rule::fact(Atom::new("out", vec![Term::sym("same")])),
        "conformance",
    );
    let rewritten = RewrittenProgram {
        rules: program,
        demand: Vec::new(),
        eager: std::collections::BTreeSet::new(),
        answer: PredicateKey::new("out", 1),
    };
    let emitted = NemoProgram::emit(&rewritten).expect("emits");
    let mut engine = run(&emitted, &NemoTables::new()).expect("nemo runs");
    let rows = relation(&mut engine, &PredicateKey::new("out", 1)).expect("read back");
    assert_eq!(rows, vec![vec![Value::sym("same")]]);
}

#[test]
fn a_bare_identifier_written_directly_is_an_iri_not_a_symbol() {
    // Bypassing our own emitter to pin the underlying Nemo fact this crate
    // is built around: `carries(same).` is valid Nemo, and `same` lands in
    // `Iri`, which our reader refuses outright.
    let program = "out(?X) :- in(?X) .\nin(same) .".to_owned();
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let err = relation(&mut engine, &PredicateKey::new("out", 1));
    assert!(
        err.is_err(),
        "an IRI was silently accepted into our fragment"
    );

    let iri = AnyDataValue::new_iri("same".to_owned());
    assert!(value_from_nemo(&iri).is_err());
}

#[test]
fn an_uppercase_bare_identifier_is_a_constant_not_a_variable() {
    // Our own grammar says an initial capital is a variable; Nemo says a
    // variable is `?`-prefixed and any bare name, of any case, is a
    // constant. A lowering that forgot the `?` produces a program that
    // parses, validates, runs, and silently derives nothing -- this is the
    // mistake `crate::lower::render_term` exists to make unreachable, pinned
    // here as a fact about Nemo rather than about our emitter.
    let program = "out(X) :- in(X) .\nin(\"x\") .".to_owned();
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let rows = relation(&mut engine, &PredicateKey::new("out", 1)).expect("read back");
    assert_eq!(
        rows,
        Vec::<Vec<Value>>::new(),
        "the capital was treated as a variable; this pin is stale"
    );
}

#[test]
fn a_quoted_symbol_stays_a_plain_string_even_when_it_looks_like_an_iri() {
    let program = "out(?X) :- in(?X) .\nin(\"http://example.com/thing\") .".to_owned();
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let rows = relation(&mut engine, &PredicateKey::new("out", 1)).expect("read back");
    assert_eq!(rows, vec![vec![Value::sym("http://example.com/thing")]]);
}

#[test]
fn an_integer_and_its_decimal_string_do_not_join() {
    let program = "same(?X) :- a(?X), b(?X) .\na(42) .\nb(\"42\") .".to_owned();
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let rows = relation(&mut engine, &PredicateKey::new("same", 1)).expect("read back");
    assert_eq!(rows, Vec::<Vec<Value>>::new());
}

#[test]
fn integers_round_trip_at_the_extremes() {
    let program = format!(
        "out(?X) :- in(?X) .\nin({}) .\nin({}) .\nin(0) .\nin(-1) .",
        i64::MAX,
        i64::MIN
    );
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let mut rows = relation(&mut engine, &PredicateKey::new("out", 1)).expect("read back");
    rows.sort();
    assert_eq!(
        rows,
        vec![
            vec![Value::Int(i64::MIN)],
            vec![Value::Int(-1)],
            vec![Value::Int(0)],
            vec![Value::Int(i64::MAX)],
        ]
    );
}

#[test]
fn every_out_of_fragment_value_domain_is_refused() {
    let outsiders = [
        AnyDataValue::new_iri("http://example.com".to_owned()),
        AnyDataValue::new_boolean(true),
        AnyDataValue::new_double_from_f64(1.5).expect("double"),
        AnyDataValue::new_float_from_f32(1.5).expect("float"),
        AnyDataValue::new_language_tagged_string("x".to_owned(), "en".to_owned()),
    ];
    for value in outsiders {
        assert!(
            value_from_nemo(&value).is_err(),
            "{value:?} in {:?} was accepted into our fragment",
            value.value_domain()
        );
    }
}

#[test]
fn a_language_tag_makes_a_different_value_than_the_bare_string() {
    let plain = AnyDataValue::new_plain_string("x".to_owned());
    let tagged = AnyDataValue::new_language_tagged_string("x".to_owned(), "en".to_owned());
    assert_ne!(plain, tagged);
    assert!(value_from_nemo(&plain).is_ok());
    assert!(value_from_nemo(&tagged).is_err());
}

#[test]
fn an_existential_head_mints_a_null_our_reader_refuses() {
    let program = "has_parent(?X, !P) :- node(?X) .\nnode(\"a\") .".to_owned();
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let err = relation(&mut engine, &PredicateKey::new("has_parent", 2));
    assert!(
        err.is_err(),
        "a null was read as a Value instead of being refused"
    );
}

#[test]
fn export_directives_survive_into_the_engine() {
    // `execute()` does not itself write, but the directive survives into
    // `engine.exports()`, so any code path that honors exports acts on
    // repository-authored text unless a validator rejects the directive
    // before this crate ever sees the program. This crate's own emitter
    // never produces one -- there is no lowering path to it -- and this
    // test pins the fact that Nemo itself does not refuse one either, which
    // is why the refusal has to be ours.
    let program = "@export out :- csv {} .\nout(?X) :- in(?X) .\nin(\"x\") .".to_owned();
    let engine = block_on(nemo::api::load_string(program)).expect("nemo accepted the directive");
    assert_eq!(engine.exports().len(), 1);
}

#[test]
fn a_magic_prefixed_name_is_an_ordinary_name_to_nemo() {
    // Our spec reserves `magic_` for the rewrite; Nemo has no opinion, so
    // the reservation is enforced entirely on our side -- at parse time in
    // `gix-query-parse`, and by this crate simply never emitting anything
    // else under that prefix.
    let program = "magic_p(?X) :- in(?X) .\nin(\"x\") .".to_owned();
    let mut engine = block_on(nemo::api::load_string(program)).expect("loads");
    block_on(engine.execute()).expect("runs");
    let rows = relation(&mut engine, &PredicateKey::new("magic_p", 1)).expect("read back");
    assert_eq!(rows, vec![vec![Value::sym("x")]]);
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
        .block_on(fut)
}

#[test]
#[ignore = "deliberately panics inside nemo_physical, which poisons the process-global \
            timing mutex for every test that runs after it in this process -- run in \
            isolation with `cargo test -- --ignored a_predicate_defined_by_both`"]
fn a_predicate_defined_by_both_a_ground_fact_and_a_rule_is_a_nemo_storage_bug() {
    // Pinned so an engine bump either confirms this is still true (do not
    // "fix" it by working around it in the emitter) or reveals it was fixed
    // upstream (delete this test and stop routing around it).
    //
    // `carries4("same", "intact") .` (a bare `Statement::Fact`) plus
    // `carries4(?P, "edited") :- weak4(?P) .` (a `Statement::Rule`) for the
    // *same* predicate and arity makes `nemo_physical`'s storage layer panic
    // with `assertion failed: source.arity() == arity` -- both clauses are
    // individually valid Nemo and individually fine mixed with other
    // predicates; only the combination, for the same predicate, panics.
    // `crate::golden`'s "multi-clause predicates with constants in heads"
    // test works around this by writing every clause as a rule (a
    // tautological comparison body for the ones that are logically facts),
    // which sidesteps the bug entirely rather than papering over it.
    let mut program = Program::new();
    program.push_rule(
        Rule::fact(Atom::new(
            "carries4",
            vec![Term::sym("same"), Term::sym("intact")],
        )),
        "t",
    );
    program.push_rule(
        Rule::new(
            Atom::new("carries4", vec![Term::var("P"), Term::sym("edited")]),
            vec![Literal::Positive(Atom::new("weak4", vec![Term::var("P")]))],
        ),
        "t",
    );
    program.push_rule(
        Rule::new(
            Atom::new("ans", vec![Term::var("P"), Term::var("C")]),
            vec![Literal::Positive(Atom::new(
                "carries4",
                vec![Term::var("P"), Term::var("C")],
            ))],
        ),
        "t",
    );
    let rewritten = RewrittenProgram {
        rules: program,
        demand: Vec::new(),
        eager: std::collections::BTreeSet::new(),
        answer: PredicateKey::new("ans", 2),
    };
    let emitted = NemoProgram::emit(&rewritten).expect("emits");
    let mut tables = NemoTables::new();
    tables.insert(
        PredicateKey::new("weak4", 1),
        vec![vec![Value::sym("lost")]],
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&emitted, &tables)));
    assert!(
        result.is_err(),
        "nemo_physical no longer panics on a fact/rule-mixed predicate -- this pin is stale"
    );
}

#[test]
fn two_rules_with_an_identical_ground_body_but_different_heads_both_derive() {
    // A second, non-panicking oddity found alongside the fact/rule storage
    // bug above: `carries4("same", "intact") :- always4("x").` and
    // `carries4("moved", "intact") :- always4("x").` -- two rules whose
    // bodies are the exact same already-ground atom -- silently derive only
    // the *first* head in this pinned Nemo version, dropping the second
    // entirely (not an error, not a warning, just a missing row). Giving
    // each rule's guard a distinct argument (`always4("same")` /
    // `always4("moved")`) makes both derive correctly, which is what
    // `crate::golden`'s multi-clause test does. This test pins the
    // once-shared-body case as broken so an engine bump either confirms it
    // or lets that workaround be dropped.
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("carries4b", vec![Term::sym("same"), Term::sym("intact")]),
            vec![Literal::Positive(Atom::new(
                "always4b",
                vec![Term::sym("x")],
            ))],
        ),
        "conformance",
    );
    program.push_rule(
        Rule::new(
            Atom::new("carries4b", vec![Term::sym("moved"), Term::sym("intact")]),
            vec![Literal::Positive(Atom::new(
                "always4b",
                vec![Term::sym("x")],
            ))],
        ),
        "conformance",
    );
    let rewritten = RewrittenProgram {
        rules: program,
        demand: Vec::new(),
        eager: std::collections::BTreeSet::new(),
        answer: PredicateKey::new("carries4b", 2),
    };
    let emitted = NemoProgram::emit(&rewritten).expect("emits");
    let mut tables = NemoTables::new();
    tables.insert(
        PredicateKey::new("always4b", 1),
        vec![vec![Value::sym("x")]],
    );
    let mut engine = run(&emitted, &tables).expect("nemo runs");
    let rows = relation(&mut engine, &PredicateKey::new("carries4b", 2)).expect("read back");
    assert_eq!(
        rows.len(),
        1,
        "both heads derived; the shared-ground-body oddity is fixed and the golden test's \
         per-clause discriminator can be dropped"
    );
}
