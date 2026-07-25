use gix_query_ir::{HostError, PredicateKey};

/// Every way this crate can fail.
///
/// `nemo::error::Error` is not `Send` (its whole API is `Rc`-based), so it
/// cannot live in this enum -- it is stringified at the boundary where it is
/// caught, which is also where the process-global timing mutex means the
/// error is about to stop being reachable from another thread anyway.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A symbol was built from text containing a `"`. Nemo's string lexer is
    /// `is_not("\"")` with no escape processing, so a `"` cannot be carried in
    /// program text at all -- refusing it here is the only sound choice,
    /// because escaping it would silently corrupt on the way out (nothing
    /// unescapes).
    #[error("a symbol cannot contain a `\"`: {0:?}")]
    QuoteInSymbol(String),
    /// A value read back from Nemo landed outside our two domains
    /// (`Value::Int`, `Value::Sym`). Reachable causes: an existential head
    /// minting a null, a bare identifier landing in the `Iri` domain, a
    /// language-tagged string, a float, a boolean.
    #[error("a value in Nemo's `{domain}` domain has no representation in our fragment")]
    ValueOutOfFragment {
        /// The offending domain, rendered with `{:?}` since `ValueDomain`
        /// lives in `nemo` and this error must not.
        domain: String,
    },
    /// The emission audit -- parsing our own emitted text back through
    /// Nemo's parser and comparing predicates, arities and rule count against
    /// what we lowered -- found a mismatch.
    #[error("emission audit failed: {0}")]
    EmissionAudit(String),
    /// Nemo rejected the emitted program or a query against it.
    #[error("nemo: {0}")]
    Nemo(String),
    /// The demand loop asked for something new on every round without
    /// converging. Unlike a fuel-bounded evaluator this is not expected to be
    /// reachable from a validator-accepted program; it is here so a bug in the
    /// rewrite fails loudly instead of spinning.
    #[error("the demand loop did not converge within {0} rounds")]
    DemandDidNotConverge(usize),
    /// A round asked a builtin or a fact source for a binding pattern it does
    /// not support, or the backing failed.
    #[error(transparent)]
    Host(#[from] HostError),
    /// A rewritten program referenced a predicate the registry does not know,
    /// which means the input was not actually a `CheckedProgram` this crate
    /// produced -- an engine bug, not a user error.
    #[error("{0} is not registered")]
    UnknownPredicate(PredicateKey),
}
