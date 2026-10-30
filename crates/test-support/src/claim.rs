//! Signed claims in a fixture repository, written by `gix-attest` itself.
//!
//! Claims used to be commits with hand-written trailers, which a fixture could
//! fake with `git commit -m`. They are not that any more: a claim is a chained
//! commit on `refs/claims/<target-key>` whose tree is a `gix-store` document
//! and whose signature is an SSHSIG block in the standard `gpgsig` header. A
//! fixture that imitated that shape would be asserting our reader against our
//! imitation, so this one writes claims through the real
//! [`gix_attest::Claims`], with real signatures produced by shelling out to
//! `ssh-keygen -Y sign` -- git's own signing backend, and therefore the same
//! bytes `git verify-commit` checks.
//!
//! What the fixture owns is *mechanics*: keys, schema registration, signing.
//! It owns no claim vocabulary -- no target-kind labels, no payload kinds --
//! because those belong to the crates that define them, and a fixture holding
//! its own copy of a label is a fixture that can agree with itself while
//! disagreeing with the reader under test.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use facet::Facet;
use gix::ObjectId;
use gix_attest::{AttestKey, Claim, Claims, Envelope, Target};
use gix_store::{RefSegment, RepoStore, SignatureBytes, Signer, schema_of};

use crate::Fixture;

/// Signs by shelling out to `ssh-keygen -Y sign` under git's own SSHSIG
/// namespace, so the block on a claim commit is byte-for-byte what git's ssh
/// signing backend would have written.
struct SshKeygen {
    key: PathBuf,
}

impl Signer for SshKeygen {
    type Error = std::io::Error;

    fn sign(&self, bytes: &[u8]) -> Result<SignatureBytes, Self::Error> {
        let mut child = Command::new("ssh-keygen")
            .args(["-Y", "sign", "-q", "-n", "git", "-f"])
            .arg(&self.key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child.stdin.take().expect("piped stdin").write_all(bytes)?;
        let out = child.wait_with_output()?;
        assert!(out.status.success(), "ssh-keygen -Y sign failed");
        Ok(SignatureBytes::from(out.stdout))
    }
}

/// A claim writer over one [`Fixture`]: an ssh key, published as an attest key
/// claim, plus the schemas attest needs to write envelopes at all.
///
/// Every method panics on failure, like the rest of the fixture builder.
pub struct ClaimFixture {
    repo: gix::Repository,
    key: PathBuf,
    key_claim: ObjectId,
    public: String,
}

impl ClaimFixture {
    /// Generate a signing key in `f`'s repository, register attest's schemas,
    /// and publish the key as a key-add claim.
    ///
    /// The repository is also configured to trust that key for its own
    /// committer identity, so `git verify-commit` and `git log
    /// --show-signature` work on the claims written here -- the whole point of
    /// the `gpgsig` transport is that stock git can check them, and a fixture
    /// that skipped the config would leave that unexercised.
    #[must_use]
    pub fn new(f: &Fixture) -> Self {
        let key = f.root().join("claim-signing-key");
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "claims", "-f"])
            .arg(&key)
            .status()
            .expect("run ssh-keygen");
        assert!(status.success(), "ssh-keygen keygen failed");
        let public = std::fs::read_to_string(f.root().join("claim-signing-key.pub"))
            .expect("read public key");

        let allowed = f.root().join("allowed_signers");
        std::fs::write(
            &allowed,
            format!("author@example.com namespaces=\"git\" {public}"),
        )
        .expect("write allowed signers");
        f.git(&["config", "gpg.format", "ssh"]);
        f.git(&[
            "config",
            "gpg.ssh.allowedSignersFile",
            allowed.to_str().expect("utf-8 allowed-signers path"),
        ]);

        let repo = f.open();
        let this = Self {
            repo,
            key,
            key_claim: ObjectId::null(gix::hash::Kind::Sha1),
            public,
        };
        let store = this.store();
        gix_attest::register_schemas(&store).expect("register attest schemas");
        let key_claim = Claims::open(&store)
            .add_key(&AttestKey::from_openssh(&this.public, false).expect("key material"))
            .expect("publish the signing key");
        Self { key_claim, ..this }
    }

    /// A signing store over the fixture's repository, in attest's own
    /// layout -- exposed so a consumer whose own write path drives
    /// `gix-attest` directly (`git-effect`'s executor seam, D6) can sign real
    /// claims through it rather than re-deriving the `ssh-keygen` plumbing
    /// itself.
    #[must_use]
    pub fn store(&self) -> RepoStore<'_> {
        RepoStore::open_with_layout(&self.repo, gix_attest::layout()).with_signer(SshKeygen {
            key: self.key.clone(),
        })
    }

    /// The key-add claim id every envelope written here names as its signer.
    #[must_use]
    pub fn key_claim(&self) -> ObjectId {
        self.key_claim
    }

    /// Register `T`'s schema under the kind name `kind`, so payloads of that
    /// kind can be compiled and, later, decoded against a schema that travels
    /// with them.
    ///
    /// In production this is the vocabulary owner's job -- anchor's
    /// `register_rebind_pin_schema`, forge's for review assertions. A fixture
    /// stands in for the owner rather than for the schema: the shape is `T`'s
    /// own, derived, never transcribed.
    pub fn register_payload_schema<T: for<'a> Facet<'a>>(&self, kind: &str) {
        let store = self.store();
        let segment = RefSegment::new(kind).expect("valid kind name");
        let schema = schema_of::<T>().expect("derive schema");
        store
            .dynamic(segment)
            .schema()
            .put(&schema)
            .expect("publish payload schema");
    }

    /// Compile `value` into a payload document tree under the kind `kind`,
    /// returning the tree hash an [`Envelope`] carries as its payload.
    ///
    /// This is store's `{value/, schema/}` tree, not a bare serialization: the
    /// schema a payload was written against travels with it, which is what lets
    /// a reader decode a payload whose Rust type it does not have.
    pub fn payload<T: for<'a> Facet<'a>>(&self, kind: &str, value: &T) -> ObjectId {
        let segment = RefSegment::new(kind).expect("valid kind name");
        self.store()
            .kind::<T>(segment)
            .compile(value)
            .expect("compile payload")
    }

    /// Sign a claim about `target` carrying `payload` of kind `payload_kind`,
    /// and return the claim id.
    pub fn sign(&self, target: &Target, payload: ObjectId, payload_kind: &str) -> ObjectId {
        let envelope = Envelope {
            target: target.clone(),
            payload: payload.into(),
            payload_kind: payload_kind.to_owned(),
            key: self.key_claim.into(),
        };
        Claims::open(&self.store())
            .sign(&envelope)
            .expect("sign a claim")
    }

    /// Revoke `claim`, returning the revocation's own claim id.
    pub fn revoke(&self, claim: ObjectId) -> ObjectId {
        Claims::open(&self.store())
            .revoke(claim, self.key_claim.into())
            .expect("revoke a claim")
    }

    /// The chain of claims on `target`, newest first, straight from
    /// `gix-attest` -- the library oracle a re-backed claim EDB is asserted
    /// against.
    #[must_use]
    pub fn log(&self, target: &Target) -> Vec<Claim> {
        Claims::open(&self.store())
            .log(target)
            .expect("walk a claim chain")
            .collect()
    }

    /// The ref the claims about `target` live on.
    #[must_use]
    pub fn reference(&self, target: &Target) -> String {
        Claims::open(&self.store())
            .reference(target)
            .expect("derive a claim ref")
            .as_str()
            .to_owned()
    }
}
