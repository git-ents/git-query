//! A Datalog query and rules layer over git.
//!
//! [`checked_program`] assembles the whole program -- every rule module
//! [`gix_query_rules::RuleStore`] currently publishes, parsed, qualified, and
//! validated together -- and [`run_goal`] is the facade for tier 3 against
//! it: parse a goal, resolve it against the host registry, validate it,
//! magic-set rewrite it, lower it to Nemo, audit the emission, and run it to
//! a demand fixpoint against a real repository.
//!
//! [`check_effect_acyclicity`] is the other half of the facade's charter
//! (`DEVPLAN.md` §2.6: "git-query exports the footprint and the acyclicity
//! check; it does not enforce anything"): the fork-bomb check over a set of
//! registered effects, computing each one's read set from its own body rather
//! than trusting a declaration.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::sync::Arc;

use gix_query_host::{AllBuiltins, RepoFacts};
use gix_query_ir::{CheckedProgram, Registry};

pub use gix_query_eval::{DerivationTree, NegationWitness};
pub use gix_query_ir::{EffectDecl, Footprint, Namespace, Registry as HostRegistry, Value};
pub use gix_query_rules::RuleStore;
pub use gix_refstore::RefName;

/// Join a list of diagnostics into one message, one per line.
fn join<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything that can go wrong running a goal.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The goal failed to parse.
    #[error("goal did not parse:\n{0}")]
    Parse(String),
    /// Loading or assembling the stored rule modules failed -- storage, a
    /// module's own syntax, or the whole program's own validation.
    #[error(transparent)]
    Rules(#[from] gix_query_rules::Error),
    /// The goal itself failed validation against the checked program.
    #[error("goal failed validation:\n{0}")]
    CheckGoal(String),
    /// Tier 1 named a predicate the checked program does not declare.
    #[error("no predicate named `{0}`")]
    UnknownPredicate(String),
    /// Tier 1 bound a position outside the predicate's declared arity.
    #[error("position {position} is outside `{predicate}`'s arity {arity}")]
    BindOutOfRange {
        /// The predicate named.
        predicate: String,
        /// The position given.
        position: usize,
        /// The predicate's actual arity.
        arity: usize,
    },
    /// [`explain_predicate`] needs one fully ground tuple to trace; `bound`
    /// covered fewer positions than `predicate`'s arity.
    #[error(
        "`explain` needs every position of `{predicate}` bound (arity {arity}), but only \
         {bound} were; explain has no free-position case the way `run` does"
    )]
    ExplainRequiresGroundTuple {
        /// The predicate named.
        predicate: String,
        /// Its declared arity.
        arity: usize,
        /// How many positions `--bind` covered.
        bound: usize,
    },
    /// Lowering the rewritten program to Nemo, or auditing that emission,
    /// failed.
    #[error(transparent)]
    Eval(#[from] gix_query_eval::EvalError),
}

/// One effect, as [`check_effect_acyclicity`] needs to see it: a declaration
/// carrying the *declared write set*, plus the rule-language source of every
/// body whose footprint makes up its *computed read set*.
///
/// `bodies` is a list because an effect doc has more than one
/// (`DEVPLAN-effect.md` D1: `when` and `gate`) and both are read at trigger
/// and admission time, so both belong in the read set. Each entry is goal-body
/// text -- the same language a rule body is written in, parsed by
/// `gix-query-parse` and validated by the same passes as any module, which is
/// the entire reason effects share this repo.
///
/// [`EffectDecl`] carries no read set to consult: `DEVPLAN-effect.md` D2
/// retired it, and the parser clause behind it, because a declared read set
/// can lie while a footprint cannot.
#[derive(Debug, Clone)]
pub struct EffectUnderCheck {
    /// The effect's name and declared write set.
    pub decl: EffectDecl,
    /// Rule-language source for each body the effect reads through -- an
    /// effect doc's `when` and `gate`.
    pub bodies: Vec<String>,
}

/// Why a set of effects failed [`check_effect_acyclicity`].
///
/// A cycle names the namespace, because "you have a fork bomb" is not
/// actionable and "effect `a` writes `refs/claims/*`, which effect `b`
/// reads" is.
#[derive(Debug, thiserror::Error)]
pub enum EffectCheckError {
    /// One of an effect's bodies did not parse.
    #[error("effect `{effect}`'s body did not parse:\n{diagnostics}")]
    Parse {
        /// The effect whose body it was.
        effect: String,
        /// The parser's diagnostics, one per line.
        diagnostics: String,
    },
    /// One of an effect's bodies parsed but failed validation against the
    /// checked program -- an unresolved predicate, a mode violation, anything
    /// [`gix_query_check::check_goal`] rejects. A body that does not validate
    /// has no trustworthy footprint, so it is reported rather than silently
    /// contributing an empty read set.
    #[error("effect `{effect}`'s body failed validation:\n{diagnostics}")]
    Body {
        /// The effect whose body it was.
        effect: String,
        /// The validator's diagnostics, one per line.
        diagnostics: String,
    },
    /// The fork bomb: following declared writes into computed reads leads back
    /// to an effect already on the path.
    #[error(
        "effect `{from}` writes `{namespace}`, which effect `{to}`'s body reads, closing a \
         cycle in the effect graph"
    )]
    Cycle {
        /// The effect whose write closes the cycle.
        from: Arc<str>,
        /// The effect whose computed read set the write overlaps.
        to: Arc<str>,
        /// The namespace the two share -- the thing to change.
        namespace: Namespace,
    },
}

/// The fork-bomb check over a set of registered effects: reject any cycle in
/// the graph whose edges run from one effect's *declared* write set into
/// another's *computed* read set.
///
/// This is the export `DEVPLAN-effect.md` D9 assigns to this crate and
/// `gix-effect` calls at `git effect define` time, over every effect the
/// repository has registered plus the one being defined: rejecting a cycle
/// when the doc is written is the earliest possible boundary, and it means
/// `gix-effect` reimplements neither the graph, nor the DFS marks, nor
/// [`Namespace`] overlap.
///
/// Each effect's read set is computed, not declared: every body in
/// [`EffectUnderCheck::bodies`] is parsed, resolved and validated against
/// `checked`, its footprint taken, and that footprint mapped to ref
/// namespaces through the registry's [`gix_query_ir::Backing::RefGlob`]
/// declarations. Backings that denote no ref namespace -- content-addressed
/// and ad-hoc predicates -- contribute no edge, deliberately: neither is a ref
/// an effect's write set can advance. `gix_query_check::ReadNamespaces`
/// records which predicates those were, and a caller that wants to show them
/// can recompute with `gix_query_check::read_namespaces`.
///
/// The check is **sound and incomplete**, per `docs/query-and-rules.adoc`: the
/// predicate-level dependency graph is coarser than a glob-intersection test,
/// so it can deny a program that would in fact have terminated, and it can
/// never admit a cycle. A spurious *denial* is debuggable; the reverse would
/// be a fork bomb.
///
/// # Errors
/// Every problem found, not just the first: one [`EffectCheckError`] per
/// unparsable body, per invalid body, and per cycle-closing edge.
pub fn check_effect_acyclicity(
    checked: &CheckedProgram,
    effects: &[EffectUnderCheck],
) -> Result<(), Vec<EffectCheckError>> {
    let mut errors: Vec<EffectCheckError> = Vec::new();
    let mut graph_input: Vec<gix_query_check::EffectReads> = Vec::with_capacity(effects.len());

    for effect in effects {
        let mut footprint = Footprint::default();
        for source in &effect.bodies {
            match body_footprint(checked, &effect.decl.name, source) {
                Ok(fp) => footprint.absorb(&fp),
                Err(e) => errors.push(e),
            }
        }
        graph_input.push(gix_query_check::EffectReads {
            decl: effect.decl.clone(),
            reads: gix_query_check::read_namespaces(&footprint, checked.registry()).namespaces,
        });
    }

    errors.extend(
        gix_query_check::check_acyclicity(&graph_input)
            .into_iter()
            .map(|e| match e {
                gix_query_check::CheckError::EffectCycle {
                    from,
                    to,
                    namespace,
                    ..
                } => EffectCheckError::Cycle {
                    from,
                    to,
                    namespace,
                },
                // `check_acyclicity` produces exactly one variant; anything
                // else would be a diagnostic about the rule program, which
                // this entry point was not given and must not invent.
                other => EffectCheckError::Body {
                    effect: String::new(),
                    diagnostics: other.to_string(),
                },
            }),
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Ref namespaces one rule-language body reads through, via its computed
/// footprint.
///
/// This is the same footprint-to-namespace projection
/// [`check_effect_acyclicity`] uses internally over an effect's `when`+`gate`
/// bodies together, exposed here for a single body in isolation. `gix-effect`
/// needs exactly this for two things `check_effect_acyclicity` does not
/// answer on its own: D3's trigger selection ("effects whose `when` footprint
/// overlaps the advanced ref") and D3's authoritative-refs-only validation --
/// both ask what one body (`when` alone) reads, not what an effect's `when`
/// and `gate` read combined. Exporting this rather than letting `gix-effect`
/// reimplement it is D9's boundary: "stratification / acyclicity check ->
/// gix-query -> calls at define time" -- the footprint projection is part of
/// that machinery, not effect's to own.
///
/// # Errors
/// [`EffectCheckError::Parse`] if `source` does not parse;
/// [`EffectCheckError::Body`] if it fails validation against `checked`.
pub fn body_read_namespaces(
    checked: &CheckedProgram,
    source: &str,
) -> Result<Vec<Namespace>, EffectCheckError> {
    let footprint = body_footprint(checked, "", source)?;
    Ok(gix_query_check::read_namespaces(&footprint, checked.registry()).namespaces)
}

/// Parse, resolve, validate and take the footprint of one effect body.
fn body_footprint(
    checked: &CheckedProgram,
    effect: &str,
    source: &str,
) -> Result<Footprint, EffectCheckError> {
    let mut goal = gix_query_parse::parse_goal(source).map_err(|e| EffectCheckError::Parse {
        effect: effect.to_owned(),
        diagnostics: join(&e),
    })?;
    gix_query_parse::resolve_goal(&mut goal, checked.registry());
    gix_query_check::check_goal(&goal, checked).map_err(|e| EffectCheckError::Body {
        effect: effect.to_owned(),
        diagnostics: join(&e),
    })?;
    Ok(gix_query_check::footprint_of_body(&goal.body, checked))
}

/// Assemble the whole program: every rule module currently published at
/// `refs/meta/rules/*` in `repo`, parsed, qualified into one namespace, and
/// checked together against the host registry.
///
/// A repository with no rule modules produces an empty, trivially valid
/// program, so a goal can still call bare host predicates and builtins with
/// no rules published at all.
///
/// # Errors
/// See [`gix_query_rules::Error`].
pub fn checked_program(repo: &gix::Repository) -> Result<CheckedProgram, QueryError> {
    let registry = Registry::host();
    let store = RuleStore::open(repo)?;
    Ok(gix_query_rules::assemble(&store, &registry)?)
}

/// Parse `goal_text`, projecting `select`, and run it against `repo`.
///
/// Assembles [`checked_program`] first, so a goal may call any `pub`
/// predicate any currently published rule module defines, in addition to
/// the host EDB relations and builtins `gix_query_ir::Registry::host`
/// declares. Rows come back in `select`'s column order, deduplicated --
/// Datalog's answer relation is a set, and the demand loop's final round
/// already dedups internally, but a caller composing several partial runs
/// should not have to re-derive that guarantee.
///
/// # Errors
/// See [`QueryError`].
pub fn run_goal(
    repo: &gix::Repository,
    goal_text: &str,
    select: &[&str],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let checked = checked_program(repo)?;

    let mut goal = gix_query_parse::parse_goal_with_select(goal_text, select)
        .map_err(|e| QueryError::Parse(join(&e)))?;
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    let facts = RepoFacts::new(repo);
    run_checked_goal(repo, &facts, &checked, &goal)
}

/// Run a rule-language body to a demand fixpoint against `repo`, selecting
/// every variable that occurs in it, in first-occurrence order -- the same
/// default [`gix_query_parse::parse_goal`] gives tier 1's desugaring.
///
/// This is [`run_goal`] without an explicit `--select`, which is what a
/// caller re-evaluating an effect's `when`/`gate` body wants (`gix-effect`'s
/// trigger detection, `DEVPLAN-effect.md` D3): the whole point is comparing
/// *which tuples* the body derives before and after a ref transition, so
/// collapsing every row to an empty tuple -- what `run_goal(repo, body, &[])`
/// would do -- would erase exactly the distinction a trigger needs to fire on
/// the right entity rather than any entity.
///
/// # Errors
/// See [`QueryError`].
pub fn run_body(repo: &gix::Repository, body_source: &str) -> Result<Vec<Vec<Value>>, QueryError> {
    let checked = checked_program(repo)?;

    let mut goal =
        gix_query_parse::parse_goal(body_source).map_err(|e| QueryError::Parse(join(&e)))?;
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    let facts = RepoFacts::new(repo);
    run_checked_goal(repo, &facts, &checked, &goal)
}

/// [`run_body`], but evaluating `body_source` against `repo` as it would
/// answer if every ref named in `overrides` held the value given there
/// instead of its real one (`Some` = held this value, `None` = did not
/// exist), rather than against the live repository.
///
/// This is the seam `gix-effect`'s trigger detection (`DEVPLAN-effect.md`
/// D3) evaluates a `when` body "at old" through, replacing what used to be a
/// rewind-then-restore of the real ref: [`RepoFacts::as_of`] layers a
/// read-only [`gix_refstore::AsOfRefStore`] decorator under the same
/// repo-backed providers `run_body` uses, so the repository is read, never
/// written, to answer what its `when` body would have derived before a ref
/// moved. Only the ref-glob-backed EDB (`member`, `revoked`, `claim`,
/// `kind`, `target`, `signer`, `verdict`, `anchor`) sees `overrides`;
/// content-addressed predicates read the object database directly and are
/// unaffected, exactly as `RepoFacts::as_of` documents.
///
/// # Errors
/// See [`QueryError`].
pub fn run_body_as_of(
    repo: &gix::Repository,
    body_source: &str,
    overrides: impl IntoIterator<Item = (RefName, Option<gix::ObjectId>)>,
) -> Result<Vec<Vec<Value>>, QueryError> {
    let checked = checked_program(repo)?;

    let mut goal =
        gix_query_parse::parse_goal(body_source).map_err(|e| QueryError::Parse(join(&e)))?;
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    let facts = RepoFacts::as_of(repo, overrides);
    run_checked_goal(repo, &facts, &checked, &goal)
}

/// Tier 1: apply `predicate` with `bound` values at the given argument
/// positions. Every other position becomes an output column, in signature
/// order -- "bound args filter, free args become columns."
///
/// `predicate` is looked up by name against [`checked_program`]'s registry,
/// so it may be a bare host predicate or builtin name, or a rule module's
/// qualified `pub` name (e.g. `review.reviewed`), exactly as tier 3 resolves
/// names in goal text.
///
/// # Errors
/// See [`QueryError`]. [`QueryError::UnknownPredicate`] and
/// [`QueryError::BindOutOfRange`] are tier 1's own; the rest are shared with
/// [`run_goal`].
pub fn run_predicate(
    repo: &gix::Repository,
    predicate: &str,
    bound: &[(usize, Value)],
) -> Result<Vec<Vec<Value>>, QueryError> {
    let checked = checked_program(repo)?;

    let key = checked
        .registry()
        .predicates()
        .find(|entry| entry.key.name.as_ref() == predicate)
        .map(|entry| entry.key.clone())
        .ok_or_else(|| QueryError::UnknownPredicate(predicate.to_owned()))?;

    for (position, _) in bound {
        if *position >= key.arity {
            return Err(QueryError::BindOutOfRange {
                predicate: predicate.to_owned(),
                position: *position,
                arity: key.arity,
            });
        }
    }

    let mut goal = gix_query_parse::goal_from_predicate(key.name.clone(), key.arity, bound);
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    let facts = RepoFacts::new(repo);
    run_checked_goal(repo, &facts, &checked, &goal)
}

/// Shared tail of tiers 1 and 3: validate a resolved [`gix_query_ir::Goal`]
/// against `checked`, magic-set rewrite it, lower it to Nemo, audit the
/// emission, and run it to a demand fixpoint against `repo` through `facts`.
///
/// `facts` is supplied rather than built here so a caller chooses live
/// ([`RepoFacts::new`]) or as-of ([`RepoFacts::as_of`]) evaluation -- the
/// only difference [`run_body_as_of`] has from [`run_body`].
fn run_checked_goal(
    repo: &gix::Repository,
    facts: &RepoFacts<'_>,
    checked: &CheckedProgram,
    goal: &gix_query_ir::Goal,
) -> Result<Vec<Vec<Value>>, QueryError> {
    Ok(run_checked_goal_full(repo, facts, checked, goal)?.2.answer)
}

/// [`run_checked_goal`], but keeping the converged tables and the rewritten
/// and emitted program alongside the answer -- what [`explain_predicate`]
/// needs to trace a derivation, since tracing re-runs against exactly the
/// state a fresh call would otherwise discard.
fn run_checked_goal_full(
    repo: &gix::Repository,
    facts: &RepoFacts<'_>,
    checked: &CheckedProgram,
    goal: &gix_query_ir::Goal,
) -> Result<
    (
        gix_query_eval::RewrittenProgram,
        gix_query_eval::NemoProgram,
        gix_query_eval::LoopResult,
    ),
    QueryError,
> {
    gix_query_check::check_goal(goal, checked).map_err(|e| QueryError::CheckGoal(join(&e)))?;

    let rewritten = gix_query_eval::rewrite_program(checked, goal);
    let emitted = gix_query_eval::NemoProgram::emit(&rewritten)?;
    emitted.audit()?;

    let builtins = AllBuiltins::new(repo);
    let result = gix_query_eval::run_to_fixpoint(&rewritten, &emitted, facts, &builtins, 64)?;

    Ok((rewritten, emitted, result))
}

/// Tier 1's `explain`: trace the derivation of one fully ground tuple of
/// `predicate`.
///
/// Every position must be bound in `bound` -- unlike [`run_predicate`], there
/// is no free-position case here. [`gix_query_eval::explain`] traces one
/// concrete tuple against a completed evaluation; a partially bound call has
/// no single tuple to trace, and resolving that (which of possibly many rows
/// to explain, and how to render several derivation trees at once) is a
/// product decision the effect and query CLIs have not made yet, not a gap
/// this function silently papers over. `explain` for tier 3 goals has the
/// same open question one level up -- a goal's `select` can still name free
/// variables -- so it is not offered here either.
///
/// This wires faithfully to [`gix_query_eval::explain`] rather than working
/// around it, and that entry point has two known gaps of its own, pinned by
/// `gix-query/tests/run.rs`: it returns `Ok(None)` for a bare base predicate
/// traced directly with no defining rule (Nemo's tracer only produces a node
/// when a join forces a rule application), and it errors on any predicate a
/// rule module defines, because module predicates are always qualified with
/// a `.` (`gix_query_rules::assemble`'s `qualify`) and the trace builds its
/// Nemo fact text from the predicate name verbatim, which Nemo's parser
/// rejects. Fixing either belongs to `gix-query-eval`, not here.
///
/// # Errors
/// [`QueryError::ExplainRequiresGroundTuple`] if `bound` does not cover every
/// position; otherwise see [`QueryError`].
pub fn explain_predicate(
    repo: &gix::Repository,
    predicate: &str,
    bound: &[(usize, Value)],
) -> Result<Option<gix_query_eval::DerivationTree>, QueryError> {
    let checked = checked_program(repo)?;

    let key = checked
        .registry()
        .predicates()
        .find(|entry| entry.key.name.as_ref() == predicate)
        .map(|entry| entry.key.clone())
        .ok_or_else(|| QueryError::UnknownPredicate(predicate.to_owned()))?;

    for (position, _) in bound {
        if *position >= key.arity {
            return Err(QueryError::BindOutOfRange {
                predicate: predicate.to_owned(),
                position: *position,
                arity: key.arity,
            });
        }
    }
    if bound.len() != key.arity {
        return Err(QueryError::ExplainRequiresGroundTuple {
            predicate: predicate.to_owned(),
            arity: key.arity,
            bound: bound.len(),
        });
    }
    let mut tuple: Vec<Option<Value>> = vec![None; key.arity];
    for (position, value) in bound {
        tuple[*position] = Some(value.clone());
    }
    let tuple: Vec<Value> = tuple
        .into_iter()
        .map(|v| v.expect("every position bound, checked above"))
        .collect();

    let mut goal = gix_query_parse::goal_from_predicate(key.name.clone(), key.arity, bound);
    gix_query_parse::resolve_goal(&mut goal, checked.registry());

    let facts = RepoFacts::new(repo);
    let (rewritten, emitted, result) = run_checked_goal_full(repo, &facts, &checked, &goal)?;

    Ok(gix_query_eval::explain(
        &emitted,
        &result.tables,
        &rewritten,
        &key,
        &tuple,
    )?)
}
