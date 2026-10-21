//! `agentcenter-cli cockpit` ponta a ponta: tres agentes falsos de verdade
//! (`claude`, `codex` e `pi`, um `sleep` copiado com esse nome) e as
//! fixtures de transcript do core, sob raizes temporarias.
//!
//! O `/proc` e o real, nao o `--proc-root`: os overlays (estado do Claude,
//! fd aberto do Codex e do pi) leem o `/proc` do sistema, entao uma arvore
//! falsa nao casaria transcript nenhum. O codex e o pi abrem o transcript no
//! stdin (casam pelo fd); o claude casa pelo `sessions/<pid>.json` com o
//! `procStart` dele.
//!
//! Valores esperados: os de referencia do C21 (`contexto.py` sobre as mesmas
//! fixtures, `crates/agentcenter-core/tests/transcripts.rs`,
//! `expected_cockpit`).

use agentcenter_core::fixture::TempDir;
use serde_json::Value;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_agentcenter-cli");
const SID: &str = "0b6f4a52-9c1e-4d7a-8f3b-2a1c5e7d9f01";
const CODEX: &str =
    "2026/10/15/rollout-2026-10-15T01-14-06-01a13dc4-6ff3-7fb2-a708-0d87a9e28591.jsonl";
const PI: &str =
    "--home-dev-app--/2026-10-17T02-53-28-810Z_01a147c7-5769-739f-8d7a-6e1addc5ac7d.jsonl";
/// Nome do claude com ESC e quebra de linha (JSON escapado no arquivo).
const EVIL_JSON: &str = r"app\u001b[2J\nTOTAL fake";
const EVIL_SHOWN: &str = r"app\x1b[2J\nTOTAL fake";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agentcenter-core/tests/fixtures/transcripts")
}

/// Copia um `sleep` para `dir/name`: o `comm` do processo vira `name`. O
/// `/bin/sleep` do uutils (multicall) nao roda copiado, entao vale o primeiro
/// que roda (como em `tests/signal.rs`).
fn fake_agent(dir: &Path, name: &str) -> PathBuf {
    let dst = dir.join(name);
    for src in ["/usr/bin/gnusleep", "/bin/sleep", "/usr/bin/sleep"] {
        if fs::copy(src, &dst).is_err() {
            continue;
        }
        // ETXTBSY (26): o fork de outro teste herdou o descritor da copia.
        for _ in 0..50 {
            match Command::new(&dst)
                .arg("0")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(s) if s.success() => return dst,
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
    }
    panic!("no sleep binary that runs under another name");
}

/// Mata e colhe o agente no `Drop`, inclusive quando o teste falha.
struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `exe` em `cwd`, com `stdin` aberto (o transcript, para casar pelo fd).
fn spawn(exe: &Path, cwd: &Path, stdin: Option<&Path>) -> Reap {
    // ETXTBSY (26): outro fork herdou o descritor da copia por um instante.
    for _ in 0..50 {
        let input = match stdin {
            Some(p) => Stdio::from(File::open(p).unwrap()),
            None => Stdio::null(),
        };
        match Command::new(exe)
            .arg("300")
            .current_dir(cwd)
            .stdin(input)
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

/// starttime (campo 22 de `/proc/<pid>/stat`).
fn starttime(pid: u32) -> String {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    rest.split_whitespace().nth(19).unwrap().to_string()
}

struct World {
    dir: TempDir,
    agents: Vec<(&'static str, u32, PathBuf, Reap)>,
}

impl World {
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// O binario com o ambiente so do mundo (nada do HOME real, nem herdr).
    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> (Output, Duration) {
        let root = self.dir.path();
        let mut cmd = Command::new(BIN);
        cmd.arg("cockpit")
            .args(args)
            .env_clear()
            .env("HOME", self.home())
            .env("CLAUDE_CONFIG_DIR", root.join("claude"))
            .env("CODEX_HOME", root.join("codex"))
            .env("PI_CODING_AGENT_DIR", root.join("pi"))
            .envs(env.iter().copied())
            .stdin(Stdio::null());
        let t0 = Instant::now();
        let out = cmd.output().unwrap();
        (out, t0.elapsed())
    }
}

/// Um agente de cada; `which` escolhe quais. Transcripts copiados (mtime de
/// agora: sem ocio), cada agente no seu cwd.
fn world(which: &[&'static str]) -> World {
    let dir = TempDir::new("ac-cockpit-cli").unwrap();
    let root = dir.path().to_path_buf();
    let data = fixtures();
    // Raiz de repo propria: a busca de handoff para aqui e nao acha os
    // .claude/handoffs de um repo acima do TMPDIR (o checkout principal tem).
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let mut agents = Vec::new();
    for &name in which {
        let cwd = root.join(format!("work-{name}"));
        fs::create_dir_all(&cwd).unwrap();
        let exe = fake_agent(&bin, name);
        let agent = match name {
            "claude" => {
                // O claude acha o transcript pelo sessionId em qualquer projeto.
                let t = root
                    .join("claude/projects/-elsewhere")
                    .join(format!("{SID}.jsonl"));
                fs::create_dir_all(t.parent().unwrap()).unwrap();
                fs::copy(
                    data.join("claude/-home-dev-app")
                        .join(format!("{SID}.jsonl")),
                    &t,
                )
                .unwrap();
                let a = spawn(&exe, &cwd, None);
                let pid = a.0.id();
                let sessions = root.join("claude/sessions");
                fs::create_dir_all(&sessions).unwrap();
                fs::write(
                    sessions.join(format!("{pid}.json")),
                    format!(
                        "{{\"pid\":{pid},\"sessionId\":\"{SID}\",\"procStart\":\"{}\",\
                         \"name\":\"{EVIL_JSON}\",\"status\":\"idle\"}}",
                        starttime(pid)
                    ),
                )
                .unwrap();
                a
            }
            "codex" | "pi" => {
                let (sub, rel) = if name == "codex" {
                    ("codex/sessions", CODEX)
                } else {
                    ("pi/sessions", PI)
                };
                let t = root.join(sub).join(rel);
                fs::create_dir_all(t.parent().unwrap()).unwrap();
                fs::copy(data.join(name).join(rel), &t).unwrap();
                spawn(&exe, &cwd, Some(&t))
            }
            other => panic!("agente {other}"),
        };
        let pid = agent.0.id();
        agents.push((name, pid, cwd, agent));
    }
    World { dir, agents }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// (REC, CTX, BURN, CACHE, RELEI, PBACK) do C21 para cada fixture.
fn expected_cells(agent: &str) -> [&'static str; 6] {
    match agent {
        "claude" => [
            "CONTINUAR",
            "/1.0M~ 12%",
            "92.8k 0.5%",
            "75%",
            "17%",
            "7.3t",
        ],
        "codex" => [
            "CONTINUAR",
            "/258.4k 16%",
            "37.6k 0.2%",
            "72%",
            "14%",
            "240t",
        ],
        "pi" => ["CONTINUAR", "/128.0k~ 16%", "31.7k 0.2%", "47%", "6%", "-"],
        a => panic!("{a}"),
    }
}

/// (window, burn_weighted, reread_share, payback, cache_hit, burn_pct) do C21.
fn expected_json(agent: &str) -> (u64, u64, f64, Option<f64>, f64, f64) {
    match agent {
        "claude" => (1_000_000, 92_811, 0.166, Some(7.3), 0.751, 0.5),
        "codex" => (258_400, 37_575, 0.138, Some(240.0), 0.717, 0.2),
        "pi" => (128_000, 31_687, 0.057, None, 0.467, 0.2),
        a => panic!("{a}"),
    }
}

#[test]
fn cockpit_table_and_json_match_the_c21_reference_values() {
    let w = world(&["claude", "codex", "pi"]);

    let (out, _) = w.cli(&["--json", "--all"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let snap: Value = serde_json::from_slice(&out.stdout).unwrap();
    for (agent, pid, _, _) in &w.agents {
        let s = snap["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["pid"].as_u64() == Some(u64::from(*pid)))
            .unwrap_or_else(|| panic!("{agent} {pid} fora do snapshot"));
        let c = &s["overlay"]["cockpit"];
        let (window, burn, share, payback, hit, burn_pct) = expected_json(agent);
        assert_eq!(c["window"].as_u64(), Some(window), "{agent}: {c}");
        assert_eq!(c["burn_weighted"].as_u64(), Some(burn), "{agent}");
        assert_eq!(c["reread_share"].as_f64(), Some(share), "{agent}");
        assert_eq!(c["payback_turns"].as_f64(), payback, "{agent}");
        assert_eq!(c["cache_hit"].as_f64(), Some(hit), "{agent}");
        assert_eq!(c["burn_pct"].as_f64(), Some(burn_pct), "{agent}");
        assert_eq!(c["recommendation"], "CONTINUAR", "{agent}");
    }
    // As tres e so elas tem cockpit (o HOME real nao entra).
    let sum = &snap["summary"];
    assert_eq!(sum["cockpit"]["continuar"], 3, "{sum}");
    assert_eq!(sum["cockpit_pending"], 0, "{sum}");
    assert_eq!(sum["sessions_burn_weighted"], 92_811 + 37_575 + 31_687);
    let cache = sum["cache_hit"].as_f64().unwrap();

    let (out, _) = w.cli(&[], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "byte de controle na saida: {text:?}"
    );
    let first = text.lines().next().unwrap();
    // Outros `claude` da maquina, sem transcript nas raizes do teste, entram
    // como "sem dado" depois das ociosas.
    assert!(
        first.starts_with("0 handoff, 0 compactar, 3 continuar, 0 ociosas"),
        "{first}"
    );
    assert!(
        first.ends_with(&format!("| burn 162.1k | cache {:.0}%", cache * 100.0)),
        "{first}"
    );
    let header: Vec<&str> = text.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(
        header,
        [
            "REC", "AGENT", "PROVIDER", "MODEL", "NAME", "CTX", "BURN", "CACHE", "RELEI", "PBACK",
            "HANDOFF", "IDLE", "CWD"
        ]
    );
    for (agent, _, cwd, _) in &w.agents {
        let cwd = cwd.to_str().unwrap();
        let line = text
            .lines()
            .find(|l| l.ends_with(cwd))
            .unwrap_or_else(|| panic!("{agent}: linha com {cwd}\n{text}"));
        let [rec, ctx, burn, cache, relei, pback] = expected_cells(agent);
        assert!(line.starts_with(rec), "{agent}: {line}");
        assert!(line.contains(ctx), "{agent}: {ctx} em {line}");
        for cell in [burn, cache, relei, pback] {
            assert!(
                line.contains(&format!(" {cell} ")),
                "{agent}: {cell} em {line}"
            );
        }
        // Sem handoff no mundo do teste; ocio de agora.
        assert!(
            line.contains(" -  ") && line.contains(" 0m  "),
            "{agent}: {line}"
        );
    }
    // O nome do claude (ESC e quebra de linha) sai escapado, numa linha so.
    let claude = text
        .lines()
        .find(|l| l.contains(EVIL_SHOWN))
        .expect("nome do claude");
    assert!(
        claude.contains("claude  anthropic  claude-opus-5-5"),
        "{claude}"
    );
}

#[test]
fn cockpit_times_out_with_what_it_has_and_says_how_many_are_reading() {
    let w = world(&["claude"]);
    let (out, took) = w.cli(
        &["--timeout-ms", "1500"],
        &[("AGENTCENTER_COCKPIT_READ_BUDGET", "1")],
    );
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(took <= Duration::from_millis(1500 + 1000), "{took:?}");
    assert!(
        err.contains("1 sessao ainda lendo o transcript apos 1500 ms"),
        "{err}"
    );
    let text = stdout(&out);
    let cwd = w.agents[0].2.to_str().unwrap().to_string();
    let line = text.lines().find(|l| l.ends_with(&cwd)).expect(&text);
    assert!(line.starts_with("lendo"), "{line}");
    assert!(text.lines().next().unwrap().contains("(1 lendo)"), "{text}");

    // Controle: sem o teto, o mesmo mundo sai sem aviso e com o cockpit.
    let (out, _) = w.cli(&["--timeout-ms", "1500"], &[]);
    assert_eq!(stderr(&out), "");
    assert!(stdout(&out).contains("CONTINUAR"), "{}", stdout(&out));
}

#[test]
fn cockpit_help_lists_the_options_and_what_is_not_measured() {
    let out = Command::new(BIN)
        .args(["cockpit", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = stdout(&out);
    for opt in ["--json", "--all", "--timeout-ms N", "--proc-root PATH"] {
        assert!(help.contains(opt), "{opt}: {help}");
    }
    assert!(help.contains("Not measured here"), "{help}");
    assert!(
        help.contains("LOOP") && help.contains("re-read many times"),
        "{help}"
    );

    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(stdout(&out).contains("  cockpit "), "{}", stdout(&out));
    let out = Command::new(BIN)
        .args(["cockpit", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
