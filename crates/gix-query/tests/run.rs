//! Tier 1 and tier 3, end-to-end against real (temp) `gix` repositories.
//!
//! Nemo's `execute()` enters a process-global timing mutex; run this file
//! with `--test-threads=1`.

use gix_query::{RuleStore, Value};
use test_support::Fixture;

fn repo() -> (tempfile::TempDir, gix::Repository, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    let first = f.commit("first");
    let second = f.commit("second");
    let repo = f.open();
    (dir, repo, first, second)
}

#[test]
fn tier1_bound_position_filters_to_existence() {
    let (_dir, repo, first, _second) = repo();

    let present =
        gix_query::run_predicate(&repo, "commit", &[(0, Value::Sym(first.into()))]).expect("runs");
    assert_eq!(
        present,
        vec![Vec::new()],
        "one empty row: the commit exists"
    );

    let absent = gix_query::run_predicate(
        &repo,
        "commit",
        &[(
            0,
            Value::Sym("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into()),
        )],
    )
    .expect("runs");
    assert_eq!(absent, Vec::<Vec<Value>>::new(), "no rows: no such commit");
}

#[test]
fn tier1_free_positions_become_columns_in_signature_order() {
    let (_dir, repo, first, second) = repo();

    let rows =
        gix_query::run_predicate(&repo, "parent", &[(0, Value::Sym(second.into()))]).expect("runs");
    assert_eq!(rows, vec![vec![Value::Sym(first.into())]]);
}

#[test]
fn tier1_unknown_predicate_is_reported_not_panicked() {
    let (_dir, repo, ..) = repo();
    let err = gix_query::run_predicate(&repo, "no_such_predicate", &[]).unwrap_err();
    assert!(matches!(err, gix_query::QueryError::UnknownPredicate(_)));
}

#[test]
fn tier1_bind_position_out_of_arity_is_reported_not_panicked() {
    let (_dir, repo, first, _second) = repo();
    let err =
        gix_query::run_predicate(&repo, "commit", &[(3, Value::Sym(first.into()))]).unwrap_err();
    assert!(matches!(err, gix_query::QueryError::BindOutOfRange { .. }));
}

#[test]
fn tier1_resolves_a_rule_modules_qualified_predicate() {
    let (_dir, repo, first, second) = repo();
    let store = RuleStore::open(&repo).expect("open store");
    store
        .put(
            "ancestry",
            "pub ancestor(X, Y).\n\
             ancestor(X, Y) :- parent(X, Y).\n\
             ancestor(X, Y) :- parent(X, Z), ancestor(Z, Y).\n",
        )
        .expect("put module");

    let rows = gix_query::run_predicate(
        &repo,
        "ancestry.ancestor",
        &[(0, Value::Sym(second.into()))],
    )
    .expect("runs");
    assert_eq!(rows, vec![vec![Value::Sym(first.into())]]);
}
