//! Recursive-descent parser: tokens to [`Module`] or [`Goal`].
//!
//! The grammar is small enough that one token of lookahead, plus the
//! span-adjacency trick for qualified names (see [`Parser::predicate_name`]),
//! is all it needs. There is no separate AST: every production builds
//! `gix_query_ir` types directly, spans included, per the IR crate's own
//! reasoning for why spans live on IR nodes rather than on a parallel tree.

use std::collections::HashSet;
use std::sync::Arc;

use gix_query_ir::{
    Atom, Compare, CompareOp, Goal, Literal, Module, PredicateKey, Rule, Span, SpannedTerm, Term,
    Value, Var,
};

use crate::error::{Errors, ParseError};
use crate::lexer::tokenize;
use crate::token::{Token, TokenKind};

/// Parse a whole rule module: `pub` declarations plus rules.
///
/// `name` becomes [`Module::name`]. It is supplied by the caller rather than
/// read from `source`, because a `.dl` file never declares its own module
/// name -- `git query rules add <name> <file>` assigns it, the same way
/// `refs/meta/rules/<name>` names the ref it is pushed to.
///
/// Recovers from a syntax error by skipping to the next `.` and continuing,
/// so one run reports every mistake in the module rather than only the
/// first.
pub fn parse_module(name: impl Into<Arc<str>>, source: &str) -> Result<Module, Errors> {
    let (tokens, lex_errors) = tokenize(source);
    let mut parser = Parser {
        tokens,
        pos: 0,
        errors: lex_errors,
    };
    let mut exports = Vec::new();
    let mut rules = Vec::new();
    while !parser.at_eof() {
        match parser.item() {
            Ok(Item::Export(e)) => exports.push(e),
            Ok(Item::Rule(r)) => rules.push(r),
            Err(()) => parser.resync_to_dot(),
        }
    }
    for export in &exports {
        if !rules.iter().any(|r: &Rule| r.head.predicate == export.key) {
            parser.errors.push(ParseError::PubUndefined {
                key: export.key.clone(),
                span: export.span,
            });
        }
    }
    if parser.errors.is_empty() {
        Ok(Module {
            name: name.into(),
            exports,
            rules,
        })
    } else {
        Err(parser.errors)
    }
}

/// Parse a tier-3 goal body: a comma-separated literal list, with no
/// terminating period -- the one syntactic difference from a rule body.
///
/// `select` defaults to every variable in first-occurrence order, which is
/// what tier 1's desugaring wants and what a goal typed with no `--select`
/// means. Use [`parse_goal_with_select`] when the caller supplies one.
pub fn parse_goal(source: &str) -> Result<Goal, Errors> {
    let (tokens, lex_errors) = tokenize(source);
    let mut parser = Parser {
        tokens,
        pos: 0,
        errors: lex_errors,
    };
    let body = parser.goal_body();
    if !parser.errors.is_empty() {
        return Err(parser.errors);
    }
    let select = first_occurrence_vars(&body);
    Ok(Goal {
        body,
        select,
        span: Span::new(0, source.len() as u32),
    })
}

/// Parse a tier-3 goal body with an explicit `--select` list.
///
/// Each entry must be a legal variable name (uppercase-initial, or
/// `_name`); a lone `_` is rejected, since [`gix_query_ir::Term::Anon`]
/// names nothing and cannot be a projected column. The names carry no
/// position in `source`, so their errors report a synthetic span.
pub fn parse_goal_with_select(source: &str, select: &[&str]) -> Result<Goal, Errors> {
    let mut goal = parse_goal(source)?;
    let mut vars = Vec::with_capacity(select.len());
    let mut errors = Errors::new();
    for name in select {
        if is_valid_var_name(name) {
            vars.push(Var::new(*name));
        } else {
            errors.push(ParseError::UnexpectedToken {
                expected: "a variable name (uppercase-initial, or `_name`)".to_owned(),
                found: format!("'{name}'"),
                span: Span::synthetic(),
            });
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    goal.select = vars;
    Ok(goal)
}

fn is_valid_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
        Some('_') => {
            let rest = chars.as_str();
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

fn first_occurrence_vars(body: &[Literal]) -> Vec<Var> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for lit in body {
        for v in lit.vars() {
            if seen.insert(v.clone()) {
                out.push(v.clone());
            }
        }
    }
    out
}

enum Item {
    Export(gix_query_ir::Export),
    Rule(Rule),
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Errors,
}

impl Parser {
    fn peek(&self) -> &Token {
        // `tokenize` always ends in `Eof`, and `advance` never steps past
        // it, so this index is always in bounds.
        &self.tokens[self.pos]
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn check(&self, want: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(want)
    }

    fn expect(&mut self, want: TokenKind, expected: &str) -> Result<Token, ()> {
        if self.check(&want) {
            Ok(self.advance())
        } else {
            let tok = self.peek().clone();
            self.expected(expected, &tok);
            Err(())
        }
    }

    fn expected(&mut self, expected: &str, tok: &Token) {
        if matches!(tok.kind, TokenKind::Eof) {
            self.errors.push(ParseError::UnexpectedEof {
                expected: expected.to_owned(),
                span: tok.span,
            });
        } else {
            self.errors.push(ParseError::UnexpectedToken {
                expected: expected.to_owned(),
                found: tok.kind.describe(),
                span: tok.span,
            });
        }
    }

    /// Recover from a malformed rule item by skipping to (and consuming) the
    /// next `.`, or to end of input if there is none. This is the whole
    /// recovery strategy: simple, and it is what lets one run of
    /// `parse_module` report several errors instead of stopping at the
    /// first.
    fn resync_to_dot(&mut self) {
        loop {
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Dot => {
                    self.advance();
                    break;
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// Recover inside a goal body by skipping to the next `,`, or to end of
    /// input. Goals have no period to resync on.
    fn resync_to_comma(&mut self) {
        loop {
            match self.peek().kind {
                TokenKind::Eof | TokenKind::Comma => break,
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn check_reserved(&mut self, key: &PredicateKey, span: Span) {
        if key.local_name().starts_with(PredicateKey::MAGIC_PREFIX) {
            self.errors.push(ParseError::ReservedPrefix {
                key: key.clone(),
                span,
            });
        }
    }

    fn item(&mut self) -> Result<Item, ()> {
        if self.at_pub_keyword() {
            self.export()
        } else {
            self.rule().map(Item::Rule)
        }
    }

    fn at_pub_keyword(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Sym(s) if &**s == "pub")
    }

    fn export(&mut self) -> Result<Item, ()> {
        self.advance(); // `pub`
        let (name, name_span) = self.predicate_name()?;
        self.expect(TokenKind::LParen, "'('")?;
        let mut arity = 0usize;
        if !self.check(&TokenKind::RParen) {
            loop {
                self.term()?;
                arity += 1;
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    continue;
                }
                break;
            }
        }
        self.expect(TokenKind::RParen, "')'")?;
        let dot = self.expect(TokenKind::Dot, "'.'")?;
        let span = name_span.join(dot.span);
        let key = PredicateKey::new(name, arity);
        self.check_reserved(&key, span);
        Ok(Item::Export(gix_query_ir::Export { key, span }))
    }

    fn rule(&mut self) -> Result<Rule, ()> {
        let head = self.atom()?;
        if self.check(&TokenKind::Dot) {
            let dot = self.advance();
            return Ok(Rule {
                span: head.span.join(dot.span),
                head,
                body: Vec::new(),
                origin: None,
            });
        }
        self.expect(TokenKind::ColonDash, "':-'")?;
        let mut body = Vec::new();
        loop {
            body.push(self.literal()?);
            if self.check(&TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        let dot = self.expect(TokenKind::Dot, "'.'")?;
        Ok(Rule {
            span: head.span.join(dot.span),
            head,
            body,
            origin: None,
        })
    }

    fn goal_body(&mut self) -> Vec<Literal> {
        let mut body = Vec::new();
        if self.at_eof() {
            return body;
        }
        loop {
            match self.literal() {
                Ok(lit) => body.push(lit),
                Err(()) => self.resync_to_comma(),
            }
            if self.check(&TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        if !self.at_eof() {
            let tok = self.peek().clone();
            self.expected("',' or end of input", &tok);
        }
        body
    }

    fn literal(&mut self) -> Result<Literal, ()> {
        if self.check(&TokenKind::Bang) {
            self.advance();
            return self.atom().map(Literal::Negated);
        }
        if self.looks_like_atom() {
            return self.atom().map(Literal::Positive);
        }
        let left = self.term()?;
        let op = self.compare_op()?;
        let right = self.term()?;
        let span = left.span.join(right.span);
        Ok(Literal::Compare(Compare {
            op,
            left,
            right,
            span,
        }))
    }

    fn compare_op(&mut self) -> Result<CompareOp, ()> {
        let tok = self.peek().clone();
        let op = match tok.kind {
            TokenKind::Eq => CompareOp::Eq,
            TokenKind::Ne => CompareOp::Ne,
            TokenKind::Lt => CompareOp::Lt,
            TokenKind::Le => CompareOp::Le,
            TokenKind::Gt => CompareOp::Gt,
            TokenKind::Ge => CompareOp::Ge,
            _ => {
                self.expected("a comparison operator, or '('", &tok);
                return Err(());
            }
        };
        self.advance();
        Ok(op)
    }

    fn atom(&mut self) -> Result<Atom, ()> {
        let (name, name_span) = self.predicate_name()?;
        self.expect(TokenKind::LParen, "'('")?;
        let mut terms = Vec::new();
        if !self.check(&TokenKind::RParen) {
            loop {
                terms.push(self.term()?);
                if self.check(&TokenKind::Comma) {
                    self.advance();
                    continue;
                }
                break;
            }
        }
        let rparen = self.expect(TokenKind::RParen, "')'")?;
        let span = name_span.join(rparen.span);
        let predicate = PredicateKey::new(name, terms.len());
        self.check_reserved(&predicate, span);
        Ok(Atom {
            predicate,
            terms,
            span,
        })
    }

    fn term(&mut self) -> Result<SpannedTerm, ()> {
        let tok = self.peek().clone();
        let term = match &tok.kind {
            TokenKind::Var(name) => Term::Var(Var::new(name.clone())),
            TokenKind::Anon => Term::Anon,
            TokenKind::Int(n) => Term::Const(Value::Int(*n)),
            TokenKind::Sym(s) => Term::Const(Value::sym(s.clone())),
            TokenKind::Str(s) => Term::Const(Value::sym(s.clone())),
            _ => {
                self.expected("a term", &tok);
                return Err(());
            }
        };
        self.advance();
        Ok(SpannedTerm::new(term, tok.span))
    }

    /// Parse a predicate name, combining a qualifier (`review.reviewed`)
    /// when one is written adjacent to the base name with no whitespace on
    /// either side of the `.`.
    ///
    /// Adjacency, tracked through spans rather than through a lexer-level
    /// rule, is what disambiguates a qualifying `.` from the `.` that ends a
    /// rule: `review.reviewed(X).` lexes as `Sym Dot Sym LParen ... RParen
    /// Dot`, and only the first `Dot` sits with no gap on both sides.
    fn predicate_name(&mut self) -> Result<(String, Span), ()> {
        let tok = self.peek().clone();
        let TokenKind::Sym(first) = &tok.kind else {
            self.expected("a predicate name", &tok);
            return Err(());
        };
        if !starts_lowercase(first) {
            self.expected("a predicate name", &tok);
            return Err(());
        }
        self.advance();
        let mut name = first.to_string();
        let mut span = tok.span;
        while let Some((part, part_span)) = self.adjacent_qualifier(span) {
            name.push('.');
            name.push_str(&part);
            span = span.join(part_span);
        }
        Ok((name, span))
    }

    /// If the next two tokens are a `.` immediately followed by a
    /// lowercase-led `Sym`, both with no gap from `after` and from each
    /// other, consume them and return the `Sym`'s text and span.
    fn adjacent_qualifier(&mut self, after: Span) -> Option<(Arc<str>, Span)> {
        let dot = self.peek().clone();
        if !matches!(dot.kind, TokenKind::Dot) || dot.span.start != after.end {
            return None;
        }
        let next = self.tokens.get(self.pos + 1)?.clone();
        let TokenKind::Sym(part) = &next.kind else {
            return None;
        };
        if next.span.start != dot.span.end || !starts_lowercase(part) {
            return None;
        }
        self.advance(); // dot
        self.advance(); // part
        Some((part.clone(), next.span))
    }

    /// Whether the tokens starting at the cursor spell a predicate name
    /// immediately followed by `(` -- the lookahead [`Self::literal`] needs
    /// to choose between an atom and a comparison without consuming
    /// anything on a path it may not take.
    fn looks_like_atom(&self) -> bool {
        let TokenKind::Sym(first) = &self.peek().kind else {
            return false;
        };
        if !starts_lowercase(first) {
            return false;
        }
        let mut end = self.peek().span.end;
        let mut i = self.pos + 1;
        while let Some(dot) = self.tokens.get(i) {
            if !matches!(dot.kind, TokenKind::Dot) || dot.span.start != end {
                break;
            }
            let Some(part) = self.tokens.get(i + 1) else {
                break;
            };
            let TokenKind::Sym(p) = &part.kind else {
                break;
            };
            if part.span.start != dot.span.end || !starts_lowercase(p) {
                break;
            }
            end = part.span.end;
            i += 2;
        }
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::LParen))
    }
}

fn starts_lowercase(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}
