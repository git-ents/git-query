//! `line(+Blob, N)`: 1-based line numbers, counting exactly as
//! [`crate::util::count_lines`] does -- the same convention `bind`'s
//! whole-file span uses, so a review anchored to a whole file and a query
//! walking `line/2` over that same blob agree on how many lines it has.

use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey, Value};

use crate::util::{count_lines, fact, parse_oid, sym_oid};

pub(crate) fn facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let Some(Some(blob_v)) = bound.first() else {
        return Err(HostError::UnsupportedMode { key: key.clone() });
    };
    let blob = parse_oid(key, blob_v)?;
    if !repo.has_object(blob) {
        return Ok(Box::new(std::iter::empty()));
    }
    let total = count_lines(repo, key, blob)?;

    let want_n = match bound.get(1) {
        Some(Some(Value::Int(n))) => Some(*n),
        Some(Some(Value::Sym(_))) => return Err(HostError::UnsupportedMode { key: key.clone() }),
        Some(None) | None => None,
    };

    let out: Vec<Result<Fact, HostError>> = match want_n {
        Some(n) => {
            let in_range = u64::try_from(n).is_ok_and(|n| n >= 1 && n <= total);
            if in_range {
                vec![Ok(fact(
                    vec![sym_oid(blob), Value::Int(n)],
                    None,
                    attribute,
                ))]
            } else {
                vec![]
            }
        }
        None => (1..=total)
            .map(|n| {
                let n = i64::try_from(n).unwrap_or(i64::MAX);
                Ok(fact(vec![sym_oid(blob), Value::Int(n)], None, attribute))
            })
            .collect(),
    };
    Ok(Box::new(out.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::Fixture;

    #[test]
    fn line_enumerates_one_based_line_numbers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "one\ntwo\nthree\n", "first");
        let blob = f.blob_at(&head, "a.txt");
        let repo = f.open();
        let key = PredicateKey::new("line", 2);

        let out: Vec<_> = facts(&repo, &key, &[Some(Value::sym(blob.as_str())), None], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        let ns: Vec<_> = out.iter().map(|f| f.values[1].as_int().unwrap()).collect();
        assert_eq!(ns, vec![1, 2, 3]);
    }

    #[test]
    fn line_all_free_on_the_blob_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "one\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("line", 2);

        let Err(err) = facts(&repo, &key, &[None, None], false) else {
            panic!("bound-only predicate must refuse an all-free call");
        };
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }

    #[test]
    fn a_missing_blob_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "one\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("line", 2);
        let missing = "0".repeat(40);

        let out = facts(&repo, &key, &[Some(Value::sym(missing)), None], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert!(out.is_empty());
    }
}
