//! Trigger detection (D3) and intent refs (D4).
//!
//! `DEVPLAN-effect.md` D3: "on a ref transition `(ref, old, new)`, select the
//! effects whose *read footprint* overlaps the advanced ref ..., and
//! re-evaluate their `when` via `run_goal`; fire if it derives a tuple it did
//! not derive at `old`." Everything below is that mechanism, plus D4's
//! reification of push intents as refs.
//!
//! # The delta-restricted contract (D3)
//!
//! Nemo has no incremental entry point -- every demand round runs cold from
//! the current repository state -- so what runs here is *delta-restricted
//! re-evaluation*: [`TriggerDetector::candidates`] narrows the effect set to
//! those whose `when` footprint overlaps the transitioning ref (reusing
//! [`git_query::check_effect_acyclicity`]'s footprint-to-namespace projection
//! via [`git_query::body_read_namespaces`], D9's "calls, never reimplements"),
//! and [`TriggerDetector::fire`] re-evaluates only *those* effects' `when`
//! bodies, once against `self.repo` as it stands (`new`) and once against a
//! read-only as-of view of it with only the transitioning ref overridden
//! back to `old` (`git_query::run_body_as_of`) -- not the whole EDB, and not
//! every registered effect, and -- since D9 forbids reimplementing ref
//! mechanics here -- not by writing to the repository to find out either
//! (see [`TriggerDetector::fire`]'s own doc for why an earlier version of
//! this module did, and why that was wrong).
//!
//! This is explicitly a performance ceiling, not a semantic one
//! (`DEVPLAN-effect.md` D3): the observable contract is "fire iff `when`
//! derives a tuple at `new` it did not derive at `old`," and that contract is
//! what a future engine-level semi-naive delta path must continue to satisfy.
//! Swapping the re-evaluation strategy changes nothing in [`EffectDoc`], and
//! nothing a caller of [`TriggerDetector::fire`] observes -- only how the
//! answer is computed.
//!
//! Triggers read authoritative refs only, per D3's last line -- enforced as a
//! *validation error* at `EffectStore::define` time
//! ([`crate::doc::Error::NonAuthoritativeTrigger`]), not filtered here at
//! fire time: an effect whose `when` could not have been defined never
//! reaches this module at all.

use std::collections::BTreeMap;

use git_query::{Namespace, RefName, Value};
use gix_store::{
    GixRefStore, InvalidRefName, Kind, Layout, RefPath, RefPrefix, RefSegment, RepoStore, Typed,
};

use crate::doc::{EffectDoc, EffectStore};

/// A ref transition: `(ref, old, new)`. `old` is `None` for a ref creation --
/// the kernel's own [`gix_query_kernel::GateRules::ref_create`]/`ref_advance`
/// split (`gix-query-kernel/src/lib.rs`), reused here rather than folding a
/// creation into an advance with a sentinel old tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// The ref that moved.
    pub ref_: String,
    /// Its previous tip, or `None` if this transition created it.
    pub old: Option<gix::ObjectId>,
    /// Its new tip.
    pub new: gix::ObjectId,
}

/// Everything that can go wrong detecting or firing a trigger.
#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    /// Loading effect docs or running a body against the checked program
    /// failed.
    #[error(transparent)]
    Query(#[from] git_query::QueryError),
    /// One of the candidate effects' `when` bodies failed to parse or
    /// validate -- it was accepted at `define` time against a program that
    /// has since changed shape (a rule module was edited or removed).
    #[error(transparent)]
    Check(#[from] git_query::EffectCheckError),
    /// The effect store itself failed to open, list, or read a doc.
    #[error(transparent)]
    Doc(#[from] crate::doc::Error),
    /// `transition.ref_` is not a legal Git ref name.
    #[error(transparent)]
    InvalidRefName(#[from] InvalidRefName),
}

/// A candidate effect that fired: its doc, and the rows its `when` derived at
/// `new` that it did not derive at `old`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firing {
    /// The effect that fired.
    pub effect: EffectDoc,
    /// The newly derivable rows -- `when`'s answer at `new`, minus its answer
    /// at `old`.
    pub new_rows: Vec<Vec<Value>>,
}

/// Selects and re-evaluates effects on a ref transition, per D3.
pub struct TriggerDetector<'r> {
    repo: &'r gix::Repository,
}

impl<'r> TriggerDetector<'r> {
    /// A detector over `repo`.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self { repo }
    }

    /// Every currently defined effect whose `when` footprint overlaps
    /// `transition.ref_` -- the footprint-narrowed candidate set D3 fires
    /// against, computed the same way [`git_query::check_effect_acyclicity`]
    /// computes read sets (reused, not reimplemented, per D9).
    ///
    /// # Errors
    /// See [`TriggerError`].
    pub fn candidates(&self, transition: &Transition) -> Result<Vec<EffectDoc>, TriggerError> {
        let checked = git_query::checked_program(self.repo)?;
        let store = EffectStore::open(self.repo)?;
        let advanced = Namespace::new(transition.ref_.clone());

        let mut out = Vec::new();
        for doc in store.list()? {
            let reads = git_query::body_read_namespaces(&checked, &doc.when)?;
            if reads.iter().any(|namespace| namespace.overlaps(&advanced)) {
                out.push(doc);
            }
        }
        Ok(out)
    }

    /// Re-evaluate every candidate's `when` at `transition.old` and at
    /// `transition.new`, returning one [`Firing`] per candidate that derived
    /// a row at `new` it did not derive at `old`.
    ///
    /// The comparison is bounded by the footprint that produced the
    /// candidate set, not by the whole EDB: only `transition.ref_` moves for
    /// the "before" snapshot, every other ref stays exactly as it is in
    /// `self.repo` right now.
    ///
    /// "At old" is answered through [`git_query::run_body_as_of`], an
    /// as-of view over `transition.ref_` alone (`gix_refstore::AsOfRefStore`,
    /// via `git-store`) -- **not** by rewinding the real ref and restoring it
    /// after, which this used to do. That mutated the repository to answer a
    /// read: it wrote synthetic reflog entries, and an `old = None` (ref
    /// creation) transition rewound by *deleting* the ref outright, briefly
    /// making it absent to any concurrent reader -- unacceptable on the
    /// server, where a [`gix_refstore::RefStore`] is D4's ref transaction
    /// manager, not just a test fixture's convenience. A restore-on-`Drop`
    /// guard also does not survive a panic-with-abort, a signal, or process
    /// death, so a crash mid-window could leave a user's ref rewound or
    /// deleted for good. The as-of view has none of these failure modes: it
    /// issues no write at all (`fire` performs zero of them, pinned by this
    /// module's tests), so there is nothing to restore and nothing for a
    /// concurrent reader to observe.
    ///
    /// # Errors
    /// See [`TriggerError`].
    pub fn fire(&self, transition: &Transition) -> Result<Vec<Firing>, TriggerError> {
        let candidates = self.candidates(transition)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let name = RefName::new(transition.ref_.as_str())?;

        // "at new": the repository's current, already-transitioned state.
        let mut after = BTreeMap::new();
        for doc in &candidates {
            after.insert(doc.name.clone(), git_query::run_body(self.repo, &doc.when)?);
        }

        // "at old": the same repository, read through a view where only
        // `transition.ref_` differs -- overridden to `old`, or to "did not
        // exist" when `old` is `None`.
        let mut before = BTreeMap::new();
        for doc in &candidates {
            let rows =
                git_query::run_body_as_of(self.repo, &doc.when, [(name.clone(), transition.old)])?;
            before.insert(doc.name.clone(), rows);
        }

        let mut firings = Vec::new();
        for doc in candidates {
            let old_rows = before.remove(&doc.name).unwrap_or_default();
            let new_rows = after.remove(&doc.name).unwrap_or_default();
            let fresh: Vec<Vec<Value>> = new_rows
                .into_iter()
                .filter(|row| !old_rows.contains(row))
                .collect();
            if !fresh.is_empty() {
                firings.push(Firing {
                    effect: doc,
                    new_rows: fresh,
                });
            }
        }
        Ok(firings)
    }
}

/// One requested transition, reified as a doc -- `DEVPLAN-effect.md` D4:
/// "Push intents are refs; the effect system is the ref transaction manager."
///
/// Stored at `refs/intent/<target-ref>/<n>` (D4), a signed ref through
/// store's `Signer` seam like any other typed doc -- gates trigger on
/// intents exactly like any other ref advance ([`TriggerDetector`] does not
/// distinguish an intent ref from any other authoritative ref; nothing about
/// this type is special-cased into trigger detection).
#[derive(Debug, Clone, PartialEq, Eq, facet::Facet)]
pub struct Intent {
    /// The ref this intent requests a transition on.
    pub ref_: String,
    /// The tip it expects that ref to currently be at, hex-encoded, or
    /// `None` to request a creation.
    pub expected: Option<String>,
    /// The tip it requests the ref advance to, hex-encoded.
    pub to: String,
}

/// Everything that can go wrong recording or consuming an intent.
#[derive(Debug, thiserror::Error)]
pub enum IntentError {
    /// `target_ref` is not usable as ref-path material.
    #[error(transparent)]
    InvalidName(#[from] InvalidRefName),
    /// The underlying `gix-store` operation failed.
    #[error(transparent)]
    Store(#[from] gix_store::Error),
    /// Computing the stored record's schema failed.
    #[error(transparent)]
    Schema(#[from] facet_git_tree::SchemaError),
}

/// Storage for intent refs at `refs/intent/<target-ref>/<n>`.
///
/// `<n>` here is the commit's own object id rather than an incrementing
/// integer: `gix-store`'s `Kind::write(..).anonymous_under(group)` primitive
/// (`gix-store/src/kind.rs`) already gives a collision-free, append-only name
/// under a group prefix -- reusing it is D9's "calls, never reimplements"
/// applied to naming, not just to evaluation; hand-rolling a sequence counter
/// with CAS-retry would duplicate exactly what that primitive is for. Every
/// intent still lists, in full, under its target ref via
/// [`IntentStore::list_under`], which is the only thing D4 needs `<n>` for --
/// a name unique enough to coexist with sibling intents, not a total order.
pub struct IntentStore<'r> {
    repo: &'r gix::Repository,
    store: RepoStore<'r>,
    kind: RefSegment,
}

impl<'r> IntentStore<'r> {
    /// Open the intent store over `repo`, signing every write with `signer`
    /// -- the machine or human identity requesting the transition.
    ///
    /// # Errors
    /// Never today; see [`EffectStore::open`]'s note on why this still
    /// returns `Result`.
    pub fn open(
        repo: &'r gix::Repository,
        signer: impl gix_store::Signer + 'static,
    ) -> Result<Self, IntentError> {
        let layout = Layout {
            data: RefPrefix::new("refs").expect("built-in ref prefix is valid"),
            schema: RefPrefix::new("refs/schema").expect("built-in ref prefix is valid"),
        };
        let store = RepoStore::open_with_layout(repo, layout).with_signer(signer);
        let kind = RefSegment::new("intent").expect("built-in ref segment is valid");
        Ok(Self { repo, store, kind })
    }

    fn kind(&self) -> Kind<'_, Typed<Intent>, GixRefStore<'r>, &'r gix::OdbHandle> {
        self.store.kind(self.kind.clone())
    }

    /// Publish this crate's schema for the `intent` kind, unless the current
    /// one already matches.
    fn ensure_schema(&self) -> Result<(), IntentError> {
        let kind = self.kind();
        let doc = facet_git_tree::schema_of::<Intent>()?;
        if kind.schema().get()?.as_ref() != Some(&doc) {
            kind.publish()?;
        }
        Ok(())
    }

    /// Record `intent` under its target ref, returning the ref it landed at.
    ///
    /// # Errors
    /// [`IntentError::InvalidName`] if `intent.ref_` is not usable ref-path
    /// material; otherwise a store failure.
    pub fn request(&self, intent: Intent) -> Result<RefPath, IntentError> {
        self.ensure_schema()?;
        let group = RefPath::new(intent.ref_.as_str())?;
        let commit = self.kind().write(&intent).anonymous_under(&group)?;
        Ok(gix_store::entity_name_under(&group, commit))
    }

    /// The intent at `path`, or `None` once it has been consumed.
    ///
    /// # Errors
    /// A store read failure.
    pub fn get(&self, path: &RefPath) -> Result<Option<Intent>, IntentError> {
        Ok(self.kind().read(path)?.value())
    }

    /// Every currently outstanding intent targeting `target_ref`.
    ///
    /// # Errors
    /// [`IntentError::InvalidName`] if `target_ref` is not usable ref-path
    /// material; otherwise a store failure.
    pub fn list_under(&self, target_ref: &str) -> Result<Vec<RefPath>, IntentError> {
        let group = RefPath::new(target_ref)?;
        Ok(self.kind().list_under(&group)?)
    }

    /// Consume the intent at `path` -- "either way [admitted or not] the
    /// intent ref is consumed" (D4). Returns whether it was still present.
    ///
    /// # Errors
    /// A store failure.
    pub fn consume(&self, path: &RefPath) -> Result<bool, IntentError> {
        Ok(self.kind().remove(path)?)
    }

    /// The repository this store is open over -- exposed so a caller composes
    /// [`TriggerDetector`] over the same repository an intent was written to,
    /// without opening a second handle.
    #[must_use]
    pub fn repo(&self) -> &'r gix::Repository {
        self.repo
    }
}

#[cfg(test)]
mod tests {
    use test_support::Fixture;

    use super::*;
    use crate::doc::{EffectDoc, EffectStore, Then};

    fn doc(name: &str, when: &str, gate: &str, writes: &[&str]) -> EffectDoc {
        EffectDoc {
            name: name.to_owned(),
            when: when.to_owned(),
            gate: gate.to_owned(),
            then: Then::Advance {
                ref_: "refs/heads/main".to_owned(),
                to: "HEAD".to_owned(),
            },
            writes: writes.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// A minimal `Signer` for tests that only need *some* signature bytes
    /// present -- `ClaimFixture` (`crates/test-support/src/claim.rs`) is the
    /// model when a test needs `git verify-commit` to actually pass; the
    /// intent and record tests here only assert on what `gix-store` reads
    /// back, so a fixed byte string is enough to exercise the `Signer` seam
    /// itself without re-deriving `ClaimFixture`'s `ssh-keygen` plumbing.
    struct FixedSigner;

    impl gix_store::Signer for FixedSigner {
        type Error = std::convert::Infallible;

        fn sign(&self, _bytes: &[u8]) -> Result<gix_store::SignatureBytes, Self::Error> {
            Ok(gix_store::SignatureBytes::from(b"test-signature".to_vec()))
        }
    }

    /// D3: a ref advance selects exactly the effects whose `when` footprint
    /// overlaps it -- an effect reading `refs/meta/members/*` is a candidate
    /// for a transition on a member ref; an effect reading `refs/claims/*`
    /// is not, even though both are registered.
    #[test]
    fn a_transition_selects_exactly_the_overlapping_effects() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        store
            .define(doc(
                "watches-members",
                "member(M)",
                "member(M)",
                &["refs/meta/candidates/*"],
            ))
            .unwrap();
        store
            .define(doc(
                "watches-claims",
                "claim(C)",
                "claim(C)",
                &["refs/meta/claimresults/*"],
            ))
            .unwrap();

        let detector = TriggerDetector::new(&repo);
        let member_hex = fixture.commit("member commit");
        let transition = Transition {
            ref_: format!("refs/meta/members/{member_hex}"),
            old: None,
            new: gix::ObjectId::from_hex(member_hex.as_bytes()).unwrap(),
        };

        let candidates = detector.candidates(&transition).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["watches-members"],
            "only the member-reading effect should be a candidate"
        );
    }

    /// D3: firing compares `when`'s answer at `old` against `new`, bounded to
    /// the transitioning ref -- creating a member ref makes `member(M)`
    /// derive a row it did not derive before, so the watching effect fires.
    #[test]
    fn creating_a_member_ref_fires_the_watching_effect() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        store
            .define(doc(
                "watches-members",
                "member(M)",
                "member(M)",
                &["refs/meta/candidates/*"],
            ))
            .unwrap();

        let member_hex = fixture.commit("member commit");
        let member_oid = gix::ObjectId::from_hex(member_hex.as_bytes()).unwrap();
        fixture.update_ref(&format!("refs/meta/members/{member_hex}"), &member_hex);

        let detector = TriggerDetector::new(&repo);
        let transition = Transition {
            ref_: format!("refs/meta/members/{member_hex}"),
            old: None,
            new: member_oid,
        };

        let firings = detector.fire(&transition).unwrap();
        assert_eq!(firings.len(), 1, "{firings:?}");
        assert_eq!(firings[0].effect.name, "watches-members");
        assert_eq!(firings[0].new_rows.len(), 1);

        // `fire()` never touched the ref: it is still at `new`, where the
        // test itself put it. See `fire_performs_no_writes_even_for_a_ref_creation`
        // below for the stronger, whole-repository version of this check.
        let current = repo
            .find_reference(format!("refs/meta/members/{member_hex}").as_str())
            .unwrap()
            .id()
            .detach();
        assert_eq!(current, member_oid);
    }

    /// Pins the invariant this module exists to fix: **trigger detection
    /// performs zero writes**. `fire()` used to answer "at old" by rewinding
    /// the real ref and restoring it through a `Drop` guard -- and for `old =
    /// None` (a ref creation, this test's case), "rewind" meant *deleting*
    /// the ref outright for the duration of the call, briefly making it
    /// absent to any concurrent reader on a server where a ref store is the
    /// transaction manager (D4). The as-of view
    /// (`git_query::run_body_as_of`, `gix_refstore::AsOfRefStore`) answers
    /// the same question without touching the repository at all: every ref
    /// and the transitioning ref's own reflog are snapshotted before
    /// `fire()` and asserted byte-identical after.
    #[test]
    fn fire_performs_no_writes_even_for_a_ref_creation() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        store
            .define(doc(
                "watches-members",
                "member(M)",
                "member(M)",
                &["refs/meta/candidates/*"],
            ))
            .unwrap();

        let member_hex = fixture.commit("member commit");
        let member_oid = gix::ObjectId::from_hex(member_hex.as_bytes()).unwrap();
        let member_ref = format!("refs/meta/members/{member_hex}");
        fixture.update_ref(&member_ref, &member_hex);

        let detector = TriggerDetector::new(&repo);
        let transition = Transition {
            ref_: member_ref.clone(),
            old: None,
            new: member_oid,
        };

        let refs_before = fixture.refs();
        let reflog_before = fixture.git(&["reflog", "show", &member_ref]);

        let firings = detector.fire(&transition).unwrap();
        assert_eq!(
            firings.len(),
            1,
            "sanity: this is the same transition `creating_a_member_ref_fires_the_watching_effect` \
             fires on, so it must still fire here"
        );

        assert_eq!(
            fixture.refs(),
            refs_before,
            "fire() must not create, move, or delete any ref, including \
             `transition.ref_` itself -- not even transiently"
        );
        assert_eq!(
            fixture.git(&["reflog", "show", &member_ref]),
            reflog_before,
            "fire() must not write a synthetic reflog entry"
        );
    }

    /// The advance shape of the same invariant (`old = Some`, not a
    /// creation): rewinding a ref that already existed used to overwrite it
    /// with an `Update`, which is a real write with its own reflog entry
    /// even though the ref never goes absent the way the `None` case does.
    /// `fire()` must be just as write-free here.
    #[test]
    fn fire_performs_no_writes_for_a_ref_advance() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        store
            .define(doc(
                "watches-members",
                "member(M)",
                "member(M)",
                &["refs/meta/candidates/*"],
            ))
            .unwrap();

        let member_hex = fixture.commit("member commit");
        let member_oid = gix::ObjectId::from_hex(member_hex.as_bytes()).unwrap();
        let member_ref = format!("refs/meta/members/{member_hex}");
        fixture.update_ref(&member_ref, &member_hex);

        let detector = TriggerDetector::new(&repo);
        // A no-op advance is enough: the invariant under test is "no writes
        // happen", not "the effect fires" (that is
        // `an_effect_does_not_fire_when_nothing_new_is_derived`, right below).
        let transition = Transition {
            ref_: member_ref.clone(),
            old: Some(member_oid),
            new: member_oid,
        };

        let refs_before = fixture.refs();
        let reflog_before = fixture.git(&["reflog", "show", &member_ref]);

        detector.fire(&transition).unwrap();

        assert_eq!(
            fixture.refs(),
            refs_before,
            "fire() must not create, move, or delete any ref for an advance either"
        );
        assert_eq!(
            fixture.git(&["reflog", "show", &member_ref]),
            reflog_before,
            "fire() must not write a synthetic reflog entry for an advance either"
        );
    }

    /// D3: an effect whose `when` derives the *same* rows at `old` and `new`
    /// does not fire -- a transition on its watched ref that changes nothing
    /// observable is not a trigger.
    #[test]
    fn an_effect_does_not_fire_when_nothing_new_is_derived() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        store
            .define(doc(
                "watches-members",
                "member(M)",
                "member(M)",
                &["refs/meta/candidates/*"],
            ))
            .unwrap();

        let member_hex = fixture.commit("member commit");
        let member_oid = gix::ObjectId::from_hex(member_hex.as_bytes()).unwrap();
        fixture.update_ref(&format!("refs/meta/members/{member_hex}"), &member_hex);

        // Advance the *same* ref to the *same* tip -- a no-op transition.
        let detector = TriggerDetector::new(&repo);
        let transition = Transition {
            ref_: format!("refs/meta/members/{member_hex}"),
            old: Some(member_oid),
            new: member_oid,
        };

        assert!(detector.fire(&transition).unwrap().is_empty());
    }

    /// D4: an intent ref round-trips (the request lands where `list_under`
    /// finds it and `get` reads back what was written) and consuming it
    /// removes it -- "either way the intent ref is consumed."
    #[test]
    fn an_intent_ref_round_trips_and_is_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = IntentStore::open(&repo, FixedSigner).unwrap();

        let intent = Intent {
            ref_: "refs/heads/main".to_owned(),
            expected: None,
            to: "deadbeef".to_owned(),
        };
        let path = store.request(intent.clone()).unwrap();

        assert_eq!(store.get(&path).unwrap(), Some(intent));
        let under = store.list_under("refs/heads/main").unwrap();
        assert_eq!(under, vec![path.clone()]);

        assert!(store.consume(&path).unwrap());
        assert_eq!(store.get(&path).unwrap(), None);
        assert!(!store.consume(&path).unwrap(), "already consumed");
    }
}
