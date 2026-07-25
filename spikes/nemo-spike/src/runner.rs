//! A minimal stand-in for the engine seam.
//!
//! This is deliberately the *shape* the real `gix-query-engine` trait would
//! have -- load a program, assert EDB tuples, run to fixpoint, read named
//! relations back -- so that what the gates measure is the shape we would
//! actually build against, not a convenient one.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::rc::Rc;
use std::time::{Duration, Instant};

use nemo::api::load_string;
use nemo::datavalues::DataValue;
use nemo::error::Error as NemoError;
use nemo::execution::ExecutionEngine;
use nemo::execution::execution_parameters::ExecutionParameters;
use nemo::io::ImportManager;
use nemo::io::resource_providers::{ResourceProvider, ResourceProviders};
use nemo::rule_file::RuleFile;
use nemo::rule_model::components::tag::Tag;
use nemo_physical::error::ReadingError;
use nemo_physical::resource::Resource;

use crate::value::{Tuple, Value};

/// How EDB tuples get from Rust into the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactRoute {
    /// Serialize each tuple as a Nemo fact and append it to the program text.
    Inline,
    /// Serialize the relation as CSV and serve it from memory through a custom
    /// [`ResourceProvider`], referenced by an `@import` directive.
    MemoryImport,
}

/// A program plus the EDB it runs against.
#[derive(Debug, Clone)]
pub struct Runner {
    rules: String,
    edb: BTreeMap<String, Vec<Tuple>>,
    route: FactRoute,
}

/// What one run cost, so the gates can report numbers rather than adjectives.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Time spent turning the program text into an engine.
    pub load: Duration,
    /// Time spent running to fixpoint.
    pub execute: Duration,
}

impl Runner {
    /// A runner over `rules`, with facts asserted inline.
    pub fn new(rules: impl Into<String>) -> Self {
        Self {
            rules: rules.into(),
            edb: BTreeMap::new(),
            route: FactRoute::Inline,
        }
    }

    /// Pick how EDB tuples reach the engine.
    pub fn route(mut self, route: FactRoute) -> Self {
        self.route = route;
        self
    }

    /// Assert tuples into a named EDB predicate.
    pub fn facts(mut self, predicate: &str, tuples: impl IntoIterator<Item = Tuple>) -> Self {
        self.edb
            .entry(predicate.to_owned())
            .or_default()
            .extend(tuples);
        self
    }

    /// The full program text handed to Nemo, including whatever the fact route
    /// contributes. Exposed because "what did we actually emit" is the
    /// question gate A6 turns on.
    pub fn program_text(&self) -> String {
        let mut out = String::new();
        match self.route {
            FactRoute::Inline => {
                for (predicate, tuples) in &self.edb {
                    for tuple in tuples {
                        out.push_str(predicate);
                        out.push('(');
                        for (i, value) in tuple.0.iter().enumerate() {
                            if i > 0 {
                                out.push_str(", ");
                            }
                            out.push_str(&value.to_nemo());
                        }
                        out.push_str(") .\n");
                    }
                }
            }
            FactRoute::MemoryImport => {
                for (predicate, tuples) in &self.edb {
                    // The column types have to be declared, and they have to
                    // match: CSV carries no types of its own, so a column
                    // declared `string` yields the symbol "42" where the
                    // caller asserted the integer 42.
                    let formats = tuples
                        .first()
                        .map(|t| {
                            t.0.iter()
                                .map(|v| match v {
                                    Value::Int(_) => "int",
                                    Value::Sym(_) => "string",
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    out.push_str(&format!(
                        "@import {predicate} :- csv{{resource = \"{MEM_SCHEME}{predicate}\", format = ({formats})}} .\n"
                    ));
                }
            }
        }
        out.push('\n');
        out.push_str(&self.rules);
        out
    }

    /// Load and run to fixpoint.
    pub async fn run(&self) -> Result<Session, NemoError> {
        let started = Instant::now();
        let mut engine = match self.route {
            FactRoute::Inline => load_string(self.program_text()).await?,
            FactRoute::MemoryImport => {
                let mut tables = BTreeMap::new();
                for (predicate, tuples) in &self.edb {
                    tables.insert(format!("{MEM_SCHEME}{predicate}"), csv_of(tuples));
                }
                let providers =
                    ResourceProviders::from(vec![Box::new(MemoryResourceProvider::new(tables))]);
                let mut parameters = ExecutionParameters::default();
                parameters.set_import_manager(ImportManager::new(providers));
                ExecutionEngine::from_file(
                    RuleFile::new(self.program_text(), String::from("spike")),
                    parameters,
                )
                .await?
                .into_object()
            }
        };
        let load = started.elapsed();

        let started = Instant::now();
        engine.execute().await?;
        let execute = started.elapsed();

        Ok(Session {
            engine,
            timing: Timing { load, execute },
        })
    }
}

/// A finished run, from which relations and traces can be read.
#[derive(Debug)]
pub struct Session {
    /// The engine, exposed because gates A4 and A5 need APIs the seam would
    /// wrap rather than hide.
    pub engine: nemo::api::Engine,
    /// What the run cost.
    pub timing: Timing,
}

impl Session {
    /// Every tuple of a named predicate, sorted and deduplicated.
    ///
    /// Sorting is ours, not Nemo's: nothing in the API promises an order, and
    /// a gate that asserts set equality should not accidentally be asserting
    /// an ordering too.
    pub async fn relation(&mut self, predicate: &str) -> Vec<Tuple> {
        let tag = Tag::new(predicate.to_owned());
        let rows = self
            .engine
            .predicate_rows(&tag)
            .await
            .expect("read predicate rows");
        let Some(rows) = rows else { return Vec::new() };
        let mut out: Vec<Tuple> = rows
            .map(|row| {
                Tuple(
                    row.iter()
                        .map(|v| {
                            Value::from_nemo(v).unwrap_or_else(|domain| {
                                panic!("value escaped our fragment into {domain:?}: {v}")
                            })
                        })
                        .collect(),
                )
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every tuple of a predicate, with values left in Nemo's own domains --
    /// used by gate A6, which is precisely about values our reader would
    /// reject.
    pub async fn raw_relation(&mut self, predicate: &str) -> Vec<Vec<(String, String)>> {
        let tag = Tag::new(predicate.to_owned());
        let rows = self
            .engine
            .predicate_rows(&tag)
            .await
            .expect("read predicate rows");
        let Some(rows) = rows else { return Vec::new() };
        let mut out: Vec<Vec<(String, String)>> = rows
            .map(|row| {
                row.iter()
                    .map(|v| (format!("{:?}", v.value_domain()), v.lexical_value()))
                    .collect()
            })
            .collect();
        out.sort();
        out
    }
}

/// The sentinel path prefix the in-memory provider claims.
const MEM_SCHEME: &str = "spike-memory/";

/// Serialize tuples as CSV for the memory import route.
fn csv_of(tuples: &[Tuple]) -> String {
    let mut out = String::new();
    for tuple in tuples {
        for (i, value) in tuple.0.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match value {
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

/// Serves `@import` resources from memory, so a spike run touches no disk.
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
        if !key.starts_with(MEM_SCHEME) {
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
