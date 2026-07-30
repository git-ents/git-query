//! [`RepoBuiltins`]: `bind`, the one user-facing resolution predicate
//! (ARCHITECTURE.md: "`bind/5` is the only user-facing resolution").
//!
//! `bind(A, Rev, Loc, O, C)` composes two legs into one confidence lattice:
//! pin ([`crate::claim::rebind_pin_locs`], at confidence 1.0 by
//! construction) and `gix-anchor`'s oracle chain (`op_log`, `diff_trace`,
//! `fingerprint`). [`candidates`] collects every leg's rows separately --
//! "each oracle contributes candidates as separate base facts" -- and
//! [`select_max`] is selection's own step, run once over whatever
//! [`candidates`] collected: ARCHITECTURE.md's aggregation stratum, kept as
//! a distinct function rather than threaded into candidate generation, so a
//! future candidate source only ever adds to [`candidates`], never changes
//! how a winner is picked. `gix-anchor` applies no threshold anywhere
//! (`gix_anchor::oracle`'s own doc comment); neither does this: `bind`
//! reports the maximum, and an admission threshold on top of it is a rule's
//! parameter, not a builtin's.
//!
//! `Loc` is one atomic term, `"<path>:<start>-<end>"`
//! (`ValueType::Location`): the value domain has no tuple type to hold a
//! path and a byte span as separate fields. Formatting it this way also
//! matches `gix_anchor::RebindPin`'s own `(path, span)` payload shape, so
//! the pin leg reads a claim's `Loc:` trailer back verbatim rather than
//! reassembling it from separate columns.
//!
//! `op_log` is called with no [`gix_anchor::OpLogSource`]: no operation log
//! exists yet anywhere in the product family (DELTA X6), so it is
//! absent-safe and always contributes zero candidates here. The call stays
//! in [`candidates`] anyway, as a marker for the day a source exists, rather
//! than silently dropping the oracle from the chain.

use gix::ObjectId;
use gix_anchor::{Candidate, Oracle};
use gix_query_ir::{Bindings, Builtin, HostError, PredicateKey, Value};

use crate::claim::rebind_pin_locs;
use crate::util::{backend, backend_msg, parse_oid};

/// A [`Builtin`] over `bind`, over one open repository.
pub struct RepoBuiltins<'r> {
    repo: &'r gix::Repository,
}

impl<'r> RepoBuiltins<'r> {
    /// The `bind` builtin over `repo`.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self { repo }
    }
}

impl Builtin for RepoBuiltins<'_> {
    fn provides(&self, key: &PredicateKey) -> bool {
        key.arity == 5 && key.name.as_ref() == "bind"
    }

    fn call(
        &self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError> {
        if !self.provides(key) {
            return Err(HostError::NoProvider(key.clone()));
        }
        let (Some(Some(anchor_v)), Some(Some(rev_v))) = (bound.first(), bound.get(1)) else {
            return Err(HostError::UnsupportedMode { key: key.clone() });
        };
        let anchor_oid = parse_oid(key, anchor_v)?;
        let rev_oid = parse_oid(key, rev_v)?;

        let Ok(target_commit) = self.repo.find_commit(rev_oid) else {
            return Ok(());
        };
        let target_tree = target_commit.tree().map_err(|error| backend(key, error))?;

        let all = candidates(self.repo, key, anchor_oid, rev_oid, &target_tree)?;
        for (label, confidence, loc) in select_max(&all) {
            let tuple = vec![
                anchor_v.clone(),
                rev_v.clone(),
                Value::sym(loc.clone()),
                Value::sym(*label),
                Value::sym(format_confidence(*confidence)),
            ];
            if satisfies(bound, &tuple) {
                out.push(tuple);
            }
        }
        Ok(())
    }
}

/// One candidate: its oracle label, its confidence, and its `Loc` term.
type Row = (&'static str, f64, String);

/// Every candidate `anchor_oid` has at `rev_oid`, from every leg of the
/// lattice -- pin, then the oracle chain in `gix_anchor::oracle`'s own
/// highest-to-lowest fidelity order. Not itself the answer: [`select_max`]
/// picks the winner from what this returns.
fn candidates(
    repo: &gix::Repository,
    key: &PredicateKey,
    anchor_oid: ObjectId,
    rev_oid: ObjectId,
    target_tree: &gix::Tree<'_>,
) -> Result<Vec<Row>, HostError> {
    let mut out = Vec::new();

    for loc in rebind_pin_locs(repo, key, anchor_oid, rev_oid)? {
        out.push(("pin", 1.0, loc));
    }

    if let Some(anchor) = crate::anchor::lookup(repo, key, anchor_oid)? {
        let rev_text = rev_oid.to_string();
        let anchor_id = gix_anchor::AnchorId::from(anchor_oid);

        for candidate in gix_anchor::op_log(None, anchor_id, rev_oid) {
            out.push(labeled(repo, key, target_tree, candidate)?);
        }
        for candidate in
            gix_anchor::diff_trace(repo, &anchor, &rev_text).map_err(|error| backend(key, error))?
        {
            out.push(labeled(repo, key, target_tree, candidate)?);
        }
        for candidate in gix_anchor::fingerprint_oracle(repo, &anchor, &rev_text)
            .map_err(|error| backend(key, error))?
        {
            out.push(labeled(repo, key, target_tree, candidate)?);
        }
    }

    Ok(out)
}

/// Every row at the maximum confidence [`candidates`] collected -- empty
/// when `candidates` itself was empty. Ties (two candidates at the same
/// maximum) all survive: `bind` reports every one, and it is a rule's job
/// (the ambiguity lint, `query-and-rules.adoc`) to decide what to do with
/// more than one.
fn select_max(all: &[Row]) -> Vec<&Row> {
    let Some(max) = all.iter().map(|(_, c, _)| *c).reduce(f64::max) else {
        return Vec::new();
    };
    all.iter().filter(|(_, c, _)| *c == max).collect()
}

/// `confidence`, formatted to four decimal places so two equal confidences
/// are always the same [`Value::Sym`] rather than differing in
/// float-formatting noise.
fn format_confidence(confidence: f64) -> String {
    format!("{confidence:.4}")
}

/// One [`Candidate`] as a [`Row`]: its oracle label, its confidence
/// verbatim, and its `Loc` term -- `"<path>:<start>-<end>"`, resolving a
/// `None` span (a whole-file match) against the target blob's own length.
fn labeled(
    repo: &gix::Repository,
    key: &PredicateKey,
    target_tree: &gix::Tree<'_>,
    candidate: Candidate,
) -> Result<Row, HostError> {
    let label = match candidate.oracle {
        Oracle::OpLog => "op_log",
        Oracle::DiffTrace => "diff_trace",
        Oracle::Fingerprint => "fingerprint",
    };
    let (start, end) = match candidate.span {
        Some(span) => (span.start, span.end),
        None => (0, whole_file_len(repo, key, target_tree, &candidate.path)?),
    };
    Ok((
        label,
        candidate.confidence,
        format!("{}:{start}-{end}", candidate.path),
    ))
}

/// The byte length of the blob at `path` in `target_tree`.
fn whole_file_len(
    repo: &gix::Repository,
    key: &PredicateKey,
    target_tree: &gix::Tree<'_>,
    path: &str,
) -> Result<u64, HostError> {
    let blob = target_tree
        .lookup_entry_by_path(path)
        .map_err(|error| backend(key, error))?
        .filter(|entry| entry.mode().is_blob())
        .map(|entry| entry.object_id())
        .ok_or_else(|| {
            backend_msg(
                key,
                format!("candidate path {path} has no blob entry in the target tree"),
            )
        })?;
    let data = repo
        .find_blob(blob)
        .map_err(|error| backend(key, error))?
        .take_data();
    Ok(u64::try_from(data.len()).unwrap_or(u64::MAX))
}

/// Whether every position `bound` fixes agrees with `tuple` -- binding more
/// than a builtin's required minimum is always legal (`mode.rs`'s
/// `satisfied_by`), so a caller that additionally bound, say, `O` must see
/// only matching rows rather than every candidate.
fn satisfies(bound: Bindings<'_>, tuple: &[Value]) -> bool {
    bound
        .iter()
        .zip(tuple.iter())
        .all(|(want, have)| want.as_ref().is_none_or(|want| want == have))
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
        ]
    }

    fn row(out: &[Vec<Value>], i: usize) -> (String, String, String) {
        let r = &out[i];
        (
            r[2].as_sym().expect("loc").to_owned(),
            r[3].as_sym().expect("oracle").to_owned(),
            r[4].as_sym().expect("confidence").to_owned(),
        )
    }

    /// `provides` refuses an arity-5 key that is not named `bind`; `call`
    /// must refuse the same key rather than silently running it.
    #[test]
    fn call_refuses_a_key_provides_would_also_refuse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();

        let key = PredicateKey::new("bind_other", 5);
        let builtins = RepoBuiltins::new(&repo);
        assert!(!builtins.provides(&key));

        let mut out = Vec::new();
        let err = builtins
            .call(&key, &bind_args("00", "00"), &mut out)
            .expect_err("call must refuse a key provides refuses");
        assert!(matches!(err, HostError::NoProvider(_)));
    }

    /// `bind` is `(+Anchor, +Rev, -Loc, -O, -C)` -- both leading arguments
    /// required bound. A free anchor must be refused rather than treated as
    /// "no rows".
    #[test]
    fn bind_refuses_a_free_anchor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("bind", 5);

        let mut out = Vec::new();
        let err = RepoBuiltins::new(&repo)
            .call(
                &key,
                &[None, Some(Value::sym(head)), None, None, None],
                &mut out,
            )
            .expect_err("bind must refuse a free anchor");
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }

    #[test]
    fn bind_reports_a_diff_trace_candidate_for_a_pure_rename() {
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
        let id = crate::anchor::attach(&repo, anchor).expect("attach");

        f.git(&["mv", "file.txt", "moved.txt"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let (loc, oracle, confidence) = row(&out, 0);
        assert_eq!(oracle, "diff_trace");
        assert_eq!(confidence, "1.0000");
        assert!(loc.starts_with("moved.txt:"));
    }

    /// A pin claim at confidence 1.0 wins over a lower-confidence
    /// `fingerprint` candidate -- ARCHITECTURE.md: "a pin dominates the
    /// lattice at 1.0 by construction". The anchored range itself is
    /// edited, so `diff_trace` reports nothing and `fingerprint` is the
    /// only oracle candidate, at less than 1.0.
    #[test]
    fn bind_pin_beats_a_lower_confidence_oracle_candidate() {
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
        let id = crate::anchor::attach(&repo, anchor).expect("attach");

        // Same byte length as the original "line 5\n" -- a fixed-width
        // fingerprint window still fits, so the edit costs confidence
        // without dropping the score to zero outright.
        let edited: String = (1..=10)
            .map(|n| {
                if n == 5 {
                    "line V\n".to_owned()
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

        // Confirm the setup: without a pin, only a sub-1.0 fingerprint
        // candidate is on offer.
        let key = PredicateKey::new("bind", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let (_, oracle, confidence) = row(&out, 0);
        assert_eq!(oracle, "fingerprint");
        assert_ne!(confidence, "1.0000", "setup: the edit must cost confidence");

        let pin_message = format!(
            "rebind\n\nKind: rebind-pin\nTarget: {id}\nRev: {head}\nLoc: file.txt:200-260\n"
        );
        let pin_id = f.commit(&pin_message);
        f.update_ref(&format!("refs/meta/claims/{pin_id}"), &pin_id);
        let repo = f.open();

        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1, "the pin alone must survive selection");
        let (loc, oracle, confidence) = row(&out, 0);
        assert_eq!(oracle, "pin");
        assert_eq!(confidence, "1.0000");
        assert_eq!(loc, "file.txt:200-260");
    }

    /// Two candidates at the same maximum confidence both survive
    /// selection -- an untouched file and a byte-identical copy both trace
    /// exactly via `diff_trace`, but `fingerprint` runs regardless (the
    /// oracle chain is not short-circuited by an earlier oracle's success)
    /// and finds the same, unmoved window at its own confidence 1.0. Both
    /// tie at the maximum, and `bind` reports both rather than arbitrarily
    /// picking one.
    #[test]
    fn bind_reports_every_candidate_tied_at_the_maximum() {
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
        let id = crate::anchor::attach(&repo, anchor).expect("attach");
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 2, "diff_trace and fingerprint both agree here");
        let mut oracles: Vec<String> = out.iter().map(|_| String::new()).collect();
        for (i, oracle) in oracles.iter_mut().enumerate() {
            let (loc, o, confidence) = row(&out, i);
            assert_eq!(confidence, "1.0000");
            assert!(loc.starts_with("file.txt:"));
            *oracle = o;
        }
        oracles.sort();
        assert_eq!(oracles, vec!["diff_trace", "fingerprint"]);
    }

    /// A deleted anchor with no pin and an anchor commit still reachable:
    /// `diff_trace` reports nothing (the path is gone) and `fingerprint`
    /// finds no surviving window, so `bind` answers with no rows at all --
    /// never a sentinel value, since there is no `Blob` column to sentinel
    /// any more.
    #[test]
    fn bind_answers_no_rows_when_nothing_resolves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("file.txt", "a\nb\nc\n", "one");
        let repo = f.open();
        let anchor = gix_anchor::capture(&repo, "HEAD", "file.txt", None).expect("capture");
        let id = crate::anchor::attach(&repo, anchor).expect("attach");

        f.git(&["rm", "-q", "file.txt"]);
        f.commit("two");
        let repo = f.open();
        let head = f.rev_parse("HEAD");

        let key = PredicateKey::new("bind", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert!(out.is_empty());
    }

    /// `bind` answers with no rows -- never an error -- for an anchor id
    /// that resolves to nothing (`refs/anchors/*` has no such ref) and has
    /// no pin claim either: a caller may name any oid, and most will not be
    /// anchors.
    #[test]
    fn bind_answers_no_rows_for_an_unregistered_anchor_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("bind", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &bind_args(&"0".repeat(40), &head), &mut out)
            .expect("call");
        assert!(out.is_empty());
    }

    /// `bind`'s registered version and `gix_anchor::DIFF_TRACE_ORACLE_VERSION`
    /// are two constants declaring a related fact: `gix-query-host` is one of
    /// the few crates that can see both `gix_query_ir`'s registry and
    /// `gix-anchor`'s own oracle versioning at once. A `diff_trace` behavior
    /// change that bumps its oracle version with no matching bump in the
    /// registry would leave `bind`'s cache-key contribution unmoved, serving
    /// a cached answer computed under the old heuristic unchanged.
    #[test]
    fn bind_version_tracks_the_diff_trace_oracle_it_wraps() {
        let registry = gix_query_ir::Registry::host();
        let entry = registry
            .builtin(&PredicateKey::new("bind", 5))
            .expect("bind is registered");
        assert_eq!(
            entry.props.version,
            gix_anchor::DIFF_TRACE_ORACLE_VERSION,
            "bind's registered version has drifted from \
             gix_anchor::DIFF_TRACE_ORACLE_VERSION; bump the registry \
             constant in gix-query-ir to match"
        );
    }

    /// `gix_anchor::Oracle`'s three variants, plus `pin` (query's own leg),
    /// are exactly the closed domain `ValueType::Oracle` admits. The match
    /// below has no wildcard arm, so `gix-anchor` adding a variant is a
    /// compile error here rather than a gap this test cannot see.
    #[test]
    fn oracle_labels_are_admitted_by_the_registry_domain() {
        for oracle in [Oracle::OpLog, Oracle::DiffTrace, Oracle::Fingerprint] {
            let label = match oracle {
                Oracle::OpLog => "op_log",
                Oracle::DiffTrace => "diff_trace",
                Oracle::Fingerprint => "fingerprint",
            };
            assert!(
                gix_query_ir::ValueType::Oracle.admits(label),
                "gix_anchor::Oracle::{oracle:?} labels as {label:?}, which \
                 ValueType::Oracle's domain does not admit"
            );
        }
        assert!(gix_query_ir::ValueType::Oracle.admits("pin"));
    }
}
