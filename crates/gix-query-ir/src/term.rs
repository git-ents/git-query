use std::fmt;
use std::sync::Arc;

use crate::Span;

/// A variable, written uppercase-initial in both rule files and command-line
/// goals.
///
/// There is one variable syntax across every tier. The `?b` form some query
/// languages use is deliberately absent: `?` is a shell glob character, so a
/// goal containing one breaks the moment someone forgets the quotes -- and it
/// breaks *silently* in the case where a matching file happens to exist.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(Arc<str>);

impl Var {
    /// A variable named `name`.
    ///
    /// Case is not validated here; the parser enforces the uppercase-initial
    /// rule, and the magic-set rewrite mints variables that never passed
    /// through the parser.
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self(name.into())
    }

    /// The variable's name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A ground value: an identifier or an integer, never content.
///
/// The language has no compound values. A constructed term is the thin end of
/// destructuring, which is the thin end of unification, which is the thin end
/// of function symbols -- and the whole worth of the no-value-invention pass
/// is that it is a syntactic check with no carve-outs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    /// An integer. The only type comparisons order.
    Int(i64),
    /// An identifier: an object id, a refname component, a path, a member
    /// name, or a bare keyword such as `approve`.
    Sym(Arc<str>),
}

impl Value {
    /// An identifier value.
    pub fn sym(s: impl Into<Arc<str>>) -> Self {
        Self::Sym(s.into())
    }

    /// The identifier text, or `None` for an integer.
    #[must_use]
    pub fn as_sym(&self) -> Option<&str> {
        match self {
            Self::Sym(s) => Some(s),
            Self::Int(_) => None,
        }
    }

    /// The integer, or `None` for an identifier.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Sym(_) => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(n) => write!(f, "{n}"),
            Self::Sym(s) => f.write_str(s),
        }
    }
}

/// An argument position in an atom.
///
/// Three forms and no fourth. There is deliberately no `Compound` variant:
/// adding one is how a Datalog stops being a Datalog.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Term {
    /// A named variable.
    Var(Var),
    /// A ground value.
    Const(Value),
    /// `_` -- a variable guaranteed to occur exactly once, so it never joins
    /// and never needs a name.
    Anon,
}

impl Term {
    /// A variable term.
    pub fn var(name: impl Into<Arc<str>>) -> Self {
        Self::Var(Var::new(name))
    }

    /// An identifier constant.
    pub fn sym(s: impl Into<Arc<str>>) -> Self {
        Self::Const(Value::sym(s))
    }

    /// An integer constant.
    #[must_use]
    pub fn int(n: i64) -> Self {
        Self::Const(Value::Int(n))
    }

    /// The variable this term names, if it names one. `_` names none: it is
    /// unjoinable by construction, which is why it can be skipped by range
    /// restriction rather than exempted from it.
    #[must_use]
    pub fn as_var(&self) -> Option<&Var> {
        match self {
            Self::Var(v) => Some(v),
            Self::Const(_) | Self::Anon => None,
        }
    }

    /// Whether this term is ground.
    #[must_use]
    pub fn is_const(&self) -> bool {
        matches!(self, Self::Const(_))
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Var(v) => write!(f, "{v}"),
            Self::Const(c) => write!(f, "{c}"),
            Self::Anon => f.write_str("_"),
        }
    }
}

/// A term paired with the source text it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpannedTerm {
    /// The term.
    pub term: Term,
    /// Where it was written.
    pub span: Span,
}

impl SpannedTerm {
    /// Pair `term` with `span`.
    #[must_use]
    pub const fn new(term: Term, span: Span) -> Self {
        Self { term, span }
    }

    /// Pair `term` with no source location.
    #[must_use]
    pub const fn synthetic(term: Term) -> Self {
        Self {
            term,
            span: Span::synthetic(),
        }
    }
}
