//! `member/1` and `revoked/1`, both backed by `refs/meta/members/*`.
//!
//! One ref per member, `refs/meta/members/<member-hex>`. Its *target's
//! object kind* is the revocation flag: a ref pointing at a commit names an
//! active member, a ref pointing at anything else (a blob, in practice) names
//! a revoked one. `member/1` is the superset over both, matching the
//! reference vocabulary's `active_member(M) :- member(M), !revoked(M).` --
//! that rule only makes sense if `member` still holds for a revoked identity.

use gix::ObjectId;
use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey};

use crate::util::{backend, backend_boxed, backend_msg, fact, parse_oid, sym_oid};

const MEMBERS_PREFIX: &str = "refs/meta/members/";

/// `Some(true)` revoked, `Some(false)` active, `None` no such member ref.
fn status(
    repo: &gix::Repository,
    key: &PredicateKey,
    member: ObjectId,
) -> Result<Option<bool>, HostError> {
    let refname = format!("{MEMBERS_PREFIX}{member}");
    let Some(mut reference) = repo
        .try_find_reference(&refname)
        .map_err(|error| backend(key, error))?
    else {
        return Ok(None);
    };
    let target = reference
        .peel_to_id()
        .map_err(|error| backend(key, error))?
        .detach();
    let kind = repo
        .try_find_header(target)
        .map_err(|error| backend(key, error))?
        .ok_or_else(|| backend_msg(key, format!("{refname} targets a missing object")))?
        .kind();
    Ok(Some(kind != gix::objs::Kind::Commit))
}

/// Every member ref, as `(member, revoked, refname)`, sorted by member.
fn all(
    repo: &gix::Repository,
    key: &PredicateKey,
) -> Result<Vec<(ObjectId, bool, String)>, HostError> {
    let platform = repo.references().map_err(|error| backend(key, error))?;
    let mut out = Vec::new();
    for reference in platform
        .prefixed(MEMBERS_PREFIX)
        .map_err(|error| backend(key, error))?
    {
        let mut reference = reference.map_err(|error| backend_boxed(key, error))?;
        let name = reference.name().as_bstr().to_string();
        let leaf = name
            .strip_prefix(MEMBERS_PREFIX)
            .ok_or_else(|| backend_msg(key, format!("{name} is outside its own prefix")))?;
        let member = ObjectId::from_hex(leaf.as_bytes()).map_err(|error| backend(key, error))?;
        let target = reference
            .peel_to_id()
            .map_err(|error| backend(key, error))?
            .detach();
        let kind = repo
            .try_find_header(target)
            .map_err(|error| backend(key, error))?
            .ok_or_else(|| backend_msg(key, format!("{name} targets a missing object")))?
            .kind();
        out.push((member, kind != gix::objs::Kind::Commit, name));
    }
    out.sort();
    Ok(out)
}

pub(crate) fn member_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    match bound.first() {
        Some(Some(v)) => {
            let member = parse_oid(key, v)?;
            let refname = format!("{MEMBERS_PREFIX}{member}");
            let out: Vec<Result<Fact, HostError>> = match status(repo, key, member)? {
                Some(_) => vec![Ok(fact(vec![sym_oid(member)], Some(&refname), attribute))],
                None => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let out: Vec<_> = all(repo, key)?
                .into_iter()
                .map(|(member, _revoked, name)| {
                    Ok(fact(vec![sym_oid(member)], Some(&name), attribute))
                })
                .collect();
            Ok(Box::new(out.into_iter()))
        }
    }
}

pub(crate) fn revoked_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    match bound.first() {
        Some(Some(v)) => {
            let member = parse_oid(key, v)?;
            let refname = format!("{MEMBERS_PREFIX}{member}");
            let out: Vec<Result<Fact, HostError>> = match status(repo, key, member)? {
                Some(true) => vec![Ok(fact(vec![sym_oid(member)], Some(&refname), attribute))],
                Some(false) | None => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let out: Vec<_> = all(repo, key)?
                .into_iter()
                .filter(|(_, revoked, _)| *revoked)
                .map(|(member, _revoked, name)| {
                    Ok(fact(vec![sym_oid(member)], Some(&name), attribute))
                })
                .collect();
            Ok(Box::new(out.into_iter()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_query_ir::Value;
    use test_support::Fixture;

    fn register(f: &Fixture, name: &str, email: &str, revoked: bool) -> String {
        let member = f.hash_object(&format!("{name} <{email}>"));
        let target = if revoked {
            f.hash_object("revoked")
        } else {
            f.commit_file("members.txt", &format!("{name}\n"), &format!("add {name}"))
        };
        f.update_ref(&format!("{MEMBERS_PREFIX}{member}"), &target);
        member
    }

    #[test]
    fn member_holds_for_both_active_and_revoked_but_revoked_only_for_the_latter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let active = register(&f, "Alice", "alice@example.com", false);
        let gone = register(&f, "Bob", "bob@example.com", true);
        let repo = f.open();
        let member_key = PredicateKey::new("member", 1);
        let revoked_key = PredicateKey::new("revoked", 1);

        let members: Vec<_> = member_facts(&repo, &member_key, &[None], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok")
            .into_iter()
            .map(|f| f.values[0].clone())
            .collect();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&Value::sym(active.as_str())));
        assert!(members.contains(&Value::sym(gone.as_str())));

        let revoked: Vec<_> = revoked_facts(&repo, &revoked_key, &[None], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok")
            .into_iter()
            .map(|f| f.values[0].clone())
            .collect();
        assert_eq!(revoked, vec![Value::sym(gone.as_str())]);
    }

    #[test]
    fn unknown_member_is_false_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("member", 1);
        let stranger = f.hash_object("nobody");

        let out = member_facts(&repo, &key, &[Some(Value::sym(stranger))], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert!(out.is_empty());
    }
}
