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
        check_literal_mode(lit, cb, registry, Site::Goal, errors);
        let recorded = body_adornment(lit, cb, registry);
        body.push(Adornment::new(recorded, lit.key().arity));
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
    if let Some(b) = builtin_of(lit, registry) {
        return b.modes.required_bound();
    }
    match lit {
        Literal::Negated(a) => ArgSet::full(a.terms.len()),
        Literal::Compare(_) => ArgSet::full(2),
        Literal::Builtin(_) => ArgSet::default(),
        Literal::Positive(a) => required_bound_of(&a.predicate, registry),
    }
}

/// The builtin a literal calls, whichever syntactic form it arrived in.
///
/// A parser that has run name resolution produces [`Literal::Builtin`], but a
/// caller may hand us a module that has not, and then `bind(...)` is a
/// [`Literal::Positive`] naming a key the predicate map has never heard of.
/// Reading only the `Builtin` form let such a literal past mode checking and
/// past minting confinement with an empty requirement -- accepted, and
/// accepted *differently* from the identical resolved module. The registry is
/// right here, so ask it.
fn builtin_of<'r>(lit: &Literal, registry: &'r Registry) -> Option<&'r gix_query_ir::BuiltinEntry> {
    match lit {
        Literal::Builtin(a) | Literal::Positive(a) => registry.builtin(&a.predicate),
        Literal::Negated(_) | Literal::Compare(_) => None,
    }
}

/// What a body literal's adornment records.
///
/// For everything except a derived call this is `cb`, the set of arguments
/// bound on entry -- which is what an EDB provider is handed and what the
/// demand loop reports a builtin was called in.
///
/// A derived call is different, and getting it wrong is silent. The magic-set
/// rewrite guards each rule for `p` on `p`'s *head* adornment, which is `p`'s
/// inferred required-bound set, and it keys the demand it emits at a call
/// site on the *body* adornment. The rewrite deliberately does not rename
/// predicates, so `p` has exactly one guard. If a call site binds more than
/// `p` requires and we recorded the full set, the rewrite would emit
/// `magic_p_bbf` while the guard consumes `magic_p_bff`: demand produced and
/// never consumed, so `p` derives nothing, the rule above it never fires, and
/// the query answers zero rows with no diagnostic anywhere. On the reference
/// vocabulary that is a self-approved blob failing to block a merge.
///
/// So a derived call records the callee's requirement, not the caller's
/// generosity. Mode *checking* still uses `cb`, which is why passing the
/// check is what makes this narrowing safe: a successful check has already
/// established that the requirement is a subset of what is bound.
fn body_adornment(lit: &Literal, cb: ArgSet, registry: &Registry) -> ArgSet {
    let Literal::Positive(a) = lit else {
        return cb;
    };
    if builtin_of(lit, registry).is_some() {
        return cb;
    }
    match registry.predicate(&a.predicate).map(|e| &e.class) {
        Some(PredicateClass::Idb { .. }) => required_bound_of(&a.predicate, registry),
        Some(PredicateClass::Edb { .. }) | None => cb,
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
            check_literal_mode(lit, cb, registry, site, errors);
            let recorded = body_adornment(lit, cb, registry);
            body.push(Adornment::new(recorded, lit.key().arity));
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

    /// A call site that binds more than the callee requires must still record
    /// the callee's requirement, because that is the single guard the
    /// magic-set rewrite generates for it. Recording the caller's generosity
    /// instead produces demand under one adornment and consumes it under
    /// another, and the predicate silently derives nothing.
    #[test]
    fn a_derived_call_records_the_callee_requirement_not_the_call_site() {
        let mut program = commit_walk();
        // ancestor(R, C) :- reach(R, C).            -- requires R
        // both(R, C)     :- reach(R, C), ancestor(R, C).  -- binds R and C
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("ancestor", vec![Term::var("R"), Term::var("C")]),
                vec![Literal::Positive(Atom::new(
                    "reach",
                    vec![Term::var("R"), Term::var("C")],
                ))],
            ),
            "m",
        );
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("both", vec![Term::var("R"), Term::var("C")]),
                vec![
                    Literal::Positive(Atom::new("reach", vec![Term::var("R"), Term::var("C")])),
                    Literal::Positive(Atom::new("ancestor", vec![Term::var("R"), Term::var("C")])),
                ],
            ),
            "m",
        );

        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        crate::pass3_types::check(&program, &mut registry, &mut errors);
        let adornments = check(&program, &mut registry, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let ancestor_required = required_bound_of(&PredicateKey::new("ancestor", 2), &registry);
        let both = adornments
            .values()
            .find(|a| program.rules()[a.rule.0].head.predicate.name.as_ref() == "both")
            .expect("both/2 has a rule");
        let ancestor_call = &both.body[1];
        assert_eq!(
            ancestor_call.bound, ancestor_required,
            "the call site binds R and C; ancestor only requires R"
        );
        assert!(ancestor_required.contains(0) && !ancestor_required.contains(1));
    }

    /// A module that has not been through name resolution carries `bind(...)`
    /// as a positive literal. It must be checked identically, or the same
    /// text is accepted or rejected depending on which entry point the caller
    /// used.
    #[test]
    fn a_builtin_written_as_a_positive_literal_is_still_mode_checked() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::new(
                Atom::new("wrap", vec![Term::var("B"), Term::var("S"), Term::var("E")]),
                vec![Literal::Positive(Atom::new(
                    "bind",
                    vec![
                        Term::var("A"),
                        Term::var("Rev"),
                        Term::var("B"),
                        Term::var("S"),
                        Term::var("E"),
                        Term::var("Po"),
                        Term::var("Co"),
                    ],
                ))],
            ),
            "m",
        );

        let mut errors = Errors::new();
        let mut registry = crate::registry_build::build(&program, &Registry::host(), &mut errors);
        crate::pass3_types::check(&program, &mut registry, &mut errors);
        errors.clear();
        check(&program, &mut registry, &mut errors);

        assert!(
            !errors.is_empty(),
            "the unbound anchor and rev went unnoticed"
        );
        assert!(errors.iter().all(|e| e.pass() == Pass::Modes), "{errors:?}");
        let rendered = errors.iter().map(ToString::to_string).collect::<String>();
        assert!(
            rendered.contains('A') && rendered.contains("Rev"),
            "{rendered}"
        );
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
