//! Subagentes por sessao (C15) sobre `tests/fixtures/transcripts`, copiadas
//! para um diretorio temporario (o teste acerta os mtimes):
//!
//! - Claude `5c1d2e3f-...`: 3 subagentes em `<sid>/subagents/`: vivo (sem
//!   `end_turn`, mtime agora), terminado (`end_turn`) e orfao (sem `end_turn`,
//!   mtime antigo) -> vivos 1, total 3.
//! - Codex `01a20000-...-0001` (2026/10/16): 2 filhos com `thread_spawn`, um
//!   com o turno aberto e outro com `task_complete`, os dois com mtime agora ->
//!   vivos 1, total 2.
//!
//! O `CANARY` esta no `description` de cada `.meta.json`, no
//! `base_instructions` e no `agent_nickname` dos rollouts e em todo texto das
//! linhas.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::overlay::claude_state::ClaudeState;
use agentcenter_core::overlay::transcript::{Roots, Transcripts};
use agentcenter_core::overlay::{self, Overlay, SessionOverlay};
use agentcenter_core::session::Session;
use agentcenter_core::{READ_ALL, Registry, Sampler, SamplerConfig, Snapshot, Summary};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const CANARY: &str = "AGENTCENTER-CANARY-7f3a9c";
const SID: &str = "5c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f";
/// Sessao Claude da fixture de C4, sem diretorio `subagents`.
const SID_NONE: &str = "0b6f4a52-9c1e-4d7a-8f3b-2a1c5e7d9f01";
const PARENT: &str = "01a20000-0000-7000-8000-000000000001";
const START: u64 = 5000;
const ALIVE: &str = "agent-a1ive0000000000001";
const ENDED: &str = "agent-a3nded000000000002";
const ORPHAN: &str = "agent-a0rphan00000000003";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for ent in fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(ent.file_name());
        if ent.file_type().unwrap().is_dir() {
            copy_tree(&ent.path(), &to);
        } else {
            fs::copy(ent.path(), to).unwrap();
        }
    }
}

fn set_age(path: &Path, age: Duration) {
    let f = fs::File::options().append(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

/// O diretorio parado ha uma hora: qualquer mudanca depois tem outro mtime.
fn set_dir_age(dir: &Path) {
    let f = fs::File::open(dir).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
}

struct World {
    t: TempDir,
    data: PathBuf,
    fp: FakeProc,
    tr: Arc<Transcripts>,
    ov: Vec<Box<dyn Overlay>>,
}

impl World {
    fn subagents(&self) -> PathBuf {
        self.data
            .join("claude/-home-dev-app")
            .join(SID)
            .join("subagents")
    }

    fn kid(&self, c: char) -> PathBuf {
        self.data.join(format!(
            "codex/2026/10/16/rollout-2026-10-16T10-0{}-00-01a20000-0000-7000-8000-00000000000{c}.jsonl",
            if c == 'a' { 1 } else { 2 }
        ))
    }

    fn tick(&self) -> Snapshot {
        let mut snap = Sampler::new(Registry::default())
            .tick(&self.fp.proc_root())
            .unwrap();
        for s in &mut snap.sessions {
            s.overlay = None;
        }
        overlay::apply(&self.ov, &mut snap.sessions);
        snap.summary = Summary::of(&snap.sessions);
        snap
    }
}

fn by_pid(snap: &Snapshot, pid: u32) -> SessionOverlay {
    snap.sessions
        .iter()
        .find(|s| s.pid == pid)
        .unwrap_or_else(|| panic!("sessao {pid}"))
        .overlay
        .clone()
        .unwrap_or_default()
}

fn counts(snap: &Snapshot, pid: u32) -> (Option<u64>, Option<u64>) {
    let o = by_pid(snap, pid);
    (o.subagents_live, o.subagents_total)
}

struct Shared(Arc<Transcripts>);

impl Overlay for Shared {
    fn id(&self) -> &str {
        self.0.id()
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        self.0.enrich(session, overlay);
    }
}

/// claude 200 (`SID`), claude 210 (`SID_NONE`), codex 220 (pai aberto no
/// fd 3) e pi 230; mtimes: vivo e filhos do Codex agora, terminado e orfao
/// ha uma hora.
fn world(name: &str) -> World {
    let t = TempDir::new(name).unwrap();
    let data = t.path().join("data");
    copy_tree(&fixtures(), &data);
    let fp = FakeProc::create(t.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    for (pid, comm, cwd) in [
        (200, "claude", "/home/dev/app"),
        (210, "claude", "/home/dev/app"),
        (220, "codex", "/home/dev/kids"),
        (230, "pi", "/home/dev/app"),
    ] {
        let mut p = FakeProcess::new(pid, 1, comm);
        p.starttime = START;
        p.cwd = Some(cwd.to_string());
        fp.add(&p).unwrap();
    }
    let fd = fp.root().join("220/fd");
    fs::create_dir_all(&fd).unwrap();
    let parent = data.join(format!(
        "codex/2026/10/16/rollout-2026-10-16T10-00-00-{PARENT}.jsonl"
    ));
    std::os::unix::fs::symlink(parent, fd.join("3")).unwrap();
    let sessions = t.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    for (pid, sid) in [(200, SID), (210, SID_NONE)] {
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!(
                "{{\"pid\":{pid},\"sessionId\":\"{sid}\",\"procStart\":\"{START}\",\"status\":\"busy\"}}"
            ),
        )
        .unwrap();
    }
    let roots = Roots {
        claude: Some(data.join("claude")),
        codex: Some(data.join("codex")),
        pi: Some(data.join("pi")),
    };
    let tr = Arc::new(Transcripts::new(roots, fp.proc_root()).with_refresh(Duration::ZERO));
    let ov: Vec<Box<dyn Overlay>> = vec![
        Box::new(ClaudeState::new(Some(sessions), fp.proc_root())),
        Box::new(Shared(tr.clone())),
    ];
    let w = World {
        t,
        data,
        fp,
        tr,
        ov,
    };
    let sub = w.subagents();
    set_age(&sub.join(format!("{ALIVE}.jsonl")), Duration::ZERO);
    for f in [ENDED, ORPHAN] {
        set_age(&sub.join(format!("{f}.jsonl")), Duration::from_secs(3600));
    }
    for c in ['a', 'b'] {
        set_age(&w.kid(c), Duration::ZERO);
    }
    set_dir_age(&sub);
    w
}

#[test]
fn subagents_live_and_total_from_claude_and_codex_fixtures() {
    let w = world("ac-subagents-counts");
    let snap = w.tick();
    assert_eq!(
        counts(&snap, 200),
        (Some(1), Some(3)),
        "claude: 1 vivo de 3"
    );
    assert_eq!(
        counts(&snap, 210),
        (Some(0), Some(0)),
        "claude sem diretorio"
    );
    assert_eq!(counts(&snap, 220), (Some(1), Some(2)), "codex: 1 vivo de 2");
    assert_eq!(counts(&snap, 230), (None, None), "pi: sem subagentes");
    assert_eq!(snap.summary.subagents_live, Some(2));

    // O vivo parado ha mais de LIVE_WINDOW vira orfao; o Codex aberto tambem.
    set_age(
        &w.subagents().join(format!("{ALIVE}.jsonl")),
        Duration::from_secs(121),
    );
    set_age(&w.kid('a'), Duration::from_secs(121));
    let snap = w.tick();
    assert_eq!(counts(&snap, 200), (Some(0), Some(3)));
    assert_eq!(counts(&snap, 220), (Some(0), Some(2)));
    assert_eq!(snap.summary.subagents_live, Some(0));
}

#[test]
fn subagent_content_never_reaches_the_snapshot() {
    let w = world("ac-subagents-canary");
    let sub = w.subagents();
    for f in [ALIVE, ENDED, ORPHAN] {
        let meta = fs::read_to_string(sub.join(format!("{f}.meta.json"))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert!(v["description"].as_str().unwrap().contains(CANARY), "{f}");
    }
    for c in ['a', 'b'] {
        let head = fs::read_to_string(w.kid(c)).unwrap();
        let v: serde_json::Value = serde_json::from_str(head.lines().next().unwrap()).unwrap();
        let text = v["payload"]["base_instructions"]["text"].as_str().unwrap();
        assert!(text.contains(CANARY), "{c}");
    }
    let snap = w.tick();
    assert_eq!(counts(&snap, 200), (Some(1), Some(3)), "overlay rodou");
    assert!(!serde_json::to_string(&snap).unwrap().contains(CANARY));
    assert!(!format!("{snap:?}").contains(CANARY));
}

#[test]
fn ended_subagent_is_read_once_until_its_meta_changes() {
    let w = world("ac-subagents-once");
    let sub = w.subagents();
    for _ in 0..5 {
        w.tick();
    }
    // Uma leitura do fim por arquivo: o terminado nunca mais; vivo e orfao
    // so com `stat` enquanto nao mudam.
    assert_eq!(w.tr.subagent_tail_reads(), 3);

    // Meta regravado (troca de arquivo, como o Claude faz) sem o jsonl mudar:
    // relista e confere com um `stat`, sem ler.
    let meta = sub.join(format!("{ENDED}.meta.json"));
    let tmp = sub.join("meta.tmp");
    fs::copy(&meta, &tmp).unwrap();
    fs::rename(&tmp, &meta).unwrap();
    let snap = w.tick();
    assert_eq!(counts(&snap, 200), (Some(1), Some(3)));
    assert_eq!(w.tr.subagent_tail_reads(), 3);

    // Retomado: o jsonl ganha uma resposta sem `end_turn` e o meta e
    // regravado. Volta a ser vivo.
    set_dir_age(&sub);
    let line = "{\"type\":\"user\",\"isSidechain\":true,\"message\":{\"content\":\"x\"}}\n";
    let answer = "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"stop_reason\":\"tool_use\"}}\n";
    let ended = sub.join(format!("{ENDED}.jsonl"));
    fs::OpenOptions::new()
        .append(true)
        .open(&ended)
        .unwrap()
        .write_all((line.to_string() + answer).as_bytes())
        .unwrap();
    fs::copy(&meta, &tmp).unwrap();
    fs::rename(&tmp, &meta).unwrap();
    let snap = w.tick();
    assert_eq!(w.tr.subagent_tail_reads(), 4);
    assert_eq!(
        counts(&snap, 200),
        (Some(2), Some(3)),
        "retomado conta como vivo"
    );

    // Orfao que volta a escrever: lido de novo e vivo.
    let orphan = sub.join(format!("{ORPHAN}.jsonl"));
    fs::OpenOptions::new()
        .append(true)
        .open(&orphan)
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
    let snap = w.tick();
    assert_eq!(w.tr.subagent_tail_reads(), 5);
    assert_eq!(counts(&snap, 200), (Some(3), Some(3)));
}

#[test]
fn new_subagent_file_is_listed_and_first_reads_are_spread_over_ticks() {
    let w = world("ac-subagents-budget");
    let sub = w.subagents();
    w.tick();
    // 12 arquivos novos de 80 KiB sem `assistant`: 64 KiB lidos de cada um,
    // mais que o teto de 512 KiB de um tick.
    let filler = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{CANARY}\"}}}}\n");
    let body = filler.repeat((80 << 10) / filler.len() + 1);
    for i in 0..12 {
        let p = sub.join(format!("agent-b{i:017}.jsonl"));
        fs::write(&p, &body).unwrap();
        set_age(&p, Duration::from_secs(3600));
    }
    let reads = w.tr.subagent_tail_reads();
    let snap = w.tick();
    assert_eq!(
        counts(&snap, 200),
        (None, Some(15)),
        "total ja vale; vivos so com tudo lido"
    );
    let first = w.tr.subagent_tail_reads() - reads;
    assert!((1..12).contains(&first), "lidos no primeiro tick: {first}");
    let mut ticks = 1;
    while counts(&w.tick(), 200).0.is_none() {
        ticks += 1;
        assert!(ticks < 20);
    }
    assert_eq!(w.tr.subagent_tail_reads() - reads, 12);
    assert_eq!(counts(&w.tick(), 200), (Some(1), Some(15)));
    drop(w.t);
}

/// `n` arquivos novos de 80 KiB sem `assistant` em `sub` (64 KiB lidos de
/// cada um), parados ha uma hora: orfaos.
fn add_orphans(sub: &Path, prefix: &str, n: usize) {
    let filler = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{CANARY}\"}}}}\n");
    let body = filler.repeat((80 << 10) / filler.len() + 1);
    for i in 0..n {
        let p = sub.join(format!("agent-{prefix}{i:016}.jsonl"));
        fs::write(&p, &body).unwrap();
        set_age(&p, Duration::from_secs(3600));
    }
}

#[test]
fn one_shot_config_reads_every_subagent_in_the_first_tick() {
    let w = world("ac-subagents-one-shot");
    let sub = w.subagents();
    // 40 x 64 KiB = 2,5 MiB: cinco vezes o teto de um tick.
    add_orphans(&sub, "c", 40);
    // O que `Sampler::with_config` faz com a configuracao do `snapshot`.
    let one_shot = SamplerConfig {
        transcript_read_budget: READ_ALL,
        ..SamplerConfig::default()
    };
    w.tr.configure(&one_shot);
    let snap = w.tick();
    assert_eq!(
        counts(&snap, 200),
        (Some(1), Some(43)),
        "um tiro: vivos ja no primeiro tick"
    );
    assert_eq!(w.tr.subagent_tail_reads(), 43, "cada arquivo lido uma vez");
    assert_eq!(snap.summary.subagents_live, Some(2));
    // O teto volta a cada tick: no seguinte tambem nao ha.
    set_dir_age(&sub);
    add_orphans(&sub, "e", 40);
    let snap = w.tick();
    assert_eq!(counts(&snap, 200), (Some(1), Some(83)), "segundo tick");
    assert_eq!(w.tr.subagent_tail_reads(), 83);

    // De volta ao padrao (`watch`, GUI): o teto por tick vale de novo.
    w.tr.configure(&SamplerConfig::default());
    w.tick();
    set_dir_age(&sub);
    add_orphans(&sub, "d", 12);
    let snap = w.tick();
    assert_eq!(counts(&snap, 200), (None, Some(95)), "teto por tick");
    drop(w.t);
}
