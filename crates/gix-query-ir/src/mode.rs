use std::collections::BTreeSet;
use std::fmt;

/// Whether an argument must be bound at call time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    /// `+` -- the caller supplies a ground value.
    Bound,
    /// `-` -- the callee supplies it.
    Free,
}

impl Mode {
    /// The `+`/`-` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bound => "+",
            Self::Free => "-",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One argument-by-argument mode pattern.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModeVector(Vec<Mode>);

impl ModeVector {
    /// A mode vector over `modes`.
    #[must_use]
    pub fn new(modes: Vec<Mode>) -> Self {
        Self(modes)
    }

    /// All-free, of the given arity.
    #[must_use]
    pub fn all_free(arity: usize) -> Self {
        Self(vec![Mode::Free; arity])
    }

    /// All-bound, of the given arity -- the only legal mode for a filter.
    #[must_use]
    pub fn all_bound(arity: usize) -> Self {
        Self(vec![Mode::Bound; arity])
    }

    /// The arity this vector describes.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.0.len()
    }

    /// The mode of argument `i`.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<Mode> {
        self.0.get(i).copied()
    }

    /// The modes, in order.
    #[must_use]
    pub fn modes(&self) -> &[Mode] {
        &self.0
    }

    /// The indices this vector requires bound.
    #[must_use]
    pub fn required_bound(&self) -> ArgSet {
        let mut set = ArgSet::empty();
        for (i, m) in self.0.iter().enumerate() {
            if *m == Mode::Bound {
                set.insert(i);
            }
        }
        set
    }

    /// Whether a call whose bound arguments are exactly `bound` satisfies this
    /// vector. Binding *more* than required is always fine: a required-free
    /// argument is a statement about what the callee supplies, not a
    /// prohibition on the caller knowing it.
    #[must_use]
    pub fn satisfied_by(&self, bound: &ArgSet) -> bool {
        self.required_bound().is_subset(bound)
    }
}

impl fmt::Display for ModeVector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for m in &self.0 {
            f.write_str(m.as_str())?;
        }
        Ok(())
    }
}

/// The set of mode vectors an EDB predicate supports.
///
/// EDB predicates carry a *set*, not a vector, and this is load-bearing rather
/// than generality for its own sake. `member/1` is legitimately called free --
/// enumerate the member namespace -- and bound -- test one member. Both are
/// affordable, so both must be legal, and a single vector cannot say that.
///
/// IDB predicates are the other case: their modes are inferred rather than
/// declared, and they carry a single required-bound [`ArgSet`] unioned over
/// their rules.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModeSet(BTreeSet<ModeVector>);

impl ModeSet {
    /// A mode set containing exactly `vectors`.
    #[must_use]
    pub fn new(vectors: impl IntoIterator<Item = ModeVector>) -> Self {
        Self(vectors.into_iter().collect())
    }

    /// The set containing only the all-free vector: fully enumerable.
    #[must_use]
    pub fn enumerable(arity: usize) -> Self {
        Self::new([ModeVector::all_free(arity)])
    }

    /// The supported vectors.
    pub fn vectors(&self) -> impl Iterator<Item = &ModeVector> {
        self.0.iter()
    }

    /// Whether the set is empty, which would make the predicate uncallable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The vector to use for a call whose bound arguments are `bound`, or
    /// `None` when no supported vector is satisfied.
    ///
    /// When several are satisfied the most-bound one wins, since it is the
    /// cheapest: a provider given more keys does less work.
    #[must_use]
    pub fn select(&self, bound: &ArgSet) -> Option<&ModeVector> {
        self.0
            .iter()
            .filter(|v| v.satisfied_by(bound))
            .max_by_key(|v| v.required_bound().len())
    }

    /// The arguments *every* supported vector requires bound -- what a caller
    /// must supply no matter which vector is chosen.
    #[must_use]
    pub fn always_required(&self) -> ArgSet {
        let mut it = self.0.iter();
        let Some(first) = it.next() else {
            return ArgSet::empty();
        };
        it.fold(first.required_bound(), |acc, v| {
            acc.intersection(&v.required_bound())
        })
    }
}

/// A set of argument indices, as a bitset over at most 64 arguments.
///
/// The shape caps make an arity above 64 unreachable, and a bitset keeps the
/// mode-inference fixpoint cheap -- it converges by set union, and union is
/// one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ArgSet(u64);

impl ArgSet {
    /// The maximum arity an `ArgSet` can describe.
    pub const MAX_ARITY: usize = 64;

    /// The empty set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Every index below `arity`.
    #[must_use]
    pub const fn full(arity: usize) -> Self {
        if arity >= Self::MAX_ARITY {
            Self(u64::MAX)
        } else {
            Self((1u64 << arity) - 1)
        }
    }

    /// The set containing exactly `indices`.
    #[must_use]
    pub fn from_indices(indices: impl IntoIterator<Item = usize>) -> Self {
        let mut s = Self::empty();
        for i in indices {
            s.insert(i);
        }
        s
    }

    /// Add `i`. Indices at or above [`Self::MAX_ARITY`] are ignored, which the
    /// shape caps make unreachable.
    pub fn insert(&mut self, i: usize) {
        if i < Self::MAX_ARITY {
            self.0 |= 1u64 << i;
        }
    }

    /// Whether `i` is present.
    #[must_use]
    pub const fn contains(&self, i: usize) -> bool {
        i < Self::MAX_ARITY && (self.0 >> i) & 1 == 1
    }

    /// How many indices are present.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.count_ones() as usize
    }

    /// Whether no index is present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Set union -- how a predicate's required-bound set accumulates over its
    /// rules.
    #[must_use]
    pub const fn union(&self, other: &Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Set intersection.
    #[must_use]
    pub const fn intersection(&self, other: &Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Whether every index here is also in `other`.
    #[must_use]
    pub const fn is_subset(&self, other: &Self) -> bool {
        self.0 & !other.0 == 0
    }

    /// The indices, ascending.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..Self::MAX_ARITY).filter(move |i| self.contains(*i))
    }

    /// The mode vector this set describes at `arity`.
    #[must_use]
    pub fn to_mode_vector(&self, arity: usize) -> ModeVector {
        ModeVector::new(
            (0..arity)
                .map(|i| {
                    if self.contains(i) {
                        Mode::Bound
                    } else {
                        Mode::Free
                    }
                })
                .collect(),
        )
    }
}

/// The binding pattern of one call site, as the magic-set rewrite consumes it.
///
/// Adornment lives here rather than in the validator or the evaluator because
/// both need it: mode inference computes it and the rewrite reads it, and a
/// type owned by either one would make those two crates depend on each other.
///
/// The adornment of every literal is *uniquely determined* by the source text,
/// because body literals are evaluated in author order. Fixing the literal
/// order fixes the sideways-information-passing strategy, which removes a
/// search from the engine as well as from the validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adornment {
    /// Which arguments are bound on entry to this literal.
    pub bound: ArgSet,
    /// The literal's arity, so the pattern can be rendered.
    pub arity: usize,
}

impl Adornment {
    /// The adornment binding exactly `bound` of `arity` arguments.
    #[must_use]
    pub const fn new(bound: ArgSet, arity: usize) -> Self {
        Self { bound, arity }
    }

    /// Whether any argument is bound. An all-free adornment needs no magic
    /// predicate: demand for it is unconditional.
    #[must_use]
    pub const fn is_adorned(&self) -> bool {
        !self.bound.is_empty()
    }

    /// The `bf` suffix the rewrite appends to a generated predicate's name.
    #[must_use]
    pub fn suffix(&self) -> String {
        (0..self.arity)
            .map(|i| if self.bound.contains(i) { 'b' } else { 'f' })
            .collect()
    }
}

impl fmt::Display for Adornment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.suffix())
    }
}

/// The adornments of one rule: its head and each body literal in order.
///
/// Carries its own [`RuleId`](crate::RuleId) rather than relying on position,
/// because the magic-set rewrite appends rules and a parallel-array contract
/// against the program would break the moment it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleAdornments {
    /// Which rule these describe.
    pub rule: crate::RuleId,
    /// The head's adornment, inherited from the call site that demanded it.
    pub head: Adornment,
    /// One entry per body literal, in author order.
    pub body: Vec<Adornment>,
}

/// The adornments of a top-level goal's body.
///
/// A goal has no head, so it has no inherited adornment -- but the rewrite
/// still needs its literal adornments, because the magic *seed* is derived
/// from them. Without this type the seed has nothing to be computed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalAdornments {
    /// One entry per goal literal, in author order.
    pub body: Vec<Adornment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_more_than_required_is_legal() {
        let v = ModeVector::new(vec![Mode::Bound, Mode::Free]);
        assert!(v.satisfied_by(&ArgSet::from_indices([0, 1])));
        assert!(v.satisfied_by(&ArgSet::from_indices([0])));
        assert!(!v.satisfied_by(&ArgSet::from_indices([1])));
    }

    #[test]
    fn mode_set_prefers_the_most_bound_satisfied_vector() {
        let set = ModeSet::new([ModeVector::all_free(1), ModeVector::all_bound(1)]);
        let chosen = set.select(&ArgSet::from_indices([0])).expect("satisfied");
        assert_eq!(chosen, &ModeVector::all_bound(1));
        let chosen = set.select(&ArgSet::empty()).expect("satisfied");
        assert_eq!(chosen, &ModeVector::all_free(1));
    }

    #[test]
    fn a_predicate_callable_both_ways_requires_nothing_always() {
        let set = ModeSet::new([ModeVector::all_free(1), ModeVector::all_bound(1)]);
        assert!(set.always_required().is_empty());
    }

    #[test]
    fn adornment_suffix_is_positional() {
        let a = Adornment::new(ArgSet::from_indices([0, 2]), 3);
        assert_eq!(a.suffix(), "bfb");
    }
}
