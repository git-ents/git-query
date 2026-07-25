//! Small helpers shared by [`crate::facts`] and [`crate::bind`]: hex-oid
//! parsing, a uniform way to box a backend failure, and blob line counting
//! (the unit `line/2` counts in, and the unit a projection's span is
//! expressed in too).

use gix::bstr::ByteSlice as _;
use gix_query_ir::{HostError, PredicateKey, Value};

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
