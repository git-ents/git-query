//! [`RepoFacts`]: the single [`FactSource`] over every repo-backed EDB
//! relation the registry declares.
//!
//! One [`FactSource`] rather than one per predicate because the trait itself
//! is dispatched by predicate key (`provides`, then `facts`), so splitting it
//! further would just move the same `match` into the evaluator. Each arm
//! delegates to the sibling module that owns that predicate's storage layout,
//! keeping this file a table of contents rather than an implementation.

use gix_query_ir::{Bindings, FactSource, Facts, HostError, PredicateKey};
use gix_refstore::RefName;

use crate::refs::HostRefs;
use crate::{anchor, author, claim, commit, line, member, tree};

/// The EDB names and arities [`RepoFacts`] answers for -- kept as one list so
/// `provides` and the `facts` dispatch can never name a predicate the other
/// does not.
const PROVIDED: &[(&str, usize)] = &[
    ("commit", 1),
    ("parent", 2),
    ("tree_entry", 3),
    ("author", 2),
    ("member", 1),
    ("revoked", 1),
    ("claim", 1),
    ("kind", 2),
    ("target", 2),
    ("signer", 2),
    ("verdict", 2),
    ("anchor", 1),
    ("line", 2),
];

/// A [`FactSource`] reading every repo-backed EDB relation over one open
/// [`gix::Repository`] -- live, or as of a fixed set of ref override values
/// (`Some` = held this value, `None` = did not exist) when constructed via
/// [`RepoFacts::as_of`].
///
/// The override set is stored, not resolved into a [`HostRefs`] eagerly:
/// each dispatched predicate builds its own [`HostRefs`] fresh
/// ([`RepoFacts::refs`]), the same way `claim.rs`'s `ClaimReader` already
/// opened its own `gix-store` `Store` fresh per call before this existed --
/// so a query touching only content-addressed predicates (`commit`,
/// `parent`, `tree_entry`, `author`, `line`) never pays for a ref read it
/// does not make.
pub struct RepoFacts<'r> {
    repo: &'r gix::Repository,
    overrides: Vec<(RefName, Option<gix::ObjectId>)>,
}

impl<'r> RepoFacts<'r> {
    /// A fact source over `repo`, as it currently stands.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self {
            repo,
            overrides: Vec::new(),
        }
    }

    /// A fact source over `repo`, reading every ref-glob-backed relation
    /// (`member`, `revoked`, `claim`, `kind`, `target`, `signer`, `verdict`,
    /// `anchor`) as it would answer if every name in `overrides` held the
    /// value given there instead of its real one.
    ///
    /// This is the seam `gix-effect`'s trigger detection (`DEVPLAN-effect.md`
    /// D3) evaluates "at old" through, in place of rewinding and restoring a
    /// real ref: the repository is read, never written, to answer what a
    /// `when` body would have derived before a ref moved. Content-addressed
    /// predicates are unaffected by `overrides` -- they read the object
    /// database directly, never a ref namespace, so there is nothing here
    /// for an override to shadow.
    #[must_use]
    pub fn as_of(
        repo: &'r gix::Repository,
        overrides: impl IntoIterator<Item = (RefName, Option<gix::ObjectId>)>,
    ) -> Self {
        Self {
            repo,
            overrides: overrides.into_iter().collect(),
        }
    }

    /// A fresh ref view over this source's overrides -- live when there are
    /// none, matching [`HostRefs::live`]'s own contract that an empty
    /// override set answers identically to the repository itself.
    fn refs(&self) -> HostRefs<'r> {
        HostRefs::as_of(self.repo, self.overrides.iter().cloned())
    }
}

impl FactSource for RepoFacts<'_> {
    fn provides(&self, key: &PredicateKey) -> bool {
        PROVIDED
            .iter()
            .any(|(name, arity)| key.name.as_ref() == *name && key.arity == *arity)
    }

    fn facts<'a>(
        &'a self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        attribute: bool,
    ) -> Result<Facts<'a>, HostError> {
        match (key.name.as_ref(), key.arity) {
            ("commit", 1) => commit::facts(self.repo, key, bound, attribute),
            ("parent", 2) => commit::parent_facts(self.repo, key, bound, attribute),
            ("tree_entry", 3) => tree::facts(self.repo, key, bound, attribute),
            ("author", 2) => author::facts(self.repo, key, bound, attribute),
            ("member", 1) => member::member_facts(self.repo, &self.refs(), key, bound, attribute),
            ("revoked", 1) => member::revoked_facts(self.repo, &self.refs(), key, bound, attribute),
            ("claim", 1) => claim::claim_facts(self.repo, self.refs(), key, bound, attribute),
            ("kind", 2) | ("target", 2) | ("signer", 2) | ("verdict", 2) => {
                claim::envelope_facts(self.repo, self.refs(), key, bound, attribute)
            }
            ("anchor", 1) => anchor::facts(self.repo, &self.refs(), key, bound, attribute),
            ("line", 2) => line::facts(self.repo, key, bound, attribute),
            _ => Err(HostError::NoProvider(key.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facet::Facet;
    use gix_attest::Target;
    use gix_query_ir::{Backing, PredicateClass, Value, host_predicates};
    use test_support::{ClaimFixture, Fixture};

    /// Forge's review payload, stood in for by the one field `verdict/2`
    /// reads. See `claim.rs`'s own test module: the schema is derived from this
    /// shape, never transcribed.
    #[derive(Debug, Facet)]
    struct Review {
        verdict: String,
    }

    /// The mode vector requiring the fewest bound arguments among those the
    /// entry's `Edb` mode set supports -- the cheapest legal call, and the
    /// one every predicate must answer for regardless of which richer
    /// vectors it also supports.
    fn least_bound_vector(entry: &gix_query_ir::PredicateEntry) -> gix_query_ir::ModeVector {
        let PredicateClass::Edb { modes, .. } = &entry.class else {
            panic!("{} is not an EDB predicate", entry.key);
        };
        modes
            .vectors()
            .min_by_key(|v| v.required_bound().len())
            .cloned()
            .unwrap_or_else(|| panic!("{}'s mode set is empty", entry.key))
    }

    /// Every EDB predicate the registry declares has a provider here, and
    /// that provider actually answers for the predicate's least-bound legal
    /// mode -- not merely `provides()`, which only reads the `PROVIDED` list
    /// `facts()`'s dispatch `match` is supposed to mirror. The two used to be
    /// two lists in one comment's word: a predicate added to both
    /// `host_predicates()` and `PROVIDED` with no matching dispatch arm would
    /// still pass a `provides()`-only check, and only fail at query time with
    /// `HostError::NoProvider`. Calling `facts()` here is what turns that
    /// into a test failure instead, and it also gives this fixture -- built
    /// but previously never queried -- a purpose.
    #[test]
    fn every_registered_edb_predicate_is_provided() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let facts = RepoFacts::new(&repo);
        // A well-formed placeholder for every Rev-/Oid-/Member-typed bound
        // position a least-bound vector can demand -- none of these
        // predicates' least-bound vectors require a Symbol-typed argument
        // (the subject position is always an identity), so one hex-shaped
        // placeholder covers every case. It need not resolve to a real
        // object: every provider answers "no rows" for an object that does
        // not exist, never an error.
        let placeholder = Value::sym("0".repeat(40));

        for entry in host_predicates() {
            assert!(
                facts.provides(&entry.key),
                "{} has no host provider",
                entry.key
            );

            let vector = least_bound_vector(&entry);
            let bound: Vec<Option<Value>> = vector
                .modes()
                .iter()
                .map(|mode| match mode {
                    gix_query_ir::Mode::Bound => Some(placeholder.clone()),
                    gix_query_ir::Mode::Free => None,
                })
                .collect();

            facts
                .facts(&entry.key, &bound, false)
                .unwrap_or_else(|e| {
                    panic!(
                        "{} dispatch failed for its own least-bound mode {vector}: {e}",
                        entry.key
                    )
                })
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|e| panic!("{}: {e}", entry.key));
        }
    }

    /// Every declared `Backing::RefGlob`, resolved through git's own glob
    /// semantics, must actually match the refs the corresponding provider
    /// reads -- `Fixture::git`'s own `for-each-ref` is the oracle, not this
    /// crate's understanding of the pattern.
    #[test]
    fn declared_ref_globs_match_what_the_provider_actually_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let blob = f.blob_at(&head, "a.txt");

        // One real ref in every RefGlob-backed namespace.
        let member = f.hash_object("Test Member <member@example.com>");
        f.update_ref(&format!("refs/meta/members/{member}"), &head);
        let revoked_member = f.hash_object("Gone <gone@example.com>");
        let revoked_target = f.hash_object("revoked");
        f.update_ref(
            &format!("refs/meta/members/{revoked_member}"),
            &revoked_target,
        );

        // One real, signed claim, so every claim-backed relation -- including
        // `verdict/2`, which reads the review payload rather than the envelope
        // -- has at least one row to attribute.
        let claims = ClaimFixture::new(&f);
        claims.register_payload_schema::<Review>("review");
        let payload = claims.payload(
            "review",
            &Review {
                verdict: "approve".to_owned(),
            },
        );
        claims.sign(
            &Target {
                kind: "blob".to_owned(),
                id: gix::ObjectId::from_hex(blob.as_bytes())
                    .expect("hex blob")
                    .into(),
            },
            payload,
            "review",
        );

        let repo = f.open();
        let anchor = gix_anchor::capture(&repo, "HEAD", "a.txt", None).expect("capture");
        crate::anchor::attach(&repo, anchor).expect("attach anchor");
        drop(repo);

        let repo = f.open();
        let facts = RepoFacts::new(&repo);

        for entry in host_predicates() {
            let PredicateClass::Edb { backing, .. } = &entry.class else {
                continue;
            };
            let globs: Vec<&str> = backing
                .iter()
                .filter_map(|b| match b {
                    Backing::RefGlob(g) => Some(g.as_str()),
                    Backing::ContentAddressed | Backing::AdHoc => None,
                })
                .collect();
            if globs.is_empty() {
                continue;
            }

            let bound = vec![None; entry.key.arity];
            let rows: Vec<_> = facts
                .facts(&entry.key, &bound, true)
                .unwrap_or_else(|e| panic!("{}: {e}", entry.key))
                .collect::<Result<Vec<_>, _>>()
                .unwrap_or_else(|e| panic!("{}: {e}", entry.key));

            assert!(
                !rows.is_empty(),
                "{}: setup should have left at least one fact to attribute",
                entry.key
            );

            // The fixture's own refs -- the same shape and order the
            // footprint digest consumes -- so attribution is checked against
            // a ref git itself confirms exists, not merely against a glob
            // pattern. Every test elsewhere in this crate passes
            // `attribute: false`, which leaves `refname()` construction
            // entirely unexercised; this is the one place it runs.
            let real_refs = f.refs();
            for row in &rows {
                let source_ref = row.source_ref.as_deref().unwrap_or_else(|| {
                    panic!(
                        "{} emitted an unattributed fact with attribute: true",
                        entry.key
                    )
                });
                assert!(
                    real_refs.iter().any(|(name, _)| name == source_ref),
                    "{}'s emitted source_ref {source_ref} is not a ref \
                     Fixture::refs() reports the fixture actually has",
                    entry.key
                );
                let matched = globs.iter().any(|glob| {
                    f.git(&["for-each-ref", "--format=%(refname)", glob])
                        .lines()
                        .any(|line| line == source_ref)
                });
                assert!(
                    matched,
                    "{}'s emitted source_ref {source_ref} is not matched by its \
                     declared glob(s) {globs:?} under git's own for-each-ref resolution",
                    entry.key
                );
            }
        }
    }
}
