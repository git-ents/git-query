use std::collections::HashMap;
use std::sync::Arc;

use crate::{PredicateKey, RewriteKind, RuleId, Value};

/// One supporting tuple in a derivation: which body literal matched what.
#[derive(Debug, Clone, Copy)]
pub struct Support<'a> {
    /// The literal's index in the rule *as it fired*, which after the
    /// magic-set rewrite is not the same as its index in the source rule.
    pub literal: usize,
    /// The literal's index in the source rule's body, or `None` for a guard
    /// the rewrite injected.
    ///
    /// A guard shifts every following literal by one, so an `explain` that
    /// indexed a source rule with rewritten offsets would misattribute every
    /// support in the tree -- silently, and most visibly on the recursive rules
    /// people actually ask about.
    pub source_literal: Option<usize>,
    /// The predicate it called.
    pub predicate: &'a PredicateKey,
    /// The tuple it matched. For a negated literal this is the instantiation
    /// that was *not* found, which is the witness `explain` reports.
    pub tuple: &'a [Value],
    /// Whether the literal was negated.
    pub negated: bool,
    /// The ref that supplied this fact, for a base fact read from one.
    ///
    /// `explain` annotates each base fact with its source ref, and for several
    /// predicates the refname is not a function of the tuple -- `claim(C)`
    /// being the clear case. Reconstructing it afterwards means re-reading the
    /// refs, which can disagree with what the evaluator actually read.
    pub source_ref: Option<&'a str>,
}

/// One firing of one rule.
#[derive(Debug, Clone, Copy)]
pub struct Derivation<'a> {
    /// The rule that fired, in the program as rewritten.
    pub generated: RuleId,
    /// The rule someone wrote, or `None` when there is none -- a magic seed is
    /// demand created by a top-level goal, and no rule stands behind it.
    pub source: Option<RuleId>,
    /// What the rewrite made of the rule, or `None` for a rule it did not
    /// touch.
    ///
    /// Carried rather than resolved away, because a firing of a demand rule
    /// would otherwise be recorded as a firing of the source rule with a
    /// `magic_` head tuple, and the tree builder would have nothing to filter
    /// on.
    pub rewrite: Option<RewriteKind>,
    /// The head tuple derived.
    pub head: &'a [Value],
    /// The body literals that supported it.
    pub support: &'a [Support<'a>],
}

impl Derivation<'_> {
    /// Whether this firing is engine bookkeeping and must not appear in a
    /// derivation tree.
    #[must_use]
    pub fn is_bookkeeping(&self) -> bool {
        matches!(
            self.rewrite,
            Some(RewriteKind::MagicSeed | RewriteKind::MagicRule)
        )
    }
}

/// Where the evaluator records derivations.
///
/// Provenance is a recording obligation *inside* the join loop -- which tuple
/// pairing produced which derived tuple -- and it cannot be reconstructed
/// afterwards from a materialized relation, because set semantics has already
/// discarded the multiplicity that would tell you. So the evaluator carries a
/// sink from its first commit rather than growing one later.
///
/// [`Self::ENABLED`] is an associated const rather than a runtime flag so that
/// the disabled case costs nothing: the evaluator guards every recording site
/// with `if P::ENABLED`, and monomorphization deletes both the branch and the
/// [`Derivation`] construction inside it. A trait object would cost a virtual
/// call and an argument build per tuple, which is the cost this design
/// explicitly refuses to pay on the gate path.
pub trait Provenance {
    /// Whether the evaluator should construct and report derivations at all.
    const ENABLED: bool;

    /// Record one rule firing.
    fn derived(&mut self, event: &Derivation<'_>);
}

/// The sink for gates and bulk queries: records nothing, costs nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProvenance;

impl Provenance for NoProvenance {
    const ENABLED: bool = false;

    #[inline(always)]
    fn derived(&mut self, _event: &Derivation<'_>) {}
}

/// An owned derivation, as a recording sink keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDerivation {
    /// The rule that fired, in the program as rewritten.
    pub generated: RuleId,
    /// The rule someone wrote, if there is one.
    pub source: Option<RuleId>,
    /// What the rewrite made of it.
    pub rewrite: Option<RewriteKind>,
    /// The head tuple.
    pub head: Vec<Value>,
    /// The supporting tuples.
    pub support: Vec<OwnedSupport>,
}

/// An owned supporting tuple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedSupport {
    /// The literal's index in the rule as it fired.
    pub literal: usize,
    /// Its index in the source rule's body.
    pub source_literal: Option<usize>,
    /// The predicate it called.
    pub predicate: PredicateKey,
    /// The tuple it matched.
    pub tuple: Vec<Value>,
    /// Whether the literal was negated.
    pub negated: bool,
    /// The ref that supplied it.
    pub source_ref: Option<Arc<str>>,
}

/// The sink `explain` uses: keeps every derivation, indexed by head tuple.
///
/// Indexed rather than scanned because building a derivation tree walks
/// supports back to base facts, looking up each one -- a linear scan per lookup
/// makes that quadratic in the number of derivations, on the one code path
/// where the recording is already the expensive part.
#[derive(Debug, Clone, Default)]
pub struct CollectingProvenance {
    derivations: Vec<OwnedDerivation>,
    by_head: HashMap<Vec<Value>, Vec<usize>>,
}

impl CollectingProvenance {
    /// An empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every derivation recorded, in firing order.
    #[must_use]
    pub fn derivations(&self) -> &[OwnedDerivation] {
        &self.derivations
    }

    /// Every recorded derivation of `head`.
    pub fn derivations_of<'a>(
        &'a self,
        head: &[Value],
    ) -> impl Iterator<Item = &'a OwnedDerivation> {
        self.by_head
            .get(head)
            .into_iter()
            .flatten()
            .map(|i| &self.derivations[*i])
    }

    /// Drop everything recorded.
    pub fn clear(&mut self) {
        self.derivations.clear();
        self.by_head.clear();
    }
}

impl Provenance for CollectingProvenance {
    const ENABLED: bool = true;

    fn derived(&mut self, event: &Derivation<'_>) {
        let head = event.head.to_vec();
        self.by_head
            .entry(head.clone())
            .or_default()
            .push(self.derivations.len());
        self.derivations.push(OwnedDerivation {
            generated: event.generated,
            source: event.source,
            rewrite: event.rewrite,
            head,
            support: event
                .support
                .iter()
                .map(|s| OwnedSupport {
                    literal: s.literal,
                    source_literal: s.source_literal,
                    predicate: s.predicate.clone(),
                    tuple: s.tuple.to_vec(),
                    negated: s.negated,
                    source_ref: s.source_ref.map(Arc::from),
                })
                .collect(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event<'a>(
        head: &'a [Value],
        support: &'a [Support<'a>],
        rewrite: Option<RewriteKind>,
    ) -> Derivation<'a> {
        Derivation {
            generated: RuleId(7),
            source: Some(RuleId(0)),
            rewrite,
            head,
            support,
        }
    }

    #[test]
    fn the_no_op_sink_is_statically_disabled() {
        const { assert!(!NoProvenance::ENABLED) };
        const { assert!(CollectingProvenance::ENABLED) };
    }

    #[test]
    fn a_collecting_sink_indexes_by_head() {
        let key = PredicateKey::new("member", 1);
        let head = [Value::sym("joey")];
        let tuple = [Value::sym("joey")];
        let support = [Support {
            literal: 1,
            source_literal: Some(0),
            predicate: &key,
            tuple: &tuple,
            negated: false,
            source_ref: Some("refs/meta/members/joey"),
        }];
        let mut sink = CollectingProvenance::new();
        sink.derived(&event(&head, &support, None));
        assert_eq!(sink.derivations().len(), 1);
        assert_eq!(sink.derivations_of(&head).count(), 1);
        assert_eq!(sink.derivations_of(&[Value::sym("other")]).count(), 0);
    }

    #[test]
    fn a_guard_shifts_the_source_literal_index() {
        // The rewrite injects a demand guard at position 0, so what the author
        // wrote as literal 0 fires as literal 1. `explain` reads
        // `source_literal`, never `literal`.
        let key = PredicateKey::new("member", 1);
        let tuple = [Value::sym("joey")];
        let support = [
            Support {
                literal: 0,
                source_literal: None,
                predicate: &key,
                tuple: &tuple,
                negated: false,
                source_ref: None,
            },
            Support {
                literal: 1,
                source_literal: Some(0),
                predicate: &key,
                tuple: &tuple,
                negated: false,
                source_ref: None,
            },
        ];
        let head = [Value::sym("joey")];
        let mut sink = CollectingProvenance::new();
        sink.derived(&event(&head, &support, Some(RewriteKind::Guarded)));
        let d = &sink.derivations()[0];
        assert_eq!(d.support[0].source_literal, None);
        assert_eq!(d.support[1].source_literal, Some(0));
    }

    #[test]
    fn demand_firings_are_marked_as_bookkeeping() {
        let head = [Value::sym("HEAD")];
        assert!(event(&head, &[], Some(RewriteKind::MagicSeed)).is_bookkeeping());
        assert!(event(&head, &[], Some(RewriteKind::MagicRule)).is_bookkeeping());
        assert!(!event(&head, &[], Some(RewriteKind::Guarded)).is_bookkeeping());
        assert!(!event(&head, &[], None).is_bookkeeping());
    }
}
