use std::fmt;
use std::sync::{Arc, LazyLock};

use crate::{Span, SpannedTerm, Term, Var};

/// A predicate name and arity. Names are qualified (`review.reviewed`) once a
/// module has been assembled; a module's own predicates are unqualified in its
/// source text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PredicateKey {
    /// The predicate's name.
    pub name: Arc<str>,
    /// Its arity. Two predicates with the same name and different arities are
    /// different predicates.
    pub arity: usize,
}

impl PredicateKey {
    /// The key for `name/arity`.
    pub fn new(name: impl Into<Arc<str>>, arity: usize) -> Self {
        Self {
            name: name.into(),
            arity,
        }
    }

    /// The module qualifier, for a qualified name.
    #[must_use]
    pub fn module(&self) -> Option<&str> {
        self.name.split_once('.').map(|(m, _)| m)
    }

    /// The name with any module qualifier stripped.
    #[must_use]
    pub fn local_name(&self) -> &str {
        self.name.split_once('.').map_or(&*self.name, |(_, n)| n)
    }

    /// The prefix reserved for relations the magic-set rewrite generates.
    ///
    /// Reserved here, in the IR, because two crates depend on the same answer:
    /// the evaluator mints these names, and the validator must recognize them
    /// to re-run stratification on the rewritten program. Two independent
    /// mangling schemes would disagree silently. A rule module defining a
    /// `magic_`-prefixed predicate would capture an engine relation and corrupt
    /// demand propagation, so the parser rejects the prefix outright.
    pub const MAGIC_PREFIX: &'static str = "magic_";

    /// The demand relation for calling `self` with `adornment`.
    ///
    /// Arity is the number of bound arguments: demand carries the values the
    /// caller supplied, and nothing else.
    #[must_use]
    pub fn magic(&self, adornment: &crate::Adornment) -> Self {
        Self::new(
            format!("{}{}_{}", Self::MAGIC_PREFIX, self.name, adornment.suffix()),
            adornment.bound.len(),
        )
    }

    /// Whether this names an engine-generated demand relation, and therefore
    /// must never appear in query output or in a derivation tree.
    #[must_use]
    pub fn is_magic(&self) -> bool {
        self.local_name().starts_with(Self::MAGIC_PREFIX)
    }
}

impl fmt::Display for PredicateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.name, self.arity)
    }
}

/// A predicate applied to argument terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    /// Which predicate.
    pub predicate: PredicateKey,
    /// Its arguments, in signature order. Length always equals the key's arity.
    pub terms: Vec<SpannedTerm>,
    /// Where it was written.
    pub span: Span,
}

impl Atom {
    /// An atom over `terms`, with no source location.
    pub fn new(name: impl Into<Arc<str>>, terms: Vec<Term>) -> Self {
        let predicate = PredicateKey::new(name, terms.len());
        Self {
            predicate,
            terms: terms.into_iter().map(SpannedTerm::synthetic).collect(),
            span: Span::synthetic(),
        }
    }

    /// The argument terms, without their spans.
    pub fn args(&self) -> impl Iterator<Item = &Term> {
        self.terms.iter().map(|t| &t.term)
    }

    /// Every named variable occurring in this atom, with repeats.
    pub fn vars(&self) -> impl Iterator<Item = &Var> {
        self.terms.iter().filter_map(|t| t.term.as_var())
    }
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.predicate.name)?;
        for (i, t) in self.terms.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}", t.term)?;
        }
        f.write_str(")")
    }
}

/// A comparison operator.
///
/// Comparisons are registered builtins -- their properties live in the same
/// table as every other builtin's, and they are the only place `!=` and `<=`
/// are defined. They keep their own literal form because they are infix in
/// source text and because range restriction special-cases them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CompareOp {
    /// `=` -- a *filter* on two ground values, never a unification. A
    /// Prolog-shaped syntax invites the opposite reading, and unification
    /// would reintroduce value invention through the back door.
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl CompareOp {
    /// The operator's source spelling, which is also its registry name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
        }
    }

    /// Every operator, in registry order.
    #[must_use]
    pub const fn all() -> [Self; 6] {
        [Self::Eq, Self::Ne, Self::Lt, Self::Le, Self::Gt, Self::Ge]
    }

    /// Whether the operator orders its operands, and is therefore restricted
    /// to integers. Ordering an object id or a member name would let a rule's
    /// meaning depend on hash bytes.
    #[must_use]
    pub const fn is_ordering(self) -> bool {
        matches!(self, Self::Lt | Self::Le | Self::Gt | Self::Ge)
    }

    /// This operator's registry key, borrowed rather than built.
    ///
    /// A comparison literal stores an operator, not a key, so asking for its
    /// key would otherwise allocate -- and comparisons are the hottest
    /// literals in the language, evaluated once per candidate line per span.
    #[must_use]
    pub fn key(self) -> &'static PredicateKey {
        static KEYS: LazyLock<[PredicateKey; 6]> =
            LazyLock::new(|| CompareOp::all().map(|op| PredicateKey::new(op.as_str(), 2)));
        let i = Self::all()
            .iter()
            .position(|o| *o == self)
            .expect("every operator is in `all`");
        &KEYS[i]
    }
}

impl fmt::Display for CompareOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A comparison between two terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compare {
    /// The operator.
    pub op: CompareOp,
    /// Left operand.
    pub left: SpannedTerm,
    /// Right operand.
    pub right: SpannedTerm,
    /// Where it was written.
    pub span: Span,
}

/// A body literal. Four forms and no fifth.
///
/// [`Literal::Positive`] and [`Literal::Builtin`] are distinguished by the
/// registry, not by syntax: the parser emits `Positive` for every applied
/// predicate, and predicate resolution reclassifies the ones the registry
/// declares as builtins. Downstream sees one type either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// `p(X, Y)` -- a positive goal over an EDB or IDB predicate.
    Positive(Atom),
    /// `!p(X, Y)` -- a negated goal. Every variable it mentions must be bound
    /// by a positive literal, and it may not close a cycle in the dependency
    /// graph.
    Negated(Atom),
    /// `X != Y` -- a filter over two already-bound terms.
    Compare(Compare),
    /// `bind(A, Rev, B, S, E, Pos, Con)` -- a moded, non-enumerable call whose
    /// bound arguments must be bound at this point in the body.
    Builtin(Atom),
}

impl Literal {
    /// The atom this literal applies, or `None` for a comparison.
    #[must_use]
    pub fn atom(&self) -> Option<&Atom> {
        match self {
            Self::Positive(a) | Self::Negated(a) | Self::Builtin(a) => Some(a),
            Self::Compare(_) => None,
        }
    }

    /// The atom this literal applies, mutably.
    pub fn atom_mut(&mut self) -> Option<&mut Atom> {
        match self {
            Self::Positive(a) | Self::Negated(a) | Self::Builtin(a) => Some(a),
            Self::Compare(_) => None,
        }
    }

    /// The predicate this literal calls. A comparison's key is its operator
    /// spelling at arity 2, which is exactly how the registry names it.
    ///
    /// Borrowed, because this is called per literal per iteration inside the
    /// fixpoint and cloning an `Arc<str>` there is a refcount bounce for
    /// nothing.
    #[must_use]
    pub fn key(&self) -> &PredicateKey {
        match self {
            Self::Positive(a) | Self::Negated(a) | Self::Builtin(a) => &a.predicate,
            Self::Compare(c) => c.op.key(),
        }
    }

    /// Whether this literal is negated.
    #[must_use]
    pub fn is_negated(&self) -> bool {
        matches!(self, Self::Negated(_))
    }

    /// Whether this literal can bind a variable that was free before it.
    ///
    /// Negated literals and comparisons cannot: both are filters, so range
    /// restriction requires every variable they mention to be bound already.
    #[must_use]
    pub fn is_generator(&self) -> bool {
        matches!(self, Self::Positive(_) | Self::Builtin(_))
    }

    /// The literal's argument terms, in order. A comparison's are its
    /// operands.
    ///
    /// Allocation-free: range restriction walks this once per literal, and the
    /// evaluator walks it once per literal per iteration.
    pub fn terms(&self) -> impl Iterator<Item = &Term> {
        let (atom, compare) = match self {
            Self::Positive(a) | Self::Negated(a) | Self::Builtin(a) => (Some(a.terms.iter()), None),
            Self::Compare(c) => (None, Some([&c.left.term, &c.right.term].into_iter())),
        };
        atom.into_iter()
            .flatten()
            .map(|t| &t.term)
            .chain(compare.into_iter().flatten())
    }

    /// Every named variable occurring in this literal, with repeats.
    pub fn vars(&self) -> impl Iterator<Item = &Var> {
        self.terms().filter_map(Term::as_var)
    }

    /// Where it was written.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Positive(a) | Self::Negated(a) | Self::Builtin(a) => a.span,
            Self::Compare(c) => c.span,
        }
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Positive(a) | Self::Builtin(a) => write!(f, "{a}"),
            Self::Negated(a) => write!(f, "!{a}"),
            Self::Compare(c) => write!(f, "{} {} {}", c.left.term, c.op, c.right.term),
        }
    }
}
