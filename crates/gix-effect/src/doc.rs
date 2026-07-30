//! The effect doc: [`EffectDoc`], its store schema, and define-time
//! validation.
//!
//! `DEVPLAN-effect.md` D1: "The effect doc is the source of truth, stored at
//! `refs/meta/effects/<name>`." Everything here is that doc's shape and the
//! checks that must pass *before* it is written -- no trigger detection, no
//! gate evaluation, no executor, no transition log (Phase B1's scope stops at
//! `git effect define`; those are B2/B3, per `DEVPLAN-effect.md`'s "Phases").

use facet::Facet;
use gix_query::{
    EffectCheckError, EffectDecl, EffectUnderCheck, Namespace, body_read_namespaces,
    checked_program,
};
use gix_store::{
    GixRefStore, InvalidRefName, Kind, Layout, RefPath, RefPrefix, RefSegment, RepoStore, Typed,
};

/// Where effect docs live: `refs/meta/effects/<name>`.
///
/// `refs/meta` mirrors [`gix_query_rules::RuleStore`]'s choice -- an effect
/// doc is meta-program vocabulary in exactly the sense a rule module is, and
/// `gix-query-kernel` already classifies `refs/meta/effects/` as `effect_ref`
/// (D1), so this is the layout that ref classification already assumes
/// rather than a new one this crate would have to teach the kernel about.
/// Schemas stay at the store-wide default `refs/schema`, again matching
/// `RuleStore`, so one schema kind (`effects`) is enough -- there is no
/// per-repository reason for effect docs to publish their schema anywhere
/// else.
const DATA_PREFIX: &str = "refs/meta";
const SCHEMA_PREFIX: &str = "refs/schema";
const KIND: &str = "effects";

/// What an effect does when it fires.
///
/// A sum type by the doc's own design (`DEVPLAN-effect.md` D1: "`then: Then`
/// -- `Advance { ref_, to }` | `Run { executor, inputs }`"), and
/// `facet-git-tree`'s codec supports enums natively: an externally tagged
/// tree with one entry named after the active variant
/// (`git-store/crates/facet-git-tree/tests/variants.rs`). So this is exactly
/// the doc's shape, not a struct-with-a-discriminant workaround -- "advance
/// this ref" and "run this executor" are not two states of one record, they
/// are two different records, and `Then` makes the third case (both, or
/// neither) unrepresentable (`DEVPLAN.md` §2.12).
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
#[repr(u8)]
pub enum Then {
    /// Advance a ref straight to a value, with no executor in between --
    /// e.g. a merge gate fast-forwarding a target ref once its `gate` derives
    /// true.
    Advance {
        /// The ref this effect advances.
        ref_: String,
        /// What to advance it to. Rule-language vocabulary (an oid
        /// expression, a symbolic ref, etc.) belongs to whatever later phase
        /// interprets it -- B1 stores the text and does not evaluate it.
        to: String,
    },
    /// Invoke an executor and record what it produced. The executor seam
    /// (`Executor` trait, D6) does not exist yet; this variant records the
    /// doc's intent to call one by name, for B3 to wire up.
    Run {
        /// Which executor to invoke, by name.
        executor: String,
        /// The inputs handed to it, as rule-language expressions.
        inputs: Vec<String>,
    },
}

/// One effect, as it is stored at `refs/meta/effects/<name>`.
///
/// `when` and `gate` are rule-language source -- the same body language a
/// rule module is written in (`DEVPLAN-effect.md`'s opening line: "an effect
/// is a rule whose head is not derivable") -- parsed and validated by
/// `gix-query-parse`/`-check` at define time, never at this type's
/// construction: an `EffectDoc` can hold syntactically invalid text between
/// parse calls, same as a `RuleModuleRecord` can. What *cannot* happen is a
/// doc missing a `then` clause, or a `then` that is both an advance and a
/// run, or neither -- [`Then`] rules those out by construction (`DEVPLAN.md`
/// §2.12).
///
/// There is deliberately no `reads` field -- D2: reads are computed from
/// `when`+`gate`'s footprint, never declared, because a declaration can lie.
#[derive(Debug, Clone, PartialEq, Eq, Facet)]
pub struct EffectDoc {
    /// The effect's name, unique within the repository -- also its ref
    /// segment under `refs/meta/effects/`.
    pub name: String,
    /// Trigger predicate body: rule-language source watched for a newly
    /// derivable tuple on ref advance (D3, not implemented here).
    pub when: String,
    /// Authorization and condition body: rule-language source a transition
    /// must derive true through before it is admitted (D6/D7, not
    /// implemented here).
    pub gate: String,
    /// What firing does.
    pub then: Then,
    /// Declared write namespaces -- `gix-query-ir` [`Namespace`] syntax, one
    /// entry per pattern this effect (or any executor it invokes) may
    /// advance.
    pub writes: Vec<String>,
}

/// Everything that can go wrong defining an effect.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `name` is not usable as a ref segment.
    #[error(transparent)]
    InvalidName(#[from] InvalidRefName),
    /// The underlying `gix-store` operation failed.
    #[error(transparent)]
    Store(#[from] gix_store::Error),
    /// Computing the stored record's schema failed.
    #[error(transparent)]
    Schema(#[from] facet_git_tree::SchemaError),
    /// Assembling the checked rule program (host registry + every published
    /// module) that `when`/`gate` are validated against failed.
    #[error(transparent)]
    Query(#[from] gix_query::QueryError),
    /// Define-time validation rejected this effect -- a parse failure, a body
    /// that does not validate, or a cycle in the effect graph. Rejected
    /// *before* anything is written, per D1: "an effect that would create a
    /// cycle is rejected at define time, which is the earliest possible
    /// boundary."
    #[error("effect `{name}` failed validation:\n{}", join_errors(errors))]
    Invalid {
        /// The effect that was rejected.
        name: String,
        /// Every problem [`gix_query::check_effect_acyclicity`] found.
        errors: Vec<EffectCheckError>,
    },
    /// `when`'s computed footprint reaches a ref namespace that is not
    /// authoritative -- `DEVPLAN-effect.md` D3's last line: "Triggers read
    /// authoritative refs only," enforced here rather than by filtering at
    /// fire time, so a bad trigger is refused before it is ever stored.
    #[error(
        "effect `{name}`'s `when` reads `{namespace}`, which is not an authoritative ref \
         namespace -- `{}/ARCHITECTURE.md`'s rederive test: state that can be reconstructed \
         with zero information loss is derived, and derived state is never an effect trigger",
        "../git-anchor"
    )]
    NonAuthoritativeTrigger {
        /// The effect whose `when` reads a derived namespace.
        name: String,
        /// The derived namespace it reads.
        namespace: Namespace,
    },
}

fn join_errors(errors: &[EffectCheckError]) -> String {
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Storage and define-time validation for effect docs at
/// `refs/meta/effects/<name>`.
pub struct EffectStore<'r> {
    repo: &'r gix::Repository,
    store: RepoStore<'r>,
    kind: RefSegment,
}

impl<'r> EffectStore<'r> {
    /// Open the effect store over `repo`.
    ///
    /// # Errors
    /// Never today -- `KIND` is a compile-time-valid ref segment -- but
    /// returns `Result` rather than panicking, matching
    /// [`gix_query_rules::RuleStore::open`]'s shape so a future invalid
    /// caller-supplied kind (there is none yet) does not change this
    /// signature.
    pub fn open(repo: &'r gix::Repository) -> Result<Self, Error> {
        let layout = Layout {
            data: RefPrefix::new(DATA_PREFIX).expect("built-in ref prefix is valid"),
            schema: RefPrefix::new(SCHEMA_PREFIX).expect("built-in ref prefix is valid"),
        };
        let store = RepoStore::open_with_layout(repo, layout);
        let kind = RefSegment::new(KIND).expect("built-in ref segment is valid");
        Ok(Self { repo, store, kind })
    }

    fn kind(&self) -> Kind<'_, Typed<EffectDoc>, GixRefStore<'r>, &'r gix::OdbHandle> {
        self.store.kind(self.kind.clone())
    }

    /// Publish this crate's schema for the `effects` kind, unless the
    /// current one already matches.
    fn ensure_schema(&self) -> Result<(), Error> {
        let kind = self.kind();
        let doc = facet_git_tree::schema_of::<EffectDoc>()?;
        if kind.schema().get()?.as_ref() != Some(&doc) {
            kind.publish()?;
        }
        Ok(())
    }

    /// The current doc for `name`, or `None` if it has never been defined.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `name` is not a usable ref segment;
    /// [`Error::Store`] if the read fails.
    pub fn get(&self, name: &str) -> Result<Option<EffectDoc>, Error> {
        let path = RefPath::from(RefSegment::new(name)?);
        Ok(self.kind().get(&path)?)
    }

    /// Every currently defined effect's doc.
    ///
    /// # Errors
    /// [`Error::Store`] if listing or a read fails.
    pub fn list(&self) -> Result<Vec<EffectDoc>, Error> {
        let names = self.kind().list()?;
        names
            .into_iter()
            .map(|path| {
                self.kind()
                    .get(&path)?
                    .ok_or_else(|| unreachable!("`list` only returns names with a current value"))
            })
            .collect()
    }

    /// Validate `doc`, then write it to `refs/meta/effects/<doc.name>`.
    ///
    /// Validation runs in full *before* the write (D1's "earliest possible
    /// boundary"), so a rejected define leaves the repository untouched:
    ///
    /// 1. `when` and `gate` are parsed with `gix-query-parse` and validated
    ///    against the host registry, by [`gix_query::check_effect_acyclicity`]
    ///    itself -- it parses and checks each body before taking its
    ///    footprint, so a syntax or resolution error surfaces as
    ///    [`EffectCheckError::Parse`]/[`EffectCheckError::Body`] here.
    /// 2. The acyclicity check (`DEVPLAN-effect.md` C4, exported by
    ///    `gix-query`) runs over every already-registered effect plus this
    ///    one, so a `when`/`gate` whose footprint lands in this or any other
    ///    effect's declared write set is rejected, naming the namespace.
    ///
    /// # Errors
    /// [`Error::InvalidName`] if `doc.name` is not a usable ref segment;
    /// [`Error::Invalid`] if validation rejects it; [`Error::Query`] or
    /// [`Error::Store`] for a failure assembling the checked program or
    /// writing the doc.
    pub fn define(&self, doc: EffectDoc) -> Result<gix::ObjectId, Error> {
        let path = RefPath::from(RefSegment::new(doc.name.as_str())?);

        let checked = checked_program(self.repo)?;
        let mut effects: Vec<EffectUnderCheck> = self
            .list()?
            .into_iter()
            .filter(|existing| existing.name != doc.name)
            .map(under_check)
            .collect();
        effects.push(under_check(doc.clone()));

        if let Err(errors) = gix_query::check_effect_acyclicity(&checked, &effects) {
            return Err(Error::Invalid {
                name: doc.name,
                errors,
            });
        }

        let when_namespaces =
            body_read_namespaces(&checked, &doc.when).map_err(|error| Error::Invalid {
                name: doc.name.clone(),
                errors: vec![error],
            })?;
        for namespace in when_namespaces {
            if !is_authoritative(&namespace) {
                return Err(Error::NonAuthoritativeTrigger {
                    name: doc.name,
                    namespace,
                });
            }
        }

        self.ensure_schema()?;
        Ok(self.kind().put(&path, &doc)?)
    }

    /// The tree hash `doc` would compile to under this store's currently
    /// published schema, without writing anything.
    ///
    /// `DEVPLAN-effect.md` D5's idempotence key for a pure advance is
    /// `normal_form::hash({effect: <doc tree hash>, trigger: {ref, old,
    /// new}})` -- this is that `<doc tree hash>`, the same identity
    /// `define`'s commit binds, so an edited doc (a changed tree hash) is a
    /// new identity for idempotence purposes, exactly the "edited effect is a
    /// new effect" reading `DEVPLAN-effect.md`'s open questions section
    /// recommends.
    ///
    /// # Errors
    /// [`Error::Store`] if no schema is published yet, or compilation fails.
    pub fn tree_hash(&self, doc: &EffectDoc) -> Result<gix::ObjectId, Error> {
        Ok(self.kind().compile(doc)?)
    }
}

/// Ref namespace prefixes `../git-anchor/ARCHITECTURE.md`'s rederive test
/// classifies as *derived* rather than authoritative: state exactly
/// reconstructible with zero information loss, which the architecture says is
/// "[n]ever an attestation target, never an effect trigger."
///
/// Today that is exactly the cache namespace `DEVPLAN.md` Phase 6 has not
/// built yet (`DEVPLAN-effect.md` C5: "Cache refs stay ... flag-gated"): a
/// promoted, signed claim is what turns a cached answer authoritative, and
/// the cache ref itself stays a pure function of the query it caches. Every
/// namespace this crate's own tests use -- `refs/meta/*`, `refs/claims/*`,
/// `refs/schema`, and this crate's own `refs/intent/*`/`refs/effects/log` --
/// exists only because something was signed into it, so the rederive test
/// puts them on the authoritative side by default; this list names the
/// exception, not the rule.
const DERIVED_NAMESPACE_PREFIXES: &[&str] = &["refs/cache/"];

/// Whether `namespace` is authoritative under the rederive test above.
fn is_authoritative(namespace: &Namespace) -> bool {
    !DERIVED_NAMESPACE_PREFIXES
        .iter()
        .any(|prefix| namespace.as_str().starts_with(prefix))
}

/// Project a stored [`EffectDoc`] into the shape
/// [`gix_query::check_effect_acyclicity`] needs: the declared write set, and
/// the source of every body whose footprint contributes to the computed read
/// set (`when` and `gate` both -- D1: both are read at trigger and admission
/// time).
fn under_check(doc: EffectDoc) -> EffectUnderCheck {
    EffectUnderCheck {
        decl: EffectDecl::new(
            doc.name,
            doc.writes.into_iter().map(Namespace::new).collect(),
        ),
        bodies: vec![doc.when, doc.gate],
    }
}

#[cfg(test)]
mod tests {
    use test_support::Fixture;

    use super::*;

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

    /// A valid effect doc round-trips through the store exactly: define it,
    /// read it back, and every field -- including the `Then` sum -- matches
    /// what was written. This is the enum-codec question `DEVPLAN-effect.md`
    /// flags as a real design constraint, pinned as a passing test rather
    /// than an assumption.
    #[test]
    fn a_valid_effect_doc_round_trips_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        let written = doc(
            "advance-main",
            "member(M)",
            "signer(C, M)",
            &["refs/heads/candidate"],
        );
        store.define(written.clone()).unwrap();

        let read = store.get("advance-main").unwrap().unwrap();
        assert_eq!(read, written);
    }

    /// `DEVPLAN-effect.md`'s definition of done, item 3: an effect whose
    /// `when` body's footprint lands in its own declared write set is
    /// rejected at `define`, naming the namespace -- the acyclicity check
    /// applied to the single-effect self-loop case.
    #[test]
    fn an_effect_that_would_read_its_own_write_set_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        // `member/1` is a host predicate backed by `RefGlob("refs/meta/members/*")`
        // (gix-query-ir's registry), so its footprint maps to a ref
        // namespace the acyclicity check can compare against `writes`.
        let self_reading = doc(
            "self-loop",
            "member(M)",
            "signer(C, M)",
            &["refs/meta/members/*"],
        );

        let err = store.define(self_reading).unwrap_err();
        let Error::Invalid { name, errors } = err else {
            panic!("expected Error::Invalid, got {err:?}");
        };
        assert_eq!(name, "self-loop");
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, EffectCheckError::Cycle { .. })),
            "expected a Cycle diagnostic, got {errors:?}"
        );
    }

    /// A rejected define must leave the repository untouched: no partial
    /// write, no schema published, no ref created for the name that failed.
    #[test]
    fn a_rejected_define_leaves_no_ref_behind() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::init(dir.path());
        let repo = fixture.open();
        let store = EffectStore::open(&repo).unwrap();

        let self_reading = doc(
            "never-written",
            "member(M)",
            "signer(C, M)",
            &["refs/meta/members/*"],
        );
        assert!(store.define(self_reading).is_err());

        assert_eq!(store.get("never-written").unwrap(), None);
        assert!(
            store.list().unwrap().is_empty(),
            "no effect doc should have been written"
        );
    }

    /// D3: "Triggers read authoritative refs only." `refs/cache/*` is
    /// `DEVPLAN-effect.md` C5's still-unbuilt cache namespace -- the one
    /// concrete "delete it, rederive it losslessly" example
    /// `../git-anchor/ARCHITECTURE.md`'s Core invariant names -- and every
    /// namespace this crate's own effect docs, intents, and transition log
    /// use (`refs/meta/*`, `refs/claims/*`, `refs/intent/*`,
    /// `refs/effects/log/*`) is on the authoritative side, because each
    /// exists only because something was signed into it.
    ///
    /// This pins the classification directly rather than end to end through
    /// `define`: no EDB predicate is backed by `refs/cache/*` yet (cache
    /// refs are `DEVPLAN.md` Phase 6, unbuilt), so no rule-language body's
    /// footprint can reach it today, and there is nothing to write a `when`
    /// against. `define`'s own call to [`is_authoritative`] (see its
    /// `when_namespaces` loop) is what turns this into a rejection the
    /// moment such a predicate exists -- no code change needed here when it
    /// does.
    #[test]
    fn derived_namespaces_are_not_authoritative() {
        assert!(!is_authoritative(&Namespace::new(
            "refs/cache/query/abc123"
        )));
        assert!(!is_authoritative(&Namespace::new("refs/cache/*")));
        assert!(is_authoritative(&Namespace::new("refs/meta/members/*")));
        assert!(is_authoritative(&Namespace::new("refs/claims/*")));
        assert!(is_authoritative(&Namespace::new("refs/effects/log/*")));
        assert!(is_authoritative(&Namespace::new("refs/intent/*")));
    }
}
