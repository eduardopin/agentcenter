//! `agentcenter-cli reclaim` com um agente falso de verdade (um `sleep`
//! renomeado, CPU ~0): aparece como ocioso por CPU, o total bate com as linhas
//! e o agente continua vivo e sem sinal.

use agentcenter_core::fixture::TempDir;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_agentcenter-cli");

/// Copia um `sleep` que rode com outro nome (ver `tests/signal.rs`).
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

#[test]
fn reclaim_lists_the_idle_fake_agent_and_never_signals_it() {
    let dir = TempDir::new("ac-reclaim-cli").unwrap();
    let mut agent = spawn(&fake_agent(dir.path()));
    let pid = agent.0.id();

    let out = Command::new(BIN)
        .args(["reclaim", "--json", "--window-s", "1"])
        .env("AGENTCENTER_EXTRA_AGENTS", "fake-agent")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["window_s"], 1);
    let rows = v["sessions"].as_array().unwrap();
    let sum: u64 = rows
        .iter()
        .map(|r| r["pss_bytes"].as_u64().unwrap() + r["swap_pss_bytes"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(v["total_bytes"].as_u64(), Some(sum), "{v}");
    for r in rows {
        assert_ne!(r["overlay"]["status"], "busy", "{r}");
        assert_ne!(r["overlay"]["status"], "blocked", "{r}");
    }
    let mine = rows
        .iter()
        .find(|r| r["pid"] == pid)
        .unwrap_or_else(|| panic!("fake-agent {pid} fora da lista: {v}"));
    assert_eq!(mine["agent"], "fake-agent");
    assert_eq!(mine["idle_by"], "cpu");
    assert!(mine["max_cpu_pct"].as_f64().unwrap() < 1.0, "{mine}");
    assert_eq!(
        mine["command"],
        format!("agentcenter-cli signal term --session {pid}")
    );

    // Somente leitura: o agente segue vivo e nao parado.
    assert_eq!(agent.0.try_wait().unwrap(), None, "o agente saiu");
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let state = stat[stat.rfind(')').unwrap() + 2..].chars().next().unwrap();
    assert!(matches!(state, 'S' | 'R'), "estado {state}");
}
