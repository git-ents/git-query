//! Pass 2 -- predicate resolution.
//!
//! Every body predicate must be host EDB, a builtin, or defined somewhere in
//! the program; base predicates are not redefinable (checked earlier, while
//! [`crate::registry_build`] assembles the working registry, since by this
//! point the host entry has already won any collision); and a private
//! predicate resolves only within its own module.

use gix_query_ir::{Literal, Program, Registry, RuleId, Visibility};

use crate::error::{CheckError, Errors, Site};

/// Resolve every literal in every stored rule's body.
pub(crate) fn check(program: &Program, registry: &Registry, errors: &mut Errors) {
    for (i, rule) in program.rules().iter().enumerate() {
        let id = RuleId(i);
        let site = Site::Rule(id);
        let referrer = program.rule_module(id);
        check_body(&rule.body, program, registry, referrer, site, errors);
    }
}

/// Resolve every literal in a standalone goal body. A goal has no module of
/// its own, so every reference it makes must already be `pub` -- there is no
/// "same module" exemption for it.
pub(crate) fn check_goal_body(
    body: &[Literal],
    program: &Program,
    registry: &Registry,
    errors: &mut Errors,
) {
    check_body(body, program, registry, None, Site::Goal, errors);
}

fn check_body(
    body: &[Literal],
    program: &Program,
    registry: &Registry,
    referrer: Option<&str>,
    site: Site,
    errors: &mut Errors,
) {
    for lit in body {
        match lit {
            Literal::Compare(_) => {}
            Literal::Builtin(atom) => {
                if registry.builtin(&atom.predicate).is_none() {
                    errors.push(CheckError::UnresolvedPredicate {
                        key: atom.predicate.clone(),
                        site,
                        span: atom.span,
                    });
                }
            }
            Literal::Negated(atom) if registry.is_builtin(&atom.predicate) => {
                errors.push(CheckError::NegatedBuiltin {
                    key: atom.predicate.clone(),
                    site,
                    span: atom.span,
                });
            }
            Literal::Negated(atom) | Literal::Positive(atom) => {
                resolve(
                    &atom.predicate,
                    atom.span,
                    program,
                    registry,
                    referrer,
                    site,
                    errors,
                );
            }
        }
    }
}

fn resolve(
    key: &gix_query_ir::PredicateKey,
    span: gix_query_ir::Span,
    program: &Program,
    registry: &Registry,
    referrer: Option<&str>,
    site: Site,
    errors: &mut Errors,
) {
    if registry.is_builtin(key) {
        // A `Positive` literal naming a builtin means the caller has not run
        // predicate reclassification (`gix_query_parse::resolve_module`)
        // before calling us; accept it rather than insist on a specific
        // upstream step this crate does not otherwise depend on.
        return;
    }
    let Some(entry) = registry.predicate(key) else {
        match find_arity(key, registry, program) {
            Some(found) => errors.push(CheckError::ArityMismatch {
                key: key.clone(),
                found,
                site,
                span,
            }),
            None => errors.push(CheckError::UnresolvedPredicate {
                key: key.clone(),
                site,
                span,
            }),
        }
        return;
    };
    if entry.class.is_edb() {
        return;
    }
    let Some(referrer) = referrer else {
        // A goal has no module; it can only call `pub` surface.
        if program.visibility(key) != Visibility::Public {
            errors.push(CheckError::CrossModulePrivate {
                key: key.clone(),
                owner: owner_module(key, program).unwrap_or_default().to_owned(),
                referrer: String::new(),
                site,
                span,
            });
        }
        return;
    };
    let Some(owner) = owner_module(key, program) else {
        return;
    };
    if owner != referrer && program.visibility(key) != Visibility::Public {
        errors.push(CheckError::CrossModulePrivate {
            key: key.clone(),
            owner: owner.to_owned(),
            referrer: referrer.to_owned(),
            site,
            span,
        });
    }
}

fn owner_module<'p>(key: &gix_query_ir::PredicateKey, program: &'p Program) -> Option<&'p str> {
    let (id, _) = program.rules_for(key).next()?;
    program.rule_module(id)
}

/// Whether `key`'s name is registered at a different arity, for a more
/// precise diagnostic than "unresolved" when it is.
fn find_arity(
    key: &gix_query_ir::PredicateKey,
    registry: &Registry,
    program: &Program,
) -> Option<usize> {
    registry
        .predicates()
        .map(|e| &e.key)
        .chain(registry.builtins().map(|e| &e.key))
        .chain(program.defined())
        .find(|k| k.name == key.name && k.arity != key.arity)
        .map(|k| k.arity)
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{Atom, PredicateKey, Rule, Term};

    use super::*;
    use crate::error::Pass;

    fn rule(head: &str, arity: usize, body: Vec<Literal>) -> Rule {
        Rule::new(
            Atom::new(
                head,
                (0..arity).map(|i| Term::var(format!("V{i}"))).collect(),
            ),
            body,
        )
    }

    #[test]
    fn an_edb_call_resolves() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            rule(
                "reviewers",
                1,
                vec![Literal::Positive(Atom::new(
                    "member",
                    vec![Term::var("V0")],
                ))],
            ),
            "m",
        );
        let registry = Registry::host();
        let mut errors = Errors::new();
        check(&program, &registry, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn an_unknown_predicate_is_reported() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            rule(
                "x",
                1,
                vec![Literal::Positive(Atom::new("nope", vec![Term::var("V0")]))],
            ),
            "m",
        );
        let registry = Registry::host();
        let mut errors = Errors::new();
        check(&program, &registry, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].pass(), Pass::Resolution);
        assert!(matches!(errors[0], CheckError::UnresolvedPredicate { .. }));
    }

    #[test]
    fn calling_a_predicate_at_the_wrong_arity_is_an_arity_mismatch() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            rule(
                "x",
                1,
                vec![Literal::Positive(Atom::new(
                    "member",
                    vec![Term::var("V0"), Term::var("V1")],
                ))],
            ),
            "m",
        );
        let registry = Registry::host();
        let mut errors = Errors::new();
        check(&program, &registry, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            CheckError::ArityMismatch { found: 1, .. }
        ));
    }

    #[test]
    fn negating_a_builtin_is_rejected() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            rule(
                "x",
                1,
                vec![Literal::Negated(Atom::new(
                    "cand",
                    vec![
                        Term::var("A"),
                        Term::var("R"),
                        Term::var("L"),
                        Term::var("O"),
                        Term::var("C"),
                    ],
                ))],
            ),
            "m",
        );
        let registry = Registry::host();
        let mut errors = Errors::new();
        check(&program, &registry, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], CheckError::NegatedBuiltin { .. }));
    }

    #[test]
    fn a_private_predicate_is_invisible_across_modules() {
        let mut program = Program::new();
        crate::testutil::add(
            &mut program,
            Rule::fact(Atom::new("secret", vec![Term::sym("x")])),
            "a",
        );
        crate::testutil::add(
            &mut program,
            rule(
                "public_one",
                1,
                vec![Literal::Positive(Atom::new(
                    "secret",
                    vec![Term::var("V0")],
                ))],
            ),
            "b",
        );
        let host = Registry::host();
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(&program, &host, &mut errors);
        assert!(errors.is_empty());
        check(&program, &registry, &mut errors);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], CheckError::CrossModulePrivate { .. }));
    }

    #[test]
    fn an_exported_predicate_is_visible_across_modules() {
        let mut program = Program::new();
        let mut module_a = gix_query_ir::Module::new("a");
        module_a.exports.push(gix_query_ir::Export {
            key: PredicateKey::new("shared", 1),
            span: gix_query_ir::Span::synthetic(),
        });
        module_a
            .rules
            .push(Rule::fact(Atom::new("shared", vec![Term::sym("x")])));
        program.add_module(module_a);
        crate::testutil::add(
            &mut program,
            rule(
                "public_one",
                1,
                vec![Literal::Positive(Atom::new(
                    "shared",
                    vec![Term::var("V0")],
                ))],
            ),
            "b",
        );
        let host = Registry::host();
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(&program, &host, &mut errors);
        assert!(errors.is_empty());
        check(&program, &registry, &mut errors);
        assert!(errors.is_empty());
    }
}
