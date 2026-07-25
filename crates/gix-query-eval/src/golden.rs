//! The spike's A2 golden set, rewritten against real types, and the
//! rewritten-vs-unrewritten agreement check.
//!
//! Every program here was hand-verified in `spikes/nemo-spike` against Nemo
//! directly; this module re-asserts the same relations through the real
//! pipeline -- rewrite, lowering, the demand loop -- by exact set equality.
//! With the naive oracle gone (`DEVPLAN.md` 2.8), the agreement check at the
//! bottom of this file is the only thing guarding the rewrite itself: both
//! runs go through the same engine, so a mismatch means the rewrite changed
//! the answer, not that the engine is wrong.
#![cfg(test)]

use std::collections::BTreeMap as Map;

use gix_query_ir::{
    Atom, CheckedProgram, Compare, CompareOp, Goal, Literal, PredicateEntry, Program, Registry,
    Rule, RuleOrigin, Span, SpannedTerm, Stratification, Term, Value, Var, Visibility,
};

use crate::rewrite::{RewrittenProgram, rewrite};
use crate::testutil::{StubTable, edb_enumerable};
use crate::{NemoProgram, run_to_fixpoint};

fn pos(name: &str, vars: &[&str]) -> Literal {
    Literal::Positive(Atom::new(
        name,
        vars.iter().map(|v| Term::var(*v)).collect(),
    ))
}

fn neg(name: &str, vars: &[&str]) -> Literal {
    Literal::Negated(Atom::new(
        name,
        vars.iter().map(|v| Term::var(*v)).collect(),
    ))
}

fn sym(s: &str) -> Value {
    Value::sym(s)
}

/// A rule guarded by a single always-true fact, so `head` behaves exactly
/// like a ground fact -- except it is a `Statement::Rule` to Nemo, not a
/// `Statement::Fact`.
///
/// Nemo's physical storage panics (`assertion failed: source.arity() ==
/// arity`) when the *same* predicate is defined by a bare `Statement::Fact`
/// and a `Statement::Rule` together -- reproduced and pinned in
/// `crate::conformance` as
/// `a_predicate_defined_by_both_a_ground_fact_and_a_rule_is_a_nemo_storage_bug`.
/// `carries4` below needs the "constants in a head, multiple clauses" shape
/// but must stay clear of that combination, so every clause is a rule.
///
/// A bare comparison will not do either: Nemo rejects "a rule without
/// positive literals" outright, so the guard has to be a real (if trivial)
/// positive literal over an EDB predicate the caller feeds exactly one row.
///
/// The guard's own argument must differ between clauses, not just the head:
/// two rules whose bodies are the identical ground atom (`always4("x")` for
/// both) silently derive only the *first* head in this pinned Nemo version --
/// a second engine-level oddity, distinct from the fact/rule storage panic,
/// found while writing this test. Keying the guard on the head's own first
/// argument sidesteps it and is what a real emitter would do anyway, since
/// nothing forces two unrelated ground facts to share a witness.
fn tautological_fact(head: Atom) -> Rule {
    let discriminator = head.terms[0].term.clone();
    Rule::new(
        head,
        vec![Literal::Positive(Atom::new("always4", vec![discriminator]))],
    )
}

/// Run `program`/`goal` to a fixpoint over `facts`, sorted, deduplicated
/// tuples in `select` order -- mirroring `Session::relation` in the spike, so
/// a golden assertion here is about the *set*, not an order nothing promises.
fn relation(checked: &CheckedProgram, goal: &Goal, facts: &StubTable) -> Vec<Vec<Value>> {
    let rewritten = rewrite(checked, goal);
    let emitted = NemoProgram::emit(&rewritten).expect("emits");
    emitted.audit().expect("audit passes");
    let builtins = StubTable::new();
    let mut out = run_to_fixpoint(&rewritten, &emitted, facts, &builtins, 16)
        .expect("converges")
        .answer;
    out.sort();
    out.dedup();
    out
}

#[test]
fn transitive_closure_is_a_set_and_a_cycle_terminates() {
    // A diamond, so `a -> d` is derivable by two distinct paths and must
    // appear once under set semantics.
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "edge", 2);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("tc", 2),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("tc", vec![Term::var("X"), Term::var("Y")]),
            vec![pos("edge", &["X", "Y"])],
        ),
        "test",
    );
    program.push_rule(
        Rule::new(
            Atom::new("tc", vec![Term::var("X"), Term::var("Z")]),
            vec![pos("edge", &["X", "Y"]), pos("tc", &["Y", "Z"])],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let goal = Goal::new(
        vec![pos("tc", &["X", "Y"])],
        vec![Var::new("X"), Var::new("Y")],
    );

    let diamond = StubTable::new().with(
        gix_query_ir::PredicateKey::new("edge", 2),
        vec![
            vec![sym("a"), sym("b")],
            vec![sym("a"), sym("c")],
            vec![sym("b"), sym("d")],
            vec![sym("c"), sym("d")],
        ],
    );
    assert_eq!(
        relation(&checked, &goal, &diamond),
        vec![
            vec![sym("a"), sym("b")],
            vec![sym("a"), sym("c")],
            vec![sym("a"), sym("d")],
            vec![sym("b"), sym("d")],
            vec![sym("c"), sym("d")],
        ]
    );

    let cycle = StubTable::new().with(
        gix_query_ir::PredicateKey::new("edge", 2),
        vec![vec![sym("a"), sym("b")], vec![sym("b"), sym("a")]],
    );
    assert_eq!(
        relation(&checked, &goal, &cycle),
        vec![
            vec![sym("a"), sym("a")],
            vec![sym("a"), sym("b")],
            vec![sym("b"), sym("a")],
            vec![sym("b"), sym("b")],
        ]
    );
}

#[test]
fn stratified_negation_one_stratum() {
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "member2", 1);
    edb_enumerable(&mut registry, "revoked2", 1);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("active2", 1),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("active2", vec![Term::var("M")]),
            vec![pos("member2", &["M"]), neg("revoked2", &["M"])],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let goal = Goal::new(vec![pos("active2", &["M"])], vec![Var::new("M")]);
    let facts = StubTable::new()
        .with(
            gix_query_ir::PredicateKey::new("member2", 1),
            vec![vec![sym("alice")], vec![sym("bob")], vec![sym("carol")]],
        )
        .with(
            gix_query_ir::PredicateKey::new("revoked2", 1),
            vec![vec![sym("bob")]],
        );
    assert_eq!(
        relation(&checked, &goal, &facts),
        vec![vec![sym("alice")], vec![sym("carol")]]
    );
}

#[test]
fn stratified_negation_across_three_strata() {
    // c is stratum 0, b negates c so it is stratum 1, a negates b so it is
    // stratum 2. Evaluating the strata out of order would over-derive `a`.
    let mut registry = Registry::host();
    for name in ["c3", "d3", "e3"] {
        edb_enumerable(&mut registry, name, 1);
    }
    for name in ["a3", "b3"] {
        registry
            .insert_predicate(PredicateEntry::idb(
                gix_query_ir::PredicateKey::new(name, 1),
                Visibility::Public,
            ))
            .expect("insertable");
    }
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("b3", vec![Term::var("X")]),
            vec![pos("d3", &["X"]), neg("c3", &["X"])],
        ),
        "test",
    );
    program.push_rule(
        Rule::new(
            Atom::new("a3", vec![Term::var("X")]),
            vec![pos("e3", &["X"]), neg("b3", &["X"])],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let facts = StubTable::new()
        .with(
            gix_query_ir::PredicateKey::new("c3", 1),
            vec![vec![sym("x")]],
        )
        .with(
            gix_query_ir::PredicateKey::new("d3", 1),
            vec![vec![sym("x")], vec![sym("y")]],
        )
        .with(
            gix_query_ir::PredicateKey::new("e3", 1),
            vec![vec![sym("x")], vec![sym("y")]],
        );

    let goal_b = Goal::new(vec![pos("b3", &["X"])], vec![Var::new("X")]);
    assert_eq!(relation(&checked, &goal_b, &facts), vec![vec![sym("y")]]);
    let goal_a = Goal::new(vec![pos("a3", &["X"])], vec![Var::new("X")]);
    assert_eq!(relation(&checked, &goal_a, &facts), vec![vec![sym("x")]]);
}

#[test]
fn a_retraction_shaped_update_has_no_state_bleed() {
    // Two independent checked programs (one per "session"), so what has to
    // hold is that a second run in the same process sees only the facts it
    // was given -- no shared table survives between them.
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "member3", 1);
    edb_enumerable(&mut registry, "revoked3", 1);
    edb_enumerable(&mut registry, "signer3", 1);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("active3", 1),
            Visibility::Public,
        ))
        .expect("insertable");
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("reviewer3", 1),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("active3", vec![Term::var("M")]),
            vec![pos("member3", &["M"]), neg("revoked3", &["M"])],
        ),
        "test",
    );
    program.push_rule(
        Rule::new(
            Atom::new("reviewer3", vec![Term::var("M")]),
            vec![pos("active3", &["M"]), pos("signer3", &["M"])],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let goal = Goal::new(vec![pos("reviewer3", &["M"])], vec![Var::new("M")]);
    let base_members = vec![vec![sym("alice")], vec![sym("bob")]];
    let base_signers = vec![vec![sym("alice")], vec![sym("bob")]];

    // `revoked3` still needs an entry even with nothing revoked: the negated
    // literal `!revoked3(M)` is a demand site regardless of whether anything
    // is ever revoked (negation always needs its argument bound), so the
    // stub must have *a* table for it, empty or not.
    let first = StubTable::new()
        .with(
            gix_query_ir::PredicateKey::new("member3", 1),
            base_members.clone(),
        )
        .with(
            gix_query_ir::PredicateKey::new("signer3", 1),
            base_signers.clone(),
        )
        .with(gix_query_ir::PredicateKey::new("revoked3", 1), vec![]);
    assert_eq!(
        relation(&checked, &goal, &first),
        vec![vec![sym("alice")], vec![sym("bob")]]
    );

    let second = StubTable::new()
        .with(
            gix_query_ir::PredicateKey::new("member3", 1),
            base_members.clone(),
        )
        .with(
            gix_query_ir::PredicateKey::new("signer3", 1),
            base_signers.clone(),
        )
        .with(
            gix_query_ir::PredicateKey::new("revoked3", 1),
            vec![vec![sym("alice")]],
        );
    assert_eq!(relation(&checked, &goal, &second), vec![vec![sym("bob")]]);

    // The first table is untouched by the second having run.
    assert_eq!(
        relation(&checked, &goal, &first),
        vec![vec![sym("alice")], vec![sym("bob")]]
    );
}

#[test]
fn multi_clause_predicates_and_constants_in_heads() {
    // `carries/2` defined partly by ground facts with constants in the head,
    // partly by a rule, consumed by a negated join.
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "weak4", 1);
    edb_enumerable(&mut registry, "suppressed4", 1);
    edb_enumerable(&mut registry, "always4", 1);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("carries4", 2),
            Visibility::Public,
        ))
        .expect("insertable");
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("propagates4", 2),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        tautological_fact(Atom::new(
            "carries4",
            vec![Term::sym("same"), Term::sym("intact")],
        )),
        "test",
    );
    program.push_rule(
        tautological_fact(Atom::new(
            "carries4",
            vec![Term::sym("moved"), Term::sym("intact")],
        )),
        "test",
    );
    program.push_rule(
        Rule::new(
            Atom::new("carries4", vec![Term::var("P"), Term::sym("edited")]),
            vec![pos("weak4", &["P"])],
        ),
        "test",
    );
    program.push_rule(
        Rule::new(
            Atom::new("propagates4", vec![Term::var("P"), Term::var("C")]),
            vec![pos("carries4", &["P", "C"]), neg("suppressed4", &["P"])],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let facts = StubTable::new()
        .with(
            gix_query_ir::PredicateKey::new("weak4", 1),
            vec![vec![sym("lost")]],
        )
        .with(
            gix_query_ir::PredicateKey::new("suppressed4", 1),
            vec![vec![sym("moved")]],
        )
        .with(
            gix_query_ir::PredicateKey::new("always4", 1),
            vec![vec![sym("same")], vec![sym("moved")]],
        );

    let carries_goal = Goal::new(
        vec![pos("carries4", &["P", "C"])],
        vec![Var::new("P"), Var::new("C")],
    );
    assert_eq!(
        relation(&checked, &carries_goal, &facts),
        vec![
            vec![sym("lost"), sym("edited")],
            vec![sym("moved"), sym("intact")],
            vec![sym("same"), sym("intact")],
        ]
    );

    let propagates_goal = Goal::new(
        vec![pos("propagates4", &["P", "C"])],
        vec![Var::new("P"), Var::new("C")],
    );
    assert_eq!(
        relation(&checked, &propagates_goal, &facts),
        vec![
            vec![sym("lost"), sym("edited")],
            vec![sym("same"), sym("intact")]
        ]
    );
}

#[test]
fn a_join_with_an_inequality_comparison() {
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "bind5", 3);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("ambiguous5", 2),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("ambiguous5", vec![Term::var("A"), Term::var("R")]),
            vec![
                pos("bind5", &["A", "R", "P1"]),
                pos("bind5", &["A", "R", "P2"]),
                Literal::Compare(Compare {
                    op: CompareOp::Ne,
                    left: SpannedTerm::synthetic(Term::var("P1")),
                    right: SpannedTerm::synthetic(Term::var("P2")),
                    span: Span::synthetic(),
                }),
            ],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let goal = Goal::new(
        vec![pos("ambiguous5", &["A", "R"])],
        vec![Var::new("A"), Var::new("R")],
    );
    let facts = StubTable::new().with(
        gix_query_ir::PredicateKey::new("bind5", 3),
        vec![
            vec![sym("anchor1"), sym("rev1"), sym("p10")],
            vec![sym("anchor1"), sym("rev1"), sym("p20")],
            vec![sym("anchor2"), sym("rev1"), sym("p30")],
            vec![sym("anchor3"), sym("rev2"), sym("p40")],
            vec![sym("anchor3"), sym("rev3"), sym("p50")],
        ],
    );
    assert_eq!(
        relation(&checked, &goal, &facts),
        vec![vec![sym("anchor1"), sym("rev1")]]
    );
}

#[test]
fn ordering_comparisons_hold_on_integers() {
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "line6", 1);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("after6", 2),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("after6", vec![Term::var("A"), Term::var("B")]),
            vec![
                pos("line6", &["A"]),
                pos("line6", &["B"]),
                Literal::Compare(Compare {
                    op: CompareOp::Gt,
                    left: SpannedTerm::synthetic(Term::var("A")),
                    right: SpannedTerm::synthetic(Term::var("B")),
                    span: Span::synthetic(),
                }),
            ],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );
    let goal = Goal::new(
        vec![pos("after6", &["A", "B"])],
        vec![Var::new("A"), Var::new("B")],
    );
    let facts = StubTable::new().with(
        gix_query_ir::PredicateKey::new("line6", 1),
        vec![
            vec![Value::Int(1)],
            vec![Value::Int(2)],
            vec![Value::Int(3)],
        ],
    );
    assert_eq!(
        relation(&checked, &goal, &facts),
        vec![
            vec![Value::Int(2), Value::Int(1)],
            vec![Value::Int(3), Value::Int(1)],
            vec![Value::Int(3), Value::Int(2)],
        ]
    );
}

/// Build a trivial "unrewritten" program: the checked program's own rules,
/// unmodified, plus a bare answer rule reading the goal straight off them.
/// No guard, no magic relation, no demand -- exactly what running the goal
/// through unrestricted bottom-up evaluation means. Every predicate the goal
/// or a rule body touches is loaded eagerly, which is only affordable
/// because this helper exists to test small programs, never to run one for
/// real.
fn unrewritten(checked: &CheckedProgram, goal: &Goal) -> RewrittenProgram {
    let mut program = Program::new();
    for rule in checked.program().rules() {
        program.push_rule(
            Rule {
                head: rule.head.clone(),
                body: rule.body.clone(),
                span: rule.span,
                origin: Some(RuleOrigin {
                    source: None,
                    kind: gix_query_ir::RewriteKind::Guarded,
                    body_map: rule.body.iter().enumerate().map(|(i, _)| Some(i)).collect(),
                }),
            },
            "unrewritten",
        );
    }
    let answer = gix_query_ir::PredicateKey::new(
        format!("{}answer_plain", gix_query_ir::PredicateKey::MAGIC_PREFIX),
        goal.select.len(),
    );
    program.push_rule(
        Rule {
            head: Atom {
                predicate: answer.clone(),
                terms: goal
                    .select
                    .iter()
                    .map(|v| SpannedTerm::synthetic(Term::Var(v.clone())))
                    .collect(),
                span: Span::synthetic(),
            },
            body: goal.body.clone(),
            span: Span::synthetic(),
            origin: Some(RuleOrigin {
                source: None,
                kind: gix_query_ir::RewriteKind::Guarded,
                body_map: goal.body.iter().enumerate().map(|(i, _)| Some(i)).collect(),
            }),
        },
        "unrewritten",
    );

    // Only the EDB predicates the program or the goal actually reference --
    // not every predicate `Registry::host()` happens to carry -- or a stub
    // fed only the ones a small test program touches would be asked for
    // `commit`, `anchor`, and every other host relation it never mentioned.
    let referenced = checked
        .program()
        .rules()
        .iter()
        .flat_map(|r| r.body.iter())
        .chain(goal.body.iter())
        .filter_map(|l| l.atom())
        .map(|a| a.predicate.clone());
    let eager = referenced
        .filter(|key| {
            checked
                .registry()
                .predicate(key)
                .is_some_and(|e| e.class.is_edb())
        })
        .collect();

    RewrittenProgram {
        rules: program,
        demand: Vec::new(),
        eager,
        answer,
    }
}

#[test]
fn rewritten_and_unrewritten_agree_on_the_demanded_subset() {
    // `reach(X,Y) :- edge(X,Y). reach(X,Z) :- edge(X,Y), reach(Y,Z).` with
    // `+X` demanded by the goal. Small enough to run both ways: the
    // unrewritten path computes `reach` over the whole graph and is filtered
    // down to the goal's bound `X` afterward, since it never narrowed in the
    // first place; the rewritten path only ever computes that slice.
    let mut registry = Registry::host();
    edb_enumerable(&mut registry, "edge7", 2);
    registry
        .insert_predicate(PredicateEntry::idb(
            gix_query_ir::PredicateKey::new("reach7", 2),
            Visibility::Public,
        ))
        .expect("insertable");
    let mut program = Program::new();
    program.push_rule(
        Rule::new(
            Atom::new("reach7", vec![Term::var("X"), Term::var("Y")]),
            vec![pos("edge7", &["X", "Y"])],
        ),
        "test",
    );
    program.push_rule(
        Rule::new(
            Atom::new("reach7", vec![Term::var("X"), Term::var("Z")]),
            vec![pos("edge7", &["X", "Y"]), pos("reach7", &["Y", "Z"])],
        ),
        "test",
    );
    let checked = CheckedProgram::assemble(
        program,
        registry,
        Stratification::default(),
        Map::new(),
        Map::new(),
    );

    let facts = StubTable::new().with(
        gix_query_ir::PredicateKey::new("edge7", 2),
        vec![
            vec![sym("a"), sym("b")],
            vec![sym("b"), sym("c")],
            vec![sym("c"), sym("d")],
            vec![sym("x"), sym("y")],
        ],
    );

    // The goal binds `X` to the constant "a" -- exactly what a tier-1 CLI
    // call desugars to.
    let goal = Goal::new(
        vec![Literal::Positive(Atom::new(
            "reach7",
            vec![Term::sym("a"), Term::var("Z")],
        ))],
        vec![Var::new("Z")],
    );

    let rewritten_program = rewrite(&checked, &goal);
    let emitted = NemoProgram::emit(&rewritten_program).expect("emits");
    emitted.audit().expect("audit passes");
    let builtins = StubTable::new();
    let mut rewritten_answer = run_to_fixpoint(&rewritten_program, &emitted, &facts, &builtins, 16)
        .expect("converges")
        .answer;
    rewritten_answer.sort();

    let plain = unrewritten(
        &checked,
        &Goal::new(
            vec![pos("reach7", &["X", "Z"])],
            vec![Var::new("X"), Var::new("Z")],
        ),
    );
    let plain_emitted = NemoProgram::emit(&plain).expect("emits");
    let plain_all = run_to_fixpoint(&plain, &plain_emitted, &facts, &builtins, 16)
        .expect("converges")
        .answer;
    let mut plain_restricted: Vec<Value> = plain_all
        .into_iter()
        .filter(|row| row[0] == sym("a"))
        .map(|row| row[1].clone())
        .collect();
    plain_restricted.sort();

    let rewritten_z: Vec<Value> = rewritten_answer
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(rewritten_z, plain_restricted);
    assert_eq!(rewritten_z, vec![sym("b"), sym("c"), sym("d")]);
}

#[test]
fn a_call_site_that_binds_more_than_required_still_derives() {
    // `p8(X, Y) :- q8(X, Y)` with `q8` moded `(+, -)`: pass 8 infers `p8`'s
    // own required-bound set as `{0}` (only `X`), since `Y` arrives as
    // `q8`'s output. `caller8(A, B) :- other8(A, B), p8(A, B)` calls `p8`
    // with *both* arguments already bound, from `other8` -- strictly more
    // than `p8` requires, which mode discipline explicitly allows
    // (`ModeVector::satisfied_by` treats binding more than required as
    // always fine).
    //
    // This is a regression test for a defect `gix-query-check`'s pass 8 used
    // to have: recording a derived call's body adornment as the call site's
    // raw bound set (`{0, 1}` here) rather than normalizing it to the
    // callee's required-bound set. The rewrite guards `p8`'s rule on its
    // *head* adornment (`{0}`) but keys demand at a call site on the *body*
    // adornment it was handed; a call site recorded as `{0, 1}` would make
    // the rewrite emit `magic_p8_bb` while `p8`'s guard consumes
    // `magic_p8_bf` -- demand produced, never consumed, `p8` derives
    // nothing, and the query silently answers zero rows. `pass8_modes.rs`'s
    // `body_adornment` now normalizes a derived call to the callee's
    // required-bound set (see its own
    // `a_derived_call_records_the_callee_requirement_not_the_call_site`),
    // which this test exercises end to end through the rewrite and the
    // demand loop rather than through `gix-query-check`'s own unit test
    // alone.
    let mut registry = Registry::host();
    registry
        .insert_predicate(PredicateEntry {
            key: gix_query_ir::PredicateKey::new("q8", 2),
            types: vec![gix_query_ir::TypeSlot::Infer; 2],
            visibility: Visibility::Public,
            class: gix_query_ir::PredicateClass::Edb {
                modes: gix_query_ir::ModeSet::new([gix_query_ir::ModeVector::new(vec![
                    gix_query_ir::Mode::Bound,
                    gix_query_ir::Mode::Free,
                ])]),
                backing: vec![gix_query_ir::Backing::AdHoc],
            },
            doc: std::borrow::Cow::Borrowed(""),
        })
        .expect("insertable");
    edb_enumerable(&mut registry, "other8", 2);

    // `Program::push_rule` alone never updates `Program::defined` -- that is
    // reserved for the magic-set rewrite's own generated rules, which must
    // not look like newly authored predicates. An author-written fixture
    // needs `add_module`, or `gix-query-check`'s registry growth phase never
    // sees `p8`/`caller8` as predicates it may infer a mode for at all.
    let mut program = Program::new();
    let mut module = gix_query_ir::Module::new("test");
    module.rules.push(Rule::new(
        Atom::new("p8", vec![Term::var("X"), Term::var("Y")]),
        vec![pos("q8", &["X", "Y"])],
    ));
    module.rules.push(Rule::new(
        Atom::new("caller8", vec![Term::var("A"), Term::var("B")]),
        vec![pos("other8", &["A", "B"]), pos("p8", &["A", "B"])],
    ));
    program.add_module(module);

    let checked = gix_query_check::check(&program, &registry).expect("program validates");
    let goal = Goal::new(
        vec![pos("caller8", &["A", "B"])],
        vec![Var::new("A"), Var::new("B")],
    );
    let facts = StubTable::new()
        .with(
            gix_query_ir::PredicateKey::new("q8", 2),
            vec![vec![sym("a"), sym("y1")], vec![sym("b"), sym("y2")]],
        )
        .with(
            gix_query_ir::PredicateKey::new("other8", 2),
            vec![vec![sym("a"), sym("y1")], vec![sym("b"), sym("y2")]],
        );

    assert_eq!(
        relation(&checked, &goal, &facts),
        vec![vec![sym("a"), sym("y1")], vec![sym("b"), sym("y2")]]
    );
}
