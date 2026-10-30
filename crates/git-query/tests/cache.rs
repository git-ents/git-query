//! `DEVPLAN.md` Phase 6's own safety net: run the query corpus twice, once
//! against the cache and once cold, and assert identical relations. A
//! subtly wrong cache key is exactly the kind of bug that would pass every
//! other test in this crate and only show up here, as a stale answer.
//!
//! Nemo's `execute()` enters a process-global timing mutex; run this file
//! with `--test-threads=1`.

use git_query::{QueryCache, RuleStore, Value};
use test_support::Fixture;

fn repo_with_module() -> (tempfile::TempDir, gix::Repository, String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    let first = f.commit("first");
    let second = f.commit("second");
    let repo = f.open();
    RuleStore::open(&repo)
        .expect("open store")
        .put(
            "ancestry",
            "pub ancestor(X, Y).\n\
             ancestor(X, Y) :- parent(X, Y).\n\
             ancestor(X, Y) :- parent(X, Z), ancestor(Z, Y).\n",
        )
        .expect("put module");
    (dir, repo, first, second)
}

/// A corpus spanning every kind of backing the cache key formula has to
/// treat differently: content-addressed (`commit`, `parent`), ref-glob
/// (`member`), and an IDB predicate a rule module defines.
fn corpus(second: &str) -> Vec<String> {
    vec![
        format!("commit(\"{second}\")"),
        format!("parent(\"{second}\", P)"),
        "member(M)".to_owned(),
        format!("ancestry.ancestor(\"{second}\", A)"),
    ]
}

#[test]
fn cached_and_cold_runs_agree_on_the_whole_corpus() {
    let (_dir, repo, _first, second) = repo_with_module();
    let queries = corpus(&second);

    let cold: Vec<Vec<Vec<Value>>> = queries
        .iter()
        .map(|q| git_query::run_body(&repo, q).unwrap_or_else(|e| panic!("{q}: {e}")))
        .collect();

    // First pass through a fresh cache: every query misses and primes an
    // entry.
    let cache = QueryCache::new();
    let primed: Vec<Vec<Vec<Value>>> = queries
        .iter()
        .map(|q| {
            git_query::run_body_cached(&repo, q, &cache).unwrap_or_else(|e| panic!("{q}: {e}"))
        })
        .collect();
    assert_eq!(
        primed, cold,
        "a cold cache must answer exactly as running uncached does"
    );
    assert_eq!(
        cache.len(),
        queries.len(),
        "every query should have missed and inserted once"
    );

    // Second pass through the same, now-primed cache: every query hits.
    let served: Vec<Vec<Vec<Value>>> = queries
        .iter()
        .map(|q| {
            git_query::run_body_cached(&repo, q, &cache).unwrap_or_else(|e| panic!("{q}: {e}"))
        })
        .collect();
    assert_eq!(
        served, cold,
        "a primed cache must answer exactly as running uncached does"
    );
    assert_eq!(cache.len(), queries.len(), "a hit must not grow the cache");
}

/// The bug `DEVPLAN.md` §1.1 exists to prevent: a member revoked between two
/// runs at the same rules/rev must not be served the pre-revocation answer.
/// This is what makes the footprint ref snapshot part of the key rather than
/// just `rev`.
#[test]
fn a_ref_moving_between_two_calls_invalidates_the_cache_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    f.commit("first");
    let repo = f.open();
    let cache = QueryCache::new();

    let before = git_query::run_body_cached(&repo, "member(M)", &cache).expect("runs");
    assert!(before.is_empty(), "no member ref exists yet");
    assert_eq!(cache.len(), 1);

    // A member ref appears -- the footprint ref snapshot must change, so this
    // must be a new cache entry, not a stale hit on the old one.
    let member = f.hash_object("Ada Lovelace <ada@example.com>");
    let target = f.commit_file("members.txt", "Ada\n", "add Ada");
    f.update_ref(&format!("refs/meta/members/{member}"), &target);

    let after = git_query::run_body_cached(&repo, "member(M)", &cache).expect("runs");
    assert_eq!(
        after,
        vec![vec![Value::sym(member.as_str())]],
        "the cache must reflect the moved ref, not the answer cached before it moved"
    );
    assert_eq!(
        cache.len(),
        2,
        "the ref move must have produced a second entry, not overwritten the first"
    );
}

/// A query touching only content-addressed predicates has an empty footprint
/// ref snapshot, so publishing an unrelated rule module -- which does move
/// the rules snapshot -- must still invalidate it: the rules snapshot is a
/// key component independent of the footprint.
#[test]
fn publishing_a_rule_module_invalidates_every_entry_even_content_addressed_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    let first = f.commit("first");
    let repo = f.open();
    let cache = QueryCache::new();

    let goal = format!("commit(\"{first}\")");
    git_query::run_body_cached(&repo, &goal, &cache).expect("runs");
    assert_eq!(cache.len(), 1);

    RuleStore::open(&repo)
        .expect("open store")
        .put(
            "unrelated",
            "pub noop(X, Y).\nnoop(X, Y) :- parent(X, Y).\n",
        )
        .expect("put module");

    git_query::run_body_cached(&repo, &goal, &cache).expect("runs");
    assert_eq!(
        cache.len(),
        2,
        "publishing a rule module must invalidate even a query with an empty footprint"
    );
}

/// The plain, uncached entry points never touch the cache at all: two
/// consecutive calls through the same repository re-derive the answer, not
/// reuse an entry from a `QueryCache` a caller never mentioned. This is the
/// "off by default" half of the feature, expressed as a fact about which
/// function was called rather than a flag that could be left set wrong.
#[test]
fn the_uncached_entry_points_never_consult_a_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    f.commit("first");
    let repo = f.open();

    let before = git_query::run_body(&repo, "member(M)").expect("runs");
    assert!(before.is_empty());

    let member = f.hash_object("Ada Lovelace <ada@example.com>");
    let target = f.commit_file("members.txt", "Ada\n", "add Ada");
    f.update_ref(&format!("refs/meta/members/{member}"), &target);

    let after = git_query::run_body(&repo, "member(M)").expect("runs");
    assert_eq!(
        after,
        vec![vec![Value::sym(member.as_str())]],
        "run_body must see the moved ref -- there is no cache in its path to disagree with"
    );
}
