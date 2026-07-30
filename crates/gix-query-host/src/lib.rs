//! Repo-backed fact sources and the projection builtins.
//!
//! This is the only crate in the workspace that depends on `gix` and
//! `gix-anchor`: it implements the [`gix_query_ir::FactSource`] and
//! [`gix_query_ir::Builtin`] traits `gix-query-ir` declares, so that
//! everything upstream of here -- the parser, the validator, the evaluator --
//! stays property-testable over synthetic facts with no repository at all.
//!
//! [`RepoFacts`] answers every repo-backed EDB relation
//! `gix_query_ir::host_predicates()` declares. Each EDB predicate's storage
//! layout is documented on the internal module that owns it -- `commit`,
//! `tree`, `author`, `member`, `claim`, `anchor`, `line` -- next to the code
//! reading it, so the two cannot drift the way a layout documented only in
//! prose could.
//!
//! [`RepoBuiltins`] answers the one projection builtin, `cand` -- every
//! candidate location an anchor might sit at on a revision, unselected.
//! `bind/5` itself, the selection over `cand`, is a rule
//! (`gix-query-rules::assemble`'s always-present core module), not a
//! builtin. `cand` is separate from [`RepoFacts`] because it is separate to
//! the validator too: a builtin is called with bound arguments inside the
//! fixpoint and may mint values, an EDB relation is read, and the
//! minting-confinement pass exists to keep those two apart.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod anchor;
mod author;
mod builtins;
mod cand;
mod claim;
mod commit;
mod facts;
mod line;
mod member;
mod refs;
mod tree;
mod util;

pub use builtins::AllBuiltins;
pub use cand::RepoBuiltins;
pub use facts::RepoFacts;
