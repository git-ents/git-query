//! Pass 10 -- effect stratification.
//!
//! Not one of the nine `docs/query-and-rules.adoc` passes: `ARCHITECTURE.md`
//! describes validation as "9 passes plus effect-stratification over declared
//! write sets", and this is the "plus". It runs over [`EffectDecl`]s rather
//! than rules, because the effect executor -- trigger detection, idempotence
//! keys, `refs/intent/*`, a `git effect` CLI -- does not exist yet; what does
//! exist is enough to decide the one property git-effect's charter names as
//! "decidable by construction": whether a chain of effects can retrigger
//! itself.
//!
//! An effect that writes a namespace another effect reads is an edge: the
//! first effect's signed transition can produce facts the second effect's
//! trigger watches. A cycle in that graph is the fork bomb -- a chain of
//! signed transitions with no reason to ever stop -- and it is rejected the
//! same way pass 5 rejects a negation cycle: build the graph, walk it, and
//! name the exact edge that closes the loop.

use std::collections::BTreeMap;
use std::sync::Arc;

use gix_query_ir::{EffectDecl, Namespace, Program};

use crate::error::{CheckError, Errors};

struct Edge {
    to: Arc<str>,
    namespace: Namespace,
}

type EffectGraph = BTreeMap<Arc<str>, Vec<Edge>>;

fn build(effects: &[EffectDecl]) -> EffectGraph {
    let mut graph: EffectGraph = effects
        .iter()
        .map(|e| (e.name.clone(), Vec::new()))
        .collect();
    for writer in effects {
        for w in &writer.writes {
            for reader in effects {
                for r in &reader.reads {
                    if w.overlaps(r) {
                        graph.entry(writer.name.clone()).or_default().push(Edge {
                            to: reader.name.clone(),
                            namespace: w.clone(),
                        });
                    }
                }
            }
        }
    }
    graph
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// On the current DFS path: an edge back into this node is a cycle.
    InProgress,
    /// Fully explored; no path from here closes a cycle.
    Done,
}

/// Check that no effect's declared write set, followed transitively through
/// other effects' declared reads, leads back to itself.
pub(crate) fn check(program: &Program, errors: &mut Errors) {
    let effects = program.effects();
    let graph = build(effects);
    let spans: BTreeMap<Arc<str>, gix_query_ir::Span> =
        effects.iter().map(|e| (e.name.clone(), e.span)).collect();

    let mut marks: BTreeMap<Arc<str>, Mark> = BTreeMap::new();
    for name in graph.keys() {
        if !marks.contains_key(name) {
            visit(name, &graph, &spans, &mut marks, errors);
        }
    }
}

fn visit(
    node: &Arc<str>,
    graph: &EffectGraph,
    spans: &BTreeMap<Arc<str>, gix_query_ir::Span>,
    marks: &mut BTreeMap<Arc<str>, Mark>,
    errors: &mut Errors,
) {
    marks.insert(node.clone(), Mark::InProgress);
    if let Some(edges) = graph.get(node) {
        for edge in edges {
            match marks.get(&edge.to) {
                None => visit(&edge.to, graph, spans, marks, errors),
                Some(Mark::InProgress) => {
                    errors.push(CheckError::EffectCycle {
                        from: node.clone(),
                        to: edge.to.clone(),
                        namespace: edge.namespace.clone(),
                        span: spans
                            .get(node)
                            .copied()
                            .unwrap_or(gix_query_ir::Span::synthetic()),
                    });
                }
                Some(Mark::Done) => {}
            }
        }
    }
    marks.insert(node.clone(), Mark::Done);
}

#[cfg(test)]
mod tests {
    use gix_query_ir::{EffectDecl, Namespace};

    use super::*;
    use crate::error::Pass;

    fn ns(s: &str) -> Namespace {
        Namespace::new(s)
    }

    fn add_effect(program: &mut Program, decl: EffectDecl) {
        let mut m = gix_query_ir::Module::new("m");
        m.effects.push(decl);
        program.add_module(m);
    }

    /// Worked example 3 from `ARCHITECTURE.md`: `build_on_push` writes the
    /// action-record namespace `release_on_green` reads, but
    /// `release_on_green`'s write (`refs/release/candidate`) appears in no
    /// trigger's read set, so the graph terminates.
    #[test]
    fn worked_example_three_is_accepted() {
        let mut program = Program::new();
        add_effect(
            &mut program,
            EffectDecl::new(
                "build_on_push",
                vec![ns("refs/heads/main")],
                vec![ns("refs/claims/action/kiln/*")],
            ),
        );
        add_effect(
            &mut program,
            EffectDecl::new(
                "release_on_green",
                vec![ns("refs/claims/action/kiln/*")],
                vec![ns("refs/release/candidate")],
            ),
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn independent_effects_are_accepted() {
        let mut program = Program::new();
        add_effect(
            &mut program,
            EffectDecl::new("a", vec![ns("refs/heads/a")], vec![ns("refs/out/a")]),
        );
        add_effect(
            &mut program,
            EffectDecl::new("b", vec![ns("refs/heads/b")], vec![ns("refs/out/b")]),
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_two_effect_cycle_is_pass_ten() {
        let mut program = Program::new();
        add_effect(
            &mut program,
            EffectDecl::new("a", vec![ns("refs/from_b")], vec![ns("refs/to_b")]),
        );
        add_effect(
            &mut program,
            EffectDecl::new("b", vec![ns("refs/to_b")], vec![ns("refs/from_b")]),
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].pass(), Pass::EffectStratification);
        assert!(matches!(errors[0], CheckError::EffectCycle { .. }));
    }

    #[test]
    fn a_self_cycle_is_pass_ten() {
        let mut program = Program::new();
        add_effect(
            &mut program,
            EffectDecl::new("loopy", vec![ns("refs/x")], vec![ns("refs/x")]),
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].pass(), Pass::EffectStratification);
        assert!(matches!(
            errors[0],
            CheckError::EffectCycle { ref from, ref to, .. } if from == to
        ));
    }

    /// The prefix-nesting shape: a writer under a narrower namespace triggers
    /// a reader declared over the wider containing prefix, and vice versa.
    #[test]
    fn a_write_under_a_nested_prefix_overlaps_a_wider_read_prefix() {
        let mut program = Program::new();
        add_effect(
            &mut program,
            EffectDecl::new(
                "writer",
                vec![ns("refs/heads/main")],
                vec![ns("refs/claims/action/kiln/main")],
            ),
        );
        add_effect(
            &mut program,
            EffectDecl::new(
                "reader",
                vec![ns("refs/claims/*")],
                vec![ns("refs/release/candidate")],
            ),
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_write_with_no_matching_reader_leaves_no_edge() {
        let mut program = Program::new();
        add_effect(
            &mut program,
            EffectDecl::new(
                "solo",
                vec![ns("refs/heads/main")],
                vec![ns("refs/release/candidate")],
            ),
        );
        let mut errors = Errors::new();
        check(&program, &mut errors);
        assert!(errors.is_empty());
    }
}
