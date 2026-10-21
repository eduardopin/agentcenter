//! Overlay do herdr contra um servidor falso que responde com as respostas
//! gravadas de `tests/fixtures/herdr` e um `/proc` falso. O socket do teste e
//! abstrato (sem caminho no disco), entao o herdr desta maquina nao entra.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::overlay::herdr::{Herdr, Timing, parse_agent_list, parse_process_info};
use agentcenter_core::overlay::{HerdrOverlay, Overlay, SessionOverlay};
use agentcenter_core::{Registry, Sampler, Snapshot};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CANARY: &str = "AGENTCENTER-CANARY-7f3a9c";

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/herdr")
        .join(rel)
}

/// Le a fixture e devolve numa linha so, como o servidor manda.
fn line(rel: &str) -> String {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(fixture(rel)).unwrap()).unwrap();
    v.to_string()
}

#[derive(Default)]
struct ServerState {
    calls: Mutex<HashMap<String, u32>>,
    subscribers: Mutex<Vec<UnixStream>>,
    /// Conexoes de assinatura ainda abertas pelo cliente.
    open_subs: AtomicU32,
}

/// Servidor falso: uma requisicao por conexao, como o herdr; a assinatura
/// fica aberta e recebe o que [`FakeServer::emit`] mandar.
struct FakeServer {
    addr: SocketAddr,
    state: Arc<ServerState>,
}

impl FakeServer {
    fn start(tag: &str) -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "agentcenter-test-herdr-{}-{tag}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&addr).unwrap();
        let state = Arc::new(ServerState::default());
        let st = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { return };
                let st = st.clone();
                std::thread::spawn(move || serve(conn, &st));
            }
        });
        Self { addr, state }
    }

    fn calls(&self, method: &str) -> u32 {
        *self.state.calls.lock().unwrap().get(method).unwrap_or(&0)
    }

    fn emit(&self, event: &str) {
        for s in self.state.subscribers.lock().unwrap().iter_mut() {
            let _ = s.write_all(format!("{event}\n").as_bytes());
        }
    }
}

fn serve(conn: UnixStream, st: &ServerState) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut req = String::new();
    if r.read_line(&mut req).unwrap_or(0) == 0 {
        return;
    }
    let req: Value = serde_json::from_str(&req).unwrap();
    let method = req["method"].as_str().unwrap().to_string();
    *st.calls.lock().unwrap().entry(method.clone()).or_default() += 1;
    let mut w = conn;
    let reply = match method.as_str() {
        "agent.list" => line("agent_list.json"),
        "pane.process_info" => {
            let pane = req["params"]["pane_id"].as_str().unwrap().replace(':', "-");
            let f = format!("process_info/{pane}.json");
            if fixture(&f).exists() {
                line(&f)
            } else {
                line("process_info_not_found.json")
            }
        }
        "events.subscribe" => {
            w.write_all(b"{\"id\":\"s\",\"result\":{\"type\":\"subscription_started\"}}\n")
                .unwrap();
            st.subscribers.lock().unwrap().push(w.try_clone().unwrap());
            st.open_subs.fetch_add(1, Ordering::SeqCst);
            // Bloqueia ate o cliente fechar a conexao.
            let mut sink = String::new();
            while r.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
            st.open_subs.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        _ => r#"{"id":"","error":{"code":"invalid_request","message":"x"}}"#.to_string(),
    };
    let _ = w.write_all(format!("{reply}\n").as_bytes());
}

fn fast() -> Timing {
    Timing {
        period: Duration::from_secs(60),
        debounce: Duration::from_millis(20),
        wait: Duration::from_millis(20),
        io_timeout: Duration::from_secs(2),
    }
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timeout: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `/proc` do cenario de `tests/fixtures/herdr/notes.txt`.
fn proc_world(dir: &TempDir) -> FakeProc {
    let fp = FakeProc::create(dir.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    for (pid, ppid, comm) in [
        (4000, 1, "herdr"),
        (4100, 4000, "bash"),
        (4101, 4100, "claude"),
        (4102, 4101, "node"),
        (4103, 4101, "codex"),
        (4200, 4000, "bash"),
        (4201, 4200, "pi"),
        (4300, 4000, "bash"),
        (4400, 1, "claude"),
    ] {
        fp.add(&FakeProcess::new(pid, ppid, comm)).unwrap();
    }
    fp
}

/// Um tick so com o overlay sob teste (o do registry e descartado).
fn tick(fp: &FakeProc, h: &Herdr) -> Snapshot {
    let mut snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    for s in &mut snap.sessions {
        let mut o = SessionOverlay::default();
        h.enrich(s, &mut o);
        s.overlay = (o != SessionOverlay::default()).then_some(o);
    }
    snap
}

fn herdr_of(snap: &Snapshot, pid: u32) -> Option<&HerdrOverlay> {
    snap.sessions
        .iter()
        .find(|s| s.pid == pid)
        .unwrap_or_else(|| panic!("sessao {pid}"))
        .overlay
        .as_ref()?
        .herdr
        .as_ref()
}

fn ready(fp: &FakeProc, srv: &FakeServer) -> Herdr {
    let h = Herdr::with_timing(Some(srv.addr.clone()), fp.proc_root(), fast());
    h.start();
    wait_until("primeiro indice", || h.generation() > 0);
    h
}

#[test]
fn herdr_recorded_responses_tolerate_unknown_fields() {
    // A fixture tem campos que o overlay nao conhece; se sumirem, o teste perde o sentido.
    let list = line("agent_list.json");
    assert!(list.contains("future_field") && list.contains("state_change_seq"));
    let agents = parse_agent_list(list.as_bytes()).unwrap();
    assert_eq!(agents.len(), 4);
    assert_eq!(agents[0].title.as_deref(), Some("Refatorar o parser"));
    assert_eq!(agents[0].status.as_deref(), Some("working"));
    let info = line("process_info/w1-p1.json");
    assert!(info.contains("future_field") && info.contains("argv0"));
    let p = parse_process_info(info.as_bytes()).unwrap();
    assert_eq!((p.shell_pid, p.leader), (Some(4100), Some(4101)));
    assert_eq!(p.pids, vec![4101, 4102, 4103]);
    assert_eq!(
        parse_process_info(line("process_info_not_found.json").as_bytes()),
        None
    );
}

#[test]
fn herdr_matches_sessions_by_pid_from_recorded_responses() {
    let t = TempDir::new("ac-herdr-match").unwrap();
    let fp = proc_world(&t);
    let srv = FakeServer::start("match");
    let h = ready(&fp, &srv);
    let snap = tick(&fp, &h);

    assert_eq!(
        herdr_of(&snap, 4101),
        Some(&HerdrOverlay {
            workspace_id: Some("w1".into()),
            tab_id: Some("w1:t1".into()),
            pane_id: Some("w1:p1".into()),
            title: Some("Refatorar o parser".into()),
            status: Some("working".into()),
        })
    );
    let pi = herdr_of(&snap, 4201).expect("pi no pane w1:p2");
    assert_eq!(pi.pane_id.as_deref(), Some("w1:p2"));
    assert_eq!(pi.tab_id.as_deref(), Some("w1:t2"));
    assert_eq!(pi.status.as_deref(), Some("idle"));
    // codex aberto pelo claude: esta entre os processos do pane, mas o pane e do claude.
    assert_eq!(herdr_of(&snap, 4103), None);
    // claude fora do herdr e a linha do proprio herdr.
    assert_eq!(herdr_of(&snap, 4400), None);
    assert_eq!(herdr_of(&snap, 4000), None);
    let with_pane = snap
        .sessions
        .iter()
        .filter(|s| s.overlay.as_ref().is_some_and(|o| o.herdr.is_some()))
        .count();
    // 4 agentes no herdr: w2:p3 sem processo vivo, w2:p5 fechou no meio.
    assert_eq!(with_pane, 2);
    assert_eq!(srv.calls("agent.list"), 1);
    assert_eq!(srv.calls("pane.process_info"), 4);
}

#[test]
fn herdr_private_fields_never_reach_the_snapshot() {
    let t = TempDir::new("ac-herdr-canary").unwrap();
    let fp = proc_world(&t);
    let srv = FakeServer::start("canary");
    let h = ready(&fp, &srv);
    let snap = tick(&fp, &h);
    assert!(herdr_of(&snap, 4101).is_some(), "overlay ran");
    assert!(!serde_json::to_string(&snap).unwrap().contains(CANARY));
    assert!(!format!("{snap:?}").contains(CANARY));
    assert!(!format!("{:?}", h.index()).contains(CANARY));
}

#[test]
fn herdr_absent_socket_leaves_overlay_empty() {
    let t = TempDir::new("ac-herdr-absent").unwrap();
    let fp = proc_world(&t);
    // Caminho que nao existe e socket abstrato sem ninguem escutando.
    // Relativo ao crate e curto: o caminho do TempDir pode passar dos 108 bytes de sun_path.
    let missing = SocketAddr::from_pathname("tests/fixtures/herdr/absent/herdr.sock").unwrap();
    let nobody = SocketAddr::from_abstract_name(
        format!("agentcenter-test-herdr-nobody-{}", std::process::id()).as_bytes(),
    )
    .unwrap();
    for addr in [missing, nobody] {
        let h = Herdr::with_timing(Some(addr), fp.proc_root(), fast());
        for _ in 0..3 {
            let snap = tick(&fp, &h);
            assert!(snap.sessions.iter().all(|s| s.overlay.is_none()));
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(h.generation(), 0);
    }
    let off = Herdr::new(None, fp.proc_root());
    assert!(tick(&fp, &off).sessions.iter().all(|s| s.overlay.is_none()));
}

#[test]
fn herdr_status_event_applies_without_new_requests() {
    let t = TempDir::new("ac-herdr-status").unwrap();
    let fp = proc_world(&t);
    let srv = FakeServer::start("status");
    let h = ready(&fp, &srv);
    wait_until("assinatura", || {
        srv.state.open_subs.load(Ordering::SeqCst) == 1
    });
    let g = h.generation();
    srv.emit(
        r#"{"data":{"agent":"claude","agent_status":"blocked","pane_id":"w1:p1","workspace_id":"w1","new":1},"event":"pane.agent_status_changed"}"#,
    );
    wait_until("status aplicado", || h.generation() > g);
    let snap = tick(&fp, &h);
    assert_eq!(
        herdr_of(&snap, 4101).unwrap().status.as_deref(),
        Some("blocked")
    );
    assert_eq!(srv.calls("agent.list"), 1);
    assert_eq!(srv.calls("pane.process_info"), 4);
}

#[test]
fn herdr_topology_event_relists_and_reuses_process_info() {
    let t = TempDir::new("ac-herdr-topo").unwrap();
    let fp = proc_world(&t);
    let srv = FakeServer::start("topo");
    let _h = ready(&fp, &srv);
    wait_until("assinatura", || {
        srv.state.open_subs.load(Ordering::SeqCst) == 1
    });
    srv.emit(r#"{"data":{"pane_id":"w3:p9","workspace_id":"w3"},"event":"pane.created"}"#);
    wait_until("agent.list de novo", || srv.calls("agent.list") == 2);
    // w1:p1 e w1:p2 tem o mesmo lider vivo: process_info so dos que nao casaram
    // (w2:p3, lider morto; w2:p5, pane_not_found). O agent.list e contado antes
    // dos process_info: espera os 2 novos e confere que nao vem mais nenhum.
    wait_until("process_info de w2:p3 e w2:p5", || {
        srv.calls("pane.process_info") >= 6
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(srv.calls("pane.process_info"), 6);
}

#[test]
fn herdr_worker_stops_when_overlay_is_dropped() {
    let t = TempDir::new("ac-herdr-drop").unwrap();
    let fp = proc_world(&t);
    let srv = FakeServer::start("drop");
    let h = ready(&fp, &srv);
    wait_until("assinatura", || {
        srv.state.open_subs.load(Ordering::SeqCst) == 1
    });
    drop(h);
    wait_until("assinatura fechada", || {
        srv.state.open_subs.load(Ordering::SeqCst) == 0
    });
}

/// O socket so e aberto depois de uma sessao `herdr` que existe na raiz do
/// overlay com o mesmo comm e ppid: sampler lendo outro `/proc` (fixture contra
/// o `/proc` real) ou sem herdr nenhum nao conecta.
#[test]
fn herdr_connects_only_after_an_anchored_herdr_session() {
    let t = TempDir::new("ac-herdr-anchor").unwrap();
    let fp = proc_world(&t);
    let srv = FakeServer::start("anchor");
    let other = FakeProc::create(t.path().join("other")).unwrap();
    other.add(&FakeProcess::new(4000, 1, "bash")).unwrap();
    let moved = FakeProc::create(t.path().join("moved")).unwrap();
    moved.add(&FakeProcess::new(4000, 2, "herdr")).unwrap();
    let empty = FakeProc::create(t.path().join("empty")).unwrap();
    let alone = FakeProc::create(t.path().join("alone")).unwrap();
    alone.add(&FakeProcess::new(1, 0, "systemd")).unwrap();
    alone.add(&FakeProcess::new(4400, 1, "claude")).unwrap();
    for (world, root) in [
        (&fp, &other),
        (&fp, &moved),
        (&fp, &empty),
        (&alone, &alone),
    ] {
        let h = Herdr::with_timing(Some(srv.addr.clone()), root.proc_root(), fast());
        for _ in 0..3 {
            let snap = tick(world, &h);
            assert!(snap.sessions.iter().all(|s| s.overlay.is_none()));
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(h.generation(), 0);
    }
    assert!(srv.state.calls.lock().unwrap().is_empty());

    // Mesma raiz: o tick sobe a thread sozinho, sem `start()`.
    let h = Herdr::with_timing(Some(srv.addr.clone()), fp.proc_root(), fast());
    tick(&fp, &h);
    wait_until("indice depois da ancora", || h.generation() > 0);
    let snap = tick(&fp, &h);
    assert_eq!(
        herdr_of(&snap, 4101).and_then(|o| o.pane_id.as_deref()),
        Some("w1:p1")
    );
}
