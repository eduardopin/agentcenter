//! `agentcenter-cli insights` ponta a ponta, sobre um `/proc` falso (`--proc-root`,
//! sem sessoes: o unico achado e `all_clear`) e, para o modelo, um servidor de
//! mentira em 127.0.0.1:0 que fala o minimo do `/v1/chat/completions`.

use agentcenter_core::fixture::{FakeProc, TempDir};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_agentcenter-cli");

struct World {
    // O TempDir apaga a raiz no Drop; o FakeProc so guarda o caminho.
    _dir: TempDir,
    proc: FakeProc,
}

fn world() -> World {
    let dir = TempDir::new("ac-insights-cli").unwrap();
    let proc = FakeProc::create(dir.path().join("proc")).unwrap();
    World { _dir: dir, proc }
}

impl World {
    /// O binario com ambiente so do mundo: nada do HOME real nem `AGENTCENTER_LLM`.
    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> (Output, Duration) {
        let t0 = Instant::now();
        let out = Command::new(BIN)
            .arg("insights")
            .args(["--proc-root", self.proc.root().to_str().unwrap()])
            .args(args)
            .env_clear()
            .env("HOME", self.proc.root())
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out, t0.elapsed())
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn json_of(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

/// Servidor local que responde uma vez; `reply: None` aceita a conexao e nao
/// responde (segura o socket ate `hold`). Devolve a URL e a thread.
fn stub(reply: Option<String>, hold: Duration) -> (String, JoinHandle<usize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let t = std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut seen = read_request(&mut s);
        match reply {
            Some(content) => {
                let body = json!({"choices": [{"message": {"content": content}}]}).to_string();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            None => std::thread::sleep(hold),
        }
        seen += 1;
        seen
    });
    (url, t)
}

/// Le cabecalho e corpo do POST (Content-Length) e devolve 0.
fn read_request(s: &mut TcpStream) -> usize {
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = s.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            return 0;
        }
        buf.extend_from_slice(&tmp[..n]);
        let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..h]).to_lowercase();
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        if buf.len() >= h + 4 + len {
            return 0;
        }
    }
}

/// O texto do primeiro achado numa corrida so de regras.
fn first_finding_text(w: &World) -> String {
    let (out, _) = w.cli(&["--json"], &[]);
    json_of(&out)["findings"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn default_run_prints_the_rule_briefing_then_the_numbered_findings() {
    let w = world();
    let (out, _) = w.cli(&[], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("sessions are running"), "{text}");
    assert!(lines.contains(&"briefing: rules"), "{text}");
    assert!(lines.iter().any(|l| l.starts_with("1. [info] ")), "{text}");
    assert!(!text.contains('\x1b'), "no colour: {text}");
}

#[test]
fn json_has_the_schema_and_the_rules_briefing() {
    let w = world();
    let (out, _) = w.cli(&["--json"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json_of(&out);
    assert_eq!(v["schema_version"], 1);
    assert!(v["taken_at_ms"].as_u64().unwrap() > 0);
    assert_eq!(v["briefing"]["source"], "rules");
    assert_eq!(v["briefing"]["rejected"], Value::Null);
    assert!(!v["briefing"]["text"].as_str().unwrap().is_empty());
    assert_eq!(v["findings"][0]["id"], "all_clear");
    assert_eq!(v["findings"][0]["severity"], "info");
}

#[test]
fn a_non_loopback_endpoint_is_a_usage_error_before_any_sampling() {
    let w = world();
    let (out, took) = w.cli(&["--llm", "http://10.0.0.5:8080"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("loopback"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    // Recusada antes de amostrar (a amostragem sozinha leva mais de 1 s).
    assert!(took < Duration::from_millis(900), "{took:?}");

    // Idem pelo ambiente.
    let (out, _) = w.cli(&[], &[("AGENTCENTER_LLM", "http://10.0.0.5:8080")]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("loopback"), "{}", stderr(&out));
}

#[test]
fn an_answer_with_an_invented_number_shows_the_rule_text_and_the_reason() {
    let w = world();
    let rules = first_finding_text(&w);
    let (url, srv) = stub(
        Some("Seven sessions are past 98% of their context, restart them now please.".into()),
        Duration::ZERO,
    );
    let (out, _) = w.cli(&["--llm", &url], &[]);
    assert_eq!(srv.join().unwrap(), 1);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with(&rules), "{text}");
    assert!(
        text.contains("briefing: rules (model answer refused: number not in findings: 98)"),
        "{text}"
    );

    let (url, srv) = stub(
        Some("Seven sessions are past 98% of their context, restart them now please.".into()),
        Duration::ZERO,
    );
    let (out, _) = w.cli(&["--json", "--llm", &url], &[]);
    srv.join().unwrap();
    let v = json_of(&out);
    assert_eq!(v["briefing"]["source"], "rules");
    assert_eq!(v["briefing"]["text"], rules);
    assert_eq!(v["briefing"]["rejected"], "number not in findings: 98");
}

#[test]
fn a_valid_model_sentence_is_shown_as_the_model_briefing() {
    let w = world();
    let sentence = first_finding_text(&w);
    let (url, srv) = stub(Some(sentence.clone()), Duration::ZERO);
    let (out, _) = w.cli(&["--llm", &url], &[]);
    srv.join().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with(&sentence), "{text}");
    assert!(text.lines().any(|l| l == "briefing: model"), "{text}");
    assert!(!text.contains("refused"), "{text}");

    // O mesmo endereco pelo ambiente.
    let (url, srv) = stub(Some(sentence.clone()), Duration::ZERO);
    let (out, _) = w.cli(&["--json"], &[("AGENTCENTER_LLM", &url)]);
    srv.join().unwrap();
    let v = json_of(&out);
    assert_eq!(v["briefing"]["source"], "model");
    assert_eq!(v["briefing"]["text"], sentence);
}

#[test]
fn a_model_that_never_answers_falls_back_within_the_llm_timeout() {
    let w = world();
    let (url, srv) = stub(None, Duration::from_secs(6));
    let (base, base_took) = w.cli(&[], &[]);
    assert!(base.status.success());
    let (out, took) = w.cli(&["--llm", &url, "--llm-timeout-ms", "300"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("briefing: rules (model answer refused: timed out)"),
        "{text}"
    );
    // Amostragem + 300 ms, bem antes dos 6 s que o servidor segura.
    assert!(
        took < base_took + Duration::from_secs(3),
        "{took:?} vs {base_took:?}"
    );
    srv.join().unwrap();
}

#[test]
fn help_states_the_model_boundaries_and_the_usage_lists_the_command() {
    let out = Command::new(BIN)
        .args(["insights", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = stdout(&out);
    for opt in [
        "--json",
        "--llm URL",
        "--llm-timeout-ms N",
        "--timeout-ms N",
        "--proc-root PATH",
        "AGENTCENTER_LLM",
    ] {
        assert!(help.contains(opt), "{opt}: {help}");
    }
    // O help quebra linha no meio das frases.
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("never sees transcript content or session names"),
        "{help}"
    );
    assert!(
        help.contains("snapshot") && help.contains("watch") && help.contains("GUI"),
        "{help}"
    );

    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(stdout(&out).contains("  insights "), "{}", stdout(&out));
    let out = Command::new(BIN)
        .args(["insights", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
