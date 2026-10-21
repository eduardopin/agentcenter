//! Overlay do estado vivo do Claude Code: `~/.claude/sessions/<pid>.json`.
//!
//! Cada `claude` aberto mantem `<pid>.json` com nome, sessionId, kind e status.
//! O arquivo so vale para o processo se o pid esta vivo E `procStart` e o
//! starttime atual dele (campo 22 de `/proc/<pid>/stat`, ticks desde o boot).
//! E o criterio do proprio Claude para dizer que a sessao esta viva, e e o que
//! separa a sessao certa de um pid reaproveitado ou de um arquivo velho.
//!
//! Custo por tick: [`Overlay::enrich`] faz um `stat` do json e uma leitura de
//! `/proc/<pid>/stat` por sessao `claude`; o json so e relido quando muda
//! (mtime, tamanho ou inode). O diretorio so e listado em
//! [`ClaudeState::background`], que quem chama usa uma vez por tick. Nenhum
//! processo externo: `claude agents --json` custa um node.
//!
//! Valores conferidos no Claude Code 2.1.300: kind em `interactive`, `bg`,
//! `daemon`, `daemon-worker`; status em `busy`, `shell`, `idle`, `waiting`.

use crate::adapters::claude::CLAUDE;
use crate::overlay::{Overlay, OverlayStatus, SessionOverlay};
use crate::procfs::stat::parse_stat;
use crate::procfs::{ProcRoot, read_file_into};
use crate::session::Session;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Diretorio de sessoes explicito; vence `CLAUDE_CONFIG_DIR` e `HOME`.
pub const SESSIONS_DIR_ENV: &str = "AGENTCENTER_CLAUDE_SESSIONS";
/// Maior arquivo aceito: o mesmo teto que o Claude usa ao ler o registro.
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
/// Acima disso o cache e zerado: pid que nao volta nao vaza memoria.
const CACHE_CAP: usize = 4096;

/// Os campos de `sessions/<pid>.json` que o overlay usa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeEntry {
    pub pid: u32,
    /// `procStart`: starttime do dono do arquivo. Sem ele o arquivo nunca casa.
    pub proc_start: Option<u64>,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub kind: Option<String>,
    /// Status como esta no arquivo; [`ClaudeEntry::status`] da a versao do contrato.
    pub status_raw: Option<String>,
    pub cwd: Option<String>,
    pub status_updated_at_ms: Option<u64>,
}

impl ClaudeEntry {
    pub fn status(&self) -> Option<OverlayStatus> {
        map_status(self.status_raw.as_deref()?)
    }
}

/// Status do Claude no contrato. `shell` (comando `!` rodando) e trabalho;
/// `waiting` (pedido de permissao, pergunta) e bloqueio. Desconhecido: `None`.
pub fn map_status(raw: &str) -> Option<OverlayStatus> {
    match raw {
        "busy" | "shell" => Some(OverlayStatus::Busy),
        "idle" => Some(OverlayStatus::Idle),
        "waiting" | "blocked" => Some(OverlayStatus::Blocked),
        _ => None,
    }
}

/// pid de `<pid>.json` na forma canonica (`012.json` e `x.json` nao contam,
/// nem `*.key`). Mesma regra do Claude.
pub fn pid_from_file_name(name: &str) -> Option<u32> {
    let digits = name.strip_suffix(".json")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.starts_with('0') {
        return None;
    }
    digits.parse().ok()
}

/// Faz o parse de um arquivo de sessao. `None` se nao e JSON de objeto ou se o
/// `pid` de dentro nao e o do nome. Campo com tipo errado vira `None` sozinho.
pub fn parse_entry(file_pid: u32, text: &str) -> Option<ClaudeEntry> {
    let v: Value = serde_json::from_str(text).ok()?;
    let o = v.as_object()?;
    if o.get("pid")?.as_u64()? != u64::from(file_pid) {
        return None;
    }
    let text_field = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    Some(ClaudeEntry {
        pid: file_pid,
        proc_start: proc_start(o),
        session_id: text_field("sessionId"),
        name: text_field("name"),
        kind: text_field("kind"),
        status_raw: text_field("status"),
        cwd: text_field("cwd"),
        status_updated_at_ms: o.get("statusUpdatedAt").and_then(Value::as_u64),
    })
}

/// O Claude grava `procStart` como texto (`"2534"`); aceita numero tambem.
fn proc_start(o: &Map<String, Value>) -> Option<u64> {
    match o.get("procStart")? {
        Value::String(s) => s.parse().ok(),
        v => v.as_u64(),
    }
}

/// Diretorio de sessoes a partir de um leitor de ambiente:
/// `AGENTCENTER_CLAUDE_SESSIONS`, senao `$CLAUDE_CONFIG_DIR/sessions`, senao
/// `$HOME/.claude/sessions`. Variavel vazia conta como ausente.
pub fn sessions_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    get(SESSIONS_DIR_ENV)
        .or_else(|| get("CLAUDE_CONFIG_DIR").map(|d| d.join("sessions")))
        .or_else(|| get("HOME").map(|h| h.join(".claude/sessions")))
}

/// Entrada do cache, valida enquanto o arquivo tiver a mesma identidade.
struct Cached {
    key: FileKey,
    /// `None`: arquivo invalido (truncado, grande demais, pid trocado). Fica no
    /// cache para nao ser relido a cada tick ate mudar.
    entry: Option<ClaudeEntry>,
}

/// mtime (s, ns), tamanho e inode: escrita atomica (rename) troca o inode e
/// escrita no lugar troca mtime ou tamanho.
type FileKey = (i64, i64, u64, u64);

#[derive(Default)]
struct Inner {
    cache: HashMap<u32, Cached>,
    buf: String,
}

/// Overlay de `sessions/<pid>.json`. Preenche `name`, `agent_session_id`,
/// `status` e `kind` das sessoes `claude` cujo arquivo casa com o processo.
pub struct ClaudeState {
    dir: Option<PathBuf>,
    proc: ProcRoot,
    inner: Mutex<Inner>,
    parses: AtomicU64,
}

impl ClaudeState {
    /// `dir`: diretorio de sessoes (`None` desliga o overlay). `proc`: raiz
    /// onde conferir `procStart`.
    pub fn new(dir: Option<PathBuf>, proc: ProcRoot) -> Self {
        Self {
            dir,
            proc,
            inner: Mutex::new(Inner::default()),
            parses: AtomicU64::new(0),
        }
    }

    /// Diretorio pelo ambiente (ver [`sessions_dir`]) e `/proc` real.
    pub fn from_env() -> Self {
        Self::new(sessions_dir(|k| std::env::var_os(k)), ProcRoot::default())
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Quantos arquivos ja foram lidos e interpretados (o cache poupa o resto).
    pub fn parses(&self) -> u64 {
        self.parses.load(Ordering::Relaxed)
    }

    /// Arquivos no cache agora (diagnostico; sobe com sessoes, desce com
    /// [`ClaudeState::background`] quando arquivos somem).
    pub fn cached_files(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cache
            .len()
    }

    /// Sessoes vivas do diretorio que nao sao linha `claude` em `sessions`:
    /// jobs em background e daemons cujo processo nao foi detectado como
    /// sessao. Sem numeros de recurso; o chamador decide como lista-las.
    ///
    /// Criterio: vivo e o arquivo cujo pid existe com starttime igual a
    /// `procStart`. Pid morto, pid reaproveitado (starttime diferente) e
    /// arquivo sem `procStart` sao arquivo velho e ficam de fora. Lista o
    /// diretorio uma vez por chamada; arquivos que sumiram saem do cache.
    pub fn background(&self, sessions: &[Session]) -> Vec<ClaudeEntry> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let shown: HashSet<u32> = sessions
            .iter()
            .filter(|s| s.agent == CLAUDE.id)
            .map(|s| s.pid)
            .collect();
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Inner { cache, buf } = &mut *inner;
        let mut present = HashSet::new();
        let mut out = Vec::new();
        for ent in rd.flatten() {
            let Some(pid) = ent.file_name().to_str().and_then(pid_from_file_name) else {
                continue;
            };
            present.insert(pid);
            if shown.contains(&pid) {
                continue;
            }
            if let Some(e) = self.refresh(dir, cache, buf, pid)
                && is_live(&self.proc, buf, e)
            {
                out.push(e.clone());
            }
        }
        cache.retain(|pid, _| present.contains(pid));
        out.sort_by_key(|e| e.pid);
        out
    }

    /// Entrada atual de `<pid>.json`, relendo so se o arquivo mudou.
    fn refresh<'a>(
        &self,
        dir: &Path,
        cache: &'a mut HashMap<u32, Cached>,
        buf: &mut String,
        pid: u32,
    ) -> Option<&'a ClaudeEntry> {
        let path = dir.join(format!("{pid}.json"));
        let meta = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            _ => {
                cache.remove(&pid);
                return None;
            }
        };
        let key = (meta.mtime(), meta.mtime_nsec(), meta.len(), meta.ino());
        if cache.get(&pid).is_none_or(|c| c.key != key) {
            self.parses.fetch_add(1, Ordering::Relaxed);
            let entry = (meta.len() <= MAX_FILE_BYTES)
                .then(|| read_file_into(&path, buf).ok())
                .flatten()
                .filter(|()| buf.len() as u64 <= MAX_FILE_BYTES)
                .and_then(|()| parse_entry(pid, buf));
            if cache.len() >= CACHE_CAP && !cache.contains_key(&pid) {
                cache.clear();
            }
            cache.insert(pid, Cached { key, entry });
        }
        cache.get(&pid)?.entry.as_ref()
    }
}

/// O processo `e.pid` existe e comecou em `procStart`.
fn is_live(proc: &ProcRoot, buf: &mut String, e: &ClaudeEntry) -> bool {
    let Some(start) = e.proc_start else {
        return false;
    };
    read_file_into(&proc.pid_path(e.pid, "stat"), buf).is_ok()
        && parse_stat(buf).is_ok_and(|st| st.pid == e.pid && st.starttime == start)
}

impl Overlay for ClaudeState {
    fn id(&self) -> &str {
        "claude_state"
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        if session.agent != CLAUDE.id {
            return;
        }
        let Some(dir) = &self.dir else {
            return;
        };
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Inner { cache, buf } = &mut *inner;
        let Some(e) = self.refresh(dir, cache, buf, session.pid) else {
            return;
        };
        if !is_live(&self.proc, buf, e) {
            return;
        }
        if e.name.is_some() {
            overlay.name.clone_from(&e.name);
        }
        if e.session_id.is_some() {
            overlay.agent_session_id.clone_from(&e.session_id);
        }
        if let Some(st) = e.status() {
            overlay.status = Some(st);
        }
        if e.kind.is_some() {
            overlay.kind.clone_from(&e.kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_dir_prefers_explicit_then_config_dir_then_home() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        let all = env(&[
            (SESSIONS_DIR_ENV, "/x"),
            ("CLAUDE_CONFIG_DIR", "/c"),
            ("HOME", "/h"),
        ]);
        assert_eq!(sessions_dir(all), Some(PathBuf::from("/x")));
        let cfg = env(&[
            (SESSIONS_DIR_ENV, ""),
            ("CLAUDE_CONFIG_DIR", "/c"),
            ("HOME", "/h"),
        ]);
        assert_eq!(sessions_dir(cfg), Some(PathBuf::from("/c/sessions")));
        let home = env(&[("HOME", "/h")]);
        assert_eq!(
            sessions_dir(home),
            Some(PathBuf::from("/h/.claude/sessions"))
        );
        assert_eq!(sessions_dir(env(&[])), None);
    }

    #[test]
    fn file_name_must_be_canonical_pid_json() {
        assert_eq!(pid_from_file_name("11435.json"), Some(11435));
        for bad in [
            "11435.abc.key",
            "11435.json.tmp",
            "011435.json",
            "0.json",
            ".json",
            "x1.json",
            "-1.json",
            "99999999999.json",
        ] {
            assert_eq!(pid_from_file_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn status_maps_claude_values_to_contract() {
        assert_eq!(map_status("busy"), Some(OverlayStatus::Busy));
        assert_eq!(map_status("shell"), Some(OverlayStatus::Busy));
        assert_eq!(map_status("idle"), Some(OverlayStatus::Idle));
        assert_eq!(map_status("waiting"), Some(OverlayStatus::Blocked));
        assert_eq!(map_status("blocked"), Some(OverlayStatus::Blocked));
        assert_eq!(map_status("Busy"), None);
        assert_eq!(map_status(""), None);
    }
}
