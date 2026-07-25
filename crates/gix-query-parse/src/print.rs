//! IR to text: the pretty-printer.
//!
//! Printing is the parser's inverse modulo spans: [`parse_module`] applied
//! to [`print_module`]'s output reconstructs a [`Module`] with the same
//! exports and the same rules, because a printed value is always chosen so
//! that the parser reads it back the same way it would read hand-written
//! source spelling the same constant. Constants are the only place this
//! matters -- [`Term::Var`] and [`Term::Anon`] have exactly one spelling
//! each, but a [`Value::Sym`] must be printed bare when the bare grammar can
//! spell it and quoted, with escapes, when it cannot.
//!
//! [`parse_module`]: crate::parse_module

use gix_query_ir::{Atom, CompareOp, Export, Goal, Literal, Module, Rule, Term, Value};

/// Print `module`'s `pub` surface and rules as `.dl` source text.
///
/// The module's name is not printed: source text never declares it, the
/// same way [`crate::parse_module`] takes it as a parameter rather than
/// reading it from `source`.
#[must_use]
pub fn print_module(module: &Module) -> String {
    let mut out = String::new();
    for (i, export) in module.exports.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        write_export(&mut out, export);
    }
    if !module.exports.is_empty() && !module.rules.is_empty() {
        out.push_str("\n\n");
    }
    for (i, rule) in module.rules.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        write_rule(&mut out, rule);
    }
    out
}

/// Print one rule, or fact, as `head :- body.` or `head.`.
#[must_use]
pub fn print_rule(rule: &Rule) -> String {
    let mut out = String::new();
    write_rule(&mut out, rule);
    out
}

/// Print a goal body: a comma-separated literal list, with no terminating
/// period -- the one syntactic difference from a rule body.
///
/// `select` has no spelling in goal source and is not printed; it travels
/// beside the goal as `--select`, supplied separately at every tier.
#[must_use]
pub fn print_goal(goal: &Goal) -> String {
    let mut out = String::new();
    write_body(&mut out, &goal.body);
    out
}

fn write_export(out: &mut String, export: &Export) {
    out.push_str("pub ");
    out.push_str(&export.key.name);
    out.push('(');
    for i in 0..export.key.arity {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&column_name(i));
    }
    out.push_str(").");
}

/// A legal variable name for `pub` declaration argument `i`.
///
/// [`Export`] does not carry the names its declaration was written with --
/// only arity is meaningful, per the grammar note that a `pub` line's
/// argument names are documentation -- so printing has to synthesize
/// something. `A`, `B`, `C`, ... reads closer to the reference vocabulary's
/// own `pub` lines than a positional `Col0` would.
fn column_name(i: usize) -> String {
    let letter = (b'A' + (i % 26) as u8) as char;
    if i < 26 {
        letter.to_string()
    } else {
        format!("{letter}{}", i / 26)
    }
}

fn write_rule(out: &mut String, rule: &Rule) {
    write_atom(out, &rule.head);
    if rule.body.is_empty() {
        out.push('.');
        return;
    }
    out.push_str(" :- ");
    write_body(out, &rule.body);
    out.push('.');
}

fn write_body(out: &mut String, body: &[Literal]) {
    for (i, lit) in body.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_literal(out, lit);
    }
}

fn write_literal(out: &mut String, lit: &Literal) {
    match lit {
        Literal::Positive(a) | Literal::Builtin(a) => write_atom(out, a),
        Literal::Negated(a) => {
            out.push('!');
            write_atom(out, a);
        }
        Literal::Compare(c) => {
            write_term(out, &c.left.term);
            out.push(' ');
            out.push_str(op_str(c.op));
            out.push(' ');
            write_term(out, &c.right.term);
        }
    }
}

fn op_str(op: CompareOp) -> &'static str {
    op.as_str()
}

fn write_atom(out: &mut String, atom: &Atom) {
    out.push_str(&atom.predicate.name);
    out.push('(');
    for (i, t) in atom.terms.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_term(out, &t.term);
    }
    out.push(')');
}

fn write_term(out: &mut String, term: &Term) {
    match term {
        Term::Var(v) => out.push_str(v.as_str()),
        Term::Anon => out.push('_'),
        Term::Const(Value::Int(n)) => out.push_str(&n.to_string()),
        Term::Const(Value::Sym(s)) => write_symbol(out, s),
    }
}

fn write_symbol(out: &mut String, s: &str) {
    if is_bare_symbol(s) {
        out.push_str(s);
        return;
    }
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            // A well-formed `Value::Sym` never carries a NUL byte -- the
            // parser rejects one at the front door -- so there is no third
            // case to escape here; this printer assumes it is only ever
            // handed IR the parser could have produced.
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Whether `s` can be spelled as a bare constant: `[a-z][A-Za-z0-9_]*`, or a
/// digit-led word that is not all digits.
///
/// An all-digit string is excluded even though the character class would
/// otherwise admit it: the lexer classifies a digit-led, all-digit token as
/// [`Value::Int`], so printing an all-digit `Value::Sym` bare would re-parse
/// as the wrong variant. It must be quoted instead.
fn is_bare_symbol(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    !s.bytes().all(|b| b.is_ascii_digit())
}
