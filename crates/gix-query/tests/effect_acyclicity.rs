//! The exported fork-bomb check, exercised the way `gix-effect` will call it
//! at `git effect define` time: a set of registered effect declarations, each
//! with the rule-language bodies whose footprints are its read set.
//!
//! No repository is involved. The check is static analysis over a
//! `CheckedProgram` and a registry, which is exactly why it can run before
//! anything is written.

use gix_query::{
    EffectCheckError, EffectDecl, EffectUnderCheck, Namespace, check_effect_acyclicity,
};
use gix_query_ir::{Program, Registry};

fn empty_program() -> gix_query_ir::CheckedProgram {
    gix_query_check::check(&Program::new(), &Registry::host()).expect("an empty program validates")
}

fn effect(name: &str, writes: &[&str], bodies: &[&str]) -> EffectUnderCheck {
    EffectUnderCheck {
        decl: EffectDecl::new(
            name,
            // A deliberately dishonest declared read set: nothing consults it.
            vec![Namespace::new("refs/nothing")],
            writes.iter().map(|w| Namespace::new(*w)).collect(),
        ),
        bodies: bodies.iter().map(|b| (*b).to_owned()).collect(),
    }
}

/// The point of the whole exercise: an effect that writes into the namespace
/// its own trigger body reads is a fork bomb, however its declaration is
/// written. `signer/2` is claim-backed, so the body's footprint is
/// `refs/meta/claims/*`, and a write under that prefix closes the loop.
#[test]
fn an_effect_reading_its_own_write_set_is_rejected_naming_the_namespace() {
    let checked = empty_program();
    let effects = vec![effect(
        "countersign",
        &["refs/meta/claims/action/countersign/*"],
        &["signer(C, M)"],
    )];

    let errors = check_effect_acyclicity(&checked, &effects).expect_err("a self cycle");
    assert_eq!(errors.len(), 1, "{errors:?}");
    match &errors[0] {
        EffectCheckError::Cycle {
            from,
            to,
            namespace,
        } => {
            assert_eq!(&**from, "countersign");
            assert_eq!(&**to, "countersign");
            assert_eq!(namespace.as_str(), "refs/meta/claims/action/countersign/*");
            // The rendered diagnostic names it too -- a caller printing the
            // error should not have to reach into the fields.
            assert!(
                errors[0]
                    .to_string()
                    .contains("refs/meta/claims/action/countersign/*"),
                "{}",
                errors[0]
            );
        }
        other => panic!("expected a cycle, got {other:?}"),
    }
}

/// Two effects chained through claims, the second writing somewhere no
/// footprint reaches: worked example 3's shape, and it must stay accepted.
#[test]
fn a_chain_that_terminates_is_accepted() {
    let checked = empty_program();
    let effects = vec![
        effect(
            "build_on_push",
            &["refs/meta/claims/action/kiln/*"],
            &["member(M)"],
        ),
        effect(
            "release_on_green",
            &["refs/release/candidate"],
            &["signer(C, M)"],
        ),
    ];
    check_effect_acyclicity(&checked, &effects).expect("no cycle");
}

/// An effect whose footprint overlaps nobody's write set -- including its own
/// -- is accepted. The declared read set here overlaps nothing either, but
/// that is a coincidence: what makes this pass is that `refs/meta/claims/*`
/// and `refs/release/candidate` do not overlap.
#[test]
fn a_footprint_overlapping_no_write_set_is_accepted() {
    let checked = empty_program();
    let effects = vec![effect(
        "lonely",
        &["refs/release/candidate"],
        &["signer(C, M)"],
    )];
    check_effect_acyclicity(&checked, &effects).expect("no cycle");
}

/// A body that does not parse is reported as such, named, rather than
/// contributing an empty read set and quietly passing.
#[test]
fn an_unparsable_body_is_reported_not_ignored() {
    let checked = empty_program();
    let effects = vec![effect("broken", &["refs/x"], &["signer(C,"])];
    let errors = check_effect_acyclicity(&checked, &effects).expect_err("a parse error");
    assert!(
        matches!(&errors[0], EffectCheckError::Parse { effect, .. } if effect == "broken"),
        "{errors:?}"
    );
}

/// A body naming a predicate nothing declares fails validation, again named.
#[test]
fn a_body_that_fails_validation_is_reported() {
    let checked = empty_program();
    let effects = vec![effect("bogus", &["refs/x"], &["no_such_predicate(X)"])];
    let errors = check_effect_acyclicity(&checked, &effects).expect_err("a validation error");
    assert!(
        matches!(&errors[0], EffectCheckError::Body { effect, .. } if effect == "bogus"),
        "{errors:?}"
    );
}
