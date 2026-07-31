//! `git effect` -- the one binary for the effect subsystem.
//!
//! `ARCHITECTURE.md`'s git-effect section names the CLI shape `define`,
//! `status`, `log`. Phase B1 wired up `define`; Phase B3 (D8) adds `status`
//! and `log`, both reading the transition log at `refs/effects/log`.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use gix_effect::{EffectDoc, EffectStore, RecordStore, Then, TransitionRecord};
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "git-effect", bin_name = "git effect")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Define an effect doc: validate it, then store it at
    /// `refs/meta/effects/<name>`.
    ///
    /// Validation -- parsing and checking `when`/`gate` against the host
    /// registry, then the acyclicity check over every already-registered
    /// effect plus this one -- runs before anything is written, so a
    /// rejected define leaves the repository untouched.
    Define {
        /// Path to the effect doc, in this CLI's TOML shape (see
        /// [`EffectDocFile`]).
        doc: std::path::PathBuf,
        /// Which repository to define into. Defaults to discovering one from
        /// the current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
    /// Show the current state of every effect doc, joined with its last
    /// transition (D8: "current effect docs joined with their last log
    /// entries -- a query, run through query").
    Status {
        /// Which repository to read. Defaults to discovering one from the
        /// current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
    /// Show every signed transition record at `refs/effects/log`, oldest
    /// first by idempotence key.
    Log {
        /// Which repository to read. Defaults to discovering one from the
        /// current directory.
        #[arg(long)]
        repo: Option<std::path::PathBuf>,
    },
}

/// The on-disk shape a `define` file is parsed from.
///
/// `ARCHITECTURE.md` names the CLI surface (`git effect define <doc>`) but
/// not a file format for `<doc>`; [`EffectDoc`] itself derives `Facet`
/// (`gix-store`'s codec), not `serde`, because its stored shape is a git tree,
/// not a text file. TOML is this crate's own choice for the *input* file: it
/// reads well for the doc's two multi-line rule-language bodies (`when`,
/// `gate`) plus a short list (`writes`) and a small tagged union (`then`),
/// and needs no dependency beyond `serde` + `toml`, which nothing else in
/// this workspace's `Cargo.lock` outside this one binary crate pulls in --
/// `gix-effect` itself stays free of both, per `DEVPLAN-effect.md`'s
/// dependency list.
#[derive(Debug, Deserialize)]
struct EffectDocFile {
    name: String,
    when: String,
    gate: String,
    then: ThenFile,
    #[serde(default)]
    writes: Vec<String>,
}

/// [`Then`], as TOML spells it: `[then]` with `kind = "advance"` or `"run"`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ThenFile {
    Advance {
        #[serde(rename = "ref")]
        ref_: String,
        to: String,
    },
    Run {
        executor: String,
        #[serde(default)]
        inputs: Vec<String>,
    },
}

impl From<ThenFile> for Then {
    fn from(then: ThenFile) -> Self {
        match then {
            ThenFile::Advance { ref_, to } => Then::Advance { ref_, to },
            ThenFile::Run { executor, inputs } => Then::Run { executor, inputs },
        }
    }
}

impl From<EffectDocFile> for EffectDoc {
    fn from(file: EffectDocFile) -> Self {
        EffectDoc {
            name: file.name,
            when: file.when,
            gate: file.gate,
            then: file.then.into(),
            writes: file.writes,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => ExitCode::from(0),
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(2)
        }
    }
}

fn open_repo(repo: Option<std::path::PathBuf>) -> anyhow::Result<gix::Repository> {
    Ok(match repo {
        Some(path) => gix::open(path)?,
        None => gix::discover(".")?,
    })
}

fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Define { doc, repo } => run_define(&doc, repo),
        Command::Status { repo } => run_status(repo),
        Command::Log { repo } => run_log(repo),
    }
}

/// A [`gix_store::Signer`] that is never asked to sign anything: `status` and
/// `log` only read [`RecordStore`], and [`RecordStore::open`] takes a signer
/// eagerly for writers to reuse, but a reader has nothing to sign.
struct NeverSigner;

impl gix_store::Signer for NeverSigner {
    type Error = std::convert::Infallible;

    fn sign(&self, _bytes: &[u8]) -> Result<gix_store::SignatureBytes, Self::Error> {
        unreachable!("`git effect status`/`log` never write, so this is never called")
    }
}

fn run_status(repo: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let repo = open_repo(repo)?;
    let effects = EffectStore::open(&repo)?.list()?;
    let records = RecordStore::open(&repo, NeverSigner)?;
    let log = records.log()?;

    for effect in effects {
        let last = log
            .iter()
            .filter(|r| r.effect == effect.name)
            .max_by_key(|r| r.key.clone());
        match last {
            Some(record) => println!(
                "{}: last {} ({})",
                effect.name,
                outcome_label(&record.outcome),
                record.key
            ),
            None => println!("{}: never fired", effect.name),
        }
    }
    Ok(())
}

fn run_log(repo: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let repo = open_repo(repo)?;
    let records = RecordStore::open(&repo, NeverSigner)?;
    let mut log = records.log()?;
    log.sort_by(|a, b| a.key.cmp(&b.key));

    for record in &log {
        print_record(record);
    }
    Ok(())
}

fn outcome_label(outcome: &gix_effect::Outcome) -> String {
    match outcome {
        gix_effect::Outcome::Advanced => "advanced".to_owned(),
        gix_effect::Outcome::Failed { reason } => format!("failed ({reason})"),
    }
}

fn print_record(record: &TransitionRecord) {
    println!(
        "{} {} [{} -> {}] key {}",
        record.effect, record.ref_, record.old, record.new, record.key
    );
    for edit in &record.edits {
        println!("  {}: {} -> {}", edit.ref_, edit.old, edit.new);
    }
    println!("  outcome: {}", outcome_label(&record.outcome));
}

fn run_define(doc: &std::path::Path, repo: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let repo = open_repo(repo)?;
    let text =
        std::fs::read_to_string(doc).with_context(|| format!("reading {}", doc.display()))?;
    let file: EffectDocFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", doc.display()))?;
    let name = file.name.clone();
    let store = EffectStore::open(&repo)?;
    store.define(file.into())?;
    println!("defined {name} at refs/meta/effects/{name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("git-effect").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?} should parse: {e}"))
    }

    #[test]
    fn define_requires_a_doc_path() {
        assert!(Cli::try_parse_from(["git-effect", "define"]).is_err());
    }

    #[test]
    fn define_parses_with_a_doc_path_and_optional_repo() {
        let Command::Define { doc, repo } = parse(&["define", "effect.toml"]).command else {
            panic!("expected Define");
        };
        assert_eq!(doc, std::path::PathBuf::from("effect.toml"));
        assert_eq!(repo, None);
    }

    #[test]
    fn status_and_log_parse() {
        assert!(matches!(
            parse(&["status"]).command,
            Command::Status { repo: None }
        ));
        assert!(matches!(
            parse(&["log"]).command,
            Command::Log { repo: None }
        ));
    }

    /// `status`/`log` outside any repository fail loudly rather than
    /// silently answering nothing.
    #[test]
    fn status_and_log_fail_outside_a_repository() {
        let missing = Some(std::path::PathBuf::from("/nonexistent-git-effect-fixture"));
        assert!(
            run(Command::Status {
                repo: missing.clone()
            })
            .is_err()
        );
        assert!(run(Command::Log { repo: missing }).is_err());
    }

    /// `status` reports every defined effect, joined with its last transition
    /// -- "never fired" for one with no log entry, the outcome for one that
    /// has. `log` lists every recorded transition.
    #[test]
    fn status_and_log_report_the_defined_effects_and_their_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = test_support::Fixture::init(dir.path());
        fixture.commit("first");
        let repo = fixture.open();

        let store = gix_effect::EffectStore::open(&repo).unwrap();
        store
            .define(gix_effect::EffectDoc {
                name: "never-fired".to_owned(),
                when: "member(M)".to_owned(),
                gate: "member(M)".to_owned(),
                then: gix_effect::Then::Advance {
                    ref_: "refs/heads/main".to_owned(),
                    to: "HEAD".to_owned(),
                },
                writes: vec!["refs/heads/main".to_owned()],
            })
            .unwrap();

        struct FixedSigner;
        impl gix_store::Signer for FixedSigner {
            type Error = std::convert::Infallible;
            fn sign(&self, _bytes: &[u8]) -> Result<gix_store::SignatureBytes, Self::Error> {
                Ok(gix_store::SignatureBytes::from(b"sig".to_vec()))
            }
        }
        let records = RecordStore::open(&repo, FixedSigner).unwrap();
        records
            .record(
                "aabb",
                TransitionRecord {
                    effect: "never-fired".to_owned(),
                    ref_: "refs/heads/main".to_owned(),
                    old: String::new(),
                    new: "cc".to_owned(),
                    key: "aabb".to_owned(),
                    edits: vec![],
                    outcome: gix_effect::Outcome::Advanced,
                },
            )
            .unwrap();

        assert!(run_status(Some(dir.path().to_path_buf())).is_ok());
        assert!(run_log(Some(dir.path().to_path_buf())).is_ok());
    }

    /// An `advance` doc parses into [`Then::Advance`] with the fields TOML
    /// names `ref`/`to` -- `ref_` only exists on the Rust side, to dodge the
    /// keyword.
    #[test]
    fn a_toml_advance_doc_parses_into_the_advance_variant() {
        let toml = r#"
            name = "release"
            when = "member(M)"
            gate = "signer(C, M)"
            writes = ["refs/release/candidate"]

            [then]
            kind = "advance"
            ref = "refs/release/candidate"
            to = "HEAD"
        "#;
        let file: EffectDocFile = toml::from_str(toml).unwrap();
        let doc: EffectDoc = file.into();
        assert_eq!(doc.name, "release");
        assert_eq!(
            doc.then,
            Then::Advance {
                ref_: "refs/release/candidate".to_owned(),
                to: "HEAD".to_owned(),
            }
        );
    }

    /// A `run` doc parses into [`Then::Run`].
    #[test]
    fn a_toml_run_doc_parses_into_the_run_variant() {
        let toml = r#"
            name = "kick-build"
            when = "member(M)"
            gate = "signer(C, M)"
            writes = ["refs/claims/*"]

            [then]
            kind = "run"
            executor = "kiln"
            inputs = ["a", "b"]
        "#;
        let file: EffectDocFile = toml::from_str(toml).unwrap();
        let doc: EffectDoc = file.into();
        assert_eq!(
            doc.then,
            Then::Run {
                executor: "kiln".to_owned(),
                inputs: vec!["a".to_owned(), "b".to_owned()],
            }
        );
    }
}
