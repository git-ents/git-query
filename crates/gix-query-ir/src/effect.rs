//! Effect declarations: the minimal surface an effect needs before the rest
//! of the effect subsystem (trigger detection, executors, idempotence keys,
//! `refs/intent/*`) exists.
//!
//! `ARCHITECTURE.md`'s git-effect section says an effect "declares its full
//! write set: the ref namespaces it -- and any executor it invokes -- may
//! advance", and that fork-bomb freedom is "stratification over declared
//! read/write namespace sets ... checked as a validation pass in git-query,
//! because effects share the rule body language." [`EffectDecl`] is exactly
//! that declaration, carrying nothing else: no trigger body, no gate, no
//! `then` clause.
//!
//! Of "declared read/write namespace sets", only the write half is still
//! declared. `docs/query-and-rules.adoc`'s "Triggers" section specifies the
//! effect graph as built from *footprints*, and the pass follows the adoc: a
//! write set is a promise about what an executor may advance, which no
//! analysis can compute, while a read set is a property of a body that pass 7
//! computes exactly. See [`EffectDecl::reads`].
//!
//! A trigger body, gate, and `then` clause need the effect executor this crate
//! deliberately does not build yet.

use std::fmt;
use std::sync::Arc;

use crate::Span;

/// A ref namespace pattern: an exact refname, or a `/*`-suffixed prefix.
///
/// This is the unit [`EffectDecl::writes`] -- and the read sets the effect
/// stratification pass *computes* from footprints -- are expressed over. Comparison is prefix-aware by construction --
/// `refs/claims/action/kiln/*` overlaps `refs/claims/*` -- rather than a
/// string-equality shortcut a caller could forget to widen.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Namespace(Arc<str>);

impl Namespace {
    /// Wrap a namespace pattern verbatim; no normalization, so what a
    /// declaration writes is what a diagnostic later names.
    #[must_use]
    pub fn new(pattern: impl Into<Arc<str>>) -> Self {
        Self(pattern.into())
    }

    /// The pattern text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The prefix a `/*`-suffixed pattern denotes, or `None` for an exact
    /// refname with no wildcard.
    fn prefix(&self) -> Option<&str> {
        self.0.strip_suffix("/*")
    }

    /// Whether `self` and `other` denote overlapping ref sets.
    ///
    /// Two exact refnames overlap only if identical. An exact refname
    /// overlaps a prefix pattern exactly when it falls under that prefix.
    /// Two prefix patterns overlap when either is nested inside the other --
    /// nesting in either direction, since a namespace declaration does not
    /// say which of two prefixes is meant to be the wider one.
    #[must_use]
    pub fn overlaps(&self, other: &Namespace) -> bool {
        match (self.prefix(), other.prefix()) {
            (None, None) => self.0 == other.0,
            // An exact ref falls under a prefix pattern only as a strict
            // descendant: the bare prefix path is not itself a ref the
            // pattern denotes.
            (Some(p), None) => under(p, &other.0),
            (None, Some(p)) => under(p, &self.0),
            // Two prefix patterns overlap when nested in either direction --
            // including the degenerate case of identical prefixes.
            (Some(a), Some(b)) => a == b || under(a, b) || under(b, a),
        }
    }
}

/// Whether `path` is a strict descendant of `prefix` (`prefix` itself does
/// not count -- see [`Namespace::overlaps`]'s exact-vs-prefix case).
fn under(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/'))
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The declared read and write namespace sets for one effect.
///
/// Nothing here is executable: no trigger predicate, no gate, no `then`.
/// Those belong to the effect executor
/// (`ARCHITECTURE.md`'s "Charter, exhaustively"), which does not exist in
/// this crate. What does exist is enough for the stratification pass --
/// [`Namespace`] overlap between one effect's declared writes and the read
/// set computed from another's body footprint is the edge the fork-bomb check
/// walks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDecl {
    /// The effect's name, unique within the program.
    pub name: Arc<str>,
    /// Namespaces this effect's trigger *declares* it reads.
    ///
    /// Not consulted by anything. Reads are computed, not declared: the
    /// effect stratification pass derives an effect's read set from its
    /// body's footprint, because a declaration can claim to read nothing
    /// while its trigger joins on every claim in the repository. This field
    /// and the parser's `reads(...)` clause survive only so that existing
    /// modules keep parsing; removing both is item D2 of
    /// `DEVPLAN-effect.md`, and that is the ticket to close, not this
    /// comment.
    pub reads: Vec<Namespace>,
    /// Namespaces this effect -- or any executor it invokes -- may advance.
    pub writes: Vec<Namespace>,
    /// Where it was declared.
    pub span: Span,
}

impl EffectDecl {
    /// A declaration with no source location, for programmatically built
    /// fixtures.
    #[must_use]
    pub fn new(name: impl Into<Arc<str>>, reads: Vec<Namespace>, writes: Vec<Namespace>) -> Self {
        Self {
            name: name.into(),
            reads,
            writes,
            span: Span::synthetic(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns(s: &str) -> Namespace {
        Namespace::new(s)
    }

    #[test]
    fn identical_exact_namespaces_overlap() {
        assert!(ns("refs/release/candidate").overlaps(&ns("refs/release/candidate")));
    }

    #[test]
    fn distinct_exact_namespaces_do_not_overlap() {
        assert!(!ns("refs/release/candidate").overlaps(&ns("refs/release/other")));
    }

    #[test]
    fn an_exact_ref_under_a_prefix_overlaps_it() {
        assert!(ns("refs/claims/action/kiln/main").overlaps(&ns("refs/claims/*")));
        assert!(ns("refs/claims/*").overlaps(&ns("refs/claims/action/kiln/main")));
    }

    #[test]
    fn an_exact_ref_equal_to_the_bare_prefix_does_not_overlap() {
        // `refs/claims` itself is not under `refs/claims/*` -- the pattern
        // denotes children, not the prefix path as a ref in its own right.
        assert!(!ns("refs/claims").overlaps(&ns("refs/claims/*")));
    }

    #[test]
    fn a_nested_prefix_pattern_overlaps_its_ancestor() {
        assert!(ns("refs/claims/action/kiln/*").overlaps(&ns("refs/claims/*")));
        assert!(ns("refs/claims/*").overlaps(&ns("refs/claims/action/kiln/*")));
    }

    #[test]
    fn unrelated_prefixes_do_not_overlap() {
        assert!(!ns("refs/claims/*").overlaps(&ns("refs/release/*")));
    }

    #[test]
    fn a_write_with_no_reader_does_not_overlap_an_unrelated_read() {
        // The worked-example-3 shape: `refs/release/candidate` is written but
        // appears in no trigger's read set.
        assert!(!ns("refs/release/candidate").overlaps(&ns("refs/heads/main")));
    }
}
