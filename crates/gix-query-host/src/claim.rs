//! `claim/1`, `kind/2`, `target/2`, `signer/2`, `verdict/2` -- all five
//! backed by `refs/meta/claims/*`.
//!
//! One ref per claim, `refs/meta/claims/<hex>`, pointing at a commit; the
//! claim's identity `C` is that commit's own oid, so no separate mapping is
//! kept. `kind`, `target`, `signer`, and `verdict` are commit-message
//! trailers on that same commit (`Kind:`, `Target:`, `Signer:`, `Verdict:`),
//! read with `gix`'s own trailer parser rather than a hand-rolled one, so
//! this crate never disagrees with `git interpret-trailers` about what counts
//! as a trailer block.

use gix::ObjectId;
use gix::bstr::ByteSlice as _;
use gix_query_ir::{Bindings, Fact, Facts, HostError, PredicateKey, Value};

use crate::util::{backend, backend_boxed, fact, parse_oid, sym_oid};

const CLAIMS_PREFIX: &str = "refs/meta/claims/";

/// One claim-attached two-place relation: its trailer token and its declared
/// column type, traveling together rather than being re-derived per value.
///
/// The registry declares each precisely -- `kind: Symbol`, `target: Oid`,
/// `signer: Member`, `verdict: Symbol` (`gix_query_ir::host_predicates`) --
/// but the value column used to be typed by *guessing from its shape*,
/// trying `ObjectId::from_hex` on every trailer regardless of which relation
/// it belonged to. That guess is silently wrong in both directions: a
/// `Signer:` written as a bare email fails the parse and falls through as an
/// opaque symbol with no diagnostic that it was never a member id, and a
/// `Verdict:` that happened to look like 40 hex characters would be
/// "canonicalized" through an oid parse for a column that was never oid-typed
/// to begin with. Deriving [`TrailerRelation`] from the predicate key once,
/// here, is what makes the column type a fact about the relation rather than
/// a property inferred anew from every value it happens to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrailerRelation {
    /// `kind/2`, `Symbol`-typed.
    Kind,
    /// `target/2`, `Oid`-typed.
    Target,
    /// `signer/2`, `Member`-typed (the same hex wire shape as `Oid`).
    Signer,
    /// `verdict/2`, `Symbol`-typed.
    Verdict,
}

impl TrailerRelation {
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

    /// The commit-message trailer token this relation reads.
    fn token(self) -> &'static str {
        match self {
            Self::Kind => "Kind",
            Self::Target => "Target",
            Self::Signer => "Signer",
            Self::Verdict => "Verdict",
        }
    }

    /// Whether this relation's value column is declared `Oid`- or
    /// `Member`-typed, and should therefore be canonicalized through a hex
    /// parse rather than passed through as a bare symbol.
    fn is_oid_shaped(self) -> bool {
        matches!(self, Self::Target | Self::Signer)
    }
}

/// Every claim ref's `(refname, claim oid)` pair, sorted by refname.
fn claim_refs(
    repo: &gix::Repository,
    key: &PredicateKey,
) -> Result<Vec<(String, ObjectId)>, HostError> {
    let platform = repo.references().map_err(|error| backend(key, error))?;
    let mut out = Vec::new();
    for reference in platform
        .prefixed(CLAIMS_PREFIX)
        .map_err(|error| backend(key, error))?
    {
        let mut reference = reference.map_err(|error| backend_boxed(key, error))?;
        let name = reference.name().as_bstr().to_string();
        let id = reference
            .peel_to_id()
            .map_err(|error| backend(key, error))?
            .detach();
        out.push((name, id));
    }
    out.sort();
    Ok(out)
}

/// `claim`'s own ref, by direct lookup rather than enumerating the whole
/// namespace to find it.
///
/// A claim's identity *is* its ref's leaf (this module's own doc comment:
/// "no separate mapping is kept"), so a bound claim never needs to scan
/// every claim ref to confirm it exists -- `member.rs`'s `status` takes the
/// same direct-lookup shape for a bound member. Without this, `target(+C,
/// B)` inside the fixpoint reads every claim ref on every call, which is
/// O(n) per call and O(n^2) over a query that revisits `C` for each
/// candidate.
fn claim_ref(
    repo: &gix::Repository,
    key: &PredicateKey,
    claim: ObjectId,
) -> Result<Option<String>, HostError> {
    let refname = format!("{CLAIMS_PREFIX}{claim}");
    let found = repo
        .try_find_reference(&refname)
        .map_err(|error| backend(key, error))?
        .is_some();
    Ok(found.then_some(refname))
}

/// Every value of trailer `token` on `claim`'s commit message, in message
/// order.
///
/// `kind/2`, `target/2`, `signer/2`, and `verdict/2` are all-free relations,
/// and git parses a repeated trailer token happily -- `git interpret-trailers
/// --parse` reports every `Target:` line on a commit with two of them, not
/// just the first. Stopping at the first match here would make `RepoFacts`
/// disagree with that oracle and, worse, disagree silently: a rejection
/// naming two blobs would leave the second unrejected, `blocked/1` would not
/// fire for it, and the gate would admit a rejected blob. Emitting one fact
/// per occurrence keeps this predicate's extension exactly what git itself
/// says the trailer block contains.
fn trailer_values(
    repo: &gix::Repository,
    key: &PredicateKey,
    claim: ObjectId,
    token: &str,
) -> Result<Vec<String>, HostError> {
    let Ok(commit) = repo.find_commit(claim) else {
        return Ok(Vec::new());
    };
    let decoded = commit.decode().map_err(|error| backend(key, error))?;
    Ok(decoded
        .message_trailers()
        .filter(|trailer| trailer.token.to_str().is_ok_and(|t| t == token))
        .map(|trailer| trailer.value.to_string())
        .collect())
}

pub(crate) fn claim_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    match bound.first() {
        Some(Some(v)) => {
            let claim = parse_oid(key, v)?;
            let out: Vec<Result<Fact, HostError>> = match claim_ref(repo, key, claim)? {
                Some(name) => vec![Ok(fact(vec![sym_oid(claim)], Some(&name), attribute))],
                None => vec![],
            };
            Ok(Box::new(out.into_iter()))
        }
        _ => {
            let out: Vec<_> = claim_refs(repo, key)?
                .into_iter()
                .map(|(name, id)| Ok(fact(vec![sym_oid(id)], Some(&name), attribute)))
                .collect();
            Ok(Box::new(out.into_iter()))
        }
    }
}

/// `kind/2`, `target/2`, `signer/2`, and `verdict/2` share this shape: they
/// differ only in which [`TrailerRelation`] `key` names, derived once here
/// rather than threaded in by each caller in `facts.rs` as a separate string
/// that could name a different token than `key` itself does.
pub(crate) fn trailer_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let relation = TrailerRelation::of(key).ok_or_else(|| HostError::NoProvider(key.clone()))?;
    let want_claim = match bound.first() {
        Some(Some(v)) => Some(parse_oid(key, v)?),
        Some(None) | None => None,
    };
    let want_value = match bound.get(1) {
        Some(Some(v)) => Some(v.clone()),
        Some(None) | None => None,
    };
    let claims: Vec<(String, ObjectId)> = match want_claim {
        Some(claim) => claim_ref(repo, key, claim)?
            .into_iter()
            .map(|name| (name, claim))
            .collect(),
        None => claim_refs(repo, key)?,
    };
    let mut out = Vec::new();
    for (name, claim) in claims {
        for text in trailer_values(repo, key, claim, relation.token())? {
            let value = if relation.is_oid_shaped() {
                match ObjectId::from_hex(text.as_bytes()) {
                    // Round-tripping through the parsed oid rather than the
                    // raw trailer text keeps the emitted value canonical even
                    // if the trailer was hand-written in a different case.
                    Ok(oid) => sym_oid(oid),
                    Err(_) => Value::sym(text),
                }
            } else {
                Value::sym(text)
            };
            if want_value.as_ref().is_some_and(|want| want != &value) {
                continue;
            }
            out.push(Ok(fact(
                vec![sym_oid(claim), value],
                Some(&name),
                attribute,
            )));
        }
    }
    Ok(Box::new(out.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::Fixture;

    /// A bound claim is answered by a direct ref lookup rather than a scan
    /// over the whole claims namespace -- with two claims present, binding
    /// one must return only its own fact, and an unregistered claim oid must
    /// answer empty rather than erroring, exactly like `member.rs`'s
    /// `unknown_member_is_false_not_an_error`.
    #[test]
    fn a_bound_claim_is_a_direct_lookup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let blob = f.commit_file("a.txt", "a\n", "first");
        let signer = f.hash_object("Alice <alice@example.com>");
        let one = claim(&f, &blob, &signer, "approve");
        let two = claim(&f, &blob, &signer, "reject");
        let repo = f.open();
        let key = PredicateKey::new("claim", 1);

        let out: Vec<_> = claim_facts(&repo, &key, &[Some(Value::sym(one.as_str()))], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].values[0], Value::sym(one.as_str()));

        assert_ne!(one, two, "setup: the two claims must be distinct");

        let stranger = f.hash_object("not a claim");
        let out = claim_facts(&repo, &key, &[Some(Value::sym(stranger))], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert!(out.is_empty());

        let verdict: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("verdict", 2),
            &[Some(Value::sym(one.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert_eq!(verdict.len(), 1);
        assert_eq!(verdict[0].values[1], Value::sym("approve"));
    }

    /// Commit a claim with the four trailers `claim.rs` reads, and point
    /// `refs/meta/claims/<message-derived-name>` at it. Returns the claim's
    /// oid.
    fn claim(f: &Fixture, target: &str, signer: &str, verdict: &str) -> String {
        let message = format!(
            "review\n\nKind: review\nTarget: {target}\nSigner: {signer}\nVerdict: {verdict}\n"
        );
        let id = f.commit(&message);
        f.update_ref(&format!("{CLAIMS_PREFIX}{id}"), &id);
        id
    }

    #[test]
    fn a_claims_trailers_populate_the_four_two_place_relations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let blob = f.commit_file("a.txt", "a\n", "first");
        let signer = f.hash_object("Alice <alice@example.com>");
        let id = claim(&f, &blob, &signer, "approve");
        let repo = f.open();

        let kind: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("kind", 2),
            &[Some(Value::sym(id.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert_eq!(kind[0].values[1], Value::sym("review"));

        let target: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("target", 2),
            &[Some(Value::sym(id.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert_eq!(target[0].values[1], Value::sym(blob.as_str()));

        let verdict: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("verdict", 2),
            &[Some(Value::sym(id.as_str())), Some(Value::sym("approve"))],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert_eq!(verdict.len(), 1);

        let claims: Vec<_> = claim_facts(&repo, &PredicateKey::new("claim", 1), &[None], false)
            .expect("facts")
            .collect::<Result<Vec<_>, _>>()
            .expect("ok");
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].values[0], Value::sym(id.as_str()));
    }

    /// A rejection naming two blobs -- two `Target:` trailers on one claim --
    /// must produce two `target/2` tuples, or `rejected(B) :- review(C),
    /// target(C,B), verdict(C,reject), ...` leaves the second blob
    /// unrejected and a gate admits it. `git interpret-trailers --parse` is
    /// the oracle: it is asked to parse the exact same message text, and
    /// this test asserts `RepoFacts` reports exactly what git reports,
    /// rather than trusting our own reading of "trailer".
    #[test]
    fn repeated_trailers_are_all_emitted_not_just_the_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let blob_a = f.commit_file("a.txt", "a\n", "first");
        let blob_b = f.hash_object("b");
        let signer = f.hash_object("Alice <alice@example.com>");
        let message = format!(
            "review\n\nKind: review\nTarget: {blob_a}\nTarget: {blob_b}\nSigner: {signer}\nVerdict: reject\n"
        );
        let id = f.commit(&message);
        f.update_ref(&format!("{CLAIMS_PREFIX}{id}"), &id);
        let repo = f.open();

        let target: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("target", 2),
            &[Some(Value::sym(id.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");

        let scratch = dir.path().join("message.txt");
        std::fs::write(&scratch, &message).expect("write scratch message");
        let git_out = f.git(&[
            "interpret-trailers",
            "--parse",
            scratch.to_str().expect("utf-8 scratch path"),
        ]);
        let git_targets: Vec<&str> = git_out
            .lines()
            .filter_map(|l| l.strip_prefix("Target: "))
            .collect();

        assert_eq!(
            git_targets.len(),
            2,
            "setup: git itself must see two Target: trailers"
        );
        assert_eq!(
            target.len(),
            git_targets.len(),
            "RepoFacts must report exactly as many target/2 tuples as git \
             interpret-trailers reports Target: trailers"
        );
        let ours: std::collections::BTreeSet<_> =
            target.iter().map(|f| f.values[1].clone()).collect();
        assert!(ours.contains(&Value::sym(blob_a.as_str())));
        assert!(ours.contains(&Value::sym(blob_b.as_str())));
    }

    /// The column type is declared per relation, not guessed from a value's
    /// shape: a `Kind:` trailer that happens to look like 40 hex characters
    /// must come back byte-for-byte rather than being round-tripped through
    /// an oid parse, because `kind/2` is `Symbol`-typed regardless of what
    /// any particular value looks like.
    #[test]
    fn symbol_typed_columns_are_never_treated_as_oids_even_if_hex_shaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let blob = f.commit_file("a.txt", "a\n", "first");
        let signer = f.hash_object("Alice <alice@example.com>");
        let hex_looking = "a".repeat(40);
        let message = format!(
            "review\n\nKind: {hex_looking}\nTarget: {blob}\nSigner: {signer}\nVerdict: {hex_looking}\n"
        );
        let id = f.commit(&message);
        f.update_ref(&format!("{CLAIMS_PREFIX}{id}"), &id);
        let repo = f.open();

        let kind: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("kind", 2),
            &[Some(Value::sym(id.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert_eq!(kind[0].values[1], Value::sym(hex_looking.as_str()));
    }

    /// The bound-path manifestation finding 7 names directly: a `Signer:`
    /// trailer written as a bare email is not oid-shaped, so it falls
    /// through as a plain symbol rather than being silently dropped -- the
    /// fact still exists, ready for `member(+M)` to answer "no such member"
    /// on it (`member.rs`'s own lenient bound-path parsing).
    #[test]
    fn signer_written_as_an_email_still_produces_a_fact() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let blob = f.commit_file("a.txt", "a\n", "first");
        let message = format!(
            "review\n\nKind: review\nTarget: {blob}\nSigner: alice@example.com\nVerdict: approve\n"
        );
        let id = f.commit(&message);
        f.update_ref(&format!("{CLAIMS_PREFIX}{id}"), &id);
        let repo = f.open();

        let signer: Vec<_> = trailer_facts(
            &repo,
            &PredicateKey::new("signer", 2),
            &[Some(Value::sym(id.as_str())), None],
            false,
        )
        .expect("facts")
        .collect::<Result<Vec<_>, _>>()
        .expect("ok");
        assert_eq!(signer[0].values[1], Value::sym("alice@example.com"));
    }
}
