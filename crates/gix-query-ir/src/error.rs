use crate::{PredicateKey, Span};

/// A malformed-IR or malformed-registry failure.
///
/// These are construction errors, not validation errors: the nine validation
/// passes live in the checking crate and report against source spans. Anything
/// here means a producer built IR the rest of the system cannot interpret.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An entry's argument count does not match its predicate key.
    #[error("{key} was built with {found} arguments")]
    Arity {
        /// The predicate.
        key: PredicateKey,
        /// How many arguments were supplied.
        found: usize,
        /// Where.
        span: Span,
    },
    /// A predicate's arity exceeds what an argument set can describe.
    #[error("{key} exceeds the maximum arity of {max}")]
    ArityLimit {
        /// The predicate.
        key: PredicateKey,
        /// The limit.
        max: usize,
    },
    /// A rule module tried to define a host predicate or a builtin. Base
    /// predicates are not redefinable, and this is where that is enforced --
    /// before predicate resolution runs, since by then the host entry would
    /// already have been replaced.
    #[error("{key} is a base predicate and cannot be redefined")]
    NotRedefinable {
        /// The predicate.
        key: PredicateKey,
    },
    /// A content-addressed predicate offers an all-free mode vector.
    ///
    /// The two claims are contradictory. "Content-addressed" means the oid is
    /// already in the goal so nothing enters the cache key, and that is false
    /// of an enumerating call: reading `parent/2` all-free is the whole commit
    /// graph, whose extension moves whenever any ref moves, with nothing in
    /// the footprint to notice.
    #[error("{key} is content-addressed but offers an enumerating mode")]
    EnumerableContentAddressed {
        /// The predicate.
        key: PredicateKey,
    },
    /// A predicate name is reserved for the engine.
    #[error("{key} uses the reserved `{prefix}` prefix")]
    ReservedName {
        /// The predicate.
        key: PredicateKey,
        /// The reserved prefix.
        prefix: &'static str,
    },
    /// A value contains a byte that would forge a delimiter in the canonical
    /// form, and therefore two different queries could share a cache entry.
    #[error("the identifier {value:?} contains a NUL byte")]
    NulInValue {
        /// The offending identifier.
        value: String,
    },
}
