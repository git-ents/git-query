//! Building a [`Goal`] without going through source text.
//!
//! Tier 1 and tier 2 of the CLI never call the parser: a predicate name plus
//! flags, or a subject plus filters, are already structured data by the time
//! they reach here. But "one syntax, three tiers" has to mean that all three
//! tiers produce the *same* IR, so this module gives tier 1 and tier 2 the
//! same [`Goal`] shape tier 3 gets from [`crate::parse_goal`], instead of
//! letting each desugaring path build `Literal`s its own way and drift.

use std::sync::Arc;

use gix_query_ir::{Atom, Goal, Literal, PredicateKey, Span, SpannedTerm, Term, Value, Var};

/// Build a tier-1 goal: apply `name/arity` with `bound` values at the given
/// argument positions. Every other position becomes a fresh output column,
/// in signature order.
///
/// This is the CLI section's tier-1 contract, restated as a builder: "bound
/// args filter, free args become columns." The synthesized column variables
/// (`Col0`, `Col1`, ...) exist only to give each free position a name to
/// join and select on; the CLI's actual column headers come from the
/// registry's argument types, not from these.
///
/// # Panics
///
/// If any bound position is `>= arity`.
#[must_use]
pub fn goal_from_predicate(
    name: impl Into<Arc<str>>,
    arity: usize,
    bound: &[(usize, Value)],
) -> Goal {
    for (i, _) in bound {
        assert!(*i < arity, "bound position {i} is outside arity {arity}");
    }
    let mut terms = Vec::with_capacity(arity);
    let mut select = Vec::new();
    for i in 0..arity {
        let term = match bound.iter().find(|(pos, _)| *pos == i) {
            Some((_, v)) => Term::Const(v.clone()),
            None => {
                let v = Var::new(format!("Col{i}"));
                select.push(v.clone());
                Term::Var(v)
            }
        };
        terms.push(SpannedTerm::synthetic(term));
    }
    let atom = Atom {
        predicate: PredicateKey::new(name, arity),
        terms,
        span: Span::synthetic(),
    };
    Goal::new(vec![Literal::Positive(atom)], select)
}

/// Build a tier-2 goal: a subject variable joined against filter atoms.
///
/// `positive` are `--with` filters, applied as [`Literal::Positive`];
/// `negative` are `--without` filters, applied as [`Literal::Negated`] and
/// placed after every positive literal. That ordering is what keeps `git
/// query run <subject> --without <pred>` safe by construction: the subject
/// is bound by a positive literal (or by the tier-2 subject enumeration
/// itself) before any negated literal runs, so range restriction holds
/// without the CLI having to prove it goal by goal, per the CLI section's
/// invariant that negation may compose only over the tier-2 subject
/// variable.
///
/// The caller has already resolved each filter atom's ambient-flag
/// arguments (`--rev`, `--in`, ...) and placed `subject` at the filter's
/// subject position; this function only assembles the literals into one
/// goal body and selects the subject.
#[must_use]
pub fn goal_from_subject(
    subject: Var,
    positive: impl IntoIterator<Item = Atom>,
    negative: impl IntoIterator<Item = Atom>,
) -> Goal {
    let mut body: Vec<Literal> = positive.into_iter().map(Literal::Positive).collect();
    body.extend(negative.into_iter().map(Literal::Negated));
    Goal::new(body, vec![subject])
}
