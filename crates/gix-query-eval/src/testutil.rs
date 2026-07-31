//! A stub [`FactSource`]/[`Builtin`] pair backed by in-memory tables, shared
//! by every test in this crate that needs to run a program end to end.
//!
//! `gix-query-eval` has no git dependency: builtins and fact sources reach it
//! only through the traits `gix-query-ir` defines. This is the
//! repo-independent implementation of both, so the demand loop and the
//! rewrite can be tested against a fixed table rather than a repository.
#![cfg(test)]

use std::cell::RefCell;
use std::collections::BTreeMap;

use gix_query_ir::{
    ArgSet, Backing, Bindings, Builtin, Fact, FactSource, Facts, HostError, PredicateClass,
    PredicateEntry, PredicateKey, Registry, TypeSlot, Value, Visibility,
};

/// Register `name/arity` as a fully-enumerable host predicate.
///
/// Every predicate a test feeds through [`StubTable`] must be registered this
/// way rather than as an IDB predicate: the rewrite treats an IDB predicate
/// as defined by rules in the program, and a program with no rule for it
/// never asks anyone for its facts.
pub(crate) fn edb_enumerable(registry: &mut Registry, name: &str, arity: usize) {
    edb(registry, name, arity, ArgSet::empty());
}

/// Register `name/arity` as a required-bound host predicate -- the five
/// `commit`/`parent`/`tree_entry`/`author`/`line`-shaped EDB predicates that
/// stay behind the demand loop under `DEVPLAN.md` §0's narrow cut, never
/// eagerly preloaded.
pub(crate) fn edb_required_bound(
    registry: &mut Registry,
    name: &str,
    arity: usize,
    bound: &[usize],
) {
    edb(
        registry,
        name,
        arity,
        ArgSet::from_indices(bound.iter().copied()),
    );
}

fn edb(registry: &mut Registry, name: &str, arity: usize, required_bound: ArgSet) {
    registry
        .insert_predicate(PredicateEntry {
            key: PredicateKey::new(name, arity),
            types: vec![TypeSlot::Infer; arity],
            visibility: Visibility::Public,
            class: PredicateClass::Edb {
                required_bound,
                backing: vec![Backing::AdHoc],
            },
            doc: std::borrow::Cow::Borrowed(""),
        })
        .expect("insertable");
}

/// A table of complete tuples per predicate, answering a call by filtering on
/// whatever positions it supplied bound.
#[derive(Debug, Default)]
pub(crate) struct StubTable {
    rows: BTreeMap<PredicateKey, Vec<Vec<Value>>>,
    // Every call this table answered, in order -- what
    // `a3_the_builtin_is_never_enumerated`-style tests count.
    calls: RefCell<Vec<(PredicateKey, Vec<Option<Value>>)>>,
}

impl StubTable {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with(
        mut self,
        key: PredicateKey,
        rows: impl IntoIterator<Item = Vec<Value>>,
    ) -> Self {
        self.rows.entry(key).or_default().extend(rows);
        self
    }

    /// How many calls this table answered for `key`, regardless of whether
    /// they came back empty.
    pub(crate) fn call_count(&self, key: &PredicateKey) -> usize {
        self.calls.borrow().iter().filter(|(k, _)| k == key).count()
    }

    fn matches(row: &[Value], bound: Bindings<'_>) -> bool {
        row.iter()
            .zip(bound.iter())
            .all(|(v, b)| b.as_ref().is_none_or(|bv| v == bv))
    }

    fn record(&self, key: &PredicateKey, bound: Bindings<'_>) {
        self.calls.borrow_mut().push((key.clone(), bound.to_vec()));
    }
}

impl FactSource for StubTable {
    fn provides(&self, key: &PredicateKey) -> bool {
        self.rows.contains_key(key)
    }

    fn facts<'a>(
        &'a self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        attribute: bool,
    ) -> Result<Facts<'a>, HostError> {
        self.record(key, bound);
        let Some(rows) = self.rows.get(key) else {
            return Err(HostError::NoProvider(key.clone()));
        };
        let bound: Vec<Option<Value>> = bound.to_vec();
        let matching: Vec<Vec<Value>> = rows
            .iter()
            .filter(|r| Self::matches(r, &bound))
            .cloned()
            .collect();
        Ok(Box::new(matching.into_iter().map(move |values| {
            Ok(if attribute {
                Fact::attributed(values, "test")
            } else {
                Fact::new(values)
            })
        })))
    }
}

impl Builtin for StubTable {
    fn provides(&self, key: &PredicateKey) -> bool {
        self.rows.contains_key(key)
    }

    fn call(
        &self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError> {
        self.record(key, bound);
        let Some(rows) = self.rows.get(key) else {
            return Err(HostError::NoProvider(key.clone()));
        };
        out.extend(rows.iter().filter(|r| Self::matches(r, bound)).cloned());
        Ok(())
    }
}
