//! Pass 7 -- footprint.
//!
//! Transitive closure to base predicates *and to builtins*, over the same
//! SCC decomposition pass 5 already computed. Builtin reachability has to be
//! transitive for the same reason base-predicate reachability does: the
//! not-gate-safe-in-gates prohibition and the ambiguity lint (both downstream
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
//! the effect-graph acyclicity check, the not-gate-safe-in-gates prohibition,
//! the ambiguity lint), and every one of those consumers needs a concept
//! this crate does not have -- "which rules are gates", "what the effect
//! graph is" -- so they belong to whatever assembles gate rules
//! (`gix-query-rules`, `gix-query`), not here. What this pass owns is
//! getting the traversal right, which the two tests below check directly
//! against the footprint's contents.

use std::collections::BTreeMap;

use gix_query_ir::{Footprint, Literal, PredicateKey, Program, Registry};

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

/// The footprint of a bare *body* -- a goal, or an effect's `when`/`gate`
/// clause -- against footprints this pass already computed for a program.
///
/// A body defines no predicate, so it has no entry in [`compute`]'s map; what
/// it has is literals, each of which is a host predicate (contributing its
/// own backing), a builtin, or an IDB predicate of the program (contributing
/// that predicate's already-computed footprint). This is the same three-way
/// fold [`fold_literal`] performs, minus the SCC bookkeeping a body does not
/// need: a body is never a member of any SCC, so every IDB dependency is
/// "outside" it and its precomputed footprint is always the right thing to
/// absorb.
pub(crate) fn compute_body(
    body: &[Literal],
    registry: &Registry,
    footprints: &BTreeMap<PredicateKey, Footprint>,
) -> Footprint {
    let mut combined = Footprint::default();
    for lit in body {
        let target = lit.key();
        if let Some(entry) = registry.predicate(target)
            && entry.class.is_edb()
        {
            combined.base.insert(target.clone());
            combined
                .ref_globs
                .extend(entry.ref_globs().map(str::to_owned));
        } else if registry.is_builtin(target) {
            combined.builtins.insert(target.clone());
        } else if let Some(dep_fp) = footprints.get(target) {
            combined.absorb(dep_fp);
        }
        // Anything else is unresolved and already reported by pass 2.
    }
    combined
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

    /// A synthetic non-gate-safe builtin, registered only in this test's
    /// own registry -- `cand` itself is gate-safe now that every candidate
    /// (pin, or one of the three named oracles) is a pure function of
    /// content and ref state, so nothing in the real registry is unsafe for
    /// gates any more.
    fn registry_with_unsafe_builtin() -> Registry {
        let mut r = Registry::host();
        let mut entry = r
            .builtin(&PredicateKey::new("cand", 5))
            .expect("cand")
            .clone();
        entry.key = PredicateKey::new("test_unsafe", 5);
        entry.gate_safe = false;
        r.insert_builtin(entry);
        r
    }

    #[test]
    fn a_footprint_reaches_a_wrapped_unsafe_builtin() {
        // wrap(A, Rev, L, O, Con) :- test_unsafe(A, Rev, L, O, Con).
        // outer(A, Rev, L, O, Con) :- wrap(A, Rev, L, O, Con).
        let mut program = Program::new();
        let vars = || {
            vec![
                Term::var("A"),
                Term::var("R"),
                Term::var("L"),
                Term::var("O"),
                Term::var("Con"),
            ]
        };
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("wrap", vars()),
                vec![Literal::Positive(Atom::new("test_unsafe", vars()))],
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
        let registry =
            crate::registry_build::build(&program, &registry_with_unsafe_builtin(), &mut errors);
        let mut stratify_errors = Errors::new();
        let stratify = crate::pass5_stratify::check(&program, &registry, &mut stratify_errors);
        assert!(stratify_errors.is_empty());
        let footprints = compute(&program, &registry, &stratify);
        let outer_fp = footprints
            .get(&PredicateKey::new("outer", 5))
            .expect("outer has a footprint");
        assert!(outer_fp.reaches_builtin(&PredicateKey::new("test_unsafe", 5)));
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
