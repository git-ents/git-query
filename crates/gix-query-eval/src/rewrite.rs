//! The magic-set rewrite (`DEVPLAN.md` §2.10): IR in, IR out.
//!
//! Semi-naive evaluation is bottom-up; mode discipline is top-down. Without
//! this pass a `+Rev` predicate has no runtime meaning and the first call
//! materializes the whole repository before filtering it down. The rewrite
//! makes demand for bound arguments explicit as `magic_`-prefixed relations,
//! guards each rule by its own demand, and leaves plain bottom-up evaluation
//! to compute exactly the demanded slice.
//!
//! # Why nothing is renamed
//!
//! A classical magic-set rewrite also renames every EDB literal it touches
//! (`p` becomes `p_ext` or similar), because a general engine cannot be
//! trusted to treat a plain relation name as "whatever the demand loop
//! injected this round" versus "the loop hasn't run yet". We do not need that
//! indirection: every host predicate name is registry-reserved and never a
//! rule head in our own program (base predicates are not redefinable), so a
//! plain predicate name can only ever mean "facts injected by us" or "nothing
//! yet". The demand loop (`crate::demand`) injects answers under the
//! predicate's own name, so call sites are left untouched. This removes an
//! entire renaming pass and the bug class that comes with keeping two names
//! for one predicate in sync.
//!
//! # Author order is the SIPS
//!
//! Every literal's adornment is already computed -- for rule bodies, by
//! whatever produced the [`CheckedProgram`]; for a goal body, by
//! [`adorn_body`] here, using the same left-to-right walk. Neither this
//! module nor its caller searches for a binding order: the order is the one
//! the author wrote.
//!
//! Left-to-right gives the *call-site* binding, which is not the same thing
//! as the adornment a demand site is keyed on. A guarded IDB predicate has
//! exactly one guard, adorned by its own required-bound set, and the rewrite
//! never renames a call site to match -- so whatever the call site binds
//! (more than required, or, for a negated literal, everything), the demand
//! this module mints for a guarded callee is always projected onto that
//! callee's own required-bound set. [`Builder::visit_literal`] is the one
//! place this projection happens, which is what lets it apply uniformly to a
//! rule body's already-narrowed adornment and a goal body's raw one alike.
//!
//! # What is emitted, and why it is enough
//!
//! For every rule whose head predicate is adorned, a demand guard is
//! inserted at body position 0 (recorded in [`RuleOrigin::body_map`], so a
//! later literal's original index survives the shift). For every literal
//! anywhere -- rule body or goal body -- whose demand adornment (see
//! [`Builder::demand_adornment`]) is non-empty, a magic rule collects the
//! concrete bound tuple demanded at that point, from the guard (if any) and
//! the literal prefix before it. A literal calling a host predicate never
//! changes; only the magic collector is new.

use std::collections::{BTreeMap, BTreeSet};

use gix_query_ir::{
    Adornment, ArgSet, Atom, CheckedProgram, Goal, GoalAdornments, Literal, PredicateKey, Program,
    Registry, RewriteKind, Rule, RuleId, RuleOrigin, SpannedTerm, Term, Var,
};

/// Which side of the seam answers a [`DemandSite`].
///
/// An IDB site drives another rule's own guard: nothing outside the rewritten
/// program answers it, Nemo's own bottom-up evaluation does. An `Edb` or
/// `Builtin` site is answered from outside Nemo entirely, by
/// [`gix_query_ir::FactSource`] or [`gix_query_ir::Builtin`] respectively,
/// which is the whole reason [`crate::demand`] exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DemandKind {
    /// Defined by rules in this program; the guard alone answers it.
    Idb,
    /// A repo-backed relation, answered through `FactSource`.
    Edb,
    /// A moded, non-enumerable call, answered through `Builtin`.
    Builtin,
}

/// One distinct (predicate, adornment) pair the rewrite found demanded.
///
/// Deduplicated across every call site: two literals demanding the same
/// predicate under the same binding pattern are the same demand relation,
/// however many magic rules feed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemandSite {
    /// The predicate as written in source text -- never renamed.
    pub base: PredicateKey,
    /// The binding pattern demanded.
    pub adornment: Adornment,
    /// Who is responsible for answering it.
    pub kind: DemandKind,
}

impl DemandSite {
    /// The demand relation's key: `magic_<name>_<suffix>`, arity equal to the
    /// number of bound positions. Reuses [`PredicateKey::magic`] so the
    /// naming scheme has exactly one definition in the workspace.
    #[must_use]
    pub fn demand_key(&self) -> PredicateKey {
        self.base.magic(&self.adornment)
    }
}

/// The magic-set rewrite of one [`CheckedProgram`] against one query.
///
/// Carries everything [`crate::lower`] and [`crate::demand`] need and nothing
/// they would have to recompute: the rewritten rules (each with its
/// [`RuleOrigin`] intact), the full set of demand sites the program and the
/// query jointly created, and the EDB predicates that must be loaded whole
/// because some call site to them is unadorned.
#[derive(Debug, Clone)]
pub struct RewrittenProgram {
    /// Every generated rule, source rules included -- a source rule with no
    /// adorned head is carried through with an identity `body_map` and an
    /// empty guard, so this is always the complete program to lower.
    pub rules: Program,
    /// Every distinct demand site, in first-discovered order.
    pub demand: Vec<DemandSite>,
    /// EDB predicates with at least one unadorned call site anywhere, and
    /// therefore loaded in full, once, rather than through demand.
    ///
    /// A predicate can be in both this set and have entries in `demand`: our
    /// registry allows `member/1` to be called both free and bound, and nothing
    /// stops one program from doing both.
    pub eager: BTreeSet<PredicateKey>,
    /// The answer relation's key -- `magic_answer_<n>`, arity equal to the
    /// goal's `select` length -- so [`crate::engine`] knows what to read back.
    pub answer: PredicateKey,
}

/// The reserved arity-suffixed name for the synthetic rule that computes a
/// goal's answer tuples.
///
/// Arity-suffixed because two goals with different `select` lengths must not
/// collide, and magic-prefixed so [`gix_query_ir::PredicateKey::is_magic`]
/// keeps it out of query output and derivation trees without a second special
/// case.
fn answer_key(arity: usize) -> PredicateKey {
    PredicateKey::new(format!("{}answer", PredicateKey::MAGIC_PREFIX), arity)
}

/// Compute the raw call-site adornment of every literal in a goal body, left
/// to right.
///
/// This is the one place the rewrite computes an adornment rather than
/// consuming one `gix-query-check` already produced -- a goal is issued at
/// query time, with no rule behind it for a validator to have annotated. A
/// position starts bound exactly when its term is a constant, and every
/// variable a positive or builtin literal touches becomes bound for
/// everything after it. A negated literal or a comparison is a filter and
/// cannot bind a fresh variable, which range restriction already guarantees
/// holds by the time this runs.
///
/// Unlike pass 8's `body_adornment`, this function does *not* narrow a
/// derived call down to the callee's own required-bound set -- it has no
/// registry to narrow against, and does not need one: [`Builder::visit_literal`]
/// applies that projection uniformly to whatever adornment it is handed,
/// goal or rule body alike. What this function returns is purely the
/// call-site binding, used to decide what is bound for the *next* literal in
/// the same body and nothing more.
#[must_use]
pub fn adorn_body(body: &[Literal]) -> GoalAdornments {
    let mut bound_vars: BTreeSet<Var> = BTreeSet::new();
    let mut out = Vec::with_capacity(body.len());
    for lit in body {
        let terms: Vec<&Term> = lit.terms().collect();
        let mut bound = ArgSet::empty();
        for (i, t) in terms.iter().enumerate() {
            let is_bound = match t {
                Term::Const(_) => true,
                Term::Var(v) => bound_vars.contains(v),
                Term::Anon => false,
            };
            if is_bound {
                bound.insert(i);
            }
        }
        out.push(Adornment::new(bound, terms.len()));
        if lit.is_generator() {
            for v in lit.vars() {
                bound_vars.insert(v.clone());
            }
        }
    }
    GoalAdornments { body: out }
}

/// Select the terms of `atom` at the positions `adornment` marks bound, in
/// argument order -- the arguments a demand or guard relation carries.
fn bound_terms(atom: &Atom, adornment: &Adornment) -> Vec<SpannedTerm> {
    atom.terms
        .iter()
        .enumerate()
        .filter(|(i, _)| adornment.bound.contains(*i))
        .map(|(_, t)| t.clone())
        .collect()
}

/// Classify a host-or-derived predicate for [`DemandSite::kind`].
fn classify(key: &PredicateKey, registry: &Registry) -> DemandKind {
    if registry.is_builtin(key) {
        DemandKind::Builtin
    } else if registry.is_base(key) {
        DemandKind::Edb
    } else {
        DemandKind::Idb
    }
}

/// Accumulates the output of the rewrite across every rule and the goal.
struct Builder<'a> {
    registry: &'a Registry,
    generated: Vec<Rule>,
    // `Adornment` has no total order (it is arity plus a bitset, and deriving
    // `Ord` on that pair is not worth doing for a key that is used nowhere
    // else) -- so dedup on `(PredicateKey, ArgSet)`, which is exactly as
    // discriminating for a real atom, whose term count already equals its
    // key's arity.
    demand: BTreeMap<(PredicateKey, ArgSet), (Adornment, DemandKind)>,
    demand_order: Vec<(PredicateKey, ArgSet)>,
    eager: BTreeSet<PredicateKey>,
}

impl<'a> Builder<'a> {
    fn new(registry: &'a Registry) -> Self {
        Self {
            registry,
            generated: Vec::new(),
            demand: BTreeMap::new(),
            demand_order: Vec::new(),
            eager: BTreeSet::new(),
        }
    }

    fn note_demand(&mut self, key: PredicateKey, adornment: Adornment, kind: DemandKind) {
        let pair = (key, adornment.bound);
        if !self.demand.contains_key(&pair) {
            self.demand.insert(pair.clone(), (adornment, kind));
            self.demand_order.push(pair);
        }
    }

    /// The adornment a demand site for `key` must be keyed on.
    ///
    /// For a guarded IDB predicate this is `key`'s own required-bound set,
    /// unconditionally -- never the call site's binding. `visit_rule` guards
    /// `key`'s defining rule on exactly that set, and the rewrite never
    /// renames a call site, so there is exactly one guard for `key` to feed.
    /// A call site that binds more than required (mode discipline allows
    /// that) or that is forced to bind everything because it is negated
    /// (range restriction requires that) would otherwise mint a demand
    /// relation under a *different* adornment than the guard reads: demand
    /// produced, never consumed, `key` derives nothing. For a positive
    /// caller that is silent under-derivation; for `!key(...)` it is
    /// over-derivation, since vacuous truth follows from `key` wrongly
    /// staying empty. Every other kind of call site -- EDB, builtin -- has
    /// no guard to align with, so its call-site adornment is definitionally
    /// the right one to key demand on.
    fn demand_adornment(
        &self,
        key: &PredicateKey,
        kind: DemandKind,
        call_site: &Adornment,
    ) -> Adornment {
        if kind != DemandKind::Idb {
            return call_site.clone();
        }
        let required = self
            .registry
            .predicate(key)
            .and_then(gix_query_ir::PredicateEntry::always_required_bound)
            .unwrap_or_default();
        Adornment::new(required, key.arity)
    }

    /// Process one literal at body position `i`, recording eager/demand
    /// obligations. `prefix` is everything at positions `< i` in the rule (or
    /// goal) as it will appear in the *generated* body, guard included, so a
    /// magic rule's body is exactly the literals that ran before this one.
    fn visit_literal(
        &mut self,
        lit: &Literal,
        adornment: &Adornment,
        prefix: &[Literal],
        source: Option<RuleId>,
        seed_kind: RewriteKind,
    ) {
        let Some(atom) = lit.atom() else {
            // A comparison: native Nemo syntax, no defining rule, never a
            // demand site.
            return;
        };
        let key = atom.predicate.clone();
        let kind = classify(&key, self.registry);
        let adornment = &self.demand_adornment(&key, kind, adornment);
        if !adornment.is_adorned() {
            if kind == DemandKind::Edb {
                self.eager.insert(key);
            }
            return;
        }
        self.note_demand(key.clone(), adornment.clone(), kind);
        let magic_key = key.magic(adornment);
        let head = Atom {
            predicate: magic_key,
            terms: bound_terms(atom, adornment),
            span: atom.span,
        };
        let body_map = prefix.iter().map(|_| None).collect();
        self.generated.push(Rule {
            head,
            body: prefix.to_vec(),
            span: atom.span,
            origin: Some(RuleOrigin {
                source,
                kind: seed_kind,
                body_map,
            }),
        });
    }

    /// Rewrite one source rule: guard its head if adorned, walk its body
    /// collecting demand, and push the resulting rule unconditionally so
    /// every source rule has exactly one counterpart in the output.
    fn visit_rule(&mut self, id: RuleId, rule: &Rule, adornments: &gix_query_ir::RuleAdornments) {
        let mut new_body = Vec::with_capacity(rule.body.len() + 1);
        let mut body_map = Vec::with_capacity(rule.body.len() + 1);

        if adornments.head.is_adorned() {
            let guard_key = rule.head.predicate.magic(&adornments.head);
            new_body.push(Literal::Positive(Atom {
                predicate: guard_key,
                terms: bound_terms(&rule.head, &adornments.head),
                span: rule.head.span,
            }));
            body_map.push(None);
        }

        for (i, lit) in rule.body.iter().enumerate() {
            let adornment = &adornments.body[i];
            self.visit_literal(lit, adornment, &new_body, Some(id), RewriteKind::MagicRule);
            new_body.push(lit.clone());
            body_map.push(Some(i));
        }

        self.generated.push(Rule {
            head: rule.head.clone(),
            body: new_body,
            span: rule.span,
            origin: Some(RuleOrigin {
                source: Some(id),
                kind: RewriteKind::Guarded,
                body_map,
            }),
        });
    }

    /// Rewrite the goal body into the answer rule, collecting demand exactly
    /// as for a source rule's body but with no guard (a goal has no caller to
    /// demand it) and [`RewriteKind::MagicSeed`] on whatever it demands.
    fn visit_goal(&mut self, goal: &Goal, adornments: &GoalAdornments) -> PredicateKey {
        let mut prefix: Vec<Literal> = Vec::with_capacity(goal.body.len());
        for (i, lit) in goal.body.iter().enumerate() {
            let adornment = &adornments.body[i];
            self.visit_literal(lit, adornment, &prefix, None, RewriteKind::MagicSeed);
            prefix.push(lit.clone());
        }

        let answer = answer_key(goal.select.len());
        let head = Atom {
            predicate: answer.clone(),
            terms: goal
                .select
                .iter()
                .map(|v| SpannedTerm::synthetic(Term::Var(v.clone())))
                .collect(),
            span: goal.span,
        };
        let body_map = goal.body.iter().enumerate().map(|(i, _)| Some(i)).collect();
        self.generated.push(Rule {
            head,
            body: goal.body.clone(),
            span: goal.span,
            origin: Some(RuleOrigin {
                source: None,
                kind: RewriteKind::Guarded,
                body_map,
            }),
        });
        answer
    }
}

/// Rewrite `checked` against `goal`, producing the program
/// [`crate::lower`] emits and [`crate::demand`] drives.
///
/// [`CheckedProgram::adornments`] must already cover every rule in the program
/// -- true of anything `gix-query-check`'s pass 8 actually produced, and a
/// contract test fixtures in this crate must uphold by hand, since that pass
/// is not this crate's to call. A rule with no entry is treated as fully
/// free, which is always sound (it just forgoes demand narrowing) and never
/// unsound, so a missing entry degrades gracefully rather than panicking.
#[must_use]
pub fn rewrite(checked: &CheckedProgram, goal: &Goal) -> RewrittenProgram {
    let mut builder = Builder::new(checked.registry());

    for (id, rule) in checked.program().rules().iter().enumerate() {
        let id = RuleId(id);
        let default_head = Adornment::new(ArgSet::empty(), rule.head.terms.len());
        let default_body: Vec<Adornment> = rule
            .body
            .iter()
            .map(|l| Adornment::new(ArgSet::empty(), l.terms().count()))
            .collect();
        match checked.adornments(id) {
            Some(a) => builder.visit_rule(id, rule, a),
            None => {
                let a = gix_query_ir::RuleAdornments {
                    rule: id,
                    head: default_head,
                    body: default_body,
                };
                builder.visit_rule(id, rule, &a);
            }
        }
    }

    let goal_adornments = adorn_body(&goal.body);
    let answer = builder.visit_goal(goal, &goal_adornments);

    let mut program = Program::new();
    for rule in builder.generated {
        program.push_rule(rule, "rewrite");
    }

    let demand = builder
        .demand_order
        .iter()
        .map(|pair| {
            let (adornment, kind) = builder.demand[pair].clone();
            DemandSite {
                base: pair.0.clone(),
                adornment,
                kind,
            }
        })
        .collect();

    RewrittenProgram {
        rules: program,
        demand,
        eager: builder.eager,
        answer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_query_ir::{PredicateClass, PredicateEntry, Stratification, Visibility};
    use std::collections::BTreeMap;

    /// Build a minimal `CheckedProgram` by hand: `gix-query-check` is being
    /// written concurrently and has no `validate()` yet, so every rewrite
    /// test constructs its own checked program directly against the frozen
    /// IR types, computing adornments with the same left-to-right rule
    /// `adorn_body` implements for goals.
    struct Fixture {
        registry: Registry,
        program: Program,
        adornments: BTreeMap<RuleId, gix_query_ir::RuleAdornments>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                registry: Registry::host(),
                program: Program::new(),
                adornments: BTreeMap::new(),
            }
        }

        /// Register a fully-enumerable IDB predicate (no required-bound
        /// arguments), the common case in these tests.
        fn idb_free(&mut self, name: &str, arity: usize) {
            self.registry
                .insert_predicate(PredicateEntry::idb(
                    PredicateKey::new(name, arity),
                    Visibility::Public,
                ))
                .expect("insertable");
        }

        /// Register an IDB predicate whose canonical call adornment requires
        /// `bound` positions.
        fn idb_bound(&mut self, name: &str, arity: usize, bound: &[usize]) {
            let mut entry = PredicateEntry::idb(PredicateKey::new(name, arity), Visibility::Public);
            entry.class = PredicateClass::Idb {
                required_bound: Some(ArgSet::from_indices(bound.iter().copied())),
            };
            self.registry.insert_predicate(entry).expect("insertable");
        }

        /// Add a rule, computing its adornments with the same left-to-right
        /// algorithm `adorn_body` uses for goals, seeded by `head_bound`
        /// (the positions the predicate's canonical adornment requires bound).
        fn rule(&mut self, rule: Rule, head_bound: &[usize]) -> RuleId {
            let head = Adornment::new(
                ArgSet::from_indices(head_bound.iter().copied()),
                rule.head.terms.len(),
            );
            let goal_style = adorn_body(&rule.body);
            let mut bound_vars: BTreeSet<Var> = BTreeSet::new();
            for (i, t) in rule.head.terms.iter().enumerate() {
                if head.bound.contains(i)
                    && let Term::Var(v) = &t.term
                {
                    bound_vars.insert(v.clone());
                }
            }
            // Re-run the propagation seeded with the head's bound variables,
            // since `adorn_body` alone assumes an empty start.
            let mut out = Vec::with_capacity(rule.body.len());
            for lit in &rule.body {
                let mut bound = ArgSet::empty();
                let terms: Vec<&Term> = lit.terms().collect();
                for (i, t) in terms.iter().enumerate() {
                    let is_bound = match t {
                        Term::Const(_) => true,
                        Term::Var(v) => bound_vars.contains(v),
                        Term::Anon => false,
                    };
                    if is_bound {
                        bound.insert(i);
                    }
                }
                out.push(Adornment::new(bound, terms.len()));
                if lit.is_generator() {
                    for v in lit.vars() {
                        bound_vars.insert(v.clone());
                    }
                }
            }
            let _ = goal_style;
            let id = self.program.push_rule(rule, "test");
            self.adornments.insert(
                id,
                gix_query_ir::RuleAdornments {
                    rule: id,
                    head,
                    body: out,
                },
            );
            id
        }

        /// Replace one rule's adornments, for the cases where the
        /// left-to-right reconstruction above is not what the test wants to
        /// exercise.
        fn adorn(&mut self, id: RuleId, adornments: gix_query_ir::RuleAdornments) {
            self.adornments.insert(id, adornments);
        }

        fn checked(self) -> CheckedProgram {
            CheckedProgram::assemble(
                self.program,
                self.registry,
                Stratification::default(),
                self.adornments,
                BTreeMap::new(),
            )
        }
    }

    fn pos(name: &str, vars: &[&str]) -> Literal {
        Literal::Positive(Atom::new(
            name,
            vars.iter().map(|v| Term::var(*v)).collect(),
        ))
    }

    fn neg(name: &str, vars: &[&str]) -> Literal {
        Literal::Negated(Atom::new(
            name,
            vars.iter().map(|v| Term::var(*v)).collect(),
        ))
    }

    #[test]
    fn an_unadorned_head_gets_no_guard() {
        // `active(M) :- member(M), !revoked(M)`: `active`'s head is free, so
        // its own defining rule is never guarded. `member(M)` is called free
        // (eager); `!revoked(M)` calls with `M` already bound by the literal
        // before it -- negation always needs full binding, so that call site
        // is a demand site regardless of `revoked`'s own enumerability.
        let mut f = Fixture::new();
        f.idb_free("active", 1);
        f.rule(
            Rule::new(
                Atom::new("active", vec![Term::var("M")]),
                vec![pos("member", &["M"]), neg("revoked", &["M"])],
            ),
            &[],
        );
        let checked = f.checked();
        let goal = Goal::new(vec![pos("active", &["M"])], vec![Var::new("M")]);
        let rewritten = rewrite(&checked, &goal);

        let revoked = PredicateKey::new("revoked", 1);
        assert_eq!(
            rewritten.demand.len(),
            1,
            "only the negated call should demand anything: {:?}",
            rewritten.demand
        );
        assert_eq!(rewritten.demand[0].base, revoked);
        assert_eq!(rewritten.demand[0].kind, DemandKind::Edb);
        assert!(rewritten.eager.contains(&PredicateKey::new("member", 1)));
        assert!(!rewritten.eager.contains(&revoked));

        let active_rule = rewritten
            .rules
            .rules()
            .iter()
            .find(|r| r.head.predicate == PredicateKey::new("active", 1))
            .expect("the active rule survived unchanged");
        assert_eq!(active_rule.body.len(), 2, "no guard should be inserted");
    }

    #[test]
    fn an_adorned_head_is_guarded_and_demanded_by_its_callers() {
        // `reach(Rev, C) :- commit(Rev)` with `+Rev` (position 0 bound):
        // the defining rule is guarded, and the goal's own bound `Rev` seeds
        // the guard through a magic seed.
        let mut f = Fixture::new();
        f.idb_bound("reach", 2, &[0]);
        f.rule(
            Rule::new(
                Atom::new("reach", vec![Term::var("Rev"), Term::var("Rev")]),
                vec![pos("commit", &["Rev"])],
            ),
            &[0],
        );
        let checked = f.checked();
        // Simulate the CLI having bound `Rev` to a constant before the goal
        // reaches this crate -- `adorn_body` reads that off the term, not off
        // an out-of-band hint.
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new(
                "reach",
                vec![Term::sym("deadbeef"), Term::var("C")],
            ))],
            vec![Var::new("C")],
        );
        let rewritten = rewrite(&checked, &goal);

        let reach_key = PredicateKey::new("reach", 2);
        assert!(
            rewritten
                .demand
                .iter()
                .any(|d| d.base == reach_key && d.kind == DemandKind::Idb),
            "reach must be demanded as an IDB site: {:?}",
            rewritten.demand
        );

        let reach_rule = rewritten
            .rules
            .rules()
            .iter()
            .find(|r| r.head.predicate == reach_key)
            .expect("reach's defining rule");
        assert_eq!(reach_rule.body.len(), 2, "guard plus the original literal");
        let guard = reach_rule.body[0].atom().expect("guard is an atom");
        assert!(guard.predicate.is_magic());
        assert_eq!(guard.terms.len(), 1, "one bound position");

        // The seed comes from the goal, so it must carry no source rule.
        let seed = rewritten
            .rules
            .rules()
            .iter()
            .find(|r| {
                r.origin
                    .as_ref()
                    .is_some_and(|o| o.kind == RewriteKind::MagicSeed)
            })
            .expect("a magic seed was generated");
        assert_eq!(seed.origin.as_ref().unwrap().source, None);
        assert!(seed.body.is_empty(), "a seed from a ground goal is a fact");
    }

    #[test]
    fn a_body_map_survives_the_guard_shift() {
        let mut f = Fixture::new();
        f.idb_bound("reach", 1, &[0]);
        f.rule(
            Rule::new(
                Atom::new("reach", vec![Term::var("Rev")]),
                vec![pos("commit", &["Rev"]), pos("member", &["M"])],
            ),
            &[0],
        );
        let checked = f.checked();
        let goal = Goal::new(vec![pos("reach", &["Rev"])], vec![Var::new("Rev")]);
        let rewritten = rewrite(&checked, &goal);

        let reach_key = PredicateKey::new("reach", 1);
        let reach_rule = rewritten
            .rules
            .rules()
            .iter()
            .find(|r| r.head.predicate == reach_key)
            .expect("reach's defining rule");
        let origin = reach_rule.origin.as_ref().expect("generated rule");
        // Position 0 is the injected guard; the original body's literal 0
        // (commit) now sits at generated position 1, and literal 1 (member)
        // at position 2.
        assert_eq!(origin.body_map, vec![None, Some(0), Some(1)]);
    }

    #[test]
    fn an_edb_predicate_can_be_both_eager_and_demanded() {
        // `member/1` legitimately called free in one rule and bound in
        // another; both must be honored.
        let mut f = Fixture::new();
        f.idb_free("all_members", 1);
        f.idb_bound("is_member", 1, &[0]);
        f.rule(
            Rule::new(
                Atom::new("all_members", vec![Term::var("M")]),
                vec![pos("member", &["M"])],
            ),
            &[],
        );
        f.rule(
            Rule::new(
                Atom::new("is_member", vec![Term::var("M")]),
                vec![pos("member", &["M"])],
            ),
            &[0],
        );
        let checked = f.checked();
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new(
                "is_member",
                vec![Term::sym("alice")],
            ))],
            vec![],
        );
        let rewritten = rewrite(&checked, &goal);

        let member = PredicateKey::new("member", 1);
        assert!(rewritten.eager.contains(&member));
        assert!(
            rewritten
                .demand
                .iter()
                .any(|d| d.base == member && d.kind == DemandKind::Edb)
        );
    }

    #[test]
    fn a_builtin_call_site_is_a_demand_site() {
        let mut f = Fixture::new();
        f.idb_free("out", 1);
        f.rule(
            Rule::new(
                Atom::new("out", vec![Term::var("Blob")]),
                vec![Literal::Builtin(Atom::new(
                    "bind",
                    vec![
                        Term::var("Anchor"),
                        Term::var("Rev"),
                        Term::var("Blob"),
                        Term::var("S"),
                        Term::var("E"),
                        Term::var("Pos"),
                        Term::var("Con"),
                    ],
                ))],
            ),
            &[],
        );
        // Hand-fix the adornment: bind is (+,+,-,-,-,-,-).
        let checked_program = {
            let id = RuleId(0);
            f.adorn(
                id,
                gix_query_ir::RuleAdornments {
                    rule: id,
                    head: Adornment::new(ArgSet::empty(), 1),
                    body: vec![Adornment::new(ArgSet::from_indices([0, 1]), 7)],
                },
            );
            f.checked()
        };
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new("out", vec![Term::var("B")]))],
            vec![Var::new("B")],
        );
        let rewritten = rewrite(&checked_program, &goal);
        let bind = PredicateKey::new("bind", 7);
        assert!(
            rewritten
                .demand
                .iter()
                .any(|d| d.base == bind && d.kind == DemandKind::Builtin)
        );
    }

    #[test]
    fn comparisons_are_never_demand_sites() {
        let mut f = Fixture::new();
        f.idb_free("after", 2);
        f.rule(
            Rule::new(
                Atom::new("after", vec![Term::var("A"), Term::var("B")]),
                vec![
                    pos("line", &["A"]),
                    pos("line", &["B"]),
                    Literal::Compare(gix_query_ir::Compare {
                        op: gix_query_ir::CompareOp::Gt,
                        left: SpannedTerm::synthetic(Term::var("A")),
                        right: SpannedTerm::synthetic(Term::var("B")),
                        span: gix_query_ir::Span::synthetic(),
                    }),
                ],
            ),
            &[],
        );
        let checked = f.checked();
        let goal = Goal::new(
            vec![pos("after", &["A", "B"])],
            vec![Var::new("A"), Var::new("B")],
        );
        let rewritten = rewrite(&checked, &goal);
        assert!(
            rewritten.demand.iter().all(|d| d.base.name.as_ref() != ">"),
            "a comparison must never become a demand site: {:?}",
            rewritten.demand
        );
    }

    #[test]
    fn the_answer_rule_projects_select_in_order() {
        let mut f = Fixture::new();
        f.idb_free("p", 2);
        f.rule(
            Rule::new(
                Atom::new("p", vec![Term::var("A"), Term::var("B")]),
                vec![pos("member", &["A"]), pos("member", &["B"])],
            ),
            &[],
        );
        let checked = f.checked();
        let goal = Goal::new(
            vec![pos("p", &["A", "B"])],
            vec![Var::new("B"), Var::new("A")],
        );
        let rewritten = rewrite(&checked, &goal);
        let answer_rule = rewritten
            .rules
            .rules()
            .iter()
            .find(|r| r.head.predicate == rewritten.answer)
            .expect("answer rule");
        assert_eq!(rewritten.answer.arity, 2);
        assert_eq!(
            answer_rule.head.terms[0].term,
            Term::var("B"),
            "select order must be preserved, not body order"
        );
    }

    #[test]
    fn magic_prefixed_relations_never_appear_as_a_query_defined_predicate() {
        // Belt-and-braces: every relation this rewrite mints is
        // `is_magic()`, so downstream filtering by that one predicate is
        // sufficient and this is the test that keeps it true.
        let mut f = Fixture::new();
        f.idb_bound("reach", 1, &[0]);
        f.rule(
            Rule::new(
                Atom::new("reach", vec![Term::var("Rev")]),
                vec![pos("commit", &["Rev"])],
            ),
            &[0],
        );
        let checked = f.checked();
        let goal = Goal::new(
            vec![Literal::Positive(Atom::new(
                "reach",
                vec![Term::sym("HEAD")],
            ))],
            vec![],
        );
        let rewritten = rewrite(&checked, &goal);
        for site in &rewritten.demand {
            assert!(site.demand_key().is_magic());
        }
        assert!(rewritten.answer.is_magic());
    }
}
