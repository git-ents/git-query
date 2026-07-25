//! End-to-end fixtures over real source text, parsed by `gix-query-parse`
//! and validated by `gix-query-check`.
//!
//! Two obligations from `DEVPLAN.md` live here rather than as unit tests,
//! because both are about the *text* an author would actually write, not
//! about hand-built IR:
//!
//! - The corrected review vocabulary from §1.7 -- the doc's own flagship
//!   program, with the `Rev`-threading fix applied -- must parse, resolve,
//!   and validate clean, through all nine passes.
//! - The doc's *original*, un-threaded `introduced/2` must be rejected by
//!   pass 8, naming `C` -- that is the exact bug the mode system exists to
//!   catch, on the first program anyone reads.

use gix_query_ir::Registry;
use gix_query_parse::{parse_module, resolve_module};

/// §1.7's corrected vocabulary, verbatim from `DEVPLAN.md`. Every `pub`
/// signature, every clause, in the order the doc gives them.
const CORRECTED_VOCABULARY: &str = r#"
pub reviewed(B).
pub unreviewed(Rev, B).
pub unreviewed_line(Rev, Path, B, N).
pub mergeable(Rev).

active_member(M) :- member(M), !revoked(M).
review(C) :- claim(C), kind(C, review).

approved_by(B, M) :- review(C), target(C, B), signer(C, M), verdict(C, approve), active_member(M).
rejected(B) :- review(C), target(C, B), verdict(C, reject), signer(C, M), active_member(M).
reviewed(B) :- approved_by(B, _).

reach(Rev, Rev) :- commit(Rev).
reach(Rev, C) :- reach(Rev, X), parent(X, C).

has_parent(C) :- parent(C, _).
introduced(Rev, B, C) :- reach(Rev, C), tree_entry(C, P, B), parent(C, Pc), !tree_entry(Pc, P, B).
introduced(Rev, B, C) :- reach(Rev, C), tree_entry(C, P, B), !has_parent(C).
authored(Rev, B, M) :- introduced(Rev, B, C), author(C, M).
self_approved(Rev, B, M) :- authored(Rev, B, M), approved_by(B, M).

carries(same, intact).
carries(moved, intact).

reviewed_span(Rev, B, S, E) :- review(C), target(C, A), anchor(A), verdict(C, approve), signer(C, M), active_member(M), bind(A, Rev, B, S, E, Pos, Con), carries(Pos, Con).

ambiguous(A, Rev) :- bind(A, Rev, B1, _, _, _, _), bind(A, Rev, B2, _, _, _, _), B1 != B2.

reviewed_line(Rev, B, N) :- reviewed_span(Rev, B, S, E), line(B, N), N >= S, N <= E.
unreviewed_line(Rev, P, B, N) :- tree_entry(Rev, P, B), line(B, N), !reviewed_line(Rev, B, N).

unreviewed(Rev, B) :- tree_entry(Rev, _, B), !reviewed(B).
blocked(Rev) :- unreviewed(Rev, _).
blocked(Rev) :- tree_entry(Rev, _, B), rejected(B).
blocked(Rev) :- tree_entry(Rev, _, B), self_approved(Rev, B, _).
mergeable(Rev) :- commit(Rev), !blocked(Rev).
"#;

/// The doc's *original* `introduced/2`, before §1.7's `Rev`-threading fix:
/// no `reach`, no `Rev` argument anywhere. `tree_entry` is `+Rev`, and this
/// calls it with a bare commit variable that is never bound at the call
/// site in `authored` -- the mode violation §1.7 exists to document.
const ORIGINAL_UNTHREADED_INTRODUCED: &str = r#"
introduced(B, C) :- tree_entry(C, P, B), parent(C, Pc), !tree_entry(Pc, P, B).
authored(B, M) :- introduced(B, C), author(C, M).
"#;

fn checked_program(source: &str) -> Result<gix_query_ir::CheckedProgram, gix_query_check::Errors> {
    let registry = Registry::host();
    let mut module = parse_module("review", source).expect("parses");
    resolve_module(&mut module, &registry);
    let mut program = gix_query_ir::Program::new();
    program.add_module(module);
    gix_query_check::check(&program, &registry)
}

#[test]
fn the_corrected_review_vocabulary_validates_end_to_end() {
    match checked_program(CORRECTED_VOCABULARY) {
        Ok(_) => {}
        Err(errors) => {
            panic!("expected the corrected §1.7 vocabulary to validate, got: {errors:#?}")
        }
    }
}

#[test]
fn the_corrected_vocabulary_infers_rev_threading_exactly_as_the_doc_says() {
    let checked = checked_program(CORRECTED_VOCABULARY).expect("validates");
    let required = |name: &str, arity: usize| {
        checked
            .registry
            .predicate(&gix_query_ir::PredicateKey::new(name, arity))
            .and_then(gix_query_ir::PredicateEntry::always_required_bound)
            .unwrap_or_else(|| panic!("{name}/{arity} should be registered"))
    };
    // "reach/2, introduced/3, authored/3 | +Rev | `commit/1` is required-bound"
    for (name, arity) in [("reach", 2), ("introduced", 3), ("authored", 3)] {
        assert!(
            required(name, arity).contains(0),
            "{name}/{arity} should require Rev bound"
        );
    }
    // "reviewed/1, approved_by/2, rejected/1 | all free"
    for (name, arity) in [("reviewed", 1), ("approved_by", 2), ("rejected", 1)] {
        assert!(
            required(name, arity).is_empty(),
            "{name}/{arity} should be all-free"
        );
    }
    // "mergeable/1 | +Rev | via `blocked`"
    assert!(required("mergeable", 1).contains(0));
}

#[test]
fn the_docs_original_introduced_is_rejected_by_pass_eight_naming_c() {
    let errors = checked_program(ORIGINAL_UNTHREADED_INTRODUCED)
        .expect_err("the un-threaded introduced/2 must fail validation");
    let hit = errors.iter().find(|e| {
        e.pass() == gix_query_check::Pass::Modes
            && matches!(
                e,
                gix_query_check::CheckError::ModeViolation { var: Some(v), .. }
                    if v.as_str() == "C"
            )
    });
    assert!(
        hit.is_some(),
        "expected a pass-8 mode violation naming `C`, got: {errors:#?}"
    );
}
