//! Everything that can go wrong storing, loading, or assembling rule modules.

/// A rule-module storage or assembly error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The underlying `git-store` operation failed.
    #[error(transparent)]
    Store(#[from] git_store::Error),
    /// A module name is not a valid ref segment.
    #[error(transparent)]
    InvalidName(#[from] git_store::InvalidRefName),
    /// Computing the stored record's schema failed.
    #[error(transparent)]
    Schema(#[from] facet_git_tree::SchemaError),
    /// Serializing a module record to a storable value failed.
    #[error(transparent)]
    Serialize(#[from] facet_format::SerializeError<facet_value::ToValueError>),
    /// A stored value did not deserialize back into a module record --
    /// unreachable unless the schema and the record type have drifted.
    #[error(transparent)]
    Deserialize(#[from] Box<facet_value::ValueError>),
    /// A stored module's source failed to parse.
    #[error("module {name:?} failed to parse:\n{detail}")]
    Parse {
        /// The module that failed to parse.
        name: String,
        /// The joined parse diagnostics.
        detail: String,
    },
    /// The assembled whole program failed validation.
    #[error("assembled program failed validation:\n{0}")]
    Check(String),
}

/// Join a list of diagnostics into one message, one per line.
pub(crate) fn join<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
