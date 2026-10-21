//! Custo do overlay de transcripts (C4, criterio 5): tick sobre 30 transcripts
//! de 20 MB (10 Claude, 10 Codex, 10 pi) em regime, sem append; orcamento
//! < +0.5 ms na mediana.
//!
//! - `transcripts_30x20mb_base`: tick + `claude_state`, sem transcripts.
//! - `transcripts_30x20mb_tick_cached`: + transcripts como no registry
//!   (conferencia a cada 5 s). Acrescimo em regime = este - base.
//! - `transcripts_30x20mb_tick`: + transcripts conferidos em todo tick (pior
//!   caso do regime: o tick em que a conferencia de 5 s cai).
//! - `transcripts_30x20mb_overlay` e `_overlay_cached`: so o overlay de
//!   transcripts, sem o ruido do tick (os mesmos dois casos).
//! - `transcripts_30x20mb_first_tick`: primeiro tick de um overlay novo (casa
//!   os 30 e le ate `READ_BUDGET`), comparar com a base.
//!
//! Handoffs do cockpit (C21, criterio 5): cada `codex` roda num repo proprio
//! com `.claude/handoffs` de 100 `.md` (10 raizes x 100) e os `pi`, sem repo,
//! caem no global `<home>/.claude/handoffs` (100 `.md`, 10 por sessao), com o
//! `home` de [`Transcripts::with_home`]. Os diretorios ficam parados (mtime de
//! uma hora atras): em regime o indice nao relista nenhum, e o fim da medida
//! confere isso pelo [`Transcripts::handoff_readdirs`].
//!
//! Os 600 MB so sao gravados quando um id deste arquivo e escolhido (o
//! `OnceCell` e preenchido dentro do closure), e a primeira leitura deles
//! acontece antes da medida. Rodar: `cargo bench -p agentcenter-core --bench
//! transcripts`. Os ids nao comecam por `tick_fixture_500`: o portao do CI
//! nao os confere.
//!
//! Subagentes (C15, criterio 4): `subagents_30x200_*`, ver [`subagents`].

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir, synthetic};
use agentcenter_core::overlay::claude_state::{ClaudeState, SESSIONS_DIR_ENV};
use agentcenter_core::overlay::transcript::{Roots, Transcripts};
use agentcenter_core::overlay::{self, Overlay, SessionOverlay};
use agentcenter_core::{ProcRoot, Registry, Sampler};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::cell::OnceCell;
use std::fs;
use std::hint::black_box;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

const FILES_PER_FORMAT: u32 = 10;
const FILE_BYTES: u64 = 20 << 20;
/// Pids das sessoes `claude` de `synthetic(500)` usadas (3, 23, ..., 183).
const CLAUDE_PIDS: std::ops::Range<u32> = 0..FILES_PER_FORMAT;
const CODEX_PID: u32 = 600;
const PI_PID: u32 = 700;
/// `starttime` das sessoes de `synthetic` (em ticks de relogio).
const START: u64 = 1000;
/// `.md` por diretorio de handoff (os 10 repos do codex e o global).
const HANDOFFS_PER_DIR: u32 = 100;
/// `readdir` de handoff de um overlay depois da primeira leitura: os 10 repos
/// do codex e o global (pi e claude o dividem).
const HANDOFF_DIRS: u64 = FILES_PER_FORMAT as u64 + 1;

fn claude_pid(i: u32) -> u32 {
    3 + 20 * i
}

fn fixture(rel: &str) -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts");
    fs::read_to_string(dir.join(rel)).unwrap()
}

/// Grava `header` e depois `body` repetido ate passar de [`FILE_BYTES`].
fn big(path: &Path, header: &str, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = BufWriter::new(fs::File::create(path).unwrap());
    w.write_all(header.as_bytes()).unwrap();
    let mut n = header.len() as u64;
    while n < FILE_BYTES {
        w.write_all(body.as_bytes()).unwrap();
        n += body.len() as u64;
    }
    w.flush().unwrap();
}

/// Primeira linha (com `\n`) e o resto.
fn split_header(s: &str) -> (String, String) {
    let i = s.find('\n').unwrap() + 1;
    (s[..i].to_string(), s[i..].to_string())
}

/// O overlay de transcripts dividido com o bench, que le o contador de
/// `readdir` de handoff.
struct Shared(Arc<Transcripts>);

impl Overlay for Shared {
    fn id(&self) -> &str {
        self.0.id()
    }

    fn enrich(&self, session: &agentcenter_core::session::Session, overlay: &mut SessionOverlay) {
        self.0.enrich(session, overlay);
    }
}

struct World {
    _dir: TempDir,
    a: ProcRoot,
    b: ProcRoot,
    roots: Roots,
    sessions: PathBuf,
    home: PathBuf,
    /// Os transcripts de `with` e de `cached`, para o contador de handoff.
    shared: [Arc<Transcripts>; 2],
    base: Vec<Box<dyn Overlay>>,
    with: Vec<Box<dyn Overlay>>,
    cached: Vec<Box<dyn Overlay>>,
}

/// Processos `codex` e `pi` com `cwd` proprio; o `codex` tem o rollout aberto
/// e roda no repo `repos/agent-<pid>`.
fn agents(fp: &FakeProc, data: &Path, repos: &Path) {
    for i in 0..FILES_PER_FORMAT {
        for (pid, comm) in [(CODEX_PID + i, "codex"), (PI_PID + i, "pi")] {
            let mut p = FakeProcess::new(pid, 2, comm);
            p.starttime = START;
            p.threads = 20;
            p.cwd = Some(agent_cwd(repos, pid));
            fp.add(&p).unwrap();
        }
        let fd = fp.root().join(format!("{}/fd", CODEX_PID + i));
        fs::create_dir_all(&fd).unwrap();
        std::os::unix::fs::symlink(codex_file(data, i), fd.join("3")).unwrap();
    }
}

/// `cwd` do agente `pid`: o repo do codex; o `pi` fica fora de repo (o
/// `/home/dev/...` nao existe), entao o handoff dele e o global.
fn agent_cwd(repos: &Path, pid: u32) -> String {
    if (CODEX_PID..CODEX_PID + FILES_PER_FORMAT).contains(&pid) {
        repos
            .join(format!("agent-{pid}"))
            .to_string_lossy()
            .into_owned()
    } else {
        format!("/home/dev/agent-{pid}")
    }
}

/// Grava os `.md` de `names` em `dir` e deixa o diretorio parado ha uma hora.
fn handoff_dir(dir: &Path, names: impl Iterator<Item = String>) {
    fs::create_dir_all(dir).unwrap();
    for name in names {
        fs::write(dir.join(name), "# handoff\n").unwrap();
    }
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    fs::File::open(dir).unwrap().set_modified(hour_ago).unwrap();
}

/// 10 repos do codex com 100 handoffs cada e o global com 100 (10 por `pi`).
fn handoffs(repos: &Path, home: &Path) {
    for i in 0..FILES_PER_FORMAT {
        let repo = repos.join(format!("agent-{}", CODEX_PID + i));
        fs::create_dir_all(repo.join(".git")).unwrap();
        handoff_dir(
            &repo.join(".claude/handoffs"),
            (0..HANDOFFS_PER_DIR).map(|j| format!("2026-10-17-{j:03}.md")),
        );
    }
    let per_pi = HANDOFFS_PER_DIR / FILES_PER_FORMAT;
    handoff_dir(
        &home.join(".claude/handoffs"),
        (0..HANDOFFS_PER_DIR).map(|j| {
            let pid = PI_PID + j / per_pi;
            format!("agent-{pid}-2026-10-17-{:02}.md", j % per_pi)
        }),
    );
}

fn codex_file(data: &Path, i: u32) -> PathBuf {
    data.join(format!(
        "codex/2026/10/15/rollout-2026-10-15T01-14-06-0000000{i}-6ff3-7fb2-a708-0d87a9e28591.jsonl"
    ))
}

fn build() -> World {
    let t0 = Instant::now();
    let dir = TempDir::new("ac-bench-transcripts").unwrap();
    let data = dir.path().join("data");
    let sessions = dir.path().join("claude-sessions");
    fs::create_dir_all(&sessions).unwrap();
    // Absoluto e sem link: o indice de handoff normaliza o cwd como `abspath`.
    let base = fs::canonicalize(dir.path()).unwrap();
    let repos = base.join("repos");
    let home = base.join("home");
    handoffs(&repos, &home);

    let (_, claude_body) = split_header(&format!(
        "\n{}",
        fixture("claude/-home-dev-app/0b6f4a52-9c1e-4d7a-8f3b-2a1c5e7d9f01.jsonl")
    ));
    let (codex_head, codex_body) = split_header(&fixture(
        "codex/2026/10/15/rollout-2026-10-15T01-14-06-01a13dc4-6ff3-7fb2-a708-0d87a9e28591.jsonl",
    ));
    let (pi_head, pi_body) = split_header(&fixture(
        "pi/--home-dev-app--/2026-10-17T02-53-28-810Z_01a147c7-5769-739f-8d7a-6e1addc5ac7d.jsonl",
    ));
    for i in CLAUDE_PIDS {
        let pid = claude_pid(i);
        let sid = format!("00000000-0000-4000-8000-{pid:012}");
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"{sid}\",\"procStart\":\"{START}\",\"status\":\"busy\"}}"),
        )
        .unwrap();
        let slug = format!("-home-dev-project-{pid}");
        big(
            &data.join(format!("claude/{slug}/{sid}.jsonl")),
            "",
            &claude_body,
        );
    }
    for i in 0..FILES_PER_FORMAT {
        let cwd = agent_cwd(&repos, CODEX_PID + i);
        big(
            &codex_file(&data, i),
            &codex_head.replace("/home/dev/app", &cwd),
            &codex_body,
        );
        let pid = PI_PID + i;
        let cwd = agent_cwd(&repos, pid);
        big(
            &data.join(format!(
                "pi/--home-dev-agent-{pid}--/2026-10-17T02-53-28-810Z_{pid:08}.jsonl"
            )),
            &pi_head.replace("/home/dev/app", &cwd),
            &pi_body,
        );
    }

    let a = synthetic(dir.path().join("a"), 500, 1, true).unwrap();
    let b = synthetic(dir.path().join("b"), 500, 2, true).unwrap();
    agents(&a, &data, &repos);
    agents(&b, &data, &repos);
    let (a, b) = (a.proc_root(), b.proc_root());
    let roots = Roots {
        claude: Some(data.join("claude")),
        codex: Some(data.join("codex")),
        pi: Some(data.join("pi")),
    };
    let state = || Box::new(ClaudeState::new(Some(sessions.clone()), a.clone()));
    let base: Vec<Box<dyn Overlay>> = vec![state()];
    let tr = || Transcripts::new(roots.clone(), a.clone()).with_home(&home);
    // Pior caso do regime: arquivo, casamento e handoff conferidos em todo tick.
    let shared = [
        Arc::new(tr().with_refresh(Duration::ZERO)),
        // Como no registry: conferencia a cada 5 s, o resto do tempo so cache.
        Arc::new(tr()),
    ];
    let with: Vec<Box<dyn Overlay>> = vec![state(), Box::new(Shared(shared[0].clone()))];
    let cached: Vec<Box<dyn Overlay>> = vec![state(), Box::new(Shared(shared[1].clone()))];
    let roots_again = roots;
    let written = t0.elapsed();

    // Primeira leitura (os 600 MB, READ_BUDGET por tick), fora da medida: ate
    // o cockpit dos 30, que so sai com a varredura completa. Assim nenhuma
    // medida de regime carrega leitura de varredura.
    let t1 = Instant::now();
    let mut sampler = Sampler::new(Registry::default());
    let mut ticks = 0;
    for ov in [&with, &cached] {
        loop {
            let mut snap = sampler.tick(&a).unwrap();
            overlay::apply(ov, &mut snap.sessions);
            ticks += 1;
            let filled = snap
                .sessions
                .iter()
                .filter(|s| {
                    s.overlay
                        .as_ref()
                        .is_some_and(|o| o.context_pct.is_some() && o.cockpit.is_some())
                })
                .count();
            if filled == 3 * FILES_PER_FORMAT as usize {
                break;
            }
            assert!(
                ticks < 5_000,
                "os 30 transcripts casam e terminam ({filled})"
            );
        }
    }
    eprintln!(
        "transcripts: 30 x {} MB gravados em {written:?}; primeira leitura (2 overlays) \
         em {ticks} ticks, {:?}",
        FILE_BYTES >> 20,
        t1.elapsed()
    );
    // Os 20 agentes com handoff o acham (codex no repo, pi no global).
    let mut snap = sampler.tick(&a).unwrap();
    overlay::apply(&with, &mut snap.sessions);
    let found = snap
        .sessions
        .iter()
        .filter_map(|s| s.overlay.as_ref()?.cockpit.as_ref()?.handoff_name.as_ref())
        .count();
    assert_eq!(found, 2 * FILES_PER_FORMAT as usize, "handoffs achados");
    for tr in &shared {
        assert_eq!(
            tr.handoff_readdirs(),
            HANDOFF_DIRS,
            "uma listagem por diretorio"
        );
    }
    World {
        _dir: dir,
        a,
        b,
        roots: roots_again,
        sessions,
        home,
        shared,
        base,
        with,
        cached,
    }
}

/// Os overlays do registry (que todo Sampler traz) olham diretorios vazios:
/// nada de ~/.claude, ~/.codex ou ~/.pi desta maquina entra na medida.
fn isolate_env() -> TempDir {
    let dir = TempDir::new("ac-bench-tr-env").unwrap();
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    for var in [
        SESSIONS_DIR_ENV,
        "HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "PI_CODING_AGENT_DIR",
    ] {
        // SAFETY: antes de qualquer Sampler e de qualquer thread; o criterion
        // roda as medidas nesta mesma thread.
        unsafe { std::env::set_var(var, &empty) };
    }
    dir
}

fn transcripts(c: &mut Criterion) {
    let _env = isolate_env();
    let world = OnceCell::new();
    for (name, pick) in [
        ("transcripts_30x20mb_base", 0),
        ("transcripts_30x20mb_tick", 1),
        ("transcripts_30x20mb_tick_cached", 2),
    ] {
        let mut sampler = Sampler::new(Registry::default());
        let mut flip = false;
        c.bench_function(name, |bench| {
            let w: &World = world.get_or_init(build);
            let ov = [&w.base, &w.with, &w.cached][pick];
            bench.iter(|| {
                flip = !flip;
                let mut snap = sampler.tick(if flip { &w.a } else { &w.b }).unwrap();
                overlay::apply(ov, &mut snap.sessions);
                black_box(snap)
            })
        });
    }

    // So o overlay de transcripts (conferencia em todo tick), sobre as sessoes
    // de um tick que ja tem o `sessionId` do `claude_state`: o acrescimo sem o
    // ruido do tick.
    let mut snap = None;
    c.bench_function("transcripts_30x20mb_overlay", |bench| {
        let w: &World = world.get_or_init(build);
        let sessions = &mut snap
            .get_or_insert_with(|| {
                let mut s = Sampler::new(Registry::default()).tick(&w.a).unwrap();
                overlay::apply(&w.base, &mut s.sessions);
                s
            })
            .sessions;
        bench.iter(|| {
            overlay::apply(&w.with[1..], sessions);
            black_box(&*sessions);
        })
    });
    // O mesmo, com a conferencia a cada 5 s do registry: o acrescimo em regime.
    let mut snap = None;
    c.bench_function("transcripts_30x20mb_overlay_cached", |bench| {
        let w: &World = world.get_or_init(build);
        let sessions = &mut snap
            .get_or_insert_with(|| {
                let mut s = Sampler::new(Registry::default()).tick(&w.a).unwrap();
                overlay::apply(&w.base, &mut s.sessions);
                s
            })
            .sessions;
        bench.iter(|| {
            overlay::apply(&w.cached[1..], sessions);
            black_box(&*sessions);
        })
    });

    // Primeiro tick de um overlay novo (inicio do monitor): casa os 30 e le
    // ate READ_BUDGET dos 600 MB. Comparar com transcripts_30x20mb_base.
    let mut sampler = Sampler::new(Registry::default());
    let mut flip = false;
    c.bench_function("transcripts_30x20mb_first_tick", |bench| {
        let w: &World = world.get_or_init(build);
        bench.iter_batched(
            || -> Vec<Box<dyn Overlay>> {
                vec![
                    Box::new(ClaudeState::new(Some(w.sessions.clone()), w.a.clone())),
                    Box::new(Transcripts::new(w.roots.clone(), w.a.clone()).with_home(&w.home)),
                ]
            },
            |ov| {
                flip = !flip;
                let mut snap = sampler.tick(if flip { &w.a } else { &w.b }).unwrap();
                overlay::apply(&ov, &mut snap.sessions);
                black_box(snap)
            },
            BatchSize::SmallInput,
        )
    });

    // Diretorios de handoff parados: nenhuma medida relistou algum.
    if let Some(w) = world.get() {
        for tr in &w.shared {
            assert_eq!(
                tr.handoff_readdirs(),
                HANDOFF_DIRS,
                "handoff relistado em regime"
            );
        }
        eprintln!(
            "transcripts: handoff readdirs em regime = {HANDOFF_DIRS} (so a primeira leitura)"
        );
    }
}

/// Sessoes `claude` de [`subagents`], fora do `synthetic(500)` (que so tem
/// 19): pids 800..830, filhas do `herdr`.
const SUB_SESSIONS: u32 = 30;
const SUB_PID: u32 = 800;
/// Subagentes terminados por sessao.
const SUB_PER_SESSION: u32 = 200;
const SUB_FIXTURE: &str =
    "claude/-home-dev-app/5c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f/subagents/agent-a3nded000000000002";

struct SubWorld {
    _dir: TempDir,
    a: ProcRoot,
    b: ProcRoot,
    /// Os transcripts de `with` e de `cached`, para o contador de leituras.
    shared: [Arc<Transcripts>; 2],
    base: Vec<Box<dyn Overlay>>,
    with: Vec<Box<dyn Overlay>>,
    cached: Vec<Box<dyn Overlay>>,
}

/// 30 sessoes Claude com transcript pequeno e 200 subagentes terminados
/// (`end_turn`, `.jsonl` e `.meta.json`) cada; arquivos e diretorios parados
/// ha uma hora. A primeira leitura (6000 arquivos, `subagents::READ_BUDGET`
/// por tick) fica fora da medida.
fn build_subagents() -> SubWorld {
    let t0 = Instant::now();
    let dir = TempDir::new("ac-bench-subagents").unwrap();
    let data = dir.path().join("data");
    let sessions = dir.path().join("claude-sessions");
    fs::create_dir_all(&sessions).unwrap();
    let transcript = fixture("claude/-home-dev-app/0b6f4a52-9c1e-4d7a-8f3b-2a1c5e7d9f01.jsonl");
    let agent = fixture(&format!("{SUB_FIXTURE}.jsonl"));
    let meta = fixture(&format!("{SUB_FIXTURE}.meta.json"));
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    let a = synthetic(dir.path().join("a"), 500, 1, true).unwrap();
    let b = synthetic(dir.path().join("b"), 500, 2, true).unwrap();
    for pid in SUB_PID..SUB_PID + SUB_SESSIONS {
        let cwd = format!("/home/dev/sub-{pid}");
        for fp in [&a, &b] {
            let mut p = FakeProcess::new(pid, 2, "claude");
            p.starttime = START;
            p.threads = 20;
            p.cwd = Some(cwd.clone());
            fp.add(&p).unwrap();
        }
        let sid = format!("00000000-0000-4000-8000-{pid:012}");
        fs::write(
            sessions.join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"{sid}\",\"procStart\":\"{START}\",\"status\":\"busy\"}}"),
        )
        .unwrap();
        let project = data.join(format!("claude/{}", cwd.replace('/', "-")));
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(format!("{sid}.jsonl")), &transcript).unwrap();
        let sub = project.join(&sid).join("subagents");
        fs::create_dir_all(&sub).unwrap();
        for j in 0..SUB_PER_SESSION {
            let stem = format!("agent-a{pid:08}{j:08}");
            let f = sub.join(format!("{stem}.jsonl"));
            fs::write(&f, &agent).unwrap();
            fs::File::options()
                .append(true)
                .open(&f)
                .unwrap()
                .set_modified(hour_ago)
                .unwrap();
            fs::write(sub.join(format!("{stem}.meta.json")), &meta).unwrap();
        }
        fs::File::open(&sub)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
    }
    let (a, b) = (a.proc_root(), b.proc_root());
    let roots = Roots {
        claude: Some(data.join("claude")),
        codex: None,
        pi: None,
    };
    let state = || Box::new(ClaudeState::new(Some(sessions.clone()), a.clone()));
    let tr = || Transcripts::new(roots.clone(), a.clone());
    let shared = [Arc::new(tr().with_refresh(Duration::ZERO)), Arc::new(tr())];
    let base: Vec<Box<dyn Overlay>> = vec![state()];
    let with: Vec<Box<dyn Overlay>> = vec![state(), Box::new(Shared(shared[0].clone()))];
    let cached: Vec<Box<dyn Overlay>> = vec![state(), Box::new(Shared(shared[1].clone()))];
    let written = t0.elapsed();

    let t1 = Instant::now();
    let mut sampler = Sampler::new(Registry::default());
    let mut ticks = 0;
    for ov in [&with, &cached] {
        loop {
            let mut snap = sampler.tick(&a).unwrap();
            overlay::apply(ov, &mut snap.sessions);
            ticks += 1;
            let done = snap
                .sessions
                .iter()
                .filter_map(|s| s.overlay.as_ref())
                .filter(|o| {
                    o.subagents_live == Some(0)
                        && o.subagents_total == Some(u64::from(SUB_PER_SESSION))
                })
                .count();
            if done == SUB_SESSIONS as usize {
                break;
            }
            assert!(ticks < 5_000, "os 30 diretorios lidos ({done})");
        }
    }
    eprintln!(
        "subagents: {SUB_SESSIONS} x {SUB_PER_SESSION} gravados em {written:?}; primeira \
         leitura (2 overlays) em {ticks} ticks, {:?}",
        t1.elapsed()
    );
    let want = u64::from(SUB_SESSIONS * SUB_PER_SESSION);
    for tr in &shared {
        assert_eq!(tr.subagent_tail_reads(), want, "cada arquivo lido uma vez");
    }
    SubWorld {
        _dir: dir,
        a,
        b,
        shared,
        base,
        with,
        cached,
    }
}

/// Custo dos subagentes (C15, criterio 4): 30 sessoes x 200 subagentes
/// terminados, em regime; orcamento < +0,3 ms na mediana.
///
/// - `subagents_30x200_base`: tick + `claude_state`, sem transcripts.
/// - `subagents_30x200_tick_cached`: + transcripts como no registry
///   (conferencia a cada 5 s). O A/B contra a main e neste id (la os
///   transcripts nao olham subagentes).
/// - `subagents_30x200_tick`: + transcripts conferidos em todo tick (pior
///   caso do regime: o tick em que a conferencia cai).
///
/// Ao fim, o contador [`Transcripts::subagent_tail_reads`] de cada overlay
/// segue em 6000: terminado lido uma vez, nunca de novo. Os ids nao comecam
/// por `tick_fixture_500`.
fn subagents(c: &mut Criterion) {
    let _env = isolate_env();
    let world = OnceCell::new();
    for (name, pick) in [
        ("subagents_30x200_base", 0),
        ("subagents_30x200_tick", 1),
        ("subagents_30x200_tick_cached", 2),
    ] {
        let mut sampler = Sampler::new(Registry::default());
        let mut flip = false;
        c.bench_function(name, |bench| {
            let w: &SubWorld = world.get_or_init(build_subagents);
            let ov = [&w.base, &w.with, &w.cached][pick];
            bench.iter(|| {
                flip = !flip;
                let mut snap = sampler.tick(if flip { &w.a } else { &w.b }).unwrap();
                overlay::apply(ov, &mut snap.sessions);
                black_box(snap)
            })
        });
    }
    if let Some(w) = world.get() {
        let want = u64::from(SUB_SESSIONS * SUB_PER_SESSION);
        for tr in &w.shared {
            assert_eq!(tr.subagent_tail_reads(), want, "terminado relido em regime");
        }
        eprintln!("subagents: leituras do fim em regime = {want} por overlay (so a primeira)");
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = transcripts, subagents
}
criterion_main!(benches);
