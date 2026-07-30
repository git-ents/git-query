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

#[test]
fn explain_reports_no_derivation_for_an_absent_fact() {
    let (_dir, repo, ..) = repo();
    let tree = gix_query::explain_predicate(
        &repo,
        "commit",
        &[(
            0,
            Value::Sym("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into()),
        )],
    )
    .expect("runs");
    assert!(tree.is_none());
}

/// Known gap, not a passing feature: [`gix_query_eval::trace::explain`]
/// (`gix_query::explain_predicate`'s underlying implementation) hands back
/// `Ok(None)` for a bare base/EDB predicate traced directly, even when the
/// fact holds -- see `gix-query-eval/src/trace.rs`'s own doc comment on
/// `a_positive_goal_traces_to_its_base_fact`: Nemo's tracer only produces a
/// genuine node when a join forces a rule application, and a single EDB
/// predicate with no defining rule at all never does. `explain_predicate`
/// wires the CLI faithfully to this entry point rather than working around
/// its gap; this test pins the gap so a future fix changes an assertion here
/// on purpose rather than by surprise.
#[test]
fn explain_on_a_bare_edb_predicate_is_a_known_gap() {
    let (_dir, repo, first, _second) = repo();
    let tree = gix_query::explain_predicate(&repo, "commit", &[(0, Value::Sym(first.into()))])
        .expect("runs");
    assert!(
        tree.is_none(),
        "if this now traces, `gix-query-eval` grew base-predicate support -- \
         update this test and its doc comment together"
    );
}

/// Known gap, not a passing feature: every predicate a rule module defines
/// is qualified with its module name (`gix_query_rules::assemble`'s
/// `qualify`), and [`gix_query_eval::trace::explain`] builds its Nemo fact
/// text from the predicate name verbatim (`engine::trace_fact`), which Nemo's
/// parser rejects for a name containing `.`. In practice this means
/// `explain_predicate` cannot reach any predicate defined by a rule module --
/// only bare host predicates, which hit the gap pinned above instead. Fixing
/// either requires a change inside `gix-query-eval`, out of this pass's
/// scope; this test documents the failure mode rather than papering over it.
#[test]
fn explain_on_a_qualified_rule_module_predicate_is_a_known_gap() {
    let (_dir, repo, first, second) = repo();
    let store = RuleStore::open(&repo).expect("open store");
    store
        .put(
            "ancestry2",
            "pub ancestor(X, Y).\n\
             ancestor(X, Y) :- parent(X, Y).\n\
             ancestor(X, Y) :- parent(X, Z), ancestor(Z, Y).\n",
        )
        .expect("put module");
    let err = gix_query::explain_predicate(
        &repo,
        "ancestry2.ancestor",
        &[
            (0, Value::Sym(second.into())),
            (1, Value::Sym(first.into())),
        ],
    )
    .unwrap_err();
    assert!(
        matches!(err, gix_query::QueryError::Eval(_)),
        "if this now succeeds, `gix-query-eval` grew qualified-name support -- \
         update this test and its doc comment together: {err:?}"
    );
}

#[test]
fn explain_requires_every_position_bound() {
    let (_dir, repo, second, ..) = repo();
    let err = gix_query::explain_predicate(&repo, "parent", &[(0, Value::Sym(second.into()))])
        .unwrap_err();
    assert!(matches!(
        err,
        gix_query::QueryError::ExplainRequiresGroundTuple { .. }
    ));
}

/// `run_body_as_of` (`gix-effect`'s replacement for rewinding a real ref to
/// answer "as it was before this ref moved") answers the *overridden* ref
/// state, not the live one -- and the live repository is unaffected by
/// having been read through the override, so a plain `run_body` right after
/// still reports the real value. `member/1` is ref-glob-backed
/// (`refs/meta/members/*`), so overriding its one ref to "did not exist" is
/// enough to observe the whole seam: [`gix_query_host::RepoFacts::as_of`] to
/// [`gix_refstore::AsOfRefStore`] and back.
#[test]
fn run_body_as_of_answers_the_overridden_ref_state_while_the_live_repo_sees_the_real_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    let member = f.hash_object("Ada Lovelace <ada@example.com>");
    let member_ref = format!("refs/meta/members/{member}");
    let target = f.commit_file("members.txt", "Ada\n", "add Ada");
    f.update_ref(&member_ref, &target);
    let repo = f.open();

    // Live: the member ref exists, so `member(M)` derives one row.
    let live = gix_query::run_body(&repo, "member(M)").expect("runs live");
    assert_eq!(live, vec![vec![Value::sym(member.as_str())]]);

    // As of "this ref did not exist": the same repository, minus one ref --
    // the "before" half of a member-ref-creation trigger.
    let name = gix_query::RefName::new(member_ref.as_str()).expect("valid ref name");
    let as_of = gix_query::run_body_as_of(&repo, "member(M)", [(name, None)]).expect("runs as-of");
    assert!(
        as_of.is_empty(),
        "the override makes the member ref answer as though it never existed"
    );

    // Reading through the override must not have written anything: the live
    // repository still reports the real, current value.
    let live_again = gix_query::run_body(&repo, "member(M)").expect("runs live again");
    assert_eq!(
        live_again, live,
        "run_body_as_of must not have changed what the live repository reports"
    );
}
