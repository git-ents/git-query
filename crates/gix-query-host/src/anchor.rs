//! `anchor(A)`, backed by `refs/anchors/<id>` -- one ref per anchor,
//! pointing directly at the serialized [`Binding`] tree
//! [`Binding::serialize_into`] returns (its [`gix_anchor::CaptureHandle`]),
//! never a commit: an anchor's own identity carries no history of its own.
//!
//! Binding-keyed storage used to live in `gix_anchor::Store`; that crate now
//! owns identity and the oracle chain only (ARCHITECTURE.md, "git-anchor"),
//! so this is `git-query`'s own minimal registry -- the one place a `bind`
//! call resolves an anchor id to the [`gix_anchor::Anchor`] its oracles
//! project onto a revision. The store holds every [`Binding`] variant `git
//! anchor inject` writes; only [`Binding::Position`] is an anchor in this
//! language's sense, so this predicate (and [`lookup`]) reports/answers only
//! that subset, matching `bind`'s own refusal of anything else.

use gix::ObjectId;
use gix_anchor::{Anchor, Binding, CaptureHandle};
use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey};

use crate::util::{backend, backend_boxed, fact, parse_oid, sym_oid};

const ANCHOR_PREFIX: &str = "refs/anchors/";

fn anchor_ref(id: ObjectId) -> String {
    format!("{ANCHOR_PREFIX}{id}")
}

/// The [`Anchor`] `refs/anchors/<id>` names, or `None` when no such ref
/// exists, its target does not decode as a [`Binding`] at all, or it decodes
/// as a binding shape other than [`Binding::Position`] -- every case is
/// simply "not an anchor `bind` can project", never a backend failure, since
/// a caller may name any oid.
pub(crate) fn lookup(
    repo: &gix::Repository,
    key: &PredicateKey,
    id: ObjectId,
) -> Result<Option<Anchor>, HostError> {
    let refname = anchor_ref(id);
    let Some(mut reference) = repo
        .try_find_reference(&refname)
        .map_err(|error| backend(key, error))?
    else {
        return Ok(None);
    };
    let handle_oid = reference
        .peel_to_id()
        .map_err(|error| backend(key, error))?
        .detach();
    match Binding::deserialize(&CaptureHandle::from(handle_oid), repo) {
        Ok(Binding::Position(anchor)) => Ok(Some(anchor)),
        Ok(_) | Err(_) => Ok(None),
    }
}

pub(crate) fn facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    match bound.first() {
        Some(Some(v)) => {
            let id = parse_oid(key, v)?;
            let out: Vec<Result<Fact, HostError>> = match lookup(repo, key, id)? {
                Some(_) => vec![Ok(fact(
                    vec![sym_oid(id)],
                    Some(&anchor_ref(id)),
                    attribute,
                ))],
                None => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let platform = repo.references().map_err(|error| backend(key, error))?;
            let mut out = Vec::new();
            for reference in platform
                .prefixed(ANCHOR_PREFIX)
                .map_err(|error| backend(key, error))?
            {
                let reference = reference.map_err(|error| backend_boxed(key, error))?;
                let refname = reference.name().as_bstr().to_string();
                let Some(id) = refname
                    .strip_prefix(ANCHOR_PREFIX)
                    .and_then(|hex| ObjectId::from_hex(hex.as_bytes()).ok())
                else {
                    continue;
                };
                if lookup(repo, key, id)?.is_some() {
                    out.push(Ok(fact(vec![sym_oid(id)], Some(&refname), attribute)));
                }
            }
            out.sort_by(|a, b| {
                let (Ok(a), Ok(b)) = (a, b) else {
                    return std::cmp::Ordering::Equal;
                };
                a.values[0].cmp(&b.values[0])
            });
            Ok(Box::new(out.into_iter()))
        }
    }
}

#[cfg(test)]
pub(crate) fn attach(repo: &gix::Repository, anchor: Anchor) -> Result<ObjectId, HostError> {
    use gix::refs::transaction::PreviousValue;

    let binding = Binding::Position(anchor);
    let id = binding
        .anchor_id()
        .map_err(|error| backend(&PredicateKey::new("anchor", 1), error))?;
    let id = ObjectId::from(id);
    let handle = binding
        .serialize_into(repo)
        .map_err(|error| backend(&PredicateKey::new("anchor", 1), error))?;
    repo.reference(
        anchor_ref(id),
        ObjectId::from(handle),
        PreviousValue::Any,
        "attach anchor",
    )
    .map_err(|error| backend(&PredicateKey::new("anchor", 1), error))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_query_ir::Value;
    use test_support::Fixture;

    #[test]
    fn anchor_enumerates_only_position_bindings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "one\ntwo\nthree\n", "first");
        let repo = f.open();

        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "a.txt",
            Some(gix_anchor::LineRange { start: 1, end: 2 }),
        )
        .expect("capture");
        let position_id = attach(&repo, anchor).expect("attach position");

        let commit_binding = Binding::Commit {
            identity: gix_anchor::CommitIdentity {
                commit: repo.head_id().expect("head").detach().into(),
            },
            hints: gix_anchor::NoHints {},
        };
        let commit_handle = commit_binding.serialize_into(&repo).expect("serialize");
        let commit_id = ObjectId::from(commit_binding.anchor_id().expect("anchor_id"));
        repo.reference(
            anchor_ref(commit_id),
            ObjectId::from(commit_handle),
            gix::refs::transaction::PreviousValue::Any,
            "attach commit binding",
        )
        .expect("write ref");

        let key = PredicateKey::new("anchor", 1);
        let out: Vec<_> = facts(&repo, &key, &[None], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].values[0], Value::sym(position_id.to_string()));

        let out = facts(
            &repo,
            &key,
            &[Some(Value::sym(commit_id.to_string()))],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert!(out.is_empty(), "a commit binding is not an anchor");

        let out = facts(&repo, &key, &[Some(Value::sym("0".repeat(40)))], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert!(out.is_empty(), "an unregistered oid is not an anchor");
    }
}
