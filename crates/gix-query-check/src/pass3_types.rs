//! Pass 3 -- arity and types.
//!
//! Arity mismatch is caught by [`crate::pass2_resolve`] as a more precise
//! form of "unresolved" -- [`gix_query_ir::PredicateKey`] carries arity in
//! its identity, so a literal whose argument count does not match the
//! predicate it names simply names a *different* predicate, and there is no
//! separate "wrong arity" case an `Atom` can even represent. What is left
//! for this pass is types: checking constants against declared or
//! closed-domain types, checking that one variable is not asked to be two
//! types in the same rule, and inferring [`TypeSlot::Infer`] positions from
//! the call sites that constrain them.
//!
//! Inference is a small least fixpoint of its own, same shape as pass 8's:
//! a silent sweep that only ever turns `Infer` into `Known` (monotone,
//! bounded by the number of argument positions in the program, so it always
//! terminates), followed by exactly one validation sweep that reports each
//! problem once against the now-stable registry. Running validation on every
//! sweep instead would report the same conflict once per iteration after the
//! registry stabilizes -- a duplicate-diagnostics bug wearing a type-checker
//! costume.

use std::collections::BTreeMap;

use gix_query_ir::{
    ArgType, Atom, Literal, PredicateKey, Program, Registry, Rule, RuleId, Span, Term, TypeSlot,
    Value, ValueType, Var,
};

use crate::error::{CheckError, Errors, Site};

/// Infer every `Infer` slot the program's call sites constrain, then
/// validate every constant and every variable against the now-stable
/// registry.
pub(crate) fn check(program: &Program, registry: &mut Registry, errors: &mut Errors) {
    let bound: usize =
        registry.predicates().map(|e| e.types.len()).sum::<usize>() + program.rules().len() + 16;
    for _ in 0..bound {
        let mut changed = false;
        for (i, rule) in program.rules().iter().enumerate() {
            process_rule(RuleId(i), rule, registry, &mut None, &mut changed);
        }
        if !changed {
            break;
        }
    }
    for (i, rule) in program.rules().iter().enumerate() {
        process_rule(RuleId(i), rule, registry, &mut Some(errors), &mut false);
    }
}

/// Check a standalone goal's constants and variables against an
/// already-inferred registry. A goal defines nothing stored, so the caller
/// passes a scratch clone of the checked program's registry here rather than
/// the shared one -- this can still *locally* resolve a leftover `Infer`
/// slot no rule ever constrained, but that inference is discarded with the
/// clone; a goal can never leave a mark on `CheckedProgram`.
pub(crate) fn check_goal_body(body: &[Literal], registry: &mut Registry, errors: &mut Errors) {
    let mut var_types = BTreeMap::new();
    for lit in body {
        process_literal(
            lit,
            Site::Goal,
            registry,
            &mut var_types,
            &mut Some(errors),
            &mut false,
        );
    }
}

fn process_rule(
    id: RuleId,
    rule: &Rule,
    registry: &mut Registry,
    errors: &mut Option<&mut Errors>,
    changed: &mut bool,
) {
    let site = Site::Rule(id);
    let mut var_types: BTreeMap<Var, ValueType> = BTreeMap::new();
    for lit in &rule.body {
        process_literal(lit, site, registry, &mut var_types, errors, changed);
    }
    process_head(&rule.head, site, registry, &mut var_types, errors, changed);
}

fn process_head(
    head: &Atom,
    site: Site,
    registry: &mut Registry,
    var_types: &mut BTreeMap<Var, ValueType>,
    errors: &mut Option<&mut Errors>,
    changed: &mut bool,
) {
    let terms = head
        .terms
        .iter()
        .enumerate()
        .map(|(i, t)| (i, &t.term, t.span));
    process_terms(
        &head.predicate,
        terms,
        site,
        registry,
        var_types,
        errors,
        changed,
    );
}

fn process_literal(
    lit: &Literal,
    site: Site,
    registry: &mut Registry,
    var_types: &mut BTreeMap<Var, ValueType>,
    errors: &mut Option<&mut Errors>,
    changed: &mut bool,
) {
    match lit {
        Literal::Positive(a) | Literal::Negated(a) | Literal::Builtin(a) => {
            let terms = a
                .terms
                .iter()
                .enumerate()
                .map(|(i, t)| (i, &t.term, t.span));
            process_terms(
                &a.predicate,
                terms,
                site,
                registry,
                var_types,
                errors,
                changed,
            );
        }
        Literal::Compare(c) => {
            let key = c.op.key().clone();
            let terms = [
                (0, &c.left.term, c.left.span),
                (1, &c.right.term, c.right.span),
            ];
            process_terms(
                &key,
                terms.into_iter(),
                site,
                registry,
                var_types,
                errors,
                changed,
            );
        }
    }
}

/// The type each argument position of `key` currently demands, resolving a
/// builtin's [`ArgType::SameAsArg`] against positions already resolved
/// earlier in the same call -- our only two uses (`=`, `!=`) always point
/// backward, so a single left-to-right pass is enough.
fn declared_types(key: &PredicateKey, registry: &Registry) -> Option<Vec<Option<ValueType>>> {
    if let Some(entry) = registry.predicate(key) {
        return Some(entry.types.iter().map(|t| t.known()).collect());
    }
    let b = registry.builtin(key)?;
    let mut out = Vec::with_capacity(b.types.len());
    for t in &b.types {
        let resolved = match t {
            ArgType::Exact(v) => Some(*v),
            ArgType::AnyScalar => None,
            ArgType::SameAsArg(n) => out.get(*n).copied().flatten(),
        };
        out.push(resolved);
    }
    Some(out)
}

fn process_terms<'a>(
    key: &PredicateKey,
    terms: impl Iterator<Item = (usize, &'a Term, Span)>,
    site: Site,
    registry: &mut Registry,
    var_types: &mut BTreeMap<Var, ValueType>,
    errors: &mut Option<&mut Errors>,
    changed: &mut bool,
) {
    let Some(declared) = declared_types(key, registry) else {
        // Unresolved; pass 2 already reported it.
        return;
    };
    for (i, term, span) in terms {
        let slot = declared.get(i).copied().flatten();
        match term {
            Term::Anon => {}
            Term::Const(Value::Int(_)) => {
                if let Some(t) = slot
                    && t != ValueType::Int
                {
                    report(
                        errors,
                        CheckError::ConstTypeMismatch {
                            predicate: key.clone(),
                            argument: i,
                            expected: t,
                            value: term.to_string(),
                            span,
                        },
                    );
                }
            }
            Term::Const(Value::Sym(s)) => {
                if let Some(t) = slot {
                    if t == ValueType::Int {
                        report(
                            errors,
                            CheckError::ConstTypeMismatch {
                                predicate: key.clone(),
                                argument: i,
                                expected: t,
                                value: term.to_string(),
                                span,
                            },
                        );
                    } else if !t.admits(s) {
                        report(
                            errors,
                            CheckError::DomainViolation {
                                predicate: key.clone(),
                                argument: i,
                                expected: t,
                                domain: t.domain().expect("admits false implies a closed domain"),
                                value: s.to_string(),
                                span,
                            },
                        );
                    }
                }
            }
            Term::Var(v) => match (slot, var_types.get(v).copied()) {
                (Some(t), Some(existing)) if existing != t => {
                    report(
                        errors,
                        CheckError::VariableTypeConflict {
                            var: v.clone(),
                            first: existing,
                            second: t,
                            site,
                            span,
                        },
                    );
                }
                (Some(t), None) => {
                    var_types.insert(v.clone(), t);
                }
                (Some(_), Some(_)) => {}
                (None, Some(known)) => propagate(key, i, known, registry, span, errors, changed),
                (None, None) => {}
            },
        }
    }
}

/// Write `t` into `key`'s `Infer` slot at `i`, or report a conflict if it
/// was already `Known` as something else.
fn propagate(
    key: &PredicateKey,
    i: usize,
    t: ValueType,
    registry: &mut Registry,
    span: Span,
    errors: &mut Option<&mut Errors>,
    changed: &mut bool,
) {
    let Some(entry) = registry.predicate_mut(key) else {
        return;
    };
    match entry.types.get(i).copied() {
        Some(TypeSlot::Infer) => {
            entry.types[i] = TypeSlot::Known(t);
            *changed = true;
        }
        Some(TypeSlot::Known(existing)) if existing != t => {
            report(
                errors,
                CheckError::PredicateTypeConflict {
                    predicate: key.clone(),
                    argument: i,
                    first: existing,
                    second: t,
                    span,
                },
            );
        }
        _ => {}
    }
}

fn report(errors: &mut Option<&mut Errors>, err: CheckError) {
    if let Some(errs) = errors {
        errs.push(err);
    }
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Compare, CompareOp, SpannedTerm};

    use super::*;
    use crate::error::Pass;

    fn commit_bound_rule(head: &str, var: &str) -> Rule {
        Rule::new(
            Atom::new(head, vec![Term::var(var)]),
            vec![Literal::Positive(Atom::new("commit", vec![Term::var(var)]))],
        )
    }

    #[test]
    fn a_well_typed_program_accepts() {
        let mut program = Program::new();
        crate::testutil::add(&mut program, commit_bound_rule("has_commit", "R"), "m");
        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        check(&program, &mut registry, &mut errors);
        assert!(errors.is_empty());
        let entry = registry
            .predicate(&PredicateKey::new("has_commit", 1))
            .expect("registered");
        assert_eq!(entry.types, vec![TypeSlot::Known(ValueType::Rev)]);
    }

    #[test]
    fn a_symbol_outside_a_closed_domain_is_pass_three() {
        // labels(pin) is fine, but labels(nonsense) is not once `labels`'s
        // argument is inferred as Oracle through `bind`'s fourth column.
        let mut program = Program::new();
        // Both the seeding call and the use of `labels` must share one
        // clause: types are unified per rule body, so splitting `bind`'s
        // oracle column into a separate rule from the `labels(O)` call
        // would leave `O` unconstrained where it matters.
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("uses", vec![Term::var("O")]),
                vec![
                    Literal::Builtin(Atom::new(
                        "cand",
                        vec![
                            Term::var("A"),
                            Term::var("Rev"),
                            Term::var("L"),
                            Term::var("O"),
                            Term::var("C"),
                        ],
                    )),
                    Literal::Positive(Atom::new("labels", vec![Term::var("O")])),
                ],
            ),
            "m",
        );
        crate::testutil::add(
            &mut program,
            Rule::fact(Atom::new("labels", vec![Term::sym("pin")])),
            "m",
        );
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("bad", vec![]),
                vec![Literal::Positive(Atom::new(
                    "labels",
                    vec![Term::sym("nonsense")],
                ))],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        check(&program, &mut registry, &mut errors);
        assert!(
            errors.iter().any(|e| e.pass() == Pass::Types
                && matches!(e, CheckError::DomainViolation { .. })),
            "expected a domain violation, got {errors:?}"
        );
    }

    #[test]
    fn a_variable_typed_two_ways_in_one_rule_conflicts() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("bad", vec![Term::var("X")]),
                vec![
                    Literal::Positive(Atom::new("commit", vec![Term::var("X")])),
                    Literal::Positive(Atom::new("member", vec![Term::var("X")])),
                ],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        check(&program, &mut registry, &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, CheckError::VariableTypeConflict { .. }))
        );
    }

    #[test]
    fn a_bare_comparison_key_resolves_through_declared_types() {
        // Exercises `declared_types` on a `Compare` literal directly, not
        // just through `process_literal`'s dispatch.
        let registry = Registry::host();
        let key = CompareOp::Lt.key();
        let types = declared_types(key, &registry).expect("comparisons are registered");
        assert_eq!(types, vec![Some(ValueType::Int), Some(ValueType::Int)]);
        // And an out-of-range constant is still just a plain int mismatch,
        // not a special case.
        let compare = Compare {
            op: CompareOp::Lt,
            left: SpannedTerm::synthetic(Term::sym("nope")),
            right: SpannedTerm::synthetic(Term::int(1)),
            span: Span::synthetic(),
        };
        let mut var_types = BTreeMap::new();
        let mut errors = Errors::new();
        process_literal(
            &Literal::Compare(compare),
            Site::Goal,
            &mut registry.clone(),
            &mut var_types,
            &mut Some(&mut errors),
            &mut false,
        );
        assert!(matches!(errors[0], CheckError::ConstTypeMismatch { .. }));
    }
}
