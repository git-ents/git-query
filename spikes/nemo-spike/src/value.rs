//! The two value types our query language has, and how they survive a round
//! trip through Nemo.
//!
//! Our spec admits exactly two: `int` and `symbol`. Everything git-shaped --
//! object ids, refnames, paths, member handles -- is a symbol. Nemo's value
//! space is much larger (IRIs, language-tagged strings, floats, nulls, maps,
//! tuples), and the question gate A6 asks is whether a round trip through it
//! can hand back something outside our two.

use nemo::datavalues::{AnyDataValue, DataValue, ValueDomain};

/// A value in our fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Value {
    /// A 64-bit signed integer.
    Int(i64),
    /// An opaque symbol: an oid, a refname, a path, a member handle, or one of
    /// the spec's closed-domain atoms such as `same` or `intact`.
    Sym(String),
}

impl Value {
    /// A symbol, for terser test code.
    pub fn sym(s: impl Into<String>) -> Self {
        Self::Sym(s.into())
    }

    /// Render as a Nemo term.
    ///
    /// Symbols become double-quoted string literals, never bare names and
    /// never IRIs. A bare name in Nemo is an abstract constant that prints
    /// back as an IRI, and an IRI is a value domain we have no representation
    /// for; quoting keeps every symbol in `PlainString` where we can read it
    /// back unchanged.
    ///
    /// Nothing is escaped, because Nemo's string literal does no unescaping:
    /// its lexer takes every byte up to the next `"`. Emitting `\t` would
    /// therefore produce the two characters `\` and `t`, which is silent
    /// corruption rather than a quoting scheme. The one byte that cannot be
    /// carried is `"` itself, and that is a panic here so a lowering cannot
    /// reach it by accident.
    pub fn to_nemo(&self) -> String {
        match self {
            Self::Int(i) => i.to_string(),
            Self::Sym(s) => {
                assert!(
                    !s.contains('"'),
                    "Nemo string literals cannot carry a double quote: {s:?}"
                );
                format!("\"{s}\"")
            }
        }
    }

    /// Read back a value Nemo produced.
    ///
    /// Returns the offending [`ValueDomain`] if the value landed outside our
    /// fragment, which is the failure gate A6 is looking for: silent coercion
    /// would show up here as a domain we did not put in.
    pub fn from_nemo(value: &AnyDataValue) -> Result<Self, ValueDomain> {
        match value.value_domain() {
            ValueDomain::PlainString => Ok(Self::Sym(value.to_plain_string_unchecked())),
            ValueDomain::Int
            | ValueDomain::Long
            | ValueDomain::UnsignedInt
            | ValueDomain::UnsignedLong
            | ValueDomain::NonNegativeInt
            | ValueDomain::NonNegativeLong => {
                value.to_i64().map(Self::Int).ok_or(ValueDomain::Other)
            }
            other => Err(other),
        }
    }
}

/// A ground tuple of a named predicate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tuple(pub Vec<Value>);

impl Tuple {
    /// Build from anything that iterates values.
    pub fn new(values: impl IntoIterator<Item = Value>) -> Self {
        Self(values.into_iter().collect())
    }

    /// The tuple's arity.
    pub fn arity(&self) -> usize {
        self.0.len()
    }
}

/// Convenience: `t![Value::Int(1), Value::sym("a")]`.
#[macro_export]
macro_rules! t {
    ($($v:expr),* $(,)?) => { $crate::value::Tuple(vec![$($v),*]) };
}

/// Convenience: `s!("a", 1)` builds a tuple, promoting `&str` to symbols and
/// integer literals to ints.
#[macro_export]
macro_rules! s {
    ($($v:expr),* $(,)?) => { $crate::value::Tuple(vec![$($crate::value::IntoValue::into_value($v)),*]) };
}

/// Lets `s!` accept both `&str` and `i64` without turbofish noise.
pub trait IntoValue {
    /// Convert.
    fn into_value(self) -> Value;
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::Sym(self.to_owned())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::Sym(self)
    }
}

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::Int(self)
    }
}

impl IntoValue for i32 {
    fn into_value(self) -> Value {
        Value::Int(i64::from(self))
    }
}
