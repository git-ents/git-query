//! Test-only helpers shared across every pass's fixtures.

#![cfg(test)]

use gix_query_ir::{Module, Program, Rule};

/// Add `rule` to `program` as the sole rule of a throwaway module named
/// `module`.
///
/// `Program::push_rule` alone does not update [`Program::defined`] --
/// correctly, since that is also how the magic-set rewrite appends generated
/// rules without the whole-program bookkeeping treating them as newly
/// authored predicates. Fixtures built directly from [`Rule`] values need the
/// bookkeeping, so they go through [`Program::add_module`] instead, wrapped
/// here so every pass's tests build programs the same way.
pub(crate) fn add(program: &mut Program, rule: Rule, module: &str) {
    let mut m = Module::new(module);
    m.rules.push(rule);
    program.add_module(m);
}
