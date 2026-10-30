//! [`HostRefs`]: the one ref-reading seam every EDB provider in this crate
//! goes through -- live by default, or a read-only overlay of a fixed set of
//! override values when constructed via [`HostRefs::as_of`].
//!
//! Before this, `member.rs` and `anchor.rs` read straight off
//! `gix::Repository` (`try_find_reference`, `references()?.prefixed(...)`),
//! duplicating "resolve one ref" and "list a ref namespace", which
//! `claim.rs` already had through `git-store`'s `Kind`/`Store`, itself
//! backed by [`gix_refstore::RefStore`]. Routing every module through this
//! one type is what makes [`crate::facts::RepoFacts::as_of`] possible at
//! all: an [`AsOfRefStore`] decorator answers every ref read in the crate
//! the same way, rather than three call sites each growing their own
//! override plumbing -- and it is `git-effect`'s replacement for rewinding
//! and restoring a real ref to answer "as it was before this ref moved"
//! (see that crate's `trigger.rs`, and `../git-store`'s `as_of.rs` doc
//! comment for why mutating the repository to answer a read is wrong in the
//! first place).
//!
//! `git_store::Store<R, O>`'s one inherent impl block requires both
//! `R: RefStore` and `R: Committer`, even for its read-only methods
//! (`kind()`, `dynamic()`) -- see that crate's `store.rs`. [`AsOfRefStore`]
//! implements only `RefStore` (identity is orthogonal to which refs are
//! visible, and a read-only view has no writer identity to report), so
//! [`HostRefs`] adds `Committer` by delegating to the same repository's own
//! [`GixRefStore`]. Every read still goes straight through
//! `AsOfRefStore::read`/`prefixed`, unmodified (D9's "calls, never
//! reimplements"); this type adds only the extra trait `git-store`
//! structurally demands alongside it, not any overlay logic of its own.

use gix::ObjectId;
use gix::actor::Signature;
use gix_refstore::{
    ApplyError, AsOfError, AsOfRefStore, Committer, GixError, GixRefStore, RefEdit, RefName,
    RefPrefix, RefStore,
};

/// A read view over one repository's refs -- live, or as of a fixed set of
/// override values (`Some` = held this value, `None` = did not exist).
///
/// Always an [`AsOfRefStore`] underneath, even for [`HostRefs::live`]: an
/// empty override set answers identically to reading `inner` directly (per
/// `AsOfRefStore`'s own contract), so there is exactly one code path here
/// rather than a live/as-of split every caller in this crate would
/// otherwise have to carry. `apply` therefore always fails with
/// [`AsOfError::ReadOnly`] -- correct even for the live case, since nothing
/// in this crate ever writes a ref (this crate's own module doc: read-only
/// EDB providers and builtins).
pub(crate) struct HostRefs<'r> {
    view: AsOfRefStore<GixRefStore<'r>>,
}

impl<'r> HostRefs<'r> {
    /// `repo`'s refs, unmodified.
    pub(crate) fn live(repo: &'r gix::Repository) -> Self {
        Self::as_of(repo, [])
    }

    /// `repo`'s refs, as they would read if every name in `overrides` held
    /// the value given there instead of its real one.
    pub(crate) fn as_of(
        repo: &'r gix::Repository,
        overrides: impl IntoIterator<Item = (RefName, Option<ObjectId>)>,
    ) -> Self {
        Self {
            view: AsOfRefStore::new(GixRefStore::new(repo), overrides),
        }
    }
}

impl RefStore for HostRefs<'_> {
    type Error = AsOfError<GixError>;

    fn read(&self, name: &RefName) -> Result<Option<ObjectId>, Self::Error> {
        self.view.read(name)
    }

    fn prefixed(&self, prefix: &RefPrefix) -> Result<Vec<(RefName, ObjectId)>, Self::Error> {
        self.view.prefixed(prefix)
    }

    fn apply(&self, edit: RefEdit) -> Result<(), ApplyError<Self::Error>> {
        self.view.apply(edit)
    }

    fn apply_batch(&self, edits: Vec<RefEdit>) -> Result<(), ApplyError<Self::Error>> {
        self.view.apply_batch(edits)
    }
}

impl Committer for HostRefs<'_> {
    type Error = GixError;

    fn signature(&self) -> Result<Signature, Self::Error> {
        self.view.inner().signature()
    }

    fn author(&self) -> Result<Signature, Self::Error> {
        self.view.inner().author()
    }
}
