//! The differential test `src/text.rs` promised: random EDBs, run through
//! the compiled `ascent` kernel and through the interpreted path fed
//! [`KERNEL_RULES`] over the identical facts, must agree relation for
//! relation.
//!
//! This is the whole justification for keeping two implementations of one
//! specification (`DEVPLAN.md` Phase 5) -- without it, a change that breaks
//! parity between the frozen bootstrap and the dynamic interpreter would
//! only be caught by inspection.

use std::borrow::Cow;
use std::collections::BTreeMap;

use gix_query_check::{check, check_goal};
use gix_query_eval::{NemoProgram, rewrite_program, run_to_fixpoint};
use gix_query_ir::{
    Backing, Bindings, Export, Fact, FactSource, HostError, ModeSet, PredicateClass,
    PredicateEntry, PredicateKey, Program, Registry, Span, TypeSlot, Value, ValueType, Visibility,
};
use gix_query_kernel::{DENIALS, Facts, KERNEL_RULES, kernel_predicates, violations};
use gix_query_parse::{parse_goal_with_select, parse_module, resolve_goal, resolve_module};
use proptest::prelude::*;

/// A registry declaring exactly the kernel's EDB relations: enumerable,
/// symbol-typed, backed by nothing a ref glob could name -- the kernel's
/// facts are one transaction's tuples, supplied whole, per `text.rs`'s own
/// doc comment on [`kernel_predicates`].
fn kernel_registry() -> Registry {
    let mut registry = Registry::new();
    for (name, arity) in kernel_predicates() {
        let entry = PredicateEntry {
            key: PredicateKey::new(name, arity),
            types: vec![TypeSlot::Known(ValueType::Symbol); arity],
            visibility: Visibility::Public,
            class: PredicateClass::Edb {
                modes: ModeSet::enumerable(arity),
                backing: vec![Backing::AdHoc],
            },
            doc: Cow::Borrowed(""),
        };
        registry
            .insert_predicate(entry)
            .expect("kernel predicates register cleanly");
    }
    registry
}

/// Parse and validate [`KERNEL_RULES`] against [`kernel_registry`].
///
/// Every denial relation is exported: a goal has no module of its own, so
/// only `pub` surface is callable from one (`gix-query-check`'s pass 2), and
/// this test queries each denial relation directly. Marking them `pub` here
/// -- rather than editing the frozen kernel text -- costs nothing semantic:
/// visibility is a resolution-time concept the `ascent!` side does not have
/// at all, so it cannot be part of the parity the differential test checks.
fn checked_kernel() -> gix_query_ir::CheckedProgram {
    let registry = kernel_registry();
    let mut module = parse_module("kernel", KERNEL_RULES).expect("kernel text parses");
    resolve_module(&mut module, &registry);
    for &name in &DENIALS {
        let arity = denial_arity(name);
        module.exports.push(Export {
            key: PredicateKey::new(name, arity),
            span: Span::synthetic(),
        });
    }
    let mut program = Program::new();
    program.add_module(module);
    check(&program, &registry).expect("kernel text validates")
}

/// Each [`DENIALS`] entry's arity, in the same order.
fn denial_arity(name: &str) -> usize {
    match name {
        "ff_violation" | "genesis_violation" => 1,
        "dangling_anchor_violation"
        | "dangling_context_violation"
        | "effect_admin_violation"
        | "second_root_violation"
        | "unsigned_violation" => 2,
        other => unreachable!("not a kernel denial relation: {other}"),
    }
}

/// [`Facts`]'s ten relations as `(key, rows)`, converted to [`Value`] tuples.
fn edb_tables(facts: &Facts) -> BTreeMap<PredicateKey, Vec<Vec<Value>>> {
    let sym = |s: &str| Value::sym(s.to_owned());
    let mut out = BTreeMap::new();
    out.insert(
        PredicateKey::new("ref_create", 2),
        facts
            .ref_create
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("ref_advance", 3),
        facts
            .ref_advance
            .iter()
            .map(|(a, b, c)| vec![sym(a), sym(b), sym(c)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("parent", 2),
        facts
            .parent
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("signed_by", 2),
        facts
            .signed_by
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("member", 1),
        facts.member.iter().map(|(a,)| vec![sym(a)]).collect(),
    );
    out.insert(
        PredicateKey::new("admin", 1),
        facts.admin.iter().map(|(a,)| vec![sym(a)]).collect(),
    );
    out.insert(
        PredicateKey::new("anchor_blob", 2),
        facts
            .anchor_blob
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("context_blob", 2),
        facts
            .context_blob
            .iter()
            .map(|(a, b)| vec![sym(a), sym(b)])
            .collect(),
    );
    out.insert(
        PredicateKey::new("effect_ref", 1),
        facts.effect_ref.iter().map(|(a,)| vec![sym(a)]).collect(),
    );
    out.insert(
        PredicateKey::new("object_exists", 1),
        facts
            .object_exists
            .iter()
            .map(|(a,)| vec![sym(a)])
            .collect(),
    );
    out
}

/// Serves [`edb_tables`] as a [`FactSource`], filtering rows against `bound`
/// -- every kernel predicate is all-free enumerable, so a real call always
/// passes an all-`None` pattern, but honoring `bound` anyway costs nothing
/// and keeps this source correct as a `FactSource`, not just as a fixture.
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

/// The kernel has no builtins at all -- its one comparison, `!=`, is a
/// native [`gix_query_ir::Compare`] literal, never a call through this trait.
struct NoBuiltins;

impl gix_query_ir::Builtin for NoBuiltins {
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

/// Every denial relation, as the ascent side reports it: `(name, sorted
/// deduplicated string rows)` -- the shape [`violations`] already returns.
fn interpreted_violations(
    checked: &gix_query_ir::CheckedProgram,
    facts: &Facts,
) -> Vec<(&'static str, Vec<Vec<String>>)> {
    let tables = TableSource(edb_tables(facts));
    let builtins = NoBuiltins;

    DENIALS
        .into_iter()
        .map(|name| {
            let arity = denial_arity(name);
            let vars: Vec<String> = (0..arity).map(|i| format!("V{i}")).collect();
            let body = format!("{name}({})", vars.join(", "));
            let select: Vec<&str> = vars.iter().map(String::as_str).collect();

            let mut goal = parse_goal_with_select(&body, &select).expect("goal parses");
            resolve_goal(&mut goal, checked.registry());
            check_goal(&goal, checked).expect("goal validates");

            let rewritten = rewrite_program(checked, &goal);
            let emitted = NemoProgram::emit(&rewritten).expect("emits");
            emitted.audit().expect("audit passes");
            let result = run_to_fixpoint(&rewritten, &emitted, &tables, &builtins, 32)
                .expect("kernel rules converge");

            let mut rows: Vec<Vec<String>> = result
                .answer
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|v| v.as_sym().expect("kernel values are symbols").to_owned())
                        .collect()
                })
                .collect();
            rows.sort();
            rows.dedup();
            (name, rows)
        })
        .collect()
}

fn ident(prefix: &'static str) -> impl Strategy<Value = String> {
    (0u8..4).prop_map(move |i| format!("{prefix}{i}"))
}

fn pairs(a: &'static str, b: &'static str) -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::vec((ident(a), ident(b)), 0..4)
}

fn triples(
    a: &'static str,
    b: &'static str,
    c: &'static str,
) -> impl Strategy<Value = Vec<(String, String, String)>> {
    prop::collection::vec((ident(a), ident(b), ident(c)), 0..3)
}

fn singles(a: &'static str) -> impl Strategy<Value = Vec<(String,)>> {
    prop::collection::vec(ident(a), 0..3).prop_map(|v| v.into_iter().map(|x| (x,)).collect())
}

/// A random transaction over a small, overlapping identifier pool -- small
/// enough that joins and negations actually fire, which is the entire point
/// of a differential test over disjoint-by-construction random strings.
fn arb_facts() -> impl Strategy<Value = Facts> {
    (
        (
            pairs("r", "c"),        // ref_create
            triples("r", "c", "c"), // ref_advance
            pairs("c", "c"),        // parent
            pairs("c", "k"),        // signed_by
            singles("k"),           // member
        ),
        (
            singles("k"),    // admin
            pairs("c", "t"), // anchor_blob
            pairs("c", "t"), // context_blob
            singles("r"),    // effect_ref
            singles("t"),    // object_exists
        ),
    )
        .prop_map(
            |(
                (ref_create, ref_advance, parent, signed_by, member),
                (admin, anchor_blob, context_blob, effect_ref, object_exists),
            )| Facts {
                ref_create,
                ref_advance,
                parent,
                signed_by,
                member,
                admin,
                anchor_blob,
                context_blob,
                effect_ref,
                object_exists,
            },
        )
}

proptest! {
    // 32 cases, each running the interpreted path's seven Nemo executions
    // fresh: `engine.rs` documents the process-global timing mutex Nemo's
    // `execute()` enters, so this crate's tests -- like every other crate
    // that touches `gix-query-eval` -- must run with `--test-threads=1`.
    // Keeping the case count small bounds wall time under that constraint.
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn ascent_and_interpreted_kernels_agree(facts in arb_facts()) {
        let checked = checked_kernel();
        let ascent = violations(facts.clone());
        let interpreted = interpreted_violations(&checked, &facts);
        prop_assert_eq!(ascent, interpreted);
    }
}
