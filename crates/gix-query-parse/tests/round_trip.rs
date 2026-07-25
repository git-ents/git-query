//! Round-trip property test: parse -> print -> parse is a fixpoint.
//!
//! [`Span`] is part of every IR node's `PartialEq`, and a hand-built
//! [`Module`] carries no meaningful source positions, so comparing generated
//! IR directly against reparsed IR would fail on span noise having nothing
//! to do with correctness. Spans are stripped from both sides before
//! comparing instead: what is asserted is that the *content* -- predicates,
//! arities, terms, literal order, `pub` surface -- survives a print/parse
//! round trip exactly.

use std::sync::Arc;

use gix_query_ir::{
    Atom, Compare, CompareOp, Export, Literal, Module, PredicateKey, Rule, Span, SpannedTerm, Term,
};
use gix_query_parse::{parse_module, print_module};
use proptest::prelude::*;

fn arb_var_name() -> impl Strategy<Value = String> {
    prop_oneof!["[A-Z][A-Za-z0-9_]{0,5}", "_[A-Za-z0-9_]{1,5}",]
}

/// A bare, lowercase-led symbol -- always printable without quoting.
fn arb_bare_symbol() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,6}"
}

/// A digit-led symbol that is not all digits, like an unquoted object id --
/// printable bare, but only because it is not all-digit.
fn arb_digit_symbol() -> impl Strategy<Value = String> {
    "[0-9][a-z0-9]{0,5}[a-z]"
}

/// A symbol that can only be spelled quoted, deliberately built from
/// characters the printer must escape (`\\`, `"`) alongside ones it need
/// only quote (`/`, `.`, space, `-`).
fn arb_quoted_symbol() -> impl Strategy<Value = String> {
    let special = prop::sample::select(vec!['\\', '"', '\n', '\t', '/', '.', ' ', '-']);
    ("[a-zA-Z0-9]{1,6}", prop::collection::vec(special, 1..=3)).prop_map(|(base, specials)| {
        let mut s = base;
        for c in specials {
            s.push(c);
        }
        s
    })
}

fn arb_symbol() -> impl Strategy<Value = String> {
    prop_oneof![arb_bare_symbol(), arb_digit_symbol(), arb_quoted_symbol()]
}

/// Every `i64`, including the negative range -- negative integers are one of
/// the deliberately awkward cases this test is required to cover.
fn arb_int() -> impl Strategy<Value = i64> {
    any::<i64>()
}

fn arb_term() -> impl Strategy<Value = Term> {
    prop_oneof![
        arb_var_name().prop_map(Term::var),
        Just(Term::Anon),
        arb_int().prop_map(Term::int),
        arb_symbol().prop_map(Term::sym),
    ]
}

/// A single lowercase word, safe as a predicate name component: never `pub`
/// and never `magic_`-prefixed, both of which are reserved.
fn arb_predicate_word() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,6}".prop_filter("reserved word", |s| {
        s != "pub" && !s.starts_with(PredicateKey::MAGIC_PREFIX)
    })
}

/// A predicate name, sometimes qualified (`mod.local`) -- qualified names
/// are one of the deliberately awkward cases this test is required to
/// cover.
fn arb_predicate_name() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_predicate_word(),
        (arb_predicate_word(), arb_predicate_word()).prop_map(|(a, b)| format!("{a}.{b}")),
    ]
}

fn spanned_terms(terms: Vec<Term>) -> Vec<SpannedTerm> {
    terms.into_iter().map(SpannedTerm::synthetic).collect()
}

fn arb_atom(name: impl Strategy<Value = String>) -> impl Strategy<Value = Atom> {
    (name, prop::collection::vec(arb_term(), 0..=4)).prop_map(|(name, terms)| Atom {
        predicate: PredicateKey::new(name, terms.len()),
        terms: spanned_terms(terms),
        span: Span::synthetic(),
    })
}

fn arb_compare_op() -> impl Strategy<Value = CompareOp> {
    prop::sample::select(CompareOp::all().to_vec())
}

fn arb_compare() -> impl Strategy<Value = Compare> {
    (arb_compare_op(), arb_term(), arb_term()).prop_map(|(op, left, right)| Compare {
        op,
        left: SpannedTerm::synthetic(left),
        right: SpannedTerm::synthetic(right),
        span: Span::synthetic(),
    })
}

fn arb_literal() -> impl Strategy<Value = Literal> {
    prop_oneof![
        arb_atom(arb_predicate_name()).prop_map(Literal::Positive),
        arb_atom(arb_predicate_name()).prop_map(Literal::Negated),
        arb_compare().prop_map(Literal::Compare),
    ]
}

/// A rule body, weighted so zero-body facts -- one of the deliberately
/// awkward cases this test is required to cover -- show up often rather than
/// only as a size-range edge case.
fn arb_body() -> impl Strategy<Value = Vec<Literal>> {
    prop_oneof![
        1 => Just(Vec::new()),
        3 => prop::collection::vec(arb_literal(), 1..=4),
    ]
}

fn arb_rule() -> impl Strategy<Value = Rule> {
    (
        arb_predicate_word(),
        prop::collection::vec(arb_term(), 0..=4),
        arb_body(),
    )
        .prop_map(|(name, head_terms, body)| Rule {
            head: Atom {
                predicate: PredicateKey::new(name, head_terms.len()),
                terms: spanned_terms(head_terms),
                span: Span::synthetic(),
            },
            body,
            span: Span::synthetic(),
            origin: None,
        })
}

/// A module whose `pub` surface is always a subset of its own rule heads,
/// so it is guaranteed to pass the "pub declares an undefined predicate"
/// check and round-trip successfully.
fn arb_module() -> impl Strategy<Value = Module> {
    prop::collection::vec(arb_rule(), 1..=5).prop_flat_map(|rules| {
        let mut keys: Vec<PredicateKey> = rules.iter().map(|r| r.head.predicate.clone()).collect();
        keys.sort();
        keys.dedup();
        let mask = prop::collection::vec(any::<bool>(), keys.len());
        (Just(rules), Just(keys), mask).prop_map(|(rules, keys, mask)| {
            let exports = keys
                .into_iter()
                .zip(mask)
                .filter(|(_, take)| *take)
                .map(|(key, _)| Export {
                    key,
                    span: Span::synthetic(),
                })
                .collect();
            Module {
                name: Arc::from("m"),
                exports,
                rules,
            }
        })
    })
}

fn zero(span: &mut Span) {
    *span = Span::synthetic();
}

fn normalize_term(t: &mut SpannedTerm) {
    zero(&mut t.span);
}

fn normalize_atom(a: &mut Atom) {
    zero(&mut a.span);
    for t in &mut a.terms {
        normalize_term(t);
    }
}

fn normalize_literal(l: &mut Literal) {
    match l {
        Literal::Positive(a) | Literal::Negated(a) | Literal::Builtin(a) => normalize_atom(a),
        Literal::Compare(c) => {
            zero(&mut c.span);
            normalize_term(&mut c.left);
            normalize_term(&mut c.right);
        }
    }
}

fn normalize_rule(r: &mut Rule) {
    zero(&mut r.span);
    normalize_atom(&mut r.head);
    for l in &mut r.body {
        normalize_literal(l);
    }
}

fn normalize_module(m: &mut Module) {
    for e in &mut m.exports {
        zero(&mut e.span);
    }
    for r in &mut m.rules {
        normalize_rule(r);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `parse(print(m))` reconstructs `m` exactly, modulo the spans a
    /// synthetic module never had in the first place. Run twice more --
    /// `print(parse(print(m)))` equals `print(m)` -- would say the same
    /// thing about text stability; comparing IR content directly says it
    /// about the thing that actually matters, which is what a validator or
    /// an evaluator downstream would see.
    #[test]
    fn parse_print_parse_is_a_fixpoint(module in arb_module()) {
        let text = print_module(&module);
        let mut reparsed = match parse_module(module.name.clone(), &text) {
            Ok(m) => m,
            Err(errors) => panic!("failed to reparse printed module:\n{text}\n\nerrors: {errors:?}"),
        };
        let mut generated = module;
        normalize_module(&mut generated);
        normalize_module(&mut reparsed);
        prop_assert_eq!(generated, reparsed);
    }
}
