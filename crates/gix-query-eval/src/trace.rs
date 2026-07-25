//! Derivation-tree recovery: Nemo's trace, mapped onto our own tree, over
//! *source* rules.
//!
//! Nemo's own [`nemo::execution::tracing::trace::ExecutionTraceTree`] is
//! reachable, typed, and leaf-matchable -- but it has two gaps the spike
//! recorded (`spike-report.md`, gate A4), and both have to be filled from our
//! own rewrite, above the lowering, never worked around inside it:
//!
//! - **Negated literals leave no node.** Nemo's tracer builds subtraces from
//!   a rule's positive body atoms only, so a derivation that turned on
//!   `~revoked(alice)` produces a tree that never mentions `revoked`. This
//!   module reconstructs the witness itself, from the same ground
//!   substitution Nemo already computed for the rule application, applied to
//!   the negated atom Nemo still parsed (even though it never traces into
//!   it).
//! - **Only one derivation comes back where several exist.** `trace` returns
//!   on the first head atom that unifies and fully derives; a fact provable
//!   two ways yields one tree. The spike's own conclusion, which this crate
//!   keeps rather than routes around, is that this is probably the right
//!   product behavior for `explain` -- so [`DerivationTree`] is honestly
//!   named and documented as *a* derivation, not *the* derivations, and
//!   nothing here pretends otherwise or attempts to reimplement Nemo's join
//!   machinery to enumerate every proof.
//!
//! `magic_`-prefixed bookkeeping -- demand guards, magic rules, magic seeds
//! -- never reaches [`DerivationTree`]: every node this module builds is
//! either a base fact or a firing of a rule someone wrote.

use gix_query_ir::{Literal, PredicateKey, RewriteKind, RuleId, Value};
use nemo::execution::tracing::trace::ExecutionTraceTree;

use crate::engine;
use crate::error::EvalError;
use crate::lower::{NemoProgram, NemoTables};
use crate::rewrite::RewrittenProgram;

/// One node of a derivation tree, reported over the rules an author wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivationTree {
    /// A base fact that grounded the derivation directly -- an EDB tuple,
    /// however it reached the table (loaded eagerly or answered on demand).
    Fact {
        /// Which predicate.
        predicate: PredicateKey,
        /// The tuple.
        tuple: Vec<Value>,
    },
    /// One firing of a rule.
    Rule {
        /// The source rule that fired, or `None` for the synthetic rule that
        /// computes a goal's own answer tuples -- a goal has no rule behind
        /// it, the same reason a magic seed carries no source.
        rule: Option<RuleId>,
        /// The head tuple this firing derived.
        head: Vec<Value>,
        /// The positive and builtin body literals that supported it, in
        /// source order.
        support: Vec<DerivationTree>,
        /// Every negated body literal that held, with the ground tuple that
        /// was checked absent -- reconstructed here, never supplied by
        /// Nemo's own trace.
        negations: Vec<NegationWitness>,
    },
}

/// A negated literal's witness: the predicate and the ground tuple that was
/// looked up and found absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegationWitness {
    /// The negated predicate.
    pub predicate: PredicateKey,
    /// The tuple that does not hold.
    pub tuple: Vec<Value>,
}

/// Trace the derivation of `predicate(tuple)` and map it onto a
/// [`DerivationTree`], or `None` if the tuple is not derivable.
///
/// `program` and `tables` should be [`crate::LoopResult::tables`] and the
/// [`NemoProgram`] that produced it -- tracing needs a completed evaluation,
/// and re-running against the same converged tables is the only route to
/// one, since Nemo has no incremental entry point to resume from.
///
/// # Errors
/// Anything [`crate::engine::run`] returns; [`EvalError::Nemo`] if `tuple`
/// cannot be rendered as a ground Nemo fact or if the trace itself fails;
/// [`EvalError::ValueOutOfFragment`] if a value in the trace lands outside
/// our two domains.
pub fn explain(
    program: &NemoProgram,
    tables: &NemoTables,
    rewritten: &RewrittenProgram,
    predicate: &PredicateKey,
    tuple: &[Value],
) -> Result<Option<DerivationTree>, EvalError> {
    let Some(tree) = engine::trace_fact(program, tables, predicate, tuple)? else {
        return Ok(None);
    };
    convert(&tree, rewritten)
}

/// Recursively map one Nemo trace node.
///
/// Returns `Ok(None)` for a node this crate's own bookkeeping produced --
/// dropping it here, rather than filtering it out of the parent's support
/// list afterward, is what keeps a whole demand-guard subtree from ever being
/// built in the first place.
fn convert(
    tree: &ExecutionTraceTree,
    rewritten: &RewrittenProgram,
) -> Result<Option<DerivationTree>, EvalError> {
    match tree {
        ExecutionTraceTree::Fact(atom) => {
            let arity = atom.datavalues().count();
            let predicate = PredicateKey::new(atom.predicate().to_string(), arity);
            if predicate.is_magic() {
                return Ok(None);
            }
            let tuple = atom
                .datavalues()
                .map(|v| engine::value_from_nemo(&v))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(DerivationTree::Fact { predicate, tuple }))
        }
        ExecutionTraceTree::Rule(application, children) => {
            let generated_id = RuleId(application.rule_index);
            let Some(generated_rule) = rewritten.rules.rule(generated_id) else {
                return Err(EvalError::Nemo(format!(
                    "trace referenced rule index {} outside the rewritten program",
                    application.rule_index
                )));
            };
            let origin = generated_rule.origin.as_ref();
            let bookkeeping = origin
                .is_some_and(|o| matches!(o.kind, RewriteKind::MagicSeed | RewriteKind::MagicRule));
            if bookkeeping {
                return Ok(None);
            }
            let source_rule = rewritten.rules.source_of(generated_id);

            let mut head_atom = application.rule.head()[application.head_index].clone();
            application.assignment.apply(&mut head_atom);
            let head = head_atom
                .terms()
                .map(term_value)
                .collect::<Result<Vec<_>, _>>()?;

            // Nemo's `children` correspond, in order, to exactly the
            // generator (positive/builtin) literals of the rule body -- the
            // same ones our own body preserves untouched, since the rewrite
            // never renames a call site.
            let mut support = Vec::with_capacity(children.len());
            let mut nemo_children = children.iter();
            for lit in &generated_rule.body {
                if !lit.is_generator() {
                    continue;
                }
                let Some(child) = nemo_children.next() else {
                    break;
                };
                if let Some(sub) = convert(child, rewritten)? {
                    support.push(sub);
                }
            }

            // Negation witnesses: ground the same negated atom Nemo parsed
            // but never traces into, using the substitution it already
            // computed for this very rule application.
            let mut negations = Vec::new();
            let mut nemo_negative = application.rule.body_negative();
            for lit in &generated_rule.body {
                let Literal::Negated(our_atom) = lit else {
                    continue;
                };
                let Some(nemo_atom) = nemo_negative.next() else {
                    break;
                };
                if our_atom.predicate.is_magic() {
                    continue;
                }
                let mut grounded = nemo_atom.clone();
                application.assignment.apply(&mut grounded);
                let tuple = grounded
                    .terms()
                    .map(term_value)
                    .collect::<Result<Vec<_>, _>>()?;
                negations.push(NegationWitness {
                    predicate: our_atom.predicate.clone(),
                    tuple,
                });
            }

            Ok(Some(DerivationTree::Rule {
                rule: source_rule,
                head,
                support,
                negations,
            }))
        }
    }
}

/// Read a fully-substituted rule-model term as one of our values.
///
/// # Errors
/// [`EvalError::Nemo`] if the term is still a variable -- which would mean
/// the substitution Nemo handed us did not cover it, an engine bug rather
/// than a user error, since range restriction guarantees every variable in a
/// fired rule is bound.
fn term_value(term: &nemo::rule_model::components::term::Term) -> Result<Value, EvalError> {
    use nemo::rule_model::components::term::Term as NemoTerm;
    use nemo::rule_model::components::term::primitive::Primitive;
    match term {
        NemoTerm::Primitive(Primitive::Ground(g)) => engine::value_from_nemo(&g.value()),
        other => Err(EvalError::Nemo(format!(
            "a term in a traced rule application was not fully substituted: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demand::run_to_fixpoint;
    use crate::lower::NemoProgram;
    use crate::rewrite::rewrite;
    use crate::testutil::StubTable;
    use gix_query_ir::{
        Atom, CheckedProgram, Goal, Literal, PredicateEntry, Program, Registry, Rule, RuleId,
        Stratification, Term, Var, Visibility,
    };
    use std::collections::BTreeMap as Map;

    // Nemo's `execute()` enters a process-global timing mutex; a second
    // concurrent evaluation in this process panics and poisons it
    // permanently. Every test in this module must run with
    // `--test-threads=1`.

    fn pos(name: &str, vars: &[&str]) -> Literal {
        Literal::Positive(Atom::new(
            name,
            vars.iter().map(|v| Term::var(*v)).collect(),
        ))
    }

    fn sym(x: &str) -> Value {
        Value::sym(x)
    }

    #[test]
    fn a_positive_goal_traces_to_its_base_fact() {
        // `p(B) :- member(B), revoked(B)`, nothing adorned anywhere: a
        // one-step derivation over two base facts joined on `B`. A
        // single-atom body (`p(B) :- member(B)` alone) is a case Nemo's own
        // chase resolves without an intervening rule application at all --
        // its trace hands back a bare `Fact` node named `p` directly, since
        // nothing distinguishes the projection from an input fact for
        // tracing purposes. A join is what forces a genuine rule-application
        // node, which is the shape this test exists to pin.
        let mut registry = Registry::host();
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
                vec![pos("member", &["B"]), pos("revoked", &["B"])],
            ),
            "test",
        );
        let checked = CheckedProgram {
            program,
            registry,
            stratification: Stratification::default(),
            adornments: Map::new(),
            footprints: Map::new(),
        };
        let goal = Goal::new(vec![pos("p", &["B"])], vec![Var::new("B")]);
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let facts = StubTable::new()
            .with(PredicateKey::new("member", 1), vec![vec![sym("alice")]])
            .with(PredicateKey::new("revoked", 1), vec![vec![sym("alice")]]);
        let builtins = StubTable::new();
        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");

        let tree = explain(
            &emitted,
            &result.tables,
            &rewritten,
            &PredicateKey::new("p", 1),
            &[sym("alice")],
        )
        .expect("explain succeeds")
        .expect("p(alice) is derivable");

        assert_eq!(
            tree,
            DerivationTree::Rule {
                rule: Some(RuleId(0)),
                head: vec![sym("alice")],
                support: vec![
                    DerivationTree::Fact {
                        predicate: PredicateKey::new("member", 1),
                        tuple: vec![sym("alice")],
                    },
                    DerivationTree::Fact {
                        predicate: PredicateKey::new("revoked", 1),
                        tuple: vec![sym("alice")],
                    },
                ],
                negations: vec![],
            }
        );
    }

    #[test]
    fn a_recursive_goal_traces_through_its_own_derivation() {
        // `reach(X,Y) :- parent(X,Y). reach(X,Z) :- parent(X,Y), reach(Y,Z).`
        // over `parent = {c->b, b->a}`, tracing `reach(c,a)` -- the shape
        // gate A4's spike fixture used, adapted to real types.
        let mut registry = Registry::host();
        registry
            .insert_predicate(PredicateEntry::idb(
                PredicateKey::new("reach", 2),
                Visibility::Public,
            ))
            .expect("insertable");
        let mut program = Program::new();
        program.push_rule(
            Rule::new(
                Atom::new("reach", vec![Term::var("X"), Term::var("Y")]),
                vec![pos("parent", &["X", "Y"])],
            ),
            "test",
        );
        program.push_rule(
            Rule::new(
                Atom::new("reach", vec![Term::var("X"), Term::var("Z")]),
                vec![pos("parent", &["X", "Y"]), pos("reach", &["Y", "Z"])],
            ),
            "test",
        );
        let checked = CheckedProgram {
            program,
            registry,
            stratification: Stratification::default(),
            adornments: Map::new(),
            footprints: Map::new(),
        };
        let goal = Goal::new(
            vec![pos("reach", &["X", "Z"])],
            vec![Var::new("X"), Var::new("Z")],
        );
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let facts = StubTable::new().with(
            PredicateKey::new("parent", 2),
            vec![vec![sym("c"), sym("b")], vec![sym("b"), sym("a")]],
        );
        let builtins = StubTable::new();
        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");

        let tree = explain(
            &emitted,
            &result.tables,
            &rewritten,
            &PredicateKey::new("reach", 2),
            &[sym("c"), sym("a")],
        )
        .expect("explain succeeds")
        .expect("reach(c,a) is derivable");

        assert_eq!(
            tree,
            DerivationTree::Rule {
                rule: Some(RuleId(1)),
                head: vec![sym("c"), sym("a")],
                support: vec![
                    DerivationTree::Fact {
                        predicate: PredicateKey::new("parent", 2),
                        tuple: vec![sym("c"), sym("b")],
                    },
                    DerivationTree::Rule {
                        rule: Some(RuleId(0)),
                        head: vec![sym("b"), sym("a")],
                        support: vec![DerivationTree::Fact {
                            predicate: PredicateKey::new("parent", 2),
                            tuple: vec![sym("b"), sym("a")],
                        }],
                        negations: vec![],
                    },
                ],
                negations: vec![],
            }
        );
    }

    #[test]
    fn a_negation_witness_is_reconstructed_from_the_rewrite() {
        // `active(M) :- member(M), ~revoked(M).` -- Nemo's own trace would
        // show only `member`; the `revoked` witness has to come from here.
        let mut registry = Registry::host();
        registry
            .insert_predicate(PredicateEntry::idb(
                PredicateKey::new("active", 1),
                Visibility::Public,
            ))
            .expect("insertable");
        let mut program = Program::new();
        program.push_rule(
            Rule::new(
                Atom::new("active", vec![Term::var("M")]),
                vec![
                    pos("member", &["M"]),
                    Literal::Negated(Atom::new("revoked", vec![Term::var("M")])),
                ],
            ),
            "test",
        );
        let checked = CheckedProgram {
            program,
            registry,
            stratification: Stratification::default(),
            adornments: Map::new(),
            footprints: Map::new(),
        };
        let goal = Goal::new(vec![pos("active", &["M"])], vec![Var::new("M")]);
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.audit().expect("audit passes");

        let facts = StubTable::new()
            .with(
                PredicateKey::new("member", 1),
                vec![vec![sym("alice")], vec![sym("bob")]],
            )
            .with(PredicateKey::new("revoked", 1), vec![vec![sym("bob")]]);
        let builtins = StubTable::new();
        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");

        let tree = explain(
            &emitted,
            &result.tables,
            &rewritten,
            &PredicateKey::new("active", 1),
            &[sym("alice")],
        )
        .expect("explain succeeds")
        .expect("active(alice) is derivable");

        assert_eq!(
            tree,
            DerivationTree::Rule {
                rule: Some(RuleId(0)),
                head: vec![sym("alice")],
                support: vec![DerivationTree::Fact {
                    predicate: PredicateKey::new("member", 1),
                    tuple: vec![sym("alice")],
                }],
                negations: vec![NegationWitness {
                    predicate: PredicateKey::new("revoked", 1),
                    tuple: vec![sym("alice")],
                }],
            }
        );
    }

    #[test]
    fn an_underivable_fact_traces_to_nothing() {
        let mut registry = Registry::host();
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
                vec![pos("member", &["B"])],
            ),
            "test",
        );
        let checked = CheckedProgram {
            program,
            registry,
            stratification: Stratification::default(),
            adornments: Map::new(),
            footprints: Map::new(),
        };
        let goal = Goal::new(vec![pos("p", &["B"])], vec![Var::new("B")]);
        let rewritten = rewrite(&checked, &goal);
        let emitted = NemoProgram::emit(&rewritten).expect("emits");

        let facts = StubTable::new().with(PredicateKey::new("member", 1), vec![vec![sym("alice")]]);
        let builtins = StubTable::new();
        let result =
            run_to_fixpoint(&rewritten, &emitted, &facts, &builtins, 8).expect("converges");

        let tree = explain(
            &emitted,
            &result.tables,
            &rewritten,
            &PredicateKey::new("p", 1),
            &[sym("zzz")],
        )
        .expect("explain succeeds");
        assert_eq!(tree, None);
    }
}
