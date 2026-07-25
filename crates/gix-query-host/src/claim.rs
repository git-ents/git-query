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
use gix_query_ir::{Bindings, Facts, HostError, PredicateKey, Value};

use crate::util::{backend, backend_boxed, fact, parse_oid, sym_oid};

const CLAIMS_PREFIX: &str = "refs/meta/claims/";

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

/// The value of trailer `token` on `claim`'s commit message, if present.
fn trailer_value(
    repo: &gix::Repository,
    key: &PredicateKey,
    claim: ObjectId,
    token: &str,
) -> Result<Option<String>, HostError> {
    let Ok(commit) = repo.find_commit(claim) else {
        return Ok(None);
    };
    let decoded = commit.decode().map_err(|error| backend(key, error))?;
    for trailer in decoded.message_trailers() {
        if trailer.token.to_str().is_ok_and(|t| t == token) {
            return Ok(Some(trailer.value.to_string()));
        }
    }
    Ok(None)
}

pub(crate) fn claim_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
) -> Result<Facts<'a>, HostError> {
    let want = match bound.first() {
        Some(Some(v)) => Some(parse_oid(key, v)?),
        Some(None) | None => None,
    };
    let out: Vec<_> = claim_refs(repo, key)?
        .into_iter()
        .filter(|(_, id)| want.is_none_or(|w| w == *id))
        .map(|(name, id)| Ok(fact(vec![sym_oid(id)], Some(&name), attribute)))
        .collect();
    Ok(Box::new(out.into_iter()))
}

/// `kind/2`, `target/2`, `signer/2`, and `verdict/2` share this shape:
/// they differ only in which trailer token they read.
pub(crate) fn trailer_facts<'a>(
    repo: &'a gix::Repository,
    key: &PredicateKey,
    bound: Bindings<'_>,
    attribute: bool,
    token: &'static str,
) -> Result<Facts<'a>, HostError> {
    let want_claim = match bound.first() {
        Some(Some(v)) => Some(parse_oid(key, v)?),
        Some(None) | None => None,
    };
    let want_value = match bound.get(1) {
        Some(Some(v)) => Some(v.clone()),
        Some(None) | None => None,
    };
    let mut out = Vec::new();
    for (name, claim) in claim_refs(repo, key)? {
        if want_claim.is_some_and(|w| w != claim) {
            continue;
        }
        let Some(text) = trailer_value(repo, key, claim, token)? else {
            continue;
        };
        let value = if let Ok(oid) = ObjectId::from_hex(text.as_bytes()) {
            // `target` and `signer` are oid-typed; round-tripping through the
            // parsed oid rather than the raw trailer text keeps the emitted
            // value canonical even if the trailer was hand-written in a
            // different case.
            sym_oid(oid)
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
    Ok(Box::new(out.into_iter()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::Fixture;

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
            "Kind",
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
            "Target",
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
            "Verdict",
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
}
