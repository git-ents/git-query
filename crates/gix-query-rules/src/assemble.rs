//! Whole-program assembly: every stored module, qualified into one flat
//! namespace and checked together.
//!
//! Validation happens here -- at assembly time, which is read/query time,
//! never at push time. Two independently-valid modules can still compose
//! into a negation cycle or an arity clash that neither module's own
//! contents reveal, so there is no way to make a single-module push safe by
//! construction; the whole program has to be re-checked whenever it is
//! about to be used, and a composition error surfaces as a query error
//! rather than blocking the write that caused it.

use std::sync::Arc;

use gix_query_check::check;
use gix_query_ir::{CheckedProgram, Module, PredicateKey, Program, Registry};

use crate::error::{Error, join};
use crate::store::RuleStore;

/// The name every program is assembled under for [`CORE_MODULE_SOURCE`] --
/// reserved, not a name any `RuleStore`-published module is expected to
/// collide with.
const CORE_MODULE_NAME: &str = "gix_query_core";

/// `bind/5`'s own definition (ARCHITECTURE.md, "git-query"): the aggregation
/// stratum over `cand/5`, expressed in the rule language rather than in
/// `gix-query-host`'s Rust, exactly as the doc writes it --
///
/// ```text
/// bind(A, Rev, Loc, O, C) :- cand(A, Rev, Loc, O, C),
///                            C = max{ C' : cand(A, Rev, _, _, C') }.
/// ```
///
/// `max{}` itself has no lowering path (`gix-query-eval`'s own module doc:
/// "the IR has no term-level operations, so arithmetic and aggregates have
/// no lowering path at all") -- this is the standard stratified-negation
/// encoding of argmax instead, needing nothing beyond what the engine
/// already runs: a `>` comparison (numeric, since `cand`'s confidence column
/// is a plain `Int`) and stratified negation. `dominated` is `bind`'s own
/// private helper, qualified to `gix_query_core.dominated` by [`qualify`]
/// like any other module-local predicate; `bind` itself is pre-declared
/// `pub` IDB in [`Registry::host`], so it resolves bare here instead of
/// picking up the same qualification.
const CORE_MODULE_SOURCE: &str = "
pub bind(A, Rev, Loc, O, C).
dominated(A, Rev, C) :- cand(A, Rev, _, _, C), cand(A, Rev, _, _, C2), C2 > C.
bind(A, Rev, Loc, O, C) :- cand(A, Rev, Loc, O, C), !dominated(A, Rev, C).
";

/// Parse, qualify, resolve, and check every module `store` currently
/// publishes, together with [`CORE_MODULE_SOURCE`], against `registry`.
///
/// `bind/5` is always present, in every assembled program, whether or not
/// any rule module is ever published: it is core substrate
/// (ARCHITECTURE.md's "only user-facing resolution predicate"), not
/// something a repository opts into the way a review policy is.
///
/// # Errors
/// A module's source failing to parse, or the assembled program failing any
/// of the eight validation passes.
pub fn assemble(store: &RuleStore<'_>, registry: &Registry) -> Result<CheckedProgram, Error> {
    let mut program = Program::new();

    let mut core = gix_query_parse::parse_module(CORE_MODULE_NAME, CORE_MODULE_SOURCE)
        .expect("CORE_MODULE_SOURCE is fixed, valid source, pinned by this crate's own tests");
    gix_query_parse::resolve_module(&mut core, registry);
    qualify(&mut core, registry);
    program.add_module(core);

    for name in store.list()? {
        let source = store
            .get(&name)?
            .expect("just listed, so it exists to read");
        let mut module =
            gix_query_parse::parse_module(name.clone(), &source).map_err(|errs| Error::Parse {
                name: name.clone(),
                detail: join(&errs),
            })?;
        gix_query_parse::resolve_module(&mut module, registry);
        qualify(&mut module, registry);
        program.add_module(module);
    }
    check(&program, registry).map_err(|errs| Error::Check(join(&errs)))
}

/// Qualify `module`'s own predicates by its name, leaving alone anything
/// that already resolves against `registry` (host EDB, a builtin) and
/// anything the author already wrote qualified (a cross-module reference,
/// e.g. `review.reviewed`).
///
/// A module's source text never qualifies its own predicates -- see
/// [`gix_query_ir::PredicateKey`]'s doc comment -- so this is the one place
/// that turns "unqualified name local to this module" into the qualified
/// name every other module, and the checker, sees. Skipping this step would
/// not fail loudly: two modules that each define a same-named local helper
/// would silently collide into one predicate, each contributing rules to the
/// other's definition.
fn qualify(module: &mut Module, registry: &Registry) {
    let should_qualify = |key: &PredicateKey| -> bool {
        key.module().is_none() && registry.predicate(key).is_none() && !registry.is_builtin(key)
    };
    let qualified = |module_name: &Arc<str>, key: &PredicateKey| -> PredicateKey {
        PredicateKey::new(format!("{module_name}.{}", key.name), key.arity)
    };

    for export in &mut module.exports {
        if should_qualify(&export.key) {
            export.key = qualified(&module.name, &export.key);
        }
    }
    for rule in &mut module.rules {
        if should_qualify(&rule.head.predicate) {
            rule.head.predicate = qualified(&module.name, &rule.head.predicate);
        }
        for literal in &mut rule.body {
            let Some(atom) = literal.atom_mut() else {
                continue;
            };
            if should_qualify(&atom.predicate) {
                atom.predicate = qualified(&module.name, &atom.predicate);
            }
        }
    }
}
