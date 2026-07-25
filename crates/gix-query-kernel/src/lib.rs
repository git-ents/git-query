//! The frozen denial rules that govern who may push a rule ref, compiled
//! rather than interpreted.
//!
//! Rule modules are repository data, and repository data is exactly what these
//! rules adjudicate. That circularity is the whole reason this crate exists:
//! the rules deciding whether a rule ref may move cannot themselves be a rule
//! ref, so they are compiled in, frozen, and small enough to read in one
//! sitting. Everything else in the workspace is the dynamic, interpreted path.
//!
//! [`ascent`] embeds Datalog in Rust via a proc macro, so rustc checks these
//! rules' types, arities, variable bindings, and stratification. A violation is
//! a non-empty relation, and [`gate`] collects them.
//!
//! # Why the rules here differ from their ancestor in `git-ents`
//!
//! These were ported from `ents-gate-rules`, and three things changed in the
//! port. Each removes a construct that Rust could express and this project's
//! Datalog deliberately cannot -- because this crate is not only the bootstrap
//! but the **differential test oracle**: the interpreter is fed these same
//! rules in text syntax over the same facts, and the two must agree relation
//! for relation. A rule the interpreter cannot state is a rule the test cannot
//! cover.
//!
//! - *An optional old tip became two relations.* `ref_update(Ref, Option<Oid>,
//!   Oid)` with `if let Some(o) = old` guards is a sum type in an argument
//!   position, which is a compound term. [`GateRules::ref_create`] and
//!   [`GateRules::ref_advance`] say the same thing with no type constructor.
//! - *A role enum became a second relation.* `member(Key, Role)` matched
//!   against `Role::Admin` is the same problem in smaller clothes.
//!   [`GateRules::member`] and [`GateRules::admin`] replace it.
//! - *A refname prefix test became a fact.* `if r.starts_with("refs/meta/
//!   effects/")` is string computation inside a rule. [`GateRules::effect_ref`]
//!   moves it to the extractor, which is where it belonged anyway: deciding
//!   which namespace a refname falls in is parsing, not policy.
//!
//! The rules are otherwise unchanged, and the third change is an improvement on
//! its own merits rather than a concession.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use ascent::ascent;

mod text;

pub use text::{KERNEL_RULES, kernel_predicates};

/// An object id. A plain `String` so a rule's facts stay readable in tests;
/// any `Clone + Eq + Hash` type works once this is wired to a real extractor.
pub type Oid = String;
/// A refname.
pub type Ref = String;
/// A signing key's identity, standing in for a member's enrolled public key
/// material.
pub type Key = String;

ascent! {
    /// The compiled rule set: relations populated from [`Facts`] and run to a
    /// fixpoint by [`gate`]. Not the public surface -- [`Facts`] and [`gate`]
    /// are what a caller needs.
    struct GateRules;

    // ---- EDB: what an extractor pulls from the pack, the proposed
    // transaction, and current repository state ----

    /// Entity creation: a ref that did not exist now points at this tip.
    relation ref_create(Ref, Oid);
    /// Entity advance: (ref, old tip, new tip).
    relation ref_advance(Ref, Oid, Oid);
    /// (child, parent) commit edges for the new tips' ancestry, bounded at the
    /// old tips.
    relation parent(Oid, Oid);
    /// (commit, signing key), emitted only after signature verification
    /// succeeds -- the cryptography lives in the extractor, never in a rule.
    relation signed_by(Oid, Key);
    /// Keys currently enrolled.
    relation member(Key);
    /// Keys enrolled by an admin-registered member. A subset of `member`.
    relation admin(Key);
    /// (entity commit, anchored blob) -- the first of the two objects an
    /// anchor's retention requires.
    relation anchor_blob(Oid, Oid);
    /// (entity commit, context blob) -- the second. Both must resolve.
    relation context_blob(Oid, Oid);
    /// Refnames in the effects namespace, classified by the extractor.
    relation effect_ref(Ref);
    /// Objects the repository already has, or that arrive in this pack.
    relation object_exists(Oid);

    // ---- IDB ----

    /// Transitive ancestry.
    relation ancestor(Oid, Oid);
    ancestor(c.clone(), p.clone()) <-- parent(c, p);
    ancestor(c.clone(), a.clone()) <-- parent(c, p), ancestor(p, a);

    /// Whether a commit has any recorded parent.
    relation has_parent(Oid);
    has_parent(c.clone()) <-- parent(c, _p);

    /// Commits a ref's old tip already reached.
    relation covered(Ref, Oid);
    covered(r.clone(), o.clone()) <-- ref_advance(r, o, _new);
    covered(r.clone(), a.clone()) <-- ref_advance(r, o, _new), ancestor(o, a);

    /// Commits this transaction introduces to a ref: the new tip and its
    /// ancestors, minus everything the old tip already reached.
    relation introduced(Ref, Oid);
    introduced(r.clone(), n.clone()) <-- ref_advance(r, _o, n), !covered(r, n);
    introduced(r.clone(), c.clone()) <--
        ref_advance(r, _o, n), ancestor(n, c), !covered(r, c);
    introduced(r.clone(), n.clone()) <-- ref_create(r, n);
    introduced(r.clone(), c.clone()) <-- ref_create(r, n), ancestor(n, c);

    /// A commit signed by any currently enrolled member.
    relation member_signed(Oid);
    member_signed(c.clone()) <-- signed_by(c, k), member(k);

    /// A commit signed by an admin-registered member specifically.
    relation admin_signed(Oid);
    admin_signed(c.clone()) <-- signed_by(c, k), admin(k);

    // ---- Denial rules: any row rejects the transaction ----

    /// Fast-forward-only. This is the anti-replay binding a signed commit
    /// relies on.
    relation ff_violation(Ref);
    ff_violation(r.clone()) <--
        ref_advance(r, o, n), if o != n, !ancestor(n, o);

    /// Creation must point at a parentless genesis commit.
    relation genesis_violation(Ref);
    genesis_violation(r.clone()) <-- ref_create(r, n), has_parent(n);

    /// One entity, one root. Past genesis an update may not introduce a second
    /// parentless commit: merging an unrelated chain would satisfy
    /// fast-forward while smuggling in a doppelganger identity.
    relation second_root_violation(Ref, Oid);
    second_root_violation(r.clone(), c.clone()) <--
        ref_advance(r, _o, _n), introduced(r, c), !has_parent(c);

    /// Every introduced commit must carry a signature from a currently
    /// enrolled member.
    relation unsigned_violation(Ref, Oid);
    unsigned_violation(r.clone(), c.clone()) <--
        introduced(r, c), !member_signed(c);

    /// An anchored blob must resolve to an object the repository will contain.
    relation dangling_anchor_violation(Ref, Oid);
    dangling_anchor_violation(r.clone(), t.clone()) <--
        introduced(r, c), anchor_blob(c, t), !object_exists(t);

    /// The paired context blob must resolve too. Retention is two objects, not
    /// one.
    relation dangling_context_violation(Ref, Oid);
    dangling_context_violation(r.clone(), t.clone()) <--
        introduced(r, c), context_blob(c, t), !object_exists(t);

    /// A write to the effects namespace must be signed by an admin-registered
    /// member, not merely by any member. Authoring an effect schedules code
    /// execution on canonical infrastructure, which needs more trust than an
    /// ordinary append.
    relation effect_admin_violation(Ref, Oid);
    effect_admin_violation(r.clone(), c.clone()) <--
        effect_ref(r), introduced(r, c), !admin_signed(c);
}

/// Facts for one proposed transaction.
///
/// In the real system an extractor pulls these with `gix` from the pack and
/// from current ref and member state; here they are supplied directly, so a
/// rule's behavior can be pinned by a test and so the same facts can be handed
/// to the interpreter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// See [`GateRules::ref_create`].
    pub ref_create: Vec<(Ref, Oid)>,
    /// See [`GateRules::ref_advance`].
    pub ref_advance: Vec<(Ref, Oid, Oid)>,
    /// See [`GateRules::parent`].
    pub parent: Vec<(Oid, Oid)>,
    /// See [`GateRules::signed_by`].
    pub signed_by: Vec<(Oid, Key)>,
    /// See [`GateRules::member`].
    pub member: Vec<(Key,)>,
    /// See [`GateRules::admin`].
    pub admin: Vec<(Key,)>,
    /// See [`GateRules::anchor_blob`].
    pub anchor_blob: Vec<(Oid, Oid)>,
    /// See [`GateRules::context_blob`].
    pub context_blob: Vec<(Oid, Oid)>,
    /// See [`GateRules::effect_ref`].
    pub effect_ref: Vec<(Ref,)>,
    /// See [`GateRules::object_exists`].
    pub object_exists: Vec<(Oid,)>,
}

/// The names of every denial relation, sorted. The differential test compares
/// exactly these.
pub const DENIALS: [&str; 7] = [
    "dangling_anchor_violation",
    "dangling_context_violation",
    "effect_admin_violation",
    "ff_violation",
    "genesis_violation",
    "second_root_violation",
    "unsigned_violation",
];

/// Every denial relation, as `(name, tuples)` pairs sorted by name, each
/// relation sorted and deduplicated.
///
/// This is the shape the differential test compares: the interpreter, fed
/// [`KERNEL_RULES`] over the same facts, must produce the same relations.
/// Comparing rendered messages instead would compare two renderings rather
/// than two answers.
#[must_use]
pub fn violations(facts: Facts) -> Vec<(&'static str, Vec<Vec<String>>)> {
    let mut rules = GateRules {
        ref_create: facts.ref_create,
        ref_advance: facts.ref_advance,
        parent: facts.parent,
        signed_by: facts.signed_by,
        member: facts.member,
        admin: facts.admin,
        anchor_blob: facts.anchor_blob,
        context_blob: facts.context_blob,
        effect_ref: facts.effect_ref,
        object_exists: facts.object_exists,
        ..GateRules::default()
    };
    rules.run();

    fn normalize(mut v: Vec<Vec<String>>) -> Vec<Vec<String>> {
        v.sort();
        v.dedup();
        v
    }
    let unary =
        |rows: &[(String,)]| normalize(rows.iter().map(|(a,)| vec![a.clone()]).collect::<Vec<_>>());
    let binary = |rows: &[(String, String)]| {
        normalize(
            rows.iter()
                .map(|(a, b)| vec![a.clone(), b.clone()])
                .collect::<Vec<_>>(),
        )
    };

    vec![
        (DENIALS[0], binary(&rules.dangling_anchor_violation)),
        (DENIALS[1], binary(&rules.dangling_context_violation)),
        (DENIALS[2], binary(&rules.effect_admin_violation)),
        (DENIALS[3], unary(&rules.ff_violation)),
        (DENIALS[4], unary(&rules.genesis_violation)),
        (DENIALS[5], binary(&rules.second_root_violation)),
        (DENIALS[6], binary(&rules.unsigned_violation)),
    ]
}

/// Run every denial rule to a fixpoint over `facts`, returning refusal
/// messages. An empty result means the transaction is admitted under every
/// invariant this crate states.
#[must_use]
pub fn gate(facts: Facts) -> Vec<String> {
    let mut out = Vec::new();
    for (name, rows) in violations(facts) {
        for row in rows {
            let detail = match name {
                "ff_violation" => "new tip does not descend from old tip".to_owned(),
                "genesis_violation" => "creation tip has parents".to_owned(),
                "second_root_violation" => format!("introduces second root {}", row[1]),
                "unsigned_violation" => format!("{} not signed by an enrolled member", row[1]),
                "dangling_anchor_violation" => {
                    format!("anchored object {} does not exist", row[1])
                }
                "dangling_context_violation" => {
                    format!("context object {} does not exist", row[1])
                }
                _ => format!("{} not signed by an admin-registered member", row[1]),
            };
            out.push(format!("{name}: {}: {detail}", row[0]));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_owned()
    }

    fn admitted_creation() -> Facts {
        Facts {
            ref_create: vec![(s("refs/meta/rules/review"), s("g"))],
            signed_by: vec![(s("g"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            ..Facts::default()
        }
    }

    #[test]
    fn a_signed_genesis_creation_is_admitted() {
        assert!(gate(admitted_creation()).is_empty());
    }

    #[test]
    fn an_unsigned_commit_is_refused() {
        let mut f = admitted_creation();
        f.signed_by.clear();
        let out = gate(f);
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("unsigned_violation:"), "{out:?}");
    }

    #[test]
    fn a_creation_pointing_at_a_child_is_refused() {
        let f = Facts {
            ref_create: vec![(s("refs/meta/rules/review"), s("c1"))],
            parent: vec![(s("c1"), s("g"))],
            signed_by: vec![(s("c1"), s("key:joey")), (s("g"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            ..Facts::default()
        };
        let out = gate(f);
        assert!(
            out.iter().any(|m| m.starts_with("genesis_violation:")),
            "{out:?}"
        );
    }

    #[test]
    fn a_non_fast_forward_advance_is_refused() {
        let f = Facts {
            ref_advance: vec![(s("refs/meta/rules/review"), s("a"), s("b"))],
            signed_by: vec![(s("b"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            ..Facts::default()
        };
        let out = gate(f);
        assert!(
            out.iter().any(|m| m.starts_with("ff_violation:")),
            "{out:?}"
        );
    }

    #[test]
    fn a_fast_forward_advance_is_admitted() {
        let f = Facts {
            ref_advance: vec![(s("refs/meta/rules/review"), s("g"), s("c1"))],
            parent: vec![(s("c1"), s("g"))],
            signed_by: vec![(s("c1"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            ..Facts::default()
        };
        let out = gate(f);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_second_root_smuggled_through_a_merge_is_refused() {
        // Fast-forward is satisfied -- `c2` descends from `g` -- but the merge
        // brings in a parentless `x`, which is a doppelganger identity.
        let f = Facts {
            ref_advance: vec![(s("refs/meta/rules/review"), s("g"), s("c2"))],
            parent: vec![(s("c2"), s("g")), (s("c2"), s("x"))],
            signed_by: vec![(s("c2"), s("key:joey")), (s("x"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            ..Facts::default()
        };
        let out = gate(f);
        assert!(
            out.iter().any(|m| m.starts_with("second_root_violation:")),
            "{out:?}"
        );
    }

    #[test]
    fn an_effect_ref_needs_an_admin_signature() {
        let base = |admin: bool| Facts {
            ref_create: vec![(s("refs/meta/effects/build"), s("g"))],
            effect_ref: vec![(s("refs/meta/effects/build"),)],
            signed_by: vec![(s("g"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            admin: if admin {
                vec![(s("key:joey"),)]
            } else {
                vec![]
            },
            ..Facts::default()
        };
        assert!(
            gate(base(false))
                .iter()
                .any(|m| m.starts_with("effect_admin_violation:"))
        );
        assert!(gate(base(true)).is_empty());
    }

    #[test]
    fn retention_needs_both_objects() {
        let f = Facts {
            ref_create: vec![(s("refs/meta/rules/review"), s("g"))],
            signed_by: vec![(s("g"), s("key:joey"))],
            member: vec![(s("key:joey"),)],
            anchor_blob: vec![(s("g"), s("blob"))],
            context_blob: vec![(s("g"), s("ctx"))],
            object_exists: vec![(s("blob"),)],
            ..Facts::default()
        };
        let out = gate(f);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("dangling_context_violation:"), "{out:?}");
    }

    #[test]
    fn violations_reports_every_denial_relation() {
        // The differential test compares this list against the interpreter's,
        // so a rule added to the ascent block and forgotten here would silently
        // drop out of the comparison.
        let names: Vec<&str> = violations(Facts::default())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, DENIALS.to_vec());
        assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted by name");
    }
}
