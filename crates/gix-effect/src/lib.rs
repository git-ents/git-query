//! `gix-effect`: the effect doc and define-time validation.
//!
//! `DEVPLAN-effect.md` Part B, Phase B1 -- "freeze the doc": [`EffectDoc`],
//! its store schema, and the validation `git effect define` runs before
//! writing anything. Nothing else: no trigger detection, no intent refs, no
//! gate evaluation, no `Executor` trait, no idempotence keys, no transition
//! log, no `Action` schema. Those are D3-D8, and they are later phases
//! (`DEVPLAN-effect.md`'s "Phases" table).
//!
//! What this crate calls rather than reimplements, per D9: rule-language
//! parsing and validation (`gix-query-parse`/`gix-query-check`, through the
//! `gix-query` facade), the acyclicity check (`gix_query::check_effect_acyclicity`,
//! C4), and typed-doc storage (`gix-store`). `Namespace` and `EffectDecl`
//! come from `gix-query-ir` via the facade; this crate does not redeclare
//! them.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod doc;

pub use doc::{EffectDoc, EffectStore, Error, Then};
