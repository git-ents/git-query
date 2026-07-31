//! The demand loop, and its call log.
//!
//! Nemo is bottom-up; a [`FactSource`] and a [`Builtin`] only answer bound
//! calls. Those do not meet without a loop on our side: run the rewritten
//! program to fixpoint, read the `magic_`-prefixed demand relations it
//! produced, answer the ones an IDB predicate cannot answer for itself,
//! inject the answers as ordinary facts, and run again -- from cold, since
//! Nemo has no incremental entry point. The loop stops the first round that
//! asks for nothing it had not already answered.
//!
//! # The call log is not a debugging aid
//!
//! `explain` needs to say which builtin ran in which mode, and the pass-7
//! footprint audit needs to know a ref-backed predicate was actually read --
//! neither can be recovered from Nemo's own trace, which records derivations,
//! not host calls. So every call this loop makes is recorded in
//! [`CallLog`], including the ones that came back empty: a call with no
//! answers is not a failure, and dropping it from the log would make it
//! indistinguishable from a call that was never made at all.

use std::collections::{BTreeMap, BTreeSet};

use gix_query_ir::{Adornment, Builtin, FactSource, PredicateKey, Value};

use crate::engine;
use crate::error::EvalError;
use crate::lower::{NemoProgram, NemoTables};
use crate::rewrite::{DemandKind, LoweredProgram};

/// One answered call, recorded as it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedCall {
    /// Which round asked.
    pub round: usize,
    /// The predicate as written in source text.
    pub base: PredicateKey,
    /// Which side of the seam answered it.
    pub kind: DemandKind,
    /// The bound arguments, in the predicate's bound-position order -- the
    /// same order the demand relation carried them in.
    pub bound: Vec<Value>,
    /// How many complete tuples came back. Zero is a valid answer, not an
    /// error.
    pub answers: usize,
}

/// Every call the loop made, in the order it made them.
///
/// A `Vec` behind an accessor rather than a bare public field: the log is
/// append-only from this module's point of view, and a consumer should not be
/// able to reorder or truncate what is, per the module doc, an audit record.
#[derive(Debug, Clone, Default)]
pub struct CallLog(Vec<LoggedCall>);

impl CallLog {
    /// Every call, in the order it was made.
    #[must_use]
    pub fn calls(&self) -> &[LoggedCall] {
        &self.0
    }

    /// Build a log from calls already made, in order.
    ///
    /// `pub(crate)` rather than a `From` impl: construction is a crate-internal
    /// concern (`crate::eager` builds its own call sequence), while a consumer
    /// outside this crate only ever reads one back through [`Self::calls`].
    pub(crate) fn from_calls(calls: Vec<LoggedCall>) -> Self {
        Self(calls)
    }
}

/// What one round of the loop found.
///
/// An enum rather than a bare `bool`: a caller matching on this cannot
/// mistake "converged" for "not yet checked", which is exactly the class of
/// mistake an ignorable flag invites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoundOutcome {
    /// At least one call was made that no earlier round had already answered.
    NewDemand,
    /// Nothing new was asked; the fixpoint has been reached.
    Converged,
}

/// What the loop produced.
#[derive(Debug, Clone)]
pub struct LoopResult {
    /// How many rounds ran, including the last one that confirmed
    /// convergence.
    pub rounds: usize,
    /// Every host call made along the way.
    pub log: CallLog,
    /// The query's answer tuples, in `select` column order, as the final
    /// round's engine computed them.
    pub answer: Vec<Vec<Value>>,
    /// The tables the final, converged round ran against -- what
    /// [`crate::trace::explain`] re-executes to trace a derivation, since
    /// tracing needs a completed evaluation and there is no cheaper route to
    /// one than running the converged state fresh.
    pub tables: NemoTables,
}

/// Run `rewritten` to a demand fixpoint.
///
/// `program` must be [`NemoProgram::emit`]'s output for the very same
/// `rewritten` -- the rewrite decides what is demanded, the emitter decides
/// what Nemo actually runs, and this loop trusts that the two agree, which is
/// exactly what the emission audit is for. Run that audit once, before the
/// first round; this function does not re-run it per round, since the rule
/// text never changes between rounds, only the tables do.
///
/// # Errors
/// [`EvalError::DemandDidNotConverge`] if `max_rounds` is exhausted; anything
/// [`crate::engine::run`] or a provider returns.
pub fn run_to_fixpoint(
    rewritten: &LoweredProgram,
    program: &NemoProgram,
    facts: &dyn FactSource,
    builtins: &dyn Builtin,
    max_rounds: usize,
) -> Result<LoopResult, EvalError> {
    let mut tables = NemoTables::new();
    for key in &rewritten.eager {
        let all_free = vec![None; key.arity];
        let rows: Vec<Vec<Value>> = facts
            .facts(key, &all_free, false)?
            .map(|f| f.map(|f| f.values))
            .collect::<Result<_, _>>()?;
        tables.insert(key.clone(), rows);
    }

    // Memoized by the demand relation's own key, which already encodes the
    // predicate and adornment -- this is the loop's memo, and it is what
    // makes a cycle in the demand chain terminate rather than re-ask forever.
    let mut answered: BTreeMap<PredicateKey, BTreeSet<Vec<Value>>> = BTreeMap::new();
    let mut log = Vec::new();

    for round in 1..=max_rounds {
        let mut engine = engine::run(program, &tables)?;
        let mut outcome = RoundOutcome::Converged;

        for site in &rewritten.demand {
            if site.kind == DemandKind::Idb {
                // Nothing outside Nemo answers this one; its own guarded
                // rules already consumed the demand within this round's
                // `execute()`.
                continue;
            }
            let demand_key = site.demand_key();
            let demanded = engine::relation(&mut engine, &demand_key)?;
            let seen = answered.entry(demand_key).or_default();
            for tuple in demanded {
                if !seen.insert(tuple.clone()) {
                    continue;
                }
                outcome = RoundOutcome::NewDemand;

                let bound = bindings_from_demand(&site.adornment, &tuple);
                let complete = match site.kind {
                    DemandKind::Edb => facts
                        .facts(&site.base, &bound, false)?
                        .map(|f| f.map(|f| f.values))
                        .collect::<Result<Vec<_>, _>>()?,
                    DemandKind::Builtin => {
                        let mut out = Vec::new();
                        builtins.call(&site.base, &bound, &mut out)?;
                        out
                    }
                    DemandKind::Idb => unreachable!("filtered above"),
                };
                log.push(LoggedCall {
                    round,
                    base: site.base.clone(),
                    kind: site.kind,
                    bound: tuple,
                    answers: complete.len(),
                });
                tables.insert(site.base.clone(), complete);
            }
        }

        if outcome == RoundOutcome::Converged {
            let mut answer = engine::relation(&mut engine, &rewritten.answer)?;
            if rewritten.answer_is_nullary {
                answer = answer.into_iter().map(|_| Vec::new()).collect();
            }
            return Ok(LoopResult {
                rounds: round,
                log: CallLog(log),
                answer,
                tables,
            });
        }
    }

    Err(EvalError::DemandDidNotConverge(max_rounds))
}

/// Expand a demand relation's row -- values at the bound positions only, in
/// ascending position order -- into a full `Bindings` pattern: `Some` at
/// every bound position, `None` elsewhere, ready to hand a provider.
///
/// `pub(crate)`: `crate::eager` drives the same required-bound demand sites
/// this loop does and must expand them identically.
pub(crate) fn bindings_from_demand(
    adornment: &Adornment,
    demand_row: &[Value],
) -> Vec<Option<Value>> {
    let mut out = vec![None; adornment.arity];
    let mut i = 0;
    for (pos, slot) in out.iter_mut().enumerate() {
        if adornment.bound.contains(pos) {
            *slot = Some(demand_row[i].clone());
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::NemoProgram;
    use crate::rewrite::rewrite;
    use crate::testutil::{StubTable, edb_enumerable, edb_required_bound};
    use gix_query_ir::{
        Atom, CheckedProgram, Goal, Literal, PredicateEntry, Program, Registry, Rule,
        RuleAdornments, Stratification, Term, Var, Visibility,
    };
    use std::collections::BTreeMap as Map;

    // Nemo's `execute()` enters a process-global timing mutex; a second
    // concurrent evaluation in this process panics and poisons it
    // permanently. Every test in this module must run with
    // `--test-threads=1`.

    /// `p(B) :- q(A, R), anchor_at(A, R, B)`, adapted from the spike's A3
    /// fixture: `anchor_at` is a builtin, `+A +R -B`, so its call sites are
    /// demand sites and the loop has to bridge them.
    fn simple_program() -> (CheckedProgram, Goal) {
        let mut registry = Registry::host();
        edb_enumerable(&mut registry, "q", 2);
        registry
            .insert_predicate(PredicateEntry::idb(
                PredicateKey::new("p", 1),
                Visibility::Public,
            ))
            .expect("insertable");
        registry.insert_builtin(gix_query_ir::BuiltinEntry {
            key: PredicateKey::new("anchor_at", 3),
            types: vec![],
            modes: gix_query_ir::ModeVector::new(vec![
                gix_query_ir::Mode::Bound,
                gix_query_ir::Mode::Bound,
                gix_query_ir::Mode::Free,
            ]),
            props: gix_query_ir::BuiltinProps::filter(),
            gate_safe: true,
            doc: "test builtin",
        });

        let mut program = Program::new();
        let id = program.push_rule(
            Rule::new(
                Atom::new("p", vec![Term::var("B")]),
                vec![
                    Literal::Positive(Atom::new("q", vec![Term::var("A"), Term::var("R")])),
                    Literal::Builtin(Atom::new(
                        "anchor_at",
                        vec![Term::var("A"), Term::var("R"), Term::var("B")],
                    )),
                ],
            ),
            "test",
        );
        let mut adornments = Map::new();
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: Adornment::new(gix_query_ir::ArgSet::empty(), 1),
                body: vec![
                    Adornment::new(gix_query_ir::ArgSet::empty(), 2),
                    Adornment::new(gix_query_ir::ArgSet::from_indices([0, 1]), 3),
                ],
            },
        );

        let checked = CheckedProgram::assemble(
            program,
            registry,
            Stratification::default(),
            adornments,
            Map::new(),
        );
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new("p", vec![Term::var("B")]))],
            vec![Var::new("B")],
        );
        (checked, goal)
    }

    fn s(x: &str) -> Value {
        Value::sym(x)
    }

    #[test]
    fn a_flat_query_converges_in_two_rounds() {
        let (checked, goal) = simple_program();
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let anchor_at = PredicateKey::new("anchor_at", 3);
        let builtins = StubTable::new().with(
            anchor_at.clone(),
            vec![
                vec![s("a1"), s("r1"), s("blob1")],
                vec![s("a2"), s("r1"), s("blob2")],
            ],
        );
        let facts = StubTable::new().with(
            PredicateKey::new("q", 2),
            vec![vec![s("a1"), s("r1")], vec![s("a2"), s("r1")]],
        );

        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");
        assert_eq!(
            result.rounds, 2,
            "one round to ask, one to confirm nothing new"
        );
        assert_eq!(result.log.calls().len(), 2);
        assert!(result.log.calls().iter().all(|c| c.round == 1));

        let mut answer: Vec<Value> = result
            .answer
            .into_iter()
            .map(|row| row[0].clone())
            .collect();
        answer.sort();
        assert_eq!(answer, vec![s("blob1"), s("blob2")]);
    }

    /// A bare `q/2` EDB predicate and nothing else -- no rules, so no
    /// unguarded builtin call site to demand eagerly.
    fn edb_only_program() -> CheckedProgram {
        let mut registry = Registry::host();
        edb_enumerable(&mut registry, "q", 2);
        CheckedProgram::assemble(
            Program::new(),
            registry,
            Stratification::default(),
            Map::new(),
            Map::new(),
        )
    }

    #[test]
    fn a_fully_bound_query_returns_existence_not_a_spurious_column() {
        // The rewrite pads a zero-`select` goal's answer to arity 1 so it
        // can lower to Nemo at all; this is the readback half, checking
        // that padding never leaks into what a fully-bound tier-1 query
        // actually returns.
        let checked = edb_only_program();
        let facts = StubTable::new().with(
            PredicateKey::new("q", 2),
            vec![vec![s("a1"), s("r1")], vec![s("a2"), s("r1")]],
        );
        let builtins = StubTable::new();

        let present = Goal::new(
            vec![Literal::Positive(Atom::new(
                "q",
                vec![Term::sym("a1"), Term::sym("r1")],
            ))],
            vec![],
        );
        let rewritten = rewrite(&checked, &present);
        assert!(rewritten.answer_is_nullary);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");
        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");
        assert_eq!(
            result.answer,
            vec![Vec::new()],
            "one empty row signals the query holds, not a row carrying the padding constant"
        );

        let absent = Goal::new(
            vec![Literal::Positive(Atom::new(
                "q",
                vec![Term::sym("nope"), Term::sym("r1")],
            ))],
            vec![],
        );
        let rewritten = rewrite(&checked, &absent);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");
        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");
        assert_eq!(
            result.answer,
            Vec::<Vec<Value>>::new(),
            "no rows signals the query does not hold"
        );
    }

    #[test]
    fn a_call_with_no_answers_is_logged_not_dropped() {
        let (checked, goal) = simple_program();
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");

        let anchor_at = PredicateKey::new("anchor_at", 3);
        let builtins = StubTable::new().with(anchor_at, vec![vec![s("a1"), s("r1"), s("blob1")]]);
        let facts = StubTable::new().with(
            PredicateKey::new("q", 2),
            vec![vec![s("a1"), s("r1")], vec![s("missing"), s("r1")]],
        );

        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");
        let missing = result
            .log
            .calls()
            .iter()
            .find(|c| c.bound[0] == s("missing"))
            .expect("the empty call was logged");
        assert_eq!(missing.answers, 0);
    }

    #[test]
    fn the_builtin_is_never_enumerated() {
        let (checked, goal) = simple_program();
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");

        let anchor_at = PredicateKey::new("anchor_at", 3);
        let rows: Vec<_> = (0..10_000)
            .map(|i| vec![s(&format!("a{i}")), s("r1"), s(&format!("blob{i}"))])
            .collect();
        let builtins = StubTable::new().with(anchor_at.clone(), rows);
        let facts = StubTable::new().with(PredicateKey::new("q", 2), vec![vec![s("a7"), s("r1")]]);

        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");
        assert_eq!(builtins.call_count(&anchor_at), 1);
        assert_eq!(result.answer.len(), 1);
        assert_eq!(result.answer[0][0], s("blob7"));
    }

    #[test]
    fn chained_demand_converges_after_discovering_new_calls() {
        // `q` is seeded, then extended by a rule that reads `p`'s own output
        // back through `follows`, so a second call is only discoverable after
        // the first has been answered.
        let mut registry = Registry::host();
        edb_enumerable(&mut registry, "seed", 2);
        edb_required_bound(&mut registry, "follows", 3, &[0]);
        for (name, arity) in [("q", 2), ("p", 1)] {
            registry
                .insert_predicate(PredicateEntry::idb(
                    PredicateKey::new(name, arity),
                    Visibility::Public,
                ))
                .expect("insertable");
        }
        registry.insert_builtin(gix_query_ir::BuiltinEntry {
            key: PredicateKey::new("anchor_at", 3),
            types: vec![],
            modes: gix_query_ir::ModeVector::new(vec![
                gix_query_ir::Mode::Bound,
                gix_query_ir::Mode::Bound,
                gix_query_ir::Mode::Free,
            ]),
            props: gix_query_ir::BuiltinProps::filter(),
            gate_safe: true,
            doc: "test builtin",
        });

        let mut program = Program::new();
        let mut adornments = Map::new();
        let free = |arity| Adornment::new(gix_query_ir::ArgSet::empty(), arity);

        let id = program.push_rule(
            Rule::new(
                Atom::new("q", vec![Term::var("A"), Term::var("R")]),
                vec![Literal::Positive(Atom::new(
                    "seed",
                    vec![Term::var("A"), Term::var("R")],
                ))],
            ),
            "test",
        );
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: free(2),
                body: vec![free(2)],
            },
        );

        let id = program.push_rule(
            Rule::new(
                Atom::new("q", vec![Term::var("A"), Term::var("R")]),
                vec![
                    Literal::Positive(Atom::new("p", vec![Term::var("B")])),
                    Literal::Positive(Atom::new(
                        "follows",
                        vec![Term::var("B"), Term::var("A"), Term::var("R")],
                    )),
                ],
            ),
            "test",
        );
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: free(2),
                body: vec![free(1), free(3)],
            },
        );

        let id = program.push_rule(
            Rule::new(
                Atom::new("p", vec![Term::var("B")]),
                vec![
                    Literal::Positive(Atom::new("q", vec![Term::var("A"), Term::var("R")])),
                    Literal::Builtin(Atom::new(
                        "anchor_at",
                        vec![Term::var("A"), Term::var("R"), Term::var("B")],
                    )),
                ],
            ),
            "test",
        );
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: free(1),
                body: vec![
                    free(2),
                    Adornment::new(gix_query_ir::ArgSet::from_indices([0, 1]), 3),
                ],
            },
        );

        let checked = CheckedProgram::assemble(
            program,
            registry,
            Stratification::default(),
            adornments,
            Map::new(),
        );
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new("p", vec![Term::var("B")]))],
            vec![Var::new("B")],
        );
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let anchor_at = PredicateKey::new("anchor_at", 3);
        let builtins = StubTable::new().with(
            anchor_at,
            vec![
                vec![s("a1"), s("r1"), s("blob1")],
                vec![s("a2"), s("r2"), s("blob2")],
                vec![s("a3"), s("r3"), s("blob3")],
            ],
        );
        let facts = StubTable::new()
            .with(PredicateKey::new("seed", 2), vec![vec![s("a1"), s("r1")]])
            .with(
                PredicateKey::new("follows", 3),
                vec![
                    vec![s("blob1"), s("a2"), s("r2")],
                    vec![s("blob2"), s("a3"), s("r3")],
                ],
            );

        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 16).expect("converges");
        let mut answer: Vec<Value> = result
            .answer
            .into_iter()
            .map(|row| row[0].clone())
            .collect();
        answer.sort();
        assert_eq!(answer, vec![s("blob1"), s("blob2"), s("blob3")]);

        let rounds_asked: Vec<usize> = result.log.calls().iter().map(|c| c.round).collect();
        assert_eq!(
            rounds_asked,
            vec![1, 2, 3],
            "each call discovered by the round before it"
        );
        assert_eq!(result.rounds, 4, "a fourth round confirms nothing new");
    }

    #[test]
    fn a_cycle_in_the_chain_still_terminates() {
        // Reuse the chained fixture's shape but make `follows` point back to
        // the seed's own anchor, so the second call's answer would re-demand
        // the first if the log were not a memo.
        let mut registry = Registry::host();
        edb_enumerable(&mut registry, "seed", 2);
        edb_required_bound(&mut registry, "follows", 3, &[0]);
        for (name, arity) in [("q", 2), ("p", 1)] {
            registry
                .insert_predicate(PredicateEntry::idb(
                    PredicateKey::new(name, arity),
                    Visibility::Public,
                ))
                .expect("insertable");
        }
        registry.insert_builtin(gix_query_ir::BuiltinEntry {
            key: PredicateKey::new("anchor_at", 3),
            types: vec![],
            modes: gix_query_ir::ModeVector::new(vec![
                gix_query_ir::Mode::Bound,
                gix_query_ir::Mode::Bound,
                gix_query_ir::Mode::Free,
            ]),
            props: gix_query_ir::BuiltinProps::filter(),
            gate_safe: true,
            doc: "test builtin",
        });
        let mut program = Program::new();
        let mut adornments = Map::new();
        let free = |arity| Adornment::new(gix_query_ir::ArgSet::empty(), arity);
        let id = program.push_rule(
            Rule::new(
                Atom::new("q", vec![Term::var("A"), Term::var("R")]),
                vec![Literal::Positive(Atom::new(
                    "seed",
                    vec![Term::var("A"), Term::var("R")],
                ))],
            ),
            "test",
        );
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: free(2),
                body: vec![free(2)],
            },
        );
        let id = program.push_rule(
            Rule::new(
                Atom::new("q", vec![Term::var("A"), Term::var("R")]),
                vec![
                    Literal::Positive(Atom::new("p", vec![Term::var("B")])),
                    Literal::Positive(Atom::new(
                        "follows",
                        vec![Term::var("B"), Term::var("A"), Term::var("R")],
                    )),
                ],
            ),
            "test",
        );
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: free(2),
                body: vec![free(1), free(3)],
            },
        );
        let id = program.push_rule(
            Rule::new(
                Atom::new("p", vec![Term::var("B")]),
                vec![
                    Literal::Positive(Atom::new("q", vec![Term::var("A"), Term::var("R")])),
                    Literal::Builtin(Atom::new(
                        "anchor_at",
                        vec![Term::var("A"), Term::var("R"), Term::var("B")],
                    )),
                ],
            ),
            "test",
        );
        adornments.insert(
            id,
            RuleAdornments {
                rule: id,
                head: free(1),
                body: vec![
                    free(2),
                    Adornment::new(gix_query_ir::ArgSet::from_indices([0, 1]), 3),
                ],
            },
        );
        let checked = CheckedProgram::assemble(
            program,
            registry,
            Stratification::default(),
            adornments,
            Map::new(),
        );
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new("p", vec![Term::var("B")]))],
            vec![Var::new("B")],
        );
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");

        let anchor_at = PredicateKey::new("anchor_at", 3);
        let builtins = StubTable::new().with(
            anchor_at,
            vec![
                vec![s("a1"), s("r1"), s("b1")],
                vec![s("a2"), s("r2"), s("b2")],
            ],
        );
        // The cycle: b2 follows back to (a1, r1), already answered in round 1.
        let facts = StubTable::new()
            .with(PredicateKey::new("seed", 2), vec![vec![s("a1"), s("r1")]])
            .with(
                PredicateKey::new("follows", 3),
                vec![
                    vec![s("b1"), s("a2"), s("r2")],
                    vec![s("b2"), s("a1"), s("r1")],
                ],
            );

        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 16).expect("converges");
        assert_eq!(result.log.calls().len(), 2, "a call was asked twice");
    }

    #[test]
    fn the_call_log_records_mode_bound_arguments_and_round() {
        let (checked, goal) = simple_program();
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        let anchor_at = PredicateKey::new("anchor_at", 3);
        let builtins =
            StubTable::new().with(anchor_at.clone(), vec![vec![s("a1"), s("r1"), s("blob1")]]);
        let facts = StubTable::new().with(PredicateKey::new("q", 2), vec![vec![s("a1"), s("r1")]]);

        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");
        assert_eq!(result.log.calls().len(), 1);
        let call = &result.log.calls()[0];
        assert_eq!(call.base, anchor_at);
        assert_eq!(call.kind, DemandKind::Builtin);
        assert_eq!(call.bound, vec![s("a1"), s("r1")]);
        assert_eq!(call.answers, 1);
        assert_eq!(call.round, 1);
    }

    #[test]
    fn a_program_that_never_converges_is_a_named_error() {
        // A pathological facts source that always answers with one more
        // distinct value than was asked, so the loop can never run out of
        // new demand -- the failure this crate must report rather than spin
        // on. `q` is fed one row per round from a stub whose facts source
        // depends on nothing (constant), so the *loop's own bug surface*
        // would be a real non-convergence; here we simulate it by handing a
        // vanishingly small round budget instead, which is the same code
        // path a genuine non-convergence would take.
        let (checked, goal) = simple_program();
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        let anchor_at = PredicateKey::new("anchor_at", 3);
        let builtins = StubTable::new().with(anchor_at, vec![vec![s("a1"), s("r1"), s("blob1")]]);
        let facts = StubTable::new().with(PredicateKey::new("q", 2), vec![vec![s("a1"), s("r1")]]);

        let err = run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 1)
            .expect_err("budget too small");
        assert!(matches!(err, EvalError::DemandDidNotConverge(1)));
    }
}
