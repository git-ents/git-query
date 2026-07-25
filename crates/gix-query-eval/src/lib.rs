//! Evaluation: the magic-set rewrite, the Nemo lowering, the demand loop, and
//! derivation-tree recovery.
//!
//! The only crate permitted to name a [Nemo](https://github.com/knowsys/nemo)
//! type. Nemo is the engine and it is the only one -- there is no trait here
//! anticipating a second, because a seam with one implementation is a seam
//! whose only client is a test. See `DEVPLAN.md` 2.11.
//!
//! Everything above this crate is ours: the grammar, the validation passes,
//! the mode system, the rewrite, the demand loop, and the call log that
//! `explain` reads. What Nemo supplies is the least fixpoint of a stratified
//! program over a finite extensional database, and nothing else.
//!
//! # Holding the fragment
//!
//! Nemo's language is far larger than ours and refuses none of the parts we do
//! not want. Three things keep us inside it, in increasing order of how much
//! they should be relied on:
//!
//! 1. **Construction.** The IR has no term-level operations, so arithmetic and
//!    aggregates have no lowering path at all. A symbol cannot hold a `"`, so
//!    the emitter cannot produce the one byte Nemo's string lexer cannot
//!    carry. A lowered program can only be built from a
//!    [`gix_query_ir::CheckedProgram`].
//! 2. **The emission audit**, run before every execution: hand the emitted
//!    text back to Nemo's own parser and assert that the program it recovers
//!    has exactly the predicates, arities, and rule count we lowered. This
//!    catches the class of emitter bug types cannot -- a symbol that
//!    terminated a literal early, a name that collided, a rule that vanished.
//! 3. **The conformance suite**, pinned to the Nemo commit, asserting what we
//!    believe about the engine rather than about any one program. It is what
//!    the upgrade audit in `CONTRIBUTING.md` runs.
//!
//! # Three things this crate must never do
//!
//! Each was learned from the spike recorded in
//! `spikes/nemo-spike/spike-report.md`.
//!
//! - **Assert facts as program text.** Nemo's parser runs at roughly 109 KB/s;
//!   ten thousand facts cost 1.7 seconds that way and six milliseconds through
//!   an in-memory resource provider, for identical evaluation. Rules lower to
//!   a program, facts lower to tables, and there is no function from a fact to
//!   program text.
//! - **Escape a symbol.** Nemo's string literal lexer does no unescaping, so an
//!   emitted `\t` arrives as two characters. A `"` cannot be carried at all and
//!   is refused at construction rather than mangled at emission.
//! - **Paper over the gaps in Nemo's derivation trees.** Negated literals leave
//!   no node in them, and only one derivation comes back where several exist.
//!   Both are filled from our own rewrite, above the lowering.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod demand;
mod engine;
mod error;
#[cfg(test)]
mod golden;
mod lower;
mod rewrite;
mod symbol;
#[cfg(test)]
mod testutil;
mod trace;

pub use demand::{CallLog, LoggedCall, LoopResult, run_to_fixpoint};
pub use error::EvalError;
pub use lower::{NemoProgram, NemoTables};
pub use rewrite::{
    DemandKind, DemandSite, RewrittenProgram, adorn_body, rewrite as rewrite_program,
};
pub use symbol::Symbol;
pub use trace::{DerivationTree, NegationWitness, explain};
