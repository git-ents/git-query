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

/// Parse, qualify, resolve, and check every module `store` currently
/// publishes, together, against `registry`.
///
/// # Errors
/// A module's source failing to parse, or the assembled program failing any
/// of the nine validation passes.
pub fn assemble(store: &RuleStore<'_>, registry: &Registry) -> Result<CheckedProgram, Error> {
    let mut program = Program::new();
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
