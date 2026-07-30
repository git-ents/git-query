//! `gix-effect`: the effect doc, trigger detection, intents, idempotence keys,
//! and the transition log.
//!
//! `DEVPLAN-effect.md` Part B:
//!
//! - **Phase B1** -- "freeze the doc": [`EffectDoc`], its store schema, and
//!   the validation `git effect define` runs before writing anything
//!   ([`doc`]).
//! - **Phase B2** -- delta-restricted trigger re-evaluation and intent refs
//!   (D3, D4, [`trigger`]); idempotence keys and the signed transition log
//!   (D5, D8, [`record`]).
//!
//! Still not here, per `DEVPLAN-effect.md`'s "Phases" table: the `Executor`
//! trait, the write-set boundary check, the `Action` schema, gate evaluation,
//! and the authorization gate (D6, D7) -- Phase B3, which ends with worked
//! example 3 as an integration test.
//!
//! What this crate calls rather than reimplements, per D9: rule-language
//! parsing and validation (`gix-query-parse`/`gix-query-check`, through the
//! `gix-query` facade), the acyclicity check and footprint-to-namespace
//! projection (`gix_query::check_effect_acyclicity`/`body_read_namespaces`,
//! C4), evaluation (`gix_query::run_goal`/`run_body`, Nemo through the
//! facade -- never `gix-query-eval` directly), and typed-doc storage, ref CAS,
//! and hashing/normal form (`gix-store`/`facet-git-tree`). `Namespace` and
//! `EffectDecl` come from `gix-query-ir` via the facade; this crate does not
//! redeclare them.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod doc;
mod record;
mod trigger;

pub use doc::{EffectDoc, EffectStore, Error, Then};
pub use record::{
    ACTION_KEY_SEAM, EditRecord, Outcome, RecordError, RecordOutcome, RecordStore,
    TransitionRecord, advance_key,
};
pub use trigger::{
    Firing, Intent, IntentError, IntentStore, Transition, TriggerDetector, TriggerError,
};
