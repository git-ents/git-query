//! The seam a downstream consumer (`git-forge`) uses to contribute its own
//! base facts: a [`FactSource`] answering for predicates it registers into
//! its own clone of [`HostRegistry::host`] via [`PredicateEntry::edb`].
//! `run_goal_with`/`run_predicate_with` layer that source over a fresh
//! `RepoFacts` internally ([`git_query::Layered`]), so a goal may join a
//! consumer-contributed predicate against a built-in one in the same rule
//! body without the consumer's own [`FactSource`] ever answering for
//! `commit/1` itself. Depends on nothing but `git_query`, the way
//! `git-forge` would.
//!
//! Nemo's `execute()` enters a process-global timing mutex; run this file
//! with `--test-threads=1`.

use git_query::{
    ArgSet, Backing, Bindings, Fact, FactSource, Facts, HostError, HostRegistry, PredicateEntry,
    PredicateKey, QueryError, Value, ValueType,
};
use test_support::Fixture;

/// A consumer's own `FactSource`, answering for one made-up EDB predicate --
/// `flagged/1` -- and nothing else. It has no notion of `commit/1` or
/// `RepoFacts` at all; the `_with` entry points supply those.
struct ForgeFacts {
    flagged: Vec<Value>,
}

impl FactSource for ForgeFacts {
    fn provides(&self, key: &PredicateKey) -> bool {
        key.name.as_ref() == "flagged" && key.arity == 1
    }

    fn facts<'a>(
        &'a self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        _attribute: bool,
    ) -> Result<Facts<'a>, HostError> {
        if !self.provides(key) {
            return Err(HostError::NoProvider(key.clone()));
        }
        let want = bound.first().and_then(Option::as_ref).cloned();
        let rows: Vec<_> = self
            .flagged
            .iter()
            .filter(move |v| want.as_ref().is_none_or(|w| *v == w))
            .cloned()
            .map(|v| Ok(Fact::new(vec![v])))
            .collect();
        Ok(Box::new(rows.into_iter()))
    }
}

/// [`HostRegistry::host`] plus one consumer-contributed EDB entry --
/// `flagged/1`, ref-glob-backed under a made-up forge namespace, mirroring
/// how `git-forge` would declare `cst_node/4` or `reviewed/1`
/// (ARCHITECTURE.md's "extension-contributed predicates").
fn registry_with_flagged() -> HostRegistry {
    let mut registry = HostRegistry::host();
    registry
        .insert_predicate(PredicateEntry::edb(
            "flagged",
            &[ValueType::Rev],
            ArgSet::empty(),
            vec![Backing::RefGlob("refs/forge/flag/*".into())],
            "test-only: a commit forge flagged",
        ))
        .expect("flagged/1 is not a host predicate");
    registry
}

#[test]
fn run_goal_with_joins_a_consumer_predicate_against_a_built_in_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    let first = f.commit("first");
    let _second = f.commit("second");
    let repo = f.open();

    let registry = registry_with_flagged();
    let facts = ForgeFacts {
        flagged: vec![Value::sym(first.as_str())],
    };

    let rows = git_query::run_goal_with(&repo, &registry, &facts, "flagged(X), commit(X)", &["X"])
        .expect("runs");

    assert_eq!(
        rows,
        vec![vec![Value::sym(first.as_str())]],
        "the join sees `first` through both the consumer source (`flagged`) \
         and the built-in one (`commit`), and only `first` -- `second` is \
         never flagged"
    );
}

#[test]
fn run_predicate_with_answers_a_bare_consumer_predicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    let first = f.commit("first");
    let repo = f.open();

    let registry = registry_with_flagged();
    let facts = ForgeFacts {
        flagged: vec![Value::sym(first.as_str())],
    };

    let rows =
        git_query::run_predicate_with(&repo, &registry, &facts, "flagged", &[]).expect("runs");
    assert_eq!(rows, vec![vec![Value::sym(first.as_str())]]);
}

/// A predicate `registry` declares but that neither the consumer's own
/// source nor the layered-in `RepoFacts` answers for surfaces as an error
/// from evaluation, never silently as "no rows" -- the failure mode
/// `run_goal_with`'s own doc comment names.
#[test]
fn a_predicate_the_registry_declares_but_no_source_answers_is_a_host_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let f = Fixture::init(dir.path().join("r"));
    f.commit("first");
    let repo = f.open();

    let mut registry = HostRegistry::host();
    registry
        .insert_predicate(PredicateEntry::edb(
            "unanswered",
            &[ValueType::Rev],
            ArgSet::empty(),
            vec![Backing::AdHoc],
            "test-only: declared but implemented by no source",
        ))
        .expect("register unanswered/1");

    let facts = ForgeFacts {
        flagged: Vec::new(),
    };
    let err =
        git_query::run_predicate_with(&repo, &registry, &facts, "unanswered", &[]).unwrap_err();
    assert!(
        matches!(err, QueryError::Eval(_)),
        "expected a host/eval error, got {err:?}"
    );
}
