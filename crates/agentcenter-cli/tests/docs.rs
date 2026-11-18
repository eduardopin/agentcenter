//! Generated CLI reference (`docs/CLI.md`) and man page (`man/agentcenter-cli.1`).
//! Both come from the hidden `gen-docs` subcommand, which reads the same command
//! table the binary dispatches on, so a command cannot exist without docs.

use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_agentcenter-cli");

fn repo_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn stdout(args: &[&str]) -> String {
    let out = run(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Subcommand names as the top-level usage lists them (`  name   summary`).
fn commands() -> Vec<String> {
    let usage = stdout(&["--help"]);
    let names: Vec<String> = usage
        .lines()
        .skip_while(|l| *l != "commands:")
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect();
    assert!(names.len() >= 6, "usage lists too few commands: {usage}");
    assert!(names.contains(&"insights".to_string()), "{usage}");
    names
}

/// Every `--long` flag token in a command's own help.
fn flags_of(cmd: &str) -> Vec<String> {
    let help = stdout(&[cmd, "--help"]);
    let mut flags: Vec<String> = help
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|w| w.starts_with("--") && w.len() > 2)
        .map(str::to_string)
        .collect();
    flags.sort();
    flags.dedup();
    assert!(!flags.is_empty(), "{cmd} --help shows no flag: {help}");
    flags
}

#[test]
fn committed_markdown_matches_the_generator() {
    let generated = stdout(&["gen-docs", "markdown"]);
    let committed = std::fs::read_to_string(repo_file("docs/CLI.md"))
        .expect("docs/CLI.md missing: run scripts/gen-docs.sh");
    assert_eq!(
        generated, committed,
        "stale docs/CLI.md: run scripts/gen-docs.sh"
    );
}

#[test]
fn committed_man_page_matches_the_generator() {
    let generated = stdout(&["gen-docs", "man"]);
    let committed = std::fs::read_to_string(repo_file("man/agentcenter-cli.1"))
        .expect("man/agentcenter-cli.1 missing: run scripts/gen-docs.sh");
    assert_eq!(
        generated, committed,
        "stale man/agentcenter-cli.1: run scripts/gen-docs.sh"
    );
}

#[test]
fn every_subcommand_appears_in_both_docs_with_its_flags() {
    let md = stdout(&["gen-docs", "markdown"]);
    let man = stdout(&["gen-docs", "man"]);
    for cmd in commands() {
        assert!(
            md.contains(&format!("## {cmd}")),
            "{cmd} missing from markdown"
        );
        assert!(
            man.contains(&format!(".SH {}", cmd.to_uppercase()))
                || man.contains(&format!(".SS {cmd}")),
            "{cmd} missing from man page"
        );
        for flag in flags_of(&cmd) {
            // roff escapes `-` as `\-`.
            let roff = flag.replace('-', "\\-");
            assert!(md.contains(&flag), "{cmd} {flag} missing from markdown");
            assert!(man.contains(&roff), "{cmd} {flag} missing from man page");
        }
    }
}

#[test]
fn the_generator_is_hidden_from_the_usage() {
    assert!(!stdout(&["--help"]).contains("gen-docs"));
}

#[test]
fn gen_docs_rejects_an_unknown_format() {
    let out = run(&["gen-docs", "html"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("markdown") && err.contains("man"), "{err}");
}

#[test]
fn man_page_renders() {
    let man = stdout(&["gen-docs", "man"]);
    // A leading `.\"` comment is fine; the title macro must come right after.
    assert!(man.lines().take(3).any(|l| l.starts_with(".TH ")), "{man}");
    // `man` is not on every CI image; the structural check above still runs.
    let Ok(out) = Command::new("man")
        .args(["-l", "-"])
        .env("MANWIDTH", "80")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(man.as_bytes())?;
            c.wait_with_output()
        })
    else {
        eprintln!("man not installed: render check skipped");
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("insights") && text.contains("--llm-timeout-ms"),
        "{text}"
    );
    assert!(
        out.stderr.is_empty(),
        "man warnings: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
