# Contribution Guide

Thank you for your interest in contributing!
Please refer to the subsections immediately below for the project's contribution expectations.

> [!IMPORTANT]
> When contributing to this project, you must agree that you have authored 100% of the content, that you have the necessary rights to the content, and that the content you contribute may be provided under the project's active license terms.

## Security Vulnerabilities

Never submit security vulnerabilities as publicly viewable bug reports.
Submit information about possible security vulnerabilities to <joseph.carpinelli@icloud.com>.
In each submission, please include as much of the following information as possible; note that the only required information is a description of the vulnerability.

- A description of the vulnerability.
- An example of how the vulnerability may be exploited.
- Proposals for patching the vulnerability, if any are known at the time of submission.

## Bug Reports

All bug reports should be submitted as GitHub Issues to the project's [issue tracker](https://github.com/git-ents/git-query/issues/new).
Please include as much detail as possible in the issue description.

## Feature Requests

All feature requests and proposals for project enhancements should be also be submitted as GitHub Issues to the project's [issue tracker](https://github.com/git-ents/git-query/issues/new).
Please include as much detail as possible in the issue description.

## Contributions

Contributions are welcome!
All code and documentation contributions should be submitted as GitHub Pull Requests to the project's [repository](https://github.com/git-ents/git-query).

## Upgrading the Nemo engine adapter

`crates/gix-query-engine-nemo` is pinned to an exact upstream commit, not a version range and never a branch.

Treat a bump as a semantics change, because that is what it is: the crate's job is to compute the same relations as `gix-query-engine-naive`, and anything that moves in Nemo's evaluator, parser, value domains, or tracer can break that without breaking the build.
Nemo is also unpublished and nightly-only, so there is no registry advisory feed and no semver contract to lean on.

The procedure, in a branch:

1. Change the `rev` in `crates/gix-query-engine-nemo/Cargo.toml`.
2. Run the full differential suite.
   Every generated program must produce relations equal to the naive engine's, exactly.
3. Run the golden `explain` traces.
   A trace that changed shape is a finding, not a rebaseline.
4. Diff the fragment inventory in `DEVPLAN.md` §2.11 against upstream's changelog.
   New syntax we could reach, new value domains our reader would have to refuse, or changes to string-literal or variable syntax are all blocking.
5. Re-run the spike if anything in step 4 is unclear: `cd spikes/nemo-spike && cargo +nightly test -- --test-threads=1`.
   It is kept for exactly this.
   `spikes/nemo-spike/spike-report.md` records which commit the current findings describe; update it in the same change.

Only then merge.

## Code of Conduct

Please refer to the in-source [code of conduct](/CONDUCT.md) for all behavioral expectations.
