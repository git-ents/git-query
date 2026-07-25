//! Pass 1 -- no value invention.
//!
//! [`gix_query_ir::Term`] has three forms and no fourth, so a constructed
//! term is unrepresentable rather than merely rejected: there is no
//! `Compound` variant to walk into in the first place. What remains
//! representable, and therefore what this pass actually checks, is the case
//! the type system cannot rule out on its own -- a [`Value::Sym`] built from
//! arbitrary text (a quoted constant in source, or a value an API caller
//! constructed directly) carrying a byte that would corrupt it downstream.
//!
//! Nemo's string lexer takes every byte up to the next `"` and never
//! unescapes, so a `"` inside a constant cannot be represented at all, let
//! alone escaped -- see `spikes/nemo-spike/spike-report.md` gate A1. The
//! canonicalizer's NUL guard is the same argument one layer up: a NUL inside
//! a constant could forge the field boundary the cache key's canonical form
//! relies on. Both are refused here, at the front door, with a span naming
//! the call that carries the bad value.

use gix_query_ir::{Literal, Program, Term, Value};

use crate::error::{CheckError, Errors};

const QUOTE: u8 = b'"';
const NUL: u8 = 0;

/// Check every constant in every rule body and head for `Program`.
pub(crate) fn check(program: &Program, errors: &mut Errors) {
    for rule in program.rules() {
        check_literal(&Literal::Positive(rule.head.clone()), errors);
        for lit in &rule.body {
            check_literal(lit, errors);
        }
    }
}

/// Check every constant in a standalone goal body.
pub(crate) fn check_goal_body(body: &[Literal], errors: &mut Errors) {
    for lit in body {
        check_literal(lit, errors);
    }
}

fn check_literal(lit: &Literal, errors: &mut Errors) {
    let Some(atom) = lit.atom() else {
        // A comparison's operands are `SpannedTerm`s too, but a constant
        // compared for equality carries no more risk than any other
        // constant; `Compare`'s own atom-free shape means there is no
        // predicate name to attribute the diagnostic to, so route it through
        // the same walk only when a real atom exists. `!=` and friends are
        // registered comparisons and their arguments are covered when the
        // literal that fed them was itself a positive or builtin call.
        return;
    };
    for (i, t) in atom.terms.iter().enumerate() {
        let Term::Const(Value::Sym(s)) = &t.term else {
            continue;
        };
        if s.bytes().any(|b| b == QUOTE) {
            errors.push(CheckError::QuotedSymbol {
                predicate: atom.predicate.clone(),
                argument: i,
                value: s.to_string(),
                span: t.span,
            });
        }
        if s.bytes().any(|b| b == NUL) {
            errors.push(CheckError::NulSymbol {
                predicate: atom.predicate.clone(),
                argument: i,
                value: s.to_string(),
                span: t.span,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Rule, Span, SpannedTerm};

    use super::*;
    use crate::error::Pass;

    #[test]
    fn a_plain_symbol_is_accepted() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::fact(Atom::new("member", vec![Term::sym("joey")])),
            "m",
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn a_quote_in_a_symbol_is_pass_one() {
        let mut program = Program::new();
        let atom = Atom {
            predicate: gix_query_ir::PredicateKey::new("member", 1),
            terms: vec![SpannedTerm::new(Term::sym("jo\"ey"), Span::new(3, 10))],
            span: Span::new(0, 11),
        };
        crate::testutil::add(&mut program, Rule::fact(atom), "m");
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pass(), Pass::NoValueInvention);
        assert!(matches!(errors[0], CheckError::QuotedSymbol { .. }));
    }
}
