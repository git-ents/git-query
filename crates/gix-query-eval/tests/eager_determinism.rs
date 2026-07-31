//! `run_eager` is the only evaluator left (`DEVPLAN.md` §0, step 6): the
//! differential proptest that used to compare it against `run_to_fixpoint`
//! has nothing left to diff against now that `demand.rs` is gone. What
//! survives from it is the fixture -- a program mixing both halves of §0's
//! narrow cut, `commit`/`parent`/`author` required-bound alongside
//! `member`/`revoked` ref-glob-enumerable -- repurposed to check the property
//! that actually matters with one evaluator: running it twice from cold over
//! the same tables, Nemo's own process-global state included, must derive
//! the same answer set both times.

use std::collections::BTreeMap;

use gix_query_ir::{Bindings, Builtin, Fact, FactSource, HostError, PredicateKey, Registry, Value};
use proptest::prelude::*;

/// `active_member(M) :- member(M), !revoked(M).`
/// `reach(Rev, Rev) :- commit(Rev).`
/// `reach(Rev, C)   :- reach(Rev, X), parent(X, C).`
/// `touched(Rev, C, M) :- reach(Rev, C), author(C, M), active_member(M).`
const RULES: &str = r"
active_member(M) :- member(M), !revoked(M).
reach(Rev, Rev) :- commit(Rev).
reach(Rev, C) :- reach(Rev, X), parent(X, C).
touched(Rev, C, M) :- reach(Rev, C), author(C, M), active_member(M).
";

fn checked_program() -> gix_query_ir::CheckedProgram {
    let registry = Registry::host();
    let mut module = gix_query_parse::parse_module("differential", RULES).expect("rules parse");
    gix_query_parse::resolve_module(&mut module, &registry);
    module.exports.push(gix_query_ir::Export {
        key: PredicateKey::new("touched", 3),
        span: gix_query_ir::Span::synthetic(),
    });
    let mut program = gix_query_ir::Program::new();
    program.add_module(module);
    gix_query_check::check(&program, &registry).expect("rules validate")
}

/// A `FactSource` over a fixed table, honoring `bound` like a real provider
/// -- both evaluators call this identically, so a mismatch here would be a
/// bug in the fixture, not in either evaluator.
struct TableSource(BTreeMap<PredicateKey, Vec<Vec<Value>>>);

impl FactSource for TableSource {
    fn provides(&self, key: &PredicateKey) -> bool {
        self.0.contains_key(key)
    }

    fn facts<'a>(
        &'a self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        _attribute: bool,
    ) -> Result<gix_query_ir::Facts<'a>, HostError> {
        let rows = self
            .0
            .get(key)
            .ok_or_else(|| HostError::NoProvider(key.clone()))?;
        let bound: Vec<Option<Value>> = bound.to_vec();
        Ok(Box::new(rows.iter().filter_map(move |row| {
            let matches = row
                .iter()
                .zip(bound.iter())
                .all(|(v, b)| b.as_ref().is_none_or(|b| b == v));
            matches.then(|| Ok(Fact::new(row.clone())))
        })))
    }
}

/// No builtin call site exists in [`RULES`], so this must never be called.
struct NoBuiltins;

impl Builtin for NoBuiltins {
    fn provides(&self, _key: &PredicateKey) -> bool {
        false
    }

    fn call(
        &self,
        key: &PredicateKey,
        _bound: Bindings<'_>,
        _out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError> {
        Err(HostError::NoProvider(key.clone()))
    }
}

fn sym(s: &str) -> Value {
    Value::sym(s)
}

/// One random extensional database plus the `Rev` to query `touched` at.
#[derive(Debug, Clone)]
struct Case {
    commit: Vec<(String,)>,
    parent: Vec<(String, String)>,
    author: Vec<(String, String)>,
    member: Vec<(String,)>,
    revoked: Vec<(String,)>,
    query_rev: String,
}

fn ident(prefix: &'static str, n: u8) -> impl Strategy<Value = String> {
    (0..n).prop_map(move |i| format!("{prefix}{i}"))
}

fn arb_case() -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(ident("c", 4), 0..4),
        prop::collection::vec((ident("c", 4), ident("c", 4)), 0..5),
        prop::collection::vec((ident("c", 4), ident("k", 3)), 0..4),
        prop::collection::vec(ident("k", 3), 0..3),
        prop::collection::vec(ident("k", 3), 0..3),
        ident("c", 4),
    )
        .prop_map(
            |(commit, parent, author, member, revoked, query_rev)| Case {
                commit: commit.into_iter().map(|c| (c,)).collect(),
                parent,
                author,
                member: member.into_iter().map(|m| (m,)).collect(),
                revoked: revoked.into_iter().map(|m| (m,)).collect(),
                query_rev,
            },
        )
}

fn tables(case: &Case) -> TableSource {
    let mut out = BTreeMap::new();
    out.insert(
        PredicateKey::new("commit", 1),
        case.commit.iter().map(|(a,)| vec![sym(a)]).collect(),
    );
    out.insert(
        PredicateKey::new("parent", 2),
        case.parent
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("author", 2),
        case.author
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("member", 1),
        case.member.iter().map(|(a,)| vec![sym(a)]).collect(),
    );
    out.insert(
        PredicateKey::new("revoked", 1),
        case.revoked.iter().map(|(a,)| vec![sym(a)]).collect(),
    );
    TableSource(out)
}

/// Sorted, deduplicated `(C, M)` rows -- the same normalization every other
/// answer-set assertion in this crate applies before comparing.
fn answer_rows(run: impl FnOnce() -> gix_query_eval::LoopResult) -> Vec<Vec<Value>> {
    let mut out = run().answer;
    out.sort();
    out.dedup();
    out
}

proptest! {
    // Each case runs `run_eager`'s Nemo execution fresh, twice, and
    // `gix-query-eval`'s own module doc pins the process-global timing
    // mutex Nemo's `execute()` enters -- this file, like every other test
    // touching this crate, must run with `--test-threads=1`. Case count kept
    // small to bound wall time under that constraint.
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn run_eager_is_deterministic_on_touched(case in arb_case()) {
        let checked = checked_program();
        let mut goal = gix_query_parse::parse_goal_with_select(
            &format!("touched({}, C, M)", case.query_rev),
            &["C", "M"],
        )
        .expect("goal parses");
        gix_query_parse::resolve_goal(&mut goal, checked.registry());
        gix_query_check::check_goal(&goal, &checked).expect("goal validates");

        let rewritten = gix_query_eval::rewrite_program(&checked, &goal);
        let emitted = gix_query_eval::NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let run = || {
            answer_rows(|| {
                gix_query_eval::run_eager(&rewritten, &emitted, &tables(&case), &NoBuiltins, 64)
                    .expect("eager converges")
            })
        };
        let first = run();
        let second = run();

        prop_assert_eq!(first, second);
    }
}
