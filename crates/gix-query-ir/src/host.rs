use std::sync::Arc;

use crate::{PredicateKey, Value};

/// A failure inside a host provider or a builtin.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// No provider is registered for this predicate.
    #[error("no provider for {0}")]
    NoProvider(PredicateKey),
    /// The call supplied a binding pattern the provider does not support.
    /// Mode inference is supposed to make this unreachable; reaching it is an
    /// engine bug, not a user error.
    #[error("{key} was called with an unsupported binding pattern")]
    UnsupportedMode {
        /// The predicate called.
        key: PredicateKey,
    },
    /// The underlying repository or object database failed.
    #[error("host failure reading {key}: {source}")]
    Backend {
        /// The predicate being read.
        key: PredicateKey,
        /// What went wrong.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// One base fact, with the ref that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The tuple. Its length is the predicate's arity, and its bound positions
    /// equal the values the call supplied.
    pub values: Vec<Value>,
    /// The ref this fact was read from, when the provider was asked to
    /// attribute and the fact came from one.
    ///
    /// `explain` annotates each base fact with its source ref, and for several
    /// predicates the refname is not a function of the tuple -- `claim(C)`
    /// being the clear case, where the claim's ref cannot be recovered from the
    /// oid alone. Attribution is requested per call rather than always on, so
    /// the gate path pays nothing for a string it will not print.
    pub source_ref: Option<Arc<str>>,
}

impl Fact {
    /// An unattributed fact.
    #[must_use]
    pub fn new(values: Vec<Value>) -> Self {
        Self {
            values,
            source_ref: None,
        }
    }

    /// A fact attributed to `source_ref`.
    pub fn attributed(values: Vec<Value>, source_ref: impl Into<Arc<str>>) -> Self {
        Self {
            values,
            source_ref: Some(source_ref.into()),
        }
    }
}

/// One call's binding pattern: `Some` for a bound argument, `None` for a free
/// one. Length always equals the predicate's arity.
pub type Bindings<'a> = &'a [Option<Value>];

/// A stream of facts.
pub type Facts<'a> = Box<dyn Iterator<Item = Result<Fact, HostError>> + 'a>;

/// A source of base facts.
///
/// Defined here and implemented in the host crate, so that the evaluator has
/// no git dependency at all and can be property-tested over synthetic facts
/// with no repository. That is the single most important structural constraint
/// in this workspace.
pub trait FactSource {
    /// Whether this source can answer for `key`.
    fn provides(&self, key: &PredicateKey) -> bool;

    /// Every tuple of `key` matching `bound`.
    ///
    /// The provider may assume `bound` satisfies one of the mode vectors the
    /// registry declares for `key`; the validator guarantees it.
    ///
    /// `attribute` asks for [`Fact::source_ref`] to be filled in. Providers
    /// must honor it rather than always attributing: it is on only for
    /// `explain`, and the gate path runs the same code.
    fn facts<'a>(
        &'a self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        attribute: bool,
    ) -> Result<Facts<'a>, HostError>;
}

/// A moded, non-enumerable call.
///
/// Builtins are called with bound inputs *inside* the fixpoint loop, which is
/// why they are a trait call in a join rather than a relation to
/// pre-materialize: materializing `bind` over the full anchor-by-rev product is
/// exactly the terminating-but-unaffordable trap the design exists to avoid.
pub trait Builtin {
    /// Whether this implementation can answer for `key`.
    fn provides(&self, key: &PredicateKey) -> bool;

    /// Call `key` with `bound`, appending complete tuples to `out`.
    ///
    /// Appending rather than returning lets the evaluator reuse one buffer
    /// across the fixpoint. Every builtin declares `finite`, so this must
    /// append finitely many tuples per call -- that is what makes a
    /// multi-candidate projection safe.
    fn call(
        &self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError>;

    /// Evaluate an all-bound builtin as a predicate, allocating nothing.
    ///
    /// The comparisons are the hottest builtins in the language -- a per-line
    /// span check evaluates two of them per candidate line -- and answering
    /// "true" by returning a vector containing a copy of the arguments is the
    /// shape to avoid. The default routes through [`Builtin::call`] for
    /// implementations that have no cheaper answer.
    fn holds(&self, key: &PredicateKey, args: &[Value]) -> Result<bool, HostError> {
        let bound: Vec<Option<Value>> = args.iter().cloned().map(Some).collect();
        let mut out = Vec::new();
        self.call(key, &bound, &mut out)?;
        Ok(!out.is_empty())
    }
}

/// The comparison builtins, which need no host at all.
///
/// They are registered like every other builtin so their properties live in
/// one table, but their implementation is here rather than in the host crate
/// because comparing two ground values does not touch a repository.
#[derive(Debug, Clone, Copy, Default)]
pub struct Comparisons;

impl Comparisons {
    fn op(key: &PredicateKey) -> Option<crate::CompareOp> {
        (key.arity == 2)
            .then(|| {
                crate::CompareOp::all()
                    .into_iter()
                    .find(|op| op.as_str() == &*key.name)
            })
            .flatten()
    }

    fn eval(op: crate::CompareOp, l: &Value, r: &Value) -> Result<bool, ()> {
        use crate::CompareOp::{Eq, Ge, Gt, Le, Lt, Ne};
        Ok(match op {
            Eq => l == r,
            Ne => l != r,
            Lt | Le | Gt | Ge => {
                // Ordering is int-only, so no rule's meaning can depend on
                // hash bytes. The type pass guarantees this; a violation here
                // is an engine bug.
                let (Some(a), Some(b)) = (l.as_int(), r.as_int()) else {
                    return Err(());
                };
                match op {
                    Lt => a < b,
                    Le => a <= b,
                    Gt => a > b,
                    _ => a >= b,
                }
            }
        })
    }
}

impl Builtin for Comparisons {
    fn provides(&self, key: &PredicateKey) -> bool {
        Self::op(key).is_some()
    }

    fn call(
        &self,
        key: &PredicateKey,
        bound: Bindings<'_>,
        out: &mut Vec<Vec<Value>>,
    ) -> Result<(), HostError> {
        let op = Self::op(key).ok_or_else(|| HostError::NoProvider(key.clone()))?;
        // Comparisons are all-bound: filters, never generators.
        let (Some(Some(l)), Some(Some(r))) = (bound.first(), bound.get(1)) else {
            return Err(HostError::UnsupportedMode { key: key.clone() });
        };
        if Self::eval(op, l, r).map_err(|()| HostError::UnsupportedMode { key: key.clone() })? {
            out.push(vec![l.clone(), r.clone()]);
        }
        Ok(())
    }

    fn holds(&self, key: &PredicateKey, args: &[Value]) -> Result<bool, HostError> {
        let op = Self::op(key).ok_or_else(|| HostError::NoProvider(key.clone()))?;
        let (Some(l), Some(r)) = (args.first(), args.get(1)) else {
            return Err(HostError::UnsupportedMode { key: key.clone() });
        };
        Self::eval(op, l, r).map_err(|()| HostError::UnsupportedMode { key: key.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CompareOp;

    fn holds(op: CompareOp, l: Value, r: Value) -> bool {
        Comparisons
            .holds(&PredicateKey::new(op.as_str(), 2), &[l, r])
            .expect("well-moded call")
    }

    #[test]
    fn equality_is_a_filter_on_ground_values() {
        assert!(holds(CompareOp::Eq, Value::sym("a"), Value::sym("a")));
        assert!(!holds(CompareOp::Eq, Value::sym("a"), Value::sym("b")));
        assert!(holds(CompareOp::Ne, Value::sym("a"), Value::sym("b")));
    }

    #[test]
    fn ordering_is_integers_only() {
        assert!(holds(CompareOp::Lt, Value::Int(1), Value::Int(2)));
        let err = Comparisons.holds(
            &PredicateKey::new("<", 2),
            &[Value::sym("a"), Value::sym("b")],
        );
        assert!(matches!(err, Err(HostError::UnsupportedMode { .. })));
    }

    #[test]
    fn a_free_operand_is_rejected_rather_than_generated() {
        let mut out = Vec::new();
        let err = Comparisons.call(
            &PredicateKey::new("=", 2),
            &[Some(Value::Int(1)), None],
            &mut out,
        );
        assert!(matches!(err, Err(HostError::UnsupportedMode { .. })));
    }

    #[test]
    fn the_two_entry_points_agree() {
        let key = PredicateKey::new("<=", 2);
        for (a, b) in [(1, 2), (2, 2), (3, 2)] {
            let mut out = Vec::new();
            Comparisons
                .call(&key, &[Some(Value::Int(a)), Some(Value::Int(b))], &mut out)
                .expect("well-moded");
            let fast = Comparisons
                .holds(&key, &[Value::Int(a), Value::Int(b)])
                .expect("well-moded");
            assert_eq!(!out.is_empty(), fast);
        }
    }
}
