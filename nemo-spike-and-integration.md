# Agent instructions: evaluate Nemo, then integrate it into the query dev plan

## Context

We are building a moded-Datalog query language over git (spec: "Query language — handoff spec").
Scope is language, validator, and CLI.

**Naming and CLI surface — these supersede the handoff spec's §8 where they differ.**
Everything ships as one binary, `git-query`, with subcommands `git query run`, `git query explain`, `git query rule`.
There is no `git explain` and no `git rules`.
Library crates are `gix-*`; binary crates are `git-*`.

Host EDB and builtins (`anchor_at`, `resolve`, `span_diff`) are demand-driven calls into git-store/git-anchor with binding-pattern (mode) restrictions — the engine may never enumerate them.

Decision on the table: use **Nemo** (<https://github.com/knowsys/nemo>, Apache-2.0/MIT) as the internal fixpoint engine behind an engine-agnostic lowering seam, with a magic-set demand loop on our side.
Your job: **Phase A** verify Nemo can serve, **Phase B** if it passes, amend the development plan.
Do not start Phase B until every Phase A gate has a written verdict.

Ground rules:

- Pin Nemo to the latest tagged release (v0.10.0 as of writing).
  Record the exact commit.
  Never track `main`.
- Nemo is used only via a seam; nothing outside one crate may name a Nemo type.
- Stub the git builtins for the spike — hardcoded tuple tables behind the same Rust trait the real builtins will implement.
  Do not integrate git-store/git-anchor in Phase A.
- Timebox Phase A to roughly 2–3 days of effort.
  If an item is blocked >half a day on undocumented API, record it as a finding rather than reverse-engineering indefinitely.

## Phase A — Spike

Create a scratch crate `nemo-spike` depending on the pinned `nemo` crate (workspace member `nemo`, not `nemo-cli`).
Answer each gate with PASS / FAIL / PARTIAL plus evidence (code path, test name).

### Gate A1 — Programmatic embedding

Can we, from Rust, without touching disk or invoking `nmo`:

1. Load a program supplied as a string.
2. Assert facts (typed tuples: strings, ints, opaque ids) into named EDB predicates.
3. Run to fixpoint.
4. Iterate result tuples of a named predicate with stable types.

If fact assertion requires a file-based import, measure the temp-file workaround and note it, but that downgrades A1 to PARTIAL.

### Gate A2 — Semantics conformance

Encode these golden programs; verify outputs against hand-computed expected relations (write them as Rust tests, exact set equality):

1. Transitive closure (recursion, set semantics — no duplicates).
2. Stratified negation: `active(M) :- member(M), !revoked(M)` plus a chain `a :- b, !c` across three strata.
3. Retraction-shaped update: run once, add a `revoked` fact, run fresh; verify downstream derived facts disappear (we re-run per invocation; confirm no state bleed between runs).
4. Multi-clause predicates and constants in facts/heads (`carries(same, intact).`).
5. A join with `!=` comparison (the `ambiguous` rule shape).

Also confirm: what does Nemo do with an *unstratifiable* program?
We reject those in our validator, but record the failure mode in case one leaks through.

### Gate A3 — Demand loop viability

Implement the outer loop for one stubbed builtin:

- Rewrite `p(X) :- q(A, Rev), anchor_at_ext(A, Rev, B)` into Nemo rules with `demand_anchor_at(A, Rev)` derived from bound positions.
- Round: run → read demand tuples → look up stub table → assert result facts → run again → repeat until demand set stops growing.
- Verify termination and correct final relation on a case where round 2 produces new demand (chained: an anchor result feeds another rule that creates new demand).
- Record wall-clock per round at ~10k facts.
  We need "fine," not "fast."

### Gate A4 — Tracing

- Can a derivation trace for a specific tuple be obtained **programmatically** (not just via their web UI)?
  What structure comes back?
- Does it identify, per node: the rule applied and the leaf facts consumed? (We re-annotate leaves against our demand-loop call log — the trace must at minimum let us match leaf facts to predicates + argument values.)
- How does it represent negation in a derivation?
Verdict: PASS if traces are programmatically reachable and leaf-matchable; PARTIAL if CLI/JSON-only; FAIL if traces exist only in the browser tooling.

### Gate A5 — Resource behavior

- Drive a deliberately quadratic program (cross product, ~2k × 2k).
  Can evaluation be bounded or aborted from the embedding (row cap, timeout, cancellation token)?
  Or is the only option killing the thread/process?
- Record memory behavior at abort.
  We need to distinguish "error" from "truncated" (exit codes 2 vs 3); note honestly whether Nemo can support "truncated" at all today.

### Gate A6 — Fragment containment

Enumerate Nemo language features our lowering could accidentally emit or accept (arithmetic, aggregates, existential rules, datatype coercions).
Confirm our emitted syntax cannot trigger them, and document any implicit coercions on our value types (hex oid strings, ints, atoms) — especially anything RDF-flavored (IRI interpretation of strings).

### Phase A report

Write `spike-report.md`: per-gate verdict + evidence, API surface actually used (module paths — this is our exposure to their instability), workaround list, and a recommendation: **adopt / adopt-with-caveats / reject**.
Rejection criteria: A1 FAIL, A2 any mismatch, or A3 FAIL.
A4/A5 PARTIAL are acceptable deferrals; record them as known debts.

## Phase B — Integrate into the development plan

Only on adopt/adopt-with-caveats.
Amend the existing phased plan as follows; keep everything not mentioned unchanged.

### Crate changes

All planned `ql-*` crates are renamed to the `gix-query-*` convention; the CLI crate is the `git-query` binary.

- `gix-query-syntax`, `gix-query-registry`, `gix-query-validate`, `gix-query-host` — unchanged in content, renamed only.
- Planned `ql-engine` becomes `gix-query-engine`: the **seam** — trait `Engine` with `load(program) → run(edb) → relations`, `trace(tuple)`, and cap/abort hooks.
  Two impls:
  - `gix-query-engine-nemo`: the only crate allowed to depend on `nemo`.
    Pin exact version; contains the fragment-emission layer from A6.
  - `gix-query-engine-naive`: ~300-line naive evaluator, correctness oracle only.
    Unoptimized by policy.
- New crate `gix-query-demand`: the magic-set rewrite + outer demand loop from A3, engine-agnostic, owns the builtin call log (which later feeds `explain` re-annotation and the pass-7 footprint audit trail).

### Phase amendments

- **Phase 3 (engine core)** becomes: seam trait + naive impl + nemo impl passing the A2 golden set through both, differentially.
  Add CI job: every generated proptest program runs on both impls, relations must match exactly.
- **Phase 4 (host binding)**: builtins implement the trait the spike stubbed; `gix-query-demand` swaps stub tables for git-anchor/git-store calls.
  No other change.
- **Phase 7 (CLI)**: the `git-query` binary with subcommands `run` (spec tiers 1–3, including `--goal`), `explain`, and `rule` (`add`, `check`, `api`, `list` — rule modules and the ref→OID snapshot identity — and `predicates` — the whole-program callable surface: host EDB, builtins, and repo predicates with signatures and pass-8 inferred modes.
  `list` and `predicates` are distinct commands; do not merge them).
  Exit-code 3 semantics are scoped by the A5 verdict.
  If Nemo cannot signal truncation, document that v1 maps caps to exit 2 (error, fail-closed) and file the debt; do not fake exit 3.
- **Phase 8 (`git query explain`)**: split into 8a (Nemo trace extraction per A4) and 8b (re-annotation: join trace leaves against the `gix-query-demand` call log to attach builtin modes, heuristic version, source refs, rules-snapshot OID).
  If A4 was PARTIAL, 8a includes whatever bridging the spike identified.
- **New standing task — upgrade audit**: a Nemo version bump is a semantics change.
  Procedure: bump in a branch, run full differential suite + golden traces, diff A6 fragment inventory against their changelog, then merge.
  Write this into CONTRIBUTING.

### Non-goals to state explicitly in the plan

- No incremental/DBSP evaluation in the CLI path; per-stratum footprint-keyed caching only (existing phase 9).
- No use of Nemo aggregates, arithmetic, or existential rules, even where convenient — the fragment is frozen by our spec, not by what the engine offers.
- The naive oracle is never deleted, never optimized, and never grows features the spec lacks.

### Deliverables for Phase B

1. Updated plan document with the amendments applied inline.
2. Skeleton workspace: seam trait, naive impl passing A2 goldens, nemo impl passing A2 goldens, differential test harness wired into CI.
3. `gix-query-demand` with the spike's loop promoted to library quality (stub-table builtin impl retained for tests).
