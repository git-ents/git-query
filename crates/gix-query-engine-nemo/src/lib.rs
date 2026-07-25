//! An optional [Nemo](https://github.com/knowsys/nemo) adapter for the engine
//! seam.
//!
//! The only crate in the tree permitted to name a Nemo type, and the only one
//! that needs a nightly compiler. It exists less to be fast than to disagree:
//! the differential job that runs every generated program through this engine
//! and through [`gix_query_engine_naive`] is what keeps either of them honest.
//!
//! Three things this adapter must never do, each learned from the spike
//! recorded in `spikes/nemo-spike/spike-report.md`:
//!
//! - Assert facts as program text. Nemo's parser runs at roughly 109 KB/s;
//!   ten thousand facts cost 1.7 seconds that way and six milliseconds through
//!   an in-memory resource provider, for identical evaluation.
//! - Escape a symbol. Nemo's string literal lexer does no unescaping, so an
//!   emitted `\t` arrives as two characters. A `"` cannot be carried at all and
//!   is refused rather than mangled.
//! - Paper over the gaps in Nemo's derivation trees. Negated literals leave no
//!   node in them, and only one derivation comes back where several exist; both
//!   are filled from our own provenance, above the seam.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
