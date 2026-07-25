//! Rule-module storage at `refs/meta/rules/<name>`, via `gix-store`.

use facet::Facet;
use facet_git_tree::schema_of;
use facet_value::{from_value, to_value};
use gix_store::Store;

use crate::error::Error;

/// Where module data lives: `<DATA_PREFIX>/<KIND>/<name>`, i.e.
/// `refs/meta/rules/<name>` -- a rule module is a rule module regardless of
/// what serializes it, so the namespace names the domain, not `gix-store`'s
/// own mechanism (`refs/store/…`).
const DATA_PREFIX: &str = "refs/meta";
const SCHEMA_PREFIX: &str = "refs/schema";
const KIND: &str = "rules";

/// The stored shape of one rule module: its source text, verbatim.
///
/// The IR is derived from this and never stored -- re-parsing is cheap and
/// the cache key already covers the module's own commit id, so storing a
/// second, redundant encoding would only be a second thing to keep in sync.
#[derive(Facet)]
struct RuleModuleRecord {
    source: String,
}

/// Storage for rule modules, one Git ref per module name.
pub struct RuleStore<'r> {
    store: Store<'r>,
}

impl<'r> RuleStore<'r> {
    /// Open the rule store over `repo`.
    pub fn open(repo: &'r gix::Repository) -> Result<Self, Error> {
        let store = Store::open_with_prefixes(repo, DATA_PREFIX, SCHEMA_PREFIX)?;
        Ok(Self { store })
    }

    /// Publish this crate's schema for the `rules` kind, unless the current
    /// one already matches -- `put_schema` always commits forward, even when
    /// nothing changed, so skipping a no-op call keeps a module's history
    /// free of empty schema churn.
    fn ensure_schema(&self) -> Result<(), Error> {
        let doc = schema_of::<RuleModuleRecord>()?;
        if self.store.schema(KIND)?.as_ref() != Some(&doc) {
            self.store.put_schema(KIND, &doc)?;
        }
        Ok(())
    }

    /// Store `source` as module `name`'s new tip, committing forward over
    /// whatever it published before.
    pub fn put(&self, name: &str, source: &str) -> Result<gix::ObjectId, Error> {
        self.ensure_schema()?;
        let record = RuleModuleRecord {
            source: source.to_owned(),
        };
        let value = to_value(&record)?;
        Ok(self.store.store(KIND, name, &value, None)?)
    }

    /// The current source text for module `name`, or `None` if it has never
    /// been published.
    pub fn get(&self, name: &str) -> Result<Option<String>, Error> {
        let Some(value) = self.store.retrieve(KIND, name)? else {
            return Ok(None);
        };
        let record: RuleModuleRecord = from_value(value).map_err(Box::new)?;
        Ok(Some(record.source))
    }

    /// Every published module name, sorted.
    pub fn list(&self) -> Result<Vec<String>, Error> {
        Ok(self.store.list(KIND)?)
    }

    /// Delete module `name`. Returns whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool, Error> {
        Ok(self.store.delete(KIND, name)?)
    }
}
