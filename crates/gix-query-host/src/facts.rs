//! [`RepoFacts`]: the single [`FactSource`] over every repo-backed EDB
//! relation the registry declares.
//!
//! One [`FactSource`] rather than one per predicate because the trait itself
//! is dispatched by predicate key (`provides`, then `facts`), so splitting it
//! further would just move the same `match` into the evaluator. Each arm
//! delegates to the sibling module that owns that predicate's storage layout,
//! keeping this file a table of contents rather than an implementation.

use gix_query_ir::{Bindings, FactSource, Facts, HostError, PredicateKey};

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
/// [`gix::Repository`].
pub struct RepoFacts<'r> {
    repo: &'r gix::Repository,
}

impl<'r> RepoFacts<'r> {
    /// A fact source over `repo`.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self { repo }
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
            ("member", 1) => member::member_facts(self.repo, key, bound, attribute),
            ("revoked", 1) => member::revoked_facts(self.repo, key, bound, attribute),
            ("claim", 1) => claim::claim_facts(self.repo, key, bound, attribute),
            ("kind", 2) | ("target", 2) | ("signer", 2) | ("verdict", 2) => {
                claim::trailer_facts(self.repo, key, bound, attribute)
            }
            ("anchor", 1) => anchor::facts(self.repo, key, bound, attribute),
            ("line", 2) => line::facts(self.repo, key, bound, attribute),
            _ => Err(HostError::NoProvider(key.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_anchor::{Binding, Store};
    use gix_query_ir::{Backing, PredicateClass, host_predicates};
    use test_support::Fixture;

    /// Every EDB predicate the registry declares has a provider here. This is
    /// what makes the registry's mode sets testable rather than aspirational:
    /// a predicate added to `host_predicates()` with no matching arm here
    /// fails this test instead of failing silently at query time.
    #[test]
    fn every_registered_edb_predicate_is_provided() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let facts = RepoFacts::new(&repo);

        for entry in host_predicates() {
            assert!(
                facts.provides(&entry.key),
                "{} has no host provider",
                entry.key
            );
        }
    }

    /// Every declared `Backing::RefGlob`, resolved through git's own glob
    /// semantics, must actually match the refs the corresponding provider
    /// reads. This is what would have caught the anchor glob bug:
    /// `refs/anchors/*` parses, and `provides`/`facts` disagreeing about it
    /// is invisible to every other test, but `git for-each-ref refs/anchors/*`
    /// matches nothing at all, because a single `*` stops at a slash and
    /// `gix_anchor::Store` writes `refs/anchors/<target>/<id>` -- one level
    /// deeper. `Fixture::git`'s own `for-each-ref` is the oracle, not this
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

        let claim_message =
            format!("review\n\nKind: review\nTarget: {blob}\nSigner: {member}\nVerdict: approve\n");
        let claim_id = f.commit(&claim_message);
        f.update_ref(&format!("refs/meta/claims/{claim_id}"), &claim_id);

        let repo = f.open();
        let anchor = gix_anchor::capture(&repo, "HEAD", "a.txt", None).expect("capture");
        Store::open(&repo)
            .attach(&Binding::Position(anchor), b"a claim body", None)
            .expect("attach anchor");

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

            for row in &rows {
                let source_ref = row.source_ref.as_deref().unwrap_or_else(|| {
                    panic!(
                        "{} emitted an unattributed fact with attribute: true",
                        entry.key
                    )
                });
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
