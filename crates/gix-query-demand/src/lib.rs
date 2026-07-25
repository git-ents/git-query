//! Demand: the magic-set rewrite, and the loop that answers what it asks for.
//!
//! Evaluation is bottom-up and host predicates only answer bound calls, so the
//! two do not meet without a loop: run to fixpoint, read which calls the
//! rewritten program asked for, answer them, feed the answers back as ordinary
//! facts, run again. It stops when a round asks for nothing it has not already
//! answered.
//!
//! The call log this leaves behind is not a debugging aid. It is what `explain`
//! joins against to say which mode a builtin was called in, and what a
//! footprint audit reads to say which refs an answer depended on -- neither of
//! which any engine's own trace can supply.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
