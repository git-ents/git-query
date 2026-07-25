//! [`RepoBuiltins`]: `bind` and `bind_fuzzy`, the two projection builtins
//! `query-and-rules.adoc`'s Anchor section leaves after `span_diff` and
//! `abbrev` are gone.
//!
//! `bind` is `project_candidates` -- exact projection, but every candidate
//! location rather than the one `project_exact` alone can represent -- so
//! that ambiguity is a property of the *result set* (several tuples for the
//! same `(Anchor, Rev)`) rather than a status this crate would otherwise have
//! to invent. `bind_fuzzy` is `project_from_context`, which cannot detect a
//! duplicate location at all and so never yields more than one tuple.
//!
//! Neither ever calls `gix_anchor::project`: that entry point silently
//! degrades to the context fallback depending on whether the anchor's own
//! commit has been garbage collected, which is exactly the ambient,
//! non-content-addressed state a cache key cannot carry (`query-and-rules.adoc`,
//! "Two entry points, and why"). `bind` calls `project_exact`'s family only,
//! so a garbage-collected anchor commit surfaces as a loud
//! [`HostError::Backend`] rather than a silent switch to the fuzzy heuristic.

use gix::ObjectId;
use gix_anchor::{Anchor, Binding, Projection, Store};
use gix_query_ir::{Bindings, Builtin, HostError, PredicateKey, Value};

use crate::util::{backend, backend_msg, count_lines, parse_oid, sym_oid};

/// A [`Builtin`] over `bind` and `bind_fuzzy`, sharing one open repository.
pub struct RepoBuiltins<'r> {
    repo: &'r gix::Repository,
}

impl<'r> RepoBuiltins<'r> {
    /// Projection builtins over `repo`.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self { repo }
    }
}

/// Which of the two projection builtins `key` names, or neither.
///
/// Parsed once per call and matched exhaustively, rather than the arity check
/// living in `provides` and a separate `== "bind_fuzzy"` string comparison
/// living in `call`: the two used to be able to disagree, and a `call` that
/// only inspects "is this fuzzy" (never "is this one of mine at all") runs
/// *any* arity-7 key with exact-projection semantics, including ones
/// `provides` itself would refuse. [`gix_query_ir::Comparisons::call`] gets
/// this right with `ok_or_else(NoProvider)`; this mirrors it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// `bind`, backed by `project_candidates` (the `project_exact` family).
    Exact,
    /// `bind_fuzzy`, backed by `project_from_context`.
    Fuzzy,
}

impl Entry {
    fn of(key: &PredicateKey) -> Option<Self> {
        if key.arity != 7 {
            return None;
        }
        match key.name.as_ref() {
            "bind" => Some(Self::Exact),
            "bind_fuzzy" => Some(Self::Fuzzy),
            _ => None,
        }
    }
}

impl Builtin for RepoBuiltins<'_> {
    fn provides(&self, key: &PredicateKey) -> bool {
        Entry::of(key).is_some()
    }

    fn call(
        &self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError> {
        let entry = Entry::of(key).ok_or_else(|| HostError::NoProvider(key.clone()))?;
        let (Some(Some(anchor_v)), Some(Some(rev_v))) = (bound.first(), bound.get(1)) else {
            return Err(HostError::UnsupportedMode { key: key.clone() });
        };
        let anchor_oid = parse_oid(key, anchor_v)?;
        let rev_oid = parse_oid(key, rev_v)?;

        let Some(anchor) = position_anchor(self.repo, key, anchor_oid)? else {
            return Ok(());
        };
        let Ok(target_commit) = self.repo.find_commit(rev_oid) else {
            return Ok(());
        };
        let target_tree = target_commit.tree().map_err(|error| backend(key, error))?;
        let rev_text = rev_oid.to_string();

        let projections = match entry {
            Entry::Fuzzy => match gix_anchor::project_from_context(self.repo, &anchor, &rev_text) {
                Ok(projection) => vec![projection],
                Err(error) => return Err(backend(key, error)),
            },
            Entry::Exact => gix_anchor::project_candidates(self.repo, &anchor, &rev_text)
                .map_err(|error| backend(key, error))?,
        };

        for projection in &projections {
            let (position, content) = projection.axes(&anchor);
            let (blob, start, end) = span(self.repo, key, &anchor, projection, &target_tree)?;
            let tuple = vec![
                anchor_v.clone(),
                rev_v.clone(),
                blob,
                Value::Int(start),
                Value::Int(end),
                Value::sym(position.label()),
                Value::sym(content.label()),
            ];
            if satisfies(bound, &tuple) {
                out.push(tuple);
            }
        }
        Ok(())
    }
}

/// Load `id` from the anchor store and return its [`Anchor`], or `None` when
/// no such note exists or it is not position-bound -- both are simply `bind`
/// holding no tuples, not a backend failure: a caller may name any oid, and
/// most will not be anchors.
fn position_anchor(
    repo: &gix::Repository,
    key: &PredicateKey,
    id: ObjectId,
) -> Result<Option<Anchor>, HostError> {
    let store = Store::open(repo);
    match store.get(id).map_err(|error| backend(key, error))? {
        Some(note) => match note.binding {
            Binding::Position(anchor) => Ok(Some(anchor)),
            Binding::Commit { .. }
            | Binding::Tree { .. }
            | Binding::Delta { .. }
            | Binding::Hybrid { .. } => Ok(None),
        },
        None => Ok(None),
    }
}

/// Whether every position `bound` fixes agrees with `tuple` -- binding more
/// than a builtin's required minimum is always legal (`mode.rs`'s
/// `satisfied_by`), so a caller that additionally bound, say, `Position` must
/// see only matching rows rather than every candidate.
fn satisfies(bound: Bindings<'_>, tuple: &[Value]) -> bool {
    bound
        .iter()
        .zip(tuple.iter())
        .all(|(want, have)| want.as_ref().is_none_or(|want| want == have))
}

/// `(Blob, Start, End)` for one projection outcome.
fn span(
    repo: &gix::Repository,
    key: &PredicateKey,
    anchor: &Anchor,
    projection: &Projection,
    target_tree: &gix::Tree<'_>,
) -> Result<(Value, i64, i64), HostError> {
    match projection {
        Projection::Current => {
            let (start, end) = line_span(repo, key, anchor.blob(), anchor.lines)?;
            Ok((sym_oid(anchor.blob()), start, end))
        }
        Projection::Relocated { path, lines } => {
            let blob = path_blob(target_tree, path).ok_or_else(|| {
                backend_msg(key, format!("relocated path {path} has no blob entry"))
            })?;
            let (start, end) = line_span(repo, key, blob, *lines)?;
            Ok((sym_oid(blob), start, end))
        }
        Projection::Outdated { path } => {
            // Content is edited (`axes` reports `Content::Edited` for every
            // `Outdated`, never `Content::None`), so there is no post-edit
            // line mapping to report; the pre-edit range against the
            // anchor's own blob is reported instead. `anchor.lines` is *not*
            // guaranteed `Some` here: `project_exact_onto`'s `!is_blob`
            // branch (`projection.rs:418`) and `project_from_context`'s
            // (`projection.rs:473`) both return `Outdated` before looking at
            // it, so a whole-file anchor reaches this arm exactly the same
            // as a line-range one whenever the path survives as a symlink or
            // a gitlink rather than a regular blob. `line_span` already
            // handles a `None` range by measuring the anchor's own blob, so
            // routing through it here rather than unwrapping directly is
            // what keeps that case from panicking.
            let (start, end) = line_span(repo, key, anchor.blob(), anchor.lines)?;
            // `path_blob` only returns a regular-blob entry, so the same
            // non-blob case that leaves `anchor.lines` unmapped also leaves
            // no post-edit blob to name here. Falling back to the anchor's
            // own (pre-edit) blob rather than the `none` sentinel matters:
            // `none` is reserved for `Position::Lost`, and pairing it with
            // `Content::Edited` is a combination the status lattice does not
            // allow (`query-and-rules.adoc`, the status lattice).
            let blob = path_blob(target_tree, path).unwrap_or_else(|| anchor.blob());
            Ok((sym_oid(blob), start, end))
        }
        Projection::Deleted => Ok((Value::sym("none"), 0, 0)),
    }
}

/// The 1-based `(start, end)` a projection's span covers: the given range for
/// a line-scoped anchor, or the whole blob's extent -- using the same
/// line-counting convention `line/2` does -- for a whole-file anchor.
fn line_span(
    repo: &gix::Repository,
    key: &PredicateKey,
    blob: ObjectId,
    lines: Option<gix_anchor::LineRange>,
) -> Result<(i64, i64), HostError> {
    match lines {
        Some(range) => Ok((int(range.start), int(range.end))),
        None => {
            let total = count_lines(repo, key, blob)?;
            Ok((1, int(total)))
        }
    }
}

fn path_blob(tree: &gix::Tree<'_>, path: &str) -> Option<ObjectId> {
    tree.lookup_entry_by_path(path)
        .ok()
        .flatten()
        .filter(|entry| entry.mode().is_blob())
        .map(|entry| entry.object_id())
}

fn int(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::Fixture;

    fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
        range.map(|n| format!("line {n}\n")).collect()
    }

    fn bind_args(anchor: &str, rev: &str) -> Vec<Option<Value>> {
        vec![
            Some(Value::sym(anchor)),
            Some(Value::sym(rev)),
            None,
            None,
            None,
            None,
            None,
        ]
    }

    /// `provides` refuses an arity-7 key that is neither `bind` nor
    /// `bind_fuzzy`; `call` must refuse the same key rather than silently
    /// running it with exact-projection semantics, which is what the old
    /// `key.name.as_ref() == "bind_fuzzy"` check (never checking membership
    /// at all) let through.
    #[test]
    fn call_refuses_a_key_provides_would_also_refuse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();

        let key = PredicateKey::new("bind_other", 7);
        let builtins = RepoBuiltins::new(&repo);
        assert!(!builtins.provides(&key));

        let mut out = Vec::new();
        let err = builtins
            .call(&key, &bind_args("00", "00"), &mut out)
            .expect_err("call must refuse a key provides refuses");
        assert!(matches!(err, HostError::NoProvider(_)));
    }

    /// `bind` is `(+Anchor, +Rev, -Blob, -Start, -End, -Position, -Content)`
    /// -- both leading arguments required bound. A free anchor must be
    /// refused rather than treated as "no rows", since silently returning
    /// empty for an unsupported mode is indistinguishable from a real
    /// "nothing binds here" answer.
    #[test]
    fn bind_refuses_a_free_anchor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("bind", 7);

        let mut out = Vec::new();
        let err = RepoBuiltins::new(&repo)
            .call(
                &key,
                &[None, Some(Value::sym(head)), None, None, None, None, None],
                &mut out,
            )
            .expect_err("bind must refuse a free anchor");
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }

    fn attach_position(repo: &gix::Repository, anchor: Anchor) -> ObjectId {
        Store::open(repo)
            .attach(&Binding::Position(anchor), b"claim body", None)
            .expect("attach")
    }

    #[test]
    fn bind_reports_moved_not_edited_for_a_pure_rename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "file.txt",
            Some(gix_anchor::LineRange { start: 3, end: 4 }),
        )
        .expect("capture");
        let id = attach_position(&repo, anchor);

        f.git(&["mv", "file.txt", "moved.txt"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[5], Value::sym("moved"));
        assert_eq!(row[6], Value::sym("intact"));
        assert_eq!(row[3], Value::Int(3));
        assert_eq!(row[4], Value::Int(4));
    }

    /// The fourth quadrant: `(moved, edited)`. Section 1.5 calls this "the
    /// knob that was missing" from the old flat status enum -- a span that
    /// moved but is textually intact should almost always carry a review
    /// forward, while one that stayed put but was edited should almost
    /// never, and a design that cannot even name the fourth combination
    /// cannot express the difference. Renaming the file *and* rewriting the
    /// anchored lines in the same commit keeps the rename detectable (only
    /// two of ten lines change) while leaving the anchored range itself
    /// unmappable.
    #[test]
    fn bind_reports_moved_and_edited_when_a_rename_also_touches_the_anchored_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "file.txt",
            Some(gix_anchor::LineRange { start: 3, end: 4 }),
        )
        .expect("capture");
        let id = attach_position(&repo, anchor);

        f.git(&["mv", "file.txt", "moved.txt"]);
        let edited: String = (1..=10)
            .map(|n| {
                if n == 3 || n == 4 {
                    format!("rewritten {n}\n")
                } else {
                    format!("line {n}\n")
                }
            })
            .collect();
        f.write("moved.txt", &edited);
        f.git(&["add", "-A"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[5], Value::sym("moved"), "the file moved to a new path");
        assert_eq!(
            row[6],
            Value::sym("edited"),
            "the anchored range itself was rewritten, not merely shifted"
        );
    }

    #[test]
    fn bind_reports_same_edited_for_an_in_place_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "file.txt",
            Some(gix_anchor::LineRange { start: 5, end: 6 }),
        )
        .expect("capture");
        let id = attach_position(&repo, anchor);

        let edited: String = (1..=10)
            .map(|n| {
                if n == 5 {
                    "line five\n".to_owned()
                } else {
                    format!("line {n}\n")
                }
            })
            .collect();
        f.write("file.txt", &edited);
        f.git(&["add", "-A"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[5], Value::sym("same"));
        assert_eq!(row[6], Value::sym("edited"));
        // `Outdated` reports the original, pre-edit range: there is no
        // post-edit mapping to report.
        assert_eq!(row[3], Value::Int(5));
        assert_eq!(row[4], Value::Int(6));
    }

    #[test]
    fn bind_reports_the_sentinel_blob_when_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("file.txt", "a\nb\nc\n", "one");
        let repo = f.open();
        let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).expect("capture");
        let id = attach_position(&repo, anchor);

        f.git(&["rm", "-q", "file.txt"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[2], Value::sym("none"));
        assert_eq!(row[5], Value::sym("lost"));
        assert_eq!(row[6], Value::sym("none"));
    }

    /// Reproduces the crash: `project_exact_onto`'s `!is_blob` branch
    /// (`projection.rs:418`) returns `Outdated` before ever looking at
    /// `anchor.lines`, and a whole-file anchor's `lines` is `None`. `span`
    /// used to `.expect()` a line range unconditionally in that arm, so this
    /// call used to panic and abort the process rather than return a
    /// verdict -- exactly the failure mode that makes a fixpoint over an
    /// ordinary repository state unusable as a gate.
    #[test]
    fn bind_does_not_panic_when_a_whole_file_anchor_becomes_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let original_blob = f.blob_at("HEAD", "file.txt");
        let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).expect("capture");
        let id = attach_position(&repo, anchor);

        let link_target = f.hash_object("elsewhere.txt");
        f.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "120000",
            &link_target,
            "file.txt",
        ]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call must not panic");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[5], Value::sym("same"), "the path did not move");
        assert_eq!(
            row[6],
            Value::sym("edited"),
            "axes() always reports Outdated as edited"
        );
        assert_ne!(
            row[2],
            Value::sym("none"),
            "none is reserved for a lost position, never paired with edited content"
        );
        assert_eq!(
            row[2],
            Value::sym(original_blob),
            "falls back to the anchor's own blob"
        );
        assert_eq!(row[3], Value::Int(1));
        assert_eq!(row[4], Value::Int(10));
    }

    /// The same crash, via a gitlink (a submodule entry) rather than a
    /// symlink -- both are "an entry survives at the anchored path but is
    /// not a regular blob", the exact condition `project_exact_onto`'s
    /// `!is_blob` branch exists for.
    #[test]
    fn bind_does_not_panic_when_a_whole_file_anchor_becomes_a_gitlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let original_blob = f.blob_at("HEAD", "file.txt");
        let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).expect("capture");
        let id = attach_position(&repo, anchor);

        // A gitlink's target need not resolve to a real object in this
        // repository -- git never validates it, since a submodule's commit
        // lives in a different object database entirely.
        let fake_submodule_commit = "a".repeat(40);
        f.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &fake_submodule_commit,
            "file.txt",
        ]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call must not panic");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[5], Value::sym("same"));
        assert_eq!(row[6], Value::sym("edited"));
        assert_ne!(row[2], Value::sym("none"));
        assert_eq!(row[2], Value::sym(original_blob));
    }

    /// The `none`/`edited` combination is unrepresentable regardless of
    /// whether the anchor is whole-file or line-range -- a line-range anchor
    /// hitting the same non-blob entry does not panic (`anchor.lines` is
    /// already `Some`), but before this fix it still emitted the sentinel
    /// blob paired with edited content.
    #[test]
    fn bind_never_pairs_the_none_sentinel_with_edited_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "file.txt",
            Some(gix_anchor::LineRange { start: 3, end: 4 }),
        )
        .expect("capture");
        let id = attach_position(&repo, anchor);

        let link_target = f.hash_object("elsewhere.txt");
        f.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "120000",
            &link_target,
            "file.txt",
        ]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row[6], Value::sym("edited"));
        assert_ne!(row[2], Value::sym("none"));
        assert_eq!(row[3], Value::Int(3));
        assert_eq!(row[4], Value::Int(4));
    }

    #[test]
    fn bind_yields_one_tuple_per_ambiguous_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("one");
        let repo = f.open();
        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "file.txt",
            Some(gix_anchor::LineRange { start: 3, end: 4 }),
        )
        .expect("capture");
        let id = attach_position(&repo, anchor);

        f.write("copy.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(
            out.len(),
            2,
            "the untouched file and its new copy both qualify"
        );
        let blobs: std::collections::BTreeSet<_> = out.iter().map(|row| row[2].clone()).collect();
        assert_eq!(
            blobs.len(),
            1,
            "both candidates carry the same, unedited blob"
        );
    }

    /// `bind` (`project_exact`'s family) fails loudly once the anchor's own
    /// commit is genuinely gone, rather than silently falling back the way
    /// `gix_anchor::project` does; `bind_fuzzy` needs no commit at all and
    /// keeps working. The anchor commit is abandoned for real -- amended out
    /// of history, reflog expired, then pruned -- rather than forged, since
    /// `Anchor`'s `commit` field is private to `gix-anchor` and this crate
    /// has no way to construct one with a fake value.
    #[test]
    fn bind_fails_loudly_when_the_anchor_commit_is_gone_and_bind_fuzzy_degrades() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.write("file.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        let anchor_commit = f.commit("one");
        let repo = f.open();
        let anchor = gix_anchor::capture(
            &repo,
            "HEAD",
            "file.txt",
            Some(gix_anchor::LineRange { start: 3, end: 4 }),
        )
        .expect("capture");
        let id = attach_position(&repo, anchor);

        // Move `main` to an *unrelated* root commit with the file renamed,
        // rather than committing on top of `anchor_commit`: `project_exact`
        // diffs two trees regardless of ancestry, so this still exercises
        // the rename it is meant to detect, but it also means nothing --
        // not even a descendant commit's parent link -- keeps
        // `anchor_commit` reachable once `main` stops pointing anywhere near
        // it. A commit built on top of `anchor_commit` would still name it
        // as a parent forever, amended copy or not, and no `git gc` short of
        // a history rewrite would ever prune it.
        f.git(&["checkout", "--quiet", "--orphan", "tmp"]);
        f.git(&["rm", "--quiet", "-f", "file.txt"]);
        f.write("moved.txt", &numbered(1..=10));
        f.git(&["add", "-A"]);
        f.commit("two");
        f.git(&["branch", "-f", "main", "tmp"]);
        f.git(&["checkout", "--quiet", "main"]);
        f.git(&["branch", "-D", "tmp"]);
        f.git(&["reflog", "expire", "--expire=now", "--all"]);
        f.git(&["gc", "--prune=now", "--quiet"]);

        let repo = f.open();
        let gone = gix::ObjectId::from_hex(anchor_commit.as_bytes()).expect("valid oid");
        assert!(
            !repo.has_object(gone),
            "setup: the anchor's own commit must actually be pruned"
        );
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 7);
        let mut out = Vec::new();
        let err = RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect_err("bind must fail loudly rather than silently degrade");
        assert!(matches!(err, HostError::Backend { .. }));

        let fuzzy_key = PredicateKey::new("bind_fuzzy", 7);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&fuzzy_key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("bind_fuzzy needs no commit at all");
        assert_eq!(out.len(), 1);
        // `project_from_context` does no rename tracking at all -- its own
        // doc comment says a genuine rename reports `Deleted`, identically to
        // an actual deletion. This is the exact divergence
        // `query-and-rules.adoc`'s "Two entry points" section and
        // `gix-anchor`'s own regression test both call out: the same anchor,
        // the same target, two different answers, decided only by whether
        // maintenance ran.
        assert_eq!(out[0][5], Value::sym("lost"));
        assert_eq!(out[0][6], Value::sym("none"));
    }

    /// The registry's `bind`/`bind_fuzzy` `version` and
    /// `gix_anchor::PROJECTION_HEURISTIC_VERSION` are two constants declaring
    /// the same fact -- the implementation version of the rename-detection
    /// and hunk-mapping heuristics `bind` wraps -- and `gix-query-host` is
    /// the only crate in the workspace that can see both at once. Without
    /// this check they can silently drift: an upstream heuristic change that
    /// bumps `PROJECTION_HEURISTIC_VERSION` with no matching bump in
    /// `gix_query_ir`'s registry would leave the builtin registry digest (and
    /// therefore the cache key, `query-and-rules.adoc` section 1.6) unmoved,
    /// serving every cached `reviewed_span` computed under the old heuristic
    /// unchanged. `gix_query_ir` is frozen from this crate's side, so a
    /// divergence caught here can only be fixed upstream in the registry.
    #[test]
    fn bind_version_tracks_the_projection_heuristic_it_wraps() {
        let registry = gix_query_ir::Registry::host();
        for name in ["bind", "bind_fuzzy"] {
            let entry = registry
                .builtin(&PredicateKey::new(name, 7))
                .unwrap_or_else(|| panic!("{name} is registered"));
            assert_eq!(
                entry.props.version,
                gix_anchor::PROJECTION_HEURISTIC_VERSION,
                "{name}'s registered version has drifted from \
                 gix_anchor::PROJECTION_HEURISTIC_VERSION; bump the registry \
                 constant in gix-query-ir to match"
            );
        }
    }

    /// `gix_anchor::Position::label()`/`Content::label()` and
    /// `gix_query_ir::ValueType::Position`/`Content`'s closed domains are two
    /// declarations of the same porcelain vocabulary, and this crate is the
    /// only one that can see both. Nothing else compares them: if
    /// `gix-anchor` ever renamed a label, `bind` would go on emitting it
    /// (this crate calls `.label()`, never a literal), but
    /// `carries(same, intact).` in a rule module would silently stop
    /// joining against it, and every review would stop carrying with no
    /// error anywhere. The `match`es below have no wildcard arm, so
    /// `gix-anchor` adding a variant is a compile error here rather than a
    /// gap this test cannot see.
    #[test]
    fn projection_axis_labels_are_admitted_by_the_registry_domains() {
        for position in [
            gix_anchor::Position::Same,
            gix_anchor::Position::Moved,
            gix_anchor::Position::Lost,
        ] {
            let label = match position {
                gix_anchor::Position::Same
                | gix_anchor::Position::Moved
                | gix_anchor::Position::Lost => position.label(),
            };
            assert!(
                gix_query_ir::ValueType::Position.admits(label),
                "gix_anchor::Position::{position:?}.label() is {label:?}, which \
                 ValueType::Position's domain does not admit"
            );
        }
        for content in [
            gix_anchor::Content::Intact,
            gix_anchor::Content::Edited,
            gix_anchor::Content::None,
        ] {
            let label = match content {
                gix_anchor::Content::Intact
                | gix_anchor::Content::Edited
                | gix_anchor::Content::None => content.label(),
            };
            assert!(
                gix_query_ir::ValueType::Content.admits(label),
                "gix_anchor::Content::{content:?}.label() is {label:?}, which \
                 ValueType::Content's domain does not admit"
            );
        }
    }
}
