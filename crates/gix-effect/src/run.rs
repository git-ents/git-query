//! The executor seam (D6), the write-set boundary check (D6), and the
//! authorization gate (D6/D7) that ties trigger firing to a recorded,
//! attested transition.
//!
//! `DEVPLAN-effect.md` D6: "The executor seam is a trait with zero
//! implementations ... What effect does own is the boundary check: every
//! `RefEdit` an executor's emitted records imply is checked against the
//! effect's declared `writes` via `Namespace::overlaps` ..., and a violation
//! aborts the transition before anything advances." [`record_action`] is
//! that boundary made structural: it is the only function in this crate that
//! calls [`gix_attest::Claims::sign`], and it always checks first, so there
//! is no path from an [`Executor`]'s output to a written claim that skips
//! the check.

use gix::objs::{Find, Write};
use gix_query::Namespace;
use gix_store::{Committer, RefStore};

use crate::doc::{EffectDoc, Then};
use crate::record::{
    Action, ActionKey, EditRecord, Outcome, Param, RecordError, RecordOutcome, RecordStore,
    TransitionRecord, action_key, compile_action,
};
use crate::trigger::Transition;

/// "No scheduler ambitions, no executor implementations" -- an [`Executor`]
/// runs one action and reports what it produced. Kiln, CI runners, and env
/// materializers live outside this family and are handed in as trait
/// objects.
pub trait Executor {
    /// Run the action `key` names, over its pinned `inputs`, and return the
    /// content hash it produced.
    ///
    /// # Errors
    /// [`ExecError`] on any failure the executor wants surfaced; this crate
    /// neither retries nor interprets it.
    fn run(&self, key: &ActionKey) -> Result<gix_attest::Oid, ExecError>;
}

/// Why an [`Executor::run`] failed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ExecError(pub String);

/// A claim an action record would land on lies outside the firing effect's
/// declared write set -- D6's boundary, named.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("effect `{effect}` would write `{namespace}`, outside its declared write set")]
pub struct BoundaryError {
    /// The effect that would have violated its own declaration.
    pub effect: String,
    /// The namespace the write would have touched.
    pub namespace: Namespace,
}

/// Check `touched` against `writes`. The one gate every write this module
/// performs passes through -- [`record_action`] and [`fire_advance`] both
/// call it before touching a ref, and nothing else in this crate reaches
/// one.
///
/// # Errors
/// [`BoundaryError`] when no declared namespace overlaps `touched`.
pub fn check_boundary(
    effect: &str,
    writes: &[Namespace],
    touched: &Namespace,
) -> Result<(), BoundaryError> {
    if writes.iter().any(|w| w.overlaps(touched)) {
        Ok(())
    } else {
        Err(BoundaryError {
            effect: effect.to_owned(),
            namespace: touched.clone(),
        })
    }
}

/// Everything that can go wrong firing an effect's `then` clause.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// `fire_run` was called on an effect whose `then` is not [`Then::Run`].
    #[error("effect `{0}`'s `then` is not `Run`")]
    NotARun(String),
    /// `fire_advance` was called on an effect whose `then` is not
    /// [`Then::Advance`].
    #[error("effect `{0}`'s `then` is not `Advance`")]
    NotAnAdvance(String),
    /// The effect's `gate` did not derive: D6's authorization check, "itself
    /// a query," refused.
    #[error("effect `{0}`'s gate did not authorize this transition")]
    Unauthorized(String),
    /// The executor failed.
    #[error(transparent)]
    Exec(#[from] ExecError),
    /// The write-set boundary check refused -- nothing was advanced.
    #[error(transparent)]
    Boundary(#[from] BoundaryError),
    /// Evaluating the gate, or resolving the effect's write namespaces,
    /// failed.
    #[error(transparent)]
    Query(#[from] gix_query::QueryError),
    /// Hashing the action key failed.
    #[error(transparent)]
    NormalForm(#[from] facet_git_tree::NormalFormError),
    /// Signing or resolving the action record's claim failed.
    #[error(transparent)]
    Attest(#[from] gix_attest::Error),
    /// The transition log or the action schema failed.
    #[error(transparent)]
    Record(#[from] RecordError),
    /// The ref store refused the advance -- a lost compare-and-swap race or a
    /// backend failure.
    #[error("advancing `{ref_}` failed: {source}")]
    RefStore {
        /// The ref the advance targeted.
        ref_: String,
        /// Why it failed.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// `then.ref_`, or a claim target's derived ref, is not a usable ref
    /// name.
    #[error(transparent)]
    InvalidRefName(#[from] gix_store::InvalidRefName),
}

/// Whether `effect`'s `gate` currently derives at least one row -- D6/D7's
/// authorization check: "check authorization (claims + gate rules, itself a
/// query)." The gate is rule-language source like any other body, evaluated
/// the same way `when` is ([`gix_query::run_body`]), so authorization is
/// decided by the rule this crate evaluates, never by policy this crate
/// hardcodes.
///
/// # Errors
/// [`RunError::Query`] if the gate fails to parse or validate.
pub fn authorized(repo: &gix::Repository, effect: &EffectDoc) -> Result<bool, RunError> {
    Ok(!gix_query::run_body(repo, &effect.gate)?.is_empty())
}

fn writes_of(effect: &EffectDoc) -> Vec<Namespace> {
    effect.writes.iter().cloned().map(Namespace::new).collect()
}

/// The claim target an action record for `key` lands on: `{kind: "action",
/// id: <action key>}`. Its claim ref -- `refs/claims/<hash of this
/// descriptor>` -- is what [`record_action`] checks against the firing
/// effect's declared write set.
///
/// # Errors
/// [`facet_git_tree::NormalFormError`] if `key` cannot be hashed.
pub fn action_target(
    key: &ActionKey,
) -> Result<gix_attest::Target, facet_git_tree::NormalFormError> {
    Ok(gix_attest::Target {
        kind: crate::record::ACTION_TARGET_KIND.to_owned(),
        id: action_key(key)?.into(),
    })
}

/// Sign `action` as a claim, after checking that the claim ref it lands on
/// lies inside `writes`. The only function in this crate that calls
/// [`gix_attest::Claims::sign`]; see the module docs for why that makes the
/// boundary structural rather than a convention.
///
/// # Errors
/// [`RunError::Boundary`] if the claim ref falls outside `writes`, without
/// writing anything; otherwise a hashing, ref-name, or attest failure.
pub fn record_action<R, O>(
    claims: &gix_attest::Claims<'_, R, O>,
    effect: &str,
    writes: &[Namespace],
    action_tree: gix::ObjectId,
    action: &Action,
    signer: gix_attest::Oid,
) -> Result<gix::ObjectId, RunError>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    let target = action_target(&action.key)?;
    let reference = claims.reference(&target)?;
    let touched = Namespace::new(reference.as_str().to_owned());
    check_boundary(effect, writes, &touched)?;

    let envelope = gix_attest::Envelope {
        target,
        payload: action_tree.into(),
        payload_kind: crate::record::ACTION_TARGET_KIND.to_owned(),
        key: signer,
    };
    Ok(claims.sign(&envelope)?)
}

/// Run a [`Then::Run`] effect end to end: check its `gate`, run `executor`,
/// check the write-set boundary, sign the action record, and append the
/// transition log entry.
///
/// D5's idempotence key is the run's own [`ActionKey`] hash -- a call whose
/// key is already in `records` is a no-op before `executor` ever runs, which
/// is what makes at-least-once delivery safe for executor-invoking effects
/// exactly as it is for pure advances.
///
/// # Errors
/// [`RunError::NotARun`] if `effect.then` is not [`Then::Run`];
/// [`RunError::Unauthorized`] if the gate does not derive; otherwise the
/// executor's, the boundary check's, or a store/attest failure.
#[allow(clippy::too_many_arguments)]
pub fn fire_run<R, O>(
    repo: &gix::Repository,
    store: &gix_store::Store<R, O>,
    records: &RecordStore<'_>,
    effect: &EffectDoc,
    trigger: &Transition,
    executor: &dyn Executor,
    inputs: Vec<gix_attest::Oid>,
    params: Vec<Param>,
    signer: gix_attest::Oid,
) -> Result<RecordOutcome, RunError>
where
    R: RefStore + Committer,
    O: Find + Write,
{
    let Then::Run { executor: name, .. } = &effect.then else {
        return Err(RunError::NotARun(effect.name.clone()));
    };
    if !authorized(repo, effect)? {
        return Err(RunError::Unauthorized(effect.name.clone()));
    }

    let key = ActionKey {
        executor: name.clone(),
        inputs,
        params,
    };
    let idempotence_key = action_key(&key)?.to_string();
    if let Some(existing) = records.get(&idempotence_key)? {
        return Ok(RecordOutcome::NoOp(existing));
    }

    let output = executor.run(&key)?;
    let action = Action {
        key: key.clone(),
        output,
    };
    let action_tree = compile_action(store, &action)?;

    let writes = writes_of(effect);
    let claims = gix_attest::Claims::open(store);
    let claim_id = record_action(&claims, &effect.name, &writes, action_tree, &action, signer)?;
    let claim_ref = claims.reference(&action_target(&key)?)?.as_str().to_owned();

    let record = TransitionRecord {
        effect: effect.name.clone(),
        ref_: trigger.ref_.clone(),
        old: trigger.old.map(|oid| oid.to_string()).unwrap_or_default(),
        new: trigger.new.to_string(),
        key: idempotence_key.clone(),
        edits: vec![EditRecord {
            ref_: claim_ref,
            old: String::new(),
            new: claim_id.to_string(),
        }],
        outcome: Outcome::Advanced,
    };
    Ok(records.record(&idempotence_key, record)?)
}

/// Run a [`Then::Advance`] effect end to end: check its `gate`, check the
/// write-set boundary, compare-and-swap `then.ref_` to `new`, and append the
/// transition log entry.
///
/// `new` is the resolved target of `then.to` -- D1 stores that field as
/// unevaluated rule-language text, and evaluating it is not this function's
/// job; the caller (the trigger loop, once one is wired end to end) resolves
/// it before calling this.
///
/// # Errors
/// [`RunError::NotAnAdvance`] if `effect.then` is not [`Then::Advance`];
/// [`RunError::Unauthorized`] if the gate does not derive; otherwise the
/// boundary check's or a ref-store failure.
pub fn fire_advance<R>(
    repo: &gix::Repository,
    refs: &R,
    records: &RecordStore<'_>,
    effect: &EffectDoc,
    effect_tree: gix::ObjectId,
    trigger: &Transition,
    new: gix::ObjectId,
) -> Result<RecordOutcome, RunError>
where
    R: RefStore,
{
    let Then::Advance { ref_, .. } = &effect.then else {
        return Err(RunError::NotAnAdvance(effect.name.clone()));
    };
    if !authorized(repo, effect)? {
        return Err(RunError::Unauthorized(effect.name.clone()));
    }

    let writes = writes_of(effect);
    let touched = Namespace::new(ref_.clone());
    check_boundary(&effect.name, &writes, &touched)?;

    let idempotence_key = crate::record::advance_key(effect_tree, trigger)?.to_string();
    if let Some(existing) = records.get(&idempotence_key)? {
        return Ok(RecordOutcome::NoOp(existing));
    }

    let name = gix_store::RefName::new(ref_.as_str())?;
    let current = refs.read(&name).map_err(|source| RunError::RefStore {
        ref_: ref_.clone(),
        source: Box::new(source),
    })?;
    let edit = match current {
        Some(expected) => gix_store::RefEdit::Update {
            name: name.clone(),
            expected,
            new,
        },
        None => gix_store::RefEdit::Create {
            name: name.clone(),
            new,
        },
    };
    refs.apply(edit).map_err(|source| RunError::RefStore {
        ref_: ref_.clone(),
        source: Box::new(source),
    })?;

    let record = TransitionRecord {
        effect: effect.name.clone(),
        ref_: trigger.ref_.clone(),
        old: trigger.old.map(|oid| oid.to_string()).unwrap_or_default(),
        new: trigger.new.to_string(),
        key: idempotence_key.clone(),
        edits: vec![EditRecord {
            ref_: ref_.clone(),
            old: current.map(|oid| oid.to_string()).unwrap_or_default(),
            new: new.to_string(),
        }],
        outcome: Outcome::Advanced,
    };
    Ok(records.record(&idempotence_key, record)?)
}

#[cfg(test)]
mod tests {
    use test_support::{ClaimFixture, Fixture};

    use super::*;

    fn doc(name: &str, when: &str, gate: &str, then: Then, writes: &[&str]) -> EffectDoc {
        EffectDoc {
            name: name.to_owned(),
            when: when.to_owned(),
            gate: gate.to_owned(),
            then,
            writes: writes.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    struct StubExecutor {
        output: gix_attest::Oid,
    }

    impl Executor for StubExecutor {
        fn run(&self, _key: &ActionKey) -> Result<gix_attest::Oid, ExecError> {
            Ok(self.output)
        }
    }

    struct FailingExecutor;

    impl Executor for FailingExecutor {
        fn run(&self, _key: &ActionKey) -> Result<gix_attest::Oid, ExecError> {
            Err(ExecError("stub executor refuses".to_owned()))
        }
    }

    fn no_op_transition() -> Transition {
        Transition {
            ref_: "refs/heads/main".to_owned(),
            old: None,
            new: gix::ObjectId::null(gix::hash::Kind::Sha1),
        }
    }

    /// `check_boundary` is the primitive `record_action`/`fire_advance` share:
    /// a namespace inside the declared writes passes, one outside is refused
    /// by name.
    #[test]
    fn check_boundary_accepts_covered_namespaces_and_refuses_others() {
        let writes = vec![Namespace::new("refs/claims/*")];
        assert!(check_boundary("e", &writes, &Namespace::new("refs/claims/aa")).is_ok());
        let err =
            check_boundary("e", &writes, &Namespace::new("refs/release/candidate")).unwrap_err();
        assert_eq!(err.effect, "e");
        assert_eq!(err.namespace, Namespace::new("refs/release/candidate"));
    }

    /// D6's boundary, exercised through `record_action`: an effect whose
    /// declared writes do not cover the action record's claim ref aborts with
    /// nothing written -- no claim signed, no ref moved.
    #[test]
    fn an_executor_emitting_outside_its_declared_writes_aborts_with_nothing_advanced() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let claims_fixture = ClaimFixture::new(&fixture);
        let store = gix_store::RepoStore::open_with_layout(&repo, gix_attest::layout());
        crate::record::register_action_schema(&store).unwrap();

        let key = ActionKey {
            executor: "kiln".to_owned(),
            inputs: vec![],
            params: vec![],
        };
        let action = Action {
            key,
            output: gix::ObjectId::null(gix::hash::Kind::Sha1).into(),
        };
        let action_tree = compile_action(&store, &action).unwrap();

        let refs_before = fixture.refs();
        let claims = gix_attest::Claims::open(&store);
        let err = record_action(
            &claims,
            "build_on_push",
            // Declares a write set that cannot possibly cover any claim ref.
            &[Namespace::new("refs/release/candidate")],
            action_tree,
            &action,
            claims_fixture.key_claim().into(),
        )
        .unwrap_err();

        assert!(matches!(err, RunError::Boundary(_)));
        assert_eq!(
            fixture.refs(),
            refs_before,
            "nothing must have been written"
        );
    }

    /// The full `Then::Run` path: an authorized effect running a stub
    /// executor whose declared writes cover the action record's claim ref
    /// signs the claim and appends exactly one transition log entry.
    #[test]
    fn firing_an_authorized_run_effect_records_a_signed_action_claim() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let claims_fixture = ClaimFixture::new(&fixture);
        let store = gix_store::RepoStore::open_with_layout(&repo, gix_attest::layout());
        crate::record::register_action_schema(&store).unwrap();
        let member_hex = fixture.commit("member commit");
        fixture.update_ref(&format!("refs/meta/members/{member_hex}"), &member_hex);

        let effect = doc(
            "build_on_push",
            "member(M)",
            "member(M)",
            Then::Run {
                executor: "kiln".to_owned(),
                inputs: vec![],
            },
            &["refs/claims/*"],
        );

        let output: gix_attest::Oid = gix::ObjectId::from_bytes_or_panic(&[0xcc; 20]).into();
        let executor = StubExecutor { output };
        let records = RecordStore::open(&repo, test_signer()).unwrap();

        let outcome = fire_run(
            &repo,
            &store,
            &records,
            &effect,
            &no_op_transition(),
            &executor,
            vec![],
            vec![],
            claims_fixture.key_claim().into(),
        )
        .unwrap();
        assert!(outcome.was_recorded());
        assert_eq!(outcome.record().outcome, Outcome::Advanced);
        assert_eq!(records.log().unwrap().len(), 1);

        // Replaying the identical run is a keyed no-op: the executor is not
        // consulted a second time (the stub is pure, so this only checks the
        // record store, but a real executor with side effects depends on
        // exactly this not calling `run` again).
        let replay = fire_run(
            &repo,
            &store,
            &records,
            &effect,
            &no_op_transition(),
            &executor,
            vec![],
            vec![],
            claims_fixture.key_claim().into(),
        )
        .unwrap();
        assert!(!replay.was_recorded());
        assert_eq!(records.log().unwrap().len(), 1);
    }

    /// An effect whose `gate` derives nothing refuses before the executor
    /// ever runs.
    #[test]
    fn an_unauthorized_gate_refuses_before_the_executor_runs() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let claims_fixture = ClaimFixture::new(&fixture);
        let store = gix_store::RepoStore::open_with_layout(&repo, gix_attest::layout());
        crate::record::register_action_schema(&store).unwrap();

        let effect = doc(
            "build_on_push",
            "member(M)",
            // No member ref exists, so this gate derives nothing.
            "member(M)",
            Then::Run {
                executor: "kiln".to_owned(),
                inputs: vec![],
            },
            &["refs/claims/*"],
        );
        let records = RecordStore::open(&repo, test_signer()).unwrap();

        let err = fire_run(
            &repo,
            &store,
            &records,
            &effect,
            &no_op_transition(),
            &FailingExecutor,
            vec![],
            vec![],
            claims_fixture.key_claim().into(),
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Unauthorized(_)));
    }

    /// The `Then::Advance` path: an authorized effect whose declared writes
    /// cover `then.ref_` advances it and appends a transition log entry.
    #[test]
    fn firing_an_authorized_advance_effect_moves_the_ref_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let member_hex = fixture.commit("member commit");
        fixture.update_ref(&format!("refs/meta/members/{member_hex}"), &member_hex);
        let target = fixture.commit("release candidate");

        let effect = doc(
            "release_on_green",
            "member(M)",
            "member(M)",
            Then::Advance {
                ref_: "refs/release/candidate".to_owned(),
                to: "HEAD".to_owned(),
            },
            &["refs/release/candidate"],
        );
        let effect_tree = gix::ObjectId::from_bytes_or_panic(&[0x42; 20]);
        let records = RecordStore::open(&repo, test_signer()).unwrap();
        let refs = gix_store::GixRefStore::new(&repo);

        let outcome = fire_advance(
            &repo,
            &refs,
            &records,
            &effect,
            effect_tree,
            &no_op_transition(),
            gix::ObjectId::from_hex(target.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(outcome.was_recorded());

        let current = repo
            .find_reference("refs/release/candidate")
            .unwrap()
            .id()
            .to_string();
        assert_eq!(current, target);
    }

    /// An advance whose declared writes do not cover `then.ref_` aborts with
    /// nothing moved.
    #[test]
    fn an_advance_outside_its_declared_writes_aborts_with_nothing_moved() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let member_hex = fixture.commit("member commit");
        fixture.update_ref(&format!("refs/meta/members/{member_hex}"), &member_hex);
        let target = fixture.commit("release candidate");

        let effect = doc(
            "release_on_green",
            "member(M)",
            "member(M)",
            Then::Advance {
                ref_: "refs/release/candidate".to_owned(),
                to: "HEAD".to_owned(),
            },
            // Declares a namespace that does not cover `then.ref_`.
            &["refs/release/other"],
        );
        let effect_tree = gix::ObjectId::from_bytes_or_panic(&[0x42; 20]);
        let records = RecordStore::open(&repo, test_signer()).unwrap();
        let refs = gix_store::GixRefStore::new(&repo);

        let refs_before = fixture.refs();
        let err = fire_advance(
            &repo,
            &refs,
            &records,
            &effect,
            effect_tree,
            &no_op_transition(),
            gix::ObjectId::from_hex(target.as_bytes()).unwrap(),
        )
        .unwrap_err();
        assert!(matches!(err, RunError::Boundary(_)));
        assert_eq!(fixture.refs(), refs_before, "nothing must have moved");
    }

    /// A minimal `Signer` for the transition log -- see
    /// `record::tests::FixedSigner`'s rationale, duplicated locally.
    struct FixedSigner;

    impl gix_store::Signer for FixedSigner {
        type Error = std::convert::Infallible;

        fn sign(&self, _bytes: &[u8]) -> Result<gix_store::SignatureBytes, Self::Error> {
            Ok(gix_store::SignatureBytes::from(b"test-signature".to_vec()))
        }
    }

    fn test_signer() -> FixedSigner {
        FixedSigner
    }
}
