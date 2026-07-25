//! The seam a fixpoint engine is reached through.
//!
//! Nothing above this trait knows which engine ran, and nothing below it knows
//! what the relations mean. That is the whole point: the grammar, the
//! validation passes, the mode system, the magic-set rewrite and the demand
//! loop are ours in every design, and what sits underneath is only "compute
//! the least fixpoint of a stratified program over a finite extensional
//! database".
#![forbid(unsafe_code)]
#![deny(missing_docs)]
