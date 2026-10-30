# git-effect — dev plan, and query's convergence to ARCHITECTURE.md

Two efforts, one document, because they share this repo and a dependency edge.

**Part A** brings git-query to `../git-anchor/ARCHITECTURE.md`: claims re-backed by attest, `key_valid_at`, footprint-based effect stratification, and the already-planned cache/promotion tail.
**Part B** implements git-effect as one crate here — **`git-effect`**, the library plus its `git effect` subcommand behind a default-on `cli` feature — per the doc's placement rule: "lives in query's repo until the rule language settles — every effect change touches that language."

This document extends `DEVPLAN.md`; its decisions (§2.7 author order, §2.11 Nemo-only, §2.12 unrepresentable states, one-binary-per-product) carry over unmodified.
Prerequisite: `../git-anchor/DEVPLAN-attest.md` Phases 1–2 — Part A's claim migration and Part B's authorization both consume `gix-attest`'s consumer contract.

**What an effect is, stated once:** a rule whose head is not derivable.
The body is query's language, evaluated by query's engine, checked by query's passes.
What effect adds is exactly the charter: watch predicates, check authorization, invoke executors, record signed transitions.
Everything below is the discipline of adding *only* that.

---

## Verdict

The scaffolding is further along than DELTA.md records, and it is shaped correctly:

- `EffectDecl`/`Namespace` with prefix-aware `overlaps` exist in `gix-query-ir/src/effect.rs`, deliberately carrying no trigger/gate/then.
- Pass 10 (effect stratification) is implemented and wired (`gix-query-check/src/pass10_effect_stratify.rs`) — but over *declared* read sets, where `docs/query-and-rules.adoc:83-104` specifies footprints.
  That is C4 below, and it is a real soundness gap: a declared read set can lie; a computed footprint cannot.
- `effect NAME reads(...) writes(...)` parses (`gix-query-parse/src/parser.rs:252-290`).
  Part of that syntax is retired below (D2).
- The claim EDB is trailer-backed with its replacement seam self-documented (`gix-query-host/src/claim.rs`: "this is the one place that needs to start asking [git-attest] to verify").
- `bind/5` now matches the doc — pin leg at 1.0, `select_max` as a distinct aggregation stratum, no thresholds.
  DELTA Q1 is stale.
- Cache and promotion remain unbuilt and remain Phase 6 of `DEVPLAN.md`; this plan adds only their attest hook (C5).
- Nothing effect-*runtime* exists: no trigger detection, no idempotence keys, no executor seam, no transition log, no `refs/intent/*`.

What is missing repo-family-wide and gates specific items: the op-log (DELTA X6) gates `key_valid_at`'s real semantics (C3) and nothing else here.

---

## Part A — query convergence

Ordered; C1–C2 are one change reviewed as two.

### C1. Re-back the claim EDB onto attest

`claim/1`, `kind/2`, `target/2`, `signer/2` re-derive from `gix-attest`'s `Claims::log/resolve/verify` over `refs/claims/*` instead of trailers over `refs/meta/claims/*`.
`kind/2` reads `Envelope.payload_kind`; `signer/2` reads the key claim id (a `Member`-shaped oid, as today); `verdict/2` moves out of the envelope entirely — a verdict is payload vocabulary (forge's review schema), so `verdict/2` becomes a payload-backed predicate the day forge registers that schema, and until then keeps its current shape against the review payload.
Unverified claims contribute no facts: verification happens at fact-derivation time, so no rule can join on a claim whose signature fails.
Crypto-verified is still not *valid* — validity stays a rule (`key_valid_at`, C3); the EDB only refuses cryptographic garbage.

Trailer parsing in `claim.rs` is deleted, not deprecated.
The `git interpret-trailers` differential tests retire with it; their successors assert agreement with `git attest log`.

### C2. `pin_claim` reads anchor's payload through store, not trailers

`rev/2` and `loc/2` — the trailer-carried rebind fields claim.rs marks as interim — are retired.
`rebind_pin_locs` becomes: resolve claims with `payload_kind == "rebind-pin"` targeting the anchor, deserialize the payload via store's codec against anchor's registered `RebindPin` schema (`gix_anchor::pin`), and emit `(rev, path, span)`.
This is the composition point the architecture names — "anchor vocabulary in claims is a store schema registered by anchor, composed only in query" — and it is the first place the two siblings meet.
`bind/5`'s pin leg is otherwise untouched; it already sits at confidence 1.0 above `select_max`.

### C3. `key_valid_at`, defined now, honest about the op-log gap

Add `key_valid_at(+Claim)` to the registry with its real signature and its real backing declared: op-log admission order.
Until the op-log exists (X6), the implementation is the same shape as `bind`'s `op_log(None, ...)` leg — an explicit absent-safe marker, evaluating to true with the gap documented at the one call site, not scattered.
Retroactivity of key revocation stays an open rule-policy question; the predicate's contract does not pre-decide it.

### C4. Pass 10 over footprints, exported

Reads are computed, not declared: build the effect graph from writer-declared write sets × reader *footprints* — pass 7's output mapped through `Backing::RefGlob` to ref namespaces — as the adoc specifies.
Export the acyclicity check from `git-query`'s facade (per `DEVPLAN.md`'s "exports it as a public [check] … in the effect runner"), so `git-effect` calls it rather than reimplementing graph, marks, or overlap.
`Namespace` and `EffectDecl` stay in `gix-query-ir`; effect does not get its own copy.

### C5. Cache and promotion: unchanged plan, one addition

Cache refs stay `DEVPLAN.md` Phase 6, last, flag-gated, differential-tested.
The addition: **promotion** — the only door from derived to authoritative — is claim emission, so it is `gix-attest` signing an envelope whose payload cites the canonical goal, the footprint digest, and the result.
Those three are precisely the cache-key inputs already specified in `docs/query-and-rules.adoc:466-505`; promotion reuses that derivation, or the citation and the cache disagree about what "the same query" means.
No promotion code before the cache key exists.

---

## Part B — git-effect

### Decisions

#### D1. The effect doc is the source of truth, stored at `refs/meta/effects/<name>`

A store typed doc:

```rust
#[derive(Facet)]
pub struct EffectDoc {
    pub name: String,
    pub when: String,    // rule-language body — trigger predicates
    pub gate: String,    // rule-language body — authorization + conditions
    pub then: Then,      // Advance { ref_, to } | Run { executor, inputs }
    pub writes: Vec<String>,  // declared namespaces, gix-query-ir Namespace syntax
}
```

`when` and `gate` are query-language source, parsed by `gix-query-parse` and validated by the same passes as any module — effects share the rule body language, which is the entire reason this crate lives here.
`git effect define` writes the doc via store's dynamic write path and then runs validation including the exported C4 check over all registered effects; an effect that would create a cycle is rejected at define time, which is the earliest possible boundary.
`gix-query-kernel` already classifies `refs/meta/effects/` (`effect_ref`); the kernel's push gate extends to deny effect-doc pushes that fail validation, the same way it gates rule refs.

#### D2. Reads computed, writes declared; the `reads(...)` clause is retired

An `EffectDecl`'s read set is derived from `when`+`gate` footprints (C4).
Only `writes` is declared, because writes are a *promise about the future* — what an executor may do — which no static analysis of the body can compute.
The parser's `effect NAME reads(...)` form is removed; `writes(...)` in rule modules remains only if module-resident effect declarations survive D1, and they should not: one source of truth, and it is the doc.
The parser keeps `effect` as a reserved word so old modules fail loudly.

#### D3. Trigger detection: delta-restricted re-evaluation now, semi-naive later, same contract

The charter says triggers are semi-naive delta evaluation on ref advance.
Nemo has no incremental entry point (every demand round runs from cold), so true semi-naive is not available without engine work.
Interim mechanics, semantically equivalent for the monotone bodies effects are restricted to: on a ref transition `(ref, old, new)`, select effects whose *read footprint* overlaps the advanced ref (the C4 machinery, reused), and re-evaluate their `when` via `run_goal`; fire if it derives a tuple it did not derive at `old`.
The before/after comparison is bounded by the footprint, not the whole EDB.
This is a performance ceiling, not a semantic one; upgrading to engine-level deltas later changes no effect doc and no observable behavior.
Triggers read authoritative refs only — enforced by checking the footprint against namespace authority declarations, which is a validation error, not a runtime filter.

#### D4. Push intents are refs; the effect system is the ref transaction manager

`refs/intent/<target-ref>/<n>`: a signed ref (store's seam) recording a requested transition `{ref, expected, to}` as a typed doc.
Gates trigger on intents like any other ref advance (D3); an admitted intent becomes the real transition; either way the intent ref is consumed.
This is the doc's reification move verbatim, and it is what makes merge gates ordinary effects instead of special-cased hooks.

#### D5. Idempotence keys are normal-form hashes; there is no key crate

- Pure advance: `normal_form::hash({effect: <doc tree hash>, trigger: {ref, old, new}})`.
- Executor run: the Action key of the invoked run — the same hash over `{executor, inputs, params}`.

Both through `facet-git-tree`'s frozen normal form; the identity-universe check at effect-schema registration enforces expressibility, exactly as the doc requires for action `params`.
A transition whose key already appears in the log is a no-op, which is what makes at-least-once delivery from hooks safe.

#### D6. The executor seam is a trait with zero implementations

```rust
pub trait Executor {
    fn run(&self, action: &ActionKey, inputs: &[Oid]) -> Result<ActionRecord, ExecError>;
}
```

"No scheduler ambitions, no executor implementations" — kiln, CI runners, env materializers live elsewhere and are handed in.
What effect does own is the boundary check: every `RefEdit` an executor's emitted records imply is checked against the effect's declared `writes` via `Namespace::overlaps` (imported from `gix-query-ir`), and a violation aborts the transition before anything advances.
Emitted action-record claims are inside the declared write set, so the C4 stratification sees executor side channels — that is the fork-bomb argument, and it only holds if this boundary check is unskippable.

#### D7. The Action schema is registered by git-effect, pro tem

`Action { key: { executor, inputs, params }, output }` with `key` marked `identity_key`, registered at `refs/schema/action`.
Action is "not a product," but a schema needs a registering owner, and effect is the first consumer that cannot exist without it (idempotence keys, D5; action-record triggers, worked example 3).
Emitted records are attested — envelope by `gix-attest`, payload this schema — landing under the effect's declared claims namespace.
When the op-log schema lands (X6), it follows the same pattern and this plan gains a dependency, not a design change.
Final home of both schemas re-opens at the effect split; recorded as an open question, not decided by inertia.

#### D8. The transition log: `git effect log` reads signed transition records

Each performed transition appends a typed doc `{effect, trigger, key, edits, outcome}` to `refs/effects/log`, signed via store's seam by the executing identity (machine key).
This ref is authoritative — it fails the rederive test; it exists because an authorized executor signed a decision — and is distinct from the server op-log, which records *admissions* and arrives with X6.
`git effect status` is derived: current effect docs joined with their last log entries — a query, run through query.

#### D9. What effect must never contain

| concern | owner | effect's relationship |
|---|---|---|
| rule language, parsing, validation, footprints | gix-query-parse / -check / -ir | calls; D1 bodies are its language |
| stratification / acyclicity check | git-query (C4 export) | calls at define time |
| evaluation | gix-query-eval (Nemo) | `run_goal` through the facade |
| hashing, normal form, typed docs, ref CAS | store | calls |
| envelope, crypto verification | gix-attest | calls for authorization + record emission |
| executor implementations, scheduling | out of family | trait object handed in |
| `Namespace`, `EffectDecl` | gix-query-ir | imports |
| policy (who is authorized) | rules + claims | `gate` is a query; effect evaluates, never decides |

### Crate layout

```text
crates/git-effect       doc.rs        EffectDoc, Then, schema registration, define-time validation
                        trigger.rs    delta-restricted re-evaluation (D3), intent refs (D4)
                        run.rs        gate evaluation, authorization via attest, Executor seam,
                                      write-set boundary check, idempotence (D5)
                        record.rs     Action schema (D7), transition log (D8)
                        main.rs       define | status | log (behind a default-on `cli` feature)
```

`git-effect` deps: `git-query` (facade), `gix-query-ir`, `gix-query-parse`, `gix-store`, `gix-refstore`, `facet-git-tree`, `gix-attest` (git dep to `../git-anchor`'s repo), `thiserror`.
Notably absent: `gix-query-eval` (engine reached only through the facade), `gix-anchor` (anchor vocabulary reaches effects only as claims through query).

### Phases

Part A first; it is smaller and Part B's authorization path depends on C1.

- **Phase A1 — solo:** C1+C2 as one branch (the claim re-backing), against attest Phases 1–2.
  C3 alongside — it touches the same registry.
- **Phase A2 — one agent:** C4, footprint-based pass 10 + facade export.
  Independent of A1.
- **Phase B1 — solo, freeze the doc:** `EffectDoc`, schema registration, define-time validation wired to C4's export, `git effect define`.
  No runtime.
- **Phase B2 — two agents, parallel:** trigger detection + intents (D3, D4); idempotence + transition log (D5, D8).
- **Phase B3 — solo:** executor seam, boundary check, action records, authorization gate (D6, D7); then `status`/`log` CLI.
  Ends with worked example 3 as an integration test: two chained effects, a stub executor, stratification proving termination.
- **C5 stays with `DEVPLAN.md` Phase 6**, after everything above.

### Definition of done

- `claim.rs` contains no trailer code; every claim fact traces to a crypto-verified envelope.
- `pin_claim` round-trips: `git attest sign anchor:<id> <rebind payload>` → `bind/5` returns the pin at 1.0 — worked example 1's second half, as a test.
- An effect doc that would read its own write set is rejected at `define` with a cycle diagnostic naming the namespace.
- Worked example 2 runs end-to-end against a fixture repo: intent ref → gate evaluates `reviewed` → signed advance recorded in `refs/effects/log`; replaying the trigger is a keyed no-op.
- An executor emitting outside its declared writes aborts with nothing advanced.
- DELTA X2 and Q-rows rewritten against reality; the stale rows already contradicted by code (Q1, X3, X4, S2, A5) corrected in the same pass.

### Open questions

- Engine-level incrementality (true semi-naive triggers): blocked on Nemo capabilities or a kernel-side delta path; D3's contract is written so this is a swap, but the affordability ceiling should be measured in B2, not assumed.
- Final home of the Action and op-log schemas at the effect split (D7).
- Intent-ref GC: consumed intents are derived-ish (their outcome is in the transition log) — GC policy interacts with the server ref-deletion open question.
- Effect-doc versioning: does editing an effect re-key its idempotence (doc tree hash changes ⇒ old triggers may re-fire)?
  Recommendation: yes, deliberately — an edited effect is a new effect; the log records which doc hash performed each transition.
