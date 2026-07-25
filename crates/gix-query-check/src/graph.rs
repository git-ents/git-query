//! The predicate dependency graph, shared by stratification (pass 5),
//! footprints (pass 7), and minting confinement (pass 9).
//!
//! Building it once and handing the same [`Sccs`] to three passes is
//! deliberate: an SCC decomposition and a stratum assignment are exactly the
//! kind of answer that must not be computed twice and silently disagree, the
//! same argument [`gix_query_ir::Stratification`] itself makes for why the
//! evaluator does not recompute it.

use std::collections::BTreeMap;

use gix_query_ir::{PredicateKey, Program, Registry, RuleId, Span};

/// One edge from a defining rule's head predicate to a predicate its body
/// calls.
#[derive(Debug, Clone)]
pub(crate) struct Edge {
    pub to: PredicateKey,
    pub negated: bool,
    pub rule: RuleId,
    pub span: Span,
}

/// Direct IDB-to-IDB edges, keyed by the predicate whose rules produced
/// them. EDB predicates and builtins are never keys here -- they have no
/// rules to walk -- and never appear as edge targets either, since an edge
/// to a host predicate cannot participate in a cycle through rule
/// recursion.
pub(crate) type DepGraph = BTreeMap<PredicateKey, Vec<Edge>>;

/// Build the direct dependency graph over every predicate the program
/// defines.
pub(crate) fn build(program: &Program, registry: &Registry) -> DepGraph {
    let mut graph: DepGraph = program.defined().map(|k| (k.clone(), Vec::new())).collect();
    for key in program.defined() {
        for (rule_id, rule) in program.rules_for(key) {
            for lit in &rule.body {
                let target = lit.key();
                if !program.defined().any(|k| k == target) {
                    continue;
                }
                // A builtin or EDB predicate can also collide by name with
                // an IDB one only if the registry refused the redefinition
                // (already reported); guard defensively rather than assume.
                if registry.is_builtin(target) || registry.is_base(target) {
                    continue;
                }
                graph.entry(key.clone()).or_default().push(Edge {
                    to: target.clone(),
                    negated: lit.is_negated(),
                    rule: rule_id,
                    span: lit.span(),
                });
            }
        }
    }
    graph
}

/// A strongly connected component decomposition of a [`DepGraph`], plus the
/// dependency-first order the components must be evaluated in.
pub(crate) struct Sccs {
    of: BTreeMap<PredicateKey, usize>,
    pub members: Vec<Vec<PredicateKey>>,
    /// Component indices, dependencies before dependents -- the order
    /// [`gix_query_ir::Stratification`] needs and the order footprint
    /// computation must fold in.
    pub order: Vec<usize>,
}

impl Sccs {
    /// Which component `key` belongs to.
    pub(crate) fn of(&self, key: &PredicateKey) -> Option<usize> {
        self.of.get(key).copied()
    }

    /// Whether the component `key` belongs to is recursive: more than one
    /// member, or one member with an edge to itself.
    pub(crate) fn is_recursive(&self, key: &PredicateKey, graph: &DepGraph) -> bool {
        let Some(idx) = self.of(key) else {
            return false;
        };
        let members = &self.members[idx];
        if members.len() > 1 {
            return true;
        }
        let [only] = members.as_slice() else {
            return false;
        };
        graph
            .get(only)
            .is_some_and(|edges| edges.iter().any(|e| &e.to == only))
    }
}

/// Tarjan's algorithm, iterative to avoid recursion-depth risk on a
/// programmatically generated graph (the magic-set rewrite will produce
/// exactly that in `gix-query-eval`, which reuses nothing from this crate
/// but the same caution applies to why this is written iteratively rather
/// than as the textbook recursive version).
pub(crate) fn tarjan(graph: &DepGraph) -> Sccs {
    struct State {
        index: BTreeMap<PredicateKey, usize>,
        lowlink: BTreeMap<PredicateKey, usize>,
        on_stack: BTreeMap<PredicateKey, bool>,
        stack: Vec<PredicateKey>,
        next_index: usize,
        components: Vec<Vec<PredicateKey>>,
    }

    enum Frame<'a> {
        Enter(&'a PredicateKey),
        Finish(&'a PredicateKey),
        AfterChild(&'a PredicateKey, &'a PredicateKey),
    }

    let mut st = State {
        index: BTreeMap::new(),
        lowlink: BTreeMap::new(),
        on_stack: BTreeMap::new(),
        stack: Vec::new(),
        next_index: 0,
        components: Vec::new(),
    };

    for start in graph.keys() {
        if st.index.contains_key(start) {
            continue;
        }
        let mut work: Vec<Frame<'_>> = vec![Frame::Enter(start)];
        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter(v) => {
                    st.index.insert(v.clone(), st.next_index);
                    st.lowlink.insert(v.clone(), st.next_index);
                    st.next_index += 1;
                    st.stack.push(v.clone());
                    st.on_stack.insert(v.clone(), true);
                    work.push(Frame::Finish(v));
                    for edge in graph.get(v).into_iter().flatten() {
                        let w = &edge.to;
                        if !st.index.contains_key(w) {
                            work.push(Frame::AfterChild(v, w));
                            work.push(Frame::Enter(w));
                        } else if st.on_stack.get(w).copied().unwrap_or(false) {
                            let w_index = st.index[w];
                            let v_low = st.lowlink[v];
                            st.lowlink.insert(v.clone(), v_low.min(w_index));
                        }
                    }
                }
                Frame::AfterChild(v, w) => {
                    let w_low = st.lowlink[w];
                    let v_low = st.lowlink[v];
                    st.lowlink.insert(v.clone(), v_low.min(w_low));
                }
                Frame::Finish(v) => {
                    if st.lowlink[v] == st.index[v] {
                        let mut component = Vec::new();
                        loop {
                            let w = st.stack.pop().expect("v is still on the stack");
                            st.on_stack.insert(w.clone(), false);
                            let is_v = &w == v;
                            component.push(w);
                            if is_v {
                                break;
                            }
                        }
                        component.sort();
                        st.components.push(component);
                    }
                }
            }
        }
    }

    let mut of = BTreeMap::new();
    for (i, members) in st.components.iter().enumerate() {
        for k in members {
            of.insert(k.clone(), i);
        }
    }
    // A DFS finishes node `v` only after every node reachable from it, so
    // when an edge `u -> v` crosses two components, `v`'s component finishes
    // (and is pushed) strictly before `u`'s. Collection order is therefore
    // already dependency-first, which is the order `Stratification` wants --
    // no reversal.
    let order: Vec<usize> = (0..st.components.len()).collect();

    Sccs {
        of,
        members: st.components,
        order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> PredicateKey {
        PredicateKey::new(name, 1)
    }

    fn graph_of(edges: &[(&str, &str)]) -> DepGraph {
        let mut g = DepGraph::new();
        for (from, to) in edges {
            g.entry(key(from)).or_default().push(Edge {
                to: key(to),
                negated: false,
                rule: RuleId(0),
                span: Span::synthetic(),
            });
        }
        g
    }

    #[test]
    fn a_two_cycle_is_one_recursive_component() {
        let g = graph_of(&[("a", "b"), ("b", "a")]);
        let sccs = tarjan(&g);
        assert_eq!(sccs.of(&key("a")), sccs.of(&key("b")));
        assert!(sccs.is_recursive(&key("a"), &g));
    }

    #[test]
    fn a_self_loop_is_recursive_even_alone() {
        let g = graph_of(&[("a", "a")]);
        let sccs = tarjan(&g);
        assert!(sccs.is_recursive(&key("a"), &g));
    }

    #[test]
    fn an_acyclic_chain_is_not_recursive() {
        let g = graph_of(&[("a", "b"), ("b", "c")]);
        let sccs = tarjan(&g);
        assert!(!sccs.is_recursive(&key("a"), &g));
        assert!(!sccs.is_recursive(&key("b"), &g));
        assert!(!sccs.is_recursive(&key("c"), &g));
        // Dependency-first: c before b before a.
        let pos = |k: &str| {
            sccs.order
                .iter()
                .position(|&i| sccs.members[i].contains(&key(k)))
                .expect("present")
        };
        assert!(pos("c") < pos("b"));
        assert!(pos("b") < pos("a"));
    }
}
