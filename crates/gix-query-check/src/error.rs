//! Diagnostics for the eight validation passes that survive `DEVPLAN.md` §0
//! (pass 6, shape caps, was a lint rather than a soundness pass and is
//! retired).
//!
//! One [`CheckError`] enum rather than one per pass, because a caller
//! collecting `Errors` from one run should not have to match on several
//! different types to print them. [`CheckError::pass`] is what keeps that
//! convenient without losing precision: every variant answers "which pass
//! fired" exhaustively, so adding a pass without adding an arm here is a
//! compile error.

use std::fmt;

use std::sync::Arc;

use gix_query_ir::{Namespace, PredicateKey, RuleId, Span, Var};

/// Which of the eight surviving validation passes produced a diagnostic.
///
/// Spelled out rather than left implicit in prose, because the test
/// obligation is that every reject fixture names the pass that fired --
/// `assert_eq!(err.pass(), Pass::Modes)`, not a string match on a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pass {
    /// 1. No value invention.
    NoValueInvention,
    /// 2. Predicate resolution.
    Resolution,
    /// 3. Arity and types.
    Types,
    /// 4. Range restriction.
    RangeRestriction,
    /// 5. Stratification.
    Stratification,
    /// 7. Footprint.
    ///
    /// Pass 6 (shape caps) is retired (`DEVPLAN.md` §0), so the numbering
    /// here has a gap rather than a renumbering.
    Footprint,
    /// 8. Mode inference.
    Modes,
    /// 9. Minting confinement.
    MintingConfinement,
    /// Effect stratification (numbered 10, after the original nine) -- the
    /// fork-bomb-freedom check `ARCHITECTURE.md`'s git-effect section
    /// describes: no cycle in the graph an effect's declared write set
    /// induces over another effect's *computed* read set -- pass 7's footprint
    /// mapped to ref namespaces, as `docs/query-and-rules.adoc`'s "Triggers"
    /// section specifies, because a declared read set can lie. Not one of the
    /// original nine `docs/query-and-rules.adoc` numbers; it runs after them,
    /// over effect declarations rather than rules.
    EffectStratification,
}

impl Pass {
    /// The pass number as the spec numbers it, for messages that cite "pass
    /// N" the way `docs/query-and-rules.adoc` does. Skips 6 -- shape caps is
    /// retired, not renumbered.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::NoValueInvention => 1,
            Self::Resolution => 2,
            Self::Types => 3,
            Self::RangeRestriction => 4,
            Self::Stratification => 5,
            Self::Footprint => 7,
            Self::Modes => 8,
            Self::MintingConfinement => 9,
            Self::EffectStratification => 10,
        }
    }
}

impl fmt::Display for Pass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::NoValueInvention => "no value invention",
            Self::Resolution => "predicate resolution",
            Self::Types => "arity and types",
            Self::RangeRestriction => "range restriction",
            Self::Stratification => "stratification",
            Self::Footprint => "footprint",
            Self::Modes => "mode inference",
            Self::MintingConfinement => "minting confinement",
            Self::EffectStratification => "effect stratification",
        };
        write!(f, "pass {} ({name})", self.number())
    }
}

/// Where in a program a literal that produced a diagnostic lives: a rule
/// body, or a standalone goal.
///
/// A goal has no [`RuleId`] -- it is not stored in the [`gix_query_ir::Program`]
/// -- so this cannot be `Option<RuleId>` without losing the ability to say
/// *which* goal literal failed when several goals are checked in one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// A body literal of a stored rule.
    Rule(RuleId),
    /// A body literal of a goal passed to [`crate::check_goal`].
    Goal,
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rule(id) => write!(f, "rule {id}"),
            Self::Goal => f.write_str("the goal"),
        }
    }
}

/// One validation failure, located in the source text that produced it.
///
/// Every variant names the predicate and, where the pass is about a
/// variable, the variable -- "mode error" is not an acceptable message on
/// its own, per the house style these diagnostics are held to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    /// A symbol constant contains a byte Nemo's string lexer cannot carry.
    ///
    /// Nothing on the lowering side unescapes, so encoding the byte instead
    /// of rejecting it would corrupt the constant silently the first time it
    /// reached the engine. Refusing it here, with a span, is strictly better
    /// than a lowering-time panic.
    #[error(
        "the constant {value:?} passed to {predicate} at argument {argument} contains a `\"`, \
         which cannot be represented in a rule constant; rename the value rather than escaping it"
    )]
    QuotedSymbol {
        /// The predicate the constant was passed to.
        predicate: PredicateKey,
        /// Which argument.
        argument: usize,
        /// The offending text.
        value: String,
        /// Where.
        span: Span,
    },
    /// A symbol constant contains a NUL byte, which would let it forge a
    /// field boundary in the canonical form.
    #[error(
        "the constant {value:?} passed to {predicate} at argument {argument} contains a NUL byte"
    )]
    NulSymbol {
        /// The predicate the constant was passed to.
        predicate: PredicateKey,
        /// Which argument.
        argument: usize,
        /// The offending text.
        value: String,
        /// Where.
        span: Span,
    },
    /// A rule module tried to define a predicate the host registry already
    /// declares -- a base predicate or a builtin.
    #[error("{key} is a base predicate and cannot be redefined by a rule")]
    NotRedefinable {
        /// The predicate.
        key: PredicateKey,
    },
    /// A defined predicate's arity exceeds what an [`gix_query_ir::ArgSet`]
    /// can describe, which the shape caps must therefore also refuse.
    #[error("{key} exceeds the maximum arity of {max}")]
    PredicateArityLimit {
        /// The predicate.
        key: PredicateKey,
        /// The limit.
        max: usize,
    },
    /// A body literal names a predicate that is neither a host EDB
    /// predicate, a builtin, nor defined anywhere in the program.
    #[error(
        "{key} in {site} is not defined: not a host predicate, not a builtin, and no rule defines it"
    )]
    UnresolvedPredicate {
        /// The predicate that failed to resolve.
        key: PredicateKey,
        /// Where it was called.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A body literal names a predicate under a name the registry knows, but
    /// at a different arity.
    #[error("{key} in {site} does not exist; {} is registered at arity {found} instead", key.name)]
    ArityMismatch {
        /// The arity the call used.
        key: PredicateKey,
        /// The arity actually registered.
        found: usize,
        /// Where it was called.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A negated literal names a builtin. Only positive builtin calls and
    /// the comparison operators are meaningful; `!bind(...)` has no defined
    /// semantics, because a builtin is not a materialized relation to test
    /// absence against.
    #[error("{key} is a builtin and cannot be negated, in {site}")]
    NegatedBuiltin {
        /// The builtin.
        key: PredicateKey,
        /// Where it was called.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A predicate defined by rules alone, with no module boundary, was
    /// referenced from a module other than the one that defines it, without
    /// being declared `pub`.
    #[error(
        "{key} is private to module {owner:?} and cannot be referenced from {referrer:?}, in {site}"
    )]
    CrossModulePrivate {
        /// The predicate referenced.
        key: PredicateKey,
        /// The module that defines it.
        owner: String,
        /// The module that referenced it.
        referrer: String,
        /// Where it was called.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A constant's runtime representation (integer or symbol) does not
    /// match the type its argument position declares.
    #[error("{predicate} argument {argument} is declared {expected}, but {value:?} is not one")]
    ConstTypeMismatch {
        /// The predicate.
        predicate: PredicateKey,
        /// Which argument.
        argument: usize,
        /// The declared type.
        expected: gix_query_ir::ValueType,
        /// The offending constant, rendered.
        value: String,
        /// Where.
        span: Span,
    },
    /// A constant falls outside a closed-domain type's admissible values.
    #[error(
        "{predicate} argument {argument} is declared {expected}, whose values are {domain:?}; \
         {value:?} is not among them"
    )]
    DomainViolation {
        /// The predicate.
        predicate: PredicateKey,
        /// Which argument.
        argument: usize,
        /// The declared type.
        expected: gix_query_ir::ValueType,
        /// The closed domain.
        domain: &'static [&'static str],
        /// The offending constant.
        value: String,
        /// Where.
        span: Span,
    },
    /// Two call sites infer different types for the same argument position
    /// of a predicate whose types are otherwise left to inference.
    #[error(
        "{predicate} argument {argument} is inferred as both {first} and {second} by different call sites"
    )]
    PredicateTypeConflict {
        /// The predicate.
        predicate: PredicateKey,
        /// Which argument.
        argument: usize,
        /// The first type inferred.
        first: gix_query_ir::ValueType,
        /// The conflicting second type.
        second: gix_query_ir::ValueType,
        /// Where the second, conflicting call site was.
        span: Span,
    },
    /// One variable is used at two argument positions whose types conflict,
    /// within one rule.
    #[error("variable {var} is used as both {first} and {second} in {site}")]
    VariableTypeConflict {
        /// The variable.
        var: Var,
        /// The first type it was constrained to.
        first: gix_query_ir::ValueType,
        /// The conflicting second type.
        second: gix_query_ir::ValueType,
        /// Where.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A head variable does not appear in any positive body literal.
    #[error("{var} appears in the head of {site} but is not bound by any positive body literal")]
    UnrestrictedHead {
        /// The variable.
        var: Var,
        /// The rule or goal.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A variable under negation does not appear in any positive body
    /// literal earlier in the same body.
    #[error("{var} appears under negation in {site} but is not bound by any positive body literal")]
    UnrestrictedNegation {
        /// The variable.
        var: Var,
        /// The rule or goal.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A variable in a comparison does not appear in any positive body
    /// literal.
    #[error(
        "{var} appears in a comparison in {site} but is not bound by any positive body literal"
    )]
    UnrestrictedCompare {
        /// The variable.
        var: Var,
        /// The rule or goal.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A negated literal closes a cycle: it depends, through zero or more
    /// positive edges, on the predicate it negates being fully computed
    /// first, and that predicate depends back on it.
    #[error("{negated} is negated in a rule for {dependent}, but the two are mutually recursive")]
    UnstratifiableNegation {
        /// The predicate doing the negating.
        dependent: PredicateKey,
        /// The predicate negated.
        negated: PredicateKey,
        /// The rule containing the negation.
        rule: RuleId,
        /// Where.
        span: Span,
    },
    /// A required-bound argument is not bound at the point in the body
    /// where the literal calling for it appears.
    ///
    /// Checking is left to right over the body as written: there is no
    /// reordering and no search, so this names both the variable that is
    /// missing and the literal that needed it, exactly as written.
    #[error(
        "{predicate} requires argument {argument} bound, but {} is not bound at that point in {site} \
         -- checking is left to right, so an earlier literal must bind it",
        var.as_ref().map_or_else(|| "_".to_owned(), std::string::ToString::to_string)
    )]
    ModeViolation {
        /// The predicate called.
        predicate: PredicateKey,
        /// Which argument.
        argument: usize,
        /// The unbound variable, or `None` when the offending term was `_`.
        var: Option<Var>,
        /// The rule or goal.
        site: Site,
        /// Where.
        span: Span,
    },
    /// An EDB call's bound arguments satisfy no mode vector the registry
    /// declares for that predicate.
    #[error(
        "{predicate} supports no mode vector satisfied by binding argument(s) {bound:?} in {site}; \
         see `git query predicates` for the vectors it supports"
    )]
    UnsupportedEdbMode {
        /// The predicate called.
        predicate: PredicateKey,
        /// Which argument positions were bound at the call.
        bound: Vec<usize>,
        /// The rule or goal.
        site: Site,
        /// Where.
        span: Span,
    },
    /// A builtin declared `mints` appears in a recursive SCC, which would
    /// let it mint an unbounded number of fresh values across iterations of
    /// the fixpoint.
    #[error(
        "{builtin} mints values and cannot appear in {predicate}, which is in a recursive \
         stratum through {rule}"
    )]
    MintingInRecursion {
        /// The minting builtin.
        builtin: PredicateKey,
        /// The predicate whose recursion it appears inside.
        predicate: PredicateKey,
        /// The rule containing the call.
        rule: RuleId,
        /// Where.
        span: Span,
    },
    /// An effect's declared write set overlaps a namespace another effect's
    /// body is computed to read, and that second effect's writes lead --
    /// through zero or more further effects -- back to a namespace the first
    /// effect reads. This is the fork bomb: a signed transition that can
    /// trigger a chain of effects which eventually re-triggers itself.
    #[error(
        "effect {from} writes {namespace}, which effect {to}'s body reads, closing a cycle in \
         the effect graph"
    )]
    EffectCycle {
        /// The effect whose write closes the cycle.
        from: Arc<str>,
        /// The effect whose read the write overlaps.
        to: Arc<str>,
        /// The namespace the two declarations share.
        namespace: Namespace,
        /// Where `from` was declared.
        span: Span,
    },
}

impl CheckError {
    /// Which pass produced this diagnostic. Exhaustive, so a new variant
    /// forces a decision here rather than silently falling through to a
    /// default.
    #[must_use]
    pub const fn pass(&self) -> Pass {
        match self {
            Self::QuotedSymbol { .. } | Self::NulSymbol { .. } => Pass::NoValueInvention,
            Self::NotRedefinable { .. }
            | Self::PredicateArityLimit { .. }
            | Self::UnresolvedPredicate { .. }
            | Self::ArityMismatch { .. }
            | Self::NegatedBuiltin { .. }
            | Self::CrossModulePrivate { .. } => Pass::Resolution,
            Self::ConstTypeMismatch { .. }
            | Self::DomainViolation { .. }
            | Self::PredicateTypeConflict { .. }
            | Self::VariableTypeConflict { .. } => Pass::Types,
            Self::UnrestrictedHead { .. }
            | Self::UnrestrictedNegation { .. }
            | Self::UnrestrictedCompare { .. } => Pass::RangeRestriction,
            Self::UnstratifiableNegation { .. } => Pass::Stratification,
            Self::ModeViolation { .. } | Self::UnsupportedEdbMode { .. } => Pass::Modes,
            Self::MintingInRecursion { .. } => Pass::MintingConfinement,
            Self::EffectCycle { .. } => Pass::EffectStratification,
        }
    }

    /// Where the offending text is, for a caller mapping to line and column.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::QuotedSymbol { span, .. }
            | Self::NulSymbol { span, .. }
            | Self::UnresolvedPredicate { span, .. }
            | Self::ArityMismatch { span, .. }
            | Self::NegatedBuiltin { span, .. }
            | Self::CrossModulePrivate { span, .. }
            | Self::ConstTypeMismatch { span, .. }
            | Self::DomainViolation { span, .. }
            | Self::PredicateTypeConflict { span, .. }
            | Self::VariableTypeConflict { span, .. }
            | Self::UnrestrictedHead { span, .. }
            | Self::UnrestrictedNegation { span, .. }
            | Self::UnrestrictedCompare { span, .. }
            | Self::UnstratifiableNegation { span, .. }
            | Self::ModeViolation { span, .. }
            | Self::UnsupportedEdbMode { span, .. }
            | Self::MintingInRecursion { span, .. }
            | Self::EffectCycle { span, .. } => *span,
            Self::NotRedefinable { .. } | Self::PredicateArityLimit { .. } => Span::synthetic(),
        }
    }
}

/// The validation failures from one run. A `Vec` rather than a single error,
/// matching how `gix-query-parse` already reports: one run should surface
/// every problem it can find, not stop at the first.
pub type Errors = Vec<CheckError>;
