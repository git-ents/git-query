//! Rule-module storage, whole-program assembly, and the cache key.
//!
//! Modules are stored as source text, one ref per module, under
//! [`RuleStore`]'s `refs/meta/rules/<name>` namespace. The active program is
//! never any single module: it is the union of everything currently
//! published, assembled and validated together by [`assemble`] every time a
//! caller needs it. See [`assemble`]'s own documentation for why validation
//! lives at that call rather than at [`RuleStore::put`].
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod assemble;
mod error;
mod store;

pub use assemble::assemble;
pub use error::Error;
pub use store::RuleStore;
