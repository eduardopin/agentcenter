//! `agentcenter-cli signal` com um agente falso de verdade: sem `--yes` e sem
//! terminal nada e enviado; com `--yes` o SIGTERM chega.

use agentcenter_core::fixture::TempDir;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_agentcenter-cli");

/// Copia um `sleep` para `dir/fake-agent`: o `comm` do processo vira
/// `fake-agent`. `/bin/sleep` do uutils (multicall) decide o programa pelo nome
/// do executavel e nao roda copiado, entao vale o primeiro que roda.
fn fake_agent(dir: &Path) -> PathBuf {
    let dst = dir.join("fake-agent");
    for src in ["/usr/bin/gnusleep", "/bin/sleep", "/usr/bin/sleep"] {
        if std::fs::copy(src, &dst).is_err() {
            continue;
        }
        let ok = Command::new(&dst)
            .arg("0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return dst;
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

fn spawn(path: &Path) -> Reap {
    // ETXTBSY (26): outro fork herdou o descritor da copia por um instante.
    for _ in 0..50 {
        match Command::new(path)
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => return Reap(c),
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("spawn fake-agent: {e}"),
        }
    }
    panic!("fake-agent: text file busy");
}

/// (estado, starttime) de `/proc/<pid>/stat`.
fn stat(pid: u32) -> Option<(char, String)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((f[0].chars().next()?, f[19].to_string()))
}

fn cli(args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("AGENTCENTER_EXTRA_AGENTS", "fake-agent")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stdin {
        Some(_) => cmd.stdin(Stdio::piped()),
        None => cmd.stdin(Stdio::null()),
    };
    let mut child = cmd.spawn().unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap()
}

#[test]
fn signal_term_session_needs_yes_without_a_terminal() {
    let dir = TempDir::new("ac-signal-cli").unwrap();
    let mut agent = spawn(&fake_agent(dir.path()));
    let pid = agent.0.id();
    let pid_s = pid.to_string();
    let (_, started) = stat(pid).expect("agente vivo");
    assert_eq!(
        std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap(),
        "fake-agent\n"
    );
    let args = ["signal", "term", "--session", pid_s.as_str()];

    // `echo n | ...`: stdin nao e terminal, nada e enviado.
    let out = cli(&args, Some(b"n\n"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("nothing sent"), "{err}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&pid_s) && stdout.contains("fake-agent"),
        "{stdout}"
    );
    // `< /dev/null`: recusa com exit != 0.
    let out = cli(&args, None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    std::thread::sleep(Duration::from_millis(200));
    // Mesmo processo, nem parado nem morto.
    let (state, now) = stat(pid).expect("agente sumiu");
    assert_eq!(now, started, "pid reutilizado");
    assert!(
        matches!(state, 'S' | 'R'),
        "o agente recebeu sinal: estado {state}"
    );

    // Recusa de seguranca chega ao usuario com o motivo.
    let out = cli(&["signal", "kill", "--pid", "1", "--yes"], None);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("pid 1"));

    // Controle: com --yes o SIGTERM chega.
    let out = cli(&["signal", "term", "--session", &pid_s, "--yes"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains(&format!("sent SIGTERM to {pid}")));
    let status = agent.0.wait().unwrap();
    assert_eq!(status.signal(), Some(15), "{status:?}");
}
