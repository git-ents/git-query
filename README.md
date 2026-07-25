# git-query

Ask a Git repository questions in Datalog, and let the repository carry the rules.

Facts come from the repo itself — commits, trees, authors, members, signed claims, and the projection of an anchor onto a later revision.
Rules are stored as refs under `refs/meta/rules/*`, so a policy such as "this revision is mergeable" is repository data with history, review, and an audit trail, rather than code in a hook.

## Demo

```console
$ git query run unreviewed --rev HEAD
9a3f21c4  src/lib.rs

$ git query run blob --with unreviewed --in main..HEAD
$ echo $?
1                                     # no rows: nothing unreviewed on this range

$ git query explain 'mergeable(HEAD)'
mergeable(HEAD)
└─ !blocked(HEAD)                     # negation witness
```

Exit codes follow grep: `0` rows, `1` none, `2` error — plus `3` for truncation at a cap, which a gate must treat as failure.
A truncated relation over-approves when the gate asks "is anything blocked?" and under-approves when it asks "is everything reviewed?", so there is no safe reading of it, only a safe refusal.

## What is actually being built

A mode-checked, magic-set-rewritten Datalog frontend over a git-backed extensional database, evaluated by a fixpoint engine reached through a seam.
Everything that makes this project what it is — the grammar, the validation passes, the mode system, the rewrite, footprints, derivation trees — sits above that seam.
What sits below is only "compute the least fixpoint of a stratified program over a finite EDB", and that is a commodity.

The fixpoint engine is [Nemo](https://github.com/knowsys/nemo), and it is the only one.
That buys stratified evaluation that was correct on the first attempt and costs a nightly toolchain and a git dependency, which is the trade `DEVPLAN.md` §2.11 records in full.

Two properties do most of the work.

**Modes are validation, and they are also affordability.**
Declaring `commit(+Rev)` turns "compute mergeability for every commit in the repository" from a query that runs for an hour into a compile error.
The same declaration is what makes the answer cacheable, since a predicate whose extension depends on an unbound revision has no finite ref set to key on either.

**Mode discipline is implemented by a magic-set rewrite.**
Semi-naive evaluation is bottom-up and modes are top-down; they do not compose without one.
Body literals are evaluated in author order, which fixes the sideways-information-passing strategy, so no planner has to invent one.

## Crates

| crate | |
| --- | --- |
| [`gix-query-ir`](crates/gix-query-ir) | terms, rules, the signature registry, canonicalization, and the traits the engine reaches the world through — the frozen seam |
| [`gix-query-parse`](crates/gix-query-parse) | text to IR, and the pretty-printer |
| [`gix-query-check`](crates/gix-query-check) | the nine validation passes |
| [`gix-query-eval`](crates/gix-query-eval) | the magic-set rewrite, the Nemo lowering, the demand loop, and derivation trees |
| [`gix-query-host`](crates/gix-query-host) | repo-backed fact sources and the projection builtins |
| [`gix-query-rules`](crates/gix-query-rules) | rule-module storage, whole-program assembly, the cache key |
| [`gix-query-kernel`](crates/gix-query-kernel) | the frozen compiled denial rules, and the differential test oracle |
| [`gix-query`](crates/gix-query) | the library surface, plus the footprint and acyclicity export |
| [`git-query`](crates/git-query) | the one binary, `git query` |

`gix-*` is a library and `git-*` is a binary, with no exceptions.
There is one `git-*` crate in this workspace and there will not be a second: `explain` and `rules` are subcommands.

See [`docs/query-and-rules.adoc`](docs/query-and-rules.adoc) for the normative specification.

## Code of Conduct

Please refer to the in-source [code of conduct](/CONDUCT.md) for all behavioral expectations.

## Contribution Guide

Contributions are welcome.
Please refer to the in-source [contribution guide](/CONTRIBUTING.md).

## License

Licensed under either of [Apache License, Version 2.0](/LICENSE-APACHE) or [MIT license](/LICENSE-MIT) at your option.
