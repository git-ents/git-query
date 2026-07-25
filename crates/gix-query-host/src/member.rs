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

use crate::util::{backend, backend_boxed, backend_msg, fact, sym_oid, try_parse_hex};

const MEMBERS_PREFIX: &str = "refs/meta/members/";

/// A member ref's standing: active or revoked. Absence is `None` at the call
/// site (`status` returns `Option<MemberStatus>`), not a third variant here --
/// "no such ref" and "this ref names a standing" are different kinds of
/// answer, and collapsing them into one three-way enum would put "absent"
/// next to two states it is not a peer of. What this type removes is the
/// `bool` it replaces: a plain `true`/`false` return required remembering,
/// at every call site, which way the flag ran; `Active`/`Revoked` needs no
/// such memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MemberStatus {
    /// The ref points at a commit.
    Active,
    /// The ref points at anything else (a blob, in practice).
    Revoked,
}

impl MemberStatus {
    fn of(kind: gix::objs::Kind) -> Self {
        if kind == gix::objs::Kind::Commit {
            Self::Active
        } else {
            Self::Revoked
        }
    }
}

/// `member`'s standing, or `None` for no such member ref.
fn status(
    repo: &gix::Repository,
    key: &PredicateKey,
    member: ObjectId,
) -> Result<Option<MemberStatus>, HostError> {
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
    Ok(Some(MemberStatus::of(kind)))
}

/// Every member ref, as `(member, status, refname)`, sorted by member.
///
/// A ref whose leaf is not a hex id is a stray or hand-written ref sharing
/// this namespace with the real ones -- `test-support`'s own generic `refs()`
/// fixture writes exactly this shape -- and skipping it is what keeps one bad
/// ref from failing every query that touches `member/1` or `revoked/1`. It is
/// not this predicate's job to validate the namespace, only to read the part
/// of it that is actually a member.
fn all(
    repo: &gix::Repository,
    key: &PredicateKey,
) -> Result<Vec<(ObjectId, MemberStatus, String)>, HostError> {
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
        let Some(member) = try_parse_hex(leaf.as_bytes()) else {
            continue;
        };
        let target = reference
            .peel_to_id()
            .map_err(|error| backend(key, error))?
            .detach();
        let kind = repo
            .try_find_header(target)
            .map_err(|error| backend(key, error))?
            .ok_or_else(|| backend_msg(key, format!("{name} targets a missing object")))?
            .kind();
        out.push((member, MemberStatus::of(kind), name));
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
            // A bound value that is not hex at all -- a `Signer:` trailer
            // written as a bare email, say -- can never name a member ref;
            // "no such member" is the same honest answer a syntactically
            // valid but unregistered id gets, not a backend failure.
            let Some(member) = v.as_sym().and_then(|s| try_parse_hex(s.as_bytes())) else {
                return Ok(Box::new(std::iter::empty()));
            };
            let refname = format!("{MEMBERS_PREFIX}{member}");
            let out: Vec<Result<Fact, HostError>> = match status(repo, key, member)? {
                Some(MemberStatus::Active | MemberStatus::Revoked) => {
                    vec![Ok(fact(vec![sym_oid(member)], Some(&refname), attribute))]
                }
                None => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let out: Vec<_> = all(repo, key)?
                .into_iter()
                .map(|(member, _status, name)| {
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
            let Some(member) = v.as_sym().and_then(|s| try_parse_hex(s.as_bytes())) else {
                return Ok(Box::new(std::iter::empty()));
            };
            let refname = format!("{MEMBERS_PREFIX}{member}");
            let out: Vec<Result<Fact, HostError>> = match status(repo, key, member)? {
                Some(MemberStatus::Revoked) => {
                    vec![Ok(fact(vec![sym_oid(member)], Some(&refname), attribute))]
                }
                Some(MemberStatus::Active) | None => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let out: Vec<_> = all(repo, key)?
                .into_iter()
                .filter(|(_, status, _)| *status == MemberStatus::Revoked)
                .map(|(member, _status, name)| {
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

    /// Reproduces the crash: `refs/meta/members/joey` is the exact refname
    /// `test-support`'s own generic `refs()` fixture writes, and before this
    /// fix `all()` propagated `ObjectId::from_hex`'s `InvalidHexEncodingLength`
    /// as a `HostError::Backend` before enumerating a single real member --
    /// every query touching `member/1` or `revoked/1` died over one stray
    /// ref in a shared namespace.
    #[test]
    fn a_non_hex_ref_in_the_members_namespace_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let active = register(&f, "Alice", "alice@example.com", false);
        let head = f.commit_file("a.txt", "a\n", "first");
        f.update_ref(&format!("{MEMBERS_PREFIX}joey"), &head);
        let repo = f.open();
        let member_key = PredicateKey::new("member", 1);
        let revoked_key = PredicateKey::new("revoked", 1);

        let members: Vec<_> = member_facts(&repo, &member_key, &[None], false)
            .expect("facts must not hard-fail on a foreign ref")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok")
            .into_iter()
            .map(|f| f.values[0].clone())
            .collect();
        assert_eq!(members, vec![Value::sym(active.as_str())]);

        let revoked: Vec<_> = revoked_facts(&repo, &revoked_key, &[None], false)
            .expect("facts must not hard-fail on a foreign ref")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert!(revoked.is_empty());
    }

    /// The bound-path manifestation of the same defect: a `Signer:` trailer
    /// written as a bare email (`claim.rs`'s type-guessing lets this
    /// through) reaches `member(+M)` as a non-hex value. It must read as "no
    /// such member", not fail the query that asked.
    #[test]
    fn a_non_hex_bound_member_value_is_false_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("member", 1);

        let out = member_facts(&repo, &key, &[Some(Value::sym("alice@example.com"))], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert!(out.is_empty());
    }
}
