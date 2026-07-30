//! The nine validation passes, and the sole producer of
//! [`gix_query_ir::CheckedProgram`].
//!
//! # Passes, in the order [`check`] runs them
//!
//! 1. No value invention -- structurally impossible for a compound term
//!    ([`gix_query_ir::Term`] has no fourth form), so what remains is
//!    catching a constant that carries a byte Nemo's lowering cannot.
//! 2. Predicate resolution -- every literal names a host predicate, a
//!    builtin, or a program-defined one; private predicates stay inside
//!    their own module.
//! 3. Arity and types -- arity is folded into pass 2 (a
//!    [`gix_query_ir::PredicateKey`] carries its own arity, so a mismatch is
//!    a more precise "unresolved"); this pass is the type side, including
//!    inferring the `Infer` slots a fact-only predicate such as `carries/2`
//!    leaves for its call sites to fill in.
//! 4. Range restriction -- every head, negated, and comparison variable
//!    traces to a positive body literal, checked without regard to order.
//! 5. Stratification -- an SCC decomposition of the predicate dependency
//!    graph, rejecting a negative edge that closes a cycle and producing the
//!    evaluation order.
//! 6. Shape caps -- literal, variable, and rule counts, plus a literal that
//!    joins nothing.
//! 7. Footprint -- the transitive base predicates and builtins each
//!    predicate reaches, over pass 5's decomposition.
//! 8. Mode inference -- left to right, author order, no search: a least
//!    fixpoint that infers every predicate's required-bound set and then
//!    validates every call site against it.
//! 9. Minting confinement -- no builtin declared `mints` inside a recursive
//!    stratum, over the same decomposition pass 5 and pass 7 already used.
//!
//! Plus, after the nine: effect stratification -- no cycle in the graph an
//! effect's declared write set induces over another effect's *computed* read
//! set, the latter being pass 7's footprint mapped to ref namespaces
//! (`ARCHITECTURE.md`'s git-effect section calls this fork-bomb freedom, and
//! `docs/query-and-rules.adoc`'s "Triggers" section specifies the footprint
//! as the graph's input). Not numbered among the nine because it validates
//! [`gix_query_ir::EffectDecl`]s, a declaration form the rule body language
//! does not produce. Its pieces -- [`check_acyclicity`], [`read_namespaces`],
//! [`footprint_of_body`] -- are public so that `gix-query`'s facade can offer
//! the same check over effect docs at define time without a second graph.
//!
//! # What this crate does not do
//!
//! It has no git dependency and never will: pass 7's footprint is
//! parameterized over the registry's own backing-source data, never a
//! repository. It does not run predicate reclassification
//! (`gix_query_parse::resolve_module`) -- a [`Literal::Positive`] naming a
//! registered builtin is still accepted, so callers that already resolved
//! their program (as `gix-query-parse`'s own documentation recommends) pay
//! nothing extra, and callers that did not still get a correct answer.
//!
//! [`Literal::Positive`]: gix_query_ir::Literal::Positive

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod graph;
mod pass10_effect_stratify;
mod pass1_symbols;
mod pass2_resolve;
mod pass3_types;
mod pass4_range;
mod pass5_stratify;
mod pass6_caps;
mod pass7_footprint;
mod pass8_modes;
mod pass9_mint;
mod registry_build;
#[cfg(test)]
mod testutil;

pub use error::{CheckError, Errors, Pass, Site};
pub use pass10_effect_stratify::{EffectReads, ReadNamespaces, check_acyclicity, read_namespaces};

use gix_query_ir::{CheckedProgram, Footprint, Goal, GoalAdornments, Literal, Program, Registry};

/// Run all nine validation passes, plus effect stratification, over
/// `program`, extending `registry` with one IDB entry per predicate the
/// program defines.
///
/// This is the only way to construct a [`CheckedProgram`]: every downstream
/// crate that needs a validated program takes one, never a `Program` plus a
/// promise that someone, somewhere, already checked it.
///
/// `registry` is typically [`Registry::host`] -- the host EDB and builtins,
/// nothing program-defined yet. Passing an already-extended registry is
/// harmless; predicates it already carries are treated as pre-declared host
/// data, exactly like `commit/1`.
///
/// On success every problem the nine passes found is empty by definition;
/// on failure, every problem any pass found is returned together, so one run
/// reports everything rather than the first thing.
pub fn check(program: &Program, registry: &Registry) -> Result<CheckedProgram, Errors> {
    let mut errors = Errors::new();

    pass1_symbols::check(program, &mut errors);

    let mut working = registry_build::build(program, registry, &mut errors);

    pass2_resolve::check(program, &working, &mut errors);
    pass3_types::check(program, &mut working, &mut errors);
    pass4_range::check(program, &mut errors);
    let stratify = pass5_stratify::check(program, &working, &mut errors);
    pass6_caps::check(program, &mut errors);
    let footprints = pass7_footprint::compute(program, &working, &stratify);
    let adornments = pass8_modes::check(program, &mut working, &mut errors);
    pass9_mint::check(program, &working, &stratify, &mut errors);
    pass10_effect_stratify::check(program, &working, &footprints, &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(CheckedProgram::assemble(
        program.clone(),
        working,
        stratify.stratification,
        adornments,
        footprints,
    ))
}

/// The footprint of a bare body -- a goal's, or an effect's `when`/`gate`
/// clause -- against an already-[`check`]ed program.
///
/// Pass 7 keys its footprints by the predicate a rule *defines*, and a body
/// defines nothing, so a consumer that has a body rather than a predicate
/// name needs this instead of [`CheckedProgram::footprint`]. Composed with
/// [`read_namespaces`] it is how an effect's read set is computed from its
/// trigger body, which is the input [`check_acyclicity`] wants.
///
/// `body` should already have been resolved against `checked`'s registry
/// (`gix_query_parse::resolve_body`) and validated by [`check_goal`]; an
/// unresolved literal contributes nothing rather than erroring, because
/// reporting it is pass 2's job.
#[must_use]
pub fn footprint_of_body(body: &[Literal], checked: &CheckedProgram) -> Footprint {
    pass7_footprint::compute_body(body, checked.registry(), checked.all_footprints())
}

/// Check a standalone goal -- a tier-1, tier-2, or tier-3 query -- against an
/// already-[`check`]ed program.
///
/// Deliberately narrower than [`check`]: a goal has no head to infer a
/// required-bound set *for*, so there is no growth phase and no footprint
/// (nothing in [`CheckedProgram::footprints`] is keyed by "the goal", since
/// it defines no predicate). What a goal needs before it is safe to hand to
/// the evaluator is exactly passes 1, 2, 4, and 8 (mode checking still runs
/// left to right, still starting from nothing bound, since a goal has no
/// caller either) plus the type checking half of pass 3. Shape caps (pass 6)
/// and stratification/footprint/minting (passes 5, 7, 9) are properties of
/// the *stored* program a goal merely queries, not of the goal itself.
///
/// Takes a [`CheckedProgram`] rather than a bare [`Registry`], so a goal can
/// only ever be checked against a program whose own modes and types have
/// already been inferred -- there is no way to call this before [`check`]
/// has produced something to check against.
pub fn check_goal(goal: &Goal, checked: &CheckedProgram) -> Result<GoalAdornments, Errors> {
    let mut errors = Errors::new();

    pass1_symbols::check_goal_body(&goal.body, &mut errors);
    pass2_resolve::check_goal_body(
        &goal.body,
        checked.program(),
        checked.registry(),
        &mut errors,
    );
    // A scratch clone: type inference may still resolve a leftover `Infer`
    // slot from this goal's call sites, but that inference must never leak
    // into the shared, already-frozen `CheckedProgram`.
    let mut scratch = checked.registry().clone();
    pass3_types::check_goal_body(&goal.body, &mut scratch, &mut errors);
    pass4_range::check_goal(goal, &mut errors);
    let adornments = pass8_modes::check_goal(goal, checked.registry(), &mut errors);

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(adornments)
}
