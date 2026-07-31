//! `ARCHITECTURE.md`'s worked example 3, "Effect chain with actions
//! (CI-shaped)," end to end: two chained effects, a stub executor, and the
//! acyclicity check (`DEVPLAN-effect.md` C4) proving the chain terminates.
//!
//! ```text
//! effect build_on_push {
//!   when:   advanced(refs/heads/main, Tip),
//!   gate:   authorized(build_executor),
//!   then:   run(kiln, inputs: tree_of(Tip)),
//!   writes: [refs/claims/action/kiln/*],
//! }
//! effect release_on_green {
//!   when:   action_record(kiln, tree_of(Tip), _, Out), tests_pass(Out),
//!   gate:   authorized(release_executor),
//!   then:   advance(refs/release/candidate, Tip),
//!   writes: [refs/release/candidate],
//! }
//! ```
//!
//! Two deliberate substitutions for predicates this workspace does not
//! implement, both noted where they matter:
//!
//! - `advanced`/`authorized` are stand-ins; `member/1`, backed by
//!   `refs/meta/members/*`, plays both roles here exactly as
//!   `trigger.rs`'s own tests use it, so this test exercises real trigger
//!   detection and real gate evaluation rather than predicates that do not
//!   exist yet.
//! - The write set an action record's claim lands under is
//!   `refs/claims/<target-key>` (`gix-attest`'s actual, flat claim-ref
//!   layout), not the illustrative `refs/claims/action/kiln/*` above --
//!   `refs/claims/*` is the real namespace this test declares and checks.
//!
//! What the example's own text asserts is unchanged and is exactly what
//! this test proves: effect 1's attested action record is inside its
//! declared write set, so effect 2's trigger sees it; and stratification
//! proves the graph terminates because `refs/release/candidate` is written
//! but appears in no trigger's read set.

use gix_effect::{
    Action, ActionKey, EffectDoc, EffectStore, ExecError, Executor, Param, RecordStore, Then,
    Transition, TriggerDetector, fire_advance, fire_run,
};
use test_support::{ClaimFixture, Fixture};

struct FixedSigner;

impl gix_store::Signer for FixedSigner {
    type Error = std::convert::Infallible;

    fn sign(&self, _bytes: &[u8]) -> Result<gix_store::SignatureBytes, Self::Error> {
        Ok(gix_store::SignatureBytes::from(b"test-signature".to_vec()))
    }
}

/// The stub executor D6 promises zero implementations of: it produces a
/// fixed output and asserts it is invoked at most once, per action key --
/// the idempotence property `fire_run`'s replay case relies on.
struct StubKiln {
    output: gix_attest::Oid,
    calls: std::cell::Cell<u32>,
}

impl Executor for StubKiln {
    fn run(&self, _key: &ActionKey) -> Result<gix_attest::Oid, ExecError> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.output)
    }
}

fn build_on_push() -> EffectDoc {
    EffectDoc {
        name: "build_on_push".to_owned(),
        when: "member(M)".to_owned(),
        gate: "member(M)".to_owned(),
        then: Then::Run {
            executor: "kiln".to_owned(),
            inputs: vec![],
        },
        writes: vec!["refs/claims/*".to_owned()],
    }
}

fn release_on_green() -> EffectDoc {
    EffectDoc {
        name: "release_on_green".to_owned(),
        // `kind(C, action)`: the action-record claim's own `Envelope`, not
        // the build cache -- exactly the note under worked example 3: "Note
        // `when` of effect 2 reads the *action record claim* (authoritative,
        // signed), not the build cache (derived)."
        when: "kind(C, action)".to_owned(),
        gate: "member(M)".to_owned(),
        then: Then::Advance {
            ref_: "refs/release/candidate".to_owned(),
            to: "HEAD".to_owned(),
        },
        writes: vec!["refs/release/candidate".to_owned()],
    }
}

#[test]
fn the_chain_fires_end_to_end_and_stratification_proves_it_terminates() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::init(dir.path());
    let head = fixture.commit("first");
    let repo = fixture.open();

    // `ClaimFixture` registers `gix-attest`'s own schemas and publishes a
    // real signing key -- needed here for real, not incidental, reasons:
    // "unverified claims contribute no facts" (`gix-query-host/src/claim.rs`),
    // so `release_on_green`'s `when` (`kind(C, action)`) only sees the action
    // record's claim once it is a claim `Claims::verify` actually accepts.
    let claims_fixture = ClaimFixture::new(&fixture);
    let store = claims_fixture.store();
    gix_effect::register_action_schema(&store).unwrap();

    // Defining both effects together is C4's acyclicity check, run as
    // `EffectStore::define`'s validation pass: `build_on_push` writes
    // `refs/claims/*`, which `release_on_green`'s `when` footprint reads,
    // but `release_on_green` writes `refs/release/candidate`, which no
    // effect's `when` reads -- so the graph has an edge in only one
    // direction and `define` accepts both. This is stratification proving
    // termination, exercised rather than asserted separately.
    let effects = EffectStore::open(&repo).unwrap();
    effects.define(build_on_push()).unwrap();
    let effect2 = release_on_green();
    effects.define(effect2.clone()).unwrap();

    // The trigger: a member ref is created, standing in for
    // `advanced(refs/heads/main, Tip)` -- see the module docs.
    let member_hex = fixture.commit("member commit");
    let member_oid = gix::ObjectId::from_hex(member_hex.as_bytes()).unwrap();
    let member_ref = format!("refs/meta/members/{member_hex}");
    fixture.update_ref(&member_ref, &member_hex);

    let detector = TriggerDetector::new(&repo);
    let transition1 = Transition {
        ref_: member_ref,
        old: None,
        new: member_oid,
    };
    let firings1 = detector.fire(&transition1).unwrap();
    assert_eq!(
        firings1
            .iter()
            .map(|f| f.effect.name.as_str())
            .collect::<Vec<_>>(),
        vec!["build_on_push"],
        "only the build effect reads the member namespace"
    );

    // Fire `build_on_push`: gate checks, executor runs, boundary checks, and
    // the action record lands as a signed claim inside the declared write
    // set.
    let records = RecordStore::open(&repo, FixedSigner).unwrap();
    let kiln = StubKiln {
        output: gix::ObjectId::from_bytes_or_panic(&[0xaa; 20]).into(),
        calls: std::cell::Cell::new(0),
    };
    let signer: gix_attest::Oid = claims_fixture.key_claim().into();
    let outcome1 = fire_run(
        &repo,
        &store,
        &records,
        &build_on_push(),
        &transition1,
        &kiln,
        vec![],
        vec![],
        signer,
    )
    .unwrap();
    assert!(outcome1.was_recorded());
    assert_eq!(kiln.calls.get(), 1);

    // Replaying the same trigger against the same effect is a keyed no-op:
    // the executor is not invoked again.
    let replay = fire_run(
        &repo,
        &store,
        &records,
        &build_on_push(),
        &transition1,
        &kiln,
        vec![],
        vec![],
        signer,
    )
    .unwrap();
    assert!(!replay.was_recorded());
    assert_eq!(kiln.calls.get(), 1, "the executor must not run twice");

    // The claim ref the action record actually landed on -- computed the
    // same way `record_action` computed it internally, so the test observes
    // the same ref the boundary check was applied to, not a guess at its
    // shape.
    let action_key = ActionKey {
        executor: "kiln".to_owned(),
        inputs: vec![],
        params: Vec::<Param>::new(),
    };
    let target = gix_effect::action_target(&action_key).unwrap();
    let claims = gix_attest::Claims::open(&store);
    let claim_ref = claims.reference(&target).unwrap();
    let claim_commit = repo
        .find_reference(claim_ref.as_str())
        .unwrap()
        .id()
        .detach();

    let action = Action {
        key: action_key,
        output: kiln.output,
    };
    assert_eq!(
        outcome1.record().edits[0].new,
        claim_commit.to_string(),
        "the transition record's edit names the claim commit `record_action` wrote"
    );
    let _ = action; // constructed for readability; identity checked above.

    // The second transition: the claim ref itself was created.
    let transition2 = Transition {
        ref_: claim_ref.as_str().to_owned(),
        old: None,
        new: claim_commit,
    };
    let firings2 = detector.fire(&transition2).unwrap();
    assert_eq!(
        firings2
            .iter()
            .map(|f| f.effect.name.as_str())
            .collect::<Vec<_>>(),
        vec!["release_on_green"],
        "the release effect's `when` derives a new row from the action-record claim"
    );

    // Fire `release_on_green`: gate checks, boundary checks, and
    // `refs/release/candidate` fast-forwards to the resolved target -- here,
    // `HEAD`, resolved by the test rather than by this crate (D1: `to` is
    // unevaluated rule-language text).
    let effect_tree = effects.tree_hash(&effect2).unwrap();
    let head_oid = gix::ObjectId::from_hex(head.as_bytes()).unwrap();
    let refs = gix_store::GixRefStore::new(&repo);
    let outcome2 = fire_advance(
        &repo,
        &refs,
        &records,
        &effect2,
        effect_tree,
        &transition2,
        head_oid,
    )
    .unwrap();
    assert!(outcome2.was_recorded());

    let candidate = repo
        .find_reference("refs/release/candidate")
        .unwrap()
        .id()
        .to_string();
    assert_eq!(candidate, head);

    assert_eq!(
        records.log().unwrap().len(),
        2,
        "one transition record per link in the chain"
    );
}

/// The fork-bomb counterfactual: if `release_on_green` also declared
/// `refs/claims/*` among its writes -- an executor side channel writing back
/// into the namespace its own trigger reads -- `define` refuses it, naming
/// the namespace, before either effect is written. This is
/// `DEVPLAN-effect.md`'s "fork-bomb freedom" made concrete: the graph this
/// test's happy path accepts is one edge away from the one this test proves
/// gets rejected.
#[test]
fn a_write_back_into_the_own_trigger_namespace_is_rejected_as_a_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::init(dir.path());
    fixture.commit("first");
    let repo = fixture.open();

    let effects = EffectStore::open(&repo).unwrap();
    effects.define(build_on_push()).unwrap();

    let mut cyclic = release_on_green();
    cyclic.writes.push("refs/claims/*".to_owned());
    let err = effects.define(cyclic).unwrap_err();
    let gix_effect::Error::Invalid { name, errors } = err else {
        panic!("expected Error::Invalid, got {err:?}");
    };
    assert_eq!(name, "release_on_green");
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, gix_query::EffectCheckError::Cycle { .. })),
        "expected a Cycle diagnostic, got {errors:?}"
    );
}
