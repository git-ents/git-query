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
//! [`RepoBuiltins`] answers the two projection builtins, `bind` and
//! `bind_fuzzy`. They are separate from [`RepoFacts`] because they are
//! separate to the validator too: a builtin is called with bound arguments
//! inside the fixpoint and may mint values, an EDB relation is read, and the
//! minting-confinement pass exists to keep those two apart.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod anchor;
mod author;
mod bind;
mod claim;
mod commit;
mod facts;
mod line;
mod member;
mod tree;
mod util;

pub use bind::RepoBuiltins;
pub use facts::RepoFacts;
