//! `anchor(A)`, backed by `refs/anchors/*` -- `gix_anchor::Store`'s own note
//! namespace.
//!
//! The store holds [`gix_anchor::Binding`], of which only
//! [`gix_anchor::Binding::Position`] is an anchor in this language's sense: a
//! durable, line-level identity `bind` can project. The store's other
//! binding shapes (`Commit`, `Tree`, `Delta`, `Hybrid`) are a broader
//! vocabulary `gix-anchor` supports for other consumers; this predicate
//! reports only the position-bound subset, so a query never gets an `A` that
//! `bind` cannot answer for.

use gix_anchor::{Binding, Store};
use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey};

use crate::util::{backend, fact, parse_oid, sym_oid};

/// `refs/anchors/<target>/<id>`, reconstructed from a [`gix_anchor::StoredNote`]'s
/// own fields -- the layout `gix_anchor::store`'s module doc commits to, not
/// something this crate invents.
fn refname(note: &gix_anchor::StoredNote) -> String {
    format!("refs/anchors/{}/{}", note.target, note.id)
}

pub(crate) fn facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let store = Store::open(repo);
    match bound.first() {
        Some(Some(v)) => {
            let id = parse_oid(key, v)?;
            let note = store.get(id).map_err(|error| backend(key, error))?;
            let out: Vec<Result<Fact, HostError>> = match note {
                Some(note) if matches!(note.binding, Binding::Position(_)) => {
                    let refname = refname(&note);
                    vec![Ok(fact(vec![sym_oid(id)], Some(&refname), attribute))]
                }
                _ => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let notes = store.list(None).map_err(|error| backend(key, error))?;
            let out: Vec<_> = notes
                .into_iter()
                .filter(|note| matches!(note.binding, Binding::Position(_)))
                .map(|note| {
                    let refname = refname(&note);
                    Ok(fact(vec![sym_oid(note.id)], Some(&refname), attribute))
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
        let store = Store::open(&repo);
        let position_id = store
            .attach(&Binding::Position(anchor), b"a claim body", None)
            .expect("attach position");

        let commit_binding = Binding::Commit {
            commit: repo.head_id().expect("head").detach(),
        };
        let commit_id = store
            .attach(&commit_binding, b"not an anchor", None)
            .expect("attach commit binding");

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
    }
}
