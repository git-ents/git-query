//! Pass 7 -- footprint.
//!
//! Transitive closure to base predicates *and to builtins*, over the same
//! SCC decomposition pass 5 already computed. Builtin reachability has to be
//! transitive for the same reason base-predicate reachability does: the
//! `bind_fuzzy`-in-gates prohibition and the ambiguity lint (both downstream
//! of this crate, in whatever assembles gate rules) are each a question of
//! "does this predicate's footprint reach builtin X", and a footprint that
//! stopped at direct references would be defeated by a one-line wrapper
//! predicate around the unsafe call.
//!
//! This pass reads only [`Registry`] data -- backing sources, `is_base`,
//! `is_builtin` -- never a repository, which is what keeps this crate free
//! of a git dependency.
//!
//! Unlike the other eight, this pass produces no [`crate::CheckError`] of
//! its own -- there is no [`crate::Pass::Footprint`] reject fixture, and
//! that is by design rather than a gap. The spec's own wording is precise
//! about this: the footprint is *consumed* by four checks (the cache key,
//! the effect-graph acyclicity check, the `bind_fuzzy`-in-gates prohibition,
//! the ambiguity lint), and every one of those consumers needs a concept
//! this crate does not have -- "which rules are gates", "what the effect
//! graph is" -- so they belong to whatever assembles gate rules
//! (`gix-query-rules`, `gix-query`), not here. What this pass owns is
//! getting the traversal right, which the two tests below check directly
//! against the footprint's contents.

use std::collections::BTreeMap;

use gix_query_ir::{Footprint, PredicateKey, Program, Registry};

use crate::pass5_stratify::Stratify;

/// One footprint per predicate the program defines, computed in
/// dependency-first order so that an IDB dependency's footprint is always
/// already on hand when a caller needs to absorb it.
pub(crate) fn compute(
    program: &Program,
    registry: &Registry,
    stratify: &Stratify,
) -> BTreeMap<PredicateKey, Footprint> {
    let mut footprints: BTreeMap<PredicateKey, Footprint> = BTreeMap::new();

    for &idx in &stratify.sccs.order {
        let members = &stratify.sccs.members[idx];
        let mut combined = Footprint::default();
        for key in members {
            for (_, rule) in program.rules_for(key) {
                for lit in &rule.body {
                    fold_literal(
                        lit.key(),
                        idx,
                        registry,
                        stratify,
                        &footprints,
                        &mut combined,
                    );
                }
            }
        }
        for key in members {
            footprints.insert(key.clone(), combined.clone());
        }
    }

    footprints
}

fn fold_literal(
    target: &PredicateKey,
    scc: usize,
    registry: &Registry,
    stratify: &Stratify,
    footprints: &BTreeMap<PredicateKey, Footprint>,
    combined: &mut Footprint,
) {
    if let Some(entry) = registry.predicate(target)
        && entry.class.is_edb()
    {
        combined.base.insert(target.clone());
        combined
            .ref_globs
            .extend(entry.ref_globs().map(str::to_owned));
        return;
    }
    if registry.is_builtin(target) {
        combined.builtins.insert(target.clone());
        return;
    }
    // An IDB predicate. Direct calls to other members of this same SCC are
    // folded in for free, since every member's own rules are walked into
    // this same `combined` accumulator; only a dependency *outside* the SCC
    // needs its precomputed footprint absorbed.
    if let Some(dep_scc) = stratify.sccs.of(target)
        && dep_scc != scc
        && let Some(dep_fp) = footprints.get(target)
    {
        combined.absorb(dep_fp);
    }
    // A predicate resolving to neither an EDB entry, a builtin, nor a known
    // SCC member is unresolved and already reported by pass 2; nothing more
    // to fold in here.
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Literal, Rule, Term};

    use super::*;
    use crate::error::Errors;

    #[test]
    fn a_footprint_reaches_a_wrapped_fuzzy_builtin() {
        // wrap(A, Rev, B, S, E, P, C) :- bind_fuzzy(A, Rev, B, S, E, P, C).
        // outer(A, Rev, B, S, E, P, C) :- wrap(A, Rev, B, S, E, P, C).
        let mut program = Program::new();
        let vars = || {
            vec![
                Term::var("A"),
                Term::var("R"),
                Term::var("B"),
                Term::var("S"),
                Term::var("E"),
                Term::var("P"),
                Term::var("Con"),
            ]
        };
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("wrap", vars()),
                vec![Literal::Positive(Atom::new("bind_fuzzy", vars()))],
            ),
            "m",
        );
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("outer", vars()),
                vec![Literal::Positive(Atom::new("wrap", vars()))],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        let mut stratify_errors = Errors::new();
        let stratify = crate::pass5_stratify::check(&program, &registry, &mut stratify_errors);
        assert!(stratify_errors.is_empty());
        let footprints = compute(&program, &registry, &stratify);
        let outer_fp = footprints
            .get(&PredicateKey::new("outer", 7))
            .expect("outer has a footprint");
        assert!(outer_fp.reaches_builtin(&PredicateKey::new("bind_fuzzy", 7)));
        assert!(!outer_fp.is_gate_safe(&registry));
    }

    #[test]
    fn a_footprint_reaches_transitive_ref_globs() {
        // reviewers(M) :- signer(C, M).
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("reviewers", vec![Term::var("M")]),
                vec![Literal::Positive(Atom::new(
                    "signer",
                    vec![Term::var("C"), Term::var("M")],
                ))],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        let stratify = crate::pass5_stratify::check(&program, &registry, &mut Errors::new());
        let footprints = compute(&program, &registry, &stratify);
        let fp = footprints
            .get(&PredicateKey::new("reviewers", 1))
            .expect("has a footprint");
        assert!(fp.base.contains(&PredicateKey::new("signer", 2)));
        assert!(fp.ref_globs.contains("refs/meta/claims/*"));
    }
}
