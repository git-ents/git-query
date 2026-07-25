//! Scratch crate for the Nemo evaluation described in
//! `nemo-spike-and-integration.md`.
//!
//! Nothing here is meant to survive. It exists so that each gate's verdict in
//! `spike-report.md` cites a test that ran rather than a paragraph that
//! sounded plausible. The git builtins are stubbed to hardcoded tables behind
//! the trait the real ones would implement; no repository is opened.

pub mod builtin;
pub mod demand;
pub mod runner;
pub mod value;

pub use runner::{FactRoute, Runner, Session};
pub use value::{Tuple, Value};
