use std::collections::{BTreeMap, BTreeSet};

use crate::{PredicateKey, Program, Registry, RuleAdornments, RuleId};

/// The stratum assignment the validator computes and the evaluator consumes.
///
/// This is in the IR for the same reason [`Adornment`](crate::Adornment) is:
/// two crates need the same answer and neither may depend on the other. The
/// validator computes it in the stratification pass; the evaluator runs stratum
/// by stratum, resolving negation against completed lower strata, and re-runs
/// the check on the rewritten program as an internal assertion. If the
/// evaluator had to recompute the decomposition itself, the two copies would be
/// free to disagree -- and a disagreement about stratum order is a wrong
/// answer, not a crash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stratification {
    strata: Vec<Vec<PredicateKey>>,
    index: BTreeMap<PredicateKey, usize>,
}

impl Stratification {
    /// Build a stratification from strata in evaluation order.
    #[must_use]
    pub fn new(strata: Vec<Vec<PredicateKey>>) -> Self {
        let mut index = BTreeMap::new();
        for (i, s) in strata.iter().enumerate() {
            for k in s {
                index.insert(k.clone(), i);
            }
        }
        Self { strata, index }
    }

    /// The strata, in the order they must be evaluated.
    #[must_use]
    pub fn strata(&self) -> &[Vec<PredicateKey>] {
        &self.strata
    }

    /// Which stratum a predicate belongs to.
    #[must_use]
    pub fn stratum_of(&self, key: &PredicateKey) -> Option<usize> {
        self.index.get(key).copied()
    }

    /// How many strata there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strata.len()
    }

    /// Whether the program has no strata at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strata.is_empty()
    }

    /// Whether `dependent` may negate `negated` -- true exactly when the
    /// negated predicate is fully computed by the time the dependent one runs.
    #[must_use]
    pub fn negation_is_stratified(&self, dependent: &PredicateKey, negated: &PredicateKey) -> bool {
        match (self.stratum_of(dependent), self.stratum_of(negated)) {
            (Some(d), Some(n)) => n < d,
            // A negated predicate with no stratum is a base predicate, which
            // is complete before evaluation begins.
            (Some(_), None) => true,
            _ => false,
        }
    }
}

/// What a predicate transitively reads.
///
/// One traversal, four consumers: the cache key needs the ref globs, the
/// effect-graph acyclicity check needs the base predicates, the gate check
/// needs the builtins, and so does the ambiguity lint. Both of the latter two
/// are defeated by a one-line wrapper predicate if the footprint stops at
/// direct references, which is why builtin reachability is transitive here
/// rather than computed at each use site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Footprint {
    /// Every host predicate reachable from this one.
    pub base: BTreeSet<PredicateKey>,
    /// Every builtin reachable from this one.
    pub builtins: BTreeSet<PredicateKey>,
    /// Every ref glob the reachable host predicates read, deduplicated.
    pub ref_globs: BTreeSet<String>,
}

impl Footprint {
    /// Whether `builtin` is reachable, however many wrapper predicates are in
    /// between.
    #[must_use]
    pub fn reaches_builtin(&self, builtin: &PredicateKey) -> bool {
        self.builtins.contains(builtin)
    }

    /// Every reachable builtin the registry marks unsafe for gates.
    pub fn gate_unsafe_builtins<'a>(
        &'a self,
        registry: &'a Registry,
    ) -> impl Iterator<Item = &'a PredicateKey> {
        self.builtins
            .iter()
            .filter(move |k| registry.builtin(k).is_some_and(|b| !b.gate_safe))
    }

    /// Whether every reachable builtin is gate-safe.
    #[must_use]
    pub fn is_gate_safe(&self, registry: &Registry) -> bool {
        self.gate_unsafe_builtins(registry).next().is_none()
    }

    /// Merge `other` into this footprint.
    pub fn absorb(&mut self, other: &Self) {
        self.base.extend(other.base.iter().cloned());
        self.builtins.extend(other.builtins.iter().cloned());
        self.ref_globs.extend(other.ref_globs.iter().cloned());
    }
}

/// A program that has passed every validation pass, with everything the passes
/// computed.
///
/// This type is the check-to-eval seam. Without it the two crates negotiate a
/// tuple of loose values and discover at merge time that one keyed adornments
/// by position and the other by id -- which is exactly the simultaneous
/// invalidation the frozen-IR gate exists to prevent. A `CheckedProgram` also
/// carries a type-level promise the loose form cannot: a `Registry` reached
/// through here has had mode and type inference run, so no consumer has to ask.
/// The fields are private, and there is no public constructor. That is the
/// whole point: "produced by the validator and by nothing else" has to be a
/// property of the type rather than a sentence in a doc comment, or a consumer
/// can assemble one from `Program::new()` and three `Default`s and hand it to
/// the evaluator, which will rewrite and run it as though every pass had
/// passed. Construction goes through [`CheckedProgram::assemble`], which is
/// hidden from the documentation and named so that calling it from anywhere
/// but `gix-query-check` reads as the mistake it is.
#[derive(Debug, Clone)]
pub struct CheckedProgram {
    program: Program,
    registry: Registry,
    stratification: Stratification,
    adornments: BTreeMap<RuleId, RuleAdornments>,
    footprints: BTreeMap<PredicateKey, Footprint>,
}

impl CheckedProgram {
    /// Assemble the result of a completed validation run.
    ///
    /// Not part of the supported surface. `gix-query-check` is the only
    /// caller, and Rust has no way to say so across a crate boundary short of
    /// a sealed trait whose ceremony would exceed its value here.
    #[doc(hidden)]
    #[must_use]
    pub fn assemble(
        program: Program,
        registry: Registry,
        stratification: Stratification,
        adornments: BTreeMap<RuleId, RuleAdornments>,
        footprints: BTreeMap<PredicateKey, Footprint>,
    ) -> Self {
        Self {
            program,
            registry,
            stratification,
            adornments,
            footprints,
        }
    }

    /// The rules, in program order.
    #[must_use]
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The registry, with every derived predicate's modes and types filled in.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The stratum assignment.
    #[must_use]
    pub fn stratification(&self) -> &Stratification {
        &self.stratification
    }

    /// Every rule's adornments, keyed by id rather than position.
    #[must_use]
    pub fn all_adornments(&self) -> &BTreeMap<RuleId, RuleAdornments> {
        &self.adornments
    }

    /// Every predicate's transitive footprint.
    #[must_use]
    pub fn all_footprints(&self) -> &BTreeMap<PredicateKey, Footprint> {
        &self.footprints
    }

    /// The footprint of `key`, empty when the predicate reads nothing.
    #[must_use]
    pub fn footprint(&self, key: &PredicateKey) -> Option<&Footprint> {
        self.footprints.get(key)
    }

    /// The adornments of `rule`.
    #[must_use]
    pub fn adornments(&self, rule: RuleId) -> Option<&RuleAdornments> {
        self.adornments.get(&rule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> PredicateKey {
        PredicateKey::new(name, 1)
    }

    #[test]
    fn negation_must_point_at_a_lower_stratum() {
        let s = Stratification::new(vec![vec![key("reviewed")], vec![key("blocked")]]);
        assert!(s.negation_is_stratified(&key("blocked"), &key("reviewed")));
        assert!(!s.negation_is_stratified(&key("reviewed"), &key("blocked")));
        // Same stratum is a cycle through negation, which is what the pass
        // exists to reject.
        assert!(!s.negation_is_stratified(&key("blocked"), &key("blocked")));
    }

    #[test]
    fn a_base_predicate_is_always_available_to_negate() {
        let s = Stratification::new(vec![vec![key("blocked")]]);
        assert!(s.negation_is_stratified(&key("blocked"), &key("member")));
    }

    #[test]
    fn a_transitively_reachable_fuzzy_builtin_is_still_reachable() {
        // The wrapper-predicate defeat is the whole reason the footprint is
        // transitive over builtins, so it gets a test rather than a comment.
        let registry = Registry::host();
        let mut inner = Footprint::default();
        inner.builtins.insert(PredicateKey::new("bind_fuzzy", 7));
        let mut outer = Footprint::default();
        outer.absorb(&inner);
        assert!(outer.reaches_builtin(&PredicateKey::new("bind_fuzzy", 7)));
        assert!(!outer.is_gate_safe(&registry));
        assert_eq!(outer.gate_unsafe_builtins(&registry).count(), 1);
    }

    #[test]
    fn a_gate_safe_footprint_passes() {
        let registry = Registry::host();
        let mut f = Footprint::default();
        f.builtins.insert(PredicateKey::new("bind", 7));
        f.builtins.insert(PredicateKey::new("!=", 2));
        assert!(f.is_gate_safe(&registry));
    }
}
