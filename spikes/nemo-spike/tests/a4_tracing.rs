//! Gate A4 -- tracing.
//!
//! `git query explain` needs a derivation tree for one tuple, obtained from
//! Rust. The gate is whether such a tree is programmatically reachable, and
//! whether each node identifies the rule applied and the leaf facts consumed
//! well enough that we can re-annotate the leaves against our own builtin call
//! log.

use nemo::execution::planning::normalization::atom::ground::GroundAtom;
use nemo::execution::tracing::trace::ExecutionTraceTree;
use nemo::rule_model::components::fact::Fact;
use nemo_spike::{Runner, Value, s};

/// A two-level derivation, so a tree has an interior node rather than only a
/// root and leaves.
const CHAIN: &str = "\
reach(?X, ?Y) :- parent(?X, ?Y) .
reach(?X, ?Z) :- parent(?X, ?Y), reach(?Y, ?Z) .";

/// Flatten a tree into `(depth, label)` pairs for coarse shape assertions.
fn shape(tree: &ExecutionTraceTree, depth: usize, out: &mut Vec<(usize, String)>) {
    match tree {
        ExecutionTraceTree::Fact(atom) => out.push((depth, format!("fact {}", atom.predicate()))),
        ExecutionTraceTree::Rule(application, children) => {
            out.push((depth, format!("rule {}", application.rule_index)));
            for child in children {
                shape(child, depth + 1, out);
            }
        }
    }
}

/// Every leaf of a tree, as `(predicate, arguments)` -- the shape `explain`
/// would join against the builtin call log.
fn leaves(tree: &ExecutionTraceTree, out: &mut Vec<(String, Vec<Value>)>) {
    match tree {
        ExecutionTraceTree::Fact(atom) => out.push(atom_of(atom)),
        ExecutionTraceTree::Rule(_, children) => {
            for child in children {
                leaves(child, out);
            }
        }
    }
}

fn atom_of(atom: &GroundAtom) -> (String, Vec<Value>) {
    (
        atom.predicate().to_string(),
        atom.datavalues()
            .map(|v| Value::from_nemo(&v).expect("value in our fragment"))
            .collect(),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn a4_a_trace_is_reachable_from_rust() {
    let mut session = Runner::new(CHAIN)
        .facts("parent", [s!("c", "b"), s!("b", "a")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("reach(\"c\", \"a\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let tree = trace.tree(handles[0]).expect("a tree for a derived fact");

    let mut got = Vec::new();
    shape(&tree, 0, &mut got);
    // reach(c,a) by the recursive rule, from parent(c,b) and reach(b,a);
    // reach(b,a) by the base rule, from parent(b,a).
    assert_eq!(
        got,
        vec![
            (0, "rule 1".to_owned()),
            (1, "fact parent".to_owned()),
            (1, "rule 0".to_owned()),
            (2, "fact parent".to_owned()),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a4_leaves_carry_predicate_and_argument_values() {
    let mut session = Runner::new(CHAIN)
        .facts("parent", [s!("c", "b"), s!("b", "a")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("reach(\"c\", \"a\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let tree = trace.tree(handles[0]).expect("tree");

    let mut got = Vec::new();
    leaves(&tree, &mut got);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("parent".to_owned(), vec![Value::sym("b"), Value::sym("a")]),
            ("parent".to_owned(), vec![Value::sym("c"), Value::sym("b")]),
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a4_interior_nodes_identify_the_rule_and_its_bindings() {
    let mut session = Runner::new(CHAIN)
        .facts("parent", [s!("c", "b"), s!("b", "a")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("reach(\"c\", \"a\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let tree = trace.tree(handles[0]).expect("tree");

    let ExecutionTraceTree::Rule(application, _) = &tree else {
        panic!("root is not a rule application");
    };
    assert_eq!(application.rule_index, 1);
    assert_eq!(application.head_index, 0);
    // The rule itself is available as a component, and the substitution that
    // instantiated it is available separately, so the two can be rendered or
    // matched independently.
    let rendered = format!("{}", application.rule);
    assert!(rendered.contains("reach"), "rule not rendered: {rendered}");
    let bindings = format!("{:?}", application.assignment);
    assert!(bindings.contains('X') || bindings.contains('Y') || bindings.contains('Z'));
}

#[tokio::test(flavor = "current_thread")]
async fn a4_a_negated_body_literal_leaves_no_node_in_the_tree() {
    // This is the finding. Nemo builds subtraces from `rule.positive_all()`;
    // negated literals are simply not visited, so a derivation that turned on
    // `~revoked(alice)` produces a tree that never mentions `revoked`. Our
    // `explain` output shows a negation witness, so that node has to come from
    // our own rewrite, not from the engine.
    let mut session = Runner::new("active(?M) :- member(?M), ~revoked(?M) .")
        .facts("member", [s!("alice"), s!("bob")])
        .facts("revoked", [s!("bob")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("active(\"alice\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let tree = trace.tree(handles[0]).expect("tree");

    let mut got = Vec::new();
    leaves(&tree, &mut got);
    assert_eq!(
        got,
        vec![("member".to_owned(), vec![Value::sym("alice")])],
        "the negated literal appeared after all, so this finding is stale"
    );

    // The rule component does still carry the negated literal, so a renderer
    // that consults the rule rather than only the tree can show it -- but it
    // gets no witness tuple, only the syntax.
    let ExecutionTraceTree::Rule(application, _) = &tree else {
        panic!("root is not a rule application");
    };
    assert!(format!("{}", application.rule).contains("revoked"));
}

#[tokio::test(flavor = "current_thread")]
async fn a4_an_underivable_fact_traces_to_no_tree() {
    let mut session = Runner::new(CHAIN)
        .facts("parent", [s!("c", "b")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("reach(\"c\", \"zzz\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    assert!(
        trace.tree(handles[0]).is_none(),
        "an underivable fact produced a tree"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a4_an_edb_fact_traces_to_a_single_leaf() {
    let mut session = Runner::new(CHAIN)
        .facts("parent", [s!("c", "b")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("parent(\"c\", \"b\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let tree = trace.tree(handles[0]).expect("tree");
    assert_eq!(tree.node_count(), 1);
    assert!(matches!(tree, ExecutionTraceTree::Fact(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn a4_traces_also_serialize_to_json_and_ascii() {
    // Not what we would build on -- the typed tree is better -- but worth
    // recording that the CLI-shaped outputs are reachable too.
    let mut session = Runner::new(CHAIN)
        .facts("parent", [s!("c", "b"), s!("b", "a")])
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("reach(\"c\", \"a\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let json = trace.json(&handles);
    let rendered = serde_json::to_string(&json).expect("serialize");
    assert!(rendered.contains("reach"), "{rendered}");

    let ascii = trace.tree(handles[0]).expect("tree").to_ascii_art();
    assert!(ascii.contains("reach"), "{ascii}");
}

#[tokio::test(flavor = "current_thread")]
async fn a4_only_one_derivation_comes_back_per_fact() {
    // A fact derivable two ways yields one tree, not all of them: `trace`
    // returns on the first head atom that unifies and fully derives. `explain`
    // therefore shows *a* derivation, not *the* derivations, which is a
    // product decision we should make deliberately rather than discover.
    let mut session = Runner::new(CHAIN)
        .facts(
            "parent",
            [s!("a", "b"), s!("a", "c"), s!("b", "d"), s!("c", "d")],
        )
        .run()
        .await
        .expect("run");

    let fact = Fact::parse("reach(\"a\", \"d\")").expect("parse fact");
    let (trace, handles) = session.engine.trace(vec![fact]).await.expect("trace");
    let tree = trace.tree(handles[0]).expect("tree");
    let mut got = Vec::new();
    leaves(&tree, &mut got);
    assert_eq!(got.len(), 2, "expected exactly one two-step derivation");
}
