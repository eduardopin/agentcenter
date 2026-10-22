//! CC6: depois de um handoff-pickup, o transcript NOVO tem o conteudo do
//! handoff (`toolUseResult.file.content` do `Read` da copia, texto da skill) e
//! o ANTIGO tem o do `Write` (`input.content`, `toolUseResult.content`). O
//! `snapshot` (JSON e tabela) e o `cockpit` rodam os overlays de verdade
//! (`Registry::from_env`) sobre os dois, com dois `claude` falsos vivos; nada
//! do conteudo pode sair no stdout nem no stderr, que e o log da CLI.
//!
//! Transcripts: as fixtures de `agentcenter-core/tests/fixtures/claude_pickup`
//! com `AGENTCENTER-CANARY-cc6` em todo campo de conteudo, mais uma linha de
//! `usage` para o cockpit ter contexto. Nada aqui fala com herdr ou Claude.

use agentcenter_core::fixture::TempDir;
use agentcenter_core::overlay::transcript::civil_from_days;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_agentcenter-cli");
const CANARY: &str = "AGENTCENTER-CANARY-cc6";
const OLD: &str = "11111111-2222-4333-8444-555555555555";
const NEW: &str = "22222222-2222-4333-8444-555555555555";
const OP: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agentcenter-core/tests/fixtures/claude_pickup")
        .join(name);
    fs::read_to_string(p).unwrap()
}

fn iso(ms: u64) -> String {
    let (s, ms) = (ms / 1000, ms % 1000);
    let (y, mo, d) = civil_from_days((s / 86400) as i64);
    let t = s % 86400;
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// One assistant entry with usage (and the canary in its text) so the cockpit
/// has a context figure, then the fixture with its placeholders filled: the
/// content-bearing entries come last, where a last-wins field would keep them.
fn transcript(name: &str, sid: &str, path: &Path, read: &Path) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let mut text = fixture(name);
    for n in 0..4 {
        text = text.replace(&format!("@TS{n}@"), &iso(now + 1 + n));
    }
    let text = text
        .replace("@SID@", sid)
        .replace("@OP@", OP)
        .replace("@ID@", &OP[..8])
        .replace("@PATH@", path.to_str().unwrap())
        .replace("@READ@", read.to_str().unwrap());
    format!(
        "{{\"type\":\"assistant\",\"sessionId\":\"{sid}\",\"timestamp\":\"{}\",\
         \"requestId\":\"req_{}\",\"message\":{{\"model\":\"claude-opus-5-5\",\
         \"content\":[{{\"type\":\"text\",\"text\":\"{CANARY} resumo\"}}],\
         \"usage\":{{\"input_tokens\":1000,\"cache_creation_input_tokens\":0,\
         \"cache_read_input_tokens\":20000,\"output_tokens\":50}}}}}}\n{text}",
        iso(now),
        &sid[..8]
    )
}

/// `sleep` copied as `claude` (its `comm`), as in `tests/cockpit.rs`.
fn fake_claude(dir: &Path) -> PathBuf {
    let dst = dir.join("claude");
    for src in ["/usr/bin/gnusleep", "/bin/sleep", "/usr/bin/sleep"] {
        if fs::copy(src, &dst).is_err() {
            continue;
        }
        for _ in 0..50 {
            match Command::new(&dst)
                .arg("0")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(s) if s.success() => return dst,
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => break,
            }
        }
    }
    panic!("no sleep binary that runs under another name");
}

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(exe: &Path, cwd: &Path) -> Reap {
    for _ in 0..50 {
        match Command::new(exe)
            .arg("300")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => return Reap(c),
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("spawn {}: {e}", exe.display()),
        }
    }
    panic!("{}: text file busy", exe.display());
}

fn starttime(pid: u32) -> String {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    rest.split_whitespace().nth(19).unwrap().to_string()
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CLAUDE_CONFIG_DIR", root.join("home/.claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("PI_CODING_AGENT_DIR", root.join("pi"))
        .env("HERDR_SOCKET_PATH", root.join("no-herdr.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn pickup_transcripts_never_put_handoff_content_in_snapshot_or_stderr() {
    let dir = TempDir::new("ac-cli-pickup").unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let repo = root.join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let home = root.join("home");
    // The handoff the old session wrote and the pickup's private copy, both
    // with the canary; the cockpit may name a handoff, never read it out.
    let original = repo.join(".claude/handoffs/20261022-120000-cc6.md");
    fs::create_dir_all(original.parent().unwrap()).unwrap();
    fs::write(&original, format!("# {CANARY}\n{CANARY} handoff body\n")).unwrap();
    let copy = home.join(format!(
        ".local/state/agentcenter/handoffs/s{}-{OP}.md",
        OLD.bytes().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::copy(&original, &copy).unwrap();

    let projects = home.join(".claude/projects/-repo");
    fs::create_dir_all(&projects).unwrap();
    fs::write(
        projects.join(format!("{OLD}.jsonl")),
        transcript("handoff.jsonl", OLD, &original, &original),
    )
    .unwrap();
    fs::write(
        projects.join(format!("{NEW}.jsonl")),
        transcript("pickup.jsonl", NEW, &copy, &copy),
    )
    .unwrap();
    for f in [
        projects.join(format!("{OLD}.jsonl")),
        projects.join(format!("{NEW}.jsonl")),
    ] {
        let text = fs::read_to_string(&f).unwrap();
        assert!(text.matches(CANARY).count() >= 3, "{f:?}");
    }

    // The process that went through /clear is now on NEW; another one still
    // runs the old session (a manual `claude --resume`).
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = fake_claude(&bin);
    let sessions = home.join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let mut agents = Vec::new();
    for sid in [NEW, OLD] {
        let a = spawn(&exe, &repo);
        let pid = a.0.id();
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!(
                "{{\"pid\":{pid},\"sessionId\":\"{sid}\",\"procStart\":\"{}\",\
                 \"cwd\":\"{}\",\"status\":\"idle\"}}",
                starttime(pid),
                repo.display()
            ),
        )
        .unwrap();
        agents.push((sid, pid, a));
    }

    let outputs = [
        run(&root, &["snapshot", "--json", "--interval-ms", "50"]),
        run(&root, &["snapshot", "--interval-ms", "50"]),
        run(&root, &["cockpit", "--all", "--timeout-ms", "3000"]),
        run(&root, &["cockpit", "--json", "--timeout-ms", "3000"]),
    ];
    for o in &outputs {
        assert!(o.status.success(), "{o:?}");
        assert!(!o.stdout.is_empty());
        for (name, bytes) in [("stdout", &o.stdout), ("stderr", &o.stderr)] {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(CANARY), "{name}: {text}");
            assert!(!text.contains("handoff body"), "{name}: {text}");
            assert!(!text.contains("skill text"), "{name}: {text}");
        }
    }
    // The overlays did read both transcripts: one tool call, closed, and the
    // usage line's model and context, for each session.
    let snap: Value = serde_json::from_slice(&outputs[0].stdout).unwrap();
    let table = String::from_utf8_lossy(&outputs[1].stdout);
    for (sid, pid, _) in &agents {
        let s = snap["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["pid"].as_u64() == Some(u64::from(*pid)))
            .unwrap_or_else(|| panic!("{sid}: {pid} not in the snapshot"));
        let o = &s["overlay"];
        assert_eq!(o["agent_session_id"], *sid, "{o}");
        assert_eq!(o["tool_calls"], 1, "{sid}: {o}");
        assert_eq!(o["tool_calls_open"], 0, "{sid}: {o}");
        assert_eq!(o["model"], "claude-opus-5-5", "{sid}: {o}");
        assert!(o["context_pct"].as_f64().is_some(), "{sid}: {o}");
        assert!(o["cockpit"].is_object(), "{sid}: {o}");
        assert!(table.contains(&pid.to_string()), "{table}");
    }
}
