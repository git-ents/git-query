//! Idempotence keys (D5), the `Action` schema (D7), and the transition log
//! (D8).
//!
//! `DEVPLAN-effect.md` D5: a pure advance's idempotence key is
//! `normal_form::hash({effect: <doc tree hash>, trigger: {ref, old, new}})`,
//! through `facet-git-tree`'s frozen normal form -- "there is no key crate."
//! An executor run's is [`action_key`], the same hash over `Action.key`. D7:
//! `Action { key: { executor, inputs, params }, output }`, `key` marked
//! identity-bearing, registered at `refs/schema/action`. D8: each performed
//! transition appends a typed doc `{effect, trigger, key, edits, outcome}` to
//! `refs/effects/log`, signed via store's `Signer` seam by the executing
//! identity.
//!
//! What this module does *not* do: decide whether a transition should
//! happen, invoke an executor, or check the write-set boundary. That is
//! [`crate::run`] (D6) -- this module is the vocabulary an executed
//! transition is expressed in and recorded through, which is exactly what
//! makes at-least-once delivery from hooks safe: a caller that computes the
//! same key twice (a hook retried after a timeout, say) finds the first
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

/// D7: the `Action` schema, "registered by `gix-effect`, pro tem." One
/// labeled parameter -- `params` is a list rather than a map because the
/// identity normal form's `Struct`/`Map` shapes both demand either
/// schema-fixed field names or a scalar key type spelled out by hand, and a
/// plain `{name, value}` pair sidesteps both without losing anything
/// `ARCHITECTURE.md`'s examples need ("engine version, diff algorithm,
/// grammar version" are exactly this shape).
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Param {
    /// The parameter's name -- `"engine-version"`, `"diff-algorithm"`, ...
    pub name: String,
    /// Its value.
    pub value: String,
}

/// `Action.key`: `{executor, inputs, params}`, identity- and key-bearing as a
/// whole (`ARCHITECTURE.md`'s action, D7) -- the action key is the store hash
/// of this subtree through the identity normal form, exactly the anchor-id
/// move, so `#[facet(facet_git_tree::identity_key)]` on the whole struct is
/// what [`crate::doc::EffectStore`]'s schema registration path (via
/// [`register_action_schema`]) enforces the universe constraint on: `inputs`
/// and `params` must be expressible in it, or defining the schema fails
/// before anything is published.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[facet(facet_git_tree::identity_key)]
pub struct ActionKey {
    /// The executor's id and version, as one opaque label.
    pub executor: String,
    /// Pinned content hashes -- everything the action was run over.
    pub inputs: Vec<gix_attest::Oid>,
    /// Everything else that affects the output and is not ambient: engine
    /// version, diff algorithm, grammar version, and the like.
    pub params: Vec<Param>,
}

/// A hermetic derivation record (`ARCHITECTURE.md`'s "action (not a
/// product)"): `key` names the run, `output` is what it produced.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct Action {
    /// What was run, over what, with what parameters.
    pub key: ActionKey,
    /// The content hash the run produced.
    pub output: gix_attest::Oid,
}

/// The kind name [`Action`]'s schema is published under -- `refs/schema/action`
/// under attest's layout ([`gix_attest::layout`]), which is exactly D7's "registered
/// at `refs/schema/action`."
pub const ACTION_KIND: &str = "action";

/// The [`Target::kind`](gix_attest::Target::kind) label an action record's
/// claim carries, and the [`Envelope::payload_kind`](gix_attest::Envelope::payload_kind)
/// its payload is written under -- one string for both, since a consumer
/// joins a payload kind against `refs/schema/<kind>` the same way
/// [`gix_attest`]'s own key and claim kinds do.
pub const ACTION_TARGET_KIND: &str = ACTION_KIND;

fn action_segment() -> gix_store::RefSegment {
    gix_store::RefSegment::new(ACTION_KIND).expect("the action kind is a valid ref segment")
}

/// `key` in the identity normal form's closed universe, by hand -- the same
/// move as `gix_attest::Target`'s `to_normal_form`: the mapping has no
/// reflection path from an arbitrary `Facet` type, so the field names here
/// must match [`ActionKey`]'s, which [`register_action_schema`]'s universe
/// check keeps honest.
fn to_normal_form(key: &ActionKey) -> NormalForm {
    let inputs = key
        .inputs
        .iter()
        .map(|oid| NormalForm::Hash(gix::ObjectId::from(*oid)))
        .collect();
    let params = key
        .params
        .iter()
        .map(|p| {
            let mut fields = BTreeMap::new();
            fields.insert("name".to_owned(), NormalForm::Str(p.name.clone()));
            fields.insert("value".to_owned(), NormalForm::Str(p.value.clone()));
            NormalForm::Struct(fields)
        })
        .collect();
    let mut fields = BTreeMap::new();
    fields.insert("executor".to_owned(), NormalForm::Str(key.executor.clone()));
    fields.insert("inputs".to_owned(), NormalForm::List(inputs));
    fields.insert("params".to_owned(), NormalForm::List(params));
    NormalForm::Struct(fields)
}

/// D5's second idempotence key: the Action key of the invoked run.
///
/// # Errors
/// [`facet_git_tree::NormalFormError`] if the value somehow fails to hash --
/// unreachable for this closed, all-scalar shape, but propagated rather than
/// unwrapped since [`normal_form::hash`] itself is fallible.
pub fn action_key(key: &ActionKey) -> Result<gix::ObjectId, facet_git_tree::NormalFormError> {
    let (key, _objects) = normal_form::hash(&to_normal_form(key))?;
    Ok(key)
}

/// Publish [`Action`]'s schema under [`ACTION_KIND`] in `store` -- the
/// identity-universe check every schema registration runs
/// ([`gix_store::KindSchema::write`]) is what this crate leans on for D7's
/// "`params` must be expressible in the identity normal form. Enforced at
/// schema registration": a future `ActionKey` field that left the universe
/// (an enum, an `Option`) would fail here, not at `action_key` time.
///
/// # Errors
/// [`RecordError::Schema`] when the schema cannot be derived from [`Action`];
/// [`RecordError::Store`] when the store write fails, including the universe
/// refusal above.
pub fn register_action_schema<R, O>(
    store: &gix_store::Store<R, O>,
) -> Result<gix::ObjectId, RecordError>
where
    R: gix_store::RefStore + gix_store::Committer,
    O: gix::objs::Find + gix::objs::Write,
{
    Ok(store.kind::<Action>(action_segment()).publish()?)
}

/// Compile `action` into the payload tree an [`gix_attest::Envelope`] carries
/// -- store's `{value/, schema/}` document tree, not a bare serialization, so
/// a reader with no `Action` type of its own can still decode it against the
/// schema [`register_action_schema`] published.
///
/// # Errors
/// [`RecordError::Store`] when no schema is published yet, or compilation
/// fails.
pub fn compile_action<R, O>(
    store: &gix_store::Store<R, O>,
    action: &Action,
) -> Result<gix::ObjectId, RecordError>
where
    R: gix_store::RefStore + gix_store::Committer,
    O: gix::objs::Find + gix::objs::Write,
{
    Ok(store.kind::<Action>(action_segment()).compile(action)?)
}

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

    fn action_key_of(executor: &str, input: u8, param: &str) -> ActionKey {
        ActionKey {
            executor: executor.to_owned(),
            inputs: vec![gix::ObjectId::from_bytes_or_panic(&[input; 20]).into()],
            params: vec![Param {
                name: "diff-algorithm".to_owned(),
                value: param.to_owned(),
            }],
        }
    }

    /// D5's second case: the action key is a pure function of `executor`,
    /// `inputs`, and `params` -- same inputs, same key, and changing any one
    /// field changes it.
    #[test]
    fn action_key_is_deterministic_and_every_field_participates() {
        let base = action_key_of("kiln", 0x11, "myers");
        let a = action_key(&base).unwrap();
        let b = action_key(&base).unwrap();
        assert_eq!(a, b, "same inputs, same key");

        assert_ne!(
            a,
            action_key(&action_key_of("other", 0x11, "myers")).unwrap()
        );
        assert_ne!(
            a,
            action_key(&action_key_of("kiln", 0x22, "myers")).unwrap()
        );
        assert_ne!(
            a,
            action_key(&action_key_of("kiln", 0x11, "patience")).unwrap()
        );
    }

    /// D7: `Action`'s schema registers under `refs/schema/action` in attest's
    /// own layout, so its target key composes with a claim's `payload_kind`
    /// exactly like `gix-attest`'s own key and claim kinds.
    #[test]
    fn the_action_schema_registers_at_refs_schema_action() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = gix_store::RepoStore::open_with_layout(&repo, gix_attest::layout());
        register_action_schema(&store).unwrap();

        let raw = fixture.git(&["for-each-ref", "refs/schema/action"]);
        assert!(!raw.is_empty(), "refs/schema/action must exist: {raw:?}");
    }

    /// `compile_action` produces the same `{value/, schema/}` document tree
    /// any store document does -- a pure function of the action and the
    /// published schema, with no ref advanced.
    #[test]
    fn compile_action_is_pure_and_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = gix_store::RepoStore::open_with_layout(&repo, gix_attest::layout());
        register_action_schema(&store).unwrap();

        let action = Action {
            key: action_key_of("kiln", 0xaa, "myers"),
            output: gix::ObjectId::from_bytes_or_panic(&[0xbb; 20]).into(),
        };
        let refs_before = fixture.refs();
        let first = compile_action(&store, &action).unwrap();
        let second = compile_action(&store, &action).unwrap();
        assert_eq!(first, second);
        assert_eq!(fixture.refs(), refs_before, "compiling advances no ref");
    }
}
