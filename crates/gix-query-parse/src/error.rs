//! Parse diagnostics.

use gix_query_ir::{PredicateKey, Span};

/// One parse failure, located in the source text.
///
/// Message text never repeats the span; a caller already has it as a field
/// and maps it to a line and column however its output format wants. This
/// mirrors the split `gix_query_ir::Error` uses for the same reason: spans
/// are position data, not prose.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// A byte that starts no token.
    #[error("unexpected character {found:?}")]
    UnexpectedChar {
        /// The character.
        found: char,
        /// Where.
        span: Span,
    },
    /// A `"..."` with no closing quote before the source ended.
    #[error("unterminated string literal")]
    UnterminatedString {
        /// From the opening quote to the end of input.
        span: Span,
    },
    /// A `\x` escape this dialect does not define. Only `\\`, `\"`, `\n`,
    /// and `\t` are escapes; everything else, including `\0`, is rejected
    /// rather than passed through, so a typo cannot silently mean something
    /// other than what it looks like.
    #[error("unknown escape '\\{found}'")]
    InvalidEscape {
        /// The character after the backslash.
        found: char,
        /// Where.
        span: Span,
    },
    /// A literal NUL byte or a `\0` escape inside a quoted constant.
    ///
    /// The canonicalizer length-prefixes every field precisely so a NUL in
    /// an identifier cannot forge a field boundary and collide two different
    /// queries into one cache entry. Rejecting it here, at the front door,
    /// is the belt to that canonicalizer's braces.
    #[error("a NUL byte cannot appear in a quoted constant")]
    NulInString {
        /// Where.
        span: Span,
    },
    /// A bare or quoted integer literal too large for `i64`.
    #[error("{text} does not fit in a 64-bit integer")]
    IntOverflow {
        /// The offending digits.
        text: String,
        /// Where.
        span: Span,
    },
    /// A token the grammar does not allow at this position.
    #[error("expected {expected}, found {found}")]
    UnexpectedToken {
        /// What the parser was looking for.
        expected: String,
        /// What it saw instead.
        found: String,
        /// Where.
        span: Span,
    },
    /// The input ended where the grammar still expected more.
    #[error("expected {expected}, found end of input")]
    UnexpectedEof {
        /// What the parser was looking for.
        expected: String,
        /// The end-of-input position.
        span: Span,
    },
    /// A predicate defined, or referenced, under the engine-reserved
    /// `magic_` prefix.
    ///
    /// Such a module would capture an engine relation the magic-set rewrite
    /// generates and corrupt demand propagation, so this is rejected here
    /// rather than left for the validator to catch.
    #[error("{key} uses the reserved `magic_` prefix")]
    ReservedPrefix {
        /// The offending predicate.
        key: PredicateKey,
        /// Where.
        span: Span,
    },
    /// A `pub` declaration names a predicate no rule in the module defines.
    #[error("pub declares {key}, which this module does not define")]
    PubUndefined {
        /// The undeclared predicate.
        key: PredicateKey,
        /// Where the `pub` declaration was written.
        span: Span,
    },
    /// An `effect` declaration still writes a `reads(...)` clause.
    ///
    /// `DEVPLAN-effect.md` D2 retires the clause: an effect's read set is
    /// computed from its body's footprint, never declared, because a
    /// declaration can lie about what it reads while a footprint cannot. A
    /// module written before D2 must fail here, loudly and by name, rather
    /// than falling through to a generic "expected '.'" error that gives no
    /// clue what changed.
    #[error(
        "the 'reads(...)' clause is retired (DEVPLAN-effect.md D2): an effect's read set is \
         computed from its body's footprint, not declared"
    )]
    RetiredReadsClause {
        /// Where the `reads` keyword was written.
        span: Span,
    },
}

impl ParseError {
    /// Where this error was found.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::UnexpectedChar { span, .. }
            | Self::UnterminatedString { span }
            | Self::InvalidEscape { span, .. }
            | Self::NulInString { span }
            | Self::IntOverflow { span, .. }
            | Self::UnexpectedToken { span, .. }
            | Self::UnexpectedEof { span, .. }
            | Self::ReservedPrefix { span, .. }
            | Self::PubUndefined { span, .. }
            | Self::RetiredReadsClause { span } => *span,
        }
    }
}

/// The parse failures from one run.
///
/// A resynchronizing parser collects several rather than stopping at the
/// first: on a syntax error it skips to the next `.` (or, inside a goal, the
/// next `,`) and keeps going, so one run of `git query rules check` can
/// report every mistake in a module at once.
pub type Errors = Vec<ParseError>;
