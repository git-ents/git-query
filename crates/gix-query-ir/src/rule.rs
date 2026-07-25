use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::{Atom, Literal, PredicateKey, Span, Var, Visibility};

/// A rule's position in the program it belongs to.
///
/// Stable across the magic-set rewrite: a generated rule carries the id of the
/// source rule it came from, so derivation trees can be reported over the rules
/// the author wrote rather than over engine bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId(pub usize);

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// Why a rule exists, when it was not written by anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteKind {
    /// A `magic_p` seed: the demand a top-level goal creates.
    MagicSeed,
    /// A `magic_p` rule: demand propagated sideways from a guarded body.
    MagicRule,
    /// A source rule guarded by its own demand.
    Guarded,
}

/// A generated rule's back-reference to the rule it was derived from.
///
/// Every rule the magic-set rewrite emits carries one. Without it the first
/// `explain` output anyone sees is full of `magic_` relations, and retrofitting
/// the back-reference afterwards means revisiting the rewrite, the provenance
/// sink, and every join between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOrigin {
    /// The source rule, or `None` for a seed -- demand created by a top-level
    /// goal has no rule behind it, and a sentinel [`RuleId`] here would be
    /// handed straight to [`Program::rule`] and come back `None` anyway.
    pub source: Option<RuleId>,
    /// What the rewrite made of it.
    pub kind: RewriteKind,
    /// Maps each body literal of the generated rule to its index in the source
    /// rule's body, or `None` for a literal the rewrite injected.
    ///
    /// Guards shift every following literal by one. Without this map a
    /// derivation records support against rewritten offsets while `explain`
    /// reads them against the source rule, so every literal in the tree is
    /// misattributed -- off by the number of injected guards, silently, and
    /// most visibly on exactly the recursive rules people ask to explain.
    pub body_map: Vec<Option<usize>>,
}

impl RuleOrigin {
    /// The source-rule body index of the generated rule's literal `i`.
    #[must_use]
    pub fn source_literal(&self, i: usize) -> Option<usize> {
        self.body_map.get(i).copied().flatten()
    }
}

/// A single rule: one head, a conjunctive body.
///
/// A body is evaluated in *author order*. That is a language decision, not an
/// engine one: it makes mode checking a linear left-to-right walk rather than a
/// search, it keeps derivation trees isomorphic to the source text, and it
/// supplies the sideways-information-passing strategy the magic-set rewrite
/// needs, so no planner has to invent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The derived atom.
    pub head: Atom,
    /// The body literals, in the order they were written.
    pub body: Vec<Literal>,
    /// Where it was written.
    pub span: Span,
    /// Set only on rules the magic-set rewrite generated.
    pub origin: Option<RuleOrigin>,
}

impl Rule {
    /// A rule with no source location and no rewrite origin.
    #[must_use]
    pub fn new(head: Atom, body: Vec<Literal>) -> Self {
        Self {
            head,
            body,
            span: Span::synthetic(),
            origin: None,
        }
    }

    /// A fact: a rule with an empty body.
    #[must_use]
    pub fn fact(head: Atom) -> Self {
        Self::new(head, Vec::new())
    }

    /// Whether this rule has no body.
    #[must_use]
    pub fn is_fact(&self) -> bool {
        self.body.is_empty()
    }

    /// Every variable in the head, with repeats.
    pub fn head_vars(&self) -> impl Iterator<Item = &Var> {
        self.head.vars()
    }

    /// Every predicate this rule's body calls, positive and negated, in author
    /// order and with repeats.
    pub fn body_keys(&self) -> impl Iterator<Item = &PredicateKey> {
        self.body.iter().map(Literal::key)
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.head)?;
        if self.body.is_empty() {
            return f.write_str(".");
        }
        f.write_str(" :- ")?;
        for (i, l) in self.body.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{l}")?;
        }
        f.write_str(".")
    }
}

/// A `pub` declaration: a predicate a module exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// Which predicate.
    pub key: PredicateKey,
    /// Where it was declared.
    pub span: Span,
}

/// One rule module's parsed contents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    /// The module's name, which qualifies every predicate it defines.
    pub name: Arc<str>,
    /// Its `pub` surface. Unmarked predicates are private and do not resolve
    /// across module boundaries.
    pub exports: Vec<Export>,
    /// Its rules, in source order.
    pub rules: Vec<Rule>,
}

impl Module {
    /// An empty module named `name`.
    pub fn new(name: impl Into<Arc<str>>) -> Self {
        Self {
            name: name.into(),
            exports: Vec::new(),
            rules: Vec::new(),
        }
    }

    /// The visibility of `key` within this module.
    #[must_use]
    pub fn visibility(&self, key: &PredicateKey) -> Visibility {
        if self.exports.iter().any(|e| &e.key == key) {
            Visibility::Public
        } else {
            Visibility::Private
        }
    }
}

/// A whole program: every module's rules, assembled and flattened.
///
/// Validation is whole-program. The active program is the union over the rule
/// namespace, so a module that is valid alone can still be rejected -- two
/// independently-valid modules can compose into a negation cycle, and only the
/// assembled program can see that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Program {
    rules: Vec<Rule>,
    /// The visibility of every predicate the program defines.
    visibility: BTreeMap<PredicateKey, Visibility>,
    /// Which module each rule came from, parallel to `rules`.
    origins: Vec<Arc<str>>,
}

impl Program {
    /// An empty program.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `module`'s rules, qualifying nothing -- qualification is the
    /// assembler's job and happens before this call, so that a `Program` is
    /// always in one flat namespace.
    pub fn add_module(&mut self, module: Module) {
        let Module {
            name,
            exports,
            rules,
        } = module;
        for rule in rules {
            let visibility = if exports.iter().any(|e| e.key == rule.head.predicate) {
                Visibility::Public
            } else {
                Visibility::Private
            };
            self.visibility
                .entry(rule.head.predicate.clone())
                .or_insert(visibility);
            self.rules.push(rule);
            self.origins.push(name.clone());
        }
        for e in exports {
            self.visibility.insert(e.key, Visibility::Public);
        }
    }

    /// Append one rule, attributed to `module`.
    pub fn push_rule(&mut self, rule: Rule, module: impl Into<Arc<str>>) -> RuleId {
        let id = RuleId(self.rules.len());
        self.rules.push(rule);
        self.origins.push(module.into());
        id
    }

    /// Every rule, in program order.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Every rule, mutably.
    pub fn rules_mut(&mut self) -> &mut [Rule] {
        &mut self.rules
    }

    /// The rule with this id.
    #[must_use]
    pub fn rule(&self, id: RuleId) -> Option<&Rule> {
        self.rules.get(id.0)
    }

    /// The module a rule came from.
    #[must_use]
    pub fn rule_module(&self, id: RuleId) -> Option<&str> {
        self.origins.get(id.0).map(|s| &**s)
    }

    /// Every rule defining `key`, with its id.
    pub fn rules_for(&self, key: &PredicateKey) -> impl Iterator<Item = (RuleId, &Rule)> {
        self.rules
            .iter()
            .enumerate()
            .filter(move |(_, r)| &r.head.predicate == key)
            .map(|(i, r)| (RuleId(i), r))
    }

    /// Every predicate the program defines, in key order.
    pub fn defined(&self) -> impl Iterator<Item = &PredicateKey> {
        self.visibility.keys()
    }

    /// The visibility of a predicate the program defines.
    #[must_use]
    pub fn visibility(&self, key: &PredicateKey) -> Visibility {
        self.visibility.get(key).copied().unwrap_or_default()
    }

    /// Resolve a generated rule back to the source rule it came from.
    ///
    /// Returns `id` unchanged when it is already a source rule, and `None` for
    /// a seed, which has no source rule to name. This is what keeps `magic_`
    /// relations out of `explain`.
    #[must_use]
    pub fn source_of(&self, id: RuleId) -> Option<RuleId> {
        match self.rule(id).and_then(|r| r.origin.as_ref()) {
            Some(o) => o.source,
            None => Some(id),
        }
    }

    /// Whether this rule exists only to carry demand, and therefore must never
    /// appear in query output or in a derivation tree.
    #[must_use]
    pub fn is_generated(&self, id: RuleId) -> bool {
        self.rule(id)
            .is_some_and(|r| r.head.predicate.is_magic() || r.origin.is_some())
    }
}

/// A query: a conjunctive body plus the variables to project.
///
/// Every tier produces one of these. Tier 1 desugars a predicate name and its
/// flags, tier 2 composes a subject with filters, tier 3 parses a goal
/// directly -- and all three produce the same IR, which is what "one syntax,
/// three tiers" has to mean concretely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    /// The literals, in author order.
    pub body: Vec<Literal>,
    /// The variables to return, in output-column order.
    pub select: Vec<Var>,
    /// Where it was written.
    pub span: Span,
}

impl Goal {
    /// A goal over `body` projecting `select`.
    #[must_use]
    pub fn new(body: Vec<Literal>, select: Vec<Var>) -> Self {
        Self {
            body,
            select,
            span: Span::synthetic(),
        }
    }
}

impl fmt::Display for Goal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, l) in self.body.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{l}")?;
        }
        Ok(())
    }
}
