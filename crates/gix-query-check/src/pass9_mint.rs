//! Pass 9 -- minting confinement.
//!
//! A builtin declared `mints` may not appear in a recursive SCC: each
//! round of the fixpoint could mint a fresh value the next round joins
//! against, and nothing bounds how many rounds that goes on for. `finite`
//! bounds the count *per call*; this pass is what stops the *number of
//! calls* from being unbounded too. It runs over the exact SCC
//! decomposition pass 5 already computed, never a second one -- see
//! [`crate::graph`]'s module doc for why that matters.

use gix_query_ir::{Literal, Program, Registry, RuleId};

use crate::error::{CheckError, Errors};
use crate::pass5_stratify::Stratify;

/// Check that no `mints` builtin appears in a rule whose head predicate is
/// in a recursive stratum.
pub(crate) fn check(
    program: &Program,
    registry: &Registry,
    stratify: &Stratify,
    errors: &mut Errors,
) {
    for (i, rule) in program.rules().iter().enumerate() {
        let head_key = &rule.head.predicate;
        if !stratify.sccs.is_recursive(head_key, &stratify.graph) {
            continue;
        }
        for lit in &rule.body {
            // Both syntactic forms: a module that has not been through name
            // resolution carries `bind(...)` as a positive literal, and
            // matching only the reclassified form let exactly the program
            // this pass exists to reject through unresolved.
            let (Literal::Builtin(atom) | Literal::Positive(atom)) = lit else {
                continue;
            };
            let Some(b) = registry.builtin(&atom.predicate) else {
                continue;
            };
            if b.props.mints {
                errors.push(CheckError::MintingInRecursion {
                    builtin: atom.predicate.clone(),
                    predicate: head_key.clone(),
                    rule: RuleId(i),
                    span: atom.span,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Rule, Term};

    use super::*;
    use crate::error::Pass;

    fn bind_call() -> Literal {
        Literal::Builtin(Atom::new(
            "bind",
            vec![
                Term::var("A"),
                Term::var("Rev"),
                Term::var("B"),
                Term::var("S"),
                Term::var("E"),
                Term::var("P"),
                Term::var("C"),
            ],
        ))
    }

    #[test]
    fn bind_outside_recursion_is_accepted() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new(
                    "spans",
                    vec![
                        Term::var("A"),
                        Term::var("Rev"),
                        Term::var("B"),
                        Term::var("S"),
                        Term::var("E"),
                        Term::var("P"),
                        Term::var("C"),
                    ],
                ),
                vec![bind_call()],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        let stratify = crate::pass5_stratify::check(&program, &registry, &mut errors);
        assert!(errors.is_empty());
        check(&program, &registry, &stratify, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn bind_inside_a_recursive_predicate_is_pass_nine() {
        // loopy(A, Rev, B, S, E, P, C) :- loopy(A, Rev, _, _, _, _, _), bind(...).
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new(
                    "loopy",
                    vec![
                        Term::var("A"),
                        Term::var("Rev"),
                        Term::var("B"),
                        Term::var("S"),
                        Term::var("E"),
                        Term::var("P"),
                        Term::var("C"),
                    ],
                ),
                vec![
                    Literal::Positive(Atom::new(
                        "loopy",
                        vec![
                            Term::var("A"),
                            Term::var("Rev"),
                            Term::Anon,
                            Term::Anon,
                            Term::Anon,
                            Term::Anon,
                            Term::Anon,
                        ],
                    )),
                    bind_call(),
                ],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        let stratify = crate::pass5_stratify::check(&program, &registry, &mut errors);
        assert!(errors.is_empty());
        check(&program, &registry, &stratify, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pass(), Pass::MintingConfinement);
        assert!(matches!(errors[0], CheckError::MintingInRecursion { .. }));
    }
}
