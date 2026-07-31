//! The intermediate representation every other `git-query` crate codes
//! against: terms, literals, rules, programs, the signature registry, the
//! builtin property table, mode and adornment types, canonicalization, and the
//! two traits the engine reaches the outside world through.
//!
//! This crate is the seam, and it is frozen before anything downstream is
//! written -- an IR change after that point invalidates the parser, the
//! validator, and the evaluator simultaneously.
//!
//! # What lives here, and why
//!
//! - **No compound terms.** [`Term`] has three forms and there is no fourth.
//!   A constructed term is destructuring, destructuring is the thin end of
//!   unification, and unification is the thin end of function symbols. The
//!   no-value-invention pass is worth having precisely because it is a
//!   syntactic check with no carve-outs, and a `Compound` variant here would
//!   be the carve-out.
//! - **One required-bound [`ArgSet`] per predicate, host or derived.**
//!   `DEVPLAN.md` §0's narrow cut made this exact: eager preload, not a second
//!   legal mode vector, is what makes `member/1` callable both free and
//!   bound, so an EDB predicate's declared mode collapses to the one thing
//!   that still varies -- which positions a caller must supply.
//! - **Backing sources beside signatures.** See [`Backing`]. This is what turns
//!   the footprint pass from a list of predicate names into a list of ref
//!   globs, which is what makes the cache key sound -- and it is registry
//!   *data*, so the validator needs no git dependency to read it.
//! - **Adornment.** See [`Adornment`]. Mode inference computes it and the
//!   magic-set rewrite consumes it; a type owned by either crate would make
//!   those two depend on each other.
//! - **[`FactSource`] and [`Builtin`].** Defined here, implemented in the host
//!   crate, so the evaluator never depends on `gix` and stays property-testable
//!   over synthetic facts with no repository at all.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod canonical;
mod checked;
mod effect;
mod error;
mod host;
mod literal;
mod mode;
mod registry;
mod rule;
mod span;
mod term;

pub use canonical::Canonicalizer;
pub use checked::{CheckedProgram, Footprint, Stratification};
pub use effect::{EffectDecl, Namespace};
pub use error::Error;
pub use host::{Bindings, Builtin, Comparisons, Fact, FactSource, Facts, HostError};
pub use literal::{Atom, Compare, CompareOp, Literal, PredicateKey};
pub use mode::{Adornment, ArgSet, GoalAdornments, Mode, ModeVector, RuleAdornments};
pub use registry::{
    ArgType, Backing, BuiltinEntry, BuiltinProps, PredicateClass, PredicateEntry, Registry,
    TypeSlot, ValueType, Visibility, builtins, host_predicates,
};
pub use rule::{Export, Goal, Module, Program, RewriteKind, Rule, RuleId, RuleOrigin};
pub use span::Span;
pub use term::{SpannedTerm, Term, Value, Var};
