//! `author/2`: the commit's author, mapped to a member identity through
//! [`crate::util::member_id`].
//!
//! There is no `refs/meta/members/*` lookup here -- `author/2` is
//! content-addressed on the commit alone, and whether the resulting hash
//! happens to also be a registered `member/1` is a join the rule text
//! performs, not something this predicate checks. A commit can be authored by
//! someone who never joined as a member.

use gix_query_ir::{Bindings, Facts, HostError, PredicateKey};

use crate::util::{backend, fact, member_id, parse_oid, sym_oid};

pub(crate) fn facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let Some(Some(commit_v)) = bound.first() else {
        return Err(HostError::UnsupportedMode { key: key.clone() });
    };
    let want_member = match bound.get(1) {
        Some(Some(v)) => Some(parse_oid(key, v)?),
        Some(None) | None => None,
    };
    let commit_oid = parse_oid(key, commit_v)?;
    let Ok(commit) = repo.find_commit(commit_oid) else {
        return Ok(Box::new(std::iter::empty()));
    };
    let signature = commit.author().map_err(|error| backend(key, error))?.trim();
    let member = member_id(key, signature.name, signature.email)?;
    if want_member.is_some_and(|want| want != member) {
        return Ok(Box::new(std::iter::empty()));
    }
    Ok(Box::new(std::iter::once(Ok(fact(
        vec![sym_oid(commit_oid), sym_oid(member)],
        None,
        attribute,
    )))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_query_ir::Value;
    use test_support::Fixture;

    #[test]
    fn author_hashes_the_committing_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let expected_member = f.hash_object("Test Author <author@example.com>");
        let repo = f.open();
        let key = PredicateKey::new("author", 2);

        let out: Vec<_> = facts(&repo, &key, &[Some(Value::sym(head.as_str())), None], false)
            .expect("facts")
            .collect::<Result<_, _>>()
            .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].values,
            vec![Value::sym(head), Value::sym(expected_member)]
        );
    }

    #[test]
    fn author_all_free_is_refused_rather_than_enumerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("author", 2);

        let Err(err) = facts(&repo, &key, &[None, None], false) else {
            panic!("keyed predicate must refuse a free commit");
        };
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }
}
