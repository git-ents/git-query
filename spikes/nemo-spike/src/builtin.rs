//! Stubbed host builtins.
//!
//! The real ones call into git-store and git-anchor. The point of stubbing
//! them behind this trait is that the demand loop in [`crate::demand`] never
//! learns which it is talking to -- gate A3 is about the loop, and swapping
//! the table for a repository is Phase 4's job, not the spike's.

use std::collections::BTreeMap;

use crate::value::{Tuple, Value};

/// A demand-driven host call.
///
/// The mode is the whole point: `bound` is how many leading argument
/// positions must arrive bound, and the engine may never ask for the relation
/// as a whole. A builtin that could be enumerated would not need a demand
/// loop.
pub trait Builtin {
    /// The predicate name as it appears in a rule body.
    fn name(&self) -> &str;

    /// Total arity, bound positions plus free ones.
    fn arity(&self) -> usize;

    /// How many leading positions are `+`.
    fn bound(&self) -> usize;

    /// Answer one call. Returns the completions of the free positions; an
    /// empty vector means the call succeeded with no answers, which is not the
    /// same as the call being illegal.
    fn call(&self, bound: &[Value]) -> Vec<Vec<Value>>;
}

/// A builtin backed by a hardcoded table, for the spike.
#[derive(Debug, Clone)]
pub struct TableBuiltin {
    name: String,
    bound: usize,
    arity: usize,
    rows: BTreeMap<Vec<Value>, Vec<Vec<Value>>>,
}

impl TableBuiltin {
    /// A builtin named `name` whose first `bound` positions are `+`, defined
    /// by whole tuples.
    pub fn new(name: &str, bound: usize, tuples: impl IntoIterator<Item = Tuple>) -> Self {
        let mut rows: BTreeMap<Vec<Value>, Vec<Vec<Value>>> = BTreeMap::new();
        let mut arity = bound;
        for tuple in tuples {
            arity = tuple.arity();
            let (key, rest) = tuple.0.split_at(bound);
            rows.entry(key.to_vec()).or_default().push(rest.to_vec());
        }
        Self {
            name: name.to_owned(),
            bound,
            arity,
            rows,
        }
    }
}

impl Builtin for TableBuiltin {
    fn name(&self) -> &str {
        &self.name
    }

    fn arity(&self) -> usize {
        self.arity
    }

    fn bound(&self) -> usize {
        self.bound
    }

    fn call(&self, bound: &[Value]) -> Vec<Vec<Value>> {
        self.rows.get(bound).cloned().unwrap_or_default()
    }
}
