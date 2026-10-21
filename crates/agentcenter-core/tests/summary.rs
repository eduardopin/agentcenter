//! Resumo do tick (`Snapshot.summary`) sobre um `/proc` falso. O estado do
//! Claude vem do overlay real sobre um diretorio de sessoes temporario; o pane
//! herdr e um overlay de teste que preenche o mesmo `HerdrOverlay` do real.

use agentcenter_core::adapters::AgentRole;
use agentcenter_core::cockpit::Tokens;
use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::overlay::claude_state::ClaudeState;
use agentcenter_core::overlay::{self, HerdrOverlay, Overlay, OverlayStatus, SessionOverlay};
use agentcenter_core::{
    ProcRoot, Registry, Sampler, Session, SessionState, Snapshot, StatusCounts, Summary,
};
use std::collections::BTreeMap;
use std::path::Path;

const START: u64 = 5000;

/// Pane herdr por pid do agente, como o overlay herdr publica.
struct Panes(Vec<(u32, &'static str)>);

impl Overlay for Panes {
    fn id(&self) -> &str {
        "panes"
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        if let Some((_, st)) = self.0.iter().find(|(pid, _)| *pid == session.pid) {
            overlay.herdr = Some(HerdrOverlay {
                pane_id: Some(format!("w1:p{}", session.pid)),
                status: Some((*st).into()),
                ..Default::default()
            });
        }
    }
}

fn add(fp: &FakeProc, pid: u32, ppid: u32, comm: &str, state: char) -> FakeProcess {
    let mut p = FakeProcess::new(pid, ppid, comm);
    p.starttime = START;
    p.state = state;
    fp.add(&p).unwrap();
    p
}

fn claude_json(dir: &Path, pid: u32, status: &str) {
    std::fs::write(
        dir.join(format!("{pid}.json")),
        format!(r#"{{"pid":{pid},"procStart":"{START}","status":"{status}"}}"#),
    )
    .unwrap();
}

/// 2 claude (20 busy, 40 blocked e num pane herdr `working`), 1 codex (30) num
/// pane `working`, 1 pi (50) sem status e 1 herdr (10).
/// Filhos: 21 em R, 22 em S, 41 em D, 31 em S, 51 em S com CPU no 2o tick, 11 em S.
fn world(t: &TempDir) -> (FakeProc, FakeProcess) {
    let fp = FakeProc::create(t.path().join("proc")).unwrap();
    add(&fp, 1, 0, "systemd", 'S');
    add(&fp, 10, 1, "herdr", 'S');
    add(&fp, 11, 10, "bash", 'S');
    add(&fp, 20, 1, "claude", 'S');
    add(&fp, 21, 20, "node", 'R');
    add(&fp, 22, 20, "bash", 'S');
    add(&fp, 30, 10, "codex", 'S');
    add(&fp, 31, 30, "bash", 'S');
    add(&fp, 40, 1, "claude", 'S');
    add(&fp, 41, 40, "cargo", 'D');
    add(&fp, 50, 1, "pi", 'S');
    let p51 = add(&fp, 51, 50, "rg", 'S');
    (fp, p51)
}

fn overlays(t: &TempDir, fp: &FakeProc) -> Vec<Box<dyn Overlay>> {
    let dir = t.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    claude_json(&dir, 20, "busy");
    claude_json(&dir, 40, "waiting");
    vec![
        Box::new(ClaudeState::new(Some(dir), fp.proc_root())),
        Box::new(Panes(vec![(30, "working"), (40, "working")])),
    ]
}

/// O resumo do sampler e o das sessoes do tick; depois troca o overlay do
/// ambiente (o do registry olharia o HOME real) pelo do teste.
fn with_test_overlays(mut snap: Snapshot, ovs: &[Box<dyn Overlay>]) -> Snapshot {
    assert_eq!(snap.summary, Summary::of(&snap.sessions));
    for s in &mut snap.sessions {
        s.overlay = None;
    }
    overlay::apply(ovs, &mut snap.sessions);
    snap.summary = Summary::of(&snap.sessions);
    snap
}

#[test]
fn summary_counts_sessions_states_and_working_children() {
    let t = TempDir::new("ac-summary").unwrap();
    let (fp, mut p51) = world(&t);
    let ovs = overlays(&t, &fp);
    let root = fp.proc_root();
    let mut sampler = Sampler::new(Registry::default());
    let first = with_test_overlays(sampler.tick(&root).unwrap(), &ovs);
    fp.set_cpu(16, 100, 1500).unwrap();
    p51.utime = 50; // 50 % de um nucleo no intervalo de 1 s
    fp.add(&p51).unwrap();
    let snap = with_test_overlays(sampler.tick(&root).unwrap(), &ovs);
    let s = &snap.summary;

    assert_eq!((s.sessions, s.multiplexers), (4, 1));
    assert_eq!(
        s.by_agent,
        BTreeMap::from([("claude".into(), 2), ("codex".into(), 1), ("pi".into(), 1)])
    );
    // 40 esta num pane `working`, mas o estado do Claude (blocked) vale mais.
    assert_eq!(
        s.status,
        StatusCounts {
            busy: 2,
            blocked: 1,
            idle: 0,
            unknown: 1
        }
    );
    // Filhos: 21, 22, 41, 31, 51 e o 11 do herdr. Trabalhando: 21 (R), 41 (D), 51 (CPU).
    assert_eq!(s.child_processes, 6);
    assert_eq!(s.working_child_processes, 3);
    assert_eq!(
        s.sessions_with_working_children, 3,
        "20, 40 e 50 tem filho trabalhando"
    );
    // 40 tem thread em D e CPU conhecida abaixo de 10 %: io_bound no fallback.
    assert_eq!(s.io_bound_sessions, 1);
    assert!(snap.sessions.iter().find(|x| x.pid == 40).unwrap().io_bound);
    assert_eq!(s.sessions_cpu_pct, Some(50.0));
    let pss: u64 = snap.sessions.iter().filter_map(|x| x.pss_bytes).sum();
    assert_eq!(s.sessions_pss_bytes, Some(pss));
    assert_eq!(s.sessions_swap_pss_bytes, Some(0));
    assert_eq!(
        (s.tool_calls_open, s.tool_calls_per_min, s.subagents_live),
        (None, None, None)
    );

    // Primeiro tick: sem CPU, 51 ainda nao trabalha; o resto ja vale.
    let f = &first.summary;
    assert_eq!((f.status, f.child_processes), (s.status, 6));
    assert_eq!(
        (f.working_child_processes, f.sessions_with_working_children),
        (2, 2)
    );
    assert_eq!(f.sessions_cpu_pct, None);
}

#[test]
fn summary_invariants_hold_on_the_sampler_snapshot() {
    let t = TempDir::new("ac-summary-inv").unwrap();
    let (fp, _) = world(&t);
    let snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    let s = &snap.summary;
    let agents = snap
        .sessions
        .iter()
        .filter(|x| x.role == AgentRole::Agent)
        .count();
    assert_eq!(s.sessions, agents);
    assert_eq!(s.by_agent.values().sum::<usize>(), s.sessions);
    let st = s.status;
    assert_eq!(st.busy + st.blocked + st.idle + st.unknown, s.sessions);
    assert_eq!(
        s.child_processes,
        snap.sessions.iter().map(|x| x.procs - 1).sum::<usize>()
    );
}

/// Sessao de agente com so o overlay dado.
fn agent_with(overlay: Option<SessionOverlay>) -> Session {
    let mut s = Session::aggregate("codex".into(), AgentRole::Agent, 7, 16, Vec::new());
    s.overlay = overlay;
    s
}

fn herdr(status: &str) -> Option<SessionOverlay> {
    Some(SessionOverlay {
        herdr: Some(HerdrOverlay {
            status: Some(status.into()),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[test]
fn state_precedence_overlay_then_herdr_then_unknown() {
    use agentcenter_core::summary::session_state;
    let cases = [
        (herdr("working"), SessionState::Busy),
        (herdr("blocked"), SessionState::Blocked),
        (herdr("idle"), SessionState::Idle),
        (herdr("done"), SessionState::Idle),
        (herdr("unknown"), SessionState::Unknown),
        (herdr("algo-novo"), SessionState::Unknown),
        (None, SessionState::Unknown),
        (Some(SessionOverlay::default()), SessionState::Unknown),
    ];
    for (o, want) in cases {
        assert_eq!(session_state(&agent_with(o.clone())), want, "{o:?}");
    }
    // overlay.status ganha do pane, nos dois sentidos.
    let mut o = herdr("working").unwrap();
    o.status = Some(OverlayStatus::Idle);
    assert_eq!(session_state(&agent_with(Some(o))), SessionState::Idle);
    let mut o = herdr("idle").unwrap();
    o.status = Some(OverlayStatus::Busy);
    assert_eq!(session_state(&agent_with(Some(o))), SessionState::Busy);
}

#[test]
fn summary_of_no_sessions_is_zero_and_sums_are_none() {
    let s = Summary::of(&[]);
    assert_eq!(s, Summary::default());
    assert_eq!(s.sessions_pss_bytes, None);
    assert!(s.by_agent.is_empty());
}

#[test]
fn snapshot_v3_without_summary_still_reads() {
    let t = TempDir::new("ac-summary-v3").unwrap();
    let (fp, _) = world(&t);
    let snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    let mut v = serde_json::to_value(&snap).unwrap();
    v.as_object_mut().unwrap().remove("summary");
    v["schema_version"] = 3.into();
    let back: Snapshot = serde_json::from_value(v).unwrap();
    assert_eq!(back.summary, Summary::default());
    assert_eq!(back.sessions, snap.sessions);
}

/// Leituras contadas por (pid, arquivo) de todos os pids do mundo.
fn reads(root: &ProcRoot) -> Vec<u32> {
    const FILES: [&str; 7] = [
        "stat",
        "io",
        "status",
        "smaps_rollup",
        "task",
        "cmdline",
        "cwd",
    ];
    [1, 10, 11, 20, 21, 22, 30, 31, 40, 41, 50, 51]
        .iter()
        .flat_map(|&pid| FILES.iter().map(move |&f| root.read_count(pid, f)))
        .collect()
}

#[test]
fn summary_opens_nothing_reads_per_tick_are_unchanged() {
    let t = TempDir::new("ac-summary-reads").unwrap();
    let (fp, _) = world(&t);
    let root = fp.counting_root();
    let mut sampler = Sampler::new(Registry::default());
    let mut per_tick = Vec::new();
    for _ in 0..3 {
        let before = reads(&root);
        let snap = sampler.tick(&root).unwrap();
        let after_tick = reads(&root);
        // Recalcular o resumo (o "com summary" extra) nao abre nada.
        for _ in 0..100 {
            assert_eq!(Summary::of(&snap.sessions), snap.summary);
        }
        assert_eq!(reads(&root), after_tick, "Summary::of leu /proc");
        let delta: u32 = after_tick.iter().zip(&before).map(|(a, b)| a - b).sum();
        per_tick.push(delta);
    }
    // Medido na main af037d3 (antes do resumo), mesmo mundo e mesma conta:
    // [55, 22, 4]. O resumo nao muda nenhum tick.
    assert_eq!(per_tick, [55, 22, 4]);
}

/// Sessao com o overlay dado e, se `tokens`, o cockpit dessas contas (e o
/// `tokens_in` e o `context_pct` que o overlay de transcripts publica junto).
fn cockpit_session(
    agent: &str,
    role: AgentRole,
    pid: u32,
    status: Option<OverlayStatus>,
    tokens: Option<(Tokens, Option<f64>)>,
) -> Session {
    use agentcenter_core::cockpit::{Cockpit, Thresholds, ctx_pct, round_n};
    let mut s = Session::aggregate(agent.into(), role, pid, 16, Vec::new());
    let mut o = SessionOverlay {
        status,
        ..Default::default()
    };
    if let Some((t, idle)) = tokens {
        o.tokens_in = Some(t.input + t.cache_write + t.cache_read);
        o.context_pct = Some(round_n(ctx_pct(t.ctx, t.window), 1));
        o.cockpit = Some(Cockpit::compute(&Thresholds::default(), &t, idle, None));
    }
    s.overlay = Some(o);
    s
}

fn tokens(
    input: u64,
    cache_write: u64,
    cache_read: u64,
    output: u64,
    ctx: u64,
    window: u64,
) -> Tokens {
    Tokens {
        input,
        cache_write,
        cache_read,
        output,
        ctx,
        window,
        window_inferred: false,
    }
}

/// 3 sessoes de agente (busy com cockpit a 60 %, idle com cockpit ociosa, uma
/// sem transcript) e um herdr com cockpit, que nao entra em nenhuma conta.
#[test]
fn summary_aggregates_the_cockpit_of_agent_sessions_only() {
    use agentcenter_core::CockpitCounts;
    use agentcenter_core::cockpit::Recommendation;
    let busy = cockpit_session(
        "claude",
        AgentRole::Agent,
        20,
        Some(OverlayStatus::Busy),
        // novo 10k + 1,25*20k + 5*4k = 55k, relei 30k: burn 85k, share 0,353.
        Some((
            tokens(10_000, 20_000, 300_000, 4_000, 120_000, 200_000),
            None,
        )),
    );
    let idle = cockpit_session(
        "codex",
        AgentRole::Agent,
        30,
        Some(OverlayStatus::Idle),
        // novo 50k + 5*2k = 60k, relei 15k: burn 75k; 15 % e 120 min parada.
        Some((
            tokens(50_000, 0, 150_000, 2_000, 60_000, 400_000),
            Some(120.0),
        )),
    );
    let mut no_transcript = cockpit_session("pi", AgentRole::Agent, 50, None, None);
    // O herdr com cockpit, pendencia e contexto maiores que todos: se entrasse
    // em qualquer conta, ela mudaria.
    let mut mux = cockpit_session(
        "herdr",
        AgentRole::Multiplexer,
        10,
        Some(OverlayStatus::Busy),
        Some((tokens(1_000_000, 0, 9_000_000, 0, 190_000, 200_000), None)),
    );
    mux.overlay.as_mut().unwrap().cockpit_pending = true;

    let rec = |s: &Session| {
        s.overlay
            .as_ref()?
            .cockpit
            .as_ref()
            .map(|c| c.recommendation)
    };
    assert_eq!(rec(&busy), Some(Recommendation::Compactar));
    assert_eq!(rec(&idle), Some(Recommendation::Ociosa));
    assert_eq!(rec(&mux), Some(Recommendation::Handoff));

    let sessions = vec![mux.clone(), busy, idle, no_transcript.clone()];
    let s = Summary::of(&sessions);
    assert_eq!((s.sessions, s.multiplexers), (3, 1));
    assert_eq!(
        s.status,
        StatusCounts {
            busy: 1,
            blocked: 0,
            idle: 1,
            unknown: 1
        }
    );
    assert_eq!(
        s.cockpit,
        CockpitCounts {
            compactar: 1,
            ociosa: 1,
            sem_dado: 1,
            ..Default::default()
        }
    );
    let c = s.cockpit;
    assert_eq!(
        c.continuar + c.compactar + c.handoff + c.ociosa + c.loop_ + c.sem_dado,
        s.sessions
    );
    assert_eq!(s.cockpit_pending, 0, "sem transcript nao e pendencia");
    assert_eq!(s.sessions_burn_weighted, Some(85_000 + 75_000));
    // (300k + 150k) / (330k + 200k) = 0,849; a media dos dois cache_hit
    // (0,909 e 0,75) daria 0,830.
    assert_eq!(s.cache_hit, Some(0.849));
    assert_eq!(s.ctx_pct_max, Some(60.0));

    // Transcript casado com varredura incompleta: continua sem_dado, agora
    // tambem pendente.
    no_transcript.overlay.as_mut().unwrap().cockpit_pending = true;
    let mut sessions = sessions;
    sessions[3] = no_transcript;
    let p = Summary::of(&sessions);
    assert_eq!((p.cockpit.sem_dado, p.cockpit_pending), (1, 1));

    // So o herdr: nenhuma sessao de agente, nada de cockpit.
    let only_mux = Summary::of(&[mux]);
    assert_eq!(only_mux.cockpit, CockpitCounts::default());
    assert_eq!(only_mux.cockpit_pending, 0);
    assert_eq!(
        (
            only_mux.sessions_burn_weighted,
            only_mux.cache_hit,
            only_mux.ctx_pct_max
        ),
        (None, None, None)
    );
}
