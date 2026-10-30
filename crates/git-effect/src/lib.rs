//! `git-effect`: the effect doc, trigger detection, intents, idempotence keys,
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
//! - **Phase B3** -- the `Executor` trait, the write-set boundary check, the
//!   `Action` schema, and the authorization gate (D6, D7, [`record`],
//!   [`run`]), plus `status`/`log` in the `git-effect` binary.
//!
//! What this crate calls rather than reimplements, per D9: rule-language
//! parsing and validation (`gix-query-parse`/`gix-query-check`, through the
//! `git-query` facade), the acyclicity check and footprint-to-namespace
//! projection (`git_query::check_effect_acyclicity`/`body_read_namespaces`,
//! C4), evaluation (`git_query::run_goal`/`run_body`, Nemo through the
//! facade -- never `gix-query-eval` directly), typed-doc storage, ref CAS,
//! and hashing/normal form (`git-store`/`facet-git-tree`), and the claim
//! envelope, signing, and verification (`git-attest`, called for
//! authorization and action-record emission -- never re-implemented here).
//! `Namespace` and `EffectDecl` come from `gix-query-ir` via the facade; this
//! crate does not redeclare them.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod doc;
mod record;
mod run;
mod trigger;

pub use doc::{EffectDoc, EffectStore, Error, Then};
pub use record::{
    ACTION_KIND, ACTION_TARGET_KIND, Action, ActionKey, EditRecord, Outcome, Param, RecordError,
    RecordOutcome, RecordStore, TransitionRecord, action_key, advance_key, compile_action,
    register_action_schema,
};
pub use run::{
    BoundaryError, ExecError, Executor, RunError, action_target, authorized, check_boundary,
    fire_advance, fire_run, record_action,
};
pub use trigger::{
    Firing, Intent, IntentError, IntentStore, Transition, TriggerDetector, TriggerError,
};
