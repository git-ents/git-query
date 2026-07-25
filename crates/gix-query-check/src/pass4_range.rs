//! Pass 4 -- range restriction.
//!
//! Every head variable, every variable under negation, and every variable in
//! a comparison must appear in a positive body literal. [`Literal::is_generator`]
//! already draws exactly the line this pass needs -- a positive or builtin
//! call can bind a variable it did not already know; a negated literal or a
//! comparison can only test one.

use std::collections::BTreeSet;

use gix_query_ir::{Goal, Literal, Program, RuleId, Var};

use crate::error::{CheckError, Errors, Site};

/// Check range restriction over every stored rule.
pub(crate) fn check(program: &Program, errors: &mut Errors) {
    for (i, rule) in program.rules().iter().enumerate() {
        let site = Site::Rule(RuleId(i));
        let bound = generator_vars(&rule.body);
        for v in unique(rule.head_vars()) {
            if !bound.contains(v) {
                errors.push(CheckError::UnrestrictedHead {
                    var: v.clone(),
                    site,
                    span: rule.head.span,
                });
            }
        }
        check_filters(&rule.body, &bound, site, errors);
    }
}

/// Check range restriction over a standalone goal: every selected variable,
/// and every variable under negation or in a comparison, same as a rule
/// head.
pub(crate) fn check_goal(goal: &Goal, errors: &mut Errors) {
    let bound = generator_vars(&goal.body);
    for v in unique(goal.select.iter()) {
        if !bound.contains(v) {
            errors.push(CheckError::UnrestrictedHead {
                var: v.clone(),
                site: Site::Goal,
                span: goal.span,
            });
        }
    }
    check_filters(&goal.body, &bound, Site::Goal, errors);
}

fn generator_vars(body: &[Literal]) -> BTreeSet<&Var> {
    body.iter()
        .filter(|l| l.is_generator())
        .flat_map(Literal::vars)
        .collect()
}

fn unique<'a>(vars: impl Iterator<Item = &'a Var>) -> BTreeSet<&'a Var> {
    vars.collect()
}

fn check_filters(body: &[Literal], bound: &BTreeSet<&Var>, site: Site, errors: &mut Errors) {
    for lit in body {
        match lit {
            Literal::Negated(atom) => {
                for v in unique(atom.vars()) {
                    if !bound.contains(v) {
                        errors.push(CheckError::UnrestrictedNegation {
                            var: v.clone(),
                            site,
                            span: atom.span,
                        });
                    }
                }
            }
            Literal::Compare(c) => {
                for t in [&c.left, &c.right] {
                    if let Some(v) = t.term.as_var()
                        && !bound.contains(v)
                    {
                        errors.push(CheckError::UnrestrictedCompare {
                            var: v.clone(),
                            site,
                            span: c.span,
                        });
                    }
                }
            }
            Literal::Positive(_) | Literal::Builtin(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Rule, Term};

    use super::*;
    use crate::error::Pass;

    #[test]
    fn a_head_var_bound_by_a_positive_literal_is_fine() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("x", vec![Term::var("V")]),
                vec![Literal::Positive(Atom::new("member", vec![Term::var("V")]))],
            ),
            "m",
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn an_unbound_head_var_is_pass_four() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("x", vec![Term::var("V")]),
                vec![Literal::Positive(Atom::new("member", vec![Term::var("W")]))],
            ),
            "m",
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pass(), Pass::RangeRestriction);
        assert!(matches!(errors[0], CheckError::UnrestrictedHead { .. }));
    }

    #[test]
    fn a_negated_var_with_no_positive_binder_is_rejected() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("x", vec![Term::var("V")]),
                vec![
                    Literal::Positive(Atom::new("member", vec![Term::var("V")])),
                    Literal::Negated(Atom::new("revoked", vec![Term::var("Z")])),
                ],
            ),
            "m",
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], CheckError::UnrestrictedNegation { .. }));
    }
}
