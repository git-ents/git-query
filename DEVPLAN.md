# git-query — dev plan

Implement `docs/query-and-rules.adoc`: a Datalog query and rules layer over git, with one frontend and one IR.
Two published-shape crates in the family style — **`gix-query`** (library) and **`git-query`** (CLI, invoked as `git query`) — plus internal crates behind them.

Template: `../git-store` and `../git-anchor`.
Copy conventions, configs, and workflows verbatim, adapting names, exactly as `../git-anchor/DEVPLAN.md` did.

**Naming, non-negotiable:** `gix-*` for every library crate, `git-*` for every binary.
There is exactly **one** binary — `git-query`, invoked as `git query` — with subcommands `run`, `explain`, `rules`, `predicates` (§1.10).
No `git-explain`, no `git-rules`.

**What is actually being built, stated once:** a mode-checked, magic-set-rewritten Datalog frontend over a git-backed EDB, evaluated by a fixpoint engine reached through a seam.
Everything that makes this project what it is — the grammar, the nine validation passes, the mode system, the magic-set rewrite, the demand loop that keeps host builtins from being enumerated, footprints, the cache key, `explain` — is ours.
The one thing that is not is "compute the least fixpoint of a stratified Datalog program over a finite EDB", and that is a commodity.

The engine was originally specified here as a hand-written semi-naive interpreter.
**§2.11 supersedes that**: the engine is [Nemo](https://github.com/knowsys/nemo), it is the only one, and `gix-query-eval` calls it by name rather than through a seam anticipating a second.
`ascent` remains the compiled kernel only (§2.2) — it is compile-time and cannot load rule modules from refs at runtime, which is the entire feature.
Cozo remains rejected (§2.1).
The Nemo spike (`spikes/nemo-spike/spike-report.md`) is the evidence, and §2.11 is the list of what adopting it costs.

**§0 supersedes the mode/magic/demand machinery in the same way §2.11 superseded the interpreter.**
The sentence two paragraphs up still names "the mode system, the magic-set rewrite, the demand loop" as ours; as of 2026-07-30 they are cut, not built around.
Read §0 first; it lists exactly what §2.10 and its dependents it retires.

Two constraints run through every phase and belong here rather than in a style guide.
**Make invalid states unrepresentable** (§2.12) — in this project it is what holds the engine's fragment, so it is load-bearing rather than tasteful.
And **one way to do things**: no second engine, no stable/nightly split, no trait with one implementation.

---

## 0. Simplification decision (2026-07-30): eager evaluation supersedes modes, magic sets, and the demand loop

The project's scope is now explicitly experimental — the question being answered is whether the core idea holds, not whether queries over large repos are affordable on first run.
Under that scope, the heaviest machinery in the plan defends against a cost we have decided to pay differently, and it goes.

**Cut:**

- The **mode system** (`mode.rs`, adornments, `ModeSet`, pass 8) and with it §2.10's magic-set rewrite (`rewrite.rs`) and the demand loop (`demand.rs`).
  Together with their share of IR, registry mode declarations, and tests this is ~2,700 lines of the hardest code in the plan.
- **Bound-argument builtins** — `span_diff(+Blob, +Covered, …)` (§1.8) and `introduced/2` (§1.7) — are deferred, not redesigned.
  They were the only thing the mode system was load-bearing for once evaluation is eager; if a future vocabulary needs one, the mode system returns as a whole, not as a special case.
- **Pass 6** (caps) — by its own admission a lint, not a soundness pass.
- **Dead code already unconsumed:** `provenance.rs` (§2.9's traits; `trace.rs` built its own `DerivationTree` from Nemo's tracer instead) and `canonical.rs` (no caller until the cache subsystem exists; restore from git history when it does).

**Kept, unchanged:** stratified negation and pass 5 (~280 lines, and the kernel's own rules are negation-shaped — `unsigned`, `dangling-anchor`, "member and not revoked"); passes 1–5, 7, 9, 10; Nemo at the pinned rev with its conformance suite; the `ascent` kernel and the ascent-vs-Nemo differential proptest, which remains the correctness story.

**Replacement — eager footprint materialization:** for each base predicate in the goal's transitive footprint (pass 7, which was already an analysis with no errors), enumerate its full extension through `FactSource`, hand everything to Nemo as tables, run once to fixpoint, read back the goal relation.
No rounds, no `magic_` relations, no re-running from cold.
Every registry predicate must therefore be finitely enumerable given the footprint — which every git-backed base predicate is.

**Affordability moves from demand-narrowing to amortization**, exploiting the fact that almost the entire EDB is content-addressed and immutable:

1. **Extraction cache, keyed by OID.**
   `tree_entry`, `parent`, claim payloads, anchor coverage — true forever once the object exists.
   Never invalidated; GC of an unreachable object just means a slow re-fill (accepted under experimental scope, and consistent with §1.6's observation that GC state is a cache input).
2. **Answer cache, keyed by the footprint ref-snapshot digest** §1.1 already specifies.
   Retractions are new commits on the same refs, so any ref advance or deletion changes the digest and is automatically a miss — no invalidation logic, and the stale-verdict bug of §1.1 is impossible by construction.
   Because answers are only reused across *identical* snapshots, non-monotonicity of derivation under ref moves is irrelevant; negation costs the cache design nothing.

First query after a ref advance pays re-derivation over mostly-cached extraction — proportional to the delta of new objects, which is the property the demand loop was buying.

**Sections this supersedes rather than deletes:** §2.10 (the rewrite decision and its SIPS payoff in §2.7), the pass-8 and mode entries wherever the nine-pass list is enumerated, the `gix-query-eval` row of §3's crate table (now: Nemo lowering, eager materialization, derivation-tree recovery), and §5's framing — affordability is still the real risk, but the answer is the two-level cache above, not demand.

---

## Verdict

**Yes, this is decidable, and yes it can be built.**
The design is sound in its core claim: stratified Datalog with a finite EDB, range restriction, and no function symbols terminates, and the validation passes are syntactic and near-linear — eight as written, nine once §1.4 adds minting confinement.
That part is textbook and the doc gets it right.

But the doc has concrete defects — fifteen of them, in the eleven areas §1 enumerates — that break decidability, soundness, or implementability as written.
Most are small.
Four are not: the cache key is unsound on its data inputs (§1.1) *and* on its implementation inputs (§1.6), the status lattice cannot express the distinction the review-carry policy exists to make (§1.5), and the flagship review vocabulary fails the spec's own pass 8 (§1.7).
All must be fixed in the spec *before* any agent writes code — each is the kind of thing an implementer papers over rather than escalates, and §1.5 in particular becomes a migration of repo-stored user data the moment anything emits a status.

Two of the newer ones are worth calling out for what they demonstrate rather than for their size. §1.7 — the reference vocabulary is *itself* mode-violating, because `introduced/2` needs an inverted blob→commit index that the design deliberately does not have — is the mode system earning its keep on the first program anyone will read.
And §1.8 — `span_diff(+Blob, +Covered, ...)` takes a *set* of spans in a language with no structured terms — is the third place in the spec where a builtin signature quietly reintroduces something pass 1 exists to forbid.
Builtins are where the semantics leak; that is the pattern.

The three serious ones share a root cause worth naming up front: **the design assumes `bind` is a pure function of content-addressed inputs, and it is not.**
Not in its return shape (§1.5), not with respect to GC state (§1.6), and not across implementation versions (§1.6).
Everything cached, gated, or carried forward rests on that assumption.

The honest summary of what "decidable" buys you: it buys termination, and the doc already says so.
It does not buy affordability, and affordability — not termination — is where this design will actually hurt. §5 treats that as a first-class engineering problem rather than a footnote.

---

## 1. Spec corrections — do these first, solo, before dispatching anyone

### 1.1 The cache key is unsound

Specified: `(rules-ref snapshot OID, goal, rev, edb digest)`.

`rev` does not determine the host EDB.
`member/1`, `revoked/1`, `claim/1`, `kind/2`, `target/2`, `signer/2`, and `verdict/2` are backed by refs that move independently of any code rev.
Two runs at the same `rev` with a member revoked in between hit the same cache entry and return the same answer.
That is a correctness bug that shows up as a stale gate verdict, which is the worst place for it.

**Fix:** key on the footprint.

```text
key = H(rules snapshot OID, goal, footprint ref snapshot, edb digest)
```

where *footprint ref snapshot* is the sorted `(refname → OID)` map over every ref the goal's transitive base predicates read, `rev` included as one entry.
This also makes the cache key *narrower* in the common case — a query touching only `tree_entry` does not invalidate when a member is revoked.

**Two things this leaves underspecified, and both bite.**

*The snapshot is not an OID.*
A ref *namespace* has no object to point at, so "footprint ref snapshot" cannot borrow the rules-ref case's shape — it has to be defined as a digest:

```text
footprint_digest = H( sorted glob patterns, NUL-delimited,
                      then for each (refname, oid) in sorted-by-refname order:
                        refname, 0x00, oid, 0x00 )
```

Sorted by full refname in byte order, NUL-delimited so no refname can forge a boundary.
The glob patterns go in first because an *absent* namespace and an *empty* one must not collide.

*The goal is not a string.*
`reviewed(B)` and `reviewed( X )` are the same query and must hit the same entry, or the cache is a whitespace lottery.
Key on a **canonical form**: parse, then serialize the IR with variables renumbered in first-occurrence order (`V0`, `V1`, …), whitespace and comments discarded, and literals left in author order — §2.8 makes author order semantically load-bearing, so it must *not* be normalized away.
Two goals share a cache entry iff they are alpha-equivalent with identical literal order.
Canonicalization lives in `gix-query-ir` beside the IR it serializes, never in the CLI.

**Pass 7 must be extended to make this possible.**
As specified it yields base *predicate names*; a snapshot needs *ref globs*.
So the signature registry grows a backing-source field per EDB predicate, and pass 7 maps through it:

| backing | example | contributes to key |
| --- | --- | --- |
| `RefGlob(g)` | `member/1` → `refs/meta/members/*` | resolved `(refname → OID)` pairs |
| `ContentAddressed` | `tree_entry/3` given bound `Rev` | nothing — the OID is already in the goal |

That second row is worth dwelling on, because it exposes a coincidence the design should lean on: **an EDB predicate whose extension depends on an unbound rev-like argument is both unaffordable and uncacheable.**
`tree_entry(Rev, _, B)` with `Rev` free ranges over an ill-defined universe of commits, and there is no finite ref set to snapshot for it either.
Requiring `+Rev` fixes both at once.
Mode discipline (§2.5) and cache determinacy are the same property viewed from two directions, and the registry should carry one field that serves both.

### 1.2 `loc(B, S, E)` is a function symbol

The flagship rule in the review vocabulary is:

```prolog
reviewed_span(Rev, B, S, E) :- ..., bind(A, Rev, loc(B, S, E), St), carries(St).
```

`loc/3` is a term constructor.
Pass 1 forbids term constructors.
The spec violates its own first validation pass in its own worked example.

You can rescue it by carving out "structured constants in builtin output positions only, never in a head" — but that carve-out is destructuring, and destructuring is the thin end of unification, which is the thin end of function symbols.
The whole reason pass 1 is worth having is that it is a pure syntactic check with no carve-outs.

**Fix:** flatten the builtin.

```text
bind(+Anchor, +Rev, -Blob, -Start, -End, -Status)
```

`lost` yields the sentinel triple `(none, 0, 0)`, and `none` joins against no blob relation, preserving the self-filtering property the doc wants.
Keep `loc` in the type registry as an opaque *scalar* if you want a printable rendering, but it must never be a constructed term in rule syntax.

### 1.3 Two variable syntaxes in a "one-syntax" language

Rule files use Prolog convention (`active_member(M)`, uppercase-initial variable, lowercase-initial constant).
Tier 3 CLI uses `?b`.
The doc's stated goal is one syntax across all three tiers.

**Fix:** uppercase-initial everywhere, `_` anonymous, drop `?`.
Bonus: `?` is a shell glob character, so `--goal 'reviewed(?b)'` breaks the moment someone forgets the quotes, and it breaks *silently* in the case where a matching file exists.

### 1.4 Value-minting builtins need an explicit pass, and pass 1 does not cover it

`bind`, `span_diff`, and `abbrev` all produce values absent from the EDB.
That is semantically function application, and pass 1 only catches the *syntactic* form. (After §1.8 and §1.9, `bind`/`bind_fuzzy` are the only minting builtins left — which does not make the pass optional, it makes it cheap.) The adoc has the right rule buried in prose — "a builtin that mints constants may not appear in a recursive SCC" — but the eight-pass list does not contain it, and the eight-pass list is what an implementer will build to.

**Fix:** promote it to **pass 9 — minting confinement**, checked against a `mints: bool` flag on each builtin's registry entry, over the same SCC decomposition pass 5 already computes.
Add to the registry the obligation each builtin must satisfy and the validator must assume:

| property | meaning |
| --- | --- |
| `total` | defined on every well-moded input |
| `deterministic` | same input, same output, forever |
| `pure` | no observable effect |
| `finite` | **finite** output tuples per bound input tuple |
| `mints` | produces values not in the active domain |
| `version` | implementation version, bumped on any behavior change (§1.6) |

`finite` is the one nobody writes down.
It is what makes `ambiguous`-yields- *n*-tuples safe: *n* is bounded per input.
A builtin that is `mints` and not `finite` breaks termination even outside a recursive SCC.

Two properties in that table are the ones an implementer will be tempted to compute instead of read.
Both must be **declared, never inferred**:

- `mints` is a claim about the builtin's *implementation*, not its signature.
  A builtin with an all-`-` output mode might return only values it received in its inputs (projection) or values it constructed (minting), and the signature cannot tell you which.
  Inferring "has an output mode, therefore mints" would make pass 9 reject harmless projections; inferring the other direction would miss the case pass 9 exists for.
  Declare it.
- `version` is the sixth property, and it exists because §1.6 needs a builtin registry digest and there is nowhere else honest to put it.
  A builtin whose answer changes when `gix-anchor` improves its heuristics is not a different builtin — same name, same signature, same modes — so version has to be carried as data next to those, not derived from them.

Pass 9 plus `finite` is what actually closes the termination argument.
Passes 1–8 alone do not, because pass 1 is syntactic and the builtins are where the semantics leak.

### 1.5 The status lattice is wrong, not merely unimplemented

I first wrote this up as an integration gap between `git-query` and `gix_anchor::Projection`, with an interim lossy mapping to unblock.
That framing was wrong, and the correction is the most consequential item on this list.

The four statuses are `unchanged | moved | ambiguous | lost`.
`gix-anchor` returns `Current | Relocated { path, lines } | Outdated { path } | Deleted`.
`Outdated` — the file survives, the anchored lines were edited — has no counterpart.
But that is a symptom.
The cause is that **position-change and content-change are orthogonal axes, and a flat four-valued enum cannot carry both.**

This is load-bearing rather than cosmetic, because `carries/1` — the entire review-carry policy, the thing the language turned out to be able to express — is defined over exactly these statuses.
So the status type is policy vocabulary, and as written it cannot express the distinction policy cares most about: *a reviewed span that moved but is textually intact* versus *a reviewed span that stayed put but was edited*.
The first should almost always carry; the second should almost never.
Today they are both unrepresentable.

An interim mapping is therefore *worse than a delay*.
Once `carries(unchanged).` appears in a user's rule module stored in a repo, the status type is repo data, and changing it is a breaking migration of everyone's rules.
**Fix the lattice before anything emits a status.**

**Fix:** two output columns, not one.

```text
bind(+Anchor, +Rev, -Blob, -Start, -End, -Position, -Content)
```

- `Position ∈ { same, moved, lost }`
- `Content  ∈ { intact, edited, none }` — `none` only when `Position = lost`

**`ambiguous` is not a status and should be dropped from the vocabulary.**
Ambiguity is a property of the result *set*, not of any one projection — a candidate is still individually intact-at-a-new-path, and stamping it `ambiguous` overwrites that.
Since `bind` already emits one tuple per candidate, ambiguity is *derivable in the language*:

```prolog
ambiguous(A, Rev) :- bind(A, Rev, B1, S1, E1, _, _),
                     bind(A, Rev, B2, S2, E2, _, _), B1 != B2.
```

A join plus a disequality — no aggregate, no negation, range-restricted, so it passes cleanly.
This is strictly better than a status: it makes ambiguity *policy* rather than *mechanism*, exactly the shape that made `carries/1` the right answer for review-carry.
It also shrinks the upstream change, since `gix-anchor` never needs an `Ambiguous` variant — `project_candidates` returning a `Vec` of length > 1 *is* the ambiguity, with no new vocabulary at all.

and `carries` becomes a binary relation over the two axes:

```prolog
carries(same,  intact).
carries(moved, intact).
```

Strictly more expressive, still pure data, and it now says the thing policy wanted to say.
`carries(moved, edited)` is the knob that was missing.

**What this costs in `../git-anchor` — less than it looks.**
I checked: `Outdated { path }` carries the path, so comparing it against `anchor.path` recovers the position axis.
Three of the four positions decompose from today's API with no upstream change:

| `Projection` | `Position` | `Content` |
| --- | --- | --- |
| `Current` | `same` | `intact` |
| `Relocated { path, lines }` | `same` or `moved`, by path compare | `intact` |
| `Outdated { path }` | `same` or `moved`, by path compare | `edited` |
| `Deleted` | `lost` | `none` |

Only `ambiguous` needs upstream work: `project` returns one `Projection`, and `ambiguous` is inherently a set, so it needs `project_candidates(...) -> Vec<Projection>`.
That is the one genuine `git-anchor` change, and it is additive.

Two consequences for sequencing:

- The `Content` axis is where the doc's open question on span growth belongs.
  A grown span cannot distinguish surviving reviewed lines from lines inserted into the middle; diff has that provenance and discards it.
  Refining `edited` into a carried/fresh subspan decomposition is a *later widening of one axis* rather than a redesign — which is precisely the property the flat enum lacked.
  Move that question to `git-anchor`.
- The `ambiguous` triggering question (open, product) is now scoped to one axis
  value instead of contaminating the whole status type.

Do **not** let an agent invent statuses inside `git-query` to route around the upstream gap.
The axis vocabularies are a `git-anchor` export — its own `Projection::label` doc comment already claims the status words as "the porcelain grammar's own vocabulary, shared by every surface that names an outcome."
A competing vocabulary defined in `git-query` would fork that grammar across two repos, and `gix-comment` (same workspace, same question about whether an anchored comment survived) would have to depend on `git-query` to use it, which is backwards.

### 1.6 `bind` is not pure, and the cache key has a second hole

§1.1 fixes the *data* half of the cache key.
There is an *implementation* half, and `bind` fails it twice.

**First: `gix_anchor::project` depends on ambient GC state.**
Verified in the source — it calls `project_exact`, and on `Error::AnchorCommitMissing` silently falls back to `project_from_context`.
Whether the anchor's own commit has been garbage collected is not content-addressed, is not a ref, and appears nowhere in the cache key.

This is not a rounding error.
`project_from_context` does no rename tracking at all — its own doc comment says "a genuine rename reports `Deleted` here, same as a real deletion."
So for a renamed file, `project` returns `Relocated` before a `git gc` and `Deleted` after it, with no other input changed.
Under §1.5's axes that is `(moved, intact)` flipping to `(lost, none)` — the difference between a review carrying forward and a review being destroyed, decided by whether maintenance ran.

A builtin declared `total, deterministic, pure` that does this poisons every materialized result derived from it, which is exactly the failure mode §1.4's property table exists to prevent.

**Fix — and it needs no upstream change, only discipline.**
Both entry points already exist.
Never call `project`.
Expose two builtins:

| builtin | backed by | in gates |
| --- | --- | --- |
| `bind` | `project_exact` | allowed |
| `bind_fuzzy` | `project_from_context` | **rejected by the validator** |

`bind` fails loudly when the anchor commit is gone rather than silently degrading; the fallback becomes something a rule author opts into *in the rule text*, which is in the cache key.
Forbidding `bind_fuzzy` in gate rules is worth having on its own merits — an admission decision should not turn on a best-scoring-window heuristic.
`gix-anchor` should get a doc-comment warning that `project` is unsuitable for any cached or gating caller.

**Second: heuristic drift.**
`project_exact`'s rename detection and hunk mapping are heuristics.
Improve them in a `gix-anchor` release and `bind` returns different answers for identical inputs — rules OID, footprint snapshot, and EDB digest all unchanged, so every cache entry is silently stale across the upgrade.

**Fix:** the cache key gains a **builtin registry digest** over each builtin's name, signature, and implementation version.
That needs one upstream export:

```rust
pub const PROJECTION_HEURISTIC_VERSION: u32 = 1;  // bump on any behavior change
```

Same class of bug as §1.1, same fix shape — put everything the answer depends on into the key.
The lesson worth writing into the docs: a cache over a pure function is only as sound as the purity claim, and "pure" has to mean pure with respect to the *implementation* as well as the inputs.

### 1.7 The reference vocabulary fails the spec's own pass 8

The flagship program in the doc does not validate.
Trace `blocked`:

```text
self_approved(+B, +M) → authored(+B, -C) → introduced(+B, -C)
                      → tree_entry(-C, ?, +B)
```

`tree_entry` is `(+Rev, -Path, -Blob)`, and nothing in the program binds `C` at any call site.
`introduced(B, C) :- tree_entry(C, P, B), ...` is asking for an inverted **blob → commit** index — "which commits contain this blob" — and that index does not exist, by design, because it is unbounded over history and maintaining it would mean a write-side index the whole content-addressed approach avoids.
This is a mode violation in the spec's own exemplar.

It is also the mode system working exactly as advertised: the failure is caught statically, on the first program anyone reads, rather than as a query that runs for an hour.
Keep the finding, keep the design, fix the program.

**Fix:** thread rev scope through.
Reachability from a *bound* rev is the only way to get a commit into a bound position, so `introduced` and everything above it gain a `Rev` argument.

```prolog
reach(Rev, Rev) :- commit(Rev).
reach(Rev, C)   :- reach(Rev, X), parent(X, C).

has_parent(C)         :- parent(C, _).
introduced(Rev, B, C) :- reach(Rev, C), tree_entry(C, P, B),
                         parent(C, Pc), !tree_entry(Pc, P, B).
introduced(Rev, B, C) :- reach(Rev, C), tree_entry(C, P, B), !has_parent(C).
```

The second `introduced` clause is a separate bug the rev-threading exposed: the doc's single clause requires `parent(C, Pc)`, so **nothing is ever introduced in a root commit**.
Every blob in the initial import is silently un-`authored`, and therefore never `self_approved`.
Worth fixing here rather than discovering it in a fixture repo whose first commit is empty.

The corrected vocabulary in full — this replaces the doc's `Review vocabulary` section, and folds in §1.2 (flattened `bind`), §1.5 (two axes, derived ambiguity), and §1.8 (per-line spans):

```prolog
pub reviewed(B)
pub unreviewed(Rev, B)
pub unreviewed_line(Rev, Path, B, N)
pub mergeable(Rev)

active_member(M) :- member(M), !revoked(M).
review(C)        :- claim(C), kind(C, review).

approved_by(B, M) :- review(C), target(C, B), signer(C, M),
                     verdict(C, approve), active_member(M).
rejected(B)       :- review(C), target(C, B), verdict(C, reject),
                     signer(C, M), active_member(M).
reviewed(B)       :- approved_by(B, _).

reach(Rev, Rev) :- commit(Rev).
reach(Rev, C)   :- reach(Rev, X), parent(X, C).

has_parent(C)            :- parent(C, _).
introduced(Rev, B, C)    :- reach(Rev, C), tree_entry(C, P, B),
                            parent(C, Pc), !tree_entry(Pc, P, B).
introduced(Rev, B, C)    :- reach(Rev, C), tree_entry(C, P, B), !has_parent(C).
authored(Rev, B, M)      :- introduced(Rev, B, C), author(C, M).
self_approved(Rev, B, M) :- authored(Rev, B, M), approved_by(B, M).

carries(same,  intact).
carries(moved, intact).

reviewed_span(Rev, B, S, E) :- review(C), target(C, A), anchor(A),
                               verdict(C, approve), signer(C, M),
                               active_member(M),
                               bind(A, Rev, B, S, E, Pos, Con),
                               carries(Pos, Con).

ambiguous(A, Rev) :- bind(A, Rev, B1, _, _, _, _),
                     bind(A, Rev, B2, _, _, _, _), B1 != B2.

reviewed_line(Rev, B, N)      :- reviewed_span(Rev, B, S, E), line(B, N),
                                 N >= S, N <= E.
unreviewed_line(Rev, P, B, N) :- tree_entry(Rev, P, B), line(B, N),
                                 !reviewed_line(Rev, B, N).

unreviewed(Rev, B) :- tree_entry(Rev, _, B), !reviewed(B).
blocked(Rev)       :- unreviewed(Rev, _).
blocked(Rev)       :- tree_entry(Rev, _, B), rejected(B).
blocked(Rev)       :- tree_entry(Rev, _, B), self_approved(Rev, B, _).
mergeable(Rev)     :- commit(Rev), !blocked(Rev).
```

Inferred modes, which `git query predicates` must surface and pass 8 must produce:

| predicate | mode | why |
| --- | --- | --- |
| `reviewed/1`, `approved_by/2`, `rejected/1` | all free | enumerable from the claim namespace |
| `reach/2`, `introduced/3`, `authored/3` | `+Rev` | `commit/1` is required-bound (§2.5) |
| `self_approved/3`, `blocked/1`, `unreviewed/2` | `+Rev` | `tree_entry` is `+Rev` |
| `reviewed_span/4`, `reviewed_line/3`, `unreviewed_line/4` | `+Rev` | `bind` is `+Rev`; `line` is `+Blob` |
| `ambiguous/2` | `+A, +Rev` | `bind` is `+Anchor, +Rev` |
| `mergeable/1` | `+Rev` | via `blocked` |

Two consequences to record in the docs.

*EDB predicates need a mode **set**, not a mode vector.*
`member/1` is called free in `active_member` (enumerate the namespace) and bound in `signer`-driven contexts (membership test).
Both are affordable and both must be legal, so the registry stores a set of supported mode vectors per EDB predicate and pass 8 checks membership in that set.
IDB predicates keep a single inferred required-bound set, unioned over clauses.

*`reach` is the affordability landmine, not `mergeable`.*
Threading `Rev` through made the program well-moded; it did not make it cheap.
`introduced` now scans every tree at every ancestor commit of `Rev`.
The scoped form is what tier 2's `--in A..B` must compile to, and it should ship in the reference vocabulary rather than being left as an exercise:

```prolog
reach_in(Base, Tip, Tip) :- commit(Tip).
reach_in(Base, Tip, C)   :- reach_in(Base, Tip, X), parent(X, C), !reach(Base, C).
```

Stratifies cleanly — `reach_in` has a negative edge to `reach`, and `reach` does not depend on `reach_in`.
Both belong in the bench suite of §5.

### 1.8 `span_diff` is unrepresentable in the language it belongs to

```text
span_diff(+Blob, +Covered, -Start, -End)
```

`Covered` is a *set of spans*.
The language has no structured terms — pass 1 forbids them, and the ban has to extend to builtin arguments or it is not a ban.
There is no way to pass a set as one argument, and no way to pass a relation as an argument at all.
Computing interval complement over a relation is an **aggregation**, and aggregates appear in this spec exactly once, as an unexplained mention in pass 5's "no negative or aggregate edge within an SCC".
They are never defined, never given a syntax, and never given a stratification treatment beyond that clause.

**Fix, two parts.**

*Delete `span_diff`.*
Interval complement decomposes into per-line negation, which the language already expresses — see `reviewed_line`/`unreviewed_line` in §1.7.
Coalescing adjacent uncovered lines back into spans is **presentation** and moves to the formatter (§1.9 makes the same move for `abbrev`, for the same reason).
The `pub unreviewed_span(Rev, Path, B, S, E)` in the doc becomes `pub unreviewed_line(Rev, Path, B, N)`, and `--format` gains span coalescing over consecutive `N` at equal `(Rev, Path, B)`.
The tsv contract is over lines; spans are a rendering.

*Declare the comparison builtins.*
The spec's own examples use `!=` and `<=`, and pass 4 has a special rule about "every variable in a comparison" — so comparisons are load-bearing, and the builtin list does not contain them.
Add them to the registry with the full property table from §1.4:

| builtin | modes | types | `mints` |
| --- | --- | --- | --- |
| `=`, `!=` | `(+, +)` | any two scalars of the same registry type | no |
| `<`, `<=`, `>`, `>=` | `(+, +)` | `int` only | no |

All-bound only — these are filters, never generators.
`int`-only ordering is the conservative choice: ordering `oid` or `member` invites rules whose meaning depends on hash bytes.
`=` on two scalars is a filter, never a unification; saying so explicitly matters, because "`=` is unification" is the assumption a Prolog-shaped syntax invites and it would reintroduce value invention through the back door.

*No aggregates in v1.*
Delete the stray mention from pass 5.
Aggregation is a real feature with real design work behind it — count/min/max need a stratification story, a mode story, and a termination story of their own — and shipping the phrase without the feature is worse than shipping neither, because it reads as a commitment.
If `span_diff` was the only motivating use case and per-line negation covers it, there is no v1 aggregate requirement at all.

### 1.9 `abbrev` violates the contract it is declared under

`abbrev(+Oid, -Short)` is declared `total, deterministic, pure`.
It is none of those: the abbreviation length git chooses depends on how many objects are in the object database, so the same OID abbreviates differently before and after a fetch, with no ref moved and no rule changed.
Under §1.1's cache key that is unkeyed state — and it cannot be keyed, short of putting the entire odb in the footprint, which defeats the purpose.

**Fix:** `abbrev` is presentation, so move it out of the language and into the CLI.
`--abbrev[=<n>]` on `git query run`, applied to `oid`-typed columns at format time.
Full OIDs remain the values; abbreviation remains a rendering that never enters a join, never enters a head, and never enters a cache key.

Worth stating as a rule rather than a one-off, since it now governs three removals (`abbrev`, `span_diff` coalescing, `loc` rendering from §1.2): **anything whose output depends on ambient repo state that is not a ref, or whose only consumer is a human reading a terminal, belongs in the formatter.**
The language's job is to be cacheable.

### 1.10 One binary, four subcommands

The doc spells the surfaces `git query …`, `git rules add …`, and `git explain <goal>`, which reads as three git subcommands and therefore three binaries.
**One binary: `git-query`, invoked as `git query`.**

```text
git query run <predicate> [--<arg> <value>]…      # tier 1
git query run <subject> --with … --without … --in A..B   # tier 2
git query run --goal '…' --select …               # tier 3
git query explain '<tuple>'
git query rules add|check|api|list
git query predicates
```

The `run` verb is not ceremony.
Tier 1 applies a *user-defined* predicate name as the first positional argument, so without it, every rule module competes with the subcommand namespace — a module defining `rules` or `explain` would be shadowed by the CLI, and adding a subcommand later would silently steal a working query.
`run` puts predicate names in their own namespace where they can be arbitrary.

`--rev` (default `HEAD`), `--edb`, `--format`, `--order`, and `--abbrev` are `run` flags.
`explain` takes `--rev` and `--format`.
Update the doc's CLI and Explain sections accordingly; `git-explain` and `git-rules` do not exist.

### 1.11 Five smaller corrections

*Gates must fail closed on truncation.*
Exit codes are `0` rows, `1` none, `2` error, plus a distinct code for row/iteration/time-cap truncation — pin it as **`3`**.
Every gate integration must treat `3` as failure, and the doc must say so in the same breath as it introduces the code.
The reason is polarity: a truncated relation read as an answer over-approves when the gate asks "is anything blocked?" and under-approves when it asks "is everything reviewed?" — so there is no safe default interpretation, only a safe *refusal*.
`2` and `3` are both failure for gate purposes; they differ only in what the operator does next.

*Tier 2's `--without` is CLI-authored negation, and the prose should admit it.*
The doc claims negation is "authored once in a rule".
`--without approved-by:joey` is a negated literal composed at the command line.
It is perfectly safe — the subject variable is bound by the tier-2 subject, so range restriction holds by construction, and the negated predicate is already stratified within the program, so no new stratum is introduced.
But it is negation, and describing the design as if the CLI cannot express any is the kind of claim that gets an implementer to reject a valid `--without` on principle.
State the actual invariant: *the CLI may compose negated literals only over the tier-2 subject variable, which is always bound.*

*`--with unreviewed` names a predicate the doc never defines.*
§1.7 defines `unreviewed(Rev, B)`; the CLI supplies `Rev` from `--rev`.
This also settles what tier 2 does with the required-bound argument of a filter predicate — it fills it from the ambient flags, and errors if it cannot.

*The ambiguity lint and the `bind_fuzzy` prohibition need the same machinery, and it is pass 7's.*
Both are questions of the form "does this predicate transitively reach builtin X?", and both are defeated by a one-line helper predicate that wraps the call.
So pass 7's footprint must carry **transitive builtin use** alongside transitive base predicates, and both checks read it: `bind_fuzzy` anywhere in a gate rule's footprint is a hard validation error (§1.6), and a rule whose footprint reaches `bind` without any guard on `ambiguous/2` is a lint.
Implement them as two consumers of one footprint, not as two traversals.

*Minting is declared, not inferred* — see the addition to §1.4's table, along with `version` as the sixth property.

---

## 2. Decisions

### 2.1 ascent yes, Cozo no — write the interpreter

> **Partly superseded by §2.11.**
> The rejection of Cozo stands, and every reason below still holds.
> What changed is the conclusion "write *the* interpreter": the evaluator now sits behind a trait with two implementations.
> Read the four problems below as the criteria any engine must meet, which is how they were used to grade the Nemo spike.

**Recommendation: use `ascent` for the compiled kernel only, and write an owned semi-naive interpreter for everything else.**
**Do not use Cozo.**

Cozo is an embedded *database*: its own storage engine, its own dialect (CozoScript), its own evaluation semantics including arithmetic, function application, aggregation, and limits.
Every one of those is a thing pass 1 exists to forbid.
Four specific problems:

1. **The validator's guarantees stop at the translation boundary.**
   IR → CozoScript text means Cozo's semantics become ground truth for the dynamic path while ascent's are ground truth for the static path.
   Two emitters that disagree on one IR is strictly worse than one emitter.
2. **Builtins do not fit.**
   `bind(+A, +Rev, ...)` must be called with bound inputs *inside the fixpoint loop*.
   In an owned interpreter that is a trait call in the join.
   In Cozo you either write a fixed-rule plugin or pre-materialize `bind` over the full anchor × rev product — which is exactly the terminating-but-unaffordable trap the doc warns against.
3. **Caps cannot be honored.**
   Row, iteration, and time caps are mandatory per the doc and must be enforced *within* the fixpoint.
   You get Cozo's limits, not yours, and "truncation announced by a distinct exit status" becomes guesswork.
4. **The one thing Cozo would buy — persistence and incrementality — is already specified as a git-ref-backed cache** keyed on content-addressed inputs.
   The benefit does not apply.

Cost of the owned path: a stratified semi-naive evaluator with indexed joins over a fixed value universe is ~3–5k lines once mode-aware join ordering and metering land.
Still smaller than the Cozo translation layer alone, and every line of it is a line you need anyway.

### 2.2 Do not build the IR → ascent codegen path

The doc specifies "one IR, two emitters," with ascent codegen at build time.
**Recommendation: build one emitter.**

Once the interpreter exists it can run every rule, including the kernel.
The only rules that genuinely *cannot* be repo data are the ones governing who may push the rule ref itself — the circularity the doc correctly identifies.
That is a handful of denial rules, frozen, of the exact shape `ents-gate-rules` already has in `../git-ents`.

So: **hand-write the kernel's `ascent!` block** (port from `../git-ents/crates/kernel/ents-gate-rules`), and add a **differential test** that the interpreter, fed the same rules in text syntax over randomly generated EDBs, agrees with the compiled kernel relation-for-relation.

This is strictly better than codegen.
It is far less machinery, and a differential test against an independently-written implementation is a stronger guarantee than a codegen path that nothing exercises.
The IR remains the seam; it just has one consumer plus a test oracle.

### 2.3 Rule storage: `git-store`, and rename the namespace

Store each module through `../git-store` as a facet type under kind `rules`:

```rust
#[derive(Facet)]
struct RuleModule {
    source: String,          // the .dl text, verbatim
    imports: Vec<String>,    // declared, see 2.4
}
```

You get history (`Store::history`), schema publication, and one-ref-per-name for free.

**On the namespace, I had this backwards.**
I first said to accept `refs/store/rules/<name>` and update the doc.
But `refs/store/` names the *mechanism*, not the domain, and refs are the public API surface of this system — a rule module is a rule module regardless of what serializes it.
Keeping `refs/meta/rules/*` is right.

I checked whether that requires giving up `git-store`: it does not, but it is not free either.
`DATA_PREFIX` and `SCHEMA_PREFIX` are private consts in `gix-store/src/store.rs`, and `Store::open` takes only a repo.
So the fix is a **small additive upstream change** — `Store::open_with_prefixes(repo, data, schema)`, with `open` keeping today's defaults.
Roughly a dozen lines plus a test.
Do that rather than either forking the layout or accepting the mechanism marker.

That upstream change is a Phase 0 item, since Phase 3's `rules` agent depends on it.

Store the *source text*, not the parsed IR.
The IR is derived, and the cache key already covers the rules snapshot OID.

### 2.4 Rule-push concurrency: epoch ref now, declared imports only if needed

**Reversing my earlier recommendation.**
I claimed serialization "is an availability property git's ref store does not give you across independent pushes."
That is wrong.
Require every rule push to also advance a single `refs/meta/rules-epoch` ref, push both with `--atomic`, and concurrent rule pushes conflict by construction on the epoch's compare-and-swap; the loser retries.
It is about twenty lines and it is correct.

**Do that first.**
The availability cost is real but rule pushes are rare, and whole-program validation already reads every module tip regardless, so serialization costs nothing the validator was not already paying.

Declared imports remain the better *end state*, but for a reason that is not concurrency:

| | epoch ref | declared imports |
| --- | --- | --- |
| concurrent-push safety | yes | yes |
| bounds validation to named tips | no — reads all modules | yes |
| cross-module cycles impossible *by construction* | no — detected by search | yes |
| makes `pub` load-bearing | no | yes |
| dependents discoverable | no | yes |

Only the middle two rows matter, and only once the module count is large enough that whole-program validation on every rule push hurts.
That is a scaling threshold nobody is near.

So: epoch ref now, revisit when validation cost bites.
The migration is safe if `imports` is introduced as optional, defaulting to "imports every `pub` surface" — that preserves existing modules' semantics, so adding the declaration later is a tightening rather than a break.
Reserve the field in the `RuleModule` schema from day one so the migration does not need a schema evolution.

### 2.5 `line/2` becomes moded

The doc lists `line(Blob, N)` as enumerable EDB, then warns in prose never to enumerate it.
A footgun documented in prose is a footgun.

**Fix:** declare it `line(+Blob, -N)`.
Calling it with the blob free is then a pass-8 validation error rather than a repo-sized materialization.
One registry line converts a runtime disaster into a compile error, which is the entire point of having mode inference.

Apply the same reasoning to `commit/1`.
The doc's own `mergeable(Rev) :- commit(Rev), !blocked(Rev)` with `blocked` doing a full-tree scan is all-commits × all-blobs.
Declare `mergeable` required-bound on `Rev` and let pass 8 reject `git query run mergeable` with no `--rev`.
This is the clearest demonstration that mode inference is load-bearing for *affordability*, not just for groundedness — worth stating in the docs as such.

### 2.6 Scope boundary: git-query owns leg 1 only

Fork-bomb freedom needs three legs.
Only the first is an analysis problem:

1. **effect graph acyclic** — git-query owns this, and exports it as a public
   API over footprints (pass 7).
2. **executor writes only its declared namespace** — enforcement, lives in
   pre-receive.
3. **result ref name is a function of the trigger tuple** — enforcement, lives
   in the effect runner.

Legs 2 and 3 belong to `../git-ents`.
Building them here would make git-query depend on the forge and invert the extraction.
**git-query exports the footprint and the acyclicity check; it does not enforce anything.**

Worth recording in the docs: the predicate-level dependency graph is *coarser* than the old glob-intersection test, so the check is **sound and incomplete** — it can deny a program that would actually have been fine, and it can never admit a cycle.
Incompleteness surfaces as a spurious denial, which is debuggable.
The reverse would be a fork bomb.
That asymmetry is the whole argument and belongs in a doc comment, not in tribal memory.

### 2.7 Literal order is author order; no sideways information passing

Moded builtins must be called with their inputs bound *at call time*, so something has to decide the order in which body literals are evaluated.
Two options, and the choice has to be made before pass 8 is written because it changes what pass 8 *is*:

| | author order | SIPS reordering |
| --- | --- | --- |
| misordered body | validation error | silently repaired |
| pass 8 | checks a fixed order | searches for a valid order |
| multi-clause mode | per-clause signature, union of required-bound | meet over orderings |
| performance | author's problem, and visible | planner's problem, and invisible |
| `explain` output | matches the rule as written | matches a plan nobody saw |

**Decision: author order, and a misordered body is a validation error naming the unbound variable and the literal that needed it.**
Three reasons.
It matches the explicitness ethos the rest of the design commits to — a language that makes you declare `pub`, declare modes, and declare backing sources should not silently rewrite your rule.
It keeps pass 8 a linear left-to-right check rather than a search, which keeps error messages precise.
And it keeps derivation trees isomorphic to the source text, which is most of what makes them explicable.

The cost is real and should be documented rather than hidden: an author who writes a bad join order gets a slow query and nothing warns them.
Mitigation is a lint, not a rewrite — pass 8 can note that a reordering would have bound more variables earlier, and say so, without doing it.

This also settles the multi-clause question the mode brief would otherwise have to guess: **each clause is checked in its own order, and the predicate's required-bound set is the union over clauses.**
Union, not meet — if any clause for `p` needs argument 2 bound, every caller of `p` must bind argument 2.

Join *ordering* being fixed does not mean join *strategy* is: index selection within a literal stays the engine's business, since it cannot change which answers come back.

There is a second payoff, unnoticed until §2.10: author order supplies the sideways-information-passing strategy the magic-set rewrite needs anyway.
Fixing it in the source removes a search from the engine as well as from the validator.

### 2.8 The naive evaluator is a permanent artifact, not a stepping stone

> **Superseded by §2.11.**
> There is no naive evaluator and no semi-naive one: Nemo does both, and writing either to check the other would be writing the engine we decided not to write.
> The reasoning below is preserved because it was right about *why* — semi-naive evaluation fails by producing wrong answers rather than crashes — and that reasoning now argues for the two differentials that remain, the `ascent` kernel comparison (§2.2) and the rewritten-vs-unrewritten check (§2.10), plus the Nemo conformance suite (§2.11).
> What we gave up is the arbitrary-generated-program oracle, and that is a real loss to name rather than gloss: nothing now checks Nemo against an independent implementation on a random program.
> The kernel differential covers a fixed rule set only.

Build the naive (round-based, recompute-everything) evaluator first, then semi-naive — and **keep the naive one forever**, behind a test-only feature, as the differential-testing oracle.

Semi-naive evaluation is where the subtle bugs live: delta management across strata, negation evaluated against a completed stratum, index maintenance under insertion.
Every one of those produces a *wrong answer*, not a crash, and wrong answers in a gate are the failure mode this whole design exists to prevent.
A naive evaluator is a few dozen lines, correct by inspection, and a perfect oracle: for any program and any finite EDB the two must agree relation-for-relation.

This is the same move as §2.2's interpreter-vs-`ascent` differential, and the two are complementary rather than redundant — `ascent` checks the interpreter against an independently-written engine on the frozen kernel rules; naive-vs-semi-naive checks the interpreter against itself on *arbitrary generated* programs.
Neither subsumes the other.

### 2.9 Provenance is designed in at the start, not retrofitted

`git query explain` needs derivation trees: for a given tuple, which rule fired, with which bindings, over which supporting tuples, down to base facts annotated with their source ref.
That is a recording obligation *inside the join loop* — which tuple pairing produced which derived tuple — and it cannot be reconstructed afterward from a materialized relation, because set semantics has already discarded the multiplicity that would tell you.

So the evaluator carries a provenance sink from its first commit, behind a runtime flag: off for gates and bulk queries, on for `explain`.
Retrofitting this after the evaluator is optimized is the most expensive mistake available in this plan — it touches every join, every delta step, and every negation, and it invalidates whatever performance work was done without it.

Cost when the flag is off must be nil: a monomorphized no-op sink, not a branch per tuple.
That constraint belongs in the Phase 2 `eval` brief, and the bench suite asserts it.

### 2.10 Bound arguments are implemented by a magic-set rewrite, and nothing else will do

This is the one structural gap in the plan that nobody has named yet, and it will stop an implementer cold in Phase 2 if it is not settled here.

Semi-naive evaluation is *bottom-up*: it computes whole relations from the EDB upward.
Mode discipline is *top-down*: `reach(+Rev, -C)` says the answer is only defined relative to a `Rev` the caller supplies.
Those do not compose.
A naive bottom-up engine handed `reach` computes it for every commit in the repo and then filters — which is exactly the unaffordable materialization the modes were introduced to prevent (§2.5), arrived at from the opposite direction.
The mode system as specified is a *validator* feature with no evaluator counterpart, and declaring `+Rev` does not by itself make anything cheaper.

**Decision: the engine performs a magic-set transformation on the IR before evaluation.**
Demand for bound arguments becomes explicit `magic_p` relations, rules are rewritten to be guarded by their demand, and the result is a program that plain bottom-up semi-naive evaluation runs correctly while touching only the demanded portion of each relation.
This is the standard answer and it is the only one that preserves both halves of the design.

Three consequences, all of which need to be in the Phase 2 `eval` brief:

- **Author order is the SIPS** (§2.7).
  The magic-set transformation is defined relative to a sideways-information-passing strategy, and choosing author order means the adornment of every literal is *uniquely determined* by the source text — no search, no cost model, no planner.
  The two decisions were made for independent reasons and fit together exactly; that is the strongest evidence available that author order was the right call.
- **Stratification must be rechecked after the rewrite.**
  Magic sets can move negated literals across the rewritten rules, and the naive transformation does not preserve stratification in general.
  Use the variant for stratified programs, and — belt and braces — run pass 5 again on the rewritten program as an internal assertion.
  If it ever fires, that is an engine bug, not a user error, and the message should say so.
- **The rewrite is a provenance and `explain` problem.**
  Derivation trees must be reported over the *source* rules, not the rewritten ones; `magic_p` nodes are engine bookkeeping and would be incomprehensible in output.
  So the rewrite carries an origin back-reference on every generated rule, and the provenance sink (§2.9) resolves through it.
  Designing this in is cheap; retrofitting it means the first `explain` output anyone sees is full of `magic_` relations.

The rewrite gets its own test, and with §2.8's oracle gone it is the only one guarding it: for programs where unrestricted bottom-up evaluation is affordable, rewritten and unrewritten runs must agree on the demanded subset.
Both runs go through the same engine, so this checks the rewrite and not the evaluator — which is the right target, since the rewrite is ours and the evaluator is not.

### 2.11 Nemo is the engine, and it is the only one

**This supersedes the parts of §2.1 and §2.8 that assume a hand-written semi-naive interpreter.**
It does not disturb §2.2 (`ascent` for the compiled kernel), §2.7 (author order is the SIPS), §2.9 (provenance), or §2.10 (the magic-set rewrite) — all four are frontend decisions and survive underneath any engine.

The decision was taken on evidence: a timeboxed spike embedded Nemo and ran six gates.
`spikes/nemo-spike/spike-report.md` is the full report; 55 tests back it.

**One way to do things.**
An earlier draft of this section put two engines behind a trait and shipped the one we wrote.
That is rejected.
Two implementations means two sets of bugs, two performance profiles, a differential suite to keep green, and a seam whose only client is a test — all paid for now against a benefit that arrives only if we ever swap.
There is one engine, it is Nemo, it is named concretely, and `gix-query-eval` calls it directly.
If a second engine ever earns its place, extracting a trait from one working implementation is a smaller job than maintaining two of them from the start.

**What we are accepting, stated once so it is not rediscovered as a surprise.**

- **The workspace is nightly-only.** `nemo` and `nemo-physical` use `#![feature(…)]`, so everything linking them needs nightly. `rust-toolchain.toml` at the root pins an exact nightly; a floating channel would make our build reproducibility a function of upstream's mood.
- **Nothing in the dependency chain can be published to crates.io.**
  Publication of `nemo` stopped at a yanked `0.2.1`, so it enters as a git dependency, and crates.io refuses any crate that has one.
  `gix-query` and `git-query` are therefore installed from source until Nemo publishes.
- **230 transitive crates**, including an HTTP client, an LSP implementation, and an RDF/SPARQL stack, none of it reachable from anything we do.
- **No concurrency, no cancellation.**
  The API is `!Send` throughout and `execute()` enters a process-global timing mutex.
  One evaluation per process, no watchdog thread, and caps become admission-time refusals rather than truncation.

What we get for it is the part that would have taken longest to write and been hardest to trust: set semantics, stratified negation, termination on cyclic input, and ten thousand facts to fixpoint in about five milliseconds, all correct on the first attempt.

**Validate the subset before executing.**
The fragment boundary is ours to hold — nothing in Nemo refuses arithmetic, aggregates, existential heads, or directives.
It is held in three places, in increasing order of how much we should rely on them:

1. **By construction, which is where almost all of it belongs.**
   The IR has no term-level operations, so arithmetic and aggregates have no lowering path and are *unrepresentable* rather than *rejected*.
   [`Symbol`](#212-make-invalid-states-unrepresentable) cannot hold a `"`, so the emitter cannot produce the one byte Nemo's string lexer cannot carry.
   A `NemoProgram` can only be constructed by the emitter from a `CheckedProgram`.
   See §2.12 — this is the single largest application of that doctrine in the plan.
2. **By an emission audit, immediately before execution.**
   After lowering, hand the emitted text back to Nemo's own parser and assert that the program it recovers has exactly the predicates, arities, and rule count we lowered.
   This is cheap, it runs every time, and it catches the class of emitter bug that types cannot: a symbol that terminated a literal early, a name that collided, a rule that silently vanished.
   **This is the check that answers "do we have the subset we support" for a given program.**
3. **By a conformance suite pinned to the Nemo commit.**
   The spike's six gates are promoted into `gix-query-eval`'s test suite, rewritten against our real types.
   They assert what we *believe* about Nemo — that a bare identifier is a constant, that strings are not unescaped, that unstratifiable programs are rejected at load, that no value outside our two domains comes back.
   **This is the check that answers "do we have the subset we support" for a given Nemo version**, and it is what the upgrade audit runs.

**Six spike findings that are implementation obligations, not commentary.**

1. **EDB never travels as program text.**
   Nemo's program parser runs at roughly 109 KB/s: ten thousand facts cost 1.74 s as inline text and 6.5 ms through an in-memory CSV resource provider, for identical 5 ms evaluation.
   Facts are asserted through an in-memory `ResourceProvider`, never by string concatenation.
   Make this a type distinction, not a convention: rules lower to `NemoProgram`, facts lower to `NemoTables`, and there is no function taking a fact to program text.
2. **A `"` cannot be written into Nemo program text at all**, escaped or otherwise — its string lexer takes every byte up to the next quote and does no unescaping, so an emitter that escapes silently corrupts.
   Git permits `"` in path names and author names.
   Constants in *rules* do go through program text, so this is a `Symbol` construction error surfaced as a parse diagnostic and as a host-fact error, never a lowering panic.
3. **Negation leaves no node in a Nemo derivation tree.**
   Its tracer walks only positive body atoms, so a derivation that turned on `~revoked(alice)` produces a tree that never mentions `revoked`. §1.11 and Phase 4 require a negation witness in `explain` output.
   **That node comes from our rewrite and our own recording, never from the engine** — which is another way of saying §2.9 was right to insist provenance is designed in rather than borrowed.
4. **Truncation is not expressible.**
   Nemo has no row cap, no timeout, and no working cancellation — `tokio::time::timeout` compiles and does nothing, because its futures never yield.
   Exit code 3 therefore has no producer.
   See the Phase 4 amendment.
5. **A bare identifier is a constant, not a variable, and lands in an IRI domain.**
   `out(X) :- in(X).` is valid Nemo that parses, validates, runs, and derives nothing.
   Every symbol we emit is quoted; every value read back is `TryFrom`-checked into our two domains and refused otherwise.
6. **`@export` directives survive into the engine.**
   Rule modules are text loaded from refs.
   If that text reached the engine unfiltered, a repository could make a query write to disk.
   Our grammar has no directives and our emitter is the only producer of `NemoProgram`, so the bypass does not exist — but the conformance suite asserts that it does not, because "the type system prevents it" is a claim that decays.

**Non-goals, stated so they are not rediscovered as good ideas.**

- **No second engine, and no trait anticipating one.**
  Extract the seam when there is something to put behind it.
- **No incremental or DBSP-style evaluation in the CLI path.**
  Per-stratum, footprint-keyed caching only (Phase 6).
  The demand loop already re-runs the whole program per round; making *that* incremental is a different project.
- **No Nemo aggregates, arithmetic, or existential rules**, even where convenient.
  The fragment is frozen by `docs/query-and-rules.adoc`, not by what an engine happens to offer.

**Standing task — engine upgrade audit.**
A Nemo version bump is a semantics change, not a dependabot patch.
Procedure, in `CONTRIBUTING.md`: bump the pinned `rev` in a branch; run the conformance suite; run the golden `explain` traces; run the kernel differential against `ascent`; diff the §2.11 fragment inventory against upstream's changelog; only then merge.
The pin is an exact commit, and `spike-report.md` records which commit the current findings describe.

### 2.12 Make invalid states unrepresentable

A standing constraint on every crate, not a phase.
It goes here rather than in a style guide because in this project it is load-bearing: the whole value proposition is that a gate's verdict can be trusted, and every one of §1's fifteen defects was a state the design permitted and did not want.

The rule: **if a value can be wrong, the type should not exist.**
Concretely, and these are obligations in the agent briefs, not suggestions:

- **Parse, do not validate.**
  Every stage produces a type only that stage can construct.
  `Module` comes from the parser; `CheckedProgram` from the validator and from nowhere else; `RewrittenProgram` from the magic-set rewrite; `NemoProgram` and `NemoTables` from the emitter.
  A function that needs a validated program takes a `CheckedProgram` rather than a `Program` plus a promise.
  This is already how `gix-query-ir` is built and it is the pattern to extend.
- **Newtype every identifier.**
  `PredicateKey`, `Var`, `RuleId`, `StratumId`, `ArgIndex`, `Symbol`, `RefGlob` — no bare `String` or `usize` crossing an API boundary where a different one would type-check.
  The bug this prevents is the one where a rule index is passed as a literal index and nothing complains.
- **Validate at construction, once.**
  `Symbol::new` refuses a `"` and returns `Result`; nothing downstream re-checks, because nothing downstream can hold an unchecked one.
  Same for `PredicateKey` and the `magic_` reservation.
- **Enums over booleans and over `Option` pairs.**
  `Mode::{Bound, Free}` not `bool`.
  A truncation outcome is `Outcome::{Rows(NonEmpty<Tuple>), Empty, Refused(Reason)}`, not a `Vec` plus a `truncated: bool` that a caller can ignore — the spike's exit-code work exists precisely because an ignorable flag is how a gate reads a partial answer as clean.
- **`NonEmpty` where empty is meaningless.**
  A rule body, a stratum, an SCC.
- **Make the illegal combination fail to compile.** `PredicateClass::Edb` carries a `ModeSet` and `Idb` carries an inferred `ArgSet`; there is no struct with both fields and a comment about which applies.
- **Exhaustive matches, no catch-all arms** on our own enums, so adding a variant is a compile error at every site that must consider it.

Two cautions, because this doctrine has a failure mode of its own.
Do not encode in types what belongs in a test — a type cannot express "these two engines agree" or "this terminates".
And do not add a type parameter whose only inhabitant is one concrete type; a phantom typestate with a single state is ceremony, and §2.11's rejection of a single-implementation trait is the same judgment applied one level up.

---

## 3. Crate layout

Create **all** skeletons in Phase 1, before fanning out, so each agent edits only inside its own crate and no two agents ever touch the same file.

| crate | contents |
| --- | --- |
| `gix-query-ir` | terms, literals, rules, program, predicate registry, signatures, mode sets, builtin property table, IR canonicalization (§1.1). **The seam.** |
| `gix-query-parse` | text → AST → IR, plus the pretty-printer. Owns the grammar and diagnostics. |
| `gix-query-check` | passes 1–9. Owns stratification, SCCs, mode inference, footprints. Sole producer of `CheckedProgram`. |
| `gix-query-eval` | magic-set rewrite (§2.10), Nemo lowering and the emission audit, the demand loop and its call log, derivation-tree recovery. The **only** crate permitted to name a `nemo` type (§2.11). |
| `gix-query-host` | repo-backed EDB providers, `bind`/`bind_fuzzy`, backing-source declarations. Only crate that depends on `gix-anchor`. |
| `gix-query-rules` | module load/store via `gix-store`, whole-program assembly, epoch CAS, cache key. |
| `gix-query-kernel` | the frozen `ascent!` denial rules + differential test harness. |
| `gix-query` | facade: the library surface everything above composes into, plus the footprint/acyclicity export (§2.6). |
| `git-query` | **the one binary**, `git query`: `run` (three tiers), `explain`, `rules`, `predicates`. Owns the formatter, and therefore abbreviation (§1.9) and span coalescing (§1.8). |
| `test-support` | copied from `../git-store`, trimmed. |

Naming is mechanical and has no exceptions: **`gix-*` is a library, `git-*` is a binary**, matching `../git-store` and `../git-anchor`.
There is one `git-*` crate in this workspace and there will not be a second — `explain` and `rules` are subcommands, not binaries (§1.10).

`gix-query-eval` must **not** depend on `gix` or `gix-anchor`.
Builtins and EDB providers reach it through the traits defined in `gix-query-ir`.
This is what makes evaluation property-testable over synthetic facts with no repo at all, and it is the single most important structural constraint in this plan.
The practical consequence for scheduling: **`gix-query-ir`, `-parse`, `-check` and `-eval` have no git dependency at all**, so Phases 1–2 can run entirely in parallel with the `../git-anchor` work of Phase 0.

The whole workspace builds on the nightly pinned by the root `rust-toolchain.toml`, because `gix-query-eval` links Nemo (§2.11).
There is no stable subset and no feature gate carving one out — a split build is a second way to do things, and it would be a way that silently stops being tested.

Four splits considered and rejected.
A separate crate for the engine seam, and a second implementation behind it — see §2.11: one client, and that client a test.
A separate crate for the magic-set rewrite and demand loop, apart from the Nemo lowering.
The rewrite is engine-agnostic in principle, but with one engine the split buys a boundary nobody crosses, and the demand loop and the lowering share the call log; they are modules, not crates.
A separate registry crate (signatures and builtin declarations apart from the IR) buys nothing here — the registry *is* the contract Phase 1 freezes, and splitting it gives two things to freeze instead of one.
A separate syntax crate holding the AST apart from the IR would add a translation layer whose only job is to be traversed; the parser produces IR directly and keeps spans on IR nodes for diagnostics.

---

## 4. Phases and agent dispatch

Dispatch rule throughout: **an agent gets a frozen contract, a spec section, and a test obligation.**
No agent may edit `gix-query-ir` after Phase 1, and no agent may edit another agent's crate.
Because crates are disjoint, worktree isolation is unnecessary; the only shared file is the root `Cargo.toml`, which Phase 1 finalizes.

### Phase 0 — solo for the doc; two upstream agents dispatched immediately

Apply §1 corrections to `docs/query-and-rules.adoc` and record §2 decisions.
This part is judgment, not typing — it does not parallelize and must not be delegated.

Three of the eleven are rewrites rather than edits, and are worth budgeting for: the `Review vocabulary` section is replaced wholesale by §1.7, the `Builtins` block loses two entries and gains six comparisons (§1.8, §1.9), and the `CLI` and `Explain` sections are restructured around one binary (§1.10).
The doc should also gain the two things it never said: that the engine is a hand-written interpreter, and that mode discipline is implemented by magic sets (§2.10).

Two upstream changes are now on the critical path and should be dispatched at the same time, because Phase 3 blocks on both:

- **Agent `anchor-axes`** → `../git-anchor`.
  Four additive changes, all in `gix-anchor`, none breaking:
  1. Public `Position`/`Content` decomposition per §1.5, exported as
     `gix-anchor`'s vocabulary alongside `Projection::label`. `git-query` must
     consume the axes, never re-derive position by comparing paths — the diff
     knows about renames and a post-hoc path compare does not.
  2. `project_candidates(&repo, &anchor, target) -> Vec<Projection>`, so multiplicity is representable.
     No `Ambiguous` variant (§1.5).
  3. `PROJECTION_HEURISTIC_VERSION` per §1.6, plus a doc-comment warning on
     `project` that its GC-dependent fallback makes it unsuitable for cached or
     gating callers.
  4. **Batched projection** — `project_many(&repo, &[Anchor], target)` that walks the target tree once.
     `bind` is called inside a fixpoint over potentially every anchor at one rev, and per-call tree resolution is the difference between linear and quadratic.
     This one is driven purely by being the caller; nothing upstream would have surfaced it.

Obligation: existing projection suite stays green; a fixture where one blob's content appears at two paths in the target tree; and a regression test pinning the rename-under-GC divergence from §1.6 so the two entry points' differing behavior is documented rather than incidental.

- **Agent `store-prefix`** → `../git-store`.
  Add `Store::open_with_prefixes` per §2.3, `open` unchanged.
  Obligation: one test that a non-default prefix round-trips store/retrieve/history.

Both are small, additive, and in repos with existing test suites, which makes them good delegation targets.
Neither blocks Phases 1 or 2.

### Phase 1 — solo, no agents: scaffold and freeze the IR

Copy the `../git-store` scaffold per the `../git-anchor/DEVPLAN.md` recipe (`.config/`, `.github/workflows/`, licenses, lint config, edition 2024).
Create every crate skeleton from §3.
Then write `gix-query-ir` by hand.

`gix-query-ir` is the contract every downstream agent codes against, so it is worth more care than anything else in the plan.
It must pin, precisely:

- the term language — variable, constant, anonymous; **no compound terms**
- the literal forms — positive, negated, comparison, builtin call
- the signature registry: per-predicate arity, argument types, `pub` visibility,
  EDB/IDB/builtin classification, and **a mode *set* for EDB predicates**
  (§1.7) against a single inferred required-bound set for IDB predicates
- the builtin property table from §1.4, all six properties, including the
  comparison builtins declared in §1.8 — the registry is the only place `!=` and
  `<=` are defined, so omitting them here makes them unimplementable downstream
- the backing-source field per EDB predicate (§1.1: `RefGlob(g)` or
  `ContentAddressed`), which pass 7 and the cache key both read
- IR canonicalization (§1.1) — alpha-renaming and serialization, since the cache
  key and the program-identity snapshot both depend on it and neither should
  reimplement it
- the `FactSource` trait (EDB providers) and `Builtin` trait (moded calls) —
  both defined *here*, implemented in `gix-query-host`
- the `Provenance` sink trait (§2.9), including its no-op implementation, so the
  evaluator can be written against it from the first commit
- the **adornment annotation** (§2.10) — pass 8 computes it in `gix-query-check`
  and the magic-set rewrite consumes it in `gix-query-eval`, so the type has to
  live here or those two crates end up depending on each other

**Gate: nothing in Phase 2 starts until this compiles and is reviewed.**
An IR change after fan-out invalidates three agents' work simultaneously.

### Phase 2 — two agents, parallel

`parse` landed ahead of this phase, recovered from the branch the engine decision parked it on: the grammar and the IR never depended on which engine consumes the result.
What remains shares only `gix-query-ir` and never each other.

Both briefs carry §2.12 verbatim.
It is not a style note here — the fragment containment that §2.11 relies on *is* a type-system argument, and an agent that reaches for `String` and a validation function has quietly moved a compile error into a runtime one.

- **Agent `check`** → `gix-query-check`.
  Passes 1–9, and `CheckedProgram` constructible from nowhere else.
  Five subtleties to state in the brief, because all five will otherwise be guessed wrong:
  - *Mode checking is left-to-right over the body **as written*** (§2.7).
    No reordering, no search.
    A literal whose required-bound argument is unbound at its position is an error naming the variable and the literal.
  - *Mode inference is itself a least fixpoint.*
    Start every IDB argument free, propagate boundness from builtins and bound call sites to convergence.
  - *A predicate's required-bound set is the **union** over its rules*, not the intersection.
    Conservative.
    If any rule for `p` needs argument 2 bound, `p` requires argument 2 bound.
  - *Pass 7's footprint carries transitive **builtin** use as well as transitive base predicates* (§1.11), because the `bind_fuzzy`-in-gates prohibition and the ambiguity lint are both defeated by a wrapper predicate otherwise.
    Pass 7 is parameterized over the backing-source map rather than reaching into the host — the map is registry data, so `gix-query-check` keeps no git dependency.
  - *A symbol containing `"` is a diagnostic here, not a panic later* (§2.11 finding 2).
    The refusal belongs to `Symbol::new`, so this pass only has to surface it with a span; what the brief must prevent is the agent "helpfully" escaping instead.

  Obligation: one accept-fixture and one reject-fixture per pass, each reject naming the pass that fired; the corrected §1.7 vocabulary as an end-to-end accept case; and **the doc's original `introduced/2` as a pass-8 reject case**, since that is the exact bug the pass exists to catch.

- **Agent `eval`** → `gix-query-eval`.
  The largest brief in the plan, in four parts, in this order:

  1. **Magic-set rewrite** (§2.10), IR to IR.

  Without it the mode system has no runtime meaning and the first `+Rev` predicate materializes the repo.
  Author order is the SIPS, so no strategy has to be searched for.
  Stratification is rechecked on the rewritten program as an internal assertion; if it fires, the message says it is an engine bug.
  Every generated rule carries its origin back-reference and body-literal map, which `gix-query-ir` already provides — `explain` reports over source rules, never over `magic_` relations.

  2. **Nemo lowering, and the emission audit.**

  Rules become a `NemoProgram`; facts become `NemoTables` served from memory.
  There is no function from a fact to program text, and that is a type-level fact rather than a review comment (§2.11 finding 1).
  Before execution, the emitted text is parsed back by Nemo and its predicates, arities, and rule count asserted against what was lowered.

  3. **The demand loop and its call log.**

  Proven in `spikes/nemo-spike`; promote it, rewritten against the real types.
  The log records builtin, bound arguments, answer count and round, because `explain` and the pass-7 footprint audit both read it and neither can get that from the engine.

  4. **Derivation-tree recovery.**

  Nemo's trace mapped onto our tree, with the negation gap filled from the rewrite rather than papered over (§2.11 finding 3), and `magic_` bookkeeping filtered out.

  Obligation, in the same order: the spike's A2 golden set, rewritten against real types, asserted by exact set equality; a rewritten-vs-unrewritten agreement test on programs small enough to evaluate both ways; an emission-audit test where a deliberately corrupted emitter is caught; the spike's A3 cases — flat query converges in two rounds, chained demand discovers a call in round 2 and converges in round 4, a cycle terminates because the log memoizes, a call with no answers is logged rather than dropped, and a builtin whose stub table holds ten thousand rows is called exactly once; the spike's A6 cases as a **conformance suite** asserting what we believe about the pinned Nemo (§2.11); a property test that every validator-accepted program reaches fixpoint within a fuel bound, fuel exhaustion being a test failure rather than a skip; and a golden derivation tree for a positive goal, a recursive goal, and a negation witness.

  If this brief needs splitting, split it after part 2: rewrite and lowering first, demand loop and tracing second.

### Phase 3 — two agents, parallel; starts when Phase 2 merges

- **Agent `host`** → `gix-query-host`.
  `FactSource` implementations for each repo-backed EDB relation over `gix`, plus `bind` and `bind_fuzzy` — the only two minting builtins left after §1.8 and §1.9 remove `span_diff` and `abbrev`.
  They implement the `Builtin` and `FactSource` traits `gix-query-ir` already defines, which is the shape the spike stubbed, so this phase is `gix-query-eval`'s demand loop swapping stub tables for git-anchor and git-store calls and nothing structural changing.
  If it turns out to be more than that, the traits were wrong and `gix-query-ir` should be fixed rather than worked around.
  `bind` emits both axes per §1.5 — **no interim mapping**, which is why this waits on `anchor-axes`.
  Each EDB predicate declares its backing source (§1.1) alongside its implementation, so the two cannot drift; that declaration is what turns pass 7 from predicate names into ref globs, so this is also where pass 7 first runs end to end — footprint → backing namespaces → cache key, plus the fuzzy-taint rejection and the ambiguity lint.
  First real query against a real repo lands here.

  Obligation: fixture repos via `test-support`; each relation asserted against a repo built by shelling out to real `git`, so the oracle is git itself and not our own reader; **one fixture exercising every EDB predicate**, which is what makes the registry's mode sets (§1.7) testable rather than aspirational.

- **Agent `rules`** → `gix-query-rules`.
  Module load/store via `gix-store` at `refs/meta/rules/*` (needs `store-prefix`), whole-program assembly, the epoch-ref CAS from §2.4, program-identity snapshot, and the cache key from §1.1.
  Reserve the `imports` field in the schema; do not implement the DAG.
  Obligation: a test that two independently-valid modules composing into a negation cycle are rejected by whole-program validation, and a test that two concurrent rule pushes cannot both win the epoch CAS.

### Phase 4 — two agents, parallel

- **Agent `cli`** → `git-query`.
  One binary, subcommands `run` (three tiers), `explain`, `rules add|check|api|list`, `predicates`, per §1.10.
  Exit codes per grep convention (`0` rows, `1` none, `2` error) plus `3` for truncation, which the docs must name as fail-closed for gates (§1.11).
  Column order from the signature registry — that is the tsv contract, so it gets its own test.

  **Exit code 3 has no producer, so it is not issued.**
  Nemo has no row cap, no timeout, and no cancellation that works (§2.11, spike gate A5), so there is no way to hand back a partial relation and say so.
  A cap is therefore an **admission-time refusal** — count the EDB, project the cost, decline to start — and that is exit 2.
  **Do not fake exit 3**: an exit 3 that did not follow a partial answer would teach gate authors to trust a signal that sometimes means nothing, which is worse than never emitting it.
  Exit 2 is already fail-closed, so the behavior is safe, merely less informative.
  Keep the code reserved and documented as reserved.

  The formatter owns everything the language shed: `--abbrev` over `oid` columns (§1.9) and span coalescing over consecutive `unreviewed_line` rows (§1.8).
  Both get golden-output tests, because both are now the *only* place that behavior exists.

  Tier ordering within the phase: tiers 1 and 2 first, tier 3 `--goal` last — tier 3 is a thin shell over the parser and is the least likely to shake out desugaring bugs.
  Tier 2 fills a filter predicate's required-bound arguments from the ambient flags (`--rev`, `--in`) and errors when it cannot (§1.11).

  `explain` is built on the Phase 2 provenance sink (§2.9), never by re-deriving; it refuses general why-not goals rather than approximating, and the refusal message says *why* no finite witness exists.
  Negation leaves in a derivation tree are annotated with the program snapshot and footprint digest that made them true, since "not provable" is only reproducible relative to those.
  Obligation: golden derivation trees for a positive goal, a recursive goal, and a negation witness.

  `explain` splits into two pieces, because §2.11 made the boundary between them real:

  - **Tree extraction**, from `gix-query-eval`.

  Nemo's trace is typed and leaf-matchable, but it **carries no node for any negated literal** and returns only *one* derivation where several exist (spike gate A4).
  Both gaps are filled from our own rewrite, above the lowering, never worked around inside it.

  - **Re-annotation.**

  Join the tree's leaves against the demand call log to attach builtin modes, the projection heuristic version, source refs, and the rules-snapshot OID.
  This is the step that turns a derivation into an audit record, and none of it can come from the engine, which is why the log exists.

- **Agent `effects`** → footprint export in `gix-query`.
  Public API returning a goal's transitive base-predicate footprint and the acyclicity verdict over a set of effect declarations.
  Leg 1 only, per §2.6.
  Obligation: doc comment stating the sound-and-incomplete asymmetry; a test that a spurious denial is a spurious *denial* and not a spurious admission.

### Phase 5 — solo: kernel and differential test

Port `../git-ents/crates/kernel/ents-gate-rules` into `gix-query-kernel` as the frozen bootstrap.
Write the same rules in text syntax as a fixture.
Differential test: random EDBs, both paths, assert relation equality.
This is the whole justification for keeping ascent, so it should not be delegated.

### Phase 6 — solo: cost, and the cache last

See §5 for benchmarks and the admission-time cost gate.

**The cache is deliberately the final thing built**, after everything it depends on exists and is trusted: footprint digests (§1.1, Phase 3), the builtin registry digest (§1.6, Phase 1 + the `anchor-axes` heuristic version), and goal canonicalization (§1.1, Phase 1).
Building it earlier means guessing at those and keying on a subset, which is precisely the §1.1 bug reintroduced by schedule pressure rather than by oversight.

Ship it behind a flag, off by default, with a **cache-vs-cold differential test in CI**: run the whole query corpus twice, once with the cache primed and once with it disabled, and assert identical relations.
That test is the only thing standing between a subtly incomplete cache key and a stale gate verdict, and it costs one CI job.

Per-stratum, footprint-keyed caching is the whole of it.
**No incremental or DBSP-style evaluation in the CLI path** (§2.11 non-goals) — the demand loop already re-runs the whole program per round, and making that incremental is a different project with a different risk profile.

---

## 5. Affordability is the real risk

Passes 1–9 give termination.
They give nothing about cost: data complexity is PTIME but combined complexity is EXPTIME-complete, so a five-variable body over a million blobs terminates long after everyone involved is dead.
Treating this as a footnote is the most likely way this project fails in production, so it gets a phase.

Concretely, the doc's own `mergeable` is the worst case in the spec: it scans every tree entry at a rev and negates over `reviewed`.
That is the canonical benchmark and it should be in the bench suite from day one.

§1.7 adds a second, and it is worse.
Fixing `introduced` cost it a `Rev` argument and a `reach` recursion, so it now walks the full ancestry of `Rev` and scans every tree at every ancestor.
Well-moded, terminating, and completely unaffordable on any real repo.
The `reach_in`/`--in A..B` scoped form is the answer, which makes **range scoping a correctness-adjacent feature rather than a convenience flag** — and makes it a Phase 4 tier-2 obligation rather than something to add when someone complains.
Bench both: `reach` unscoped is the number that tells you how badly the scoped form is needed.

**On the gate-timeout open question.**
Fail closed is a push DoS; fail open makes gates bypassable by making them slow.
My first answer was admission-time benchmarking, which is real but is early warning, not a guarantee — an adversary grows data until an already-admitted gate exceeds its cap, and admission said nothing about that.

The option that actually removes the dilemma is one neither of us costed initially: **move gate evaluation off the push path entirely.**
Evaluate repo-authored gates at merge/promotion rather than at pre-receive.

This works better than it first appears, because it makes the compiled-kernel/interpreted-rules split do double duty.
That split was introduced for circularity — the rules governing who may push the rule ref cannot themselves be repo data.
But it lands on exactly the same line as the cost boundary:

| | evaluated at | rules | cost bound | on failure |
| --- | --- | --- | --- | --- |
| bootstrap kernel | pre-receive | frozen, compiled | bounded by construction, not by data | fail closed |
| repo-authored gates | merge/promotion | dynamic, interpreted | data-dependent, unbounded | fail closed |
| advisory pre-check | pre-receive | dynamic, interpreted | capped, best-effort | **fail open** |

The push path only ever runs rules whose cost does not depend on repo size, so there is no DoS surface there.
Promotion runs the expensive ones, but promotion is lower-frequency, more-authorized, and typically already serialized, so the blast radius is one promotion queue rather than every push by everyone.

The fail-open/fail-closed dilemma dissolves once you notice the two answers apply to two different evaluations.
The advisory pre-check *can* fail open precisely because it is not the gate — it exists only to give the author early feedback, and its being skippable costs nothing.

Cost of this: later feedback.
An author learns their change is unmergeable at merge time rather than push time, mitigated but not erased by the advisory pre-check.
That is a genuine product tradeoff and it should be Joey's call, not mine — see open question 5.

Runtime caps stay mandatory in all three rows regardless.
Admission-time benchmarking stays too, demoted to what it actually is: early warning that catches the honest mistake, not a defense against the adversarial case.

---

## Definition of done

- `cargo test --workspace` green, doctests included.
- The corrected review vocabulary from §1.7 parses, validates, evaluates, and
  answers `git query run unreviewed` against a fixture repo — and the doc's
  original `introduced/2` is rejected by pass 8 with a message naming `C`.
- Differential tests, all three: rewritten vs unrewritten over programs
  affordable both ways; `gix-query-eval` vs the `ascent` kernel over random
  EDBs; cache-primed vs cold over the query corpus.
- The Nemo conformance suite passes against the pinned commit, and the emission
  audit is exercised by a test that corrupts the emitter and watches it caught
  (§2.11).
- `reach(+Rev, -C)` at a fixture repo's `HEAD` touches only ancestors of `HEAD` — asserted by counting EDB reads, not by wall clock.
  This is the test that proves the mode system does something at runtime and not only at validation.
- Property test: every validator-accepted program terminates within fuel.
- A query over a cost-gated EDB is refused at admission with exit `2`, and a gate fixture treats that as failure.
  Exit `3` is asserted **not** to be issued, since nothing can produce it honestly (§2.11).
- Golden `explain` trees for a positive goal, a recursive goal, and a negation
  witness.
- A fixture repo exercising every EDB predicate in the registry.
- Bench suite covers `mergeable` and unscoped `reach` over a repo large enough
  to matter, plus the scoped `reach_in` form for comparison.
- One binary. `ls target/release | grep '^git-'` yields exactly `git-query`.
- Repo file tree is a believable sibling of `../git-store` and `../git-anchor`.

---

## Open questions — genuinely open, recommendations where I have one

1. **Line or byte spans** — *closed by §1.8, and worth noticing that it was closed by an unrelated decision.*
   Deleting `span_diff` in favor of per-line negation makes `line(+Blob, -N)` the unit of uncovered-region reasoning, and there is no `byte(+Blob, -N)` that anyone would want to enumerate.
   Combined with `gix-anchor` already committing to `LineRange`, spans are lines.
   Byte ranges keep their advantages — total, encoding-agnostic — and lose anyway, because the language now has a relation for lines and cannot have one for bytes.
   Record this in the doc as decided rather than leaving it open; a reader who finds it listed as open will reasonably assume it is still negotiable.
2. **`ambiguous` and effect triggering** — *mostly closed.*
   With ambiguity derived rather than stamped (§1.5), "fire once, per candidate, or not until disambiguated" becomes three rules an author writes, not three semantics the engine must choose between.
   What remains open is only which of them ships as the default in the review vocabulary — a much smaller question.
3. **Cross products.**
   Well-defined and quadratic.
   Lean toward explicit opt-in over a cardinality cap: a cap makes an expensive query silently *wrong* at the boundary, an opt-in makes it loudly refused.
4. **Predicate stability.**
   A module you do not control depending on your predicate makes your refactor their veto.
   Recommend deferring.
   Note that choosing the epoch ref over declared imports (§2.4) gives up cheap dependent *discovery* — with no import declarations you must parse every module to find who references your predicate.
   That is acceptable at current scale and it is the second reason declared imports eventually win, but it means this question stays fully open rather than becoming a small feature on top of existing machinery.
5. **Closed: Nemo is the only engine.**
   §2.11 records the decision and what it costs.
   The two alternatives that remain live are **vendoring Nemo** — removes the crates.io blocker and pins semantics exactly, keeps the nightly requirement, takes on 230 crates of maintenance — and **waiting for Nemo to publish**, which is not actionable on this schedule.
   Revisit if publication of `gix-query` becomes a requirement, or if a Nemo bump ever fails the conformance suite in a way we cannot work around above the lowering.
6. **Gate evaluation point — the biggest product call here.**
   Pre-receive versus merge/promotion (§5).
   Moving gates to promotion removes the push-path DoS surface entirely; the price is that authors learn about gate failures later, softened but not erased by an advisory pre-check.
   This trades a security property against feedback latency, which is a product decision, not a technical one.
   It also affects what `../git-ents` must implement in pre-receive, so it should be settled before Phase 4's `effects` agent finalizes the exported API shape.
