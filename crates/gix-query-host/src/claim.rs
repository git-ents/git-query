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
        for text in trailer_values(repo, key, claim, token)? {
            let value = if let Ok(oid) = ObjectId::from_hex(text.as_bytes()) {
                // `target` and `signer` are oid-typed; round-tripping through
                // the parsed oid rather than the raw trailer text keeps the
                // emitted value canonical even if the trailer was
                // hand-written in a different case.
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
            "Target",
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
}
