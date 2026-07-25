//! Pass 6 -- shape caps.
//!
//! Caps on literals, variables, and rules bound validation cost itself:
//! every other pass is near-linear in program size, and a cap is what keeps
//! "near-linear" from being tested against pathological input nobody would
//! actually author. The numbers are not pinned by the specification, so they
//! are chosen generously and documented rather than derived; the spec's own
//! bound is [`gix_query_ir::ArgSet::MAX_ARITY`], which the working registry
//! already enforces per predicate through [`crate::registry_build`].
//!
//! The fourth check -- a body literal unjoined to the rest of the body -- is
//! a caps concern rather than a range-restriction one, because a literal
//! that mentions no variable the rest of the body also mentions is not
//! *unsound*, it is an unbounded cross product (or, if it binds nothing at
//! all, dead weight); either way it is a shape problem, not a soundness one.

use std::collections::BTreeSet;

use gix_query_ir::{Literal, Program, Rule, RuleId, Var};

use crate::error::{CheckError, Errors};

/// Literals per rule body.
pub(crate) const MAX_BODY_LITERALS: usize = 64;
/// Distinct variables per rule, head included.
pub(crate) const MAX_RULE_VARS: usize = 64;
/// Rules per program, across every module.
pub(crate) const MAX_PROGRAM_RULES: usize = 4096;

/// Check every cap over the whole program.
pub(crate) fn check(program: &Program, errors: &mut Errors) {
    let rule_count = program.rules().len();
    if rule_count > MAX_PROGRAM_RULES {
        errors.push(CheckError::TooManyRules {
            found: rule_count,
            max: MAX_PROGRAM_RULES,
        });
    }
    for (i, rule) in program.rules().iter().enumerate() {
        check_rule(RuleId(i), rule, errors);
    }
}

fn check_rule(id: RuleId, rule: &Rule, errors: &mut Errors) {
    if rule.body.len() > MAX_BODY_LITERALS {
        errors.push(CheckError::TooManyLiterals {
            rule: id,
            found: rule.body.len(),
            max: MAX_BODY_LITERALS,
        });
    }
    let vars: BTreeSet<&Var> = rule
        .head_vars()
        .chain(rule.body.iter().flat_map(Literal::vars))
        .collect();
    if vars.len() > MAX_RULE_VARS {
        errors.push(CheckError::TooManyVariables {
            rule: id,
            found: vars.len(),
            max: MAX_RULE_VARS,
        });
    }
    check_joins(id, rule, errors);
}

fn check_joins(id: RuleId, rule: &Rule, errors: &mut Errors) {
    if rule.body.len() <= 1 {
        return;
    }
    let var_sets: Vec<BTreeSet<&Var>> = rule.body.iter().map(|l| l.vars().collect()).collect();
    for (i, lit) in rule.body.iter().enumerate() {
        if var_sets[i].is_empty() {
            // No variable to join with anything -- an all-constant literal
            // is a plain filter, never a cross product.
            continue;
        }
        let joins = var_sets
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .any(|(_, s)| !s.is_disjoint(&var_sets[i]));
        if !joins {
            errors.push(CheckError::UnjoinedLiteral {
                rule: id,
                index: i,
                predicate: lit.key().clone(),
                span: lit.span(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Term};

    use super::*;
    use crate::error::Pass;

    #[test]
    fn a_body_that_all_joins_is_accepted() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("x", vec![Term::var("V")]),
                vec![
                    Literal::Positive(Atom::new("member", vec![Term::var("V")])),
                    Literal::Negated(Atom::new("revoked", vec![Term::var("V")])),
                ],
            ),
            "m",
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn an_unjoined_literal_is_pass_six() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("x", vec![Term::var("V"), Term::var("W")]),
                vec![
                    Literal::Positive(Atom::new("member", vec![Term::var("V")])),
                    Literal::Positive(Atom::new("member", vec![Term::var("W")])),
                ],
            ),
            "m",
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert_eq!(errors.len(), 2, "neither literal joins the other");
        for e in &errors {
            assert_eq!(e.pass(), Pass::ShapeCaps);
            assert!(matches!(e, CheckError::UnjoinedLiteral { .. }));
        }
    }
}
