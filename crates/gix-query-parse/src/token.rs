//! The lexical token alphabet.
//!
//! Small enough to stay in one file: the grammar has one string form, one
//! variable form, one bare-constant form, and a handful of punctuation.
//! Everything else -- qualified names, `pub`, negative-vs-comparison
//! disambiguation -- is resolved by the parser from this flat stream plus
//! span adjacency, not by a richer token alphabet.

use std::sync::Arc;

use gix_query_ir::Span;

/// One lexical token, with the span it was read from.
#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// A token's syntactic class.
///
/// Variable-vs-constant-vs-anonymous classification happens here, in the
/// lexer, because the character class alone determines it -- there is no
/// context in which `X` could mean anything but a variable. Predicate-name
/// qualification (`review.reviewed`) is the one classification the lexer
/// cannot make: it is a decision that depends on the *position* a bare word
/// occupies, so it lives in the parser, keyed off span adjacency between a
/// `Sym`, a `Dot`, and the `Sym` that follows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    /// `[A-Z][A-Za-z0-9_]*` or `_[A-Za-z0-9_]+`.
    Var(Arc<str>),
    /// Bare `_`.
    Anon,
    /// A bare constant: `[a-z][A-Za-z0-9_]*`, or a digit-led word that is not
    /// all digits (an unquoted object id such as `9a3f21c4`).
    Sym(Arc<str>),
    /// A bare, all-digit word, with an optional leading `-`.
    Int(i64),
    /// A quoted constant's already-unescaped content.
    Str(Arc<str>),
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `:-`
    ColonDash,
    /// `!`
    Bang,
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// The end of the source text.
    Eof,
}

impl TokenKind {
    /// A short, human-readable name for diagnostics.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Var(s) => format!("variable '{s}'"),
            Self::Anon => "'_'".to_owned(),
            Self::Sym(s) => format!("'{s}'"),
            Self::Int(n) => format!("integer '{n}'"),
            Self::Str(s) => format!("string {s:?}"),
            Self::LParen => "'('".to_owned(),
            Self::RParen => "')'".to_owned(),
            Self::Comma => "','".to_owned(),
            Self::Dot => "'.'".to_owned(),
            Self::ColonDash => "':-'".to_owned(),
            Self::Bang => "'!'".to_owned(),
            Self::Eq => "'='".to_owned(),
            Self::Ne => "'!='".to_owned(),
            Self::Lt => "'<'".to_owned(),
            Self::Le => "'<='".to_owned(),
            Self::Gt => "'>'".to_owned(),
            Self::Ge => "'>='".to_owned(),
            Self::Eof => "end of input".to_owned(),
        }
    }
}
