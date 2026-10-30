//! Rule-module storage at `refs/meta/rules/<name>`, via `git-store`.

use facet::Facet;
use git_store::{GixRefStore, Kind, Layout, RefPath, RefPrefix, RefSegment, RepoStore, Typed};

use crate::error::Error;

/// Where module data lives: `<DATA_PREFIX>/<KIND>/<name>`, i.e.
/// `refs/meta/rules/<name>` -- a rule module is a rule module regardless of
/// what serializes it, so the namespace names the domain, not `git-store`'s
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
    store: RepoStore<'r>,
    kind: RefSegment,
}

impl<'r> RuleStore<'r> {
    /// Open the rule store over `repo`.
    pub fn open(repo: &'r gix::Repository) -> Result<Self, Error> {
        let layout = Layout {
            data: RefPrefix::new(DATA_PREFIX).expect("built-in ref prefix is valid"),
            schema: RefPrefix::new(SCHEMA_PREFIX).expect("built-in ref prefix is valid"),
        };
        let store = RepoStore::open_with_layout(repo, layout);
        let kind = RefSegment::new(KIND).expect("built-in ref segment is valid");
        Ok(Self { store, kind })
    }

    fn kind(&self) -> Kind<'_, Typed<RuleModuleRecord>, GixRefStore<'r>, &'r gix::OdbHandle> {
        self.store.kind(self.kind.clone())
    }

    /// Publish this crate's schema for the `rules` kind, unless the current
    /// one already matches -- `publish` always commits forward, even when
    /// nothing changed, so skipping a no-op call keeps a module's history
    /// free of empty schema churn.
    fn ensure_schema(&self) -> Result<(), Error> {
        let kind = self.kind();
        let doc = facet_git_tree::schema_of::<RuleModuleRecord>()?;
        if kind.schema().get()?.as_ref() != Some(&doc) {
            kind.publish()?;
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
        let name = RefPath::from(RefSegment::new(name)?);
        Ok(self.kind().put(&name, &record)?)
    }

    /// The current source text for module `name`, or `None` if it has never
    /// been published.
    pub fn get(&self, name: &str) -> Result<Option<String>, Error> {
        let name = RefPath::from(RefSegment::new(name)?);
        Ok(self.kind().read(&name)?.value().map(|record| record.source))
    }

    /// Every published module name, sorted.
    pub fn list(&self) -> Result<Vec<String>, Error> {
        Ok(self
            .kind()
            .list()?
            .into_iter()
            .map(|s| s.to_string())
            .collect())
    }

    /// Delete module `name`. Returns whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool, Error> {
        let name = RefPath::from(RefSegment::new(name)?);
        Ok(self.kind().remove(&name)?)
    }
}
