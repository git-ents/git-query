//! The engine `git-query` ships.
//!
//! Two modes behind one implementation. The semi-naive one is what runs; the
//! round-based one is a few dozen lines, correct by inspection, and exists so
//! that the fast one has something to be wrong against. Delta management
//! across strata, negation evaluated against a completed stratum, and index
//! maintenance under insertion all fail by producing a *wrong answer* rather
//! than a crash, and a wrong answer in a gate is the failure mode this design
//! exists to prevent.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
