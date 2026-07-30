//! Shared scaffolding for the workspace's integration tests.
//!
//! The fixture builder shells out to real `git` rather than writing objects
//! through `gix`. That is deliberate: the crates under test read repositories
//! with `gix`, so a fixture written with `gix` would test our reader against
//! our writer and agree with itself about any misunderstanding they shared.
//! Building with the `git` binary makes git itself the oracle.

use std::path::{Path, PathBuf};
use std::process::Command;

mod claim;

pub use claim::ClaimFixture;

/// Initialize a git repository at `path` with a committer identity configured,
/// so `gix::Repository::commit` has an author and committer to record.
pub fn init_repo(path: &Path) {
    let repo = gix::init(path).expect("init repo");
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(repo.git_dir().join("config"))
        .expect("open config");
    use std::io::Write as _;
    writeln!(config, "[user]\n\tname = Test\n\temail = test@example.com").expect("write config");
}

/// A repository built by driving the `git` binary.
///
/// Every method panics on failure. These are fixtures: a fixture that fails
/// halfway and returns an error only moves the confusion into the test that
/// used it.
#[derive(Debug)]
pub struct Fixture {
    root: PathBuf,
}

impl Fixture {
    /// Initialize a repository at `root`, which must not already exist as a
    /// repository.
    ///
    /// The default branch is pinned to `main` rather than inherited, and
    /// commit identity, date, and GPG settings are all fixed, so that object
    /// ids are reproducible across machines and across runs. A fixture whose
    /// oids move is a fixture no test can assert against.
    pub fn init(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        std::fs::create_dir_all(&root).expect("create fixture root");
        let f = Self { root };
        f.git(&["init", "--quiet", "--initial-branch=main"]);
        f.git(&["config", "user.name", "Test Author"]);
        f.git(&["config", "user.email", "author@example.com"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        f.git(&["config", "gc.auto", "0"]);
        f
    }

    /// The repository's working-tree root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Open the fixture with `gix`.
    #[must_use]
    pub fn open(&self) -> gix::Repository {
        gix::open(&self.root).expect("open fixture")
    }

    /// Run `git` in the fixture and return its trimmed stdout.
    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            // A fixed clock keeps commit ids reproducible; without it every
            // run mints new oids and no test can name one.
            .env("GIT_AUTHOR_DATE", "2005-04-07T22:13:13+00:00")
            .env("GIT_COMMITTER_DATE", "2005-04-07T22:13:13+00:00")
            .env("GIT_COMMITTER_NAME", "Test Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .output()
            .unwrap_or_else(|e| panic!("run git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("git output is utf-8")
            .trim_end()
            .to_owned()
    }

    /// Write `contents` to `path` within the fixture, creating parents.
    pub fn write(&self, path: &str, contents: &str) {
        let full = self.root.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(full, contents).expect("write fixture file");
    }

    /// Write `path`, stage it, and commit with `message`. Returns the new
    /// commit's full object id.
    pub fn commit_file(&self, path: &str, contents: &str, message: &str) -> String {
        self.write(path, contents);
        self.git(&["add", "--", path]);
        self.commit(message)
    }

    /// Commit whatever is staged, with `message`. Returns the commit's oid.
    ///
    /// `--allow-empty` because a fixture legitimately wants a commit that
    /// changes nothing -- a merge, or a root commit whose tree is empty.
    pub fn commit(&self, message: &str) -> String {
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.rev_parse("HEAD")
    }

    /// Resolve a revision expression to a full object id.
    pub fn rev_parse(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev])
    }

    /// The object id of `path`'s blob at `rev`.
    pub fn blob_at(&self, rev: &str, path: &str) -> String {
        self.git(&["rev-parse", &format!("{rev}:{path}")])
    }

    /// Point `refname` at `target`, creating it. Takes a full refname, since
    /// the fixtures that matter here live outside `refs/heads`.
    pub fn update_ref(&self, refname: &str, target: &str) {
        self.git(&["update-ref", refname, target]);
    }

    /// Run `git` in the fixture and report only whether it succeeded.
    ///
    /// For the commands whose *failure* is the assertion -- `git
    /// verify-commit` on a forgery -- where [`Fixture::git`]'s panic-on-failure
    /// is exactly the wrong behavior.
    pub fn git_ok(&self, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap_or_else(|e| panic!("run git {args:?}: {e}"))
            .status
            .success()
    }

    /// Store `text` as a loose commit object verbatim and return its oid.
    ///
    /// The bytes are written unaltered, which is what makes a tampered-commit
    /// fixture real: `git hash-object -t commit` stores whatever it is handed,
    /// so a signature that no longer covers those bytes is a genuine forgery in
    /// the object database rather than a mocked verification failure.
    pub fn hash_commit(&self, text: &str) -> gix::ObjectId {
        let path = self.root.join(".git-fixture-commit");
        std::fs::write(&path, text).expect("write scratch commit");
        let oid = self.git(&[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--",
            path.to_str().expect("utf-8 scratch path"),
        ]);
        std::fs::remove_file(&path).expect("remove scratch");
        gix::ObjectId::from_hex(oid.as_bytes()).expect("git returned a hex oid")
    }

    /// Store `contents` as a loose blob and return its oid.
    pub fn hash_object(&self, contents: &str) -> String {
        let path = self.root.join(".git-fixture-scratch");
        std::fs::write(&path, contents).expect("write scratch");
        let oid = self.git(&[
            "hash-object",
            "-w",
            "--",
            path.to_str().expect("utf-8 scratch path"),
        ]);
        std::fs::remove_file(&path).expect("remove scratch");
        oid
    }

    /// Every ref in the repository, as `(refname, oid)` pairs sorted by
    /// refname -- the same shape and order the footprint digest consumes.
    pub fn refs(&self) -> Vec<(String, String)> {
        let out = self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
        let mut pairs: Vec<(String, String)> = out
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();
        pairs.sort();
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixture_is_reproducible() {
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        let one = Fixture::init(a.path().join("r"));
        let two = Fixture::init(b.path().join("r"));
        let c1 = one.commit_file("src/lib.rs", "fn main() {}\n", "add lib");
        let c2 = two.commit_file("src/lib.rs", "fn main() {}\n", "add lib");
        // Same content, same message, same pinned clock and identity, so the
        // same oid -- which is what lets a test name one.
        assert_eq!(c1, c2);
    }

    #[test]
    fn refs_come_back_sorted_and_paired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        // The leaf under `refs/meta/members/*` is a member's hex id
        // (`gix-query-host`'s `member.rs` and `util.rs::parse_oid` both
        // commit to this), not an arbitrary name, so the fixture uses one
        // here rather than a literal like `joey` that no real member ref
        // would ever hold.
        let member = f.hash_object("joey");
        let refname = format!("refs/meta/members/{member}");
        f.update_ref(&refname, &head);
        let refs = f.refs();
        assert!(refs.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(
            refs.iter()
                .any(|(name, oid)| name == &refname && oid == &head)
        );
    }

    #[test]
    fn gix_can_open_what_git_built() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = Fixture::init(dir.path().join("r"));
        let head = f.commit_file("a.txt", "a\n", "first");
        let repo = f.open();
        let resolved = repo.rev_parse_single("HEAD").expect("resolve HEAD");
        assert_eq!(resolved.to_string(), head);
    }
}
