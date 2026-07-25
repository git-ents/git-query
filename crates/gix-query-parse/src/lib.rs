//! Text to IR for the git-query Datalog dialect, plus the pretty-printer.
//!
//! # What lives here
//!
//! - [`parse_module`] and [`parse_goal`] -- the two entry points. A rule
//!   module gets `pub` declarations and rules; a goal gets a comma-separated
//!   literal list with no terminating period, which is the only syntactic
//!   difference between tier 3 and a rule body.
//! - [`goal_from_predicate`] and [`goal_from_subject`] -- the same [`Goal`]
//!   shape, built directly for tier 1 and tier 2 of the CLI, so that "one
//!   syntax, three tiers" means what it says: all three produce identical
//!   IR, never three independent encodings that happen to agree today.
//! - [`resolve_module`] and friends -- predicate resolution, reclassifying
//!   the builtins a [`gix_query_ir::Registry`] declares. The validator
//!   should not have to run its own rewrite pass before it can check
//!   anything, and neither should the canonicalizer, which is only defined
//!   on resolved IR.
//! - [`print_module`], [`print_rule`], [`print_goal`] -- the inverse of
//!   parsing, close enough that `parse_module(name, &print_module(&m))`
//!   reconstructs `m`'s rules and `pub` surface exactly.
//!
//! # Diagnostics
//!
//! Every entry point returns `Result<_, `[`Errors`]`>`, a `Vec` rather than
//! a single error: the parser resynchronizes after a syntax error -- to the
//! next `.` in a module, to the next `,` in a goal -- so one run reports
//! every mistake it can find rather than stopping at the first.
//!
//! [`Goal`]: gix_query_ir::Goal

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod desugar;
mod error;
mod lexer;
mod parser;
mod print;
mod resolve;
mod token;

pub use desugar::{goal_from_predicate, goal_from_subject};
pub use error::{Errors, ParseError};
pub use parser::{parse_goal, parse_goal_with_select, parse_module};
pub use print::{print_goal, print_module, print_rule};
pub use resolve::{resolve_body, resolve_goal, resolve_module, resolve_rule};
