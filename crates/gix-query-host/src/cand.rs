//! [`RepoBuiltins`]: `cand`, every candidate location an anchor might sit at
//! on a revision, unselected -- and `key_valid_at`, the claim-validity filter
//! that shares this crate's one absent-safe convention with it.
//!
//! `cand(A, Rev, Loc, O, C)` is monotone -- it only ever adds rows, never
//! retracts one candidate because a better one turned up -- so semi-naive
//! incrementality holds for it (ARCHITECTURE.md: "No negation: candidate
//! generation is monotone"). Selection is a *rule*, not this builtin:
//! `bind/5` (`gix-query-rules::assemble`'s always-present core module) reads
//! `cand` back and picks the maximum by `!dominated(...)` over a genuine `>`
//! comparison, in its own stratum, exactly as ARCHITECTURE.md writes it. That
//! split is also what keeps a rule able to reach a sub-maximal candidate at
//! all (ARCHITECTURE.md: "the confidence threshold is a rule parameter here,
//! not anchor behavior") -- a threshold or an orphaning rule can only be
//! written against a relation the rule language can see, and a builtin that
//! pre-selected the winner internally would have thrown every other
//! candidate away before any rule got a look at it.
//!
//! `cand` composes two legs: pin ([`crate::claim::rebind_pin_locs`], at
//! confidence 1.0 by construction) and `gix-anchor`'s oracle chain
//! (`op_log`, `diff_trace`, `fingerprint`) -- "each oracle contributes
//! candidates as separate base facts" (ARCHITECTURE.md), so every candidate
//! becomes its own row here rather than being folded together. The pin leg
//! reads a signed `rebind-pin` claim's payload through anchor's own schema; a
//! claim whose signature does not verify pins nothing.
//!
//! `Loc` is one atomic term, `"<path>:<start>-<end>"` (`ValueType::Location`):
//! the value domain has no tuple type to hold a path and a byte span as
//! separate fields. Formatting it this way also matches
//! `gix_anchor::RebindPin`'s own `(path, span)` payload shape, so the pin leg
//! reads a claim's `Loc:` trailer back verbatim rather than reassembling it
//! from separate columns.
//!
//! `C` is a plain `Int`: basis points on `0..=10000` (an oracle's own
//! `0.0..=1.0` confidence, times `10000`, rounded), never a formatted
//! string. `bind`'s rule compares it with `>`, and only a genuinely ordered
//! numeric value orders correctly under every possible representation
//! change -- a fixed-width zero-padded decimal string would happen to sort
//! the same way today, but "happens to" is exactly the coincidence a real
//! integer avoids (see `gix-query-eval`'s
//! `bind_selects_the_maximum_confidence_not_the_lexicographically_greatest`
//! for the concrete case a naive string comparison gets backwards).
//!
//! # The one absent-safe convention, and its one gap
//!
//! No operation log exists yet anywhere in the product family (DELTA X6).
//! Two things here are backed by it, and both say so the same way rather than
//! inventing a second convention:
//!
//! - `op_log` is called with no [`gix_anchor::OpLogSource`], so it is
//!   absent-safe and contributes zero candidates. The call stays in
//!   [`candidates`] as a marker for the day a source exists, rather than
//!   silently dropping the oracle from the chain.
//! - [`key_valid_at`] is the same marker in filter position, with the opposite
//!   absent value -- see its own documentation for why, and for the one place
//!   the gap is written down.

use gix::ObjectId;
use gix_anchor::{Candidate, Oracle};
use gix_query_ir::{Bindings, Builtin, HostError, PredicateKey, Value};

use crate::claim::rebind_pin_locs;
use crate::refs::HostRefs;
use crate::util::{backend, backend_msg, parse_oid};

/// Basis points per unit of confidence: `cand`'s `C` column is
/// `round(confidence * CONFIDENCE_SCALE)`, an `i64` on `0..=CONFIDENCE_SCALE`.
const CONFIDENCE_SCALE: f64 = 10_000.0;

/// A [`Builtin`] over `cand`, over one open repository.
pub struct RepoBuiltins<'r> {
    repo: &'r gix::Repository,
}

impl<'r> RepoBuiltins<'r> {
    /// The `cand` builtin over `repo`.
    #[must_use]
    pub fn new(repo: &'r gix::Repository) -> Self {
        Self { repo }
    }
}

/// Whether `key` is `key_valid_at/1`.
fn is_key_valid_at(key: &PredicateKey) -> bool {
    key.arity == 1 && key.name.as_ref() == "key_valid_at"
}

/// `key_valid_at(+Claim)`: was the claim's signing key valid at the op-log
/// position where the claim was admitted (ARCHITECTURE.md, "git-attest").
///
/// **This is the one place the op-log gap is written down.** Admission order
/// lives in the op-log, and no op-log exists yet anywhere in the product family
/// (DELTA X6) -- there is no ref to read, no adapter to call, and no position to
/// compare a key's lifetime against. Until one exists this holds for every
/// claim, which is the absent-safe answer for a *filter*: a filter that
/// answered `false` with no data would silently empty every rule that composes
/// it, and `reviewed(B) :- ..., key_valid_at(C).` would derive nothing at all
/// rather than deriving what the available evidence supports. `cand`'s
/// `op_log(None, ...)` leg contributes zero *candidates* for the mirror-image
/// reason: absent evidence adds no rows to a generator and removes none from a
/// filter.
///
/// What this does not decide: whether revoking a key retroactively invalidates
/// claims already admitted under it. That is rule policy, deliberately left
/// open (ARCHITECTURE.md's open questions), and the contract here is worded so
/// that either answer is a change to this function's *backing* and to no
/// caller's rule text.
///
/// A claim's cryptographic soundness is not asked here and never was: an
/// unverified claim contributes no facts at all (`crate::claim`), so a rule can
/// only ever reach this predicate with a claim whose signature already checked
/// out.
fn key_valid_at(key: &PredicateKey, bound: Bindings<'_>) -> Result<bool, HostError> {
    let Some(Some(claim)) = bound.first() else {
        return Err(HostError::UnsupportedMode { key: key.clone() });
    };
    // The claim is parsed rather than ignored: `+Claim` is `Oid`-typed, and a
    // predicate that accepted a non-oid today would accept it after the op-log
    // lands, when the value is a lookup key rather than a formality.
    parse_oid(key, claim)?;
    Ok(true)
}

impl Builtin for RepoBuiltins<'_> {
    fn provides(&self, key: &PredicateKey) -> bool {
        (key.arity == 5 && key.name.as_ref() == "cand") || is_key_valid_at(key)
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
        if is_key_valid_at(key) {
            if key_valid_at(key, bound)? {
                out.push(vec![bound[0].clone().expect("bound, checked above")]);
            }
            return Ok(());
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

        for (label, confidence, loc) in
            candidates(self.repo, key, anchor_oid, rev_oid, &target_tree)?
        {
            let tuple = vec![
                anchor_v.clone(),
                rev_v.clone(),
                Value::sym(loc),
                Value::sym(label),
                Value::Int(confidence),
            ];
            if satisfies(bound, &tuple) {
                out.push(tuple);
            }
        }
        Ok(())
    }

    fn holds(&self, key: &PredicateKey, args: &[Value]) -> Result<bool, HostError> {
        if is_key_valid_at(key) {
            // A filter answered without allocating a row, the same shortcut the
            // comparisons take: `key_valid_at` sits in the body of every
            // validity-checking rule, once per claim.
            let bound: Vec<Option<Value>> = args.iter().cloned().map(Some).collect();
            return key_valid_at(key, &bound);
        }
        let bound: Vec<Option<Value>> = args.iter().cloned().map(Some).collect();
        let mut out = Vec::new();
        self.call(key, &bound, &mut out)?;
        Ok(!out.is_empty())
    }
}

/// One candidate: its oracle label, its confidence in basis points
/// (`0..=10_000`), and its `Loc` term.
type Row = (&'static str, i64, String);

/// Every candidate `anchor_oid` has at `rev_oid`, from every leg of the
/// lattice -- pin, then the oracle chain in `gix_anchor::oracle`'s own
/// highest-to-lowest fidelity order. Unselected: this is `cand`'s whole
/// extension for `(anchor_oid, rev_oid)`, not an answer.
fn candidates(
    repo: &gix::Repository,
    key: &PredicateKey,
    anchor_oid: ObjectId,
    rev_oid: ObjectId,
    target_tree: &gix::Tree<'_>,
) -> Result<Vec<Row>, HostError> {
    // `cand` is a builtin, not an EDB predicate: `gix-query-ir`'s registry
    // declares no `Backing` for it (`BuiltinEntry` has no such field), so a
    // rule calling it contributes no namespace to any footprint
    // (`gix_query_check::read_namespaces` only ever projects `PredicateEntry`
    // backings). `git-effect`'s trigger detection therefore never selects an
    // effect on `cand`/`bind` alone as a candidate for a ref transition, and
    // this leg of the read seam has nothing to answer "as of" -- always live.
    // Built fresh per leg below (as cheap as the `GixRefStore` it wraps)
    // rather than shared, since `rebind_pin_locs` needs to own one.

    let mut out = Vec::new();

    for loc in rebind_pin_locs(repo, HostRefs::live(repo), key, anchor_oid, rev_oid)? {
        out.push(("pin", basis_points(1.0), loc));
    }

    if let Some(anchor) = crate::anchor::lookup(repo, &HostRefs::live(repo), key, anchor_oid)? {
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

/// `confidence` (`0.0..=1.0`), scaled to basis points and rounded to the
/// nearest integer.
#[allow(
    clippy::cast_possible_truncation,
    reason = "confidence is 0.0..=1.0, so the scaled result always fits an i64"
)]
fn basis_points(confidence: f64) -> i64 {
    (confidence * CONFIDENCE_SCALE).round() as i64
}

/// One [`Candidate`] as a [`Row`]: its oracle label, its confidence in basis
/// points, and its `Loc` term -- `"<path>:<start>-<end>"`, resolving a
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
        basis_points(candidate.confidence),
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
    use gix_anchor::{REBIND_PIN_KIND, RebindPin};
    use gix_attest::Target;
    use test_support::{ClaimFixture, Fixture};

    use super::*;
    use crate::claim::ANCHOR_TARGET_KIND;

    fn numbered(range: std::ops::RangeInclusive<u32>) -> String {
        range.map(|n| format!("line {n}\n")).collect()
    }

    fn cand_args(anchor: &str, rev: &str) -> Vec<Option<Value>> {
        vec![
            Some(Value::sym(anchor)),
            Some(Value::sym(rev)),
            None,
            None,
            None,
        ]
    }

    fn row(out: &[Vec<Value>], i: usize) -> (String, String, i64) {
        let r = &out[i];
        (
            r[2].as_sym().expect("loc").to_owned(),
            r[3].as_sym().expect("oracle").to_owned(),
            r[4].as_int().expect("confidence"),
        )
    }

    /// `provides` refuses an arity-5 key that is not named `cand`; `call`
    /// must refuse the same key rather than silently running it.
    #[test]
    fn call_refuses_a_key_provides_would_also_refuse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();

        let key = PredicateKey::new("cand_other", 5);
        let builtins = RepoBuiltins::new(&repo);
        assert!(!builtins.provides(&key));

        let mut out = Vec::new();
        let err = builtins
            .call(&key, &cand_args("00", "00"), &mut out)
            .expect_err("call must refuse a key provides refuses");
        assert!(matches!(err, HostError::NoProvider(_)));
    }

    /// `cand` is `(+Anchor, +Rev, -Loc, -O, -C)` -- both leading arguments
    /// required bound. A free anchor must be refused rather than treated as
    /// "no rows".
    #[test]
    fn cand_refuses_a_free_anchor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("cand", 5);

        let mut out = Vec::new();
        let err = RepoBuiltins::new(&repo)
            .call(
                &key,
                &[None, Some(Value::sym(head)), None, None, None],
                &mut out,
            )
            .expect_err("cand must refuse a free anchor");
        assert!(matches!(err, HostError::UnsupportedMode { .. }));
    }

    #[test]
    fn cand_reports_a_diff_trace_candidate_for_a_pure_rename() {
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

        let key = PredicateKey::new("cand", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &cand_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let (loc, oracle, confidence) = row(&out, 0);
        assert_eq!(oracle, "diff_trace");
        assert_eq!(confidence, 10_000);
        assert!(loc.starts_with("moved.txt:"));
    }

    /// `cand` reports the pin candidate *and* the lower-confidence oracle
    /// candidate side by side -- unselected, both survive, and it is
    /// `bind`'s rule, not this builtin, that picks pin at confidence 1.0
    /// over it (ARCHITECTURE.md: "a pin dominates the lattice at 1.0 by
    /// construction"). The anchored range itself is edited, so `diff_trace`
    /// reports nothing and `fingerprint` is the only oracle candidate, at
    /// less than 1.0.
    #[test]
    fn cand_reports_the_pin_and_the_lower_confidence_oracle_candidate_side_by_side() {
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

        // Confirm the setup: without a pin, only a sub-maximum fingerprint
        // candidate is on offer.
        let key = PredicateKey::new("cand", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &cand_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(out.len(), 1);
        let (_, oracle, confidence) = row(&out, 0);
        assert_eq!(oracle, "fingerprint");
        assert_ne!(confidence, 10_000, "setup: the edit must cost confidence");

        // The pin arrives the only way pins arrive: a signed `rebind-pin`
        // claim on the anchor, its payload written against anchor's own
        // registered schema. The span is deliberately far from where either
        // oracle would look, so a pin row cannot be mistaken for one of theirs.
        let claims = ClaimFixture::new(&f);
        claims.register_payload_schema::<RebindPin>(REBIND_PIN_KIND);
        let pin = RebindPin {
            rev: ObjectId::from_hex(head.as_bytes()).expect("hex rev").into(),
            path: "file.txt".to_owned(),
            span: (200, 260),
        };
        let payload = claims.payload(REBIND_PIN_KIND, &pin);
        claims.sign(
            &Target {
                kind: ANCHOR_TARGET_KIND.to_owned(),
                id: id.into(),
            },
            payload,
            REBIND_PIN_KIND,
        );
        let repo = f.open();

        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &cand_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert_eq!(
            out.len(),
            2,
            "cand does not select; both candidates survive"
        );
        let mut oracles: Vec<String> = Vec::new();
        let mut pin_confidence = None;
        for i in 0..out.len() {
            let (loc, oracle, confidence) = row(&out, i);
            if oracle == "pin" {
                pin_confidence = Some(confidence);
                assert_eq!(loc, "file.txt:200-260");
            }
            oracles.push(oracle);
        }
        oracles.sort();
        assert_eq!(oracles, vec!["fingerprint", "pin"]);
        assert_eq!(pin_confidence, Some(10_000));
    }

    /// A deleted anchor with no pin and an anchor commit still reachable:
    /// `diff_trace` reports nothing (the path is gone) and `fingerprint`
    /// finds no surviving window, so `cand` answers with no rows at all --
    /// never a sentinel value, since there is no `Blob` column to sentinel
    /// any more.
    #[test]
    fn cand_answers_no_rows_when_nothing_resolves() {
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

        let key = PredicateKey::new("cand", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &cand_args(&id.to_string(), &head), &mut out)
            .expect("call");
        assert!(out.is_empty());
    }

    /// `cand` answers with no rows -- never an error -- for an anchor id
    /// that resolves to nothing (`refs/anchors/*` has no such ref) and has
    /// no pin claim either: a caller may name any oid, and most will not be
    /// anchors.
    #[test]
    fn cand_answers_no_rows_for_an_unregistered_anchor_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let key = PredicateKey::new("cand", 5);
        let mut out = Vec::new();
        RepoBuiltins::new(&repo)
            .call(&key, &cand_args(&"0".repeat(40), &head), &mut out)
            .expect("call");
        assert!(out.is_empty());
    }

    /// `cand`'s registered version and `gix_anchor::DIFF_TRACE_ORACLE_VERSION`
    /// are two constants declaring a related fact: `gix-query-host` is one of
    /// the few crates that can see both `gix_query_ir`'s registry and
    /// `gix-anchor`'s own oracle versioning at once. A `diff_trace` behavior
    /// change that bumps its oracle version with no matching bump in the
    /// registry would leave `cand`'s cache-key contribution unmoved, serving
    /// a cached answer computed under the old heuristic unchanged.
    #[test]
    fn cand_version_tracks_the_diff_trace_oracle_it_wraps() {
        let registry = gix_query_ir::Registry::host();
        let entry = registry
            .builtin(&PredicateKey::new("cand", 5))
            .expect("cand is registered");
        assert_eq!(
            entry.props.version,
            gix_anchor::DIFF_TRACE_ORACLE_VERSION,
            "cand's registered version has drifted from \
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
