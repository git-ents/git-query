//! Assembling the working registry: the host registry plus one IDB entry per
//! predicate the program defines.
//!
//! This is prose for "base predicates are not redefinable" turned into code
//! that runs before anything else touches the registry -- a program that
//! tries to define `commit/1` must fail here, not silently acquire an IDB
//! entry that later shadows the host one in some other pass's lookup.

use gix_query_ir::{PredicateEntry, Program, Registry};

use crate::error::{CheckError, Errors};

/// The host registry extended with one IDB entry per predicate `program`
/// defines, skipping (and reporting) any that collide with a host predicate
/// or a builtin.
pub(crate) fn build(program: &Program, host: &Registry, errors: &mut Errors) -> Registry {
    let mut registry = host.clone();
    for key in program.defined() {
        if host.is_base(key) || host.is_builtin(key) {
            errors.push(CheckError::NotRedefinable { key: key.clone() });
            continue;
        }
        let entry = PredicateEntry::idb(key.clone(), program.visibility(key));
        if let Err(gix_query_ir::Error::ArityLimit { key, max }) = registry.insert_predicate(entry)
        {
            errors.push(CheckError::PredicateArityLimit { key, max });
        }
    }
    registry
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, Rule, Term};

    use super::*;

    #[test]
    fn a_derived_predicate_gets_an_idb_entry() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::fact(Atom::new("reviewed", vec![Term::sym("b1")])),
            "m",
        );
        let mut errors = Errors::new();
        let registry = build(&program, &Registry::host(), &mut errors);
        assert!(errors.is_empty());
        let entry = registry
            .predicate(&gix_query_ir::PredicateKey::new("reviewed", 1))
            .expect("registered");
        assert!(!entry.class.is_edb());
    }

    #[test]
    fn redefining_a_base_predicate_is_reported() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::fact(Atom::new("commit", vec![Term::sym("c1")])),
            "m",
        );
        let mut errors = Errors::new();
        build(&program, &Registry::host(), &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], CheckError::NotRedefinable { .. }));
    }
}
