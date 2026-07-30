//! `claim/1`, `kind/2`, `target/2`, `signer/2`, `verdict/2` -- all five
//! backed by `gix-attest`'s claim chains under `refs/claims/*`.
//!
//! A claim is a chained commit on `refs/claims/<target-key>`; its identity `C`
//! is that commit's own oid, so no separate mapping is kept. What the claim
//! says is its [`Envelope`] -- a `gix-store` document in the commit's tree --
//! and the signature is an SSHSIG block in the commit's standard `gpgsig`
//! header. Nothing here parses a commit message: the trailer-backed store this
//! module used to be (`refs/meta/claims/*`, `Kind:`/`Target:`/`Signer:`
//! trailers) is gone, and with it the possibility of a fact that nobody signed.
//!
//! # Verification happens here, at fact-derivation time
//!
//! [`Claims::verify`] is called on every claim before any of its facts are
//! emitted, and a claim that is not [`Verdict::Verified`] contributes *no
//! rows*. That placement is the point: no rule can join on a claim whose
//! signature fails, because no such claim ever enters the EDB, so no rule
//! author has to remember to check. There is no predicate for "unverified
//! claim" for the same reason -- the relation does not exist to be joined
//! against.
//!
//! Crypto-verified is still not *valid*. Validity is `key_valid_at` (a rule's
//! predicate over op-log admission order, [`crate::cand::key_valid_at`]), and
//! this module only refuses cryptographic garbage. `gix-attest`'s
//! [`Verdict`] has no `Valid` variant precisely so the two cannot be confused.
//!
//! # The relations, and where each column comes from
//!
//! | predicate | column | envelope field |
//! |---|---|---|
//! | `claim/1` | `C` | the claim commit's oid |
//! | `kind/2` | `K` | [`Envelope::payload_kind`] |
//! | `target/2` | `Obj` | [`Target::id`] -- the `kind` *label* is not a column |
//! | `signer/2` | `M` | [`Envelope::key`], the key-add claim's id |
//! | `verdict/2` | `V` | the `verdict` field of a `review` *payload* |
//!
//! `verdict/2` is the one that moved out of the envelope entirely. A verdict is
//! payload vocabulary -- forge's review schema -- not envelope machinery, so it
//! is read by decoding the payload document against the schema that travels
//! with it, and a claim of any other payload kind contributes no verdict row.
//! Until forge registers that schema and writes such payloads, the relation is
//! simply empty: kept, not faked.
//!
//! Revocation needs no predicate of its own. A revocation *is* a claim, with
//! `payload_kind = revocation` and its target naming the claim it revokes, so
//! `kind(R, revocation), target(R, C)` already says "R revokes C" in the rule
//! language. `gix-attest`'s structural mark ([`Claim::revoked_by`]) is the same
//! fact read the other way round, and what a revocation *means* stays a rule's
//! business.

use gix::ObjectId;
use gix_anchor::{REBIND_PIN_KIND, RebindPin};
use gix_attest::{
    CLAIM_TARGET_KIND, Claim, Claims, Envelope, REVOCATION_KIND, Target, Verdict, target_key,
};
use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey, Value};
use gix_store::{RefSegment, RepoStore};

use crate::util::{backend, backend_msg, fact, parse_oid, sym_oid};

/// The [`Target::kind`] label a claim about an anchor carries.
///
/// Attest never interprets a target kind ("targets are typed per check"), and
/// anchor does not know attest exists, so the label that joins the two is
/// query's -- this crate is the composition point, and this constant is where
/// the composition is written down. `git attest sign anchor:<id>` produces
/// exactly this.
pub const ANCHOR_TARGET_KIND: &str = "anchor";

/// The [`Envelope::payload_kind`] of a review assertion, whose payload carries
/// `verdict/2`'s column.
///
/// Forge's vocabulary, named here because query composes it and forge does not
/// exist yet to export it. When it does, this constant is replaced by its
/// export rather than duplicated.
const REVIEW_PAYLOAD_KIND: &str = "review";

/// The field of a review payload `verdict/2` reads.
const VERDICT_FIELD: &str = "verdict";

/// How many revocation hops [`chain_target`] follows before giving up.
///
/// A revocation is stored on the chain of the claim it revokes, so finding a
/// chain's target from one of its claims means following revocation targets
/// down to a claim that is not itself a revocation. A revocation of a
/// revocation is legal and terminates; a cycle is not constructible by
/// `gix-attest`'s writer but is constructible by hand, and an unbounded walk
/// over hand-written refs is a hang rather than a bad answer.
const MAX_REVOCATION_HOPS: usize = 32;

/// The claim chains of one repository, read through `gix-attest`.
///
/// A handle rather than free functions because every read needs the store,
/// the store borrows the repository, and [`Claims`] borrows the store: three
/// lifetimes that are one object's business, not each caller's.
struct ClaimReader<'r> {
    store: RepoStore<'r>,
    /// The predicate being answered, for error attribution.
    key: PredicateKey,
}

impl<'r> ClaimReader<'r> {
    /// Open `repo`'s claims. The layout is `gix-attest`'s own, which is what
    /// puts claim refs at `refs/claims/<target-key>`; naming it here rather
    /// than spelling the prefix keeps this crate from having a second opinion
    /// about where claims live.
    fn open(repo: &'r gix::Repository, key: &PredicateKey) -> Self {
        Self {
            store: RepoStore::open_with_layout(repo, gix_attest::layout()),
            key: key.clone(),
        }
    }

    fn claims(&self) -> Claims<'_, gix_store::GixRefStore<'r>, &'r gix::OdbHandle> {
        Claims::open(&self.store)
    }

    fn kind(
        &self,
    ) -> gix_store::Kind<
        '_,
        gix_store::Typed<Envelope>,
        gix_store::GixRefStore<'r>,
        &'r gix::OdbHandle,
    > {
        self.store.kind::<Envelope>(claim_segment())
    }

    /// Every verified claim in the repository, with the ref it lives on,
    /// grouped chain by chain.
    ///
    /// Enumeration is over the claim kind's own entity list -- the refs under
    /// `refs/claims/` -- and each chain is read whole through
    /// [`Claims::resolve`], because a claim's revocation lives on the same
    /// chain and re-deriving that structurally here would be a second opinion
    /// about `gix-attest`'s own semantics.
    fn all(&self) -> Result<Vec<(String, Claim)>, HostError> {
        let kind = self.kind();
        let claims = self.claims();
        let mut out = Vec::new();
        for name in kind.list().map_err(|e| backend(&self.key, e))? {
            let Some(tip) = kind.get(&name).map_err(|e| backend(&self.key, e))? else {
                continue;
            };
            let Some(target) = self.chain_target(tip)? else {
                continue;
            };
            // A ref whose leaf is not the target key of the claims on it is
            // not a claim ref `gix-attest` wrote; reading it as one would
            // invent a chain out of hand-placed refs.
            let derived = target_key(&target).map_err(|e| backend(&self.key, e))?;
            if name.to_string() != derived.to_string() {
                continue;
            }
            let refname = kind.reference(&name).as_str().to_owned();
            for claim in claims.resolve(&target).map_err(|e| backend(&self.key, e))? {
                if self.verified(&claim)? {
                    out.push((refname.clone(), claim));
                }
            }
        }
        out.sort_by(|a, b| (&a.0, a.1.id).cmp(&(&b.0, b.1.id)));
        Ok(out)
    }

    /// The verified claim `id`, with the ref it lives on, or `None` when `id`
    /// is not a claim commit, is not on the chain its own envelope names, or
    /// does not verify.
    ///
    /// A bound claim is read by walking one chain, not by scanning the
    /// namespace: the chain is derived from the claim's own envelope, so
    /// `target(+C, B)` inside a fixpoint costs one chain rather than every
    /// claim in the repository.
    fn one(&self, id: ObjectId) -> Result<Option<(String, Claim)>, HostError> {
        let claims = self.claims();
        let Ok(envelope) = claims.envelope_at(id) else {
            // Not a claim commit at all -- the same "no such identity" answer
            // `member/1` gives for an unregistered member, not an error.
            return Ok(None);
        };
        let Some(target) = self.chain_target(envelope)? else {
            return Ok(None);
        };
        let chain = claims
            .resolve(&target)
            .map_err(|e| backend(&self.key, e))?
            .into_iter()
            .find(|claim| claim.id == id);
        let Some(claim) = chain else {
            return Ok(None);
        };
        if !self.verified(&claim)? {
            return Ok(None);
        }
        let refname = claims
            .reference(&target)
            .map_err(|e| backend(&self.key, e))?
            .as_str()
            .to_owned();
        Ok(Some((refname, claim)))
    }

    /// The target whose chain `envelope`'s claim sits on: its own target,
    /// unless it is a revocation, which lives on the chain of the claim it
    /// revokes.
    ///
    /// `None` when the walk runs past [`MAX_REVOCATION_HOPS`] -- a
    /// hand-constructed cycle, which contributes no facts rather than hanging.
    fn chain_target(&self, envelope: Envelope) -> Result<Option<Target>, HostError> {
        let claims = self.claims();
        let mut envelope = envelope;
        for _ in 0..MAX_REVOCATION_HOPS {
            if !(envelope.payload_kind == REVOCATION_KIND
                && envelope.target.kind == CLAIM_TARGET_KIND)
            {
                return Ok(Some(envelope.target));
            }
            let Ok(next) = claims.envelope_at(envelope.target.id.into()) else {
                return Ok(None);
            };
            envelope = next;
        }
        Ok(None)
    }

    /// Whether `claim`'s signature is the signing key's over its own bytes.
    ///
    /// A [`Claims::verify`] *error* -- an envelope naming a key that is not a
    /// key claim, a key chain that cannot be read -- is "not verified" rather
    /// than a query failure. A claim signed by an unreachable key is exactly as
    /// unusable as one signed badly, and one such claim in a namespace must not
    /// fail every query that reads the namespace. This is also why a key-add
    /// claim contributes no facts today: a chain root names no predecessor to
    /// verify against, and whether to trust a root is policy, not cryptography.
    fn verified(&self, claim: &Claim) -> Result<bool, HostError> {
        Ok(matches!(self.claims().verify(claim), Ok(Verdict::Verified)))
    }

    /// `claim`'s payload, decoded as `T`, or `None` when its payload kind is
    /// not `payload_kind` or the payload does not decode as `T`.
    ///
    /// The decode is store's document decode, which reads the schema pinned
    /// *inside the payload tree* rather than from any ref: a payload carries
    /// the schema it was written against, so a reader needs no registration of
    /// its own and cannot silently read a payload under a schema it was not
    /// written under. A payload labeled one kind but shaped like another is bad
    /// data at a boundary this crate does not control, so it contributes
    /// nothing rather than failing the query.
    fn payload<T: for<'a> facet::Facet<'a>>(&self, claim: &Claim, payload_kind: &str) -> Option<T> {
        if claim.envelope.payload_kind != payload_kind {
            return None;
        }
        let segment = RefSegment::new(payload_kind).ok()?;
        self.store
            .kind::<T>(segment)
            .decode(claim.envelope.payload.into())
            .ok()
    }

    /// `verdict/2`'s column for `claim`: the `verdict` field of its review
    /// payload, read dynamically because forge's review type is not a type this
    /// crate has.
    fn review_verdict(&self, claim: &Claim) -> Option<String> {
        if claim.envelope.payload_kind != REVIEW_PAYLOAD_KIND {
            return None;
        }
        let segment = RefSegment::new(REVIEW_PAYLOAD_KIND).ok()?;
        let value = self
            .store
            .dynamic(segment)
            .decode(claim.envelope.payload.into())
            .ok()?;
        Some(
            value
                .as_object()?
                .get(VERDICT_FIELD)?
                .as_string()?
                .as_str()
                .to_owned(),
        )
    }
}

/// The store kind name claims are written under -- `gix-attest`'s own, so the
/// ref layout is read from the crate that defines it.
fn claim_segment() -> RefSegment {
    RefSegment::new(gix_attest::CLAIM_KIND).expect("gix-attest's claim kind is a valid ref segment")
}

/// One claim-attached two-place relation, and which part of a claim it reads.
///
/// Derived from the predicate key once, here, so the column's type is a fact
/// about the *relation* rather than a guess from the shape of whatever value
/// happened to be read -- the registry declares `kind: Symbol`, `target: Oid`,
/// `signer: Member`, `verdict: Symbol` (`gix_query_ir::host_predicates`), and
/// each arm below produces exactly its declared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimRelation {
    /// `kind/2` from [`Envelope::payload_kind`], `Symbol`-typed.
    Kind,
    /// `target/2` from [`Target::id`], `Oid`-typed.
    Target,
    /// `signer/2` from [`Envelope::key`], `Member`-typed: the id of the
    /// key-add claim that published the signing key. The same oid wire shape
    /// `member/1` reads, as before the re-backing.
    Signer,
    /// `verdict/2` from the review payload, `Symbol`-typed.
    Verdict,
}

impl ClaimRelation {
    fn of(key: &PredicateKey) -> Option<Self> {
        if key.arity != 2 {
            return None;
        }
        match key.name.as_ref() {
            "kind" => Some(Self::Kind),
            "target" => Some(Self::Target),
            "signer" => Some(Self::Signer),
            "verdict" => Some(Self::Verdict),
            _ => None,
        }
    }

    /// This relation's value for `claim`, or `None` when the claim has none --
    /// which for `verdict/2` is every claim that is not a review.
    fn value(self, reader: &ClaimReader<'_>, claim: &Claim) -> Option<Value> {
        match self {
            Self::Kind => Some(Value::sym(claim.envelope.payload_kind.clone())),
            Self::Target => Some(sym_oid(claim.envelope.target.id.into())),
            Self::Signer => Some(sym_oid(claim.envelope.key.into())),
            Self::Verdict => reader.review_verdict(claim).map(Value::sym),
        }
    }
}

pub(crate) fn claim_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let reader = ClaimReader::open(repo, key);
    let found = match bound.first() {
        Some(Some(v)) => reader.one(parse_oid(key, v)?)?.into_iter().collect(),
        _ => reader.all()?,
    };
    let out: Vec<Result<Fact, HostError>> = found
        .into_iter()
        .map(|(refname, claim)| Ok(fact(vec![sym_oid(claim.id)], Some(&refname), attribute)))
        .collect();
    Ok(Box::new(out.into_iter()))
}

/// `kind/2`, `target/2`, `signer/2`, and `verdict/2` share this shape: they
/// differ only in which [`ClaimRelation`] `key` names, derived once here rather
/// than threaded in by each caller in `facts.rs` as a separate label that could
/// name a different field than `key` itself does.
pub(crate) fn envelope_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let relation = ClaimRelation::of(key).ok_or_else(|| HostError::NoProvider(key.clone()))?;
    let want_value = match bound.get(1) {
        Some(Some(v)) => Some(v.clone()),
        Some(None) | None => None,
    };
    let reader = ClaimReader::open(repo, key);
    let found: Vec<(String, Claim)> = match bound.first() {
        Some(Some(v)) => reader.one(parse_oid(key, v)?)?.into_iter().collect(),
        _ => reader.all()?,
    };
    let mut out = Vec::new();
    for (refname, claim) in found {
        let Some(value) = relation.value(&reader, &claim) else {
            continue;
        };
        if want_value.as_ref().is_some_and(|want| want != &value) {
            continue;
        }
        out.push(Ok(fact(
            vec![sym_oid(claim.id), value],
            Some(&refname),
            attribute,
        )));
    }
    Ok(Box::new(out.into_iter()))
}

/// Every location a verified `rebind-pin` claim pins `anchor` to at `rev` --
/// `cand`'s pin leg (ARCHITECTURE.md: "pin dominates the lattice"), at
/// confidence 1.0 by construction wherever this returns a non-empty list.
///
/// This is the composition point the architecture names: "anchor vocabulary in
/// claims is a store schema registered by anchor, composed only in query." The
/// envelope is attest's, the [`RebindPin`] payload schema is anchor's, and
/// neither crate knows the other exists -- the join happens here and nowhere
/// else. The `Loc` term is assembled from the payload's own `path` and `span`
/// fields, formatted exactly as `cand/5`'s third column
/// (`"<path>:<start>-<end>"`), so a pin and an oracle candidate are the same
/// shape of value.
///
/// A claim whose signature does not verify pins nothing: verification happens
/// before the payload is read, so a forged pin cannot dominate the lattice.
pub(crate) fn rebind_pin_locs(
    repo: &gix::Repository,
    key: &PredicateKey,
    anchor: ObjectId,
    rev: ObjectId,
) -> Result<Vec<String>, HostError> {
    let reader = ClaimReader::open(repo, key);
    let target = Target {
        kind: ANCHOR_TARGET_KIND.to_owned(),
        id: anchor.into(),
    };
    let chain = reader
        .claims()
        .resolve(&target)
        .map_err(|e| backend_msg(key, format!("reading claims about anchor {anchor}: {e}")))?;
    let mut out = Vec::new();
    for claim in chain {
        if !reader.verified(&claim)? {
            continue;
        }
        let Some(pin) = reader.payload::<RebindPin>(&claim, REBIND_PIN_KIND) else {
            continue;
        };
        if ObjectId::from(pin.rev) != rev {
            continue;
        }
        let (start, end) = pin.span;
        out.push(format!("{}:{start}-{end}", pin.path));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use facet::Facet;

    use super::*;
    use test_support::{ClaimFixture, Fixture};

    /// Forge's review payload, stood in for by its shape: `verdict/2` reads the
    /// `verdict` field of whatever document a `review` payload decodes to, and
    /// this is the smallest document with that field. When forge registers the
    /// real schema, this fixture type is what it replaces.
    #[derive(Debug, Facet)]
    struct Review {
        verdict: String,
    }

    struct Repo {
        _dir: tempfile::TempDir,
        f: Fixture,
        claims: ClaimFixture,
    }

    fn repo() -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        f.commit_file("a.txt", "a\n", "first");
        let claims = ClaimFixture::new(&f);
        claims.register_payload_schema::<Review>(REVIEW_PAYLOAD_KIND);
        Repo {
            _dir: dir,
            f,
            claims,
        }
    }

    fn blob_target(f: &Fixture, path: &str) -> Target {
        let blob = f.blob_at("HEAD", path);
        Target {
            kind: "blob".to_owned(),
            id: ObjectId::from_hex(blob.as_bytes())
                .expect("hex blob")
                .into(),
        }
    }

    /// A review claim about `target` with verdict `verdict`.
    fn review(r: &Repo, target: &Target, verdict: &str) -> ObjectId {
        let payload = r.claims.payload(
            REVIEW_PAYLOAD_KIND,
            &Review {
                verdict: verdict.to_owned(),
            },
        );
        r.claims.sign(target, payload, REVIEW_PAYLOAD_KIND)
    }

    fn facts_of(repo: &gix::Repository, name: &str, bound: &[Option<Value>]) -> Vec<Vec<Value>> {
        let key = PredicateKey::new(name, bound.len());
        let facts = if bound.len() == 1 {
            claim_facts(repo, &key, bound, false)
        } else {
            envelope_facts(repo, &key, bound, false)
        };
        facts
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok")
            .into_iter()
            .map(|f| f.values)
            .collect()
    }

    /// Every column of every claim relation, on one signed review claim.
    #[test]
    fn a_signed_claims_envelope_populates_the_four_two_place_relations() {
        let r = repo();
        let target = blob_target(&r.f, "a.txt");
        let id = review(&r, &target, "approve");
        let repo = r.f.open();

        assert_eq!(
            facts_of(&repo, "claim", &[None]),
            vec![vec![sym_oid(id)]],
            "the key-add claim contributes nothing: a chain root has no \
             predecessor to verify against"
        );
        assert_eq!(
            facts_of(&repo, "kind", &[Some(sym_oid(id)), None]),
            vec![vec![sym_oid(id), Value::sym(REVIEW_PAYLOAD_KIND)]],
            "kind/2 is the envelope's payload_kind"
        );
        assert_eq!(
            facts_of(&repo, "target", &[Some(sym_oid(id)), None]),
            vec![vec![sym_oid(id), sym_oid(target.id.into())]],
            "target/2 is the target's id; its kind label is not a column"
        );
        assert_eq!(
            facts_of(&repo, "signer", &[Some(sym_oid(id)), None]),
            vec![vec![sym_oid(id), sym_oid(r.claims.key_claim())]],
            "signer/2 is the key-add claim id, Member-shaped as before"
        );
        assert_eq!(
            facts_of(&repo, "verdict", &[Some(sym_oid(id)), None]),
            vec![vec![sym_oid(id), Value::sym("approve")]],
            "verdict/2 is read out of the review payload, not the envelope"
        );
    }

    /// A bound claim is answered from its own chain, and an oid that is not a
    /// claim answers empty rather than erroring -- the same lenience
    /// `member.rs`'s `unknown_member_is_false_not_an_error` has.
    #[test]
    fn a_bound_claim_reads_its_own_chain_and_a_stranger_answers_empty() {
        let r = repo();
        let target = blob_target(&r.f, "a.txt");
        let one = review(&r, &target, "approve");
        let two = review(&r, &target, "reject");
        assert_ne!(one, two, "setup: the two claims must be distinct");
        let repo = r.f.open();

        assert_eq!(facts_of(&repo, "claim", &[Some(sym_oid(one))]).len(), 1);
        assert_eq!(
            facts_of(&repo, "claim", &[None]).len(),
            2,
            "both claims on the one chain are enumerated"
        );

        let stranger = r.f.hash_object("not a claim");
        assert!(
            facts_of(&repo, "claim", &[Some(Value::sym(stranger))]).is_empty(),
            "an oid that is not a claim commit is no such claim, not an error"
        );
    }

    /// The whole point of moving verification to fact-derivation time: a claim
    /// whose commit bytes were tampered with after signing contributes nothing,
    /// so no rule can join on it. The tampering is done with `git hash-object`,
    /// so the forgery is real objects in the repository rather than a mocked
    /// verdict.
    #[test]
    fn a_tampered_claim_contributes_no_facts_at_all() {
        let r = repo();
        let target = blob_target(&r.f, "a.txt");
        let id = review(&r, &target, "approve");

        let raw = r.f.git(&["cat-file", "commit", &id.to_string()]);
        let flipped = raw.replace("claim review", "claim REVIEW");
        assert_ne!(raw, flipped, "setup: the summary must have been rewritten");
        let tampered = r.f.hash_commit(&flipped);
        // Point the chain at the forgery: same tree, same signature, different
        // bytes.
        let refname = r.claims.reference(&target);
        r.f.update_ref(&refname, &tampered.to_string());

        let repo = r.f.open();
        assert!(
            facts_of(&repo, "claim", &[None]).is_empty(),
            "a tampered claim is not a claim"
        );
        assert!(
            facts_of(&repo, "verdict", &[None]).is_empty(),
            "and it contributes no payload fact either"
        );
        assert!(
            !r.f.git_ok(&["verify-commit", &tampered.to_string()]),
            "the oracle agrees: stock git rejects the forgery too"
        );
    }

    /// A revocation is a claim like any other, and says what it revokes in the
    /// rule language: `kind(R, revocation), target(R, C)`. No `revoked_claim/1`
    /// predicate is needed, and none exists.
    #[test]
    fn a_revocation_is_readable_as_an_ordinary_claim_about_a_claim() {
        let r = repo();
        let target = blob_target(&r.f, "a.txt");
        let id = review(&r, &target, "approve");
        let revocation = r.claims.revoke(id);
        let repo = r.f.open();

        assert_eq!(
            facts_of(&repo, "kind", &[Some(sym_oid(revocation)), None]),
            vec![vec![sym_oid(revocation), Value::sym(REVOCATION_KIND)]]
        );
        assert_eq!(
            facts_of(&repo, "target", &[Some(sym_oid(revocation)), None]),
            vec![vec![sym_oid(revocation), sym_oid(id)]],
            "the revocation's target is the claim it revokes"
        );
        assert_eq!(
            facts_of(&repo, "claim", &[None]).len(),
            2,
            "the revoked claim is marked, not deleted: it is still a claim"
        );
    }

    /// `verdict/2` is payload-backed, so a claim of another payload kind has no
    /// verdict -- the relation is empty rather than defaulted.
    #[test]
    fn a_non_review_claim_has_no_verdict() {
        let r = repo();
        let target = blob_target(&r.f, "a.txt");
        let payload = r.claims.payload(
            REVIEW_PAYLOAD_KIND,
            &Review {
                verdict: "approve".to_owned(),
            },
        );
        let id = r.claims.sign(&target, payload, "something-else");
        let repo = r.f.open();

        assert_eq!(facts_of(&repo, "kind", &[Some(sym_oid(id)), None]).len(), 1);
        assert!(
            facts_of(&repo, "verdict", &[Some(sym_oid(id)), None]).is_empty(),
            "a verdict is review vocabulary, not something every claim has"
        );
    }

    /// The pin leg: a signed `rebind-pin` claim on an anchor target is read
    /// back through anchor's own schema, at the rev it names and no other.
    #[test]
    fn a_signed_rebind_pin_is_read_back_through_anchors_schema() {
        let r = repo();
        r.claims
            .register_payload_schema::<RebindPin>(REBIND_PIN_KIND);
        let head = r.f.rev_parse("HEAD");
        let rev = ObjectId::from_hex(head.as_bytes()).expect("hex rev");
        let anchor = ObjectId::from_hex(r.f.hash_object("an anchor").as_bytes()).expect("hex");

        let pin = RebindPin {
            rev: rev.into(),
            path: "src/refdb/store.rs".to_owned(),
            span: (118, 568),
        };
        let payload = r.claims.payload(REBIND_PIN_KIND, &pin);
        r.claims.sign(
            &Target {
                kind: ANCHOR_TARGET_KIND.to_owned(),
                id: anchor.into(),
            },
            payload,
            REBIND_PIN_KIND,
        );

        let repo = r.f.open();
        let key = PredicateKey::new("cand", 5);
        assert_eq!(
            rebind_pin_locs(&repo, &key, anchor, rev).expect("pins"),
            vec!["src/refdb/store.rs:118-568".to_owned()],
        );

        let other = ObjectId::null(gix::hash::Kind::Sha1);
        assert!(
            rebind_pin_locs(&repo, &key, anchor, other)
                .expect("pins")
                .is_empty(),
            "a pin is pinned at one rev"
        );
        assert!(
            rebind_pin_locs(&repo, &key, other, rev)
                .expect("pins")
                .is_empty(),
            "and about one anchor"
        );
    }
}
