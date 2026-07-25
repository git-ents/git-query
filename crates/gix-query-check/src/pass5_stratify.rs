//! Pass 5 -- stratification.
//!
//! No negative edge may close a cycle: a predicate may not negate another
//! predicate that, through zero or more further calls, calls back into it.
//! The SCC decomposition in [`crate::graph`] answers exactly that question --
//! two predicates in the same component are mutually reachable, so a
//! negative edge between them is the cycle this pass exists to reject --
//! and its dependency-first order is what becomes [`Stratification`]'s
//! evaluation order.

use gix_query_ir::{PredicateKey, Program, Registry, Stratification};

use crate::error::{CheckError, Errors};
use crate::graph::{self, DepGraph, Sccs};

/// Everything pass 5 computes, since pass 7 and pass 9 both need the same
/// decomposition rather than a second, possibly-disagreeing one.
pub(crate) struct Stratify {
    pub stratification: Stratification,
    pub sccs: Sccs,
    pub graph: DepGraph,
}

/// Decompose the program's predicate dependency graph into strata, checking
/// that no negative edge closes a cycle.
pub(crate) fn check(program: &Program, registry: &Registry, errors: &mut Errors) -> Stratify {
    let graph = graph::build(program, registry);
    let sccs = graph::tarjan(&graph);

    for (dependent, edges) in &graph {
        for edge in edges {
            if edge.negated && sccs.of(dependent) == sccs.of(&edge.to) {
                errors.push(CheckError::UnstratifiableNegation {
                    dependent: dependent.clone(),
                    negated: edge.to.clone(),
                    rule: edge.rule,
                    span: edge.span,
                });
            }
        }
    }

    let stratification = assign_strata(&sccs, &graph);
    Stratify {
        stratification,
        sccs,
        graph,
    }
}

/// A component's stratum is one past the highest stratum among its
/// dependencies, zero when it has none. `sccs.order` already visits
/// dependencies before dependents, so a single forward pass with a
/// pre-filled table is enough -- no recursion, no revisiting.
fn assign_strata(sccs: &Sccs, graph: &DepGraph) -> Stratification {
    let mut stratum_of_component = vec![0usize; sccs.members.len()];
    for &idx in &sccs.order {
        let mut stratum = 0usize;
        for key in &sccs.members[idx] {
            for edge in graph.get(key).into_iter().flatten() {
                if let Some(dep_idx) = sccs.of(&edge.to) {
                    stratum = stratum.max(stratum_of_component[dep_idx] + 1);
                }
            }
        }
        stratum_of_component[idx] = stratum;
    }

    let num_strata = stratum_of_component
        .iter()
        .copied()
        .max()
        .map_or(0, |m| m + 1);
    let mut strata: Vec<Vec<PredicateKey>> = vec![Vec::new(); num_strata];
    for (idx, members) in sccs.members.iter().enumerate() {
        strata[stratum_of_component[idx]].extend(members.iter().cloned());
    }
    Stratification::new(strata)
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Rule, Term};

    use super::*;
    use crate::error::Pass;

    fn fact_rule(head: &str, arg: &str) -> Rule {
        Rule::fact(Atom::new(head, vec![Term::sym(arg)]))
    }

    #[test]
    fn a_negation_across_strata_is_accepted() {
        // active_member(M) :- member(M), !revoked(M).
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("active_member", vec![Term::var("M")]),
                vec![
                    gix_query_ir::Literal::Positive(Atom::new("member", vec![Term::var("M")])),
                    gix_query_ir::Literal::Negated(Atom::new("revoked", vec![Term::var("M")])),
                ],
            ),
            "m",
        );
        let registry =
            crate::registry_build::build(&program, &Registry::host(), &mut Errors::new());
        let mut errors = Errors::new();
        check(&program, &registry, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn mutual_negation_is_pass_five() {
        // p(X) :- q(X), !q(X). is degenerate but exercises the same cycle a
        // real mutual-negation pair would: a predicate negating something in
        // its own component.
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("p", vec![Term::var("X")]),
                vec![
                    gix_query_ir::Literal::Positive(Atom::new("q", vec![Term::var("X")])),
                    gix_query_ir::Literal::Negated(Atom::new("p", vec![Term::var("X")])),
                ],
            ),
            "m",
        );
        crate::testutil::add(&mut program, fact_rule("q", "x"), "m");
        let mut build_errors = Errors::new();
        let registry = crate::registry_build::build(&program, &Registry::host(), &mut build_errors);
        let mut errors = Errors::new();
        check(&program, &registry, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pass(), Pass::Stratification);
        assert!(matches!(
            errors[0],
            CheckError::UnstratifiableNegation { .. }
        ));
    }

    #[test]
    fn strata_place_dependencies_before_dependents() {
        let mut program = Program::new();
        crate::testutil::add(&mut program, fact_rule("a", "x"), "m");
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("b", vec![Term::var("X")]),
                vec![gix_query_ir::Literal::Positive(Atom::new(
                    "a",
                    vec![Term::var("X")],
                ))],
            ),
            "m",
        );
        let registry =
            crate::registry_build::build(&program, &Registry::host(), &mut Errors::new());
        let mut errors = Errors::new();
        let result = check(&program, &registry, &mut errors);
        assert!(errors.is_empty());
        let a = PredicateKey::new("a", 1);
        let b = PredicateKey::new("b", 1);
        assert!(
            result.stratification.stratum_of(&a).unwrap()
                < result.stratification.stratum_of(&b).unwrap()
        );
    }
}
