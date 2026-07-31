//! Eager evaluation for the ref-glob EDB (`DEVPLAN.md` §0).
//!
//! Every enumerable base predicate the rewrite touches -- called free or
//! demanded bound, it makes no difference, since its whole extension is
//! affordable to read -- is loaded once before the first round and never
//! asked through the demand loop again. A required-bound EDB predicate or a
//! builtin still cannot be enumerated cold (`commit`, `parent`,
//! `tree_entry`, `author`, `line`, and every builtin), so those keep going
//! through [`crate::demand`]'s one-bound-call-at-a-time loop underneath this
//! one. This is the "narrow cut" §0 landed on after the general demand loop
//! turned out to still be load-bearing for those five predicates.

use std::collections::{BTreeMap, BTreeSet};

use gix_query_ir::{Builtin, FactSource, PredicateKey, Registry, Value};

use crate::demand::{CallLog, LoggedCall, LoopResult, bindings_from_demand};
use crate::engine;
use crate::error::EvalError;
use crate::lower::{NemoProgram, NemoTables};
use crate::rewrite::{DemandKind, RewrittenProgram};

/// Run `rewritten` to a fixpoint, eagerly preloading every enumerable base
/// predicate it touches instead of demanding it one bound call at a time.
///
/// `program` must be [`NemoProgram::emit`]'s output for the very same
/// `rewritten`, exactly as [`crate::demand::run_to_fixpoint`] requires --
/// this function drives the identical rewritten program, just with a
/// different table-loading strategy. `registry` is `rewritten`'s own
/// [`CheckedProgram`](gix_query_ir::CheckedProgram)'s registry, consulted
/// only for [`gix_query_ir::PredicateEntry::is_enumerable`].
///
/// # Errors
/// [`EvalError::DemandDidNotConverge`] if `max_rounds` is exhausted; anything
/// [`crate::engine::run`] or a provider returns.
pub fn run_eager(
    rewritten: &RewrittenProgram,
    program: &NemoProgram,
    registry: &Registry,
    facts: &dyn FactSource,
    builtins: &dyn Builtin,
    max_rounds: usize,
) -> Result<LoopResult, EvalError> {
    let mut tables = NemoTables::new();
    let mut preloaded: BTreeSet<PredicateKey> = BTreeSet::new();

    for key in &rewritten.eager {
        load_whole(key, facts, &mut tables)?;
        preloaded.insert(key.clone());
    }
    for site in &rewritten.demand {
        if site.kind == DemandKind::Edb
            && !preloaded.contains(&site.base)
            && is_enumerable(&site.base, registry)
        {
            load_whole(&site.base, facts, &mut tables)?;
            preloaded.insert(site.base.clone());
        }
    }

    let mut answered: BTreeMap<PredicateKey, BTreeSet<Vec<Value>>> = BTreeMap::new();
    let mut log = Vec::new();

    for round in 1..=max_rounds {
        let mut engine = engine::run(program, &tables)?;
        let mut converged = true;

        for site in &rewritten.demand {
            if site.kind == DemandKind::Idb || preloaded.contains(&site.base) {
                continue;
            }
            let demand_key = site.demand_key();
            let demanded = engine::relation(&mut engine, &demand_key)?;
            let seen = answered.entry(demand_key).or_default();
            for tuple in demanded {
                if !seen.insert(tuple.clone()) {
                    continue;
                }
                converged = false;

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

        if converged {
            let mut answer = engine::relation(&mut engine, &rewritten.answer)?;
            if rewritten.answer_is_nullary {
                answer = answer.into_iter().map(|_| Vec::new()).collect();
            }
            return Ok(LoopResult {
                rounds: round,
                log: CallLog::from_calls(log),
                answer,
                tables,
            });
        }
    }

    Err(EvalError::DemandDidNotConverge(max_rounds))
}

fn is_enumerable(key: &PredicateKey, registry: &Registry) -> bool {
    registry
        .predicate(key)
        .is_some_and(gix_query_ir::PredicateEntry::is_enumerable)
}

/// Read a predicate's whole extension through `facts` -- the eager
/// materialization step itself, valid only for a predicate
/// [`gix_query_ir::PredicateEntry::is_enumerable`] says can be read whole.
fn load_whole(
    key: &PredicateKey,
    facts: &dyn FactSource,
    tables: &mut NemoTables,
) -> Result<(), EvalError> {
    let all_free = vec![None; key.arity];
    let rows: Vec<Vec<Value>> = facts
        .facts(key, &all_free, false)?
        .map(|f| f.map(|f| f.values))
        .collect::<Result<_, _>>()?;
    tables.insert(key.clone(), rows);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lower::NemoProgram;
    use crate::rewrite::rewrite;
    use crate::testutil::{StubTable, edb_enumerable};
    use gix_query_ir::{
        Atom, CheckedProgram, Goal, Literal, PredicateEntry, Program, Rule, Stratification, Term,
        Var, Visibility,
    };
    use std::collections::BTreeMap as Map;

    // Same process-global Nemo timing mutex as `demand`'s tests: run this
    // module with `--test-threads=1`.

    /// `p(B) :- q(A), r(A, B)` with `q` enumerable and `r` an enumerable
    /// two-place EDB relation called free -- both should be preloaded
    /// whole, with no demand round needed at all.
    fn all_enumerable_program() -> (CheckedProgram, Goal) {
        let mut registry = Registry::host();
        edb_enumerable(&mut registry, "q", 1);
        edb_enumerable(&mut registry, "r", 2);
        registry
            .insert_predicate(PredicateEntry::idb(
                PredicateKey::new("p", 1),
                Visibility::Public,
            ))
            .expect("insertable");

        let mut program = Program::new();
        program.push_rule(
            Rule::new(
                Atom::new("p", vec![Term::var("B")]),
                vec![
                    Literal::Positive(Atom::new("q", vec![Term::var("A")])),
                    Literal::Positive(Atom::new("r", vec![Term::var("A"), Term::var("B")])),
                ],
            ),
            "test",
        );
        let checked = CheckedProgram::assemble(
            program,
            registry,
            Stratification::default(),
            Map::new(),
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
    fn a_fully_enumerable_program_converges_in_one_round_with_no_calls() {
        let (checked, goal) = all_enumerable_program();
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let facts = StubTable::new()
            .with(PredicateKey::new("q", 1), vec![vec![s("a1")]])
            .with(
                PredicateKey::new("r", 2),
                vec![vec![s("a1"), s("b1")], vec![s("a2"), s("b2")]],
            );
        let builtins = StubTable::new();

        let result = run_eager(
            &rewritten,
            &emitted,
            checked.registry(),
            &facts,
            &builtins,
            8,
        )
        .expect("converges");
        assert_eq!(result.rounds, 1, "nothing left to demand after preload");
        assert!(result.log.calls().is_empty());
        assert_eq!(result.answer, vec![vec![s("b1")]]);
    }

    /// `reach(Rev, Rev) :- commit(Rev)` -- `commit` is required-bound, so it
    /// must still go through demand even under the eager path.
    #[test]
    fn a_required_bound_edb_predicate_still_goes_through_demand() {
        let mut registry = Registry::host();
        let mut reach = PredicateEntry::idb(PredicateKey::new("reach", 2), Visibility::Public);
        reach.class = gix_query_ir::PredicateClass::Idb {
            required_bound: Some(gix_query_ir::ArgSet::from_indices([0])),
        };
        registry.insert_predicate(reach).expect("insertable");

        let mut program = Program::new();
        let id = program.push_rule(
            Rule::new(
                Atom::new("reach", vec![Term::var("Rev"), Term::var("Rev")]),
                vec![Literal::Positive(Atom::new(
                    "commit",
                    vec![Term::var("Rev")],
                ))],
            ),
            "test",
        );
        let mut adornments = Map::new();
        adornments.insert(
            id,
            gix_query_ir::RuleAdornments {
                rule: id,
                head: gix_query_ir::Adornment::new(gix_query_ir::ArgSet::from_indices([0]), 2),
                body: vec![gix_query_ir::Adornment::new(
                    gix_query_ir::ArgSet::from_indices([0]),
                    1,
                )],
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
            vec![Literal::Positive(Atom::new(
                "reach",
                vec![Term::sym("deadbeef"), Term::var("C")],
            ))],
            vec![Var::new("C")],
        );

        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let facts = StubTable::new().with(
            PredicateKey::new("commit", 1),
            vec![vec![s("deadbeef")], vec![s("other")]],
        );
        let builtins = StubTable::new();

        let result = run_eager(
            &rewritten,
            &emitted,
            checked.registry(),
            &facts,
            &builtins,
            8,
        )
        .expect("converges");
        assert!(
            result
                .log
                .calls()
                .iter()
                .any(|c| c.base == PredicateKey::new("commit", 1)),
            "commit must have been demanded, not preloaded: {:?}",
            result.log.calls()
        );
        assert_eq!(result.answer, vec![vec![s("deadbeef")]]);
    }
}
