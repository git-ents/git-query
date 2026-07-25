//! Small helpers shared by [`crate::facts`] and [`crate::bind`]: hex-oid
//! parsing, a uniform way to box a backend failure, blob line counting (the
//! unit `line/2` counts in, and the unit a projection's span is expressed in
//! too), fact construction, and member-identity hashing.

use gix::bstr::{BStr, ByteSlice as _};
use gix_query_ir::{Fact, HostError, PredicateKey, Value};

/// Parse `value` as a lowercase-hex object id.
///
/// Every `Rev`-, `Oid`-, and `Member`-typed [`Value`] the registry declares
/// is, at the wire level, a [`Value::Sym`]: storage binds resolved OIDs
/// (`query-and-rules.adoc`, "Targets"), so by the time a value reaches a
/// host provider it is always hex text, never an unresolved revspec.
pub(crate) fn parse_oid(key: &PredicateKey, value: &Value) -> Result<gix::ObjectId, HostError> {
    let text = value
        .as_sym()
        .ok_or_else(|| HostError::UnsupportedMode { key: key.clone() })?;
    gix::ObjectId::from_hex(text.as_bytes()).map_err(|error| backend(key, error))
}

/// Parse `text` as a lowercase-hex object id, or `None` if it is not one.
///
/// A shared ref namespace is not a type boundary: `refs/meta/members/*` can
/// hold a hand-written ref whose leaf is a name rather than a hex id, and a
/// `Signer:` trailer can be written as a bare email rather than a resolved
/// member id. Neither is a backend failure -- both are simply bad data at a
/// boundary this crate does not control -- so a caller reading a namespace
/// that might contain either uses this rather than [`parse_oid`], and treats
/// `None` as "no such identity" rather than propagating an error that would
/// fail every query touching the relation over one bad ref or one mistyped
/// trailer.
pub(crate) fn try_parse_hex(text: &[u8]) -> Option<gix::ObjectId> {
    gix::ObjectId::from_hex(text).ok()
}

/// Box any backend failure as [`HostError::Backend`], attributed to `key`.
pub(crate) fn backend(
    key: &PredicateKey,
    error: impl std::error::Error + Send + Sync + 'static,
) -> HostError {
    HostError::Backend {
        key: key.clone(),
        source: Box::new(error),
    }
}

/// Wrap an already-boxed backend failure as [`HostError::Backend`].
///
/// `gix`'s own reference-iteration API combines loose and packed refs behind
/// one iterator and boxes its error for that reason, so its `Item` is already
/// `Result<_, Box<dyn Error + Send + Sync>>` rather than a concrete error
/// type -- [`backend`] cannot take it, because a trait object does not itself
/// implement [`std::error::Error`].
pub(crate) fn backend_boxed(
    key: &PredicateKey,
    source: Box<dyn std::error::Error + Send + Sync>,
) -> HostError {
    HostError::Backend {
        key: key.clone(),
        source,
    }
}

/// Box a plain message as [`HostError::Backend`], for a failure with no
/// underlying [`std::error::Error`] value -- a tree missing an expected
/// entry, say.
pub(crate) fn backend_msg(key: &PredicateKey, message: impl Into<String>) -> HostError {
    let message: String = message.into();
    HostError::Backend {
        key: key.clone(),
        source: message.into(),
    }
}

/// The number of 1-based lines in `blob` -- [`gix_anchor`]'s own
/// line-counting convention (`lines_with_terminator`): a trailing newline
/// does not add an empty final line, and a final line missing one still
/// counts as a line.
///
/// The blob is expected to exist (a caller that already resolved it from a
/// tree, or is about to reject the call otherwise); a missing blob is
/// reported as [`HostError::Backend`] here rather than silently answering
/// zero, because every call site already knows the blob should be present.
pub(crate) fn count_lines(
    repo: &gix::Repository,
    key: &PredicateKey,
    blob: gix::ObjectId,
) -> Result<u64, HostError> {
    let data = repo
        .find_blob(blob)
        .map_err(|error| backend(key, error))?
        .take_data();
    Ok(u64::try_from(data.lines_with_terminator().count()).unwrap_or(u64::MAX))
}

/// `oid`, rendered as the [`Value`] every EDB provider hands back for an
/// `Oid`-, `Rev`-, or `Member`-typed argument.
///
/// Every provider constructs this from an object id it just resolved rather
/// than echoing the caller's own bound term back verbatim, so a query never
/// observes a case difference between what it supplied and what came back --
/// `gix::ObjectId`'s `Display` is the single source of truth for spelling.
pub(crate) fn sym_oid(oid: gix::ObjectId) -> Value {
    Value::sym(oid.to_string())
}

/// A [`Fact`], attributed to `source_ref` only when `attribute` asked for it.
///
/// The gate path always calls with `attribute: false`; building the
/// attributed variant unconditionally would allocate a ref-name string on
/// every row of every gate query for a field nothing reads.
pub(crate) fn fact(values: Vec<Value>, source_ref: Option<&str>, attribute: bool) -> Fact {
    match (attribute, source_ref) {
        (true, Some(r)) => Fact::attributed(values, r.to_owned()),
        _ => Fact::new(values),
    }
}

/// The member identity for a git actor: the object id a blob containing
/// `"<name> <email>"` would hash to, computed without writing anything to the
/// object database.
///
/// [`parse_oid`]'s doc comment already commits `Member`-typed values to the
/// same hex-text wire shape as `Rev` and `Oid`; a raw `name <email>` pair
/// cannot be that shape, so every host predicate that produces or consumes a
/// member identity (`author/2`, and the `refs/meta/members/*` layout
/// `member/1` and `revoked/1` read) needs one deterministic mapping from an
/// actor to a hex id. Hashing under git's own blob framing rather than a bare
/// digest means `git hash-object` reproduces the same id for anyone
/// registering a member ref by hand.
pub(crate) fn member_id(
    key: &PredicateKey,
    name: &BStr,
    email: &BStr,
) -> Result<gix::ObjectId, HostError> {
    let mut canonical = Vec::with_capacity(name.len() + email.len() + 3);
    canonical.extend_from_slice(name.trim());
    canonical.extend_from_slice(b" <");
    canonical.extend_from_slice(email.trim());
    canonical.push(b'>');
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, &canonical)
        .map_err(|error| backend(key, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_id_matches_hash_object_of_the_canonical_identity_string() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = test_support::Fixture::init(dir.path().join("r"));
        let expected = f.hash_object("Test Author <author@example.com>");
        let key = PredicateKey::new("author", 2);
        let id =
            member_id(&key, "Test Author".into(), "author@example.com".into()).expect("member id");
        assert_eq!(id.to_string(), expected);
    }

    #[test]
    fn member_id_trims_surrounding_whitespace() {
        let key = PredicateKey::new("author", 2);
        let padded = member_id(
            &key,
            "  Test Author  ".into(),
            "  author@example.com ".into(),
        )
        .expect("member id");
        let bare =
            member_id(&key, "Test Author".into(), "author@example.com".into()).expect("member id");
        assert_eq!(padded, bare);
    }

    /// `count_lines`'s convention -- a final line missing a trailing
    /// newline still counts -- pinned against git's own blob bytes rather
    /// than trusted from the doc comment: `git cat-file -p` returns the
    /// object verbatim, so counting newlines in *that* output and adding one
    /// for an unterminated final line is an independent recomputation of the
    /// same convention over the same bytes git itself stored.
    #[test]
    fn count_lines_matches_a_manual_count_of_gits_own_bytes_for_an_unterminated_final_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = test_support::Fixture::init(dir.path().join("r"));
        let oid = f.hash_object("a\nb\nc");
        let repo = f.open();
        let key = PredicateKey::new("line", 2);
        let blob = gix::ObjectId::from_hex(oid.as_bytes()).expect("valid oid");

        let got = count_lines(&repo, &key, blob).expect("count");

        let raw = std::process::Command::new("git")
            .arg("-C")
            .arg(f.root())
            .args(["cat-file", "-p", &oid])
            .output()
            .expect("run git cat-file")
            .stdout;
        let newlines = raw.iter().filter(|&&b| b == b'\n').count();
        let unterminated_final_line = !raw.is_empty() && raw.last() != Some(&b'\n');
        let expected = newlines + usize::from(unterminated_final_line);

        assert_eq!(got, expected as u64);
        assert_eq!(
            got, 3,
            "a, b, c -- the unterminated final line still counts"
        );
    }

    #[test]
    fn count_lines_is_zero_for_an_empty_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = test_support::Fixture::init(dir.path().join("r"));
        let oid = f.hash_object("");
        let repo = f.open();
        let key = PredicateKey::new("line", 2);
        let blob = gix::ObjectId::from_hex(oid.as_bytes()).expect("valid oid");

        let got = count_lines(&repo, &key, blob).expect("count");
        assert_eq!(got, 0);
    }
}
