//! The one validated string type this crate emits into Nemo program text.
//!
//! Nemo's string literal lexer is `is_not("\"")`: it consumes every byte up to
//! the next `"` and unescapes nothing. That leaves exactly two sound choices
//! for a `"` in a symbol -- refuse it, or refuse it later after corrupting
//! something first. [`Symbol::new`] is where the refusal lives, so nothing
//! downstream can hold a [`Symbol`] that would corrupt on the way out.

use std::fmt;
use std::sync::Arc;

use crate::error::EvalError;

/// Program text that Nemo's parser can only interpret as a single opaque
/// string literal.
///
/// Constructible only through [`Symbol::new`], which is the one place the
/// `"` refusal has to be written. A `Value::Sym` becomes a `Symbol` exactly
/// once, at the point it is about to be rendered, so there is nowhere else in
/// the crate a second copy of this check could drift out of sync with this
/// one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Symbol(Arc<str>);

impl Symbol {
    /// Validate `s` as program-text-safe.
    ///
    /// # Errors
    /// Returns [`EvalError::QuoteInSymbol`] if `s` contains a `"`.
    pub fn new(s: impl Into<Arc<str>>) -> Result<Self, EvalError> {
        let s = s.into();
        if s.contains('"') {
            return Err(EvalError::QuoteInSymbol(s.to_string()));
        }
        Ok(Self(s))
    }

    /// The validated text, unquoted.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Render as a Nemo string-literal term.
    ///
    /// Nothing here escapes anything: Nemo's lexer does no unescaping, so an
    /// emitter that escaped `\t` into two characters would corrupt the round
    /// trip silently, and nothing downstream would ever notice. The `"`
    /// refusal already happened at construction, which is what makes emitting
    /// verbatim safe here.
    #[must_use]
    pub fn to_nemo_literal(&self) -> String {
        format!("\"{}\"", self.0)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_is_refused_at_construction() {
        assert!(matches!(
            Symbol::new("has \" a quote"),
            Err(EvalError::QuoteInSymbol(_))
        ));
    }

    #[test]
    fn nothing_is_escaped_on_the_way_out() {
        // A backslash is not special to Nemo's lexer, so it must survive
        // unchanged -- escaping it would produce `\\t` where the input had
        // `\t`, and nothing on the read side would undo that.
        let s = Symbol::new("a\\tb").expect("no quote");
        assert_eq!(s.to_nemo_literal(), "\"a\\tb\"");
    }

    #[test]
    fn a_plain_symbol_round_trips_through_the_literal_form() {
        let s = Symbol::new("refs/heads/main").expect("no quote");
        assert_eq!(s.to_nemo_literal(), "\"refs/heads/main\"");
    }
}
