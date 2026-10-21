//! Overlay de transcripts sobre `tests/fixtures/transcripts` (sinteticas, com
//! `CANARY` em todo campo de conteudo) e um `/proc` falso.
//!
//! Valores esperados: os de `contexto.py` do agentic-swarm para o mesmo
//! arquivo (`scan_<cliente>` + `analyse`, comando no card C4):
//! claude 205671/2670/12.2, codex 72500/2300/16.4, pi 38512/850/16.0.
//!
//! Tool calls (C14), total/em voo, os do `jq` sobre o mesmo arquivo (`-R` +
//! `fromjson?` pula a linha quebrada do Claude, que o parser tambem pula):
//! claude 4/2, codex 3/1 (idle), pi 3/1 (busy).
//!
//! ```text
//! jq -cR 'fromjson? | select(.type=="assistant") | .message.content[]?
//!     | select(.type=="tool_use")' claude.jsonl | wc -l
//! jq -c 'select(.type=="response_item") | .payload
//!     | select(.type=="function_call" or .type=="custom_tool_call")' codex.jsonl | wc -l
//! jq -c 'select(.type=="message" and .message.role=="assistant")
//!     | .message.content[]? | select(.type=="toolCall")' pi.jsonl | wc -l
//! ```
//!
//! Em voo: ids de chamada menos ids de resultado (`tool_result.tool_use_id` em
//! linha `user`; `*_output.call_id`; `toolResult.toolCallId`).
//!
//! Sinal de loop (C23), `(max_streak, maior contagem)` de `contexto.loops()`
//! sobre o `st["calls"]` de `scan_<cliente>`: claude 0b6f 2/2, claude 5c1d
//! 1/1, codex 01a13 1/1, pi 1/1, codex de 16/10 (sem chamadas) 1/0, e a
//! fixture `tests/fixtures/loop` 2/3 (LOOP, "chamada repetida 3x").

use agentcenter_core::cockpit::{Cockpit, Loops, Reason, Recommendation, Thresholds};
use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::overlay::claude_state::ClaudeState;
use agentcenter_core::overlay::transcript::{Format, READ_BUDGET, Roots, Transcripts};
use agentcenter_core::overlay::{self, HerdrOverlay, Overlay, OverlayStatus, SessionOverlay};
use agentcenter_core::session::Session;
use agentcenter_core::summary::session_state;
use agentcenter_core::{Registry, Sampler, SessionState, Snapshot, Summary};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CANARY: &str = "AGENTCENTER-CANARY-7f3a9c";
const SID: &str = "0b6f4a52-9c1e-4d7a-8f3b-2a1c5e7d9f01";
const CWD: &str = "/home/dev/app";
const START: u64 = 5000;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcripts")
}

fn claude_file(root: &Path) -> PathBuf {
    root.join("claude/-home-dev-app")
        .join(format!("{SID}.jsonl"))
}

fn codex_file(root: &Path) -> PathBuf {
    root.join(
        "codex/2026/10/15/rollout-2026-10-15T01-14-06-01a13dc4-6ff3-7fb2-a708-0d87a9e28591.jsonl",
    )
}

fn pi_file(root: &Path) -> PathBuf {
    root.join(
        "pi/--home-dev-app--/2026-10-17T02-53-28-810Z_01a147c7-5769-739f-8d7a-6e1addc5ac7d.jsonl",
    )
}

fn roots(root: &Path) -> Roots {
    Roots {
        claude: Some(root.join("claude")),
        codex: Some(root.join("codex")),
        pi: Some(root.join("pi")),
    }
}

/// (tokens_in, tokens_out, context_pct, modelo) de `contexto.py`.
fn expected(f: Format) -> (u64, u64, f64, &'static str) {
    match f {
        Format::Claude => (205_671, 2_670, 12.2, "claude-opus-5-5"),
        Format::Codex => (72_500, 2_300, 16.4, "gpt-5.5-codex"),
        Format::Pi => (38_512, 850, 16.0, "claude-opus-5-5"),
    }
}

/// (tool_calls, tool_calls_open, estado do transcript) do `jq` (ver topo).
fn expected_calls(f: Format) -> (u64, u64, Option<OverlayStatus>) {
    match f {
        Format::Claude => (4, 2, None),
        Format::Codex => (3, 1, Some(OverlayStatus::Idle)),
        Format::Pi => (3, 1, Some(OverlayStatus::Busy)),
    }
}

fn calls(o: &SessionOverlay) -> (Option<u64>, Option<u64>) {
    (o.tool_calls, o.tool_calls_open)
}

fn numbers(o: &SessionOverlay) -> (Option<u64>, Option<u64>, Option<f64>, Option<&str>) {
    (o.tokens_in, o.tokens_out, o.context_pct, o.model.as_deref())
}

fn want(f: Format) -> (Option<u64>, Option<u64>, Option<f64>, Option<&'static str>) {
    let (i, o, c, m) = expected(f);
    (Some(i), Some(o), Some(c), Some(m))
}

/// `/proc` com claude 100, codex 120 (rollout aberto no fd 3) e pi 130, todos
/// em `CWD`; `sessions/100.json` liga o claude ao `SID`.
fn world(t: &TempDir, data: &Path) -> (FakeProc, PathBuf) {
    let fp = FakeProc::create(t.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    for (pid, comm) in [(100, "claude"), (120, "codex"), (130, "pi")] {
        let mut p = FakeProcess::new(pid, 1, comm);
        p.starttime = START;
        p.cwd = Some(CWD.to_string());
        fp.add(&p).unwrap();
    }
    let fd = fp.root().join("120/fd");
    fs::create_dir_all(&fd).unwrap();
    std::os::unix::fs::symlink("/dev/null", fd.join("0")).unwrap();
    std::os::unix::fs::symlink(codex_file(data), fd.join("3")).unwrap();
    let sessions = t.path().join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("100.json"),
        format!(
            "{{\"pid\":100,\"sessionId\":\"{SID}\",\"procStart\":\"{START}\",\"status\":\"busy\"}}"
        ),
    )
    .unwrap();
    (fp, sessions)
}

/// Um tick so com os overlays sob teste (o do registry olharia o HOME real).
fn tick(fp: &FakeProc, overlays: &[Box<dyn Overlay>]) -> Snapshot {
    let mut snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    for s in &mut snap.sessions {
        s.overlay = None;
    }
    overlay::apply(overlays, &mut snap.sessions);
    snap
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

/// O overlay de transcripts dividido com o teste, que le o contador de bytes.
struct Shared(Arc<Transcripts>);

impl Overlay for Shared {
    fn id(&self) -> &str {
        self.0.id()
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        self.0.enrich(session, overlay);
    }
}

/// `claude_state` (da o `sessionId`) e depois `tr`, como no registry.
fn stack(
    fp: &FakeProc,
    sessions: PathBuf,
    tr: Transcripts,
) -> (Vec<Box<dyn Overlay>>, Arc<Transcripts>) {
    let tr = Arc::new(tr);
    let ov: Vec<Box<dyn Overlay>> = vec![
        Box::new(ClaudeState::new(Some(sessions), fp.proc_root())),
        Box::new(Shared(tr.clone())),
    ];
    (ov, tr)
}

fn overlays(fp: &FakeProc, sessions: PathBuf, data: &Path) -> Vec<Box<dyn Overlay>> {
    let tr = Transcripts::new(roots(data), fp.proc_root()).with_refresh(Duration::ZERO);
    stack(fp, sessions, tr).0
}

#[test]
fn transcript_numbers_match_contexto_py() {
    let data = fixtures();
    for (f, path) in [
        (Format::Claude, claude_file(&data)),
        (Format::Codex, codex_file(&data)),
        (Format::Pi, pi_file(&data)),
    ] {
        let u = Transcripts::read_file(&path, f).unwrap();
        let m = u.meta(f);
        let (i, o, c, model) = expected(f);
        assert_eq!(
            (m.tokens_in, m.tokens_out, m.context_pct, m.model.as_deref()),
            (Some(i), Some(o), Some(c), Some(model)),
            "{f:?}"
        );
        assert!(m.last_at_ms.is_some(), "{f:?}");
    }
}

/// Cockpit de `contexto.analyse(client, 'x', path, '', mtime + 60)` com HOME
/// isolado (sem handoff, ocio de 1 min), comando e saida no card C21:
/// (window, window_inferred, ctx_pct, burn_weighted, reread_share,
/// reread_ratio, payback_turns, veredito, cache_hit, burn_pct, piso).
#[allow(clippy::type_complexity)]
fn expected_cockpit(
    f: Format,
) -> (
    u64,
    bool,
    f64,
    u64,
    f64,
    f64,
    Option<f64>,
    Recommendation,
    f64,
    f64,
    u64,
) {
    use Recommendation::Continuar;
    match f {
        Format::Claude => (
            1_000_000,
            true,
            12.2,
            92_811,
            0.166,
            0.2,
            Some(7.3),
            Continuar,
            0.751,
            0.5,
            898_143,
        ),
        Format::Codex => (
            258_400,
            false,
            16.4,
            37_575,
            0.138,
            0.161,
            Some(240.0),
            Continuar,
            0.717,
            0.2,
            232_365,
        ),
        Format::Pi => (
            128_000, true, 16.0, 31_687, 0.057, 0.06, None, Continuar, 0.467, 0.2, 115_118,
        ),
    }
}

#[test]
fn transcript_cockpit_matches_contexto_py_per_fixture() {
    let data = fixtures();
    for (f, path) in [
        (Format::Claude, claude_file(&data)),
        (Format::Codex, codex_file(&data)),
        (Format::Pi, pi_file(&data)),
    ] {
        let u = Transcripts::read_file(&path, f).unwrap();
        let t = u.tokens(f).unwrap();
        let c = Cockpit::compute(&Thresholds::default(), &t, Some(1.0), None);
        let ctx_pct = agentcenter_core::cockpit::round_n(
            agentcenter_core::cockpit::ctx_pct(c.ctx_tokens, c.window),
            1,
        );
        assert_eq!(
            (
                c.window,
                c.window_inferred,
                ctx_pct,
                c.burn_weighted,
                c.reread_share,
                c.reread_ratio,
                c.payback_turns,
                c.recommendation,
                c.cache_hit.unwrap(),
                c.burn_pct,
                c.floor_tokens,
            ),
            expected_cockpit(f),
            "{f:?}"
        );
        assert!(c.reasons.is_empty(), "{f:?}: {:?}", c.reasons);
        assert_eq!((c.idle_min, c.handoff_age_min), (Some(1.0), None), "{f:?}");
        assert_eq!(c.handoff_name, None, "{f:?}");
    }
}

#[test]
fn transcript_codex_and_pi_declare_session_id_and_provider() {
    let data = fixtures();
    let codex = Transcripts::read_file(&codex_file(&data), Format::Codex).unwrap();
    assert_eq!(
        codex.session_id.as_deref(),
        Some("01a13dc4-6ff3-7fb2-a708-0d87a9e28591")
    );
    // O session_meta da fixture nao tem model_provider: o provider sai do agente.
    assert_eq!(codex.provider, None);
    let pi = Transcripts::read_file(&pi_file(&data), Format::Pi).unwrap();
    assert_eq!(
        pi.session_id.as_deref(),
        Some("01a147c7-5769-739f-8d7a-6e1addc5ac7d")
    );
    assert_eq!(pi.provider.as_deref(), Some("anthropic"));
    let claude = Transcripts::read_file(&claude_file(&data), Format::Claude).unwrap();
    assert_eq!((claude.session_id, claude.provider), (None, None));

    // No overlay: o id do transcript (Codex e pi, sem o do claude_state) e o
    // provider canonico.
    let t = TempDir::new("ac-transcript-sid").unwrap();
    let (fp, sessions) = world(&t, &data);
    let ov = overlays(&fp, sessions, &data);
    tick(&fp, &ov);
    // O pi casa por cwd + inicio: so sai no segundo tick.
    let snap = tick(&fp, &ov);
    for (pid, sid, provider) in [
        (100, SID, "anthropic"),
        (120, "01a13dc4-6ff3-7fb2-a708-0d87a9e28591", "openai"),
        (130, "01a147c7-5769-739f-8d7a-6e1addc5ac7d", "anthropic"),
    ] {
        let o = by_pid(&snap, pid);
        assert_eq!(o.agent_session_id.as_deref(), Some(sid), "{pid}");
        assert_eq!(o.provider.as_deref(), Some(provider), "{pid}");
        assert!(o.cockpit.is_some() && !o.cockpit_pending, "{pid}");
    }
}

#[test]
fn transcript_tool_calls_match_jq_per_fixture() {
    let data = fixtures();
    for (f, path) in [
        (Format::Claude, claude_file(&data)),
        (Format::Codex, codex_file(&data)),
        (Format::Pi, pi_file(&data)),
    ] {
        let m = Transcripts::read_file(&path, f).unwrap().meta(f);
        let (total, open, status) = expected_calls(f);
        assert_eq!(
            (m.tool_calls, m.tool_calls_open, m.status),
            (Some(total), Some(open), status),
            "{f:?}"
        );
        // Linhas de 2026-10-15/17: fora da janela de 5 min de agora.
        assert_eq!(m.tool_calls_per_min, Some(0.0), "{f:?}");
    }
}

#[test]
fn transcript_tool_calls_reach_overlay_and_summary_without_content() {
    let t = TempDir::new("ac-transcript-tools").unwrap();
    let data = fixtures();
    let (fp, sessions) = world(&t, &data);
    let ov = overlays(&fp, sessions, &data);
    tick(&fp, &ov);
    let mut snap = tick(&fp, &ov);
    for (pid, f) in [
        (100, Format::Claude),
        (120, Format::Codex),
        (130, Format::Pi),
    ] {
        let o = by_pid(&snap, pid);
        let (total, open, status) = expected_calls(f);
        assert_eq!(calls(&o), (Some(total), Some(open)), "{f:?}");
        assert_eq!(o.tool_calls_per_min, Some(0.0), "{f:?}");
        // O Claude ja tem estado do claude_state; o transcript nao o troca.
        let status = status.or(Some(OverlayStatus::Busy));
        assert_eq!(o.status, status, "{f:?}");
    }
    snap.summary = Summary::of(&snap.sessions);
    assert_eq!(snap.summary.tool_calls_open, Some(2 + 1 + 1));
    assert_eq!(snap.summary.tool_calls_per_min, Some(0.0));
    let json = serde_json::to_string(&snap).unwrap();
    assert!(json.contains("\"tool_calls\":4"), "{json}");
    assert!(!json.contains(CANARY));
    assert!(!format!("{snap:?}").contains(CANARY));
}

/// Pane do herdr com `status` fixo para as sessoes Codex e pi, no lugar do
/// overlay do herdr (que falaria com um servidor real).
struct PaneStatus(&'static str);

impl Overlay for PaneStatus {
    fn id(&self) -> &str {
        "pane-status"
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        if matches!(session.pid, 120 | 130) {
            overlay.herdr = Some(HerdrOverlay {
                status: Some(self.0.to_string()),
                ..HerdrOverlay::default()
            });
        }
    }
}

#[test]
fn transcript_status_does_not_override_herdr_blocked_pane() {
    let t = TempDir::new("ac-transcript-herdr-blocked").unwrap();
    let data = fixtures();
    let (fp, sessions) = world(&t, &data);
    let tr = Transcripts::new(roots(&data), fp.proc_root()).with_refresh(Duration::ZERO);
    // Ordem do registry: claude_state, herdr, transcripts.
    let ov: Vec<Box<dyn Overlay>> = vec![
        Box::new(ClaudeState::new(Some(sessions), fp.proc_root())),
        Box::new(PaneStatus("blocked")),
        Box::new(tr),
    ];
    tick(&fp, &ov);
    let snap = tick(&fp, &ov);
    for (pid, f) in [(120, Format::Codex), (130, Format::Pi)] {
        let s = snap.sessions.iter().find(|s| s.pid == pid).unwrap();
        let o = s.overlay.clone().unwrap_or_default();
        // O transcript foi lido (contagens la), mas o estado e o do pane.
        let (total, open, _) = expected_calls(f);
        assert_eq!(calls(&o), (Some(total), Some(open)), "{f:?}");
        assert_eq!(o.status, None, "{f:?}");
        assert_eq!(session_state(s), SessionState::Blocked, "{f:?}");
    }
    let sum = Summary::of(&snap.sessions);
    // Claude busy (sessions file); Codex e pi blocked (pane).
    assert_eq!((sum.status.busy, sum.status.blocked), (1, 2));
}

#[test]
fn transcript_status_fills_when_herdr_pane_status_is_not_recognized() {
    let t = TempDir::new("ac-transcript-herdr-unknown").unwrap();
    let data = fixtures();
    let (fp, sessions) = world(&t, &data);
    let tr = Transcripts::new(roots(&data), fp.proc_root()).with_refresh(Duration::ZERO);
    // Valor que o herdr ainda nao publica: nao e estado declarado.
    let ov: Vec<Box<dyn Overlay>> = vec![
        Box::new(ClaudeState::new(Some(sessions), fp.proc_root())),
        Box::new(PaneStatus("thinking_v2")),
        Box::new(tr),
    ];
    tick(&fp, &ov);
    let snap = tick(&fp, &ov);
    for (pid, f) in [(120, Format::Codex), (130, Format::Pi)] {
        let s = snap.sessions.iter().find(|s| s.pid == pid).unwrap();
        let o = s.overlay.clone().unwrap_or_default();
        let (_, _, status) = expected_calls(f);
        assert!(status.is_some(), "{f:?}: fixture sem estado no transcript");
        assert_eq!(o.status, status, "{f:?}");
        assert_ne!(session_state(s), SessionState::Unknown, "{f:?}");
    }
}

#[test]
fn transcript_overlay_fills_claude_codex_and_pi_sessions() {
    let t = TempDir::new("ac-transcript-fill").unwrap();
    let data = fixtures();
    let (fp, sessions) = world(&t, &data);
    let ov = overlays(&fp, sessions, &data);
    let snap = tick(&fp, &ov);
    assert_eq!(numbers(&by_pid(&snap, 100)), want(Format::Claude));
    assert_eq!(numbers(&by_pid(&snap, 120)), want(Format::Codex));
    // O pi casa por cwd + inicio (sem fd): so sai no tick seguinte, depois
    // que o desempate viu todos os processos.
    assert_eq!(by_pid(&snap, 130), SessionOverlay::default());
    let snap = tick(&fp, &ov);
    assert_eq!(numbers(&by_pid(&snap, 100)), want(Format::Claude));
    assert_eq!(numbers(&by_pid(&snap, 120)), want(Format::Codex));
    assert_eq!(numbers(&by_pid(&snap, 130)), want(Format::Pi));
}

#[test]
fn transcript_content_never_reaches_the_snapshot() {
    let t = TempDir::new("ac-transcript-canary").unwrap();
    let data = fixtures();
    for f in [claude_file(&data), codex_file(&data), pi_file(&data)] {
        assert!(fs::read_to_string(&f).unwrap().contains(CANARY), "{f:?}");
    }
    let (fp, sessions) = world(&t, &data);
    let ov = overlays(&fp, sessions, &data);
    tick(&fp, &ov);
    let snap = tick(&fp, &ov);
    let filled = snap
        .sessions
        .iter()
        .filter(|s| s.overlay.as_ref().is_some_and(|o| o.context_pct.is_some()))
        .count();
    assert_eq!(filled, 3, "overlay ran nas tres sessoes");
    assert!(!serde_json::to_string(&snap).unwrap().contains(CANARY));
    assert!(!format!("{snap:?}").contains(CANARY));
}

#[test]
fn transcript_claude_needs_the_session_id_from_claude_state() {
    let t = TempDir::new("ac-transcript-nosid").unwrap();
    let data = fixtures();
    let (fp, _) = world(&t, &data);
    let only: Vec<Box<dyn Overlay>> = vec![Box::new(
        Transcripts::new(roots(&data), fp.proc_root()).with_refresh(Duration::ZERO),
    )];
    let snap = tick(&fp, &only);
    // Nada do transcript; so o provider, que vem do agente (C21).
    let only_provider = SessionOverlay {
        provider: Some("anthropic".into()),
        ..SessionOverlay::default()
    };
    assert_eq!(by_pid(&snap, 100), only_provider);
    assert_eq!(numbers(&by_pid(&snap, 120)), want(Format::Codex));
}

/// Overlays sobre uma copia gravavel da fixture do Claude, mais o
/// `Transcripts` deles (para o contador de bytes).
struct Writable {
    fp: FakeProc,
    data: PathBuf,
    file: PathBuf,
    ov: Vec<Box<dyn Overlay>>,
    tr: Arc<Transcripts>,
}

fn writable(t: &TempDir) -> Writable {
    writable_with(t, |tr| tr)
}

/// [`writable`] com o `Transcripts` ajustado por `cfg`. So o claude 100 tem
/// transcript nesta raiz (o codex e o pi nao casam com nada).
fn writable_with(t: &TempDir, cfg: impl FnOnce(Transcripts) -> Transcripts) -> Writable {
    let data = t.path().join("data");
    let file = claude_file(&data);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::copy(claude_file(&fixtures()), &file).unwrap();
    let (fp, sessions) = world(t, &fixtures());
    let tr = cfg(Transcripts::new(roots(&data), fp.proc_root()).with_refresh(Duration::ZERO));
    let (ov, tr) = stack(&fp, sessions, tr);
    Writable {
        fp,
        data,
        file,
        ov,
        tr,
    }
}

fn assistant(rid: &str, ctx: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"2026-10-15T10:01:00.000Z\",\"requestId\":\"{rid}\",\
         \"message\":{{\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"{CANARY}\"}}],\
         \"usage\":{{\"input_tokens\":{ctx},\"cache_creation_input_tokens\":0,\
         \"cache_read_input_tokens\":0,\"output_tokens\":10}}}}}}\n"
    )
}

fn append(path: &Path, s: &str) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(s.as_bytes())
        .unwrap();
}

#[test]
fn transcript_second_refresh_reads_only_the_appended_bytes() {
    let t = TempDir::new("ac-transcript-append").unwrap();
    let Writable {
        fp, file, ov, tr, ..
    } = writable(&t);
    let snap = tick(&fp, &ov);
    let size = fs::metadata(&file).unwrap().len();
    assert_eq!(tr.bytes_read(), size, "primeira leitura: o arquivo todo");
    assert_eq!(by_pid(&snap, 100).context_pct, Some(12.2));

    // Sem mudanca: nada lido.
    tick(&fp, &ov);
    assert_eq!(tr.bytes_read(), size);

    let line = assistant("req_04", 500_000);
    append(&file, &line);
    let snap = tick(&fp, &ov);
    assert_eq!(
        tr.bytes_read(),
        size + line.len() as u64,
        "so os bytes novos"
    );
    let o = by_pid(&snap, 100);
    assert_eq!((o.context_pct, o.tokens_out), (Some(50.0), Some(2_680)));
}

/// Linha `assistant` do Claude so com a chamada `id` (sem `usage`).
fn tool_use(id: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"2026-10-15T10:02:00.000Z\",\
         \"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\
         \"name\":\"Bash\",\"input\":{{\"command\":\"{CANARY}\"}}}}]}}}}\n"
    )
}

/// Linha `user` do Claude com o resultado da chamada `id`.
fn tool_result(id: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-10-15T10:02:01.000Z\",\
         \"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\
         \"tool_use_id\":\"{id}\",\"content\":\"{CANARY}\",\"is_error\":false}}]}}}}\n"
    )
}

#[test]
fn transcript_appended_tool_call_opens_then_closes_reading_only_new_bytes() {
    let t = TempDir::new("ac-transcript-tools-append").unwrap();
    let Writable {
        fp, file, ov, tr, ..
    } = writable(&t);
    // Comeca sem chamada nenhuma: so um pedido.
    fs::write(&file, assistant("req_01", 100_000)).unwrap();
    let snap = tick(&fp, &ov);
    let mut size = fs::metadata(&file).unwrap().len();
    assert_eq!(tr.bytes_read(), size);
    assert_eq!(calls(&by_pid(&snap, 100)), (Some(0), Some(0)));

    for (line, want) in [
        (tool_use("tu9"), (Some(1), Some(1))),
        (tool_result("tu9"), (Some(1), Some(0))),
    ] {
        append(&file, &line);
        let snap = tick(&fp, &ov);
        size += line.len() as u64;
        assert_eq!(tr.bytes_read(), size, "so os bytes novos");
        let o = by_pid(&snap, 100);
        assert_eq!(calls(&o), want);
        assert_eq!(o.context_pct, Some(10.0), "tokens seguem os do pedido");
        assert!(!serde_json::to_string(&snap).unwrap().contains(CANARY));
    }
}

#[test]
fn transcript_read_budget_caps_each_tick_and_resumes_mid_line() {
    const BUDGET: u64 = 200;
    let t = TempDir::new("ac-transcript-budget").unwrap();
    let Writable {
        fp, file, ov, tr, ..
    } = writable_with(&t, |tr| tr.with_read_budget(BUDGET));
    let text = fs::read_to_string(&file).unwrap();
    let size = text.len() as u64;
    assert!(
        text.lines().any(|l| l.len() as u64 > 2 * BUDGET),
        "alguma linha atravessa mais de um tick"
    );
    // Bytes da varredura incremental: o total menos o fim lido de tras para
    // frente (que tem teto proprio, por arquivo).
    let scanned = |tr: &Transcripts| tr.bytes_read() - tr.tail_bytes_read();
    let mut ticks = 0;
    let mut before = 0;
    let mut tail = None;
    loop {
        let snap = tick(&fp, &ov);
        ticks += 1;
        let read = scanned(&tr) - before;
        before = scanned(&tr);
        assert!(read <= BUDGET, "tick {ticks}: {read} bytes");
        // O fim e lido uma vez so: o arquivo nao cresceu.
        let t = *tail.get_or_insert(tr.tail_bytes_read());
        assert_eq!(tr.tail_bytes_read(), t, "tick {ticks}: fim relido");
        if before < size {
            assert_eq!(read, BUDGET, "tick {ticks}: teto inteiro enquanto falta");
            let o = by_pid(&snap, 100);
            assert_eq!(
                (o.tokens_in, o.tokens_out),
                (None, None),
                "tick {ticks}: leitura incompleta nao publica acumulados"
            );
            assert_eq!(
                (o.context_pct, o.model.as_deref()),
                (Some(12.2), Some("claude-opus-5-5")),
                "tick {ticks}: contexto e modelo vem do fim"
            );
        } else {
            assert_eq!(numbers(&by_pid(&snap, 100)), want(Format::Claude));
            break;
        }
        assert!(ticks < 1000);
    }
    assert_eq!(before, size, "nenhum byte lido duas vezes");
    assert_eq!(ticks, size.div_ceil(BUDGET));
    // Depois da primeira leitura completa, o acrescimo tambem respeita o teto
    // e os numeros ja publicados ficam ate a linha nova completar.
    let line = assistant("req_04", 500_000);
    append(&file, &line);
    for _ in 1..(line.len() as u64).div_ceil(BUDGET) {
        let snap = tick(&fp, &ov);
        assert_eq!(by_pid(&snap, 100).context_pct, Some(12.2));
    }
    let snap = tick(&fp, &ov);
    assert_eq!(by_pid(&snap, 100).context_pct, Some(50.0));
    assert_eq!(scanned(&tr), size + line.len() as u64);
    let t = tail.unwrap();
    assert!(t > 0 && t <= size, "fim lido: {t} de {size}");
    assert_eq!(
        tr.tail_bytes_read(),
        t,
        "depois de pronto, nada de tras para frente"
    );
}

/// Copia `src` em `dst` com linhas sem uso (e com o `CANARY`) logo depois da
/// primeira, ate passar de `min` bytes.
fn padded(src: &Path, dst: &Path, min: u64) {
    let text = fs::read_to_string(src).unwrap();
    let (head, rest) = text.split_at(text.find('\n').unwrap() + 1);
    let pad = format!(
        "{{\"type\":\"padding\",\"text\":\"{}\"}}\n",
        CANARY.repeat(400)
    );
    let mut out = head.to_string();
    while (out.len() as u64) < min {
        out.push_str(&pad);
    }
    out.push_str(rest);
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::write(dst, out).unwrap();
}

#[test]
fn transcript_bigger_than_read_budget_shows_context_on_first_refresh() {
    let t = TempDir::new("ac-transcript-tail").unwrap();
    let data = t.path().join("data");
    let min = READ_BUDGET + READ_BUDGET / 2;
    for file in [claude_file, codex_file, pi_file] {
        padded(&file(&fixtures()), &file(&data), min);
        assert!(fs::metadata(file(&data)).unwrap().len() > READ_BUDGET);
    }
    let (fp, sessions) = world(&t, &data);
    let tr = Transcripts::new(roots(&data), fp.proc_root()).with_refresh(Duration::ZERO);
    let (ov, tr) = stack(&fp, sessions, tr);
    let early = |f: Format| {
        let (_, _, c, m) = expected(f);
        (None, None, Some(c), Some(m))
    };
    // Primeiro tick: o claude gasta o teto do tick e o codex nao le nada da
    // varredura, mas os dois ja tem modelo e contexto %; o pi so casa no tick
    // seguinte (desempate).
    let snap = tick(&fp, &ov);
    assert_eq!(numbers(&by_pid(&snap, 100)), early(Format::Claude));
    assert_eq!(numbers(&by_pid(&snap, 120)), early(Format::Codex));
    assert_eq!(by_pid(&snap, 130), SessionOverlay::default());
    let snap = tick(&fp, &ov);
    assert_eq!(numbers(&by_pid(&snap, 130)), early(Format::Pi));
    assert!(!serde_json::to_string(&snap).unwrap().contains(CANARY));
    // A varredura termina e os acumulados aparecem, com os mesmos numeros.
    let mut ticks = 2;
    let snap = loop {
        let snap = tick(&fp, &ov);
        ticks += 1;
        if by_pid(&snap, 130).tokens_in.is_some() {
            break snap;
        }
        assert!(ticks < 20, "varredura nao terminou");
    };
    for (pid, f) in [
        (100, Format::Claude),
        (120, Format::Codex),
        (130, Format::Pi),
    ] {
        assert_eq!(numbers(&by_pid(&snap, pid)), want(f), "{f:?}");
    }
    assert!(tr.tail_bytes_read() > 0);
}

#[test]
fn transcript_partial_line_waits_until_complete() {
    let t = TempDir::new("ac-transcript-partial").unwrap();
    let Writable { fp, file, ov, .. } = writable(&t);
    tick(&fp, &ov);
    let line = assistant("req_04", 500_000);
    let (head, rest) = line.split_at(line.len() / 2);
    append(&file, head);
    let snap = tick(&fp, &ov);
    assert_eq!(
        by_pid(&snap, 100).context_pct,
        Some(12.2),
        "linha parcial ignorada"
    );
    append(&file, rest);
    let snap = tick(&fp, &ov);
    assert_eq!(by_pid(&snap, 100).context_pct, Some(50.0));
}

#[test]
fn transcript_truncated_or_replaced_file_is_read_from_scratch() {
    let t = TempDir::new("ac-transcript-trunc").unwrap();
    let Writable {
        fp,
        data,
        file,
        ov,
        tr,
    } = writable(&t);
    tick(&fp, &ov);

    // Truncado e reescrito menor: relido do zero.
    let small = assistant("req_x", 100_000);
    fs::write(&file, &small).unwrap();
    let before = tr.bytes_read();
    let snap = tick(&fp, &ov);
    assert_eq!(tr.bytes_read() - before, small.len() as u64);
    let o = by_pid(&snap, 100);
    assert_eq!((o.context_pct, o.tokens_out), (Some(10.0), Some(10)));

    // Trocado por rename (inode novo), mesmo tamanho: relido do zero.
    let other = data.join("novo.jsonl");
    fs::write(&other, assistant("req_y", 200_000)).unwrap();
    fs::rename(&other, &file).unwrap();
    let snap = tick(&fp, &ov);
    assert_eq!(by_pid(&snap, 100).context_pct, Some(20.0));
}

#[test]
fn transcript_refresh_waits_for_the_interval() {
    let t = TempDir::new("ac-transcript-every").unwrap();
    let data = t.path().join("data");
    let file = claude_file(&data);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::copy(claude_file(&fixtures()), &file).unwrap();
    let (fp, sessions) = world(&t, &fixtures());
    let (ov, tr) = stack(
        &fp,
        sessions,
        Transcripts::new(roots(&data), fp.proc_root()),
    );
    tick(&fp, &ov);
    let read = tr.bytes_read();
    append(&file, &assistant("req_04", 500_000));
    let snap = tick(&fp, &ov);
    assert_eq!(tr.bytes_read(), read, "dentro de 5 s: cache");
    assert_eq!(by_pid(&snap, 100).context_pct, Some(12.2));
}

#[test]
fn transcript_pi_without_fd_matches_by_cwd_and_ambiguity_stays_empty() {
    let t = TempDir::new("ac-transcript-pi").unwrap();
    let data = fixtures();
    let (fp, sessions) = world(&t, &data);
    // Outro pi no mesmo cwd: o unico transcript serve aos dois, entao a nenhum.
    let mut p = FakeProcess::new(131, 1, "pi");
    p.starttime = START;
    p.cwd = Some(CWD.to_string());
    fp.add(&p).unwrap();
    let ov = overlays(&fp, sessions, &data);
    let snap = tick(&fp, &ov);
    assert_eq!(by_pid(&snap, 130).context_pct, None);
    assert_eq!(by_pid(&snap, 131).context_pct, None);
    let snap = tick(&fp, &ov);
    assert_eq!(
        by_pid(&snap, 130).context_pct,
        None,
        "estavel no tick seguinte"
    );
    assert_eq!(by_pid(&snap, 131).context_pct, None);

    // Outro cwd: nada a casar.
    let mut q = FakeProcess::new(131, 1, "pi");
    q.cwd = Some("/home/dev/outro".to_string());
    fp.add(&q).unwrap();
    let snap = tick(&fp, &ov);
    assert_eq!(by_pid(&snap, 131).context_pct, None);
}

#[test]
fn transcript_written_before_the_process_started_is_not_matched() {
    let t = TempDir::new("ac-transcript-old").unwrap();
    let data = fixtures();
    let (fp, sessions) = world(&t, &data);
    // Boot "agora": o pi 130 comecou depois do ultimo mtime da fixture.
    let stat = fs::read_to_string(fp.root().join("stat")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        fp.root().join("stat"),
        stat.replace("btime 0", &format!("btime {now}")),
    )
    .unwrap();
    let ov = overlays(&fp, sessions, &data);
    let snap = tick(&fp, &ov);
    assert_eq!(by_pid(&snap, 130).context_pct, None);
    assert_eq!(
        numbers(&by_pid(&snap, 120)),
        want(Format::Codex),
        "fd vale sempre"
    );
}

/// So o codex 120, com o rollout da fixture aberto no fd 3 e cwd `cwd`.
fn codex_world(t: &TempDir, cwd: &Path) -> FakeProc {
    codex_world_on(t, cwd, &codex_file(&fixtures()))
}

/// [`codex_world`] com o rollout `rollout` no fd 3.
fn codex_world_on(t: &TempDir, cwd: &Path, rollout: &Path) -> FakeProc {
    let fp = FakeProc::create(t.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    let mut p = FakeProcess::new(120, 1, "codex");
    p.starttime = START;
    p.cwd = Some(cwd.to_string_lossy().into_owned());
    fp.add(&p).unwrap();
    let fd = fp.root().join("120/fd");
    fs::create_dir_all(&fd).unwrap();
    std::os::unix::fs::symlink(rollout, fd.join("3")).unwrap();
    fp
}

fn handoff_md(dir: &Path, name: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(name), format!("# {CANARY}\n")).unwrap();
}

fn cockpit_handoff(fp: &FakeProc, tr: Transcripts) -> Option<String> {
    let only: Vec<Box<dyn Overlay>> = vec![Box::new(tr.with_refresh(Duration::ZERO))];
    let o = by_pid(&tick(fp, &only), 120);
    let c = o.cockpit.expect("cockpit");
    assert_eq!(c.handoff_name.is_some(), c.handoff_age_min.is_some());
    c.handoff_name
}

#[test]
fn transcript_cockpit_finds_the_repo_handoff_without_home() {
    let t = TempDir::new("ac-transcript-ho-local").unwrap();
    let base = fs::canonicalize(t.path()).unwrap();
    let repo = base.join("projeto-local");
    fs::create_dir_all(repo.join(".git")).unwrap();
    handoff_md(&repo.join(".claude/handoffs"), "2026-10-17-local.md");
    let fp = codex_world(&t, &repo);
    // `Transcripts::new` nao tem `home`: o handoff do repo vale assim mesmo.
    let tr = Transcripts::new(roots(&fixtures()), fp.proc_root());
    assert_eq!(
        cockpit_handoff(&fp, tr).as_deref(),
        Some("2026-10-17-local.md")
    );
}

#[test]
fn pi_without_agentcenter_handoff_keeps_existing_local_discovery() {
    let t = TempDir::new("ac-pi-ho-local").unwrap();
    let repo = fs::canonicalize(t.path()).unwrap().join("projeto-pi");
    fs::create_dir_all(repo.join(".git")).unwrap();
    handoff_md(&repo.join(".claude/handoffs"), "pi-local.md");
    let fp = FakeProc::create(t.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    let mut p = FakeProcess::new(120, 1, "pi");
    p.starttime = START;
    p.cwd = Some(repo.to_string_lossy().into_owned());
    fp.add(&p).unwrap();
    let fd = fp.root().join("120/fd");
    fs::create_dir_all(&fd).unwrap();
    std::os::unix::fs::symlink(pi_file(&fixtures()), fd.join("3")).unwrap();
    let tr = Transcripts::new(roots(&fixtures()), fp.proc_root());
    assert_eq!(cockpit_handoff(&fp, tr).as_deref(), Some("pi-local.md"));
}

#[test]
fn transcript_cockpit_global_handoff_needs_home() {
    let t = TempDir::new("ac-transcript-ho-global").unwrap();
    let base = fs::canonicalize(t.path()).unwrap();
    let repo = base.join("projeto-global");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let home = base.join("home");
    handoff_md(
        &home.join(".claude/handoffs"),
        "projeto-global-2026-10-17.md",
    );
    let fp = codex_world(&t, &repo);
    // Sem handoff local: o global casa pelo nome do diretorio, so com `home`.
    let tr = Transcripts::new(roots(&fixtures()), fp.proc_root());
    assert_eq!(cockpit_handoff(&fp, tr), None);
    let tr = Transcripts::new(roots(&fixtures()), fp.proc_root()).with_home(&home);
    assert_eq!(
        cockpit_handoff(&fp, tr).as_deref(),
        Some("projeto-global-2026-10-17.md")
    );
}

/// Nome de handoff com ANSI, quebra de linha e BEL; `clean_name` o deixa assim.
const HOSTILE_HANDOFF: &str = "2026-10-17-\x1b[31mvermelho\x1b[0m\nlinha\x07.md";
const HOSTILE_HANDOFF_CLEAN: &str = "2026-10-17-vermelho linha.md";

fn set_dir_mtime(p: &Path, mtime: std::time::SystemTime) {
    fs::File::open(p).unwrap().set_modified(mtime).unwrap();
}

#[test]
fn transcript_handoff_content_and_hostile_names_never_reach_the_snapshot() {
    let t = TempDir::new("ac-transcript-ho-canary").unwrap();
    let base = fs::canonicalize(t.path()).unwrap();
    let repo = base.join("projeto-hostil");
    fs::create_dir_all(repo.join(".git")).unwrap();
    // O handoff tem o CANARY no conteudo e controle/ANSI no nome.
    let ho = repo.join(".claude/handoffs");
    handoff_md(&ho, HOSTILE_HANDOFF);
    assert!(
        fs::read_to_string(ho.join(HOSTILE_HANDOFF))
            .unwrap()
            .contains(CANARY)
    );
    // Rollout do Codex que declara um provider hostil (ANSI, quebra de linha
    // e o CANARY) no session_meta.
    // Dentro da raiz do Codex: fora dela o fd nao casa.
    let data = base.join("data");
    let rollout = codex_file(&data);
    fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    let src = fs::read_to_string(codex_file(&fixtures())).unwrap();
    let declared = format!("\\u001b[31mopenai\\u001b[0m\\n{CANARY}");
    let hostile = src.replacen(
        "\"originator\":",
        &format!("\"model_provider\":\"{declared}\",\"originator\":"),
        1,
    );
    assert_ne!(hostile, src);
    fs::write(&rollout, hostile).unwrap();
    let raw = Transcripts::read_file(&rollout, Format::Codex).unwrap();
    let raw = raw.provider.expect("o parser le o provider declarado");
    assert!(raw.contains(CANARY) && raw.contains('\x1b') && raw.contains('\n'));

    let fp = codex_world_on(&t, &repo, &rollout);
    let home = base.join("home");
    let tr = Transcripts::new(roots(&data), fp.proc_root())
        .with_home(&home)
        .with_refresh(Duration::ZERO);
    let only: Vec<Box<dyn Overlay>> = vec![Box::new(tr)];
    let mut snap = tick(&fp, &only);
    snap.summary = Summary::of(&snap.sessions);

    let o = by_pid(&snap, 120);
    let c = o.cockpit.as_ref().expect("cockpit");
    assert_eq!(c.handoff_name.as_deref(), Some(HOSTILE_HANDOFF_CLEAN));
    assert!(c.handoff_age_min.is_some());
    // O declarado nao passa no alfabeto do provider: vale o do agente.
    assert_eq!(o.provider.as_deref(), Some("openai"));
    // O provider sai em minusculas: o CANARY tambem nao entra assim.
    let lower = CANARY.to_lowercase();
    for field in [c.handoff_name.as_deref(), o.provider.as_deref()] {
        let field = field.unwrap();
        assert!(!field.chars().any(char::is_control), "{field:?}");
        assert!(!field.to_lowercase().contains(&lower), "{field:?}");
    }

    let json = serde_json::to_string(&snap).unwrap();
    assert!(json.contains(HOSTILE_HANDOFF_CLEAN), "{json}");
    assert!(!json.to_lowercase().contains(&lower), "{json}");
    assert!(
        !json.contains("\\u001b") && !json.contains('\x1b'),
        "{json}"
    );
    let debug = format!("{snap:?}");
    assert!(!debug.to_lowercase().contains(&lower));
    assert!(!debug.contains("\\u{1b}") && !debug.contains('\x1b'));
}

#[test]
fn transcript_handoff_directory_unchanged_is_not_listed_again() {
    let t = TempDir::new("ac-transcript-ho-readdir").unwrap();
    let base = fs::canonicalize(t.path()).unwrap();
    let repo = base.join("projeto-parado");
    fs::create_dir_all(repo.join(".git")).unwrap();
    let ho = repo.join(".claude/handoffs");
    for i in 0..100 {
        handoff_md(&ho, &format!("2026-10-17-{i:03}.md"));
    }
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    set_dir_mtime(&ho, hour_ago);
    let fp = codex_world(&t, &repo);
    // Conferencia em todo tick: o handoff e consultado em todos os tres.
    let tr = Arc::new(
        Transcripts::new(roots(&fixtures()), fp.proc_root())
            .with_home(base.join("home"))
            .with_refresh(Duration::ZERO),
    );
    let only: Vec<Box<dyn Overlay>> = vec![Box::new(Shared(tr.clone()))];
    for _ in 0..3 {
        let o = by_pid(&tick(&fp, &only), 120);
        assert!(o.cockpit.unwrap().handoff_name.is_some());
    }
    assert_eq!(tr.handoff_readdirs(), 1, "diretorio parado: uma listagem");

    // Handoff novo muda o mtime do diretorio: relista uma vez e o ve.
    handoff_md(&ho, "2026-10-18-novo.md");
    set_dir_mtime(&ho, hour_ago + Duration::from_secs(60));
    for _ in 0..2 {
        let o = by_pid(&tick(&fp, &only), 120);
        let name = o.cockpit.unwrap().handoff_name;
        assert_eq!(name.as_deref(), Some("2026-10-18-novo.md"));
    }
    assert_eq!(tr.handoff_readdirs(), 2);
}

/// Raiz com a fixture do sinal de loop (C23): um transcript do Claude em
/// `claude/-home-dev-app/<SID>.jsonl` com quatro chamadas, `Bash` tres vezes
/// com o mesmo `command` e `description`/`timeout`/`run_in_background`
/// diferentes, e um `Read` no meio. `contexto.loops()`: max_streak 2, a
/// mesma chamada 3x; `analyse` da LOOP, "chamada repetida 3x".
fn loop_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/loop")
}

fn codex_day(day: &str, rest: &str) -> PathBuf {
    fixtures().join(format!(
        "codex/2026/10/{day}/rollout-2026-10-{day}T{rest}.jsonl"
    ))
}

/// `(max_streak, maior contagem)` de `contexto.loops()` sobre `st["calls"]`
/// de `scan_<cliente>` (script no card C23; a maior contagem e a do
/// `Counter(calls[-30:])`). Sem chamadas o script da streak 1.
#[test]
fn transcript_loops_match_contexto_py_per_fixture() {
    let data = fixtures();
    let cases = [
        (Format::Claude, claude_file(&data), (2, 2)),
        (
            Format::Claude,
            data.join("claude/-home-dev-app/5c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f.jsonl"),
            (1, 1),
        ),
        (Format::Codex, codex_file(&data), (1, 1)),
        (Format::Pi, pi_file(&data), (1, 1)),
        (
            Format::Codex,
            codex_day("16", "10-00-00-01a20000-0000-7000-8000-000000000001"),
            (1, 0),
        ),
        (
            Format::Codex,
            codex_day("16", "10-01-00-01a20000-0000-7000-8000-00000000000a"),
            (1, 0),
        ),
        (
            Format::Codex,
            codex_day("16", "10-02-00-01a20000-0000-7000-8000-00000000000b"),
            (1, 0),
        ),
        (Format::Claude, claude_file(&loop_fixtures()), (2, 3)),
    ];
    for (f, path, (max_streak, repeated_max)) in cases {
        let m = Transcripts::read_file(&path, f).unwrap().meta(f);
        assert_eq!(
            m.loops,
            Some(Loops {
                max_streak,
                repeated_max
            }),
            "{f:?} {path:?}"
        );
    }
}

#[test]
fn transcript_three_identical_calls_are_a_loop_in_the_cockpit() {
    let t = TempDir::new("ac-transcript-loop").unwrap();
    let data = loop_fixtures();
    let (fp, sessions) = world(&t, &data);
    let ov = overlays(&fp, sessions, &data);
    let snap = tick(&fp, &ov);
    let c = by_pid(&snap, 100).cockpit.expect("cockpit");
    assert_eq!((c.max_streak, c.repeated_max), (2, 3));
    assert_eq!(c.recommendation, Recommendation::Loop);
    assert!(
        c.reasons.contains(&Reason::Loop { repeats: 3 }),
        "{:?}",
        c.reasons
    );
    // As fixtures de sempre: no maximo 2 iguais, sem LOOP.
    let data = fixtures();
    let t = TempDir::new("ac-transcript-noloop").unwrap();
    let (fp, sessions) = world(&t, &data);
    let ov = overlays(&fp, sessions, &data);
    let c = by_pid(&tick(&fp, &ov), 100).cockpit.expect("cockpit");
    assert_eq!((c.max_streak, c.repeated_max), (2, 2));
    assert_ne!(c.recommendation, Recommendation::Loop);
}

/// Linha `assistant` do Claude com uma chamada `name` de entrada `input`
/// (JSON cru), sem `usage`.
fn call_line(id: &str, name: &str, input: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"2026-10-17T10:00:00.000Z\",\
         \"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\
         \"id\":\"{id}\",\"name\":\"{name}\",\"input\":{input}}}]}}}}\n"
    )
}

/// O transcript da fixture do Claude mais as chamadas `inputs` de `Bash`.
fn claude_with_calls(t: &TempDir, name: &str, inputs: &[String]) -> PathBuf {
    let path = t.path().join(name);
    let mut s = fs::read_to_string(claude_file(&fixtures())).unwrap();
    for (i, input) in inputs.iter().enumerate() {
        s.push_str(&call_line(&format!("loop{i}"), "Bash", input));
    }
    fs::write(&path, s).unwrap();
    path
}

/// O CANARY so nos argumentos (chave, valor, aninhado, chave volatil): o
/// sinal os le (a contagem sobe para 3) e nada deles chega ao snapshot.
#[test]
fn transcript_tool_arguments_never_reach_the_snapshot() {
    let t = TempDir::new("ac-transcript-args-canary").unwrap();
    let data = t.path().join("data");
    let file = claude_file(&data);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let base = fs::read_to_string(claude_file(&fixtures())).unwrap();
    let marker = "CANARIO-ARGS-5e2b";
    assert!(!base.contains(marker));
    let input = format!(
        "{{\"command\":\"{marker}\",\"{marker}\":[{{\"k\":\"{marker}\"}}],\"description\":\"{marker}\"}}"
    );
    let mut s = base;
    for i in 0..3 {
        s.push_str(&call_line(&format!("c{i}"), marker, &input));
    }
    fs::write(&file, s).unwrap();
    let (fp, sessions) = world(&t, &fixtures());
    let ov = overlays(&fp, sessions, &data);
    let snap = tick(&fp, &ov);
    let c = by_pid(&snap, 100).cockpit.expect("cockpit");
    assert_eq!((c.max_streak, c.repeated_max), (3, 3), "argumentos lidos");
    assert_eq!(c.recommendation, Recommendation::Loop);
    let json = serde_json::to_string(&snap).unwrap();
    assert!(!json.contains(marker), "{json}");
    assert!(!json.contains(CANARY));
    assert!(!format!("{snap:?}").contains(marker));
}

/// O `[:400]` do script conta caracteres: com `é` (2 bytes) no valor, duas
/// chamadas iguais nos 400 primeiros caracteres sao a mesma, e diferentes no
/// caractere 394 sao outra (um corte em 400 bytes as juntaria).
#[test]
fn transcript_loop_key_is_cut_at_400_characters_not_bytes() {
    let t = TempDir::new("ac-transcript-cut").unwrap();
    // `{"command": "` sao 13 caracteres: o valor fica com os 387 seguintes.
    let calls = |pad: usize| -> Vec<String> {
        ["A", "B", "C"]
            .iter()
            .map(|end| format!("{{\"command\":\"{}{end}\"}}", "\u{e9}".repeat(pad)))
            .collect()
    };
    let loops = |name: &str, pad: usize| {
        let path = claude_with_calls(&t, name, &calls(pad));
        Transcripts::read_file(&path, Format::Claude)
            .unwrap()
            .meta(Format::Claude)
            .loops
            .unwrap()
    };
    // Diferem no caractere 404: depois do corte, as tres sao a mesma.
    assert_eq!(
        loops("same.jsonl", 390),
        Loops {
            max_streak: 3,
            repeated_max: 3
        }
    );
    // Diferem no caractere 394 (byte 774): antes do corte, sao tres.
    assert_eq!(
        loops("diff.jsonl", 380),
        Loops {
            max_streak: 2,
            repeated_max: 2
        }
    );
}
