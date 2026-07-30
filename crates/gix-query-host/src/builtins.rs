//! [`AllBuiltins`]: every builtin a real repo query needs, combined into one
//! [`Builtin`].
//!
//! `gix_query_eval::run_to_fixpoint` takes exactly one `&dyn Builtin`, and the
//! two builtin implementations that exist -- [`RepoBuiltins`] (`cand`) and
//! [`gix_query_ir::Comparisons`] (the six comparison operators, which need no
//! repository) -- live in different crates for exactly the reason
//! `gix-query-ir`'s module doc gives: comparing two ground values does not
//! touch a repository, so it has no business depending on `gix`. A caller
//! wiring an actual evaluation needs both, so this composes them behind one
//! dispatch by predicate key.

use gix_query_ir::{Bindings, Builtin, Comparisons, HostError, PredicateKey, Value};

use crate::cand::RepoBuiltins;

/// [`RepoBuiltins`] and [`Comparisons`], dispatched by predicate key.
///
/// The two never provide the same key -- `cand` is arity 5, every comparison
/// is arity 2 and named after a [`gix_query_ir::CompareOp`] -- so which one
/// answers a call is unambiguous by construction, not by precedence.
pub struct AllBuiltins<'r> {
    repo: RepoBuiltins<'r>,
    cmp: Comparisons,
}

impl<'r> AllBuiltins<'r> {
    /// Every builtin over `repo`.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self {
            repo: RepoBuiltins::new(repo),
            cmp: Comparisons,
        }
    }
}

impl Builtin for AllBuiltins<'_> {
    fn provides(&self, key: &PredicateKey) -> bool {
        self.repo.provides(key) || self.cmp.provides(key)
    }

    fn call(
        &self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError> {
        if self.repo.provides(key) {
            self.repo.call(key, bound, out)
        } else {
            self.cmp.call(key, bound, out)
        }
    }

    fn holds(&self, key: &PredicateKey, args: &[Value]) -> Result<bool, HostError> {
        if self.repo.provides(key) {
            self.repo.holds(key, args)
        } else {
            self.cmp.holds(key, args)
        }
    }
}
