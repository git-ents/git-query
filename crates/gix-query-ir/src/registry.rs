use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use crate::{ArgSet, CompareOp, Error, Mode, ModeSet, ModeVector, PredicateKey};

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
    /// `bind/5`'s third column: a path and byte span, encoded as
    /// `"<path>:<start>-<end>"` -- one atomic term, since the value domain
    /// has no tuple type to hold `(path, start, end)` as three fields.
    Location,
    /// `bind/5`'s fourth column: which oracle contributed a candidate --
    /// `pin`, or one of `gix_anchor::Oracle`'s three named oracles.
    Oracle,
    /// `bind/5`'s fifth column: an oracle's own confidence in a candidate,
    /// `0.0..=1.0`, formatted to four decimal places (`"0.9300"`) so two
    /// equal confidences are the same term rather than differing in
    /// float-formatting noise. Not ordered by the comparison builtins --
    /// max-selection over it is `bind`'s own aggregation stratum, not a rule
    /// author comparing it with `>`.
    Confidence,
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
            Self::Confidence => "confidence",
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
    /// Host-supplied and repo-backed. Carries a mode *set*, not a vector.
    Edb {
        /// The mode vectors this predicate's provider supports.
        modes: ModeSet,
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

    /// The arguments every caller must bind, whichever mode vector is chosen.
    ///
    /// For a derived predicate this is `None` until mode inference has run.
    #[must_use]
    pub fn always_required_bound(&self) -> Option<ArgSet> {
        match &self.class {
            PredicateClass::Edb { modes, .. } => Some(modes.always_required()),
            PredicateClass::Idb { required_bound } => *required_bound,
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

    /// The host registry: the repo-backed EDB predicates and every builtin.
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
            let PredicateClass::Edb { modes, backing } = &e.class else {
                continue;
            };
            if !backing.contains(&Backing::ContentAddressed) {
                continue;
            }
            if modes.vectors().any(|v| v.required_bound().is_empty()) {
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
    modes: ModeSet,
    backing: Vec<Backing>,
    doc: &'static str,
) -> PredicateEntry {
    PredicateEntry {
        key: PredicateKey::new(name, types.len()),
        types: types.iter().copied().map(TypeSlot::Known).collect(),
        visibility: Visibility::Public,
        class: PredicateClass::Edb { modes, backing },
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
    use Mode::{Bound, Free};
    use ValueType::{Int, Location, Member, Oid, Path, Rev, Symbol};

    // Enumerate the namespace, or test one identity. Both affordable, so both
    // legal -- which is the whole reason an EDB predicate carries a mode set.
    let enumerable_or_test1 = ModeSet::new([ModeVector::all_free(1), ModeVector::all_bound(1)]);
    // The same for a two-place relation over an enumerable namespace: read it
    // whole, index by its subject, or test one pair.
    let enumerable_or_test2 = ModeSet::new([
        ModeVector::all_free(2),
        ModeVector::new(vec![Bound, Free]),
        ModeVector::all_bound(2),
    ]);
    // Content-addressed, so never enumerable: the subject must be bound.
    let keyed2 = ModeSet::new([ModeVector::new(vec![Bound, Free]), ModeVector::all_bound(2)]);

    let claims = || vec![RefGlob("refs/meta/claims/*".into())];
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
            ModeSet::new([ModeVector::all_bound(1)]),
            vec![ContentAddressed],
            "the revision names a commit",
        ),
        edb(
            "parent",
            &[Rev, Rev],
            // Required-bound on the child, so an ancestry walk is a walk. An
            // all-free vector here would make `reach` read the entire commit
            // graph and would make its result uncacheable, since nothing
            // content-addressed enters the footprint.
            keyed2.clone(),
            vec![ContentAddressed],
            "the second commit is a parent of the first",
        ),
        edb(
            "tree_entry",
            &[Rev, Path, Oid],
            ModeSet::new([
                ModeVector::new(vec![Bound, Free, Free]),
                ModeVector::new(vec![Bound, Bound, Free]),
                ModeVector::all_bound(3),
            ]),
            vec![ContentAddressed],
            "the revision's tree holds this blob at this path",
        ),
        edb(
            "author",
            &[Rev, Member],
            keyed2.clone(),
            vec![ContentAddressed],
            "the commit's author",
        ),
        edb(
            "member",
            &[Member],
            enumerable_or_test1.clone(),
            members(),
            "a known member identity",
        ),
        edb(
            "revoked",
            &[Member],
            enumerable_or_test1.clone(),
            members(),
            "the member's identity has been revoked",
        ),
        edb(
            "claim",
            &[Oid],
            enumerable_or_test1.clone(),
            claims(),
            "a signed body attached to an anchor",
        ),
        edb(
            "kind",
            &[Oid, Symbol],
            enumerable_or_test2.clone(),
            claims(),
            "the claim's kind",
        ),
        edb(
            "target",
            &[Oid, Oid],
            enumerable_or_test2.clone(),
            claims(),
            "what the claim is about",
        ),
        edb(
            "signer",
            &[Oid, Member],
            enumerable_or_test2.clone(),
            claims(),
            "who signed the claim",
        ),
        edb(
            "verdict",
            &[Oid, Symbol],
            enumerable_or_test2.clone(),
            claims(),
            "the claim's verdict",
        ),
        edb(
            "anchor",
            &[Oid],
            enumerable_or_test1.clone(),
            // One ref per anchor, `refs/anchors/<id>` -- a single component,
            // so a single-star pattern matches every anchor.
            vec![RefGlob("refs/anchors/*".into())],
            "an immutable identity whose location is derived",
        ),
        edb(
            "rev",
            &[Oid, Rev],
            enumerable_or_test2.clone(),
            claims(),
            "a rebind-pin claim's pinned revision",
        ),
        edb(
            "loc",
            &[Oid, Location],
            enumerable_or_test2,
            claims(),
            "a rebind-pin claim's pinned location, \"<path>:<start>-<end>\"",
        ),
        edb(
            "line",
            &[Oid, Int],
            // Required-bound on the blob. The specification first listed this
            // as enumerable and warned in prose never to enumerate it; a
            // footgun documented in prose is a footgun. One registry line
            // turns a repo-sized materialization into a validation error.
            keyed2,
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
    use ValueType::{Confidence, Int, Location, Oid, Oracle, Rev};

    let bind_types = vec![
        Exact(Oid),
        Exact(Rev),
        Exact(Location),
        Exact(Oracle),
        Exact(Confidence),
    ];
    let bind_modes = ModeVector::new(vec![Bound, Bound, Free, Free, Free]);
    let bind_props = BuiltinProps {
        total: true,
        deterministic: true,
        pure: true,
        finite: true,
        mints: true,
        version: 1,
    };

    let mut out = vec![BuiltinEntry {
        key: PredicateKey::new("bind", 5),
        types: bind_types,
        modes: bind_modes,
        props: bind_props,
        // Every candidate (pin, or one of the three named oracles) is a
        // pure function of the anchor's own content and the ref state
        // `Rev` resolves to -- unlike the old `bind_fuzzy`, nothing here
        // silently degrades based on ambient state such as whether
        // garbage collection has run.
        gate_safe: true,
        doc: "resolve an anchor onto a revision: pin and the oracle chain, one confidence lattice",
    }];

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
    fn a_content_addressed_predicate_may_not_be_enumerable() {
        let mut r = Registry::host();
        let mut parent = r
            .predicate(&PredicateKey::new("parent", 2))
            .expect("parent")
            .clone();
        parent.class = PredicateClass::Edb {
            modes: ModeSet::enumerable(2),
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
    fn bind_is_gate_safe() {
        let r = Registry::host();
        assert!(
            r.builtin(&PredicateKey::new("bind", 5))
                .expect("bind")
                .gate_safe
        );
    }

    #[test]
    fn claim_backed_predicates_contribute_ref_globs() {
        let r = Registry::host();
        let e = r
            .predicate(&PredicateKey::new("verdict", 2))
            .expect("verdict");
        assert_eq!(
            e.ref_globs().collect::<Vec<_>>(),
            vec!["refs/meta/claims/*"]
        );
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
            .builtin(&PredicateKey::new("bind", 5))
            .expect("bind")
            .clone();
        entry.props.version += 1;
        a.insert_builtin(entry);
        assert_ne!(before, a.builtin_digest_input());
    }
}
