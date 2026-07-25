//! End-to-end module storage and whole-program assembly against real (temp)
//! `gix` repositories.

use gix_query_ir::Registry;
use gix_query_rules::{Error, RuleStore, assemble};
use test_support::Fixture;

fn repo() -> (tempfile::TempDir, gix::Repository) {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    f.commit_file("a.txt", "a\n", "first");
    let repo = f.open();
    (dir, repo)
}

#[test]
fn a_module_round_trips_through_storage() {
    let (_dir, repo) = repo();
    let store = RuleStore::open(&repo).expect("open");

    assert_eq!(store.list().expect("list"), Vec::<String>::new());
    assert_eq!(store.get("review").expect("get"), None);

    store
        .put("review", "reviewed(B) :- member(B).")
        .expect("put");

    assert_eq!(store.list().expect("list"), vec!["review".to_owned()]);
    assert_eq!(
        store.get("review").expect("get"),
        Some("reviewed(B) :- member(B).".to_owned())
    );

    assert!(store.delete("review").expect("delete"));
    assert_eq!(store.list().expect("list"), Vec::<String>::new());
    assert!(!store.delete("review").expect("delete"));
}

#[test]
fn a_second_push_advances_the_same_module() {
    let (_dir, repo) = repo();
    let store = RuleStore::open(&repo).expect("open");

    store
        .put("review", "reviewed(B) :- member(B).")
        .expect("put");
    store
        .put("review", "reviewed(B) :- member(B), commit(B).")
        .expect("put");

    assert_eq!(
        store.get("review").expect("get"),
        Some("reviewed(B) :- member(B), commit(B).".to_owned())
    );
    assert_eq!(store.list().expect("list"), vec!["review".to_owned()]);
}

#[test]
fn independently_valid_modules_are_rejected_when_their_composition_cycles() {
    let (_dir, repo) = repo();
    let store = RuleStore::open(&repo).expect("open");

    // Each module is well-formed alone: neither one's own body negates
    // anything it also defines. Only assembled together do `a.p` and `b.q`
    // close a negative cycle across the module boundary.
    store
        .put("a", "pub p(X).\n\np(X) :- member(X), !b.q(X).")
        .expect("put a");
    store
        .put("b", "pub q(X).\n\nq(X) :- member(X), !a.p(X).")
        .expect("put b");

    let registry = Registry::host();
    let err = assemble(&store, &registry).expect_err("cross-module cycle must be rejected");
    assert!(
        matches!(err, Error::Check(_)),
        "expected a whole-program check failure, got {err:?}"
    );
}

#[test]
fn two_modules_with_no_cycle_assemble_and_check_cleanly() {
    let (_dir, repo) = repo();
    let store = RuleStore::open(&repo).expect("open");

    store
        .put(
            "active",
            "pub active_member(M).\n\nactive_member(M) :- member(M), !revoked(M).",
        )
        .expect("put active");
    store
        .put(
            "review",
            "pub reviewed(B).\n\nreviewed(B) :- claim(C), target(C, B), verdict(C, approve), \
             signer(C, M), active.active_member(M).",
        )
        .expect("put review");

    let registry = Registry::host();
    assemble(&store, &registry).expect("a non-cyclic composition checks cleanly");
}

#[test]
fn a_module_that_does_not_parse_is_reported() {
    let (_dir, repo) = repo();
    let store = RuleStore::open(&repo).expect("open");

    store
        .put("broken", "this is not valid( datalog")
        .expect("put");

    let registry = Registry::host();
    let err = assemble(&store, &registry).expect_err("a syntax error must be reported");
    assert!(matches!(err, Error::Parse { .. }));
}
