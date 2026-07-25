//! `tree_entry/3`: `Rev`'s tree, walked or looked up by path.
//!
//! Only blob entries are reported -- "the revision's tree holds this blob at
//! this path" has no reading under which a directory is a blob -- and only
//! the three mode vectors the registry declares (`bff`, `bbf`, `bbb`) are
//! legal; anything else is an engine bug in mode inference, not a call this
//! provider tries to answer.

use gix::bstr::ByteSlice as _;
use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey, Value};

use crate::util::{backend, fact, parse_oid, sym_oid};

pub(crate) fn facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let Some(Some(rev)) = bound.first() else {
        return Err(HostError::UnsupportedMode { key: key.clone() });
    };
    let rev_oid = parse_oid(key, rev)?;
    let path_bound = match bound.get(1) {
        Some(Some(v)) => Some(v),
        Some(None) | None => None,
    };
    let blob_bound = match bound.get(2) {
        Some(Some(v)) => Some(v),
        Some(None) | None => None,
    };

    let Ok(commit) = repo.find_commit(rev_oid) else {
        return Ok(Box::new(std::iter::empty()));
    };
    let tree = commit.tree().map_err(|error| backend(key, error))?;

    let out: Vec<Result<Fact, HostError>> = match (path_bound, blob_bound) {
        (None, None) => {
            let entries = tree
                .traverse()
                .breadthfirst
                .files()
                .map_err(|error| backend(key, error))?;
            entries
                .into_iter()
                .filter(|entry| entry.mode.is_blob())
                .map(|entry| {
                    let path = entry.filepath.to_str_lossy().into_owned();
                    Ok(fact(
                        vec![sym_oid(rev_oid), Value::sym(path), sym_oid(entry.oid)],
                        None,
                        attribute,
                    ))
                })
                .collect()
        }
        (Some(path_v), blob_bound) => {
            let path = path_v
                .as_sym()
                .ok_or_else(|| HostError::UnsupportedMode { key: key.clone() })?;
            match tree
                .lookup_entry_by_path(path)
                .map_err(|error| backend(key, error))?
            {
                Some(entry) if entry.mode().is_blob() => {
                    let oid = entry.object_id();
                    if let Some(want) = blob_bound {
                        let want_oid = parse_oid(key, want)?;
                        if want_oid != oid {
                            return Ok(Box::new(std::iter::empty()));
                        }
                    }
                    vec![Ok(fact(
                        vec![sym_oid(rev_oid), Value::sym(path), sym_oid(oid)],
                        None,
                        attribute,
                    ))]
                }
                _ => vec![],
            }
        }
        (None, Some(_)) => return Err(HostError::UnsupportedMode { key: key.clone() }),
    };
    Ok(Box::new(out.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::Fixture;

    #[test]
    fn all_free_walks_the_whole_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("src/lib.rs", "fn main() {}\n");
        f.write("README.md", "hi\n");
        f.git(&["add", "-A"]);
        let head = f.commit("first");
        let repo = f.open();
        let key = PredicateKey::new("tree_entry", 3);

        let mut out: Vec<_> = facts(
            &repo,
            &key,
            &[Some(Value::sym(head.as_str())), None, None],
            false,
        )
        .expect("facts")
        .collect::<Result<_, _>>()
        .expect("ok");
        out.sort_by(|a, b| a.values[1].cmp(&b.values[1]));
        let paths: Vec<_> = out
            .iter()
            .map(|f| f.values[1].as_sym().unwrap().to_owned())
            .collect();
        assert_eq!(paths, vec!["README.md", "src/lib.rs"]);
    }

    #[test]
    fn bound_path_is_a_direct_lookup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let blob = f.blob_at(&head, "a.txt");
        let repo = f.open();
        let key = PredicateKey::new("tree_entry", 3);

        let out: Vec<_> = facts(
            &repo,
            &key,
            &[
                Some(Value::sym(head.as_str())),
                Some(Value::sym("a.txt")),
                None,
            ],
            false,
        )
        .expect("facts")
        .collect::<Result<_, _>>()
        .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].values[2], Value::sym(blob));
    }

    #[test]
    fn missing_path_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("tree_entry", 3);

        let out = facts(
            &repo,
            &key,
            &[
                Some(Value::sym(head.as_str())),
                Some(Value::sym("missing.txt")),
                None,
            ],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert!(out.is_empty());
    }

    #[test]
    fn tree_entry_free_rev_is_refused_rather_than_enumerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("tree_entry", 3);

        let Err(err) = facts(&repo, &key, &[None, None, None], false) else {
            panic!("rev-keyed predicate must refuse a free Rev");
        };
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }
}
