# Nemo spike report

- **Date:** 2026-07-25
- **Subject:** [knowsys/nemo](https://github.com/knowsys/nemo) as the internal fixpoint engine behind an engine-agnostic seam
- **Pin:** tag `v0.9.1`, commit `4415bc2e180adf33a7a4b98ddc41be9914b7584e`
- **Spike:** `spikes/nemo-spike` — 55 tests, all passing, `cargo +nightly test -- --test-threads=1`

## Recommendation

**Adopt with caveats**, and the caveats are load-bearing enough that they should be read before the verdicts.

No rejection criterion fired: A1 passes, A2 matches on every golden program, A3 passes.
The evaluation core is genuinely good — set semantics, stratified negation, and termination are all correct on the first attempt, and ten thousand facts run to fixpoint in about five milliseconds.

Everything that is wrong with Nemo is packaging, not semantics, and two of those are project-level rather than gate-level:

1. **Nemo is nightly-only.**
   `nemo` and `nemo-physical` both use `#![feature(…)]`, so *any* crate that links them must be compiled by a nightly toolchain.
   Their `rust-toolchain.toml` pins the unversioned `nightly` channel.
   Evidence: `cargo +stable check` in the spike fails with `error[E0554]: #![feature] may not be used on the stable release channel`.
2. **Nemo is not on crates.io.**
   The published `nemo` crate stops at `0.2.1` and every version is yanked; `v0.9.1` exists only as a git tag.
   A git dependency is the only way in, and crates.io refuses to publish a crate that has one — so `gix-query-engine-nemo` could never be released.
   For a workspace whose whole `gix-*` convention is "this is a publishable library", that is a structural problem, not an inconvenience.

The consequence is that the seam has to carry real weight: Nemo cannot be the only engine, and the crate that wraps it has to be optional, non-default, and outside the published set.
That in turn promotes the naive evaluator from "correctness oracle" to "the engine that ships by default", which contradicts the non-goal the integration brief states ("the naive oracle is never optimized").
**That tension is the one decision this report cannot make on its own.**
See [Open decision](#open-decision).

## Gate verdicts

| Gate | Verdict | One-line |
| --- | --- | --- |
| A1 Programmatic embedding | **PASS** | Two routes; only one of them scales, and neither is called `assert_facts`. |
| A2 Semantics conformance | **PASS** | Every golden program matched hand-computed relations exactly, first run. |
| A3 Demand loop viability | **PASS** | Converges, stays correct across chained demand, 44 ms for 10k facts. |
| A4 Tracing | **PASS** with debts | A typed Rust tree, rule-identified and leaf-matchable — but negation is invisible in it. |
| A5 Resource behavior | **PARTIAL** | No cap, no timeout, no working cancellation. Truncation is not expressible. |
| A6 Fragment containment | **PASS** | The fragment is entirely ours to hold; three silent-wrong-answer traps found. |

---

### Gate A1 — Programmatic embedding · PASS

**Evidence:** `tests/a1_embedding.rs` (9 tests).

All four sub-questions are answerable from Rust with no disk access and no `nmo` invocation.

1. **Load from a string** — `nemo::api::load_string(String)`, async.
   `a1_a_program_loads_from_a_string_and_runs`.
2. **Assert typed tuples into named EDB predicates** — there is no `assert_facts` entry point.
   Two routes exist and both work:
   - **Inline**: serialize each tuple as a Nemo fact and prepend it to the
     program text. `ExecutionEngine::initialize` turns `program.facts()` into a
     `SimpleTable` EDB source. `a1_facts_assert_inline_with_both_value_types`.
   - **Memory import**: implement `nemo::io::resource_providers::ResourceProvider` to serve CSV bytes from a `String`, hand it to the engine through `ExecutionParameters::set_import_manager`, and reference it from an `@import` directive.
     No temp file, no working-directory dependence.
     `a1_facts_assert_from_memory_without_touching_disk`.
3. **Run to fixpoint** — `engine.execute()`, async.
4. **Iterate a named predicate with stable types** — `engine.predicate_rows(&Tag) -> Option<impl Iterator<Item = Vec<AnyDataValue>>>`.
   Integers come back as integers, symbols as plain strings.
   An underived or entirely unknown predicate yields the empty relation rather than an error, which is what the CLI's exit-code-1 case needs (`a1_an_underived_predicate_reads_back_empty_rather_than_erroring`).

**The two routes are not interchangeable.**
The inline route is parse-bound and unusable at any real scale:

| route | 10 000 binary facts | |
| --- | --- | --- |
| inline (program text) | **1.738 s** | 189 KB of text at ≈109 KB/s |
| memory import (CSV) | **6.5 ms** | load 1.0 ms + execute 5.4 ms |

`tests/a3_cost_breakdown.rs`, release build.
The evaluation is 5 ms in both cases; the 1.7 s is Nemo's `nom` program parser.
**EDB must never travel as program text.**

The inline route has a second, worse defect.
Nemo's string literal lexer is `is_not("\"")` — everything up to the next quote, with **no escape processing whatsoever**.
Therefore:

- A `"` cannot appear in a program-text symbol at all, escaped or otherwise.
  Git permits `"` in path names and author names, so this is reachable from real data.
  `a1_a_double_quote_in_a_symbol_cannot_be_written_as_a_fact`.
- Emitting `\t` produces the two characters `\` and `t`.
  Any emitter that escapes on the way in silently corrupts, because nothing unescapes on the way out.
  The correct lowering escapes nothing and *refuses* the quote.
  `a1_a_backslash_in_a_symbol_survives_because_nothing_escapes_it`.
- The CSV route is unaffected — CSV has its own quoting.
  `a1_the_memory_import_route_carries_symbols_program_text_cannot`.

This binds constants written into *rules*, which do go through program text.
A rule module stored in a ref that mentions a path containing `"` cannot be lowered.
That has to be a validator error with a real diagnostic, not a lowering panic.

---

### Gate A2 — Semantics conformance · PASS

**Evidence:** `tests/a2_semantics.rs` (10 tests).
Every assertion is exact set equality against hand-computed relations.
No mismatches, no workarounds.

1. **Transitive closure, set semantics** — a diamond graph, so `a → d` is derivable by two paths.
   It appears once.
   Asserted twice: against the read-back relation, and against `count_facts_in_memory_for_predicate`, so that the spike's own dedup cannot hide bag semantics.
   `a2_1_transitive_closure_is_a_set`.
   A cyclic graph terminates with the expected four pairs (`a2_1_a_cycle_terminates`).
2. **Stratified negation** — `active(M) :- member(M), ~revoked(M)` gives `{alice, carol}`.
   A three-stratum chain (`c` → `b` negating `c` → `a` negating `b`) gives `b = {y}`, `a = {x}`; wrong stratum order would over-derive `a`.
   `a2_2_*`.
3. **Retraction-shaped update** — run, add a `revoked` fact, run fresh: the downstream `reviewer` tuple disappears.
   The first session is unchanged by the second having run, and a third run with the original facts recovers the original answer, so nothing bled through a shared structure.
   `a2_3_a_retraction_shaped_update_has_no_state_bleed`.
4. **Multi-clause predicates and constants in heads** — the spec's
   `carries(same, intact).` shape, defined partly by ground facts and partly by
   a rule with a constant in the head, then consumed by a negated join.
   `a2_4_multi_clause_predicates_and_constants_in_heads`.
5. **Join with `!=`** — the `ambiguous` rule shape, one anchor projecting onto one revision at two positions.
   `a2_5_a_join_with_an_inequality_comparison`.
   Ordering comparisons on integers also hold (`a2_5_ordering_comparisons_hold_on_integers`).

**Unstratifiable programs.**
Rejected at load, before any evaluation, with a diagnostic that names stratification.
Both the direct case (`p(X) :- q(X), ~p(X)`) and the mutual case (`p` negates `r`, `r` negates `p`).
So a program that leaked past our validator fails loudly rather than hanging or answering wrongly.
`a2_an_unstratifiable_program_is_rejected_at_load`, `a2_mutual_recursion_through_negation_is_also_rejected`.

---

### Gate A3 — Demand loop viability · PASS

**Evidence:** `tests/a3_demand.rs` (7 tests), `src/demand.rs`.

`p(X) :- q(A, Rev), anchor_at(A, Rev, B)` lowers to two rules:

```text
demand_anchor_at(?A, ?R) :- q(?A, ?R) .
p(?B) :- q(?A, ?R), anchor_at_ext(?A, ?R, ?B) .
```

The outer loop runs the program, reads `demand_anchor_at`, calls the stubbed builtin for each pair not already answered, appends the answers to `anchor_at_ext` as ordinary EDB, and runs again.
It stops when a round asks for nothing new.

- **Flat query** converges in two rounds — one to ask and answer, one to
  observe that nothing new was asked (`a3_one_round_answers_a_flat_query`).
- **Chained demand** works: with an answer feeding a rule that derives a new
  `q`, three calls are discovered in rounds 1, 2 and 3 and the loop converges in
  round 4, with the correct final relation.
  `a3_a_second_round_of_demand_converges_and_is_correct`.
- **A cycle in the chain terminates** — the call log is the memo, and a call
  already answered is not re-asked. `a3_a_cycle_in_the_chain_still_terminates`.
- **The builtin is never enumerated** — the stub table holds ten thousand rows and exactly one call is made.
  This is the mode discipline actually working.
  `a3_the_builtin_is_never_enumerated`.
- **A call with no answers is not a failure**, and is still logged, because a
  footprint audit needs to know it was made.
- **The call log** records builtin, bound arguments, answer count and round —
  which is what `explain` re-annotation needs, since the engine's own trace
  cannot supply it.

**Timing, 10 000 facts, release, memory-import route:** 44.7 ms total across 2 rounds (8.8 ms + 16.0 ms plus read-back).
Fine.

Two structural notes, both ours to carry rather than Nemo's to fix:

- **Every round re-runs the whole program from cold.**
  Nemo has no incremental entry point, so a *k*-round query pays *k* times.
  It is cheap here because evaluation is cheap; it will not be cheap on a query that derives millions of tuples and needs three rounds of demand.
- The magic-set rewrite that produces `demand_*` is assumed by the spike, not implemented.
  That is ours in every design, so it is not a Nemo risk — but it is also not evidence, and should not be read as such.

---

### Gate A4 — Tracing · PASS, with two debts

**Evidence:** `tests/a4_tracing.rs` (8 tests).

Traces are programmatically reachable and come back as a **typed Rust tree**, not a string or a JSON blob:

```rust
engine.trace(Vec<Fact>) -> (ExecutionTrace, Vec<TraceFactHandle>)
trace.tree(handle) -> Option<ExecutionTraceTree>

enum ExecutionTraceTree {
    Fact(GroundAtom),                                       // leaf
    Rule(TraceTreeRuleApplication, Vec<ExecutionTraceTree>) // interior
}
```

- **Leaves are matchable**: `GroundAtom::predicate()` and
  `GroundAtom::datavalues()` give predicate plus argument values, which is
  exactly the join key against our builtin call log.
  `a4_leaves_carry_predicate_and_argument_values`.
- **Interior nodes identify the rule**: `rule_index`, `head_index`, the `Rule`
  component itself, and the `Substitution` that instantiated it, separately —
  so rule and bindings can be rendered or matched independently.
  `a4_interior_nodes_identify_the_rule_and_its_bindings`.
- An EDB fact traces to a single leaf; an underivable fact yields `None` rather
  than an empty or fabricated tree.
- `to_ascii_art()` and `json()` also exist, but the typed tree is strictly
  better for our purposes.

**Debt 1 — negation leaves no node.**
`trace_recursive` builds subtraces from `rule.positive_all()`.
Negated body literals are never visited, so a derivation that turned on `~revoked(alice)` produces a tree that never mentions `revoked`.
Our `explain` output shows a negation witness, so **that node has to come from our own rewrite, not from the engine**.
The rule component does still carry the negated literal syntactically, so a renderer can show *that* the negation held — it just gets no witness tuple.
`a4_a_negated_body_literal_leaves_no_node_in_the_tree`.

**Debt 2 — one derivation, not all.**
`trace` returns on the first head atom that unifies and fully derives.
A fact derivable two ways yields one tree.
That is probably the right product behavior for `explain`, but it should be a decision we state rather than a limit we inherit.
`a4_only_one_derivation_comes_back_per_fact`.

---

### Gate A5 — Resource behavior · PARTIAL

**Evidence:** `tests/a5_resources.rs` (6 tests, one `#[ignore]`d because it does not come back).

**There is no cap, no timeout, and no cancellation token.**
`ExecutionParameters` exposes exactly three knobs — the import manager, global variables, and export selection.
Nothing bounds the chase.
`a5_there_is_no_row_cap_or_cancellation_parameter`.

**Async cancellation does not work.**
`execute()` is `async`, so `tokio::time::timeout` compiles — and does nothing.
Nemo's inner futures never yield to the runtime; the async is there for the import path, not for cooperative scheduling.
Measured: a 50 ms budget around a cross product that takes 1.16 s in release (14.95 s in debug) returned only when the chase finished.
`a5_a_timeout_around_execute_does_not_interrupt_the_chase`.

**Killing the thread is not an option either**, and this is worse than the gate anticipated:

- `nemo::error::Error` is **not `Send`** — it cannot cross a thread boundary.
  The whole API is `Rc`-based with `?Send` async, so neither an engine nor its errors can be moved between threads.
- `execute()` unconditionally enters a **process-global `Mutex<TimedCode>`** (`nemo_physical::meta::timing`).
  In a debug build it also `debug_assert!`s that no other thread is inside, so concurrent runs panic — and the panic **poisons the mutex, so every later run in the process fails too**.
  In release the assertion is compiled out and the mutex merely serializes.
  `a5_two_engines_cannot_run_concurrently_in_one_process`.

Consequences, all architectural:

- A watchdog thread cannot supervise an evaluation.
- Two engines cannot be raced in-process, which is exactly what a differential
  test harness wants to do.
- Any test suite touching the engine must run `--test-threads=1`.
- The only true abort is **killing a subprocess**.

**Memory** is observable but only post hoc: `engine.memory_usage()` returns a `MemoryUsage` tree broken down by predicate and chase step.
Useful for reporting, useless as a bound, because it can only be read after `execute()` returns.

**Truncation (exit 3) is not expressible.**
Nothing produces a partial answer plus a "this was cut short" signal.
The only bound available is **admission time**: count the EDB, project the cost, and refuse to start.
`a5_the_only_bound_available_is_bounding_the_input`.
That is a refusal, not a truncation, so it is honestly exit 2 — and a gate reading exit 2 as failure is already fail-closed, which is the behavior we want.
**v1 should not fake exit 3.**

---

### Gate A6 — Fragment containment · PASS

**Evidence:** `tests/a6_fragment.rs` (12 tests).

Nemo's language is far larger than ours.
Nothing in it refuses the parts we do not want, so the fragment boundary is entirely ours to hold — but our reader can be made to refuse anything that reaches us anyway, and it is.

**Three traps that produce silently wrong answers, not errors:**

1. **A bare identifier is an IRI, not a symbol.**
   `carries(same, intact).` is valid Nemo, but `same` lands in the `Iri` value domain.
   Our reader rejects `Iri` outright rather than guessing, which is why the emitter quotes every symbol.
   `a6_a_bare_identifier_is_an_iri_not_a_symbol`.
2. **A capitalized bare identifier is a constant, not a variable.**
   Our syntax says an initial capital is a variable; Nemo says variables are `?`-prefixed and any bare name is a constant.
   `out(X) :- in(X).` parses, validates, runs, and derives **nothing**.
   This cost the spike an afternoon and is by some distance the most dangerous item on the list, because there is no error at any stage.
   `a6_an_uppercase_bare_identifier_is_a_constant_not_a_variable`.
3. **An existential head mints a null.**
   `has_parent(?X, !P) :- node(?X).` produces a fresh `Null`, a value outside our two domains that would flow into output columns if the reader were permissive.
   It is not — the typed read panics rather than coerces.
   `a6_an_existential_head_mints_a_null_our_reader_refuses`.

**Coercion checks that came out right:**

- A quoted symbol stays `PlainString` even when it looks like an IRI (`http://example.com/thing`, `refs/heads/main`).
  No RDF-flavored reinterpretation.
  `a6_a_quoted_symbol_stays_a_plain_string_even_when_it_looks_like_an_iri`.
- The integer `42` and the symbol `"42"` are distinct and do not join.
  Object ids are hex and line numbers are integers; they must never meet.
- `i64::MIN`, `-1`, `0`, `i64::MAX` all round-trip in integer domains we accept.
- A language-tagged string is a different value from the bare string, and our
  reader rejects it.
- Our reader rejects every out-of-fragment domain tested: `Iri`, `Boolean`,
  `Double`, `Float`, `LanguageTaggedString`, `Other`.

**Features we could accidentally emit, confirmed available and confirmed not reachable from our IR** (no term-level operations exist in it):

| feature | syntax | status |
| --- | --- | --- |
| arithmetic | `?Z = ?X + ?Y` | works; we have no lowering path to it |
| aggregates | `total(#count(?X))` | works; we have no lowering path to it |
| existential heads | `p(?X, !Y)` | works, mints nulls; no lowering path |
| import/export directives | `@export p :- csv {}.` | parsed and carried into the engine |
| `magic_` prefix | `magic_p(?X)` | an ordinary name to Nemo; the reservation is ours to enforce |

The `@export` case deserves emphasis.
`execute()` does not itself write files, but the directive survives into `engine.exports()`, so any code path honoring exports would act on it.
**Rule modules are text loaded from refs.**
If that text reached the engine unfiltered, a repository could make a query write to disk.
The validator must reject directives *before* lowering.
`a6_import_and_export_directives_are_program_text_we_control`.

---

## API surface actually used

This is our exposure to Nemo's instability.
Every path below is `pub`, but several are plainly internal by intent — `execution::planning::normalization` is not a stable-looking address.

| path | used for |
| --- | --- |
| `nemo::api::{load_string, Engine}` | load from string |
| `nemo::execution::ExecutionEngine::{from_file, execute, predicate_rows, predicate_arity, exports, count_facts_in_memory_for_predicate, memory_usage, trace}` | run and read |
| `nemo::execution::execution_parameters::ExecutionParameters::set_import_manager` | inject the memory provider |
| `nemo::execution::tracing::trace::{ExecutionTrace, ExecutionTraceTree, TraceTreeRuleApplication, TraceFactHandle}` | derivation trees |
| `nemo::execution::planning::normalization::atom::ground::GroundAtom` | trace leaves — **deep, internal-looking** |
| `nemo::io::ImportManager` | ditto |
| `nemo::io::resource_providers::{ResourceProvider, ResourceProviders}` | in-memory EDB |
| `nemo::rule_file::RuleFile` | program + label |
| `nemo::rule_model::components::tag::Tag` | predicate names |
| `nemo::rule_model::components::fact::Fact` | trace input |
| `nemo::datavalues::{AnyDataValue, DataValue, ValueDomain}` | value round trip |
| `nemo::error::Error` | error type — **not `Send`** |
| `nemo_physical::{error::ReadingError, resource::Resource}` | resource provider impl |

## Dependency and toolchain exposure

- **230 unique crates** in the transitive tree, for a git plumbing tool.
- Among them: `reqwest` (full HTTP client with gzip, brotli, zstd, deflate), `tower-lsp` (a language-server implementation), `oxrdf` / `oxrdfio` / `spargebra` (RDF and SPARQL), `ariadne` + `colored` (diagnostic rendering).
  None of these are reachable from anything we do.
- Nightly-only, against the unpinned `nightly` channel upstream.
  The spike builds on `1.99.0-nightly (89c61a754 2026-07-23)`; that is a fact about today, not a guarantee about next week.
- Git dependency, so no crates.io publication of the wrapping crate, no
  registry-mediated semver, and no RustSec advisory coverage through the
  registry.

## Workarounds carried by the spike

| # | Workaround | Where |
| --- | --- | --- |
| 1 | Facts asserted by emitting Nemo syntax or in-memory CSV, since no `assert_facts` API exists | `src/runner.rs` |
| 2 | Custom `ResourceProvider` serving CSV from a `String`, so no temp file is needed | `src/runner.rs::MemoryResourceProvider` |
| 3 | The CSV column-type list is derived per column from the first tuple, because CSV carries no types and a `string` column would yield the symbol `"42"` where the caller asserted `42` | `src/runner.rs::program_text` |
| 4 | Symbols are quoted and never escaped; `"` is refused at emit time | `src/value.rs::to_nemo` |
| 5 | All tests run `--test-threads=1` because of the global timing mutex | every gate |
| 6 | Errors stringified before crossing a thread boundary, because `nemo::error::Error` is not `Send` | `tests/a5_resources.rs` |

## Corrections to the brief

- The brief says "latest tagged release (v0.10.0 as of writing)".
  **There is no v0.10.0.**
  The latest tag is `v0.9.1`, which is what was pinned.
- The brief expects a possible temp-file workaround for fact assertion.
  Not needed — the in-memory `ResourceProvider` route works, and A1 is a clean PASS rather than a PARTIAL.

## Open decision

The gates say adopt.
The packaging says the wrapping crate cannot be published and the workspace would go nightly-only.
The reconciliation is the seam doing real work: `gix-query-engine-nemo` becomes optional, non-default, nightly-gated, and unpublished, and the default engine is the one we write.

That makes the naive evaluator load-bearing, which contradicts the integration brief's non-goal that it be "never optimized, never grown".
Three ways to resolve it, in the order I would rank them:

1. **Ship naive by default, Nemo as an opt-in accelerator.**
   Honest, publishable, stable-toolchain.
   Cost: the naive evaluator has to be good enough to be a default, which is most of the engine work the Nemo decision was meant to avoid — though *only* the evaluator, since the rewrite, the validator, the modes and the demand loop were always ours.
2. **Vendor Nemo.**
   Removes the crates.io blocker and pins semantics exactly.
   Does not remove nightly, and takes on 230 crates of maintenance.
3. **Wait for Nemo to publish and stabilize.**
   Not actionable on our schedule.

I have amended the development plan for option 1, because it is the only one that keeps `gix-*` publishable, and because it makes the differential CI job the brief already asks for into the thing that keeps both engines honest rather than a one-way check on the oracle.
If you prefer 2 or 3, the amendments in the plan marked **[engine-default]** are the ones that change.
