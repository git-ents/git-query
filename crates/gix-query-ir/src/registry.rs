use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use crate::{ArgSet, CompareOp, Error, Mode, ModeVector, PredicateKey};

/// A column type. Values are identifiers, never content.
///
/// `Rev` is distinct from `Oid` even though both are object ids at runtime,
/// because the distinction is what makes required-bound rev arguments
/// expressible: an EDB predicate whose extension depends on an unbound
/// rev-like argument is both unaffordable and uncacheable, and the type is how
/// the registry says which arguments those are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueType {
    /// A commit id, or a revision that resolves to one.
    Rev,
    /// Any other object id: a blob, an anchor, a claim.
    Oid,
    /// A path within a tree.
    Path,
    /// A member identity.
    Member,
    /// The position axis of a projection.
    Position,
    /// The content axis of a projection.
    Content,
    /// An integer. The only type comparisons order.
    Int,
    /// A bare keyword: a claim kind, a verdict.
    Symbol,
    /// `cand/5`'s and `bind/5`'s third column: a path and byte span, encoded
    /// as `"<path>:<start>-<end>"` -- one atomic term, since the value
    /// domain has no tuple type to hold `(path, start, end)` as three
    /// fields.
    Location,
    /// `cand/5`'s and `bind/5`'s fourth column: which oracle contributed a
    /// candidate -- `pin`, or one of `gix_anchor::Oracle`'s three named
    /// oracles.
    Oracle,
}

impl ValueType {
    /// The type's name as `git query predicates` prints it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rev => "rev",
            Self::Oid => "oid",
            Self::Path => "path",
            Self::Member => "member",
            Self::Position => "position",
            Self::Content => "content",
            Self::Int => "int",
            Self::Symbol => "symbol",
            Self::Location => "location",
            Self::Oracle => "oracle",
        }
    }

    /// The closed set of identifiers this type admits, for the types that have
    /// one.
    ///
    /// The projection axes are enumerations, not open identifier spaces, and
    /// saying so in code rather than in a comment is what lets type inference
    /// settle `carries(same, intact).` -- a fact whose predicate is defined by
    /// facts alone, so its argument types come from unification with its call
    /// site plus this domain, and from nowhere else.
    #[must_use]
    pub const fn domain(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Position => Some(&["same", "moved", "lost"]),
            // `none` occurs only alongside `Position::lost`; that pairing is
            // a host invariant, not a type-system one.
            Self::Content => Some(&["intact", "edited", "none"]),
            // Pin dominates the lattice at confidence 1.0 by construction
            // (ARCHITECTURE.md), but is not itself a `gix_anchor::Oracle`
            // variant -- it is query's own leg, added to anchor's three.
            Self::Oracle => Some(&["pin", "op_log", "diff_trace", "fingerprint"]),
            _ => None,
        }
    }

    /// Whether `s` is admissible at this type. Types with no closed domain
    /// admit any identifier.
    #[must_use]
    pub fn admits(self, s: &str) -> bool {
        self.domain().is_none_or(|d| d.contains(&s))
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One argument's type in a predicate signature.
///
/// Host predicates are fully declared. A predicate a rule module defines is
/// not: `carries/2` is given by facts alone, and its argument types are
/// whatever its call site demands. Without a slot for "not yet known" the type
/// pass has only two options on the reference vocabulary -- reject it, or skip
/// type checking for every derived predicate -- and neither is recoverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeSlot {
    /// Declared.
    Known(ValueType),
    /// To be inferred by unification with call sites.
    Infer,
}

impl TypeSlot {
    /// The declared type, if there is one.
    #[must_use]
    pub const fn known(self) -> Option<ValueType> {
        match self {
            Self::Known(t) => Some(t),
            Self::Infer => None,
        }
    }
}

impl fmt::Display for TypeSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(t) => write!(f, "{t}"),
            Self::Infer => f.write_str("?"),
        }
    }
}

/// The declared type of one builtin argument.
///
/// Builtins need two forms predicates do not, because `=` and `!=` accept any
/// two scalars of the *same* type, and spelling that as six monomorphic
/// entries per type would put the type list in two places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    /// Exactly this type.
    Exact(ValueType),
    /// Any scalar type.
    AnyScalar,
    /// Whatever type argument `n` resolved to.
    SameAsArg(usize),
}

/// Whether a predicate is exported from its module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Visibility {
    /// Visible only within its own module. Cross-module references do not
    /// resolve against it.
    #[default]
    Private,
    /// `pub` -- part of the module's declared surface.
    Public,
}

/// Where an EDB predicate's facts come from, and therefore what the cache key
/// must cover.
///
/// This field is what turns the footprint pass from a list of predicate names
/// into a list of ref globs. Without it the cache key can only be keyed on
/// `rev`, and `rev` does not determine the host EDB: members, claims, and
/// verdicts are backed by refs that move independently of any code rev, so two
/// runs at the same rev with a member revoked in between hit the same entry and
/// return the same answer. That surfaces as a stale gate verdict.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Backing {
    /// Facts are read from refs matching this glob. Their resolved
    /// `(refname, oid)` pairs go into the footprint digest.
    RefGlob(String),
    /// Facts are derived from an object id the call already supplies, so
    /// nothing needs to enter the key -- the oid is already in it.
    ///
    /// This is a claim about a *bound* call. `parent(Child, Parent)` is
    /// content-addressed given a bound child and is the entire commit graph
    /// given a free one, and only the first contributes nothing to the key. A
    /// content-addressed predicate therefore may not offer an all-free mode
    /// vector, and [`Registry::validate`] enforces exactly that.
    ContentAddressed,
    /// Facts came from `--edb name=file` for this run. They enter the key
    /// through the run's ad-hoc EDB digest rather than through any ref.
    AdHoc,
}

/// How a predicate is implemented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredicateClass {
    /// Host-supplied and repo-backed.
    Edb {
        /// The arguments a caller must bind. Empty means the provider's
        /// whole extension can be read in one go (`DEVPLAN.md` §0) --
        /// binding more than this is always legal on top, so this one
        /// [`ArgSet`] is enough to describe every call this predicate
        /// accepts, not just the cheapest one.
        required_bound: ArgSet,
        /// What the provider reads. A predicate may read more than one
        /// namespace.
        backing: Vec<Backing>,
    },
    /// Defined by rules. Its required-bound set is inferred, not declared, and
    /// is the *union* over its rules -- conservative, so a caller that
    /// satisfies it satisfies every rule.
    Idb {
        /// Filled by mode inference. `None` until it has run, so a consumer
        /// can tell an un-inferred registry from one that inferred nothing.
        required_bound: Option<ArgSet>,
    },
}

impl PredicateClass {
    /// Whether this is a host predicate.
    #[must_use]
    pub fn is_edb(&self) -> bool {
        matches!(self, Self::Edb { .. })
    }
}

/// One predicate's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateEntry {
    /// Name and arity.
    pub key: PredicateKey,
    /// Argument types, in signature order. This order is the tsv contract.
    pub types: Vec<TypeSlot>,
    /// Whether the predicate is exported.
    pub visibility: Visibility,
    /// EDB or IDB, with the mode information each carries.
    pub class: PredicateClass,
    /// One line, printed by `git query predicates`. Owned, because a
    /// repo-defined predicate's documentation arrives as a runtime string.
    pub doc: Cow<'static, str>,
}

impl PredicateEntry {
    /// A derived predicate with all types left to inference.
    #[must_use]
    pub fn idb(key: PredicateKey, visibility: Visibility) -> Self {
        let arity = key.arity;
        Self {
            key,
            types: vec![TypeSlot::Infer; arity],
            visibility,
            class: PredicateClass::Idb {
                required_bound: None,
            },
            doc: Cow::Borrowed(""),
        }
    }

    /// A host-supplied, repo-backed predicate: the public counterpart of the
    /// `edb(...)` helper [`host_predicates`] itself is built from.
    ///
    /// This is the seam a downstream consumer (`git-forge`, per
    /// ARCHITECTURE.md's "extension-contributed predicates") uses to declare
    /// its own base facts -- `cst_node/4`, `reviewed/1`, and the like -- with
    /// the same knobs `host_predicates()` uses for `commit/1` and `verdict/2`:
    /// name and arity (via `types`'s length), argument types, the
    /// required-bound [`ArgSet`] that decides eager-vs-demand affordability
    /// (`DEVPLAN.md` §0), the [`Backing`] sources that feed the cache key's
    /// footprint digest, and one doc line for `git query predicates`.
    /// Visibility is always [`Visibility::Public`], matching every host
    /// predicate: an EDB relation is base data, never module-private.
    /// [`Registry::insert_predicate`] rejects any attempt to redefine a name
    /// already registered, host or otherwise, so a consumer cannot shadow
    /// `commit/1` by accident.
    #[must_use]
    pub fn edb(
        name: &'static str,
        types: &[ValueType],
        required_bound: ArgSet,
        backing: Vec<Backing>,
        doc: &'static str,
    ) -> Self {
        edb(name, types, required_bound, backing, doc)
    }

    /// The ref globs this predicate reads, empty for a content-addressed,
    /// ad-hoc, or derived predicate.
    pub fn ref_globs(&self) -> impl Iterator<Item = &str> {
        let backing: &[Backing] = match &self.class {
            PredicateClass::Edb { backing, .. } => backing,
            PredicateClass::Idb { .. } => &[],
        };
        backing.iter().filter_map(|b| match b {
            Backing::RefGlob(g) => Some(g.as_str()),
            Backing::ContentAddressed | Backing::AdHoc => None,
        })
    }

    /// Whether any of this predicate's facts come from an ad-hoc source
    /// rather than from refs or from the call's own bound oid.
    ///
    /// [`Backing::AdHoc`] is the one backing whose content nothing in a
    /// repository's state accounts for, so a cache key computed from refs
    /// alone cannot speak for it. A caller keying on ref state must treat a
    /// predicate answering `true` here as uncacheable.
    #[must_use]
    pub fn is_ad_hoc(&self) -> bool {
        match &self.class {
            PredicateClass::Edb { backing, .. } => {
                backing.iter().any(|b| matches!(b, Backing::AdHoc))
            }
            PredicateClass::Idb { .. } => false,
        }
    }

    /// The arguments every caller must bind.
    ///
    /// For a derived predicate this is `None` until mode inference has run.
    #[must_use]
    pub fn always_required_bound(&self) -> Option<ArgSet> {
        match &self.class {
            PredicateClass::Edb { required_bound, .. } => Some(*required_bound),
            PredicateClass::Idb { required_bound } => *required_bound,
        }
    }

    /// Whether this predicate's whole extension can be read in one go: an EDB
    /// predicate with no required-bound arguments.
    ///
    /// `DEVPLAN.md` §0's eager path may only preload a predicate whose
    /// registry entry says so -- a required-bound EDB predicate such as
    /// `commit/1` or `parent/2` cannot be called all-free, precisely because
    /// enumerating it whole is the unaffordable materialization mode
    /// discipline exists to prevent. An IDB predicate is never enumerable by
    /// this definition; it is derived, not read.
    #[must_use]
    pub fn is_enumerable(&self) -> bool {
        match &self.class {
            PredicateClass::Edb { required_bound, .. } => required_bound.is_empty(),
            PredicateClass::Idb { .. } => false,
        }
    }
}

/// The six properties every builtin declares and the validator assumes.
///
/// `finite` is the one nobody writes down, and it is what makes a
/// multi-candidate `bind` safe: the count is bounded per input. A builtin that
/// is `mints` and not `finite` breaks termination even outside a recursive SCC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinProps {
    /// Defined on every well-moded input.
    pub total: bool,
    /// Same input, same output, forever.
    pub deterministic: bool,
    /// No observable effect.
    pub pure: bool,
    /// Finitely many output tuples per bound input tuple.
    pub finite: bool,
    /// Produces values not in the active domain.
    ///
    /// Declared, never inferred. It is a claim about the *implementation*, not
    /// the signature: an all-free output mode might return values the builtin
    /// received (projection) or values it constructed (minting), and the
    /// signature cannot tell you which. Inferring "has an output mode,
    /// therefore mints" would reject harmless projections; inferring the other
    /// direction would miss the case minting confinement exists for.
    pub mints: bool,
    /// Implementation version, bumped on any behavior change.
    ///
    /// This exists because the cache key needs a builtin registry digest and
    /// there is nowhere else honest to put it. A builtin whose answer changes
    /// when its backing heuristics improve is not a different builtin -- same
    /// name, same signature, same modes -- so the version has to be carried as
    /// data beside those rather than derived from them.
    pub version: u32,
}

impl BuiltinProps {
    /// The properties of a pure filter: total, deterministic, pure, finite,
    /// and minting nothing.
    #[must_use]
    pub const fn filter() -> Self {
        Self {
            total: true,
            deterministic: true,
            pure: true,
            finite: true,
            mints: false,
            version: 1,
        }
    }
}

/// One builtin's signature and properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinEntry {
    /// Name and arity.
    pub key: PredicateKey,
    /// Argument types, in signature order.
    pub types: Vec<ArgType>,
    /// The single mode vector this builtin supports. Builtins are
    /// non-enumerable, so unlike EDB predicates they get one vector, not a set.
    pub modes: ModeVector,
    /// The declared property table.
    pub props: BuiltinProps,
    /// Whether this builtin may appear in a rule reachable from a gate.
    ///
    /// A builtin whose answer depends on ambient state that is neither
    /// content-addressed nor a ref cannot appear in a cache key, and an
    /// admission decision should not turn on such a builtin either.
    pub gate_safe: bool,
    /// One line, printed by `git query predicates`.
    pub doc: &'static str,
}

/// The signature registry: every host predicate, every builtin, and every
/// derived predicate the assembled program defines.
///
/// This is the contract every other crate codes against, and it is the reason
/// the validator has no git dependency: backing sources are registry *data*,
/// so the footprint pass maps predicate names to ref globs without ever
/// reaching into a repository.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    predicates: BTreeMap<PredicateKey, PredicateEntry>,
    builtins: BTreeMap<PredicateKey, BuiltinEntry>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The host registry: the repo-backed EDB predicates and every builtin,
    /// plus one pre-declared `pub` IDB entry for `bind/5`.
    ///
    /// `bind/5` is "the only user-facing resolution predicate"
    /// (ARCHITECTURE.md) and must resolve bare, unqualified, exactly like
    /// `commit/1` or `anchor/1` -- never `core.bind`. It is defined by a
    /// rule (`gix-query-rules::assemble`'s always-present core module, over
    /// `cand/5`), not a builtin, so it cannot enter through
    /// [`builtins()`] the way `cand` does; pre-declaring it here instead is
    /// exactly the pattern [`crate::check`](../../gix_query_check/fn.check.html)'s
    /// own doc comment already names as harmless: "predicates \[a
    /// pre-extended registry\] already carries are treated as pre-declared
    /// host data, exactly like `commit/1`." Assembly's `qualify` step then
    /// sees `bind` already resolvable and leaves it unqualified, while
    /// everything else the core module defines (its own `dominated` helper)
    /// still gets namespaced normally.
    ///
    /// Rule modules add their own derived predicates on top of this; nothing
    /// here is redefinable.
    #[must_use]
    pub fn host() -> Self {
        let mut r = Self::new();
        for e in host_predicates() {
            r.predicates.insert(e.key.clone(), e);
        }
        for e in builtins() {
            r.builtins.insert(e.key.clone(), e);
        }
        let bind = PredicateEntry::idb(PredicateKey::new("bind", 5), Visibility::Public);
        r.predicates.insert(bind.key.clone(), bind);
        debug_assert!(r.validate().is_ok(), "the host registry is self-consistent");
        r
    }

    /// Add a predicate the program defines.
    ///
    /// Refuses to shadow a host EDB predicate or a builtin. This is the
    /// mechanism behind "base predicates are not redefinable": without it a
    /// rule module defining `commit/1` silently replaces the host entry, mode
    /// set and all, before predicate resolution ever runs.
    pub fn insert_predicate(&mut self, entry: PredicateEntry) -> Result<(), Error> {
        if self.is_base(&entry.key) || self.builtins.contains_key(&entry.key) {
            return Err(Error::NotRedefinable { key: entry.key });
        }
        if entry.key.arity > ArgSet::MAX_ARITY {
            return Err(Error::ArityLimit {
                key: entry.key,
                max: ArgSet::MAX_ARITY,
            });
        }
        if entry.types.len() != entry.key.arity {
            return Err(Error::Arity {
                found: entry.types.len(),
                key: entry.key,
                span: crate::Span::synthetic(),
            });
        }
        self.predicates.insert(entry.key.clone(), entry);
        Ok(())
    }

    /// Add or replace a builtin entry. Only the host registry does this.
    pub fn insert_builtin(&mut self, entry: BuiltinEntry) {
        self.builtins.insert(entry.key.clone(), entry);
    }

    /// Check the registry's own invariants.
    ///
    /// The one that matters: a content-addressed predicate may not be
    /// enumerable. `ContentAddressed` means "the oid is already in the goal, so
    /// nothing enters the cache key", and that claim is false of an all-free
    /// call -- an all-free read of `parent/2` is the whole commit graph, whose
    /// extension moves whenever any ref moves, with nothing in the footprint to
    /// notice.
    pub fn validate(&self) -> Result<(), Error> {
        for e in self.predicates.values() {
            let PredicateClass::Edb {
                required_bound,
                backing,
            } = &e.class
            else {
                continue;
            };
            if !backing.contains(&Backing::ContentAddressed) {
                continue;
            }
            if required_bound.is_empty() {
                return Err(Error::EnumerableContentAddressed { key: e.key.clone() });
            }
        }
        Ok(())
    }

    /// The predicate registered under `key`.
    #[must_use]
    pub fn predicate(&self, key: &PredicateKey) -> Option<&PredicateEntry> {
        self.predicates.get(key)
    }

    /// The predicate registered under `key`, mutably. Mode and type inference
    /// use this to fill in a derived predicate's signature.
    pub fn predicate_mut(&mut self, key: &PredicateKey) -> Option<&mut PredicateEntry> {
        self.predicates.get_mut(key)
    }

    /// The builtin registered under `key`.
    #[must_use]
    pub fn builtin(&self, key: &PredicateKey) -> Option<&BuiltinEntry> {
        self.builtins.get(key)
    }

    /// Whether `key` names a builtin. Predicate resolution uses this to
    /// reclassify a parsed positive literal as a builtin call.
    #[must_use]
    pub fn is_builtin(&self, key: &PredicateKey) -> bool {
        self.builtins.contains_key(key)
    }

    /// Whether `key` names a host EDB predicate, which is therefore not
    /// redefinable by a rule module.
    #[must_use]
    pub fn is_base(&self, key: &PredicateKey) -> bool {
        self.predicates.get(key).is_some_and(|e| e.class.is_edb())
    }

    /// Every predicate, in key order.
    pub fn predicates(&self) -> impl Iterator<Item = &PredicateEntry> {
        self.predicates.values()
    }

    /// Every builtin, in key order.
    pub fn builtins(&self) -> impl Iterator<Item = &BuiltinEntry> {
        self.builtins.values()
    }

    /// A stable digest input covering every EDB predicate's name, signature,
    /// required-bound set, and backing.
    ///
    /// The companion to [`Self::builtin_digest_input`] for the half of the
    /// registry a consumer can extend: two registries that agree on every
    /// builtin may still declare entirely different base facts under the same
    /// names -- `Registry::host()` versus one a consumer has added its own
    /// [`PredicateEntry::edb`] entries to, or two consumers' registries in one
    /// process. Without this in the cache key, a [`crate::CheckedProgram`]
    /// built from one of them would answer for a goal keyed under the other.
    #[must_use]
    pub fn edb_digest_input(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for p in self.predicates.values() {
            let PredicateClass::Edb {
                required_bound,
                backing,
            } = &p.class
            else {
                continue;
            };
            crate::canonical::push_delimited(&mut out, p.key.name.as_bytes());
            crate::canonical::push_delimited(&mut out, p.key.arity.to_string().as_bytes());
            let bound: String = (0..p.key.arity)
                .map(|i| if required_bound.contains(i) { '1' } else { '0' })
                .collect();
            crate::canonical::push_delimited(&mut out, bound.as_bytes());
            for t in &p.types {
                let s = match t {
                    TypeSlot::Known(v) => v.as_str().to_owned(),
                    TypeSlot::Infer => "?".to_owned(),
                };
                crate::canonical::push_delimited(&mut out, s.as_bytes());
            }
            for b in backing {
                let s = match b {
                    Backing::RefGlob(g) => format!("ref:{g}"),
                    Backing::ContentAddressed => "content".to_owned(),
                    Backing::AdHoc => "adhoc".to_owned(),
                };
                crate::canonical::push_delimited(&mut out, s.as_bytes());
            }
        }
        out
    }

    /// A stable digest input covering every builtin's name, signature, modes,
    /// and implementation version.
    ///
    /// Without this in the cache key, improving a projection heuristic leaves
    /// every cache entry silently stale across the upgrade -- rules oid,
    /// footprint digest, and EDB digest all unchanged. A cache over a pure
    /// function is only as sound as the purity claim, and "pure" has to mean
    /// pure with respect to the implementation as well as the inputs.
    #[must_use]
    pub fn builtin_digest_input(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for b in self.builtins.values() {
            crate::canonical::push_delimited(&mut out, b.key.name.as_bytes());
            crate::canonical::push_delimited(&mut out, b.key.arity.to_string().as_bytes());
            crate::canonical::push_delimited(&mut out, b.modes.to_string().as_bytes());
            for t in &b.types {
                let s = match t {
                    ArgType::Exact(v) => v.as_str().to_owned(),
                    ArgType::AnyScalar => "*".to_owned(),
                    ArgType::SameAsArg(n) => format!("={n}"),
                };
                crate::canonical::push_delimited(&mut out, s.as_bytes());
            }
            crate::canonical::push_delimited(&mut out, b.props.version.to_string().as_bytes());
        }
        out
    }
}

fn edb(
    name: &'static str,
    types: &[ValueType],
    required_bound: ArgSet,
    backing: Vec<Backing>,
    doc: &'static str,
) -> PredicateEntry {
    PredicateEntry {
        key: PredicateKey::new(name, types.len()),
        types: types.iter().copied().map(TypeSlot::Known).collect(),
        visibility: Visibility::Public,
        class: PredicateClass::Edb {
            required_bound,
            backing,
        },
        doc: Cow::Borrowed(doc),
    }
}

/// The repo-backed EDB, exactly as the specification declares it.
///
/// Every entry's backing source is declared here beside its signature, so the
/// two cannot drift; the host crate's providers are keyed off this table, and
/// a missing provider is a test failure rather than a runtime surprise.
#[must_use]
pub fn host_predicates() -> Vec<PredicateEntry> {
    use Backing::{ContentAddressed, RefGlob};
    use ValueType::{Int, Member, Oid, Path, Rev, Symbol};

    // Enumerable: reading the whole namespace is affordable (`DEVPLAN.md`
    // §0's eager path), so nothing is required bound -- binding a position
    // anyway, to test one identity or one pair, is always legal on top of
    // that (`ModeVector::satisfied_by`'s "binding more than required" rule,
    // which a single `ArgSet` already expresses without a second vector).
    let enumerable = ArgSet::empty();
    // Content-addressed, so never enumerable: the subject (position 0) must
    // be bound.
    let keyed_on_subject = ArgSet::from_indices([0]);

    // `gix-attest`'s layout: one ref per target key, claims chained as commits
    // on it. A single star matches, because a target key is one component.
    let claims = || vec![RefGlob("refs/claims/*".into())];
    let members = || vec![RefGlob("refs/meta/members/*".into())];

    vec![
        edb(
            "commit",
            &[Rev],
            // Required-bound. Enumerating every commit in a repository is the
            // unaffordable materialization mode discipline exists to prevent,
            // and it is also what makes the reference vocabulary's `+Rev`
            // inference work: `reach`, `introduced`, `authored`, and
            // `mergeable` are all `+Rev` *because* this one is.
            keyed_on_subject,
            vec![ContentAddressed],
            "the revision names a commit",
        ),
        edb(
            "parent",
            &[Rev, Rev],
            // Required-bound on the child, so an ancestry walk is a walk. An
            // all-free call here would make `reach` read the entire commit
            // graph and would make its result uncacheable, since nothing
            // content-addressed enters the footprint.
            keyed_on_subject,
            vec![ContentAddressed],
            "the second commit is a parent of the first",
        ),
        edb(
            "tree_entry",
            &[Rev, Path, Oid],
            // Required-bound on the revision only -- a call additionally
            // binding the path is legal on top (a direct single-file lookup
            // rather than a whole-tree walk), which required-bound alone
            // already permits without a second declared vector.
            keyed_on_subject,
            vec![ContentAddressed],
            "the revision's tree holds this blob at this path",
        ),
        edb(
            "author",
            &[Rev, Member],
            keyed_on_subject,
            vec![ContentAddressed],
            "the commit's author",
        ),
        edb(
            "member",
            &[Member],
            enumerable,
            members(),
            "a known member identity",
        ),
        edb(
            "revoked",
            &[Member],
            enumerable,
            members(),
            "the member's identity has been revoked",
        ),
        edb(
            "claim",
            &[Oid],
            enumerable,
            claims(),
            "a signed body attached to an anchor",
        ),
        edb(
            "kind",
            &[Oid, Symbol],
            enumerable,
            claims(),
            "the claim's kind",
        ),
        edb(
            "target",
            &[Oid, Oid],
            enumerable,
            claims(),
            "what the claim is about",
        ),
        edb(
            "signer",
            &[Oid, Member],
            enumerable,
            claims(),
            "who signed the claim",
        ),
        edb(
            "verdict",
            &[Oid, Symbol],
            enumerable,
            claims(),
            // Payload vocabulary, not envelope machinery: the column is a
            // field of a `review` payload, read through the schema that
            // travels with it, and a claim of any other payload kind has no
            // verdict at all.
            "the verdict a review claim's payload carries",
        ),
        edb(
            "anchor",
            &[Oid],
            enumerable,
            // One ref per anchor, `refs/anchors/<id>` -- a single component,
            // so a single-star pattern matches every anchor.
            vec![RefGlob("refs/anchors/*".into())],
            "an immutable identity whose location is derived",
        ),
        edb(
            "line",
            &[Oid, Int],
            // Required-bound on the blob. The specification first listed this
            // as enumerable and warned in prose never to enumerate it; a
            // footgun documented in prose is a footgun. One registry line
            // turns a repo-sized materialization into a validation error.
            keyed_on_subject,
            vec![ContentAddressed],
            "the blob has a line numbered N, counting from 1",
        ),
    ]
}

/// The projection builtins and the comparisons.
///
/// The comparisons are here rather than assumed because the range-restriction
/// pass already refers to them, which makes them load-bearing, and the registry
/// is the only place `!=` and `<=` are defined.
#[must_use]
pub fn builtins() -> Vec<BuiltinEntry> {
    use ArgType::{AnyScalar, Exact, SameAsArg};
    use Mode::{Bound, Free};
    use ValueType::{Int, Location, Oid, Oracle, Rev};

    // `cand/5`'s fifth column, confidence, is plain `Int`: basis points on
    // `0..=10000` (four decimal digits of an oracle's `0.0..=1.0` score),
    // never a formatted string. A rule's own `bind/5` (`gix-query-rules`'s
    // always-assembled core module) selects the maximum by `!dominated(...)`
    // over a genuine `>` comparison -- ARCHITECTURE.md's aggregation stratum,
    // expressed in the rule language, not in this crate -- and that only
    // orders correctly if the value it compares is actually numeric. A
    // fixed-width zero-padded string would happen to sort the same way, but
    // "happens to" is exactly the coincidence a real ordered type avoids.
    let cand_types = vec![
        Exact(Oid),
        Exact(Rev),
        Exact(Location),
        Exact(Oracle),
        Exact(Int),
    ];
    let cand_modes = ModeVector::new(vec![Bound, Bound, Free, Free, Free]);
    let cand_props = BuiltinProps {
        total: true,
        deterministic: true,
        pure: true,
        finite: true,
        mints: true,
        version: 1,
    };

    let mut out = vec![BuiltinEntry {
        key: PredicateKey::new("cand", 5),
        types: cand_types,
        modes: cand_modes,
        props: cand_props,
        // Every candidate (pin, or one of the three named oracles) is a
        // pure function of the anchor's own content and the ref state
        // `Rev` resolves to -- unlike the old `bind_fuzzy`, nothing here
        // silently degrades based on ambient state such as whether
        // garbage collection has run.
        gate_safe: true,
        doc: "every candidate location an anchor might sit at on a revision: pin and the oracle chain, unselected",
    }];

    // `key_valid_at(+Claim)`: was the signing key valid at the op-log position
    // where the claim was admitted (ARCHITECTURE.md, "git-attest": "a claim is
    // valid iff its signing key was valid at the op-log position where the
    // claim was admitted").
    //
    // Backed by op-log admission order, which no ref in the product family
    // holds yet (DELTA X6). It is declared now, with its real signature and its
    // real backing named, because validity is a *rule's* concern and a rule
    // cannot be written against a predicate that does not exist -- the review
    // vocabulary's `reviewed/1` needs it in its body today. Its interim
    // implementation is `gix-query-host`'s absent-safe marker, the same shape
    // as `cand`'s `op_log(None, ...)` leg, and the gap is documented at that
    // one call site rather than restated here.
    //
    // A builtin rather than an EDB relation because it is not enumerable in
    // either direction: there is no finite set of "valid claims" to read, only
    // a question asked of a claim already in hand. That also keeps the op-log's
    // absence out of the cache key's `RefGlob` table, where a namespace that
    // does not exist would be a lie rather than a gap.
    out.push(BuiltinEntry {
        key: PredicateKey::new("key_valid_at", 1),
        types: vec![Exact(Oid)],
        modes: ModeVector::all_bound(1),
        props: BuiltinProps::filter(),
        // Admission order is a property of the op-log, which is authoritative,
        // append-only, and server-written -- not of ambient state -- so asking
        // this question inside a gate is safe by the same argument `cand` is.
        gate_safe: true,
        doc: "the claim's signing key was valid at the op-log position where the claim was admitted",
    });

    for op in CompareOp::all() {
        let types = if op.is_ordering() {
            vec![Exact(Int), Exact(Int)]
        } else {
            vec![AnyScalar, SameAsArg(0)]
        };
        out.push(BuiltinEntry {
            key: PredicateKey::new(op.as_str(), 2),
            types,
            modes: ModeVector::all_bound(2),
            props: BuiltinProps::filter(),
            gate_safe: true,
            doc: match op {
                CompareOp::Eq => "both terms are the same value; a filter, never a unification",
                CompareOp::Ne => "the terms differ",
                CompareOp::Lt => "strictly less, integers only",
                CompareOp::Le => "less or equal, integers only",
                CompareOp::Gt => "strictly greater, integers only",
                CompareOp::Ge => "greater or equal, integers only",
            },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required(name: &str, arity: usize) -> ArgSet {
        Registry::host()
            .predicate(&PredicateKey::new(name, arity))
            .unwrap_or_else(|| panic!("{name}/{arity} is registered"))
            .always_required_bound()
            .expect("a host predicate's modes are declared")
    }

    #[test]
    fn the_host_registry_is_self_consistent() {
        Registry::host()
            .validate()
            .expect("host registry validates");
    }

    #[test]
    fn commit_is_required_bound_so_that_rev_threading_infers() {
        // Every `+Rev` in the reference vocabulary's mode table traces back to
        // this one declaration. If `commit/1` were enumerable, `reach`,
        // `introduced`, `authored`, and `mergeable` would all infer all-free,
        // and `git query run mergeable` with no `--rev` would validate.
        assert!(required("commit", 1).contains(0));
    }

    #[test]
    fn ancestry_and_content_are_keyed_on_their_subject() {
        for (name, arity) in [("parent", 2), ("line", 2), ("author", 2), ("tree_entry", 3)] {
            assert!(
                required(name, arity).contains(0),
                "{name}/{arity} must be keyed on its subject"
            );
        }
    }

    #[test]
    fn claim_and_member_relations_stay_enumerable() {
        // The claim namespace is a finite ref glob, so reading it whole is
        // affordable -- and `reviewers(M) :- signer(C, M).` must validate.
        for (name, arity) in [
            ("member", 1),
            ("claim", 1),
            ("anchor", 1),
            ("signer", 2),
            ("verdict", 2),
            ("target", 2),
            ("kind", 2),
        ] {
            assert!(
                required(name, arity).is_empty(),
                "{name}/{arity} must be enumerable"
            );
        }
    }

    #[test]
    fn exactly_the_ref_glob_edb_is_enumerable() {
        // DEVPLAN.md §0's corrected list: the eight ref-glob predicates, never
        // the five required-bound ones, never `bind` (IDB).
        let r = Registry::host();
        let enumerable = [
            "member", "revoked", "claim", "kind", "target", "signer", "verdict", "anchor",
        ];
        let required_bound = ["commit", "parent", "tree_entry", "author", "line"];
        for name in enumerable {
            let e = r
                .predicates()
                .find(|e| e.key.name.as_ref() == name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            assert!(e.is_enumerable(), "{name} must be enumerable");
        }
        for name in required_bound {
            let e = r
                .predicates()
                .find(|e| e.key.name.as_ref() == name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            assert!(!e.is_enumerable(), "{name} must not be enumerable");
        }
        let bind = r
            .predicate(&PredicateKey::new("bind", 5))
            .expect("bind is registered");
        assert!(
            !bind.is_enumerable(),
            "an IDB predicate is never enumerable"
        );
    }

    #[test]
    fn a_content_addressed_predicate_may_not_be_enumerable() {
        let mut r = Registry::host();
        let mut parent = r
            .predicate(&PredicateKey::new("parent", 2))
            .expect("parent")
            .clone();
        parent.class = PredicateClass::Edb {
            required_bound: ArgSet::empty(),
            backing: vec![Backing::ContentAddressed],
        };
        r.predicates.insert(parent.key.clone(), parent);
        assert!(matches!(
            r.validate(),
            Err(Error::EnumerableContentAddressed { .. })
        ));
    }

    #[test]
    fn base_predicates_are_not_redefinable() {
        let mut r = Registry::host();
        let err = r.insert_predicate(PredicateEntry::idb(
            PredicateKey::new("commit", 1),
            Visibility::Public,
        ));
        assert!(matches!(err, Err(Error::NotRedefinable { .. })));
        // And the host entry survives the attempt.
        assert!(required("commit", 1).contains(0));
    }

    #[test]
    fn a_derived_predicate_starts_uninferred() {
        let e = PredicateEntry::idb(PredicateKey::new("reviewed", 1), Visibility::Public);
        assert_eq!(e.always_required_bound(), None);
        assert_eq!(e.types, vec![TypeSlot::Infer]);
    }

    #[test]
    fn the_projection_axes_are_closed_domains() {
        assert!(ValueType::Position.admits("moved"));
        assert!(!ValueType::Position.admits("intact"));
        assert!(ValueType::Content.admits("intact"));
        assert!(ValueType::Oracle.admits("pin"));
        assert!(ValueType::Oracle.admits("fingerprint"));
        assert!(!ValueType::Oracle.admits("nonsense"));
        // An open type admits anything, which is what lets `approve` be a
        // plain symbol.
        assert!(ValueType::Symbol.admits("approve"));
    }

    #[test]
    fn every_comparison_is_registered_all_bound() {
        let r = Registry::host();
        for op in CompareOp::all() {
            let e = r
                .builtin(&PredicateKey::new(op.as_str(), 2))
                .unwrap_or_else(|| panic!("{op} is registered"));
            assert_eq!(e.modes, ModeVector::all_bound(2));
            assert!(!e.props.mints);
        }
    }

    #[test]
    fn cand_is_gate_safe() {
        let r = Registry::host();
        assert!(
            r.builtin(&PredicateKey::new("cand", 5))
                .expect("cand")
                .gate_safe
        );
    }

    /// `key_valid_at` is a required-bound, non-minting filter over a claim --
    /// the signature ARCHITECTURE.md gives it. Its backing (op-log admission
    /// order) does not exist yet, and the shape of the declaration is what has
    /// to survive its arrival unchanged.
    #[test]
    fn key_valid_at_is_a_bound_filter_over_a_claim() {
        let r = Registry::host();
        let e = r
            .builtin(&PredicateKey::new("key_valid_at", 1))
            .expect("key_valid_at");
        assert_eq!(e.modes, ModeVector::all_bound(1));
        assert_eq!(e.types, vec![ArgType::Exact(ValueType::Oid)]);
        assert!(!e.props.mints, "a filter mints nothing");
        assert!(e.gate_safe, "a merge gate is its first caller");
    }

    /// The retired interim relations: the rebind-pin fields claim trailers used
    /// to carry are payload fields now, read by `cand`'s pin leg through
    /// anchor's own schema, and nothing in the rule language should be able to
    /// reach a half-parsed version of them.
    #[test]
    fn the_trailer_carried_rebind_fields_are_gone() {
        let r = Registry::host();
        for name in ["rev", "loc"] {
            assert!(
                r.predicate(&PredicateKey::new(name, 2)).is_none(),
                "{name}/2 was a trailer-era interim relation and is retired"
            );
        }
    }

    /// `bind/5` is pre-declared, `pub`, and derived -- never a builtin --
    /// exactly the shape `gix-query-rules`'s always-assembled core module
    /// needs to define it unqualified.
    #[test]
    fn bind_is_a_predeclared_public_derived_predicate() {
        let r = Registry::host();
        assert!(r.builtin(&PredicateKey::new("bind", 5)).is_none());
        let e = r
            .predicate(&PredicateKey::new("bind", 5))
            .expect("bind is pre-declared");
        assert!(!e.class.is_edb());
        assert_eq!(e.visibility, Visibility::Public);
    }

    #[test]
    fn claim_backed_predicates_contribute_ref_globs() {
        let r = Registry::host();
        let e = r
            .predicate(&PredicateKey::new("verdict", 2))
            .expect("verdict");
        // `gix-attest`'s layout, not the retired interim `refs/meta/claims/*`.
        assert_eq!(e.ref_globs().collect::<Vec<_>>(), vec!["refs/claims/*"]);
        // And a content-addressed one contributes nothing, because its oid is
        // already in the goal.
        let e = r
            .predicate(&PredicateKey::new("tree_entry", 3))
            .expect("tree_entry");
        assert_eq!(e.ref_globs().count(), 0);
    }

    #[test]
    fn the_builtin_digest_input_moves_when_a_version_moves() {
        let mut a = Registry::host();
        let before = a.builtin_digest_input();
        let mut entry = a
            .builtin(&PredicateKey::new("cand", 5))
            .expect("cand")
            .clone();
        entry.props.version += 1;
        a.insert_builtin(entry);
        assert_ne!(before, a.builtin_digest_input());
    }
}
