//! Pass 8 -- mode inference.
//!
//! Two phases, same shape as pass 3's type inference and for the same
//! reason: a silent growth sweep that only ever adds bits to a predicate's
//! required-bound set (monotone, bounded, so it always reaches a fixpoint),
//! followed by exactly one validation sweep against the now-stable registry
//! that reports each violation once and records the
//! [`Adornment`] every literal fired with.
//!
//! Checking is left to right over the body *as written* -- no reordering, no
//! search (§2.7). A clause's own required-bound set is discovered by
//! *assuming* its head variables are supplied exactly where the current
//! hypothesis says they are, simulating the body, and promoting any head
//! variable a literal needed early into the hypothesis. A body-local
//! variable that is still unbound when needed can never be promoted -- there
//! is no caller to supply it -- so it surfaces as a hard error in the
//! validation sweep, at the call site that needed it. This is exactly what
//! makes the doc's original `introduced(B, C) :- tree_entry(C, P, B), ...`
//! fail: growth happily lets `introduced` require `C` bound (`C` is a head
//! variable), and the error appears wherever `introduced` is *called* with
//! `C` free -- which is everywhere, since nothing in the program ever binds
//! a commit that way.

use std::collections::{BTreeMap, BTreeSet};

use gix_query_ir::{
    Adornment, ArgSet, Goal, Literal, PredicateClass, Program, Registry, Rule, RuleAdornments,
    RuleId, Term, Var,
};

use crate::error::{CheckError, Errors, Site};

/// Infer every predicate's required-bound set, then validate every call site
/// against the stable result, returning one [`RuleAdornments`] per rule.
pub(crate) fn check(
    program: &Program,
    registry: &mut Registry,
    errors: &mut Errors,
) -> BTreeMap<RuleId, RuleAdornments> {
    init(program, registry);
    let bound: usize =
        registry.predicates().map(|e| e.key.arity).sum::<usize>() + program.rules().len() + 16;
    for _ in 0..bound {
        if !growth_sweep(program, registry) {
            break;
        }
    }
    validate(program, registry, errors)
}

/// Mode-check a standalone goal against an already-inferred registry. A goal
/// has no caller and defines nothing, so there is no growth phase for it --
/// its body starts with nothing bound, full stop, and every requirement it
/// cannot satisfy is an immediate error.
pub(crate) fn check_goal(
    goal: &Goal,
    registry: &Registry,
    errors: &mut Errors,
) -> gix_query_ir::GoalAdornments {
    let mut bound_vars: BTreeSet<Var> = BTreeSet::new();
    let mut body = Vec::with_capacity(goal.body.len());
    for lit in &goal.body {
        let cb = call_bound(lit, &bound_vars);
        body.push(Adornment::new(cb, lit.key().arity));
        check_literal_mode(lit, cb, registry, Site::Goal, errors);
        bind_generator(lit, &mut bound_vars);
    }
    gix_query_ir::GoalAdornments { body }
}

/// Every `Idb` predicate starts with `required_bound = Some(empty)` rather
/// than `None`, so the rest of this pass can read it without special-casing
/// "inference has not run yet" -- that state exists only before this
/// function runs, never during the fixpoint.
fn init(program: &Program, registry: &mut Registry) {
    for key in program.defined() {
        if let Some(entry) = registry.predicate_mut(key)
            && let PredicateClass::Idb { required_bound } = &mut entry.class
            && required_bound.is_none()
        {
            *required_bound = Some(ArgSet::empty());
        }
    }
}

fn required_bound_of(key: &gix_query_ir::PredicateKey, registry: &Registry) -> ArgSet {
    registry
        .predicate(key)
        .and_then(gix_query_ir::PredicateEntry::always_required_bound)
        .unwrap_or_default()
}

fn head_vars_at(head: &gix_query_ir::Atom, positions: &ArgSet) -> BTreeSet<Var> {
    head.terms
        .iter()
        .enumerate()
        .filter(|(i, _)| positions.contains(*i))
        .filter_map(|(_, t)| t.term.as_var())
        .cloned()
        .collect()
}

fn is_head_var(head: &gix_query_ir::Atom, v: &Var) -> bool {
    head.terms.iter().any(|t| t.term.as_var() == Some(v))
}

fn term_is_bound(t: &Term, bound_vars: &BTreeSet<Var>) -> bool {
    match t {
        Term::Const(_) => true,
        Term::Var(v) => bound_vars.contains(v),
        Term::Anon => false,
    }
}

fn call_bound(lit: &Literal, bound_vars: &BTreeSet<Var>) -> ArgSet {
    let mut s = ArgSet::empty();
    for (i, t) in lit.terms().enumerate() {
        if term_is_bound(t, bound_vars) {
            s.insert(i);
        }
    }
    s
}

fn bind_generator(lit: &Literal, bound_vars: &mut BTreeSet<Var>) {
    if !lit.is_generator() {
        return;
    }
    for t in lit.terms() {
        if let Term::Var(v) = t {
            bound_vars.insert(v.clone());
        }
    }
}

/// The requirement a non-EDB literal imposes: full grounding for negation
/// and comparison, the declared vector for a builtin, the live
/// (possibly still-growing) hypothesis for an IDB call. EDB calls are
/// handled by their caller, since a mode *set* needs to be selected against
/// `call_bound` rather than read as one fixed vector.
fn fixed_or_idb_required(lit: &Literal, registry: &Registry) -> ArgSet {
    match lit {
        Literal::Negated(a) => ArgSet::full(a.terms.len()),
        Literal::Compare(_) => ArgSet::full(2),
        Literal::Builtin(a) => registry
            .builtin(&a.predicate)
            .map(|b| b.modes.required_bound())
            .unwrap_or_default(),
        Literal::Positive(a) => required_bound_of(&a.predicate, registry),
    }
}

fn is_edb_positive<'r>(
    lit: &'r Literal,
    registry: &'r Registry,
) -> Option<(&'r gix_query_ir::PredicateKey, &'r gix_query_ir::ModeSet)> {
    let Literal::Positive(a) = lit else {
        return None;
    };
    let entry = registry.predicate(&a.predicate)?;
    let PredicateClass::Edb { modes, .. } = &entry.class else {
        return None;
    };
    Some((&a.predicate, modes))
}

/// One silent pass over every rule, growing required-bound sets. Returns
/// whether anything changed, so the caller knows when the fixpoint is
/// reached.
fn growth_sweep(program: &Program, registry: &mut Registry) -> bool {
    let mut changed = false;
    for rule in program.rules() {
        growth_over_rule(rule, registry, &mut changed);
    }
    changed
}

fn growth_over_rule(rule: &Rule, registry: &mut Registry, changed: &mut bool) {
    let head = &rule.head;
    if registry.predicate(&head.predicate).is_none() {
        return;
    }
    let req = required_bound_of(&head.predicate, registry);
    let mut bound_vars = head_vars_at(head, &req);
    for lit in &rule.body {
        let cb = call_bound(lit, &bound_vars);
        let required = if let Some((_, modes)) = is_edb_positive(lit, registry) {
            // EDB modes are declared, not grown -- but promoting a head
            // variable that *every* supported vector needs is always a safe
            // step toward satisfying one of them, which is what makes
            // `commit(Rev)` inside `reach(Rev, Rev) :- commit(Rev).` promote
            // `Rev` instead of stalling. The validation sweep re-checks with
            // the real, exact `ModeSet::select`.
            modes.always_required()
        } else {
            fixed_or_idb_required(lit, registry)
        };
        for pos in required.iter().filter(|i| !cb.contains(*i)) {
            if let Some(Term::Var(v)) = lit.terms().nth(pos) {
                if is_head_var(head, v) && grow(head, v, registry) {
                    *changed = true;
                }
                bound_vars.insert(v.clone());
            }
        }
        bind_generator(lit, &mut bound_vars);
    }
}

/// Union `v`'s *first* head position into `head.predicate`'s required-bound
/// set. Only the first: a repeated head variable such as
/// `reach(Rev, Rev) :- commit(Rev).` needs exactly one of its two
/// occurrences supplied to know the value, and promoting both would demand
/// `reach/2` be called `+Rev, +Rev` where the spec's own table says `+Rev`
/// alone -- our flat `ArgSet` mode model has no way to say "these positions
/// must agree", only "this position must be bound", so the leftmost
/// occurrence is the one occurrence that stands for the variable.
fn grow(head: &gix_query_ir::Atom, v: &Var, registry: &mut Registry) -> bool {
    let Some(first) = head.terms.iter().position(|t| t.term.as_var() == Some(v)) else {
        return false;
    };
    let mut new_bits = ArgSet::empty();
    new_bits.insert(first);
    let Some(entry) = registry.predicate_mut(&head.predicate) else {
        return false;
    };
    let PredicateClass::Idb { required_bound } = &mut entry.class else {
        return false;
    };
    let before = required_bound.unwrap_or_default();
    let after = before.union(&new_bits);
    if after == before {
        return false;
    }
    *required_bound = Some(after);
    true
}

fn validate(
    program: &Program,
    registry: &Registry,
    errors: &mut Errors,
) -> BTreeMap<RuleId, RuleAdornments> {
    let mut out = BTreeMap::new();
    for (i, rule) in program.rules().iter().enumerate() {
        let id = RuleId(i);
        let site = Site::Rule(id);
        let head = &rule.head;
        let req = required_bound_of(&head.predicate, registry);
        let mut bound_vars = head_vars_at(head, &req);
        let mut body = Vec::with_capacity(rule.body.len());
        for lit in &rule.body {
            let cb = call_bound(lit, &bound_vars);
            body.push(Adornment::new(cb, lit.key().arity));
            check_literal_mode(lit, cb, registry, site, errors);
            bind_generator(lit, &mut bound_vars);
        }
        let head_adornment = Adornment::new(req, head.terms.len());
        out.insert(
            id,
            RuleAdornments {
                rule: id,
                head: head_adornment,
                body,
            },
        );
    }
    out
}

fn check_literal_mode(
    lit: &Literal,
    cb: ArgSet,
    registry: &Registry,
    site: Site,
    errors: &mut Errors,
) {
    if let Some((key, modes)) = is_edb_positive(lit, registry) {
        if modes.select(&cb).is_none() {
            let Literal::Positive(atom) = lit else {
                unreachable!("is_edb_positive only matches Positive")
            };
            errors.push(CheckError::UnsupportedEdbMode {
                predicate: key.clone(),
                bound: cb.iter().collect(),
                site,
                span: atom.span,
            });
        }
        return;
    }
    let required = fixed_or_idb_required(lit, registry);
    let terms: Vec<&Term> = lit.terms().collect();
    for pos in required.iter().filter(|i| !cb.contains(*i)) {
        let var = terms.get(pos).and_then(|t| t.as_var()).cloned();
        errors.push(CheckError::ModeViolation {
            predicate: lit.key().clone(),
            argument: pos,
            var,
            site,
            span: lit.span(),
        });
    }
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, PredicateKey};

    use super::*;
    use crate::error::Pass;

    fn commit_walk() -> Program {
        // reach(Rev, Rev) :- commit(Rev).
        // reach(Rev, C)   :- reach(Rev, X), parent(X, C).
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("reach", vec![Term::var("Rev"), Term::var("Rev")]),
                vec![Literal::Positive(Atom::new(
                    "commit",
                    vec![Term::var("Rev")],
                ))],
            ),
            "m",
        );
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("reach", vec![Term::var("Rev"), Term::var("C")]),
                vec![
                    Literal::Positive(Atom::new("reach", vec![Term::var("Rev"), Term::var("X")])),
                    Literal::Positive(Atom::new("parent", vec![Term::var("X"), Term::var("C")])),
                ],
            ),
            "m",
        );
        program
    }

    #[test]
    fn reach_infers_rev_required_and_validates() {
        let program = commit_walk();
        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        crate::pass3_types::check(&program, &mut registry, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let adornments = check(&program, &mut registry, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(adornments.len(), 2);
        let req = required_bound_of(&PredicateKey::new("reach", 2), &registry);
        assert!(req.contains(0), "Rev must be required bound");
        assert!(!req.contains(1), "C stays free");
    }

    #[test]
    fn calling_reach_with_rev_free_is_a_mode_violation() {
        let mut program = commit_walk();
        // bad(C) :- reach(R, C). -- R is never bound.
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("bad", vec![Term::var("C")]),
                vec![Literal::Positive(Atom::new(
                    "reach",
                    vec![Term::var("R"), Term::var("C")],
                ))],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        crate::pass3_types::check(&program, &mut registry, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        check(&program, &mut registry, &mut errors);
        assert!(errors.iter().any(|e| e.pass() == Pass::Modes
            && matches!(e, CheckError::ModeViolation { var: Some(v), .. } if v.as_str() == "R")));
    }

    #[test]
    fn the_docs_original_introduced_is_a_mode_violation_naming_c() {
        // introduced(B, C) :- tree_entry(C, P, B), ...
        // tree_entry is +Rev, so calling it with C free is the bug.
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("introduced", vec![Term::var("B"), Term::var("C")]),
                vec![Literal::Positive(Atom::new(
                    "tree_entry",
                    vec![Term::var("C"), Term::var("P"), Term::var("B")],
                ))],
            ),
            "m",
        );
        // authored(B, M) :- introduced(B, C), author(C, M).
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("authored", vec![Term::var("B"), Term::var("M")]),
                vec![
                    Literal::Positive(Atom::new(
                        "introduced",
                        vec![Term::var("B"), Term::var("C")],
                    )),
                    Literal::Positive(Atom::new("author", vec![Term::var("C"), Term::var("M")])),
                ],
            ),
            "m",
        );
        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        crate::pass3_types::check(&program, &mut registry, &mut errors);
        check(&program, &mut registry, &mut errors);
        let hit = errors.iter().find(|e| {
            e.pass() == Pass::Modes
                && matches!(e, CheckError::ModeViolation { var: Some(v), predicate, .. }
                    if v.as_str() == "C" && predicate.name.as_ref() == "introduced")
        });
        assert!(
            hit.is_some(),
            "expected a mode violation naming C, got {errors:?}"
        );
    }
}
