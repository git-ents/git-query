//! The Nemo execution wrapper: current-thread runtime, in-memory fact
//! import, and typed read-back.
//!
//! Nemo's API is async and `!Send` (`nemo::error::Error` cannot cross a
//! thread boundary), and `execute()` unconditionally enters a process-global
//! timing mutex that a second concurrent evaluation poisons. Both mean the
//! same thing operationally: one current-thread runtime, one call at a time,
//! nothing here ever spawned onto a worker thread.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::rc::Rc;

use nemo::datavalues::{AnyDataValue, DataValue, ValueDomain};
use nemo::execution::ExecutionEngine;
use nemo::execution::execution_parameters::ExecutionParameters;
use nemo::io::ImportManager;
use nemo::io::resource_providers::{ResourceProvider, ResourceProviders};
use nemo::rule_file::RuleFile;
use nemo::rule_model::components::tag::Tag;
use nemo_physical::error::ReadingError;
use nemo_physical::resource::Resource;

use gix_query_ir::{PredicateKey, Value};

use crate::error::EvalError;
use crate::lower::{NemoProgram, NemoTables, render_predicate_name};

/// The URI scheme our in-memory resource provider claims. Never touches
/// disk; the whole point of the memory-import route is that it doesn't have
/// to.
const SCHEME: &str = "gix-query-eval-memory/";

fn resource_name(key: &PredicateKey) -> String {
    format!("{SCHEME}{}", key.name)
}

/// Run `program` once against `tables`, returning the finished engine.
///
/// Every round of the demand loop calls this from cold: Nemo has no
/// incremental entry point, so a *k*-round query pays *k* times. That cost is
/// [`crate::demand`]'s to carry, not this function's to hide.
///
/// # Errors
/// [`EvalError::Nemo`] if Nemo rejects the program or a table's content, with
/// the underlying `nemo::error::Error` stringified at this boundary because
/// it is not `Send`.
pub(crate) fn run(
    program: &NemoProgram,
    tables: &NemoTables,
) -> Result<nemo::api::Engine, EvalError> {
    let mut text = String::new();
    let mut csv_tables = BTreeMap::new();
    for (key, rows) in tables.iter() {
        if rows.is_empty() {
            continue;
        }
        let formats = column_formats(rows);
        text.push_str(&format!(
            "@import {} :- csv{{resource = \"{}\", format = ({formats})}} .\n",
            render_predicate_name(&key.name),
            resource_name(key),
        ));
        csv_tables.insert(resource_name(key), csv_of(rows));
    }
    text.push('\n');
    text.push_str(&program.to_string());

    block_on(async {
        let providers =
            ResourceProviders::from(vec![
                Box::new(MemoryResourceProvider::new(csv_tables)) as Box<dyn ResourceProvider>
            ]);
        let mut parameters = ExecutionParameters::default();
        parameters.set_import_manager(ImportManager::new(providers));
        let mut engine = ExecutionEngine::from_file(
            RuleFile::new(text, "gix-query-eval".to_owned()),
            parameters,
        )
        .await
        .map_err(|e| EvalError::Nemo(format!("{e}")))?
        .into_object();
        engine
            .execute()
            .await
            .map_err(|e| EvalError::Nemo(format!("{e}")))?;
        Ok(engine)
    })
}

/// Read every tuple of `key` back, checked into our two value domains.
///
/// # Errors
/// [`EvalError::ValueOutOfFragment`] the moment any column holds a value
/// outside `Value::Int`/`Value::Sym` -- an existential head minting a null,
/// a bare identifier landing in `Iri`, a language-tagged string. None of
/// those are coerced; the read simply refuses.
pub(crate) fn relation(
    engine: &mut nemo::api::Engine,
    key: &PredicateKey,
) -> Result<Vec<Vec<Value>>, EvalError> {
    let tag = Tag::new(key.name.to_string());
    let rows =
        block_on(engine.predicate_rows(&tag)).map_err(|e| EvalError::Nemo(format!("{e}")))?;
    let Some(rows) = rows else {
        return Ok(Vec::new());
    };
    rows.map(|row| {
        row.iter()
            .map(value_from_nemo)
            .collect::<Result<Vec<_>, _>>()
    })
    .collect()
}

/// Convert one Nemo value into ours, refusing everything outside
/// `PlainString` and the integer domains.
pub(crate) fn value_from_nemo(v: &AnyDataValue) -> Result<Value, EvalError> {
    match v.value_domain() {
        ValueDomain::PlainString => Ok(Value::sym(v.to_plain_string_unchecked())),
        ValueDomain::Int
        | ValueDomain::Long
        | ValueDomain::UnsignedInt
        | ValueDomain::UnsignedLong
        | ValueDomain::NonNegativeInt
        | ValueDomain::NonNegativeLong => {
            v.to_i64()
                .map(Value::Int)
                .ok_or_else(|| EvalError::ValueOutOfFragment {
                    domain: "an integer domain, but out of i64 range".to_owned(),
                })
        }
        other => Err(EvalError::ValueOutOfFragment {
            domain: format!("{other:?}"),
        }),
    }
}

/// Per-column CSV type declarations, taken from the first row: CSV carries no
/// types of its own, and a column declared `string` would read the integer
/// `42` back as the symbol `"42"`, which is exactly the join our fragment
/// exists to prevent.
fn column_formats(rows: &[Vec<Value>]) -> String {
    rows.first()
        .map(|row| {
            row.iter()
                .map(|v| match v {
                    Value::Int(_) => "int",
                    Value::Sym(_) => "string",
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Serialize rows as CSV. This is CSV quoting, not Nemo program-text quoting
/// -- doubled internal `"` is exactly what a CSV reader expects, unlike
/// Nemo's own string lexer, which is why this is not routed through
/// [`crate::Symbol`].
fn csv_of(rows: &[Vec<Value>]) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match v {
                Value::Int(n) => out.push_str(&n.to_string()),
                Value::Sym(s) => {
                    out.push('"');
                    out.push_str(&s.replace('"', "\"\""));
                    out.push('"');
                }
            }
        }
        out.push('\n');
    }
    out
}

/// Serves `@import` resources from memory, so a round never touches disk.
#[derive(Debug)]
struct MemoryResourceProvider {
    tables: Rc<RefCell<BTreeMap<String, String>>>,
}

impl MemoryResourceProvider {
    fn new(tables: BTreeMap<String, String>) -> Self {
        Self {
            tables: Rc::new(RefCell::new(tables)),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ResourceProvider for MemoryResourceProvider {
    async fn open_resource(
        &self,
        resource: &Resource,
        _media_type: &str,
    ) -> Result<Option<Box<dyn Read>>, ReadingError> {
        let key = resource.to_string();
        if !key.starts_with(SCHEME) {
            return Ok(None);
        }
        let bytes = self
            .tables
            .borrow()
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_bytes();
        Ok(Some(Box::new(Cursor::new(bytes))))
    }
}

/// Run `program` against `tables` and trace the derivation of one ground
/// fact, `predicate(tuple)`.
///
/// Runs the whole program fresh, exactly like [`run`]: Nemo's trace only
/// covers a completed evaluation, so there is no cheaper route than
/// re-executing against the same, already-converged tables a caller's demand
/// loop produced.
///
/// # Errors
/// [`EvalError::Nemo`] if Nemo rejects the program, the tuple contains a
/// value that cannot be rendered (a `"` in a symbol), or tracing itself
/// fails.
pub(crate) fn trace_fact(
    program: &NemoProgram,
    tables: &NemoTables,
    predicate: &PredicateKey,
    tuple: &[Value],
) -> Result<Option<nemo::execution::tracing::trace::ExecutionTraceTree>, EvalError> {
    let mut engine = run(program, tables)?;
    let literal = tuple
        .iter()
        .map(crate::lower::value_literal)
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let text = format!("{}({literal})", predicate.name);
    let fact = nemo::rule_model::components::fact::Fact::parse(&text)
        .map_err(|e| EvalError::Nemo(format!("{e:?}")))?;
    let (trace, handles) =
        block_on(engine.trace(vec![fact])).map_err(|e| EvalError::Nemo(format!("{e}")))?;
    Ok(trace.tree(handles[0]))
}

/// Run one future to completion on a fresh current-thread runtime.
///
/// A fresh runtime per call rather than one held across the demand loop: the
/// process-global timing mutex already serializes every round, so there is no
/// concurrency to lose by tearing down between them, and it keeps this
/// function free of any state a caller could hold across an `await` Nemo's
/// own types cannot survive anyway.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime always builds")
        .block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quoting_doubles_an_internal_quote() {
        let rows = vec![vec![Value::sym("has \"quote\" inside")]];
        assert_eq!(csv_of(&rows), "\"has \"\"quote\"\" inside\"\n");
    }

    #[test]
    fn column_formats_read_the_first_row() {
        let rows = vec![vec![Value::Int(1), Value::sym("a")]];
        assert_eq!(column_formats(&rows), "int, string");
    }

    // Nemo's `execute()` enters a process-global timing mutex
    // (`nemo_physical::meta::timing`); a second concurrent evaluation in the
    // same process panics and poisons it for every later test. Run this
    // module with `--test-threads=1`, as the crate-level test modules note.
    #[test]
    fn a_lowered_program_runs_through_nemo_and_reads_back() {
        use gix_query_ir::{Atom, PredicateKey, Rule, Term};

        let mut program = gix_query_ir::Program::new();
        program.push_rule(
            Rule::new(
                Atom::new("out", vec![Term::var("X")]),
                vec![gix_query_ir::Literal::Positive(Atom::new(
                    "member",
                    vec![Term::var("X")],
                ))],
            ),
            "test",
        );
        let rewritten = crate::rewrite::RewrittenProgram {
            rules: program,
            demand: Vec::new(),
            eager: std::iter::once(PredicateKey::new("member", 1)).collect(),
            answer: PredicateKey::new("out", 1),
            answer_is_nullary: false,
        };
        let nemo_program = NemoProgram::emit(&rewritten).expect("emits");
        nemo_program.audit().expect("audit passes");

        let mut tables = NemoTables::new();
        tables.insert(
            PredicateKey::new("member", 1),
            vec![vec![Value::sym("alice")], vec![Value::sym("bob")]],
        );

        let mut engine = run(&nemo_program, &tables).expect("nemo runs");
        let mut got = relation(&mut engine, &PredicateKey::new("out", 1)).expect("read back");
        got.sort();
        assert_eq!(
            got,
            vec![vec![Value::sym("alice")], vec![Value::sym("bob")]]
        );
    }
}
