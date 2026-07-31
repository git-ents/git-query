//! Footprint-keyed query cache (`DEVPLAN.md` §1.1, §2.11, Phase 6).
//!
//! Off by default, and "off" is a type-level fact, not a runtime flag: the
//! plain [`crate::run_goal`]/[`crate::run_body`] entry points contain no
//! cache lookup at all. A caller opts in by holding a [`QueryCache`] and
//! calling [`crate::run_goal_cached`]/[`crate::run_body_cached`] instead --
//! there is no boolean to leave set wrong, only a call that either happened
//! or did not.
//!
//! # Scope
//!
//! One entry per whole checked, rewritten goal -- not per stratum. Nemo's
//! engine has no per-stratum boundary [`gix_query_eval::run_eager`] exposes
//! for a cache to sit around without reaching into the engine itself, so this
//! caches the coarser unit that is always safe to cache: a goal's complete
//! answer relation, keyed on exactly the inputs a per-stratum cache would
//! key each of its finer entries on. Narrowing the grain later is a
//! performance improvement, not a correctness fix.
//!
//! # Key
//!
//! `H(rules snapshot, goal, footprint ref snapshot, edb digest)`, each field
//! length-prefixed so no field's bytes can forge another field's boundary:
//!
//! - **rules snapshot** -- [`ref_snapshot_digest`] over the fixed glob
//!   `refs/meta/rules/*`, so any published rule module moving invalidates
//!   every entry, and nothing else does.
//! - **goal** -- [`gix_query_ir::Canonicalizer::goal`]'s canonical bytes,
//!   already alpha-equivalence- and author-order-aware; reused, not
//!   reimplemented.
//! - **footprint ref snapshot** -- [`ref_snapshot_digest`] over the goal's
//!   own footprint's ref globs ([`gix_query_check::footprint_of_body`]),
//!   narrower than the rules snapshot in the common case: a goal touching
//!   only content-addressed predicates resolves no globs at all.
//! - **edb digest** -- [`gix_query_ir::Registry::builtin_digest_input`] and
//!   [`gix_query_ir::Registry::edb_digest_input`]. The second matters once a
//!   consumer contributes its own base facts through
//!   [`crate::run_goal_with_cached`]: two registries agreeing on every
//!   builtin may still declare different EDB relations under the same names,
//!   and the answer to a goal over `refs/forge/issue/*` must not be served to
//!   a registry that never declared it.
//!
//! A goal whose footprint reaches a `Backing::AdHoc` predicate is refused a
//! key outright ([`CacheError::AdHocBacking`]) rather than keyed on refs that
//! do not account for it -- ad-hoc facts (`--edb name=file`) are not repo
//! state, so no ref snapshot can speak for them. Refusal means running
//! uncached, which is always correct.

use std::collections::HashMap;
use std::sync::Mutex;

use gix_query_ir::{CheckedProgram, Goal, Value};
use gix_refstore::{GixRefStore, RefPrefix, RefStore};
use sha2::{Digest, Sha256};

const RULES_GLOB: &str = "refs/meta/rules/*";

/// Everything that can go wrong computing a cache key. Never a correctness
/// failure by itself -- a caller that hits one should fall back to running
/// uncached, never guess at a key.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// A registered ref glob was not of the `<prefix>/*` shape
    /// [`gix_query_ir::Backing::RefGlob`] declarations always use.
    #[error("ref glob `{0}` is not of the form `<prefix>/*`")]
    MalformedGlob(String),
    /// Reading the ref namespace a glob names failed.
    #[error("reading refs for the cache key failed: {0}")]
    Refs(String),
    /// The goal reaches a predicate backed by an ad-hoc source, which no ref
    /// snapshot accounts for.
    #[error("predicate `{0}` is backed by an ad-hoc source and cannot be keyed on ref state")]
    AdHocBacking(String),
}

/// The cache key, per this module's doc. Opaque outside this module: nothing
/// but [`compute_cache_key`] may construct one, since every byte of it is
/// part of the soundness argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey([u8; 32]);

/// Append `bytes` to `hasher` length-prefixed, so two adjacent fields can
/// never be reread as a different split of the same bytes.
fn push_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// `H(sorted glob patterns, NUL-delimited, then for each (refname, oid) in
/// sorted-by-refname order: refname, 0x00, oid, 0x00)` -- `DEVPLAN.md` §1.1.
/// Glob patterns are hashed first so an absent namespace and an empty one
/// never collide, and a ref name is never NUL-delimiter-forgeable: Git ref
/// names cannot contain a NUL.
fn ref_snapshot_digest(repo: &gix::Repository, globs: &[String]) -> Result<[u8; 32], CacheError> {
    let mut sorted_globs: Vec<&str> = globs.iter().map(String::as_str).collect();
    sorted_globs.sort_unstable();
    sorted_globs.dedup();

    let store = GixRefStore::new(repo);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for glob in &sorted_globs {
        let prefix = glob
            .strip_suffix("/*")
            .ok_or_else(|| CacheError::MalformedGlob((*glob).to_owned()))?;
        let prefix =
            RefPrefix::new(prefix).map_err(|_| CacheError::MalformedGlob((*glob).to_owned()))?;
        let resolved = store
            .prefixed(&prefix)
            .map_err(|e| CacheError::Refs(e.to_string()))?;
        pairs.extend(
            resolved
                .into_iter()
                .map(|(name, oid)| (name.as_str().to_owned(), oid.to_string())),
        );
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    pairs.dedup_by(|a, b| a.0 == b.0);

    let mut h = Sha256::new();
    for g in &sorted_globs {
        h.update(g.as_bytes());
        h.update([0u8]);
    }
    for (name, oid) in &pairs {
        h.update(name.as_bytes());
        h.update([0u8]);
        h.update(oid.as_bytes());
        h.update([0u8]);
    }
    Ok(h.finalize().into())
}

/// The cache key for `goal` against `checked`'s program state and `repo`'s
/// current ref state.
fn compute_cache_key(
    repo: &gix::Repository,
    checked: &CheckedProgram,
    goal: &Goal,
) -> Result<CacheKey, CacheError> {
    let footprint = gix_query_check::footprint_of_body(&goal.body, checked);

    for entry in checked.registry().predicates() {
        if footprint.base.contains(&entry.key) && entry.is_ad_hoc() {
            return Err(CacheError::AdHocBacking(entry.key.name.to_string()));
        }
    }

    let footprint_globs: Vec<String> = footprint.ref_globs.into_iter().collect();

    let rules_snapshot = ref_snapshot_digest(repo, &[RULES_GLOB.to_owned()])?;
    let footprint_snapshot = ref_snapshot_digest(repo, &footprint_globs)?;
    let goal_bytes = gix_query_ir::Canonicalizer::goal(goal);
    let builtin_digest = checked.registry().builtin_digest_input();
    let edb_digest = checked.registry().edb_digest_input();

    let mut h = Sha256::new();
    push_field(&mut h, &rules_snapshot);
    push_field(&mut h, &goal_bytes);
    push_field(&mut h, &footprint_snapshot);
    push_field(&mut h, &builtin_digest);
    push_field(&mut h, &edb_digest);
    Ok(CacheKey(h.finalize().into()))
}

/// A footprint-keyed cache of goal answers, shared across calls through
/// [`crate::run_goal_cached`] and [`crate::run_body_cached`].
///
/// Construction is the opt-in: nothing implicitly creates one, and a
/// process that never constructs one never runs a byte of lookup code.
#[derive(Default)]
pub struct QueryCache {
    entries: Mutex<HashMap<CacheKey, Vec<Vec<Value>>>>,
}

impl QueryCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of entries currently cached, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Look up `goal`'s answer against `repo`/`checked`'s current state, or
    /// compute and store it via `run`.
    ///
    /// `run` is called at most once per miss; a hit never calls it at all.
    /// Errors from key computation fall back to running uncached rather than
    /// failing the query outright -- a cache is an optimization, and a
    /// caller should never be able to observe it behaving differently from
    /// running with none.
    pub(crate) fn get_or_run<E>(
        &self,
        repo: &gix::Repository,
        checked: &CheckedProgram,
        goal: &Goal,
        run: impl FnOnce() -> Result<Vec<Vec<Value>>, E>,
    ) -> Result<Vec<Vec<Value>>, E> {
        let key = compute_cache_key(repo, checked, goal).ok();

        if let Some(key) = &key
            && let Some(hit) = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(key)
        {
            return Ok(hit.clone());
        }

        let answer = run()?;

        if let Some(key) = key {
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key, answer.clone());
        }

        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_cache_is_empty() {
        assert!(QueryCache::new().is_empty());
    }
}
