//! Subagentes por sessao (C15): `subagents_live` e `subagents_total`, so
//! metadados.
//!
//! **Claude**: cada subagente grava `<sid>/subagents/agent-*.jsonl` (e um
//! `agent-*.meta.json`) ao lado do transcript `<sid>.jsonl` da sessao. O total
//! e o numero de `agent-*.jsonl`. O `.meta.json` nunca e aberto: o
//! `description` dele e texto. Terminado = a ultima linha `assistant` do fim do
//! arquivo (lido de tras para frente, ate [`TAIL_BUDGET`]) tem `stop_reason`
//! `end_turn`; vivo = sem `end_turn` e mtime ha menos de [`LIVE_WINDOW`]; sem
//! `end_turn` e mtime antigo e orfao (cortado), nao conta como vivo. O
//! `tool_result` do `Agent` no pai chega na hora (chamada assincrona) e nao
//! serve de sinal.
//!
//! **Custo**: o diretorio e conferido a cada `refresh_every`, so para sessao
//! viva com transcript casado, e so e relistado quando o mtime dele muda
//! (subagente novo, ou `.meta.json` regravado). Arquivo terminado ha mais de
//! [`DONE_GRACE`] e lido uma vez e nunca mais: nem lido nem `stat`. Ele so
//! volta a ser olhado se o `.meta.json` dele trocar de inode na listagem (o
//! Claude regrava o meta, por troca de arquivo, quando retoma um subagente:
//! 246 de 246 retomadas medidas em 17/10/2026), e ai so com um `stat`. Os
//! outros (vivos, orfaos, terminados ha pouco) tem um `stat` por conferencia e
//! sao relidos so quando tamanho ou mtime mudam. A primeira leitura dos
//! arquivos de uma sessao divide [`READ_BUDGET`] por tick com as outras
//! sessoes; enquanto sobra arquivo sem leitura, `subagents_live` sai `None`
//! (o total ja vale). O comando de um tiro (`transcript_read_budget` =
//! `READ_ALL`) tira esse teto.
//!
//! **Codex**: subagente e um rollout proprio cujo `session_meta` tem
//! `source.subagent.thread_spawn.parent_thread_id` = id do rollout pai
//! ([`codex::spawn_parent`]), no diretorio de dia do pai ou num seguinte
//! ([`codex::spawn_dirs`]). Vivo = ultimo evento de turno `task_started` (sem
//! `task_complete` nem `turn_aborted` depois) e mtime ha menos de
//! [`LIVE_WINDOW`]. Rollout com mtime antigo nao e lido; o recente e lido de
//! forma incremental. O fd do filho no processo pai nao e usado.
//!
//! **pi**: sem subagentes; os campos ficam `None`.

use super::{
    CACHE_CAP, Format, MAX_HEADER, MAX_LINE, Usage, codex, first_line, lenient, parse_line,
};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{DirEntryExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Sem `end_turn`, o subagente e vivo enquanto o arquivo mudou ha menos disso.
pub const LIVE_WINDOW: Duration = Duration::from_secs(120);
/// `end_turn` mais novo que isso ainda e conferido (`stat`) a cada vez: o
/// Claude as vezes continua o subagente logo depois sem regravar o meta.
pub const DONE_GRACE: Duration = Duration::from_secs(10);
/// Bytes lidos de tras para frente, por arquivo, atras da ultima `assistant`
/// (a maior distancia medida em 1884 arquivos foi ~97 KB, 1 acima de 64 KiB;
/// p99 ~20 KB).
pub const TAIL_BUDGET: u64 = 64 << 10;
/// Bloco da leitura de tras para frente (p90 da distancia: ~11 KB).
const TAIL_CHUNK: u64 = 16 << 10;
/// Bytes de primeira leitura de subagente por tick, somadas todas as sessoes.
pub const READ_BUDGET: u64 = 512 << 10;
/// Bloco da leitura incremental do rollout de subagente do Codex.
const CHUNK: usize = 64 << 10;

/// Identidade e versao de um arquivo ou diretorio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
}

impl Stamp {
    fn of(m: &std::fs::Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: (m.mtime(), m.mtime_nsec()),
        }
    }

    /// Idade do mtime em `wall` (zero se no futuro).
    fn age(&self, wall: SystemTime) -> Duration {
        let (s, ns) = self.mtime;
        let at = if s >= 0 {
            SystemTime::UNIX_EPOCH + Duration::new(s as u64, ns.clamp(0, 999_999_999) as u32)
        } else {
            SystemTime::UNIX_EPOCH
        };
        wall.duration_since(at).unwrap_or_default()
    }
}

/// Um `agent-*.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Agent {
    /// Ainda nao lido.
    New,
    /// Lido nesta versao; `ended`: ultima `assistant` com `end_turn`.
    Seen { at: Stamp, ended: bool },
    /// Terminado ha mais de [`DONE_GRACE`]: nunca mais lido.
    Done(Stamp),
    /// Terminado, com o meta trocado: um `stat` diz se mudou.
    Recheck(Stamp),
}

struct AgentEntry {
    state: Agent,
    /// Inode do `.meta.json` na ultima listagem.
    meta_ino: Option<u64>,
}

#[derive(Default)]
struct ClaudeDir {
    stamp: Option<Stamp>,
    checked: Option<Instant>,
    agents: HashMap<String, AgentEntry>,
    live: Option<u64>,
}

/// Rollout de um diretorio de dia do Codex: o pai do subagente, se for um.
enum Spawn {
    /// Primeira linha ainda incompleta.
    Pending,
    Parent(Option<String>),
}

#[derive(Default)]
struct CodexDay {
    stamp: Option<Stamp>,
    rollouts: HashMap<PathBuf, Spawn>,
}

/// Ultima conta dos subagentes de um rollout pai do Codex.
struct CodexParent {
    checked: Instant,
    counts: Counts,
}

/// Leitura incremental do rollout de um subagente do Codex: so o estado.
#[derive(Default)]
struct Kid {
    dev: u64,
    ino: u64,
    offset: u64,
    partial: Vec<u8>,
    usage: Usage,
    at_end: bool,
}

/// Estado do overlay de subagentes, dentro do `Inner` dos transcripts.
#[derive(Default)]
pub(super) struct Subagents {
    claude: HashMap<PathBuf, ClaudeDir>,
    days: HashMap<PathBuf, CodexDay>,
    parents: HashMap<PathBuf, CodexParent>,
    kids: HashMap<PathBuf, Kid>,
    /// Bytes de primeira leitura que ainda cabem neste tick.
    pub(super) budget: u64,
    buf: Vec<u8>,
}

/// Contadores (para teste e bench): leituras do fim de `agent-*.jsonl` e
/// bytes de subagente lidos (fim do Claude, cabecalho e rollout do Codex).
#[derive(Default)]
pub(super) struct Counters {
    pub(super) tail_reads: AtomicU64,
    pub(super) bytes: AtomicU64,
}

/// O que sai no overlay: (vivos, total).
pub(super) type Counts = (Option<u64>, Option<u64>);

/// Linha do `agent-*.jsonl`: so o tipo e o `stop_reason`.
#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default, deserialize_with = "lenient::string")]
    stop_reason: Option<String>,
}

/// `Some(end_turn?)` se a linha e `assistant`; `None` para as outras.
fn assistant_end(line: &[u8]) -> Option<bool> {
    let d = parse_line::<Line>(line).filter(|d| d.kind.as_deref() == Some("assistant"))?;
    Some(d.message.and_then(|m| m.stop_reason).as_deref() == Some("end_turn"))
}

impl Subagents {
    pub(super) fn new() -> Self {
        Self {
            budget: READ_BUDGET,
            ..Self::default()
        }
    }

    /// Subagentes da sessao cujo transcript e `path` (na raiz `root`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn counts(
        &mut self,
        f: Format,
        root: &Path,
        path: &Path,
        every: Duration,
        now: Instant,
        wall: SystemTime,
        c: &Counters,
    ) -> Counts {
        match f {
            Format::Claude => {
                let dir = path.with_extension("").join("subagents");
                self.claude_counts(dir, every, now, wall, c)
            }
            Format::Codex => self.codex_counts(root, path, every, now, wall, c),
            Format::Pi => (None, None),
        }
    }

    fn claude_counts(
        &mut self,
        dir: PathBuf,
        every: Duration,
        now: Instant,
        wall: SystemTime,
        c: &Counters,
    ) -> Counts {
        let Self {
            claude,
            budget,
            buf,
            ..
        } = self;
        if claude.len() >= CACHE_CAP && !claude.contains_key(&dir) {
            claude.clear();
        }
        let d = claude.entry(dir.clone()).or_default();
        // Primeira leitura ainda incompleta (`live` None): continua ja.
        let fresh = d.live.is_some() && d.checked.is_some_and(|t| now.duration_since(t) < every);
        if !fresh {
            d.checked = Some(now);
            refresh_claude(&dir, d, budget, buf, wall, c);
        }
        (d.live, Some(d.agents.len() as u64))
    }

    fn codex_counts(
        &mut self,
        root: &Path,
        path: &Path,
        every: Duration,
        now: Instant,
        wall: SystemTime,
        c: &Counters,
    ) -> Counts {
        if let Some(p) = self.parents.get(path)
            && now.duration_since(p.checked) < every
        {
            return p.counts;
        }
        let Some(id) = codex::rollout_id(path) else {
            return (None, None);
        };
        let now_ms = wall
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let Self {
            days,
            parents,
            kids,
            budget,
            buf,
            ..
        } = self;
        for map_len in [days.len(), parents.len(), kids.len()] {
            if map_len >= CACHE_CAP {
                days.clear();
                parents.clear();
                kids.clear();
            }
        }
        let mut found = Vec::new();
        for dir in codex::spawn_dirs(root, path, now_ms) {
            let day = days.entry(dir.clone()).or_default();
            refresh_day(&dir, day, buf, c);
            for (p, s) in &day.rollouts {
                if matches!(s, Spawn::Parent(Some(parent)) if parent == id) {
                    found.push(p.clone());
                }
            }
        }
        let (mut live, mut complete) = (0, true);
        for p in &found {
            let Ok(m) = std::fs::metadata(p) else {
                continue;
            };
            if Stamp::of(&m).age(wall) >= LIVE_WINDOW {
                kids.remove(p);
                continue;
            }
            let k = kids.entry(p.clone()).or_default();
            match scan_kid(p, k, buf, budget, c) {
                Ok(()) if k.at_end => live += u64::from(k.usage.busy == Some(true)),
                Ok(()) => complete = false,
                Err(_) => {}
            }
        }
        let counts = (complete.then_some(live), Some(found.len() as u64));
        if complete {
            let checked = now;
            parents.insert(path.to_path_buf(), CodexParent { checked, counts });
        }
        counts
    }
}

/// Confere o diretorio `subagents` (relista se mudou) e os arquivos que nao
/// estao em [`Agent::Done`]; recalcula `d.live`.
fn refresh_claude(
    dir: &Path,
    d: &mut ClaudeDir,
    budget: &mut u64,
    buf: &mut Vec<u8>,
    wall: SystemTime,
    c: &Counters,
) {
    let listed = match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => {
            let st = Stamp::of(&m);
            d.stamp == Some(st) || list_claude(dir, d).map(|()| d.stamp = Some(st)).is_ok()
        }
        _ => false,
    };
    if !listed {
        // Sem diretorio: sessao sem subagente.
        d.agents.clear();
        d.stamp = None;
        d.live = Some(0);
        return;
    }
    let (mut live, mut unread) = (0, false);
    for (stem, e) in &mut d.agents {
        if matches!(e.state, Agent::Done(_)) {
            continue;
        }
        if e.state == Agent::New && *budget == 0 {
            unread = true;
            continue;
        }
        let path = dir.join(format!("{stem}.jsonl"));
        e.state = step(&path, e.state, budget, buf, wall, c);
        match e.state {
            Agent::Seen { at, ended: false } if at.age(wall) < LIVE_WINDOW => live += 1,
            Agent::New => unread = true,
            _ => {}
        }
    }
    d.live = (!unread).then_some(live);
}

/// Lista `agent-*.jsonl` e o inode de cada `agent-*.meta.json` (do `readdir`,
/// sem abrir o meta). Terminado com o meta trocado vai para
/// [`Agent::Recheck`].
fn list_claude(dir: &Path, d: &mut ClaudeDir) -> std::io::Result<()> {
    let mut jsonl = HashSet::new();
    let mut metas = HashMap::new();
    for ent in std::fs::read_dir(dir)?.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with("agent-")) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".jsonl") {
            jsonl.insert(stem.to_owned());
        } else if let Some(stem) = name.strip_suffix(".meta.json") {
            metas.insert(stem.to_owned(), ent.ino());
        }
    }
    d.agents.retain(|k, _| jsonl.contains(k));
    for stem in jsonl {
        let meta = metas.get(&stem).copied();
        let e = d.agents.entry(stem).or_insert(AgentEntry {
            state: Agent::New,
            meta_ino: meta,
        });
        if e.meta_ino != meta {
            e.meta_ino = meta;
            if let Agent::Done(at) = e.state {
                e.state = Agent::Recheck(at);
            }
        }
    }
    Ok(())
}

/// Proximo estado de um `agent-*.jsonl` que nao esta em [`Agent::Done`]: um
/// `stat`, e a leitura do fim so se o arquivo mudou (ou nunca foi lido).
fn step(
    path: &Path,
    prev: Agent,
    budget: &mut u64,
    buf: &mut Vec<u8>,
    wall: SystemTime,
    c: &Counters,
) -> Agent {
    let Ok(m) = std::fs::metadata(path) else {
        return prev;
    };
    let st = Stamp::of(&m);
    let (at, ended) = match prev {
        Agent::Seen { at, ended } if at == st => (at, ended),
        Agent::Recheck(at) if at == st => (at, true),
        _ => match read_end(path, buf, c) {
            Ok((ended, n)) => {
                *budget = budget.saturating_sub(n);
                (st, ended)
            }
            Err(_) => return prev,
        },
    };
    if ended && at.age(wall) >= DONE_GRACE {
        Agent::Done(at)
    } else {
        Agent::Seen { at, ended }
    }
}

/// A ultima linha `assistant` completa do arquivo (lido de tras para frente,
/// em blocos de [`TAIL_CHUNK`], ate [`TAIL_BUDGET`]) tem `end_turn`? Devolve
/// tambem os bytes lidos. A linha sem `\n` no fim ainda esta sendo escrita e
/// nao conta.
fn read_end(path: &Path, buf: &mut Vec<u8>, c: &Counters) -> std::io::Result<(bool, u64)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    c.tail_reads.fetch_add(1, Ordering::Relaxed);
    let lo = len.saturating_sub(TAIL_BUDGET);
    buf.clear();
    buf.resize((len - lo) as usize, 0);
    let mut at = buf.len();
    // Indice do `\n` que fecha a linha mais nova ainda nao olhada.
    let mut right: Option<usize> = None;
    let mut read = 0;
    let mut ended = None;
    while at > 0 && ended.is_none() {
        let start = at.saturating_sub(TAIL_CHUNK as usize);
        file.seek(SeekFrom::Start(lo + start as u64))?;
        file.read_exact(&mut buf[start..at])?;
        read += (at - start) as u64;
        at = start;
        while let Some(i) = buf[at..right.unwrap_or(buf.len())]
            .iter()
            .rposition(|&b| b == b'\n')
        {
            let i = at + i;
            if let Some(r) = right {
                ended = assistant_end(&buf[i + 1..r]);
                if ended.is_some() {
                    break;
                }
            }
            right = Some(i);
        }
    }
    // Chegou ao comeco do arquivo: o que sobrou e a primeira linha.
    if ended.is_none()
        && lo == 0
        && let Some(r) = right
    {
        ended = assistant_end(&buf[..r]);
    }
    c.bytes.fetch_add(read, Ordering::Relaxed);
    Ok((ended == Some(true), read))
}

/// Confere um diretorio de dia do Codex: relista se o mtime mudou e le a
/// primeira linha dos rollouts ainda sem ela (uma vez por arquivo).
fn refresh_day(dir: &Path, day: &mut CodexDay, buf: &mut Vec<u8>, c: &Counters) {
    let st = match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => Stamp::of(&m),
        _ => {
            day.rollouts.clear();
            day.stamp = None;
            return;
        }
    };
    if day.stamp != Some(st) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut names = HashSet::new();
        for ent in rd.flatten() {
            let name = ent.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                names.insert(ent.path());
            }
        }
        day.rollouts.retain(|p, _| names.contains(p));
        for p in names {
            day.rollouts.entry(p).or_insert(Spawn::Pending);
        }
        day.stamp = Some(st);
    }
    for (p, s) in &mut day.rollouts {
        if matches!(s, Spawn::Pending) {
            match first_line(p, buf, &c.bytes) {
                Some(line) => *s = Spawn::Parent(codex::spawn_parent(line)),
                // Cabecalho maior que o teto nunca completa: nao e subagente.
                None if std::fs::metadata(p).is_ok_and(|m| m.len() > MAX_HEADER as u64) => {
                    *s = Spawn::Parent(None)
                }
                None => {}
            }
        }
    }
}

/// Le o que foi acrescentado ao rollout do subagente (ate `budget`) e aplica
/// as linhas completas: so o estado do turno (`usage.busy`) importa.
fn scan_kid(
    path: &Path,
    k: &mut Kid,
    buf: &mut Vec<u8>,
    budget: &mut u64,
    c: &Counters,
) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let m = file.metadata()?;
    if (m.dev(), m.ino()) != (k.dev, k.ino) || m.len() < k.offset {
        *k = Kid {
            dev: m.dev(),
            ino: m.ino(),
            ..Kid::default()
        };
    }
    file.seek(SeekFrom::Start(k.offset))?;
    buf.clear();
    buf.append(&mut k.partial);
    while *budget > 0 && k.offset < m.len() {
        let old = buf.len();
        let want = CHUNK.min(usize::try_from(*budget).unwrap_or(CHUNK));
        buf.resize(old + want, 0);
        let n = file.read(&mut buf[old..])?;
        buf.truncate(old + n);
        if n == 0 {
            break;
        }
        c.bytes.fetch_add(n as u64, Ordering::Relaxed);
        *budget = budget.saturating_sub(n as u64);
        k.offset += n as u64;
        let mut start = 0;
        while let Some(i) = buf[start..].iter().position(|&b| b == b'\n') {
            Format::Codex.feed(&buf[start..start + i], &mut k.usage);
            start += i + 1;
        }
        buf.drain(..start);
        if buf.len() > MAX_LINE {
            // Linha gigante: o resto dela vira uma linha invalida, ignorada.
            buf.clear();
        }
    }
    k.at_end = k.offset >= m.len();
    k.partial.extend_from_slice(buf);
    Ok(())
}
