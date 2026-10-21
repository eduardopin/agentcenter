//! Overlay `sessions/<pid>.json` sobre `tests/fixtures/claude_sessions` e um
//! `/proc` falso. O overlay e montado com diretorio e raiz explicitos; o do
//! registry (ambiente, `/proc` real) nao entra aqui.

use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::overlay::claude_state::ClaudeState;
use agentcenter_core::overlay::{self, Overlay, OverlayStatus, SessionOverlay};
use agentcenter_core::{Registry, Sampler, Snapshot};
use std::fs;
use std::path::{Path, PathBuf};

/// Starttime de todos os processos vivos da fixture; os json casam com 5000.
const START: u64 = 5000;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_sessions")
}

/// `/proc` com: claude 100..=106 e 113 (101 reaproveitado: o json diz 4000),
/// codex 120, e processos soltos 110 (job bg vivo) e 112 (daemon com pid
/// reaproveitado). 111 nao existe (job morto).
fn proc_world(dir: &TempDir) -> FakeProc {
    let fp = FakeProc::create(dir.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    let add = |pid: u32, comm: &str, starttime: u64| {
        let mut p = FakeProcess::new(pid, 1, comm);
        p.starttime = starttime;
        fp.add(&p).unwrap();
    };
    for pid in [100, 101, 102, 103, 104, 105, 106, 113] {
        add(pid, "claude", START);
    }
    add(110, "bun", START);
    add(112, "bun", START + 2000);
    add(120, "codex", START);
    fp
}

fn sample(fp: &FakeProc) -> Snapshot {
    let mut snap = Sampler::new(Registry::default())
        .tick(&fp.proc_root())
        .unwrap();
    for s in &mut snap.sessions {
        s.overlay = None; // so o overlay sob teste
    }
    snap
}

/// Um tick com o overlay sob teste. Mesmo efeito de [`overlay::apply`], mas por
/// referencia, para o teste olhar o cache entre ticks.
fn tick(fp: &FakeProc, state: &ClaudeState) -> Snapshot {
    let mut snap = sample(fp);
    for s in &mut snap.sessions {
        let mut o = SessionOverlay::default();
        state.enrich(s, &mut o);
        s.overlay = (o != SessionOverlay::default()).then_some(o);
    }
    snap
}

fn ov(snap: &Snapshot, pid: u32) -> Option<&SessionOverlay> {
    snap.sessions
        .iter()
        .find(|s| s.pid == pid)
        .unwrap_or_else(|| panic!("sessao {pid}"))
        .overlay
        .as_ref()
}

fn state_on(dir: PathBuf, fp: &FakeProc) -> ClaudeState {
    ClaudeState::new(Some(dir), fp.proc_root())
}

#[test]
fn claude_state_fills_overlay_when_pid_and_proc_start_match() {
    let t = TempDir::new("ac-cs-fill").unwrap();
    let fp = proc_world(&t);
    let snap = tick(&fp, &state_on(fixture_dir(), &fp));
    let o = ov(&snap, 100).expect("100 casa");
    assert_eq!(o.name.as_deref(), Some("app-busy"));
    assert_eq!(
        o.agent_session_id.as_deref(),
        Some("00000000-0000-4000-8000-000000000100")
    );
    assert_eq!(o.status, Some(OverlayStatus::Busy));
    assert_eq!(o.kind.as_deref(), Some("interactive"));
    assert_eq!(ov(&snap, 103).unwrap().status, Some(OverlayStatus::Blocked));
    assert_eq!(ov(&snap, 104).unwrap().status, Some(OverlayStatus::Busy));
    // Status desconhecido: o resto do arquivo vale, o status fica ausente.
    let unknown = ov(&snap, 106).unwrap();
    assert_eq!(unknown.name.as_deref(), Some("app-unknown-status"));
    assert_eq!(unknown.status, None);

    // O caminho do Sampler (`overlay::apply` com `Box<dyn Overlay>`) da o mesmo.
    let mut via_apply = sample(&fp);
    let boxed: Vec<Box<dyn Overlay>> = vec![Box::new(state_on(fixture_dir(), &fp))];
    overlay::apply(&boxed, &mut via_apply.sessions);
    for s in &via_apply.sessions {
        assert_eq!(s.overlay.as_ref(), ov(&snap, s.pid), "pid {}", s.pid);
    }
}

#[test]
fn claude_state_reused_pid_does_not_match() {
    let t = TempDir::new("ac-cs-reuse").unwrap();
    let fp = proc_world(&t);
    let snap = tick(&fp, &state_on(fixture_dir(), &fp));
    // 101.json diz procStart 4000; o processo 101 de agora comecou em 5000.
    assert_eq!(ov(&snap, 101), None);
    // Sem procStart nao ha como provar que e o mesmo processo.
    assert_eq!(ov(&snap, 113), None);
    // `pid` de dentro do json (999) diferente do nome do arquivo.
    assert_eq!(ov(&snap, 105), None);
}

#[test]
fn claude_state_truncated_json_does_not_break_tick() {
    let t = TempDir::new("ac-cs-trunc").unwrap();
    let fp = proc_world(&t);
    let state = state_on(fixture_dir(), &fp);
    let snap = tick(&fp, &state);
    assert_eq!(ov(&snap, 102), None);
    assert!(
        ov(&snap, 100).is_some(),
        "os vizinhos continuam preenchidos"
    );
    // O arquivo ruim fica no cache como invalido: nao e relido no tick seguinte.
    let before = state.parses();
    let snap = tick(&fp, &state);
    assert_eq!(ov(&snap, 102), None);
    assert_eq!(state.parses(), before);
}

#[test]
fn claude_state_ignores_key_and_non_canonical_files() {
    let t = TempDir::new("ac-cs-key").unwrap();
    let fp = proc_world(&t);
    let state = state_on(fixture_dir(), &fp);
    let snap = tick(&fp, &state);
    // `100.<hash>.key` e `0100.json` tambem falam do pid 100; nenhum vale.
    assert_eq!(ov(&snap, 100).unwrap().name.as_deref(), Some("app-busy"));
    let bg = state.background(&snap.sessions);
    assert!(bg.iter().all(|e| e.name.as_deref() != Some("from-key")));
    assert!(
        bg.iter()
            .all(|e| e.name.as_deref() != Some("non-canonical"))
    );
}

#[test]
fn claude_state_only_enriches_claude_sessions() {
    let t = TempDir::new("ac-cs-codex").unwrap();
    let fp = proc_world(&t);
    let dir = t.path().join("sessions");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("120.json"),
        r#"{"pid":120,"procStart":"5000","name":"not-claude","status":"busy"}"#,
    )
    .unwrap();
    let snap = tick(&fp, &state_on(dir, &fp));
    let codex = snap.sessions.iter().find(|s| s.pid == 120).unwrap();
    assert_eq!(codex.agent, "codex");
    assert_eq!(codex.overlay, None);
}

#[test]
fn claude_state_background_lists_live_unattached_files_only() {
    let t = TempDir::new("ac-cs-bg").unwrap();
    let fp = proc_world(&t);
    let state = state_on(fixture_dir(), &fp);
    let snap = tick(&fp, &state);
    let parses = state.parses();
    let bg = state.background(&snap.sessions);
    // Arquivos que o enrich ja leu nao sao relidos pela listagem.
    assert_eq!(state.parses(), parses + 3, "so 110..=112 sao novos");
    // 110: job bg vivo, nao e linha claude. 111 morto, 112 reaproveitado,
    // 113 sem procStart: arquivo velho. 100..=106 ja sao linha com numeros.
    let pids: Vec<u32> = bg.iter().map(|e| e.pid).collect();
    assert_eq!(pids, vec![110]);
    assert_eq!(bg[0].kind.as_deref(), Some("bg"));
    assert_eq!(bg[0].status(), Some(OverlayStatus::Busy));
    assert_eq!(bg[0].name.as_deref(), Some("bg-live"));
    assert_eq!(bg[0].status_updated_at_ms, Some(1792040464474));
}

#[test]
fn claude_state_rereads_only_changed_files() {
    let t = TempDir::new("ac-cs-mtime").unwrap();
    let fp = proc_world(&t);
    let dir = t.path().join("sessions");
    fs::create_dir_all(&dir).unwrap();
    for e in fs::read_dir(fixture_dir()).unwrap().flatten() {
        fs::copy(e.path(), dir.join(e.file_name())).unwrap();
    }
    let state = state_on(dir.clone(), &fp);
    tick(&fp, &state);
    let first = state.parses();
    assert!(first > 0);
    tick(&fp, &state);
    assert_eq!(state.parses(), first, "nada mudou: nada relido");

    // Escrita atomica como a do Claude: arquivo novo e rename (troca o inode).
    let text = fs::read_to_string(dir.join("100.json"))
        .unwrap()
        .replace("app-busy", "app-renamed")
        .replace("\"busy\"", "\"idle\"");
    fs::write(dir.join("100.json.tmp"), text).unwrap();
    fs::rename(dir.join("100.json.tmp"), dir.join("100.json")).unwrap();
    let snap = tick(&fp, &state);
    assert_eq!(state.parses(), first + 1, "so o arquivo alterado");
    let o = ov(&snap, 100).unwrap();
    assert_eq!(o.name.as_deref(), Some("app-renamed"));
    assert_eq!(o.status, Some(OverlayStatus::Idle));
}

#[test]
fn claude_state_torn_write_heals_when_file_changes() {
    let t = TempDir::new("ac-cs-torn").unwrap();
    let fp = proc_world(&t);
    let dir = t.path().join("sessions");
    fs::create_dir_all(&dir).unwrap();
    let full = fs::read_to_string(fixture_dir().join("100.json")).unwrap();
    fs::write(dir.join("100.json"), &full[..full.len() / 2]).unwrap();
    let state = state_on(dir.clone(), &fp);
    let snap = tick(&fp, &state);
    assert_eq!(ov(&snap, 100), None);
    fs::write(dir.join("100.json"), &full).unwrap();
    let snap = tick(&fp, &state);
    assert_eq!(ov(&snap, 100).unwrap().name.as_deref(), Some("app-busy"));
}

#[test]
fn claude_state_file_removed_or_dir_missing_is_inert() {
    let t = TempDir::new("ac-cs-gone").unwrap();
    let fp = proc_world(&t);
    let dir = t.path().join("sessions");
    fs::create_dir_all(&dir).unwrap();
    for f in ["100.json", "110.json"] {
        fs::copy(fixture_dir().join(f), dir.join(f)).unwrap();
    }
    let state = state_on(dir.clone(), &fp);
    let snap = tick(&fp, &state);
    assert!(ov(&snap, 100).is_some());
    assert_eq!(state.background(&snap.sessions).len(), 1);
    assert_eq!(state.cached_files(), 2);
    // A listagem tira do cache o que sumiu do diretorio.
    fs::remove_file(dir.join("110.json")).unwrap();
    assert!(state.background(&snap.sessions).is_empty());
    assert_eq!(state.cached_files(), 1);
    fs::remove_file(dir.join("100.json")).unwrap();
    let snap = tick(&fp, &state);
    assert_eq!(ov(&snap, 100), None, "arquivo apagado nao fica no cache");
    assert_eq!(state.cached_files(), 0);

    for state in [
        state_on(t.path().join("nao-existe"), &fp),
        ClaudeState::new(None, fp.proc_root()),
    ] {
        let snap = tick(&fp, &state);
        assert!(snap.sessions.iter().all(|s| s.overlay.is_none()));
        assert!(state.background(&snap.sessions).is_empty());
    }
}

#[test]
fn claude_state_is_in_the_registry() {
    let ids: Vec<String> = overlay::registry()
        .iter()
        .map(|o| o.id().to_string())
        .collect();
    // Lista exata, na ordem de registro: herdr (C3) e transcripts (C4) depois
    // deste; o de transcripts consome o `agent_session_id` deste.
    assert_eq!(
        ids,
        vec![
            "claude_state".to_string(),
            "herdr".to_string(),
            "transcript".to_string()
        ]
    );
}

/// Monta um `sessions/` so com `100.json` (a fixture) e devolve o estado ja
/// lido uma vez (cache aquecido) e o mundo `/proc`.
fn warmed(tag: &str) -> (TempDir, FakeProc, PathBuf, ClaudeState) {
    let t = TempDir::new(tag).unwrap();
    let fp = proc_world(&t);
    let dir = t.path().join("sessions");
    fs::create_dir_all(&dir).unwrap();
    fs::copy(fixture_dir().join("100.json"), dir.join("100.json")).unwrap();
    let state = state_on(dir.clone(), &fp);
    let snap = tick(&fp, &state);
    assert_eq!(ov(&snap, 100).unwrap().status, Some(OverlayStatus::Busy));
    (t, fp, dir, state)
}

fn set_mtime(path: &Path, t: std::time::SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Defeito travado: troca atomica (rename) que preserva tamanho e mtime
/// ("busy" -> "idle", 4 bytes cada) so se distingue pelo inode; sem `ino` na
/// chave do cache o overlay ficaria preso em "busy".
#[test]
fn claude_state_same_size_same_mtime_rename_is_seen_by_inode() {
    let (_t, fp, dir, state) = warmed("ac-cs-ino");
    let path = dir.join("100.json");
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let new = text.replace("\"busy\"", "\"idle\"");
    assert_eq!(new.len(), text.len());
    let tmp = dir.join("100.json.tmp");
    fs::write(&tmp, new).unwrap();
    set_mtime(&tmp, mtime); // mesmo mtime (s e ns): so o inode difere
    fs::rename(&tmp, &path).unwrap();
    let before = state.parses();
    let snap = tick(&fp, &state);
    assert_eq!(state.parses(), before + 1);
    assert_eq!(ov(&snap, 100).unwrap().status, Some(OverlayStatus::Idle));
}

/// Defeito travado: escrita no lugar (mesmo inode, mesmo tamanho) so se
/// distingue pelo mtime (segundos + nanos); sem ele o cache serviria "busy".
#[test]
fn claude_state_same_size_in_place_write_is_seen_by_mtime() {
    use std::time::Duration;
    let (_t, fp, dir, state) = warmed("ac-cs-mtime-inplace");
    let path = dir.join("100.json");
    let m0 = fs::metadata(&path).unwrap().modified().unwrap();
    let text = fs::read_to_string(&path).unwrap();
    // 1) so os nanos mudam (mesmo segundo inteiro, mesmo inode e tamanho).
    let base = m0.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let at = |s: u64, ns: u32| std::time::UNIX_EPOCH + Duration::new(s, ns);
    set_mtime(&path, at(base, 100));
    tick(&fp, &state);
    let before = state.parses();
    fs::write(&path, text.replace("\"busy\"", "\"idle\"")).unwrap();
    set_mtime(&path, at(base, 200));
    let snap = tick(&fp, &state);
    assert_eq!(state.parses(), before + 1, "mtime_nsec mudou");
    assert_eq!(ov(&snap, 100).unwrap().status, Some(OverlayStatus::Idle));
    // 2) so os segundos mudam (mesmos nanos).
    fs::write(&path, text).unwrap();
    set_mtime(&path, at(base + 5, 200));
    let snap = tick(&fp, &state);
    assert_eq!(state.parses(), before + 2, "mtime (s) mudou");
    assert_eq!(ov(&snap, 100).unwrap().status, Some(OverlayStatus::Busy));
}

/// JSON valido com exatamente `total` bytes (preenche `name` com "x").
fn json_of_len(total: usize) -> String {
    let head = r#"{"pid":100,"procStart":"5000","status":"idle","name":""#;
    let tail = r#""}"#;
    let pad = total - head.len() - tail.len();
    let s = format!("{head}{}{tail}", "x".repeat(pad));
    assert_eq!(s.len(), total);
    s
}

/// Defeito travado: o teto `MAX_FILE_BYTES` e inclusivo. Exatamente o teto e
/// lido; um byte a mais e ignorado (sem ler o arquivo todo para a memoria).
#[test]
fn claude_state_file_size_cap_is_inclusive() {
    use agentcenter_core::overlay::claude_state::MAX_FILE_BYTES;
    let max = MAX_FILE_BYTES as usize;
    for (len, accepted) in [(max, true), (max + 1, false)] {
        let t = TempDir::new("ac-cs-cap").unwrap();
        let fp = proc_world(&t);
        let dir = t.path().join("sessions");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("100.json"), json_of_len(len)).unwrap();
        let state = state_on(dir, &fp);
        let snap = tick(&fp, &state);
        let o = ov(&snap, 100);
        assert_eq!(o.is_some(), accepted, "arquivo de {len} bytes (teto {max})");
        if accepted {
            assert_eq!(o.unwrap().status, Some(OverlayStatus::Idle));
        }
    }
}
