//! Idempotence keys (D5) and the transition log (D8).
//!
//! `DEVPLAN-effect.md` D5: a pure advance's idempotence key is
//! `normal_form::hash({effect: <doc tree hash>, trigger: {ref, old, new}})`,
//! through `facet-git-tree`'s frozen normal form -- "there is no key crate."
//! D8: each performed transition appends a typed doc `{effect, trigger, key,
//! edits, outcome}` to `refs/effects/log`, signed via store's `Signer` seam
//! by the executing identity.
//!
//! What this module does *not* do: decide whether a transition should
//! happen. That is gate evaluation and the authorization gate (D6/D7,
//! `Executor`, the write-set boundary check) -- Phase B3. This module is the
//! record of a transition already decided and performed, which is exactly
//! what makes at-least-once delivery from hooks safe: a caller that computes
//! the same key twice (a hook retried after a timeout, say) finds the first
//! record already logged and treats the second call as a no-op, instead of
//! performing the transition twice.

use std::collections::BTreeMap;

use facet::Facet;
use facet_git_tree::normal_form::{self, NormalForm};
use gix_store::{
    GixRefStore, InvalidRefName, Kind, Layout, RefPath, RefPrefix, RefSegment, RepoStore, Typed,
};

use crate::trigger::Transition;

/// The idempotence key for a *pure advance* transition (D5's first case): a
/// content hash of the effect doc's identity and the transition it responds
/// to, so replaying the same trigger against the same effect doc always
/// produces the same key.
///
/// `old`/`new` are recorded as their hex text rather than
/// [`normal_form::NormalForm::Hash`]: the normal form's closed universe has
/// no `Option` variant (by design -- see `facet-git-tree`'s module docs), and
/// `old` is absent for a ref creation, so a scalar that can represent absence
/// -- an empty string, which is not a legal object-id hex string -- is used
/// uniformly for both `old` and `new` rather than mixing a `Hash` variant for
/// one and an `Option`-shaped workaround for the other.
///
/// # Errors
/// [`facet_git_tree::NormalFormError`] if the value somehow fails to hash --
/// unreachable for this closed, all-scalar shape, but propagated rather than
/// unwrapped since [`normal_form::hash`] itself is fallible.
pub fn advance_key(
    effect_tree: gix::ObjectId,
    trigger: &Transition,
) -> Result<gix::ObjectId, facet_git_tree::NormalFormError> {
    let mut trigger_fields = BTreeMap::new();
    trigger_fields.insert("ref".to_owned(), NormalForm::Str(trigger.ref_.clone()));
    trigger_fields.insert(
        "old".to_owned(),
        NormalForm::Str(trigger.old.map(|oid| oid.to_string()).unwrap_or_default()),
    );
    trigger_fields.insert("new".to_owned(), NormalForm::Str(trigger.new.to_string()));

    let mut fields = BTreeMap::new();
    fields.insert(
        "effect".to_owned(),
        NormalForm::Str(effect_tree.to_string()),
    );
    fields.insert("trigger".to_owned(), NormalForm::Struct(trigger_fields));

    let (key, _objects) = normal_form::hash(&NormalForm::Struct(fields))?;
    Ok(key)
}

/// D5's second case, named rather than built: an executor run's idempotence
/// key is the Action key of the invoked run -- `normal_form::hash({executor,
/// inputs, params})`. The `Action` schema is D7/Phase B3's, not this phase's
/// (`DEVPLAN-effect.md`'s "Phases": D6/D7 land in B3), so there is
/// deliberately no `action_key` function here. A function guessing at
/// `Action`'s shape now would be worse than the honest gap: B3 would either
/// have to match a guess it never agreed to, or break this module's public
/// surface to replace it. [`advance_key`] and [`RecordStore`] below are
/// written so that adding an `action_key` counterpart later requires no
/// change to either -- [`RecordStore::record`] takes a key by value, not by
/// deriving it from a `Then` variant.
pub const ACTION_KEY_SEAM: &str =
    "D7/Phase B3: Action{ executor, inputs, params }, not yet defined";

/// What a transition record says happened -- D8's `outcome` field.
///
/// A sum rather than a `success: bool` plus an optional message, so "failed
/// with no reason recorded" is not a state this type can hold (`DEVPLAN.md`
/// §2.12).
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum Outcome {
    /// The transition was performed.
    Advanced,
    /// It was attempted and refused or failed, with why.
    Failed {
        /// Why it did not go through.
        reason: String,
    },
}

/// One ref edit a transition performed, as text (`<ref>: <old> -> <new>`) --
/// D8's `edits` field. Text rather than a structured `RefEdit` because the
/// log is read by `git effect log` (Phase B3) as a query fact, and a
/// rule-language body over this predicate wants scalars, not a nested
/// `gix::refs` type it would have no vocabulary for.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EditRecord {
    /// The ref this edit advanced.
    pub ref_: String,
    /// Its previous tip, hex, or empty for a creation.
    pub old: String,
    /// Its new tip, hex.
    pub new: String,
}

/// A performed transition, exactly as D8 specifies: `{effect, trigger, key,
/// edits, outcome}`.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct TransitionRecord {
    /// The effect that performed this transition.
    pub effect: String,
    /// The trigger it responded to.
    pub ref_: String,
    /// The trigger's previous tip, hex, or empty for a creation.
    pub old: String,
    /// The trigger's new tip, hex.
    pub new: String,
    /// The idempotence key this transition was recorded under -- hex of
    /// [`advance_key`]'s result (or, once B3 exists, an Action key).
    pub key: String,
    /// Every ref edit this transition performed.
    pub edits: Vec<EditRecord>,
    /// What happened.
    pub outcome: Outcome,
}

/// Everything that can go wrong recording or reading a transition.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The idempotence key is not usable as a ref segment -- unreachable for
    /// a real object-id hex string, but the hex is caller-supplied text, not
    /// a type this module controls end to end.
    #[error(transparent)]
    InvalidKey(#[from] InvalidRefName),
    /// The underlying `gix-store` operation failed.
    #[error(transparent)]
    Store(#[from] gix_store::Error),
    /// Computing the stored record's schema failed.
    #[error(transparent)]
    Schema(#[from] facet_git_tree::SchemaError),
}

/// What recording a transition did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// No record existed at this key yet; `record` was written.
    Recorded(TransitionRecord),
    /// A record already existed at this key -- D5's no-op: "a transition
    /// whose key already appears in the log is a no-op." The existing record
    /// is returned, not the one that was passed in, since nothing was
    /// written and the existing record is what is now authoritative.
    NoOp(TransitionRecord),
}

impl RecordOutcome {
    /// The record either written or already present.
    #[must_use]
    pub fn record(&self) -> &TransitionRecord {
        match self {
            RecordOutcome::Recorded(r) | RecordOutcome::NoOp(r) => r,
        }
    }

    /// Whether this call actually wrote anything.
    #[must_use]
    pub fn was_recorded(&self) -> bool {
        matches!(self, RecordOutcome::Recorded(_))
    }
}

/// The transition log at `refs/effects/log/<key-hex>` -- D8, "authoritative:
/// it exists because an authorized executor signed a decision," distinct from
/// the server op-log which records admissions (X6, not built here).
///
/// One ref per idempotence key, rather than one growing chain, is the shape
/// that makes D5's no-op check a single ref read: "does a record already
/// exist at this key" is exactly [`RecordStore::get`], with no chain walk
/// needed to answer it.
pub struct RecordStore<'r> {
    store: RepoStore<'r>,
    kind: RefSegment,
}

impl<'r> RecordStore<'r> {
    /// Open the transition log over `repo`, signing every write with
    /// `signer` -- D8's "signed via store's `Signer` seam by the executing
    /// identity."
    ///
    /// # Errors
    /// Never today; see [`crate::doc::EffectStore::open`]'s note on why this
    /// still returns `Result`.
    pub fn open(
        repo: &'r gix::Repository,
        signer: impl gix_store::Signer + 'static,
    ) -> Result<Self, RecordError> {
        let layout = Layout {
            data: RefPrefix::new("refs/effects").expect("built-in ref prefix is valid"),
            schema: RefPrefix::new("refs/schema").expect("built-in ref prefix is valid"),
        };
        let store = RepoStore::open_with_layout(repo, layout).with_signer(signer);
        let kind = RefSegment::new("log").expect("built-in ref segment is valid");
        Ok(Self { store, kind })
    }

    fn kind(&self) -> Kind<'_, Typed<TransitionRecord>, GixRefStore<'r>, &'r gix::OdbHandle> {
        self.store.kind(self.kind.clone())
    }

    fn ensure_schema(&self) -> Result<(), RecordError> {
        let kind = self.kind();
        let doc = facet_git_tree::schema_of::<TransitionRecord>()?;
        if kind.schema().get()?.as_ref() != Some(&doc) {
            kind.publish()?;
        }
        Ok(())
    }

    /// The record at `key` (hex), or `None` if this transition has never
    /// been recorded.
    ///
    /// # Errors
    /// [`RecordError::InvalidKey`] if `key` is not usable as a ref segment;
    /// otherwise a store failure.
    pub fn get(&self, key: &str) -> Result<Option<TransitionRecord>, RecordError> {
        let path = RefPath::from(RefSegment::new(key)?);
        Ok(self.kind().get(&path)?)
    }

    /// Record `record` under `key` (hex), unless a record already exists
    /// there -- D5's no-op, and the whole reason at-least-once delivery from
    /// hooks is safe: a retried hook computes the same key and finds this
    /// call already satisfied.
    ///
    /// # Errors
    /// [`RecordError::InvalidKey`] if `key` is not usable as a ref segment;
    /// otherwise a store failure.
    pub fn record(
        &self,
        key: &str,
        record: TransitionRecord,
    ) -> Result<RecordOutcome, RecordError> {
        if let Some(existing) = self.get(key)? {
            return Ok(RecordOutcome::NoOp(existing));
        }
        self.ensure_schema()?;
        let path = RefPath::from(RefSegment::new(key)?);
        self.kind().put(&path, &record)?;
        Ok(RecordOutcome::Recorded(record))
    }

    /// Every transition ever recorded, in no particular order -- the EDB
    /// `git effect log` (Phase B3) will project over.
    ///
    /// # Errors
    /// A store failure.
    pub fn log(&self) -> Result<Vec<TransitionRecord>, RecordError> {
        self.kind()
            .list()?
            .into_iter()
            .map(|path| {
                self.kind()
                    .get(&path)?
                    .ok_or_else(|| unreachable!("`list` only returns names with a current value"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use test_support::Fixture;

    use super::*;
    use crate::trigger::Transition;

    /// See `trigger::tests::FixedSigner` -- same rationale, duplicated
    /// locally rather than shared across modules so each test module stays
    /// readable without hunting for a cross-module test helper.
    struct FixedSigner;

    impl gix_store::Signer for FixedSigner {
        type Error = std::convert::Infallible;

        fn sign(&self, _bytes: &[u8]) -> Result<gix_store::SignatureBytes, Self::Error> {
            Ok(gix_store::SignatureBytes::from(b"test-signature".to_vec()))
        }
    }

    fn sample_record(key: &str) -> TransitionRecord {
        TransitionRecord {
            effect: "watches-members".to_owned(),
            ref_: "refs/meta/members/aa".to_owned(),
            old: String::new(),
            new: "bb".to_owned(),
            key: key.to_owned(),
            edits: vec![EditRecord {
                ref_: "refs/heads/candidate".to_owned(),
                old: String::new(),
                new: "cc".to_owned(),
            }],
            outcome: Outcome::Advanced,
        }
    }

    /// D5: the idempotence key is a pure function of the effect's doc tree
    /// hash and the transition -- computing it twice for the same inputs
    /// gives the same key, and changing either input changes it.
    #[test]
    fn advance_key_is_deterministic_and_input_sensitive() {
        let effect_tree =
            gix::ObjectId::from_hex(format!("{}a", "1".repeat(39)).as_bytes()).unwrap();
        let other_tree =
            gix::ObjectId::from_hex(format!("{}b", "1".repeat(39)).as_bytes()).unwrap();
        let new = gix::ObjectId::from_hex(format!("{}a", "2".repeat(39)).as_bytes()).unwrap();
        let transition = Transition {
            ref_: "refs/meta/members/aa".to_owned(),
            old: None,
            new,
        };

        let a = advance_key(effect_tree, &transition).unwrap();
        let b = advance_key(effect_tree, &transition).unwrap();
        assert_eq!(a, b, "same inputs, same key");

        let c = advance_key(other_tree, &transition).unwrap();
        assert_ne!(a, c, "a different effect doc must key differently");
    }

    /// D8: a performed transition appends a typed doc to `refs/effects/log`,
    /// signed via the store's `Signer` seam by the executing identity -- the
    /// commit `RecordStore::record` writes carries a `gpgsig` header with
    /// exactly the bytes `FixedSigner` produced.
    #[test]
    fn recording_a_transition_appends_a_signed_log_entry() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = RecordStore::open(&repo, FixedSigner).unwrap();

        let key = "a".repeat(40);
        let outcome = store.record(&key, sample_record(&key)).unwrap();
        assert!(outcome.was_recorded());
        assert_eq!(outcome.record(), &sample_record(&key));

        assert_eq!(store.log().unwrap(), vec![sample_record(&key)]);

        let raw = fixture.git(&["cat-file", "commit", &format!("refs/effects/log/{key}")]);
        assert!(
            raw.contains("gpgsig"),
            "the log commit must carry a signature: {raw}"
        );
    }

    /// D5: "a transition whose key already appears in the log is a no-op" --
    /// replaying the same trigger (same key) does not write a second record,
    /// and returns the one already there. This is what makes at-least-once
    /// delivery from hooks safe.
    #[test]
    fn replaying_the_same_trigger_is_a_keyed_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = RecordStore::open(&repo, FixedSigner).unwrap();

        let key = "b".repeat(40);
        let first = store.record(&key, sample_record(&key)).unwrap();
        assert!(first.was_recorded());

        // A second call with a *different* record value at the same key --
        // as a retried hook computing the same idempotence key would send --
        // must not overwrite the first, and must report itself as a no-op.
        let mut different = sample_record(&key);
        different.outcome = Outcome::Failed {
            reason: "should never be written".to_owned(),
        };
        let second = store.record(&key, different).unwrap();
        assert!(!second.was_recorded());
        assert_eq!(second.record(), &sample_record(&key));

        assert_eq!(
            store.log().unwrap().len(),
            1,
            "the no-op must not have appended a second entry"
        );
    }
}
