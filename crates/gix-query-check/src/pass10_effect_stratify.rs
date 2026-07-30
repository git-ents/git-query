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
//!
//! # Writes are declared; reads are computed
//!
//! `docs/query-and-rules.adoc` ("Triggers") specifies the effect graph as
//! built "from predicate dependency footprints", and that asymmetry is the
//! whole soundness argument. A write set is a promise about the future -- what
//! an executor *may* advance -- which no static analysis of a body can
//! compute, so it is declared. A read set is a property of a body that pass 7
//! already computes exactly, so declaring it would only introduce a way to
//! lie: an effect whose declaration reads nothing while its `when` clause
//! joins on `signer/2` still retriggers on every new claim. Reads therefore
//! come from [`crate::pass7_footprint`], mapped to ref namespaces by
//! [`read_namespaces`], and [`EffectDecl::reads`] is *ignored* by this pass.
//! Retiring that field and the parser clause behind it is item D2 of
//! `DEVPLAN-effect.md`; until then it parses, and nothing reads it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gix_query_ir::{
    Backing, EffectDecl, Footprint, Namespace, PredicateClass, PredicateKey, Program, Registry,
};

use crate::error::{CheckError, Errors};

/// The ref namespaces a [`Footprint`] reads, with the backings that denote no
/// ref namespace kept rather than dropped.
///
/// Produced by [`read_namespaces`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadNamespaces {
    /// One namespace per [`Backing::RefGlob`] any reachable host predicate
    /// declares, deduplicated, in namespace order.
    pub namespaces: Vec<Namespace>,
    /// Reachable host predicates whose backing is *not* a ref glob, and which
    /// therefore contribute no edge to the effect graph.
    ///
    /// A [`Backing::ContentAddressed`] predicate answers from an object id the
    /// call already supplies, and a [`Backing::AdHoc`] one from a file handed
    /// to this run with `--edb`; neither is a ref, so neither is something an
    /// effect's declared write set can advance. Dropping them is correct, but
    /// dropping them *silently* would leave a reader unable to tell the
    /// correct case from a mapping that forgot a `Backing` variant, so they
    /// are carried here and a caller may report them.
    pub non_ref_backed: BTreeSet<PredicateKey>,
}

/// Map a footprint to the ref namespaces it reads, totally and explicitly.
///
/// The mapping walks [`Footprint::base`] against `registry` and matches every
/// [`Backing`] variant by hand rather than going through
/// `Footprint::ref_globs` -- the precomputed glob set is the same answer, but
/// it has already discarded which predicates contributed nothing, and this
/// check's incompleteness argument (`docs/query-and-rules.adoc`: sound and
/// incomplete, never admitting a cycle) is only auditable if what was
/// discarded is visible. See [`ReadNamespaces::non_ref_backed`].
#[must_use]
pub fn read_namespaces(footprint: &Footprint, registry: &Registry) -> ReadNamespaces {
    let mut namespaces: BTreeSet<Namespace> = BTreeSet::new();
    let mut non_ref_backed: BTreeSet<PredicateKey> = BTreeSet::new();
    for key in &footprint.base {
        let Some(entry) = registry.predicate(key) else {
            // Unresolved: pass 2's diagnostic, not ours.
            continue;
        };
        let PredicateClass::Edb { backing, .. } = &entry.class else {
            // An IDB predicate in `base` would be a pass 7 bug; it reads no
            // namespace of its own either way.
            continue;
        };
        let mut ref_backed = false;
        for b in backing {
            match b {
                Backing::RefGlob(glob) => {
                    ref_backed = true;
                    namespaces.insert(Namespace::new(glob.as_str()));
                }
                Backing::ContentAddressed | Backing::AdHoc => {}
            }
        }
        if !ref_backed {
            non_ref_backed.insert(key.clone());
        }
    }
    ReadNamespaces {
        namespaces: namespaces.into_iter().collect(),
        non_ref_backed,
    }
}

/// One effect as the acyclicity check sees it: a declared write set beside a
/// *computed* read set.
///
/// This is the input [`check_acyclicity`] takes, and the shape a caller
/// outside this crate -- `gix-effect` at effect-define time, through
/// `gix-query`'s facade -- assembles from an effect doc plus the footprint of
/// its parsed body.
#[derive(Debug, Clone)]
pub struct EffectReads {
    /// The declaration. Only `name`, `writes`, and `span` are consulted;
    /// `reads` is ignored (see this module's documentation, and D2).
    pub decl: EffectDecl,
    /// The ref namespaces the effect's body actually reads, from
    /// [`read_namespaces`] over its footprint.
    pub reads: Vec<Namespace>,
}

struct Edge {
    to: Arc<str>,
    namespace: Namespace,
}

type EffectGraph = BTreeMap<Arc<str>, Vec<Edge>>;

fn build(effects: &[EffectReads]) -> EffectGraph {
    let mut graph: EffectGraph = effects
        .iter()
        .map(|e| (e.decl.name.clone(), Vec::new()))
        .collect();
    for writer in effects {
        for w in &writer.decl.writes {
            for reader in effects {
                for r in &reader.reads {
                    if w.overlaps(r) {
                        graph
                            .entry(writer.decl.name.clone())
                            .or_default()
                            .push(Edge {
                                to: reader.decl.name.clone(),
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
/// other effects' *computed* read sets, leads back to itself.
///
/// The single entry point for the acyclicity property, shared by [`check`]
/// (module-resident declarations, during whole-program validation) and by
/// `gix-query`'s facade export (registered effect docs, at define time), so
/// that neither reimplements the graph, the marks, or the overlap test.
#[must_use]
pub fn check_acyclicity(effects: &[EffectReads]) -> Errors {
    let mut errors = Errors::new();
    let graph = build(effects);
    let spans: BTreeMap<Arc<str>, gix_query_ir::Span> = effects
        .iter()
        .map(|e| (e.decl.name.clone(), e.decl.span))
        .collect();

    let mut marks: BTreeMap<Arc<str>, Mark> = BTreeMap::new();
    for name in graph.keys() {
        if !marks.contains_key(name) {
            visit(name, &graph, &spans, &mut marks, &mut errors);
        }
    }
    errors
}

/// Pass 10 over a whole program: compute each module-resident effect's read
/// set from pass 7's footprints, then run [`check_acyclicity`].
///
/// An [`EffectDecl`] carries no body -- no trigger, no gate -- so the body
/// whose footprint is the effect's read set has to be found some other way,
/// and the only linkage a module offers is the name: `effect watch
/// writes("...")` beside `watch(X) :- ...` declares the effect whose trigger
/// is `watch`. Every program-defined predicate of that name, at any arity,
/// contributes its footprint. An effect naming no defined predicate reads
/// nothing, and so induces no edge -- which is the honest answer for a
/// declaration with no body in this program, and stops being reachable at all
/// once `DEVPLAN-effect.md` D1 makes the effect *doc* the single source of
/// truth and D2 removes module-resident declarations.
pub(crate) fn check(
    program: &Program,
    registry: &Registry,
    footprints: &BTreeMap<PredicateKey, Footprint>,
    errors: &mut Errors,
) {
    let effects: Vec<EffectReads> = program
        .effects()
        .iter()
        .map(|decl| {
            let mut footprint = Footprint::default();
            for (key, fp) in footprints {
                if key.name == decl.name {
                    footprint.absorb(fp);
                }
            }
            EffectReads {
                decl: decl.clone(),
                reads: read_namespaces(&footprint, registry).namespaces,
            }
        })
        .collect();
    errors.extend(check_acyclicity(&effects));
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
    use gix_query_ir::{Atom, EffectDecl, Literal, Namespace, Rule, Term};

    use super::*;
    use crate::error::Pass;

    fn ns(s: &str) -> Namespace {
        Namespace::new(s)
    }

    /// Build the reader/writer pairs directly, the way `gix-effect` will:
    /// declared writes, computed reads.
    fn reads(name: &str, reads: Vec<Namespace>, writes: Vec<Namespace>) -> EffectReads {
        EffectReads {
            decl: EffectDecl::new(name, Vec::new(), writes),
            reads,
        }
    }

    /// Worked example 3 from `ARCHITECTURE.md`: `build_on_push` writes the
    /// action-record namespace `release_on_green` reads, but
    /// `release_on_green`'s write (`refs/release/candidate`) appears in no
    /// trigger's read set, so the graph terminates.
    #[test]
    fn worked_example_three_is_accepted() {
        let errors = check_acyclicity(&[
            reads(
                "build_on_push",
                vec![ns("refs/heads/main")],
                vec![ns("refs/claims/action/kiln/*")],
            ),
            reads(
                "release_on_green",
                vec![ns("refs/claims/action/kiln/*")],
                vec![ns("refs/release/candidate")],
            ),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn independent_effects_are_accepted() {
        let errors = check_acyclicity(&[
            reads("a", vec![ns("refs/heads/a")], vec![ns("refs/out/a")]),
            reads("b", vec![ns("refs/heads/b")], vec![ns("refs/out/b")]),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_two_effect_cycle_is_pass_ten() {
        let errors = check_acyclicity(&[
            reads("a", vec![ns("refs/from_b")], vec![ns("refs/to_b")]),
            reads("b", vec![ns("refs/to_b")], vec![ns("refs/from_b")]),
        ]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].pass(), Pass::EffectStratification);
        assert!(matches!(errors[0], CheckError::EffectCycle { .. }));
    }

    #[test]
    fn a_self_cycle_is_pass_ten() {
        let errors = check_acyclicity(&[reads("loopy", vec![ns("refs/x")], vec![ns("refs/x")])]);
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
        let errors = check_acyclicity(&[
            reads(
                "writer",
                vec![ns("refs/heads/main")],
                vec![ns("refs/claims/action/kiln/main")],
            ),
            reads(
                "reader",
                vec![ns("refs/claims/*")],
                vec![ns("refs/release/candidate")],
            ),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_write_with_no_matching_reader_leaves_no_edge() {
        let errors = check_acyclicity(&[reads(
            "solo",
            vec![ns("refs/heads/main")],
            vec![ns("refs/release/candidate")],
        )]);
        assert!(errors.is_empty(), "{errors:?}");
    }

    /// `signer/2` is claim-backed, so a body joining on it reads
    /// `refs/claims/*` whatever the declaration says.
    #[test]
    fn a_claim_backed_footprint_maps_to_the_claims_namespace() {
        let registry = Registry::host();
        let mut footprint = Footprint::default();
        footprint.base.insert(PredicateKey::new("signer", 2));
        footprint.base.insert(PredicateKey::new("parent", 2));
        let mapped = read_namespaces(&footprint, &registry);
        assert_eq!(mapped.namespaces, vec![ns("refs/claims/*")]);
        // `parent/2` is content-addressed: no namespace, and visibly so.
        assert!(
            mapped
                .non_ref_backed
                .contains(&PredicateKey::new("parent", 2))
        );
    }

    /// A program whose effect declaration reads nothing relevant, but whose
    /// trigger body's *footprint* lands inside its own write set. The old
    /// declared-reads graph saw no edge here; the footprint graph sees a self
    /// cycle. This is the soundness fix, so it is the test that matters most.
    #[test]
    fn a_cycle_invisible_in_declared_reads_is_rejected() {
        let program = program_with_claim_reading_effect(vec![ns("refs/claims/*")]);
        let errors = run(&program);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(matches!(
            errors[0],
            CheckError::EffectCycle { ref namespace, .. }
                if namespace.as_str() == "refs/claims/*"
        ));
    }

    /// The same program, writing somewhere its footprint does not reach:
    /// accepted.
    #[test]
    fn a_footprint_overlapping_no_write_set_is_accepted() {
        let program = program_with_claim_reading_effect(vec![ns("refs/release/candidate")]);
        let errors = run(&program);
        assert!(errors.is_empty(), "{errors:?}");
    }

    /// `watch(M) :- signer(C, M).` plus `effect watch reads("refs/nothing")
    /// writes(...)` -- a declared read set that overlaps nothing, over a body
    /// that reads `refs/claims/*`.
    fn program_with_claim_reading_effect(writes: Vec<Namespace>) -> Program {
        let mut module = gix_query_ir::Module::new("m");
        module.rules.push(Rule::new(
            Atom::new("watch", vec![Term::var("M")]),
            vec![Literal::Positive(Atom::new(
                "signer",
                vec![Term::var("C"), Term::var("M")],
            ))],
        ));
        module
            .effects
            .push(EffectDecl::new("watch", vec![ns("refs/nothing")], writes));
        let mut program = Program::new();
        program.add_module(module);
        program
    }

    fn run(program: &Program) -> Errors {
        let mut errors = Errors::new();
        let registry = crate::registry_build::build(program, &Registry::host(), &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let stratify = crate::pass5_stratify::check(program, &registry, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let footprints = crate::pass7_footprint::compute(program, &registry, &stratify);
        check(program, &registry, &footprints, &mut errors);
        errors
    }
}
