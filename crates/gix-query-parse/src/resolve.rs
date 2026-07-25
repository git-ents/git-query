//! Predicate resolution: reclassify [`Literal::Positive`] atoms the registry
//! declares as builtins into [`Literal::Builtin`].
//!
//! The parser cannot do this itself -- it has no registry, and by design:
//! the grammar draws no syntactic line between calling a builtin and calling
//! a locally-defined predicate, so which is which can only be answered
//! against a [`Registry`] snapshot. [`literal.rs`](gix_query_ir) documents
//! the same split from the IR side. Run this before validation, and before
//! [`gix_query_ir::Canonicalizer`], which is only defined on resolved IR --
//! an unresolved and a resolved program canonicalize differently on
//! purpose, so skipping this step is a silent wrong-cache-entry bug, not a
//! type error.

use gix_query_ir::{Goal, Literal, Module, Registry, Rule};

/// Reclassify every builtin call in `module`'s rule bodies.
///
/// Only body literals are touched: a rule's head is always
/// [`gix_query_ir::Atom`], never a [`Literal`], so there is nothing to
/// reclassify there.
pub fn resolve_module(module: &mut Module, registry: &Registry) {
    for rule in &mut module.rules {
        resolve_rule(rule, registry);
    }
}

/// Reclassify every builtin call in one rule's body.
pub fn resolve_rule(rule: &mut Rule, registry: &Registry) {
    resolve_body(&mut rule.body, registry);
}

/// Reclassify every builtin call in a goal's body.
pub fn resolve_goal(goal: &mut Goal, registry: &Registry) {
    resolve_body(&mut goal.body, registry);
}

/// Reclassify every [`Literal::Positive`] in `body` that names a registered
/// builtin.
///
/// [`Literal::Compare`] is untouched: the parser already emits it straight
/// from infix syntax (`A = B`, not `=(A, B)`), so there is nothing
/// ambiguous to reclassify -- only a named call such as `bind(...)` could be
/// either a builtin or a locally-defined predicate until resolution runs.
pub fn resolve_body(body: &mut Vec<Literal>, registry: &Registry) {
    for lit in std::mem::take(body) {
        let resolved = match lit {
            Literal::Positive(atom) if registry.is_builtin(&atom.predicate) => {
                Literal::Builtin(atom)
            }
            other => other,
        };
        body.push(resolved);
    }
}
