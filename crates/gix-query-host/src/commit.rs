//! `commit/1` and `parent/2`.
//!
//! Both are `Backing::ContentAddressed` and required-bound on the commit they
//! key off: the registry's own tests pin this (`commit_is_required_bound...`,
//! `ancestry_and_content_are_keyed_on_their_subject`), so `bound.first()`
//! being unset here is unreachable except as an engine bug -- mode inference
//! is supposed to have refused the call already.

use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey};

use crate::util::{fact, parse_oid, sym_oid};

/// `commit(+Rev)`: whether `Rev` names a commit. Neither "does not exist" nor
/// "exists but is some other object kind" is a backend failure -- both are
/// simply the predicate being false, exactly as a bound EDB membership test
/// is supposed to answer.
pub(crate) fn facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let Some(Some(rev)) = bound.first() else {
        return Err(HostError::UnsupportedMode { key: key.clone() });
    };
    let oid = parse_oid(key, rev)?;
    let out: Vec<Result<Fact, HostError>> = if repo.find_commit(oid).is_ok() {
        vec![Ok(fact(vec![sym_oid(oid)], None, attribute))]
    } else {
        vec![]
    };
    Ok(Box::new(out.into_iter()))
}

/// `parent(+Child, Parent)`: `Child`'s direct parents, in the order the
/// commit object lists them.
pub(crate) fn parent_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let Some(Some(child)) = bound.first() else {
        return Err(HostError::UnsupportedMode { key: key.clone() });
    };
    let child_oid = parse_oid(key, child)?;
    let want_parent = match bound.get(1) {
        Some(Some(v)) => Some(parse_oid(key, v)?),
        Some(None) | None => None,
    };
    let Ok(commit) = repo.find_commit(child_oid) else {
        return Ok(Box::new(std::iter::empty()));
    };
    let mut out = Vec::new();
    for parent_id in commit.parent_ids() {
        let parent_id = parent_id.detach();
        if want_parent.is_some_and(|want| want != parent_id) {
            continue;
        }
        out.push(Ok(fact(
            vec![sym_oid(child_oid), sym_oid(parent_id)],
            None,
            attribute,
        )));
    }
    Ok(Box::new(out.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_query_ir::Value;
    use test_support::Fixture;

    fn bound1(v: &str) -> Vec<Option<Value>> {
        vec![Some(Value::sym(v))]
    }

    #[test]
    fn commit_holds_for_a_real_commit_and_not_for_a_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let blob = f.blob_at(&head, "a.txt");
        let repo = f.open();
        let key = PredicateKey::new("commit", 1);

        let mut out = facts(&repo, &key, &bound1(&head), false).expect("facts");
        assert_eq!(
            out.next().expect("one row").expect("ok").values,
            vec![Value::sym(head)]
        );
        assert!(out.next().is_none());

        let mut out = facts(&repo, &key, &bound1(&blob), false).expect("facts");
        assert!(out.next().is_none(), "a blob is not a commit");
    }

    #[test]
    fn commit_all_free_is_refused_rather_than_enumerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("commit", 1);

        let Err(err) = facts(&repo, &key, &[None], false) else {
            panic!("bound-only predicate must refuse an all-free call");
        };
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }

    #[test]
    fn parent_walks_one_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let first = f.commit_file("a.txt", "a\n", "first");
        let second = f.commit_file("a.txt", "b\n", "second");
        let repo = f.open();
        let key = PredicateKey::new("parent", 2);

        let out: Vec<_> = parent_facts(
            &repo,
            &key,
            &[Some(Value::sym(second.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<_, _>>()
        .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].values, vec![Value::sym(second), Value::sym(first)]);
    }
}
