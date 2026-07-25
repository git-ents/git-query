/// The kernel's rules in this project's own syntax, byte for byte equivalent
/// to the compiled `ascent!` block.
///
/// Two implementations, written independently against one specification, is a
/// stronger guarantee than a code generator that nothing exercises. Keeping the
/// text form here rather than in a test file makes the pairing visible: change
/// one and the differential test fails until you change the other.
///
/// Every EDB relation is enumerable and every rule is range-restricted and
/// stratified, so this program passes the validator without any mode
/// declarations -- the kernel's facts arrive as one transaction's worth of
/// tuples, not as a repository to walk.
pub const KERNEL_RULES: &str = r"
% Transitive ancestry.
ancestor(C, P) :- parent(C, P).
ancestor(C, A) :- parent(C, P), ancestor(P, A).

has_parent(C) :- parent(C, _).

% Commits a ref's old tip already reached.
covered(R, O) :- ref_advance(R, O, _).
covered(R, A) :- ref_advance(R, O, _), ancestor(O, A).

% The new tip and its ancestors, minus what the old tip already reached.
introduced(R, N) :- ref_advance(R, _, N), !covered(R, N).
introduced(R, C) :- ref_advance(R, _, N), ancestor(N, C), !covered(R, C).
introduced(R, N) :- ref_create(R, N).
introduced(R, C) :- ref_create(R, N), ancestor(N, C).

member_signed(C) :- signed_by(C, K), member(K).
admin_signed(C)  :- signed_by(C, K), admin(K).

% Fast-forward-only: the anti-replay binding a signed commit relies on.
ff_violation(R) :- ref_advance(R, O, N), O != N, !ancestor(N, O).

% Creation must point at a parentless genesis commit.
genesis_violation(R) :- ref_create(R, N), has_parent(N).

% One entity, one root. A merge can satisfy fast-forward while smuggling in an
% unrelated parentless chain -- a doppelganger identity.
second_root_violation(R, C) :-
    ref_advance(R, _, _), introduced(R, C), !has_parent(C).

unsigned_violation(R, C) :- introduced(R, C), !member_signed(C).

% Retention is two objects, not one.
dangling_anchor_violation(R, T) :-
    introduced(R, C), anchor_blob(C, T), !object_exists(T).
dangling_context_violation(R, T) :-
    introduced(R, C), context_blob(C, T), !object_exists(T).

% Authoring an effect schedules code execution on canonical infrastructure,
% which needs more trust than an ordinary append.
effect_admin_violation(R, C) :-
    effect_ref(R), introduced(R, C), !admin_signed(C).
";

/// The kernel's EDB, as `(name, arity)` pairs.
///
/// The kernel runs over a fact universe of its own: one proposed transaction's
/// tuples, supplied whole, with no repository behind them. So these predicates
/// are enumerable and their backing is ad hoc rather than a ref glob -- there
/// is no ref to snapshot, and the facts enter a cache key through the run's own
/// digest if they enter one at all.
#[must_use]
pub fn kernel_predicates() -> Vec<(&'static str, usize)> {
    vec![
        ("ref_create", 2),
        ("ref_advance", 3),
        ("parent", 2),
        ("signed_by", 2),
        ("member", 1),
        ("admin", 1),
        ("anchor_blob", 2),
        ("context_blob", 2),
        ("effect_ref", 1),
        ("object_exists", 1),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DENIALS;

    #[test]
    fn every_denial_relation_has_a_rule_in_the_text_form() {
        // The two forms are kept in sync by hand, so the cheapest possible
        // check that nobody added a rule to one and not the other earns its
        // place -- the real differential test needs the evaluator, this does
        // not.
        for name in DENIALS {
            assert!(
                KERNEL_RULES.contains(&format!("{name}(")),
                "{name} is missing from the text form"
            );
        }
    }

    #[test]
    fn every_edb_predicate_is_referenced_by_the_text_form() {
        for (name, _) in kernel_predicates() {
            assert!(
                KERNEL_RULES.contains(&format!("{name}(")),
                "{name} is declared but never used"
            );
        }
    }
}
