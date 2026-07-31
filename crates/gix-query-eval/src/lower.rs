//! Nemo lowering, and the emission audit.
//!
//! Two types, and the type system is the whole argument: [`NemoProgram`]
//! holds rule text and is built only from rules; [`NemoTables`] holds fact
//! rows and is built only from tuples. There is no function anywhere in this
//! module, or this crate, that turns a fact into program text -- that is
//! Gate A1's finding that ten thousand facts cost 1.7s as inline text and
//! 6.5ms through an in-memory resource provider, for identical evaluation.
//!
//! # Quoting, not escaping
//!
//! Every symbol that reaches program text goes through [`crate::Symbol`],
//! which refuses a `"` at construction. Nothing here escapes anything, because
//! Nemo's string lexer does no unescaping on the way back.
//!
//! # Variables
//!
//! Every [`Term::Var`] is rendered `?`-prefixed. A bare identifier is a
//! constant to Nemo -- an abstract IRI, not a variable -- and a lowering that
//! forgot the `?` would produce a program that parses, validates, runs, and
//! silently derives nothing. There is exactly one place in this crate that
//! renders a variable ([`render_term`]), so there is exactly one place this
//! mistake could be made.

use std::collections::BTreeMap;
use std::fmt;

use gix_query_ir::{Atom, Literal, PredicateKey, Rule, Term, Value};

use crate::error::EvalError;
use crate::rewrite::LoweredProgram;
use crate::symbol::Symbol;

/// Validated Nemo program text, built only from [`gix_query_ir::Rule`]s that
/// themselves trace back to a [`gix_query_ir::CheckedProgram`] through
/// [`crate::rewrite::rewrite`].
///
/// There is no `From<&str>` and no public constructor other than
/// [`NemoProgram::emit`]: the text in here either came from our own renderer,
/// or this type could not have been built.
#[derive(Debug, Clone)]
pub struct NemoProgram {
    text: String,
    /// One `(name, arity)` per rule with a non-empty body, in emission order.
    rule_signatures: Vec<(String, usize)>,
    /// One `(name, arity)` per bodyless rule (a fact), in emission order.
    ///
    /// Kept apart from `rule_signatures` because Nemo's own parser does not
    /// consider a fact a rule at all -- it becomes a `Statement::Fact`, not a
    /// `Statement::Rule` -- so a single combined count would make the audit
    /// report every program containing a magic seed or a `carries(...).`-style
    /// fact as missing rules that were never rules to begin with.
    fact_signatures: Vec<(String, usize)>,
}

impl NemoProgram {
    /// Render every rule in `program.rules` to Nemo syntax.
    ///
    /// # Errors
    /// Fails if any constant along the way contains a `"`.
    pub fn emit(program: &LoweredProgram) -> Result<Self, EvalError> {
        let rules = program.rules.rules();
        let mut text = String::new();
        let mut rule_signatures = Vec::new();
        let mut fact_signatures = Vec::new();
        for rule in rules {
            let rendered = render_rule(rule)?;
            text.push_str(&rendered);
            text.push('\n');
            let signature = (
                rule.head.predicate.name.to_string(),
                rule.head.predicate.arity,
            );
            if rule.is_fact() {
                fact_signatures.push(signature);
            } else {
                rule_signatures.push(signature);
            }
        }
        Ok(Self {
            text,
            rule_signatures,
            fact_signatures,
        })
    }

    /// Parse this program's own text back through Nemo's parser and check
    /// that the recovered program has exactly the rule count, exactly the
    /// fact count, and exactly the predicate names and arities in emission
    /// order for each, that we lowered.
    ///
    /// This is the check that answers "do we have the subset we support" for
    /// *this* program -- it catches the class of emitter bug types cannot: a
    /// symbol that terminated a literal early, a name that collided, a rule
    /// that silently vanished. It runs every time, immediately before
    /// execution.
    ///
    /// # Errors
    /// [`EvalError::EmissionAudit`] if Nemo rejects the text, if the rule or
    /// fact count differs, or if any signature differs.
    pub fn audit(&self) -> Result<(), EvalError> {
        use gix_query_ir_nemo_shim::ProgramReadExt as _;

        let parsed = nemo::api::load_program(self.text.clone(), "gix-query-eval".to_owned())
            .map_err(|report| EvalError::EmissionAudit(format!("{report:?}")))?;
        let recovered_rules = parsed.rule_signatures();
        if recovered_rules != self.rule_signatures {
            return Err(EvalError::EmissionAudit(format!(
                "lowered {} rules with signatures {:?}, but Nemo's own parser recovered {} with {:?}",
                self.rule_signatures.len(),
                self.rule_signatures,
                recovered_rules.len(),
                recovered_rules,
            )));
        }
        let recovered_facts = parsed.fact_signatures();
        if recovered_facts != self.fact_signatures {
            return Err(EvalError::EmissionAudit(format!(
                "lowered {} facts with signatures {:?}, but Nemo's own parser recovered {} with {:?}",
                self.fact_signatures.len(),
                self.fact_signatures,
                recovered_facts.len(),
                recovered_facts,
            )));
        }
        Ok(())
    }
}

/// A thin extension trait over Nemo's `ProgramRead`, kept in its own module
/// so the `use` bringing Nemo's own trait into scope is visibly scoped to the
/// one function that needs it, rather than importing it crate-wide.
mod gix_query_ir_nemo_shim {
    use nemo::rule_model::programs::ProgramRead;

    pub(crate) trait ProgramReadExt {
        fn rule_signatures(&self) -> Vec<(String, usize)>;
        fn fact_signatures(&self) -> Vec<(String, usize)>;
    }

    impl<P: ProgramRead> ProgramReadExt for P {
        fn rule_signatures(&self) -> Vec<(String, usize)> {
            self.rules()
                .map(|r| {
                    let heads = r.head();
                    debug_assert_eq!(
                        heads.len(),
                        1,
                        "our rules always have exactly one head atom"
                    );
                    let atom = &heads[0];
                    (atom.predicate().name().to_owned(), atom.len())
                })
                .collect()
        }

        fn fact_signatures(&self) -> Vec<(String, usize)> {
            self.facts()
                .map(|f| (f.predicate().name().to_owned(), f.terms().count()))
                .collect()
        }
    }
}

/// Facts, as tables -- never as program text.
///
/// The only route from a [`Value`] tuple into an evaluation is through here:
/// [`crate::engine`] serializes each table as CSV served from memory by a
/// [`nemo::io::resource_providers::ResourceProvider`], referenced by an
/// `@import` directive the engine writes fresh every round. Nothing in this
/// type, or anything that constructs one, can produce a byte of rule text.
#[derive(Debug, Clone, Default)]
pub struct NemoTables(BTreeMap<PredicateKey, Vec<Vec<Value>>>);

impl NemoTables {
    /// An empty table set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add rows to `key`'s table, creating it if absent.
    pub fn insert(&mut self, key: PredicateKey, rows: impl IntoIterator<Item = Vec<Value>>) {
        self.0.entry(key).or_default().extend(rows);
    }

    /// Every table, key order.
    pub fn iter(&self) -> impl Iterator<Item = (&PredicateKey, &[Vec<Value>])> {
        self.0.iter().map(|(k, v)| (k, v.as_slice()))
    }

    /// Whether any facts have been recorded for `key`.
    #[must_use]
    pub fn contains(&self, key: &PredicateKey) -> bool {
        self.0.contains_key(key)
    }
}

/// Render one rule as `head :- body .` or, for a fact, `head .`.
fn render_rule(rule: &Rule) -> Result<String, EvalError> {
    let head = render_atom(&rule.head)?;
    if rule.body.is_empty() {
        return Ok(format!("{head} ."));
    }
    let mut parts = Vec::with_capacity(rule.body.len());
    for lit in &rule.body {
        parts.push(render_literal(lit)?);
    }
    Ok(format!("{head} :- {} .", parts.join(", ")))
}

fn render_literal(lit: &Literal) -> Result<String, EvalError> {
    Ok(match lit {
        Literal::Positive(a) | Literal::Builtin(a) => render_atom(a)?,
        Literal::Negated(a) => format!("~{}", render_atom(a)?),
        Literal::Compare(c) => format!(
            "{} {} {}",
            render_term(&c.left.term)?,
            c.op.as_str(),
            render_term(&c.right.term)?,
        ),
    })
}

/// Render a predicate name as a Nemo identifier.
///
/// A bare Nemo `Name` token is an ASCII letter followed by letters, digits,
/// `_`, or `%` -- never `.` -- so a module-qualified name such as
/// `review.reviewed` (see `gix_query_ir::PredicateKey`'s doc comment) cannot
/// be written as a bare identifier at all. Nemo's grammar has a second, more
/// general predicate-tag form for exactly this: `<...>`, an IRI reference,
/// whose content becomes the resolved tag name verbatim -- `nemo`'s own
/// `resolve_tag` returns an `Iri` tag's content completely unchanged, with
/// no base or prefix expansion applied unlike its other two tag forms. That
/// makes it an exact, lossless carrier for any name our own grammar
/// produces, so only a name a bare identifier cannot express pays for the
/// brackets; every other name keeps emitting the plain form untouched.
pub(crate) fn render_predicate_name(name: &str) -> String {
    if is_bare_nemo_name(name) {
        name.to_owned()
    } else {
        format!("<{name}>")
    }
}

/// Whether `name` is a legal bare Nemo `Name` token on its own.
fn is_bare_nemo_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '%')
}

fn render_atom(atom: &Atom) -> Result<String, EvalError> {
    let mut out = String::new();
    out.push_str(&render_predicate_name(&atom.predicate.name));
    out.push('(');
    for (i, t) in atom.terms.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&render_term(&t.term)?);
    }
    out.push(')');
    Ok(out)
}

/// Render one term. The only place a [`Term::Var`] becomes `?name` -- see the
/// module doc for why that prefix is the single most important byte this
/// crate emits.
fn render_term(term: &Term) -> Result<String, EvalError> {
    Ok(match term {
        Term::Var(v) => format!("?{}", v.as_str()),
        // Nemo has its own native anonymous-variable syntax: a bare `_`,
        // with no name, parses as a fresh anonymous variable at every
        // occurrence (`nemo::parser::ast::expression::basic::variable`'s
        // `VariableType::Anonymous`, name optional). That is exactly our
        // semantics -- `_` occurs exactly once by construction -- so no
        // synthesized name is needed at all. A prior version minted
        // `?__anon{n}`, which Nemo's own variable-name grammar rejects
        // outright (a name must start with a letter, never `_`), so every
        // rule containing an anonymous term failed at the emission audit.
        Term::Anon => "_".to_owned(),
        Term::Const(v) => value_literal(v)?,
    })
}

/// Render a ground value as a Nemo term -- shared by rule emission and
/// [`crate::trace`], which has to write a ground query fact in the same
/// syntax to ask Nemo to trace it.
pub(crate) fn value_literal(v: &Value) -> Result<String, EvalError> {
    Ok(match v {
        Value::Int(n) => n.to_string(),
        Value::Sym(s) => Symbol::new(s.clone())?.to_nemo_literal(),
    })
}

impl fmt::Display for NemoProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gix_query_ir::{
        CheckedProgram, Goal, PredicateEntry, Program, Registry, Stratification, Var, Visibility,
    };
    use std::collections::BTreeMap as Map;

    fn pos(name: &str, vars: &[&str]) -> Literal {
        Literal::Positive(Atom::new(
            name,
            vars.iter().map(|v| Term::var(*v)).collect(),
        ))
    }

    fn trivial_rewritten() -> LoweredProgram {
        let mut registry = Registry::host();
        registry
            .insert_predicate(PredicateEntry::idb(
                PredicateKey::new("p", 1),
                Visibility::Public,
            ))
            .expect("insertable");
        let mut program = Program::new();
        program.push_rule(
            Rule::new(
                Atom::new("p", vec![Term::var("M")]),
                vec![pos("member", &["M"])],
            ),
            "test",
        );
        let checked = CheckedProgram::assemble(
            program,
            registry,
            Stratification::default(),
            Map::new(),
            Map::new(),
        );
        let goal = Goal::new(vec![pos("p", &["M"])], vec![Var::new("M")]);
        crate::rewrite::rewrite(&checked, &goal)
    }

    #[test]
    fn variables_are_question_mark_prefixed() {
        let text = render_rule(&Rule::new(
            Atom::new("p", vec![Term::var("X")]),
            vec![pos("q", &["X"])],
        ))
        .expect("renders");
        assert_eq!(text, "p(?X) :- q(?X) .");
    }

    #[test]
    fn symbols_are_always_quoted() {
        let text =
            render_rule(&Rule::fact(Atom::new("p", vec![Term::sym("same")]))).expect("renders");
        assert_eq!(text, "p(\"same\") .");
    }

    #[test]
    fn a_quote_in_a_constant_is_refused_not_mangled() {
        let err = render_rule(&Rule::fact(Atom::new("p", vec![Term::sym("has \" quote")])))
            .expect_err("must refuse, never escape");
        assert!(matches!(err, EvalError::QuoteInSymbol(_)));
    }

    #[test]
    fn negation_and_comparisons_render_natively() {
        let text = render_rule(&Rule::new(
            Atom::new("p", vec![Term::var("X")]),
            vec![
                pos("q", &["X"]),
                Literal::Negated(Atom::new("r", vec![Term::var("X")])),
            ],
        ))
        .expect("renders");
        assert_eq!(text, "p(?X) :- q(?X), ~r(?X) .");
    }

    #[test]
    fn an_anonymous_term_renders_as_nemos_own_bare_underscore() {
        // Regression: a prior version minted `?__anon{n}`, a name Nemo's own
        // variable grammar rejects (a name must start with a letter). Nemo's
        // `_` is native syntax for exactly this, so no synthesized name is
        // needed at all.
        let text = render_rule(&Rule::new(
            Atom::new("p", vec![Term::var("X")]),
            vec![Literal::Positive(Atom::new(
                "q",
                vec![Term::var("X"), Term::Anon],
            ))],
        ))
        .expect("renders");
        assert_eq!(text, "p(?X) :- q(?X, _) .");
    }

    #[test]
    fn a_fact_has_no_turnstile() {
        let text = render_rule(&Rule::fact(Atom::new("p", vec![Term::int(42)]))).expect("renders");
        assert_eq!(text, "p(42) .");
    }

    #[test]
    fn emission_matches_what_the_audit_recovers() {
        let rewritten = trivial_rewritten();
        let emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted
            .audit()
            .expect("a faithful emission passes its own audit");
    }

    #[test]
    fn a_corrupted_program_fails_its_own_audit() {
        // Deliberately drop a rule after emission -- exactly the "a rule
        // silently vanished" class the audit exists to catch, since nothing
        // in the type system stops a `String` from being truncated after the
        // fact.
        let rewritten = trivial_rewritten();
        let mut emitted = NemoProgram::emit(&rewritten).expect("emits");
        emitted.rule_signatures.push(("ghost".to_owned(), 3));
        let err = emitted
            .audit()
            .expect_err("a phantom signature must be caught");
        assert!(matches!(err, EvalError::EmissionAudit(_)));
    }

    #[test]
    fn a_truncated_program_text_fails_its_own_audit() {
        let rewritten = trivial_rewritten();
        let mut emitted = NemoProgram::emit(&rewritten).expect("emits");
        // Cut the text mid-rule so Nemo's parser recovers fewer rules than we
        // claim to have lowered.
        emitted.text.truncate(emitted.text.len() / 2);
        assert!(emitted.audit().is_err());
    }
}
