//! Arvore e modelo de sessao sobre as fixtures estaticas.

use agentcenter_core::adapters::{AgentRole, ExtraAgent, Registry, parse_extra_agents};
use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::{ProcRoot, Sampler, Snapshot};
use std::path::Path;

fn fixture(name: &str) -> ProcRoot {
    ProcRoot::new(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/proc")
            .join(name),
    )
}

fn snap(name: &str, registry: Registry) -> Snapshot {
    Sampler::new(registry).tick(&fixture(name)).unwrap()
}

fn pids(snap: &Snapshot, agent_pid: u32) -> Vec<u32> {
    let s = snap
        .sessions
        .iter()
        .find(|s| s.pid == agent_pid)
        .expect("sessao");
    let mut v: Vec<u32> = s.processes.iter().map(|p| p.pid).collect();
    v.sort();
    v
}

#[test]
fn nested_agent_makes_two_sessions_children_go_to_nearest() {
    let s = snap("nested", Registry::default());
    let claudes: Vec<u32> = s
        .sessions
        .iter()
        .filter(|x| x.agent == "claude")
        .map(|x| x.pid)
        .collect();
    assert_eq!(claudes.len(), 2, "claude -> bash -> claude = 2 sessoes");
    // O claude externo fica com o bash intermediario e o MCP; o interno com o seu node.
    assert_eq!(pids(&s, 20), vec![20, 21, 22]);
    assert_eq!(pids(&s, 30), vec![30, 31]);
    let outer = s.sessions.iter().find(|x| x.pid == 20).unwrap();
    assert_eq!(outer.pss_bytes, Some((300_000 + 600 + 50_000) * 1024));
    assert_eq!(outer.swap_pss_bytes, Some((40_000 + 1000) * 1024));
    assert_eq!(outer.cwd.as_deref(), Some("/home/dev/app"));
    assert_eq!(outer.cmdline.as_deref(), Some("claude"));
}

#[test]
fn herdr_is_its_own_row_and_does_not_swallow_agents() {
    let s = snap("nested", Registry::default());
    let herdr = s
        .sessions
        .iter()
        .find(|x| x.agent == "herdr")
        .expect("linha herdr");
    assert_eq!(herdr.role, AgentRole::Multiplexer);
    assert_eq!(herdr.pid, 10);
    // Servidor + shells de painel sem agente; os claudes dos paineis nao.
    assert_eq!(pids(&s, 10), vec![10, 11, 12]);
}

#[test]
fn sessions_sorted_by_pss_plus_swap_desc_and_strays_left_out() {
    let s = snap("nested", Registry::default());
    let order: Vec<u32> = s.sessions.iter().map(|x| x.pid).collect();
    assert_eq!(order, vec![20, 30, 10]);
    // systemd, "(a b) c)" e fakeagent/sleep nao pertencem a sessao.
    assert_eq!(s.machine.processes, 12);
    assert_eq!(s.machine.session_processes, 8);
}

#[test]
fn extra_agents_from_list_add_sessions_by_comm() {
    assert_eq!(
        parse_extra_agents(" fakeagent, ,other,fakeagent ")
            .iter()
            .map(|e| e.comm.as_str())
            .collect::<Vec<_>>(),
        vec!["fakeagent", "other"]
    );
    let r = Registry::with_extra("fakeagent,claude");
    // claude ja e embutido: nao duplica.
    assert_eq!(
        r.ids(),
        vec![
            "claude",
            "codex",
            "pi",
            "herdr",
            "gemini",
            "copilot",
            "kiro",
            "opencode",
            "aider",
            "goose",
            "cursor-agent",
            "fakeagent"
        ]
    );
    let s = snap("nested", r);
    let fake = s
        .sessions
        .iter()
        .find(|x| x.agent == "fakeagent")
        .expect("agente extra");
    assert_eq!(pids(&s, fake.pid), vec![50, 51]);
    assert_eq!(fake.role, AgentRole::Agent);
}

#[test]
fn process_that_vanishes_mid_read_is_dropped_without_error() {
    let s = snap("vanished", Registry::default());
    assert_eq!(s.sessions.len(), 1);
    // 21 tinha stat mas perdeu status; 22 nem stat tinha.
    assert_eq!(pids(&s, 20), vec![20, 23]);
    assert_eq!(
        s.machine.processes, 4,
        "1, 20, 21 (stat lido) e 23; 22 sem stat"
    );
}

#[test]
fn codex_detected_by_exe_when_comm_is_a_variant() {
    let dir = TempDir::new("ac-codex").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    let mut native = FakeProcess::new(20, 1, "codex");
    native.exe = Some("/opt/codex/codex".into());
    let mut variant = FakeProcess::new(30, 1, "codex-x86_64-un");
    variant.exe = Some("/opt/vendor/codex".into());
    let mut other = FakeProcess::new(40, 1, "codexfmt");
    other.exe = Some("/usr/bin/codexfmt".into());
    for p in [&native, &variant, &other] {
        fp.add(p).unwrap();
    }
    let s = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    let mut codex: Vec<u32> = s
        .sessions
        .iter()
        .filter(|x| x.agent == "codex")
        .map(|x| x.pid)
        .collect();
    codex.sort();
    assert_eq!(codex, vec![20, 30]);
}

#[test]
fn memory_order_is_pss_plus_swap_pss_then_pid() {
    let dir = TempDir::new("ac-order").unwrap();
    let fp = FakeProc::create(dir.path()).unwrap();
    // (pid, PSS kB, SwapPss kB)
    for (pid, pss, swap) in [
        (20, 1000, 0),
        (30, 1000, 500),
        (40, 600, 1200),
        (50, 1500, 0),
    ] {
        let mut p = FakeProcess::new(pid, 1, "claude");
        p.pss_kb = pss;
        p.swap_pss_kb = swap;
        fp.add(&p).unwrap();
    }
    let s = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    let order: Vec<u32> = s.sessions.iter().map(|x| x.pid).collect();
    // 40: menos PSS, mais PSS+SwapPss, vem primeiro. 30 e 20: mesmo PSS, o swap
    // desempata. 30 e 50: mesma soma, pid menor primeiro.
    assert_eq!(order, vec![40, 30, 50, 20]);
    let cost: Vec<u64> = s.sessions.iter().map(|x| x.memory_cost()).collect();
    assert_eq!(
        cost,
        vec![1800 * 1024, 1500 * 1024, 1500 * 1024, 1000 * 1024]
    );
}

#[test]
fn process_gone_with_esrch_is_dropped_without_error() {
    // Zumbi real: stat e status ainda leem, smaps_rollup devolve ESRCH. Os dois
    // filhos tem o comm do link (nome unico); o vivo e o controle positivo.
    let dir = TempDir::new("ac-esrch").unwrap();
    let name = format!("acz{}", std::process::id());
    let exe = dir.path().join(&name);
    std::os::unix::fs::symlink("/bin/sh", &exe).unwrap();
    let spawn = |script: &str| {
        std::process::Command::new(&exe)
            .args(["-c", script])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut alive = spawn("read x");
    let mut zombie = spawn("exit 0");
    let zpid = zombie.id();
    let state = || {
        std::fs::read_to_string(format!("/proc/{zpid}/stat"))
            .ok()
            .and_then(|s| {
                s.rsplit_once(')')
                    .map(|(_, r)| r.trim_start().starts_with('Z'))
            })
            .unwrap_or(false)
    };
    let t0 = std::time::Instant::now();
    while !state() {
        assert!(t0.elapsed().as_secs() < 5, "filho nao virou zumbi");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let registry = Registry::new(vec![Box::new(ExtraAgent { comm: name.clone() })]);
    let snap = Sampler::new(registry).tick(&ProcRoot::default()).unwrap();
    let pids: Vec<u32> = snap.sessions.iter().map(|x| x.pid).collect();

    drop(alive.stdin.take());
    alive.wait().unwrap();
    zombie.wait().unwrap();
    assert!(
        pids.contains(&alive.id()),
        "controle: o vivo e sessao ({pids:?})"
    );
    assert!(!pids.contains(&zpid), "zumbi (ESRCH) descartado ({pids:?})");
}
