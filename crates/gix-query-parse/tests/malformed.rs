//! A corpus of malformed inputs, each asserted against a specific
//! diagnostic rather than merely "it errors".

use gix_query_ir::PredicateKey;
use gix_query_parse::{ParseError, parse_module};

fn errors(src: &str) -> Vec<ParseError> {
    parse_module("t", src).expect_err("expected a parse failure")
}

fn has(errs: &[ParseError], pred: impl Fn(&ParseError) -> bool) -> bool {
    errs.iter().any(pred)
}

#[test]
fn unterminated_string() {
    let errs = errors("p(\"abc).\n");
    assert!(
        has(&errs, |e| matches!(
            e,
            ParseError::UnterminatedString { .. }
        )),
        "{errs:?}"
    );
}

#[test]
fn missing_period() {
    let errs = errors("p(X) :- q(X)\n");
    assert!(
        has(&errs, |e| matches!(
            e,
            ParseError::UnexpectedEof { expected, .. } if expected == "'.'"
        )),
        "{errs:?}"
    );
}

#[test]
fn magic_prefixed_predicate_is_reserved() {
    let errs = errors("magic_foo(X).\n");
    assert!(
        has(&errs, |e| matches!(
            e,
            ParseError::ReservedPrefix { key, .. } if *key == PredicateKey::new("magic_foo", 1)
        )),
        "{errs:?}"
    );
}

#[test]
fn a_literal_nul_byte_in_a_quoted_constant_is_rejected() {
    let src = "p(\"a\0b\").\n";
    let errs = errors(src);
    assert!(
        has(&errs, |e| matches!(e, ParseError::NulInString { .. })),
        "{errs:?}"
    );
}

#[test]
fn a_nul_escape_in_a_quoted_constant_is_rejected() {
    let src = "p(\"a\\0b\").\n";
    let errs = errors(src);
    assert!(
        has(&errs, |e| matches!(e, ParseError::NulInString { .. })),
        "{errs:?}"
    );
}

#[test]
fn unbalanced_parens() {
    let errs = errors("p(X.\n");
    assert!(
        has(&errs, |e| matches!(
            e,
            ParseError::UnexpectedToken { expected, .. } if expected == "')'"
        )),
        "{errs:?}"
    );
}

#[test]
fn a_comparison_missing_its_right_operand() {
    let errs = errors("p(X) :- X = .\n");
    assert!(
        has(&errs, |e| matches!(
            e,
            ParseError::UnexpectedToken { expected, .. } if expected == "a term"
        )),
        "{errs:?}"
    );
}

#[test]
fn pub_on_an_undefined_predicate() {
    let errs = errors("pub foo(X).\n");
    assert!(
        has(&errs, |e| matches!(
            e,
            ParseError::PubUndefined { key, .. } if *key == PredicateKey::new("foo", 1)
        )),
        "{errs:?}"
    );
}
