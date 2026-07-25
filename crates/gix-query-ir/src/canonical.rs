use std::collections::HashMap;

use crate::{Goal, Literal, Rule, Term, Value};

/// Append `bytes` as a length-prefixed, NUL-terminated field.
///
/// Length-prefixing rather than delimiting alone is the point. An identifier is
/// arbitrary text -- `--edb name=file` supplies base facts from a file, and
/// whatever escape rules the parser adopts are the parser's business -- so a
/// symbol containing a NUL could otherwise forge a field boundary and make two
/// semantically different goals canonicalize identically. That is a *wrong
/// cache hit*, the worst failure this key has. The same reasoning already
/// governs the footprint digest, which NUL-delimits so no refname can forge a
/// boundary; here the input is less trustworthy, so the guard is stronger.
pub(crate) fn push_delimited(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(bytes);
    out.push(0);
}

/// Renders IR into a canonical byte string, renumbering variables in
/// first-occurrence order.
///
/// Two goals share a cache entry iff they are alpha-equivalent *with identical
/// literal order*. `reviewed(B)` and `reviewed( X )` are the same query and
/// must hit the same entry, or the cache is a whitespace lottery -- but author
/// order is semantically load-bearing, since it is the strategy the magic-set
/// rewrite reads, so it must not be normalized away.
///
/// This lives beside the IR it serializes rather than in the CLI, because the
/// cache key and the program-identity snapshot both depend on it and neither
/// should reimplement it.
///
/// # Only defined on resolved IR
///
/// The parser emits [`Literal::Positive`] for every applied predicate, and
/// predicate resolution reclassifies the builtins. Those two forms canonicalize
/// differently, deliberately -- a resolved and an unresolved program are not
/// interchangeable. Canonicalize *after* resolution, always, or the CLI's goal
/// path and the rules path will disagree about the same query.
#[derive(Debug, Default)]
pub struct Canonicalizer {
    names: HashMap<String, u32>,
    next: u32,
    out: Vec<u8>,
}

impl Canonicalizer {
    /// A fresh canonicalizer with an empty variable numbering.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The canonical form of `goal`, including its projected columns -- two
    /// goals with the same body and different `--select` are different
    /// queries.
    #[must_use]
    pub fn goal(goal: &Goal) -> Vec<u8> {
        let mut c = Self::new();
        c.field(b"goal");
        c.body(&goal.body);
        c.field(b"select");
        for v in &goal.select {
            let n = c.number(v.as_str());
            c.field(format!("V{n}").as_bytes());
        }
        c.out
    }

    /// The canonical form of one rule.
    #[must_use]
    pub fn rule(rule: &Rule) -> Vec<u8> {
        let mut c = Self::new();
        c.field(b"head");
        c.literal(&Literal::Positive(rule.head.clone()));
        c.field(b"body");
        c.body(&rule.body);
        c.out
    }

    fn field(&mut self, bytes: &[u8]) {
        push_delimited(&mut self.out, bytes);
    }

    fn number(&mut self, name: &str) -> u32 {
        if let Some(n) = self.names.get(name) {
            return *n;
        }
        let n = self.next;
        self.next += 1;
        self.names.insert(name.to_owned(), n);
        n
    }

    fn body(&mut self, body: &[Literal]) {
        for l in body {
            self.literal(l);
        }
    }

    fn literal(&mut self, l: &Literal) {
        let tag: &[u8] = match l {
            Literal::Positive(_) => b"p",
            Literal::Negated(_) => b"n",
            Literal::Builtin(_) => b"b",
            Literal::Compare(_) => b"c",
        };
        let key = l.key();
        self.field(tag);
        self.field(key.name.as_bytes());
        self.field(key.arity.to_string().as_bytes());
        for t in l.terms() {
            self.term(t);
        }
        self.field(b";");
    }

    fn term(&mut self, t: &Term) {
        match t {
            Term::Var(v) => {
                let n = self.number(v.as_str());
                self.field(format!("V{n}").as_bytes());
            }
            // Each `_` is a distinct variable that occurs exactly once, so it
            // gets no number. A singleton named variable and `_` therefore
            // canonicalize differently even though they mean the same thing;
            // that is a missed cache hit, never a wrong one.
            Term::Anon => self.field(b"_"),
            Term::Const(Value::Int(n)) => {
                self.field(format!("i{n}").as_bytes());
            }
            Term::Const(Value::Sym(s)) => {
                let mut bytes = Vec::with_capacity(s.len() + 1);
                bytes.push(b's');
                bytes.extend_from_slice(s.as_bytes());
                self.field(&bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Atom, Var};

    fn goal(a: &str, b: &str, select: &str) -> Goal {
        Goal::new(
            vec![Literal::Positive(Atom::new(
                "reviewed",
                vec![Term::var(a), Term::var(b)],
            ))],
            vec![Var::new(select)],
        )
    }

    #[test]
    fn alpha_equivalent_goals_canonicalize_alike() {
        assert_eq!(
            Canonicalizer::goal(&goal("B", "M", "B")),
            Canonicalizer::goal(&goal("X", "Y", "X"))
        );
    }

    #[test]
    fn variable_identity_survives_renumbering() {
        // `reviewed(B, B)` is not `reviewed(B, M)`, whatever the names.
        assert_ne!(
            Canonicalizer::goal(&goal("B", "B", "B")),
            Canonicalizer::goal(&goal("B", "M", "B"))
        );
    }

    #[test]
    fn the_projection_is_part_of_the_query() {
        assert_ne!(
            Canonicalizer::goal(&goal("B", "M", "B")),
            Canonicalizer::goal(&goal("B", "M", "M"))
        );
    }

    #[test]
    fn literal_order_is_preserved_because_it_is_load_bearing() {
        let a = Goal::new(
            vec![
                Literal::Positive(Atom::new("commit", vec![Term::var("R")])),
                Literal::Positive(Atom::new("member", vec![Term::var("M")])),
            ],
            vec![Var::new("R")],
        );
        let b = Goal::new(
            vec![
                Literal::Positive(Atom::new("member", vec![Term::var("M")])),
                Literal::Positive(Atom::new("commit", vec![Term::var("R")])),
            ],
            vec![Var::new("R")],
        );
        assert_ne!(Canonicalizer::goal(&a), Canonicalizer::goal(&b));
    }

    #[test]
    fn a_constant_never_collides_with_a_variable_name() {
        let with_const = Goal::new(
            vec![Literal::Positive(Atom::new(
                "member",
                vec![Term::sym("V0")],
            ))],
            vec![],
        );
        let with_var = Goal::new(
            vec![Literal::Positive(Atom::new("member", vec![Term::var("M")]))],
            vec![],
        );
        assert_ne!(
            Canonicalizer::goal(&with_const),
            Canonicalizer::goal(&with_var)
        );
    }

    #[test]
    fn a_symbol_cannot_forge_a_field_boundary() {
        // Without length prefixing, an identifier carrying the delimiter bytes
        // of a second literal would canonicalize to the same string as an
        // actual two-literal goal -- one cache entry, two different answers.
        let forged = Goal::new(
            vec![Literal::Positive(Atom::new(
                "p",
                vec![Term::sym("x\u{0}1:;\u{0}1:p\u{0}1:1\u{0}")],
            ))],
            vec![],
        );
        let honest = Goal::new(
            vec![
                Literal::Positive(Atom::new("p", vec![Term::sym("x")])),
                Literal::Positive(Atom::new("p", vec![Term::sym("y")])),
            ],
            vec![],
        );
        assert_ne!(Canonicalizer::goal(&forged), Canonicalizer::goal(&honest));
    }

    #[test]
    fn resolution_changes_the_canonical_form() {
        // Stated as a test because it is a contract, not an accident: the
        // canonicalizer is only defined on resolved IR.
        let atom = Atom::new("=", vec![Term::var("A"), Term::var("B")]);
        let unresolved = Goal::new(vec![Literal::Positive(atom.clone())], vec![]);
        let resolved = Goal::new(vec![Literal::Builtin(atom)], vec![]);
        assert_ne!(
            Canonicalizer::goal(&unresolved),
            Canonicalizer::goal(&resolved)
        );
    }
}
