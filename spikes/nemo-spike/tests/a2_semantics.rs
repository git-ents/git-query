//! Gate A2 -- semantics conformance.
//!
//! Five golden programs whose expected relations are computed by hand, plus
//! the question of what Nemo does with a program our validator would reject.
//! Every assertion is exact set equality; `Session::relation` sorts and
//! deduplicates, so an assertion here is about the set and not about an order
//! nothing promises.

use nemo_spike::{Runner, s};

#[tokio::test(flavor = "current_thread")]
async fn a2_1_transitive_closure_is_a_set() {
    // A diamond, so that `a -> d` is derivable by two distinct paths. Under
    // set semantics it appears once; under bag semantics it would appear
    // twice, and the dedup in `relation` would hide that -- so the count is
    // asserted against the engine directly as well.
    let runner = Runner::new(
        "\
tc(?X, ?Y) :- edge(?X, ?Y) .
tc(?X, ?Z) :- edge(?X, ?Y), tc(?Y, ?Z) .",
    )
    .facts(
        "edge",
        [s!("a", "b"), s!("a", "c"), s!("b", "d"), s!("c", "d")],
    );
    let mut session = runner.run().await.expect("run");

    let expected = vec![
        s!("a", "b"),
        s!("a", "c"),
        s!("a", "d"),
        s!("b", "d"),
        s!("c", "d"),
    ];
    assert_eq!(session.relation("tc").await, expected);

    let tag = nemo::rule_model::components::tag::Tag::new("tc".into());
    assert_eq!(
        session.engine.count_facts_in_memory_for_predicate(&tag),
        Some(expected.len()),
        "duplicate derivations were retained, so this is not set semantics"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_1_a_cycle_terminates() {
    let mut session = Runner::new(
        "\
tc(?X, ?Y) :- edge(?X, ?Y) .
tc(?X, ?Z) :- edge(?X, ?Y), tc(?Y, ?Z) .",
    )
    .facts("edge", [s!("a", "b"), s!("b", "a")])
    .run()
    .await
    .expect("run");
    assert_eq!(
        session.relation("tc").await,
        vec![s!("a", "a"), s!("a", "b"), s!("b", "a"), s!("b", "b")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_2_stratified_negation_one_stratum() {
    let mut session = Runner::new("active(?M) :- member(?M), ~revoked(?M) .")
        .facts("member", [s!("alice"), s!("bob"), s!("carol")])
        .facts("revoked", [s!("bob")])
        .run()
        .await
        .expect("run");
    assert_eq!(
        session.relation("active").await,
        vec![s!("alice"), s!("carol")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_2_stratified_negation_across_three_strata() {
    // c is stratum 0, b negates c so it is stratum 1, a negates b so it is
    // stratum 2. If the strata were evaluated in the wrong order, `a` would
    // see an incomplete `b` and over-derive.
    let mut session = Runner::new(
        "\
b(?X) :- d(?X), ~c(?X) .
a(?X) :- e(?X), ~b(?X) .",
    )
    .facts("c", [s!("x")])
    .facts("d", [s!("x"), s!("y")])
    .facts("e", [s!("x"), s!("y")])
    .run()
    .await
    .expect("run");
    assert_eq!(session.relation("b").await, vec![s!("y")]);
    assert_eq!(session.relation("a").await, vec![s!("x")]);
}

#[tokio::test(flavor = "current_thread")]
async fn a2_3_a_retraction_shaped_update_has_no_state_bleed() {
    // We re-run per invocation rather than update incrementally, so what has
    // to hold is that a second engine in the same process sees only the facts
    // it was given. A global dictionary that remembered the first run's
    // strings would be fine; a global *table* that remembered its tuples
    // would not.
    let program = "\
active(?M) :- member(?M), ~revoked(?M) .
reviewer(?M) :- active(?M), signer(?M) .";
    let base = Runner::new(program)
        .facts("member", [s!("alice"), s!("bob")])
        .facts("signer", [s!("alice"), s!("bob")]);

    let mut first = base.clone().run().await.expect("first run");
    assert_eq!(
        first.relation("reviewer").await,
        vec![s!("alice"), s!("bob")]
    );

    let mut second = base
        .clone()
        .facts("revoked", [s!("alice")])
        .run()
        .await
        .expect("second run");
    assert_eq!(second.relation("reviewer").await, vec![s!("bob")]);

    // And the first session is unchanged by the second having run.
    assert_eq!(
        first.relation("reviewer").await,
        vec![s!("alice"), s!("bob")]
    );

    // A third run with the original facts recovers the original answer, so
    // the retraction did not leave a residue in any shared structure.
    let mut third = base.run().await.expect("third run");
    assert_eq!(
        third.relation("reviewer").await,
        vec![s!("alice"), s!("bob")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_4_multi_clause_predicates_and_constants_in_heads() {
    // The spec's `carries(same, intact).` shape: a predicate defined partly by
    // ground facts with constant arguments and partly by rules, with the
    // constants appearing in a head.
    let mut session = Runner::new(
        "\
carries(\"same\", \"intact\") .
carries(\"moved\", \"intact\") .
carries(?P, \"edited\") :- weak(?P) .
propagates(?P, ?C) :- carries(?P, ?C), ~suppressed(?P) .",
    )
    .facts("weak", [s!("lost")])
    .facts("suppressed", [s!("moved")])
    .run()
    .await
    .expect("run");

    assert_eq!(
        session.relation("carries").await,
        vec![
            s!("lost", "edited"),
            s!("moved", "intact"),
            s!("same", "intact"),
        ]
    );
    assert_eq!(
        session.relation("propagates").await,
        vec![s!("lost", "edited"), s!("same", "intact")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_5_a_join_with_an_inequality_comparison() {
    // The `ambiguous` rule shape: one anchor projecting onto one revision at
    // two different positions.
    let mut session =
        Runner::new("ambiguous(?A, ?R) :- bind(?A, ?R, ?P1), bind(?A, ?R, ?P2), ?P1 != ?P2 .")
            .facts(
                "bind",
                [
                    s!("anchor1", "rev1", 10),
                    s!("anchor1", "rev1", 20),
                    s!("anchor2", "rev1", 30),
                    s!("anchor3", "rev2", 40),
                    s!("anchor3", "rev3", 50),
                ],
            )
            .run()
            .await
            .expect("run");
    assert_eq!(
        session.relation("ambiguous").await,
        vec![s!("anchor1", "rev1")]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_5_ordering_comparisons_hold_on_integers() {
    let mut session = Runner::new("after(?A, ?B) :- line(?A), line(?B), ?A > ?B .")
        .facts("line", [s!(1), s!(2), s!(3)])
        .run()
        .await
        .expect("run");
    assert_eq!(
        session.relation("after").await,
        vec![s!(2, 1), s!(3, 1), s!(3, 2)]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_an_unstratifiable_program_is_rejected_at_load() {
    // We reject these in our own validator; this records what happens if one
    // ever leaks through, so the failure is a diagnosable error rather than a
    // wrong answer or a hang.
    let result = Runner::new("p(?X) :- q(?X), ~p(?X) .")
        .facts("q", [s!("a")])
        .run()
        .await;
    let Err(error) = result else {
        panic!("an unstratifiable program was accepted");
    };
    let message = format!("{error:?}");
    assert!(
        message.to_lowercase().contains("stratif"),
        "the diagnostic does not mention stratification: {message}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a2_mutual_recursion_through_negation_is_also_rejected() {
    let result = Runner::new(
        "\
p(?X) :- q(?X), ~r(?X) .
r(?X) :- q(?X), ~p(?X) .",
    )
    .facts("q", [s!("a")])
    .run()
    .await;
    assert!(result.is_err(), "a negative cycle was accepted");
}
