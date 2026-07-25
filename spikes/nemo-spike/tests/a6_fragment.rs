//! Gate A6 -- fragment containment.
//!
//! Nemo's language is much larger than ours: arithmetic, aggregates,
//! existential rules with nulls, IRIs, language-tagged strings, floats,
//! booleans, maps, tuples, format strings, global variables, import and export
//! directives. Our spec freezes a fragment with two value types and no
//! function symbols. The gate has two halves: our emitter must not be able to
//! reach the rest, and our reader must reject anything that reaches us anyway.

use nemo::datavalues::{AnyDataValue, DataValue, ValueDomain};
use nemo_spike::{Runner, Value, s};

#[tokio::test(flavor = "current_thread")]
async fn a6_a_bare_identifier_is_an_iri_not_a_symbol() {
    // The trap that cost the spike an afternoon. `carries(same, intact).` is
    // valid Nemo, but `same` is an abstract constant that lands in the `Iri`
    // domain and prints back as an IRI. Our reader rejects it rather than
    // guessing, which is why the emitter quotes every symbol.
    let mut session = Runner::new("out(?X) :- in(?X) .\nin(same) .")
        .run()
        .await
        .expect("run");
    let raw = session.raw_relation("out").await;
    assert_eq!(raw, vec![vec![("Iri".to_owned(), "same".to_owned())]]);

    let iri = AnyDataValue::new_iri("same".to_owned());
    assert_eq!(Value::from_nemo(&iri), Err(ValueDomain::Iri));
}

#[tokio::test(flavor = "current_thread")]
async fn a6_an_uppercase_bare_identifier_is_a_constant_not_a_variable() {
    // Our syntax says an initial capital is a variable; Nemo says a variable
    // is `?`-prefixed and a bare name of any case is a constant. A lowering
    // that forgot the `?` would produce a program that parses, validates, runs
    // and silently derives nothing.
    let mut session = Runner::new("out(X) :- in(X) .")
        .facts("in", [s!("x")])
        .run()
        .await
        .expect("run");
    assert_eq!(
        session.relation("out").await,
        Vec::new(),
        "the capital was treated as a variable, so this finding is stale"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a6_a_quoted_symbol_stays_a_plain_string_even_when_it_looks_like_an_iri() {
    let mut session = Runner::new("out(?X) :- in(?X) .")
        .facts(
            "in",
            [s!("http://example.com/thing"), s!("refs/heads/main")],
        )
        .run()
        .await
        .expect("run");
    let raw = session.raw_relation("out").await;
    assert!(
        raw.iter().all(|row| row[0].0 == "PlainString"),
        "a quoted symbol was reinterpreted: {raw:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a6_an_integer_and_its_decimal_string_are_distinct_values() {
    // Object ids are hex and line numbers are integers, and they must never
    // join. This is the check that our two value types stay two.
    let mut session = Runner::new("same(?X) :- a(?X), b(?X) .")
        .facts("a", [s!(42)])
        .facts("b", [s!("42")])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("same").await, Vec::new());
}

#[tokio::test(flavor = "current_thread")]
async fn a6_integers_come_back_in_an_integer_domain_we_accept() {
    let mut session = Runner::new("out(?X) :- in(?X) .")
        .facts("in", [s!(0), s!(-1), s!(i64::MAX), s!(i64::MIN)])
        .run()
        .await
        .expect("run");
    assert_eq!(
        session.relation("out").await,
        vec![s!(i64::MIN), s!(-1), s!(0), s!(i64::MAX)]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a6_our_reader_rejects_every_value_domain_outside_the_fragment() {
    // The other half of the gate: whatever arrives, if it is not a plain
    // string or an integer, it does not become a `Value` by accident.
    let outsiders = [
        AnyDataValue::new_iri("http://example.com".to_owned()),
        AnyDataValue::new_boolean(true),
        AnyDataValue::new_double_from_f64(1.5).expect("double"),
        AnyDataValue::new_float_from_f32(1.5).expect("float"),
        AnyDataValue::new_language_tagged_string("x".to_owned(), "en".to_owned()),
        AnyDataValue::new_other("x".to_owned(), "http://example.com/dt".to_owned()),
    ];
    for value in outsiders {
        assert!(
            Value::from_nemo(&value).is_err(),
            "{value:?} in {:?} was accepted",
            value.value_domain()
        );
    }
}

#[test]
fn a6_a_language_tag_makes_a_different_value_than_the_bare_string() {
    // Asserted through the value API rather than program text, because the
    // point is the value identity, not the syntax.
    let plain = AnyDataValue::new_plain_string("x".to_owned());
    let tagged = AnyDataValue::new_language_tagged_string("x".to_owned(), "en".to_owned());
    assert_ne!(plain, tagged);
    assert!(Value::from_nemo(&plain).is_ok());
    assert!(Value::from_nemo(&tagged).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn a6_arithmetic_is_available_and_we_simply_never_emit_it() {
    // Recorded so the boundary is known to be ours to hold, not the engine's.
    // Our IR has no term-level operations, so there is no lowering path that
    // produces this -- but nothing in Nemo would refuse it.
    let mut session = Runner::new("out(?Z) :- in(?X, ?Y), ?Z = ?X + ?Y .")
        .facts("in", [s!(2, 3)])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("out").await, vec![s!(5)]);
}

#[tokio::test(flavor = "current_thread")]
async fn a6_aggregates_are_available_and_we_simply_never_emit_them() {
    let mut session = Runner::new("total(#count(?X)) :- in(?X) .")
        .facts("in", [s!("a"), s!("b"), s!("c")])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("total").await, vec![s!(3)]);
}

#[tokio::test(flavor = "current_thread")]
async fn a6_an_existential_head_mints_a_null_our_reader_refuses() {
    // The most dangerous feature to reach by accident, because it is the one
    // that produces values outside our domain rather than merely outside our
    // syntax. A null is not a symbol and not an integer, and it would flow
    // into the CLI's output columns if the reader were permissive.
    let mut session = Runner::new("has_parent(?X, !P) :- node(?X) .")
        .facts("node", [s!("a")])
        .run()
        .await
        .expect("run");
    let raw = session.raw_relation("has_parent").await;
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0][1].0, "Null", "expected a fresh null: {raw:?}");
    // And the typed read panics rather than coercing, which is what the seam
    // would surface as an internal error.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        rt.block_on(session.relation("has_parent"))
    }));
    assert!(panicked.is_err(), "a null was read as a Value");
}

#[tokio::test(flavor = "current_thread")]
async fn a6_import_and_export_directives_are_program_text_we_control() {
    // `@export` writes files. Our emitter never produces one, and the
    // memory-import route is the only `@import` we emit -- but a rule module
    // loaded from a ref is *text*, and if it were passed through unfiltered a
    // repository could make a query write to disk. The validator has to reject
    // directives before lowering, not after.
    let hostile = "@export out :- csv {} .\nout(?X) :- in(?X) .";
    let session = Runner::new(hostile)
        .facts("in", [s!("x")])
        .run()
        .await
        .expect("Nemo accepted the directive, which is the point");
    // `execute` does not itself write; export is a separate step. But the
    // directive survives into the engine, so any code path that honors
    // `exports()` would act on repository-authored text.
    let exports = session.engine.exports();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].0.to_string(), "out");
}

#[tokio::test(flavor = "current_thread")]
async fn a6_a_magic_prefixed_predicate_name_is_an_ordinary_name_to_nemo() {
    // Our spec reserves `magic_` for the rewrite. Nemo has no opinion, so the
    // reservation is enforced at parse time on our side or not at all.
    let mut session = Runner::new("magic_p(?X) :- in(?X) .")
        .facts("in", [s!("x")])
        .run()
        .await
        .expect("run");
    assert_eq!(session.relation("magic_p").await, vec![s!("x")]);
}
