//! Overlay de transcripts: modelo, tokens, contexto % e tool calls das
//! sessoes `claude`, `codex` e `pi`, e o estado (`busy`/`idle`) do Codex e do
//! pi, a partir dos `.jsonl` que cada agente grava.
//!
//! **So metadados** (ADR #5). Cada linha e desserializada em structs que
//! declaram so os campos de contagem, modelo, tipo, id e horario; o resto
//! (prompt, resposta, saida de ferramenta) e pulado pelo serde sem virar
//! `String`, e nada do conteudo e guardado, logado ou exposto. Ids de tool
//! call viram hash `u64` na desserializacao ([`lenient::hash`]). Nome e
//! argumentos de tool call (emenda do ADR #5, C23) viram, tambem na
//! desserializacao, um hash `u64` da forma canonica do script, cortada em 400
//! caracteres; o texto e descartado ali mesmo, ficam so os 30 ultimos hashes
//! por transcript e no snapshot so `max_streak` e `repeated_max`
//! ([`tools`]).
//!
//! **Tool calls** ([`tools`]): total, em voo (chamada sem resultado) e por
//! minuto (ultimos 5 min), so depois da varredura completa, como os tokens.
//! **Estado** (Codex: turno aberto; pi: `stopReason` da ultima resposta, ver
//! [`codex`] e [`pi`]) entra em `overlay.status` so se nenhum overlay anterior
//! publicou estado: nem `overlay.status` (o do Claude vem de `claude_state`)
//! nem `overlay.herdr.status` (o pane do herdr) com um valor reconhecido
//! ([`super::herdr_state`]: `working`, `blocked`, `idle`, `done`). Valor do
//! pane fora dessa lista nao e estado, e o transcript preenche.
//!
//! **A conta** e a de `skills/contexto-de-sessao/scripts/contexto.py` do
//! agentic-swarm, formato por formato (ver [`claude`], [`codex`], [`pi`]):
//! `tokens_in` = entrada + cache write + cache read, `tokens_out` = saida, e
//! `context_pct` = 100 * contexto do ultimo pedido / janela, com uma casa. A
//! janela e a do transcript quando ele diz (Codex) e cobre o maior contexto
//! visto; senao a menor de [`KNOWN_WINDOWS`] que o cobre, e no Claude nunca
//! menos de 1M ([`Usage::window`]).
//!
//! **Casamento** transcript -> processo:
//! - Claude: `projects/<slug do cwd>/<sessionId>.jsonl`, com o `sessionId`
//!   que o overlay `claude_state` pos em `agent_session_id` (roda antes, no
//!   registry). Sem ele, nada. Slug que nao bate (sessao retomada em outro
//!   diretorio, caminho longo) cai na busca em `projects/*/`.
//! - Codex e pi: primeiro um fd aberto em `/proc/<pid>/fd` apontando para um
//!   `.jsonl` da raiz do agente (o Codex mantem o rollout aberto; o pi, nesta
//!   maquina, nao: ele abre e fecha a cada escrita). Sem fd, os transcripts
//!   cujo cabecalho tem o mesmo `cwd` da sessao e que foram escritos depois do
//!   inicio do processo: um so, e ele; mais de um, o que comecou mais perto do
//!   inicio do processo (ate [`START_WINDOW_MS`]); e um transcript escolhido
//!   assim por dois processos vivos nao vale para nenhum dos dois. Por isso
//!   um casamento desses so e publicado a partir do tick seguinte ao da
//!   escolha, quando o desempate ja viu todos os processos. Ambiguo fica
//!   vazio: melhor vazio que errado.
//!
//! **Leitura incremental**: offset por arquivo, so os bytes acrescentados sao
//! lidos; linha parcial no fim espera completar; inode trocado ou arquivo
//! menor que o offset e lido de novo do zero. Arquivo e casamento sao
//! conferidos no maximo a cada [`REFRESH_EVERY`]; entre uma conferencia e
//! outra o tick so consulta o cache.
//!
//! **Teto por tick**: um tick le no maximo [`READ_BUDGET`] bytes de
//! transcript, somados todos os arquivos (a primeira leitura de um transcript
//! de 100 MB, inteira, custaria centenas de ms num tick de 5 ms). O que
//! sobra continua no tick seguinte, do ponto em que parou, inclusive no meio
//! de uma linha; e o arquivo ainda nao lido ate o fim uma primeira vez nao
//! publica tokens acumulados (parciais, seriam errados).
//!
//! **Fim do arquivo primeiro**: enquanto a varredura nao chegou ao fim, o
//! modelo e o contexto % vem da ultima linha de uso, lida de tras para frente
//! a partir do fim em blocos de [`TAIL_CHUNK`], parando na primeira linha
//! valida, ate [`TAIL_BUDGET`] por arquivo (fora do teto do tick). Assim eles
//! aparecem no primeiro tick em que o arquivo casa, mesmo num transcript de
//! GBs. A janela desse % e a do ultimo pedido (o maior contexto da sessao so a
//! varredura conhece); quando ela termina, valem os numeros dela, os de
//! `contexto.py`. Arquivo que cresce antes disso tem so os bytes novos lidos
//! de tras para frente de novo.
//!
//! **Subagentes** ([`subagents`], C15): `subagents_live` e `subagents_total`
//! da sessao Claude (`<sid>/subagents/agent-*.jsonl`) e Codex (rollouts com
//! `thread_spawn` do pai), com teto proprio por tick
//! ([`subagents::READ_BUDGET`]) e contadores proprios. Com
//! `transcript_read_budget` = [`READ_ALL`] (comando de um tiro) os subagentes
//! tambem sao lidos sem teto, e `subagents_live` sai ja no primeiro tick.

pub mod claude;
pub mod codex;
pub mod pi;
pub mod subagents;
pub mod tools;

use super::{HerdrOverlay, Overlay, OverlayStatus, SessionOverlay};
use crate::cockpit::{self, Cockpit, HandoffIndex, Loops, Thresholds, Tokens};
use crate::procfs::stat::parse_stat;
use crate::procfs::{ProcRoot, read_file_into};
use crate::sampler::{READ_ALL, SamplerConfig, USER_HZ};
use crate::session::Session;
use serde::de::{DeserializeOwned, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::marker::PhantomData;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tools::ToolCalls;

/// Intervalo minimo entre duas conferencias do mesmo arquivo ou casamento.
pub const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// Janelas conhecidas, as mesmas de `contexto.py` (`KNOWN_WINDOWS`).
pub const KNOWN_WINDOWS: [u64; 6] = [128_000, 200_000, 258_400, 400_000, 1_000_000, 2_000_000];
/// Janela minima do Claude quando o transcript nao a diz (`DEFAULT_WINDOW`).
pub const CLAUDE_MIN_WINDOW: u64 = 1_000_000;
/// Distancia maxima entre o inicio do processo e o do transcript para o
/// desempate por inicio mais proximo.
pub const START_WINDOW_MS: u64 = 120_000;
/// Folga entre o relogio do kernel (inicio do processo) e o mtime do arquivo.
const CLOCK_SLACK_MS: u64 = 2_000;
/// Uma linha maior que isso e descartada inteira (imagem embutida, saida
/// gigante): nunca tem contagem que falte nas outras.
const MAX_LINE: usize = 64 << 20;
/// Bloco de leitura do transcript.
const CHUNK: usize = 1 << 20;
/// Bytes de transcript lidos por tick, somados todos os arquivos. A ~330 MB/s
/// de parse (maquina carregada), 1 MiB e ~3 ms.
pub const READ_BUDGET: u64 = 1 << 20;
/// Bytes lidos de tras para frente, por arquivo, atras da ultima linha de uso
/// (a maior distancia medida nesta maquina, do fim ate ela, foi ~190 KB; o
/// resto e folga para uma imagem embutida).
pub const TAIL_BUDGET: u64 = 4 << 20;
/// Bloco da leitura de tras para frente.
const TAIL_CHUNK: u64 = 64 << 10;
/// Cabecalho (primeira linha) do Codex e do pi: lido ate isso.
const MAX_HEADER: usize = 1 << 20;
/// Modelo vem de fora: cortado aqui.
const MAX_MODEL: usize = 128;
/// Acima disso os caches sao zerados (sessao que nao volta nao vaza memoria).
const CACHE_CAP: usize = 1024;
/// Um casamento conta para o desempate entre processos enquanto o processo
/// apareceu num tick ha menos que isso.
const LIVE_LINK: Duration = Duration::from_secs(30);

/// Formato de transcript, um por agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Claude,
    Codex,
    Pi,
}

impl Format {
    /// Formato do agente da sessao (`Session::agent`).
    pub fn of(agent: &str) -> Option<Self> {
        match agent {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    /// Aplica uma linha completa (sem o `\n`) ao acumulado.
    pub fn feed(self, line: &[u8], u: &mut Usage) {
        match self {
            Self::Claude => claude::feed(line, u),
            Self::Codex => codex::feed(line, u),
            Self::Pi => pi::feed(line, u),
        }
    }
}

/// Acumulado de um transcript: so numeros, modelo e horarios.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Tokens de entrada sem cache.
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    /// Pedidos contados (Claude: por `requestId`; Codex: eventos `token_count`).
    pub requests: u64,
    /// Contexto do ultimo pedido.
    pub context: u64,
    pub context_max: u64,
    pub model: Option<String>,
    /// Janela que o transcript declara (so o Codex declara).
    pub window: Option<u64>,
    /// Horario da ultima linha com `timestamp`, em ms desde a epoca.
    pub last_at_ms: Option<u64>,
    /// Claude: `requestId` do ultimo pedido contado (o mesmo pedido aparece em
    /// varias linhas, uma por bloco de conteudo).
    pub(crate) last_request: Option<String>,
    pub tools: ToolCalls,
    /// Codex e pi: `Some(true)` com turno em andamento, `Some(false)` com o
    /// turno encerrado; `None` sem evento que diga (e sempre no Claude).
    pub busy: Option<bool>,
    /// Provider declarado, o ultimo visto (Codex: `session_meta.payload.
    /// model_provider`; pi: `provider` de `model_change` e da `assistant`).
    /// Cru (cortado em [`MAX_MODEL`]): [`cockpit::provider`] o valida.
    pub provider: Option<String>,
    /// Id da sessao do agente (Codex: `session_meta.payload.id`; pi: `id` da
    /// linha `session`), so se for um nome simples ([`valid_session_id`]).
    pub session_id: Option<String>,
}

impl Usage {
    pub fn tokens_in(&self) -> u64 {
        self.input
            .saturating_add(self.cache_write)
            .saturating_add(self.cache_read)
    }

    pub(crate) fn set_context(&mut self, v: u64) {
        self.context = v;
        self.context_max = self.context_max.max(v);
    }

    pub(crate) fn set_model(&mut self, m: Option<String>) {
        if let Some(m) = cut(m) {
            self.model = Some(m);
        }
    }

    pub(crate) fn set_provider(&mut self, p: Option<String>) {
        if let Some(p) = cut(p) {
            self.provider = Some(p);
        }
    }

    pub(crate) fn set_session_id(&mut self, id: Option<String>) {
        if let Some(id) = id.filter(|id| valid_session_id(id)) {
            self.session_id = Some(id);
        }
    }

    /// Horario da linha (tambem devolvido, para a tool call da linha).
    pub(crate) fn set_timestamp(&mut self, ts: Option<&str>) -> Option<u64> {
        let ms = ts.and_then(parse_iso_ms);
        if ms.is_some() {
            self.last_at_ms = ms;
        }
        ms
    }

    /// Janela usada no %: a declarada se cobre o maior contexto visto; senao a
    /// menor conhecida que o cobre (no Claude, no minimo 1M).
    pub fn window(&self, f: Format) -> u64 {
        self.window_inferred(f).0
    }

    /// [`Usage::window`] e se ela foi inferida (o `window_inferred` do
    /// script: o transcript nao a diz, ou o contexto passou dela).
    pub fn window_inferred(&self, f: Format) -> (u64, bool) {
        let declared = match f {
            Format::Claude => self
                .model
                .as_deref()
                .is_some_and(|m| m.contains("[1m]"))
                .then_some(CLAUDE_MIN_WINDOW),
            Format::Codex => self.window,
            Format::Pi => None,
        };
        match declared {
            Some(w) if w > 0 && self.context_max <= w => (w, false),
            _ => {
                let known = KNOWN_WINDOWS
                    .iter()
                    .copied()
                    .find(|&w| w >= self.context_max)
                    .unwrap_or(KNOWN_WINDOWS[KNOWN_WINDOWS.len() - 1]);
                if f == Format::Claude {
                    (known.max(CLAUDE_MIN_WINDOW), true)
                } else {
                    (known, true)
                }
            }
        }
    }

    /// Entrada das contas do cockpit; `None` sem pedido contado (o script
    /// nao analisa a sessao).
    pub fn tokens(&self, f: Format) -> Option<Tokens> {
        let (window, window_inferred) = self.window_inferred(f);
        (self.requests > 0).then_some(Tokens {
            input: self.input,
            cache_write: self.cache_write,
            cache_read: self.cache_read,
            output: self.output,
            ctx: self.context,
            window,
            window_inferred,
        })
    }

    /// `round(100 * contexto / janela, 1)`, como `contexto.py`.
    pub fn context_pct(&self, f: Format) -> f64 {
        let w = self.window(f);
        let pct = if w == 0 {
            0.0
        } else {
            100.0 * self.context as f64 / w as f64
        };
        round1(pct)
    }

    /// O que o overlay publica agora (relogio de parede para o por minuto).
    pub fn meta(&self, f: Format) -> TranscriptMeta {
        self.meta_at(f, now_ms())
    }

    /// [`Usage::meta`] com `now_ms` (ms desde a epoca) como agora. Sem pedido
    /// contado nao ha tokens; tool calls sempre (zero e numero).
    pub fn meta_at(&self, f: Format, now_ms: u64) -> TranscriptMeta {
        let counted = self.requests > 0;
        TranscriptMeta {
            model: self.model.clone(),
            tokens_in: counted.then(|| self.tokens_in()),
            tokens_out: counted.then_some(self.output),
            context_pct: counted.then(|| self.context_pct(f)),
            last_at_ms: self.last_at_ms,
            tool_calls: Some(self.tools.total),
            tool_calls_open: Some(self.tools.open()),
            tool_calls_per_min: Some(self.tools.per_min(now_ms)),
            status: self.busy.map(|b| {
                if b {
                    OverlayStatus::Busy
                } else {
                    OverlayStatus::Idle
                }
            }),
            provider: self.provider.clone(),
            session_id: self.session_id.clone(),
            tokens: self.tokens(f),
            loops: Some(self.tools.loops()),
            pending: false,
            cockpit: None,
        }
    }

    /// [`Usage::meta`] sem os acumulados (tokens, tool calls, estado): o que a
    /// leitura do fim sabe.
    fn tail_meta(&self, f: Format) -> TranscriptMeta {
        TranscriptMeta {
            model: self.model.clone(),
            context_pct: (self.requests > 0).then(|| self.context_pct(f)),
            last_at_ms: self.last_at_ms,
            provider: self.provider.clone(),
            ..TranscriptMeta::default()
        }
    }
}

/// Metadados prontos para o overlay, calculados quando o arquivo muda.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptMeta {
    pub model: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub context_pct: Option<f64>,
    /// Ultima mensagem (ms desde a epoca). Ainda sem campo no contrato.
    pub last_at_ms: Option<u64>,
    pub tool_calls: Option<u64>,
    pub tool_calls_open: Option<u64>,
    pub tool_calls_per_min: Option<f64>,
    pub status: Option<OverlayStatus>,
    /// Provider declarado, cru ([`Usage::provider`]).
    pub provider: Option<String>,
    pub session_id: Option<String>,
    /// Entrada do cockpit, so com a varredura completa.
    pub tokens: Option<Tokens>,
    /// Sinal de loop do cockpit, so com a varredura completa.
    pub loops: Option<Loops>,
    /// Varredura ainda incompleta.
    pub pending: bool,
    /// Calculado na conferencia do arquivo ([`Transcripts`]).
    pub cockpit: Option<Cockpit>,
}

impl TranscriptMeta {
    fn fill(&self, o: &mut SessionOverlay) {
        if o.agent_session_id.is_none() {
            o.agent_session_id.clone_from(&self.session_id);
        }
        if self.cockpit.is_some() {
            o.cockpit.clone_from(&self.cockpit);
        }
        o.cockpit_pending = self.pending && o.cockpit.is_none();
        if self.model.is_some() {
            o.model.clone_from(&self.model);
        }
        if self.tokens_in.is_some() {
            o.tokens_in = self.tokens_in;
            o.tokens_out = self.tokens_out;
        }
        if self.context_pct.is_some() {
            o.context_pct = self.context_pct;
        }
        if self.tool_calls.is_some() {
            o.tool_calls = self.tool_calls;
            o.tool_calls_open = self.tool_calls_open;
            o.tool_calls_per_min = self.tool_calls_per_min;
        }
        // Estado declarado antes (sessions file do Claude ou pane do herdr)
        // vale sobre o do transcript: um pane `blocked` nao vira busy/idle.
        // Valor do pane que o herdr nao publica hoje nao e estado: o
        // transcript preenche (`herdr_state`).
        let herdr_state = o.herdr.as_ref().and_then(HerdrOverlay::state);
        if o.status.is_none() && herdr_state.is_none() {
            o.status = self.status;
        }
    }
}

/// Texto vindo do transcript, nao vazio e cortado em [`MAX_MODEL`] bytes.
fn cut(v: Option<String>) -> Option<String> {
    let mut v = v.filter(|v| !v.is_empty())?;
    if v.len() > MAX_MODEL {
        let mut end = MAX_MODEL;
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        v.truncate(end);
    }
    Some(v)
}

/// Minutos de `then` ate `now` (negativo se `then` esta no futuro).
fn minutes_since(now: SystemTime, then: SystemTime) -> f64 {
    match now.duration_since(then) {
        Ok(d) => d.as_secs_f64() / 60.0,
        Err(e) => -e.duration().as_secs_f64() / 60.0,
    }
}

/// Agora em ms desde a epoca.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Uma casa decimal com o arredondamento do `round` do Python (o da
/// formatacao: valor decimal exato, empate para o par).
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Le uma linha JSON no tipo `T`. Bytes que nao sao UTF-8 viram U+FFFD antes
/// de desistir, como o `errors="replace"` do script. Linha que so falhou
/// porque o nome ou os argumentos de uma tool call nao se leem como valor
/// (surrogate solto, numero fora do `f64`) e lida de novo sem eles
/// ([`tools::skipping`]): a linha conta, a chamada fica fora do sinal de loop.
pub(crate) fn parse_line<T: DeserializeOwned>(line: &[u8]) -> Option<T> {
    tools::take_touched();
    parse_bytes(line).or_else(|| {
        if tools::take_touched() {
            tools::skipping(|| parse_bytes(line))
        } else {
            None
        }
    })
}

fn parse_bytes<T: DeserializeOwned>(line: &[u8]) -> Option<T> {
    match serde_json::from_slice(line) {
        Ok(v) => Some(v),
        Err(_) if std::str::from_utf8(line).is_err() => {
            serde_json::from_str(&String::from_utf8_lossy(line)).ok()
        }
        Err(_) => None,
    }
}

/// Desserializadores tolerantes: campo com tipo inesperado vira `None` em vez
/// de derrubar a linha, e o valor ignorado e consumido sem ser guardado.
pub(crate) mod lenient {
    use super::*;

    fn drain_map<'de, A: MapAccess<'de>>(mut m: A) -> Result<(), A::Error> {
        while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(())
    }

    fn drain_seq<'de, A: SeqAccess<'de>>(mut s: A) -> Result<(), A::Error> {
        while s.next_element::<IgnoredAny>()?.is_some() {}
        Ok(())
    }

    struct Str;
    impl<'de> Visitor<'de> for Str {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }
        fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Self::Value, A::Error> {
            drain_map(m).map(|()| None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, s: A) -> Result<Self::Value, A::Error> {
            drain_seq(s).map(|()| None)
        }
    }

    struct Num;
    impl<'de> Visitor<'de> for Num {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
            Ok(u64::try_from(v).ok())
        }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Self::Value, A::Error> {
            drain_map(m).map(|()| None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, s: A) -> Result<Self::Value, A::Error> {
            drain_seq(s).map(|()| None)
        }
    }

    struct Obj<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Obj<T> {
        type Value = Option<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Self::Value, A::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(m)).map(Some)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, s: A) -> Result<Self::Value, A::Error> {
            drain_seq(s).map(|()| None)
        }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        d.deserialize_any(Str)
    }

    pub fn number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        d.deserialize_any(Num)
    }

    /// Numero ausente ou de outro tipo conta 0, como `u.get(k, 0)`.
    pub fn count<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        d.deserialize_any(Num).map(|v| v.unwrap_or(0))
    }

    pub fn object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<T>, D::Error> {
        d.deserialize_any(Obj(PhantomData))
    }

    /// Texto vira o seu hash ([`tools::id_hash`]), sem `String`.
    struct Hash;
    impl<'de> Visitor<'de> for Hash {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(tools::id_hash(v.as_bytes())))
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Self::Value, A::Error> {
            drain_map(m).map(|()| None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, s: A) -> Result<Self::Value, A::Error> {
            drain_seq(s).map(|()| None)
        }
    }

    /// Id (de tool call) como hash; outro tipo vira `None`.
    pub fn hash<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        d.deserialize_any(Hash)
    }

    /// Elemento de lista: objeto vira `T`, o resto `None`.
    struct Each<T>(Option<T>);
    impl<'de, T: Deserialize<'de>> Deserialize<'de> for Each<T> {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(Obj(PhantomData)).map(Each)
        }
    }

    struct List<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for List<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(Each(x)) = s.next_element::<Each<T>>()? {
                out.extend(x);
            }
            Ok(out)
        }
        fn visit_map<A: MapAccess<'de>>(self, m: A) -> Result<Self::Value, A::Error> {
            drain_map(m).map(|()| Vec::new())
        }
        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }

    /// Lista de objetos `T` (os elementos que nao sao objeto saem); outro tipo
    /// (texto, como o `content` de um prompt) vira lista vazia.
    pub fn list<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
        d.deserialize_any(List(PhantomData))
    }
}

/// `2026-10-17T02:53:28.810Z` (ou com `+HH:MM`) em ms desde a epoca.
pub fn parse_iso_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = b.get(r)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| std::str::from_utf8(part).ok()?.parse().ok())?
    };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        let frac = &s[start..i];
        let first3: String = frac.chars().chain("000".chars()).take(3).collect();
        ms = first3.parse().ok()?;
    }
    let offset_s = match b.get(i)? {
        b'Z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if b.len() == i + 6 && b[i + 3] == b':' => {
            let off = num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60;
            if *sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset_s;
    u64::try_from(secs * 1000 + ms).ok()
}

/// Dias desde 1970-01-01 (algoritmo de Howard Hinnant, dominio publico).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverso de [`days_from_civil`]: (ano, mes, dia).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// Cabecalho de um transcript Codex ou pi: diretorio e inicio da sessao.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub cwd: Option<String>,
    pub started_ms: Option<u64>,
}

/// Raizes onde cada agente grava os transcripts. `None` desliga o formato.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// `~/.claude/projects`.
    pub claude: Option<PathBuf>,
    /// `~/.codex/sessions`.
    pub codex: Option<PathBuf>,
    /// `~/.pi/agent/sessions`.
    pub pi: Option<PathBuf>,
}

impl Roots {
    /// Todas sob um `HOME` (como o agente grava sem configuracao).
    pub fn under_home(home: &Path) -> Self {
        Self {
            claude: Some(home.join(".claude/projects")),
            codex: Some(home.join(".codex/sessions")),
            pi: Some(home.join(".pi/agent/sessions")),
        }
    }

    /// Pelo ambiente: `CLAUDE_CONFIG_DIR`, `CODEX_HOME` e
    /// `PI_CODING_AGENT_DIR` quando definidos, senao sob `HOME`.
    pub fn from_env(env: impl Fn(&str) -> Option<OsString>) -> Self {
        let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = get("HOME");
        let under = |var: &str, own: &str, default: &str| {
            get(var)
                .map(|d| d.join(own))
                .or_else(|| home.as_ref().map(|h| h.join(default)))
        };
        Self {
            claude: under("CLAUDE_CONFIG_DIR", "projects", ".claude/projects"),
            codex: under("CODEX_HOME", "sessions", ".codex/sessions"),
            pi: under("PI_CODING_AGENT_DIR", "sessions", ".pi/agent/sessions"),
        }
    }

    pub fn get(&self, f: Format) -> Option<&Path> {
        match f {
            Format::Claude => self.claude.as_deref(),
            Format::Codex => self.codex.as_deref(),
            Format::Pi => self.pi.as_deref(),
        }
    }
}

/// Estado de leitura de um arquivo.
struct FileState {
    dev: u64,
    ino: u64,
    /// Bytes do arquivo ja consumidos (linhas aplicadas mais `partial`).
    offset: u64,
    /// Comeco de linha lido e ainda sem `\n` (o teto do tick cortou, ou o
    /// agente ainda esta escrevendo).
    partial: Vec<u8>,
    /// No meio de uma linha maior que [`MAX_LINE`]: pula ate o proximo `\n`.
    skipping: bool,
    /// Ja chegou ao fim do arquivo alguma vez desde o comeco: os numeros valem.
    ready: bool,
    /// A ultima leitura chegou ao fim do arquivo.
    at_end: bool,
    usage: Usage,
    /// Antes de `ready`: modelo, contexto e janela da ultima linha de uso,
    /// achados de tras para frente ([`read_tail`]).
    tail: Usage,
    /// Fim da ultima linha completa ja olhada de tras para frente.
    tail_end: u64,
    meta: TranscriptMeta,
    checked: Option<Instant>,
    /// mtime do arquivo na ultima conferencia: o ocio do cockpit.
    mtime: Option<SystemTime>,
}

/// Casamento de um processo com o seu transcript.
struct Link {
    format: Format,
    /// Claude: o `sessionId` que levou ao arquivo.
    session_id: Option<String>,
    /// Escolhido pelo casamento (antes do desempate entre processos).
    chosen: Option<Arc<Path>>,
    /// O que vale: `chosen`, ou `None` se outro processo vivo escolheu o mesmo.
    path: Option<Arc<Path>>,
    certain: bool,
    /// Casamento incerto (cwd + inicio) ainda nao publicado: so vale a partir
    /// do tick seguinte, quando todo processo do tick ja teve a chance de
    /// escolher o mesmo arquivo (o desempate so ve quem ja passou).
    pending: bool,
    at: Instant,
    seen: Instant,
}

#[derive(Default)]
struct Inner {
    files: HashMap<Arc<Path>, FileState>,
    links: HashMap<u32, Link>,
    headers: HashMap<PathBuf, (u64, Header)>,
    btime_ms: Option<u64>,
    buf: Vec<u8>,
    text: String,
    /// Bytes que ainda podem ser lidos neste tick.
    budget: u64,
    /// Pids ja vistos neste tick: um repetido marca o tick seguinte.
    tick_pids: Vec<u32>,
    handoffs: Handoffs,
    subagents: subagents::Subagents,
}

/// Ultimo handoff por `cwd` de sessao, consultado no maximo a cada
/// `refresh_every`; o indice so refaz o `readdir` quando o mtime do diretorio
/// muda.
#[derive(Default)]
struct Handoffs {
    index: HandoffIndex,
    by_cwd: HashMap<String, Asked>,
    by_session: HashMap<(PathBuf, String), Asked>,
}

/// Uma consulta ao indice de handoffs.
struct Asked {
    at: Instant,
    /// Relogio de parede da consulta: a idade guardada vale nele.
    wall: SystemTime,
    /// (nome do arquivo, idade em minutos).
    found: Option<(String, f64)>,
}

impl Handoffs {
    fn agentcenter(
        &mut self,
        dir: &Path,
        sid: &str,
        now: Instant,
        wall: SystemTime,
    ) -> Option<(String, f64)> {
        let key = (dir.to_path_buf(), sid.to_owned());
        if let Some(a) = self.by_session.get(&key)
            && now.duration_since(a.at) < REFRESH_EVERY
        {
            let (name, age) = a.found.as_ref()?;
            return Some((name.clone(), age + minutes_since(wall, a.wall)));
        }
        let found = self.index.find_agentcenter(dir, sid, wall).map(|h| {
            (
                h.path.file_name().unwrap().to_string_lossy().into_owned(),
                h.age_min,
            )
        });
        if self.by_session.len() >= CACHE_CAP && !self.by_session.contains_key(&key) {
            self.by_session.clear();
        }
        self.by_session.insert(
            key,
            Asked {
                at: now,
                wall,
                found: found.clone(),
            },
        );
        found
    }

    /// (nome do arquivo, idade em minutos em `wall`) do ultimo handoff do
    /// projeto de `cwd`. Consulta o indice no maximo a cada `every` por cwd;
    /// entre uma consulta e outra a idade anda com o relogio.
    fn latest(
        &mut self,
        cwd: &str,
        home: Option<&Path>,
        now: Instant,
        wall: SystemTime,
        every: Duration,
    ) -> Option<(String, f64)> {
        if let Some(a) = self.by_cwd.get(cwd)
            && now.duration_since(a.at) < every
        {
            let (name, age) = a.found.as_ref()?;
            return Some((name.clone(), age + minutes_since(wall, a.wall)));
        }
        let found = self.index.find(Path::new(cwd), home, wall).map(|h| {
            let name = h
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name, h.age_min)
        });
        if self.by_cwd.len() >= CACHE_CAP && !self.by_cwd.contains_key(cwd) {
            self.by_cwd.clear();
        }
        let asked = Asked {
            at: now,
            wall,
            found: found.clone(),
        };
        self.by_cwd.insert(cwd.to_owned(), asked);
        found
    }
}

/// O overlay. Preenche `model`, `tokens_in`, `tokens_out`, `context_pct`,
/// `tool_calls`, `tool_calls_open`, `tool_calls_per_min`, `cockpit`,
/// `cockpit_pending`, `agent_session_id` (Codex e pi, se vazio),
/// `subagents_live` e `subagents_total` (Claude e Codex) e, no Codex e no pi
/// sem estado de outro overlay, `status`. `provider` sai para toda
/// sessao cujo agente ou transcript o diga, com transcript ou sem.
pub struct Transcripts {
    roots: Roots,
    proc: ProcRoot,
    /// Raiz dos handoffs globais (`<home>/.claude/handoffs`). `None`: so o
    /// handoff local do repo da sessao conta, sem o fallback global por nome.
    home: Option<PathBuf>,
    agentcenter_handoffs: Option<PathBuf>,
    thresholds: Thresholds,
    refresh_every: Duration,
    /// Teto por tick; [`Overlay::configure`] o troca por `&self`.
    read_budget: AtomicU64,
    /// Teto de primeira leitura de subagente por tick
    /// ([`subagents::READ_BUDGET`]; [`READ_ALL`] quando o dos transcripts e
    /// [`READ_ALL`]).
    sub_read_budget: AtomicU64,
    inner: Mutex<Inner>,
    bytes_read: AtomicU64,
    tail_bytes: AtomicU64,
    sub_counters: subagents::Counters,
}

impl Transcripts {
    /// `roots`: onde ler; `proc`: raiz para fds, `cwd` e inicio dos processos.
    pub fn new(roots: Roots, proc: ProcRoot) -> Self {
        Self {
            roots,
            proc,
            home: None,
            agentcenter_handoffs: None,
            thresholds: Thresholds::default(),
            refresh_every: REFRESH_EVERY,
            read_budget: AtomicU64::new(READ_BUDGET),
            sub_read_budget: AtomicU64::new(subagents::READ_BUDGET),
            inner: Mutex::new(Inner {
                budget: READ_BUDGET,
                subagents: subagents::Subagents::new(),
                ..Inner::default()
            }),
            bytes_read: AtomicU64::new(0),
            tail_bytes: AtomicU64::new(0),
            sub_counters: subagents::Counters::default(),
        }
    }

    /// Raizes pelo ambiente ([`Roots::from_env`]), `/proc` real, handoffs
    /// globais sob `HOME` e limiares do ambiente do processo
    /// ([`Thresholds::from_process_env`]).
    pub fn from_env() -> Self {
        let tr = Self::new(
            Roots::from_env(|k| std::env::var_os(k)),
            ProcRoot::default(),
        )
        .with_thresholds(Thresholds::from_process_env());
        let mut tr = match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            Some(home) => tr.with_home(PathBuf::from(home)),
            None => tr,
        };
        if let Some(state) = std::env::var_os("XDG_STATE_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            tr.agentcenter_handoffs = Some(state.join("agentcenter/handoffs"));
        }
        tr
    }

    /// `HOME` dos handoffs globais do cockpit ([`HandoffIndex::latest`]).
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        self.agentcenter_handoffs = Some(home.join(".local/state/agentcenter/handoffs"));
        self.home = Some(home);
        self
    }

    /// Limiares do cockpit (padrao: os do script sem ambiente).
    pub fn with_thresholds(mut self, th: Thresholds) -> Self {
        self.thresholds = th;
        self
    }

    /// Troca o intervalo de conferencia (teste: `Duration::ZERO`).
    pub fn with_refresh(mut self, every: Duration) -> Self {
        self.refresh_every = every;
        self
    }

    /// Troca o teto de bytes lidos por tick ([`READ_BUDGET`]).
    pub fn with_read_budget(mut self, bytes: u64) -> Self {
        *self.read_budget.get_mut() = bytes;
        self.inner
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .budget = bytes;
        self
    }

    pub fn roots(&self) -> &Roots {
        &self.roots
    }

    /// Bytes de transcript lidos ate agora (linhas, cabecalhos e fim lido de
    /// tras para frente).
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// Parte de [`Self::bytes_read`] lida de tras para frente.
    pub fn tail_bytes_read(&self) -> u64 {
        self.tail_bytes.load(Ordering::Relaxed)
    }

    /// Leituras do fim de `agent-*.jsonl` de subagente do Claude ate agora
    /// (um arquivo terminado e lido uma vez).
    pub fn subagent_tail_reads(&self) -> u64 {
        self.sub_counters.tail_reads.load(Ordering::Relaxed)
    }

    /// Bytes lidos para os subagentes (fora de [`Self::bytes_read`]).
    pub fn subagent_bytes_read(&self) -> u64 {
        self.sub_counters.bytes.load(Ordering::Relaxed)
    }

    /// `readdir` de diretorio de handoff feitos ate agora (o indice so relista
    /// um diretorio cujo mtime mudou).
    pub fn handoff_readdirs(&self) -> u64 {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.handoffs.index.readdir_count()
    }

    /// Transcript casado com o pid no ultimo tick, se houver.
    pub fn transcript_of(&self, pid: u32) -> Option<PathBuf> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .links
            .get(&pid)?
            .path
            .as_deref()
            .map(Path::to_path_buf)
    }

    /// Le um arquivo do zero, sem cache nem casamento (teste e diagnostico).
    pub fn read_file(path: &Path, f: Format) -> std::io::Result<Usage> {
        let mut st = FileState::empty();
        let mut buf = Vec::new();
        let mut unlimited = u64::MAX;
        let (total, tail) = (AtomicU64::new(0), AtomicU64::new(0));
        read_new(path, f, &mut st, &mut buf, [&total, &tail], &mut unlimited)?;
        Ok(st.usage)
    }

    fn link(
        &self,
        inner: &mut Inner,
        s: &Session,
        f: Format,
        root: &Path,
        sid: Option<&str>,
        now: Instant,
    ) -> Option<Arc<Path>> {
        if let Some(l) = inner.links.get_mut(&s.pid)
            && l.format == f
            && l.session_id.as_deref() == sid
            && now.duration_since(l.at) < self.refresh_every
        {
            l.seen = now;
            l.pending = false;
            return l.path.clone();
        }
        let (chosen, certain) = match (f, sid) {
            (Format::Claude, Some(sid)) => (claude::find(root, s.cwd.as_deref(), sid), true),
            (Format::Claude, None) => (None, false),
            _ => self.by_process(inner, s, f, root),
        };
        let chosen: Option<Arc<Path>> = chosen.map(Into::into);
        // Ja escolhido no tick anterior: o desempate daquele tick ja correu.
        let seen_before = inner
            .links
            .get(&s.pid)
            .is_some_and(|l| l.format == f && l.chosen == chosen);
        let pending = !certain && chosen.is_some() && !seen_before;
        let mut path = chosen.clone();
        // Desempate entre processos: o mesmo arquivo escolhido por dois vivos.
        if let Some(p) = &chosen {
            for (pid, other) in &mut inner.links {
                if *pid == s.pid
                    || other.chosen.as_deref() != Some(&**p)
                    || now.duration_since(other.seen) >= LIVE_LINK
                {
                    continue;
                }
                if !certain {
                    path = None;
                }
                if !other.certain {
                    other.path = None;
                }
            }
        }
        if inner.links.len() >= CACHE_CAP && !inner.links.contains_key(&s.pid) {
            inner.links.clear();
        }
        inner.links.insert(
            s.pid,
            Link {
                format: f,
                session_id: sid.map(str::to_string),
                chosen,
                path: path.clone(),
                certain,
                pending,
                at: now,
                seen: now,
            },
        );
        if pending { None } else { path }
    }

    /// Codex e pi: fd aberto, senao cwd + inicio. `(arquivo, por fd)`.
    fn by_process(
        &self,
        inner: &mut Inner,
        s: &Session,
        f: Format,
        root: &Path,
    ) -> (Option<PathBuf>, bool) {
        match open_transcripts(&self.proc, s.pid, root, f).as_slice() {
            [one] => return (Some(one.clone()), true),
            [] => {}
            _ => return (None, true),
        }
        let Some(cwd) = s.cwd.as_deref() else {
            return (None, false);
        };
        let Some(start) = self.proc_start_ms(inner, s.pid) else {
            return (None, false);
        };
        let now_ms = now_ms();
        let files = match f {
            Format::Codex => codex::candidates(root, start, now_ms),
            Format::Pi => pi::candidates(root, cwd),
            Format::Claude => Vec::new(),
        };
        let mut found = Vec::new();
        for (path, mtime_ms) in files {
            if mtime_ms + CLOCK_SLACK_MS < start {
                continue;
            }
            let Some(h) = self.header(inner, &path, f) else {
                continue;
            };
            if h.cwd.as_deref() == Some(cwd) {
                found.push((path, h.started_ms));
            }
        }
        (choose(found, start), false)
    }

    /// Inicio do processo em ms desde a epoca: `btime` + campo 22 do `stat`.
    fn proc_start_ms(&self, inner: &mut Inner, pid: u32) -> Option<u64> {
        if inner.btime_ms.is_none() {
            read_file_into(&self.proc.path().join("stat"), &mut inner.text).ok()?;
            inner.btime_ms = inner
                .text
                .lines()
                .find_map(|l| l.strip_prefix("btime "))
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|s| s * 1000);
        }
        let btime = inner.btime_ms?;
        read_file_into(&self.proc.pid_path(pid, "stat"), &mut inner.text).ok()?;
        let st = parse_stat(&inner.text).ok()?;
        Some(btime + (st.starttime as f64 * 1000.0 / USER_HZ) as u64)
    }

    /// Cabecalho do arquivo, lido uma vez por inode.
    fn header(&self, inner: &mut Inner, path: &Path, f: Format) -> Option<Header> {
        let ino = std::fs::metadata(path).ok()?.ino();
        if let Some((i, h)) = inner.headers.get(path)
            && *i == ino
        {
            return Some(h.clone());
        }
        let line = first_line(path, &mut inner.buf, &self.bytes_read)?;
        let h = match f {
            Format::Codex => codex::header(line),
            Format::Pi => pi::header(line),
            Format::Claude => return None,
        };
        if inner.headers.len() >= CACHE_CAP {
            inner.headers.clear();
        }
        inner.headers.insert(path.to_path_buf(), (ino, h.clone()));
        Some(h)
    }

    /// Metadados atuais do arquivo, relendo so o que foi acrescentado. A cada
    /// releitura o cockpit e refeito: tokens do arquivo inteiro, ocio pelo
    /// mtime e handoff do projeto de `cwd` (o cwd da sessao; o script usa o do
    /// transcript, que e o mesmo enquanto o agente nao troca de diretorio).
    fn refresh<'a>(
        &self,
        inner: &'a mut Inner,
        path: &Arc<Path>,
        f: Format,
        cwd: Option<&str>,
        now: Instant,
    ) -> Option<&'a TranscriptMeta> {
        if inner.files.len() >= CACHE_CAP && !inner.files.contains_key(&**path) {
            inner.files.clear();
        }
        let Inner {
            files,
            buf,
            budget,
            handoffs,
            ..
        } = inner;
        let st = files.entry(path.clone()).or_insert_with(FileState::empty);
        // Leitura cortada pelo teto continua no tick seguinte, sem esperar.
        let fresh = st.at_end
            && st
                .checked
                .is_some_and(|c| now.duration_since(c) < self.refresh_every);
        let ok = fresh || {
            st.checked = Some(now);
            let counters = [&self.bytes_read, &self.tail_bytes];
            read_new(path, f, st, buf, counters, budget).is_ok()
        };
        if !ok {
            files.remove(&**path);
            return None;
        }
        let st = files.get_mut(&**path)?;
        if !fresh {
            st.meta.cockpit = st.meta.tokens.map(|t| {
                let wall = SystemTime::now();
                let idle = st.mtime.map(|m| minutes_since(wall, m));
                let home = self.home.as_deref();
                let native_handoff = (f == Format::Pi)
                    .then(|| {
                        self.agentcenter_handoffs
                            .as_deref()
                            .zip(st.meta.session_id.as_deref())
                            .and_then(|(dir, sid)| handoffs.agentcenter(dir, sid, now, wall))
                    })
                    .flatten();
                let handoff = native_handoff.or_else(|| {
                    cwd.and_then(|cwd| handoffs.latest(cwd, home, now, wall, self.refresh_every))
                });
                let handoff = handoff.as_ref().map(|(n, age)| (n.as_str(), *age));
                let loops = st.meta.loops.unwrap_or_default();
                Cockpit::compute_with_loops(&self.thresholds, &t, loops, idle, handoff)
            });
        }
        Some(&st.meta)
    }
}

impl FileState {
    fn empty() -> Self {
        Self {
            dev: 0,
            ino: 0,
            offset: 0,
            partial: Vec::new(),
            skipping: false,
            ready: false,
            at_end: false,
            usage: Usage::default(),
            tail: Usage::default(),
            tail_end: 0,
            meta: TranscriptMeta::default(),
            checked: None,
            mtime: None,
        }
    }
}

/// Le de `st.offset` ate o fim, ou ate gastar `budget`, e aplica as linhas
/// completas; sem ter chegado ao fim, le tambem o fim de tras para frente
/// ([`read_tail`]). Inode trocado ou arquivo menor que o offset: recomeca do
/// zero. `counters`: bytes lidos (todos, e so os do fim).
fn read_new(
    path: &Path,
    f: Format,
    st: &mut FileState,
    buf: &mut Vec<u8>,
    [counter, tail_counter]: [&AtomicU64; 2],
    budget: &mut u64,
) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    if (meta.dev(), meta.ino()) != (st.dev, st.ino) || meta.len() < st.offset {
        *st = FileState {
            dev: meta.dev(),
            ino: meta.ino(),
            checked: st.checked,
            ..FileState::empty()
        };
    }
    if meta.len() == st.offset {
        st.at_end = true;
    } else {
        st.at_end = false;
        file.seek(SeekFrom::Start(st.offset))?;
        buf.clear();
        buf.append(&mut st.partial);
        while *budget > 0 {
            let old = buf.len();
            let want = CHUNK.min(usize::try_from(*budget).unwrap_or(CHUNK));
            buf.resize(old + want, 0);
            let n = file.read(&mut buf[old..])?;
            buf.truncate(old + n);
            counter.fetch_add(n as u64, Ordering::Relaxed);
            *budget -= n as u64;
            st.offset += n as u64;
            if n == 0 {
                st.at_end = true;
                break;
            }
            let mut start = 0;
            let mut scan = old;
            while let Some(i) = buf[scan..].iter().position(|&b| b == b'\n') {
                let end = scan + i;
                if !st.skipping {
                    f.feed(&buf[start..end], &mut st.usage);
                }
                st.skipping = false;
                start = end + 1;
                scan = start;
            }
            buf.drain(..start);
            if buf.len() > MAX_LINE {
                // Linha gigante: descarta o que ja veio e pula ate o proximo `\n`.
                buf.clear();
                st.skipping = true;
            }
        }
        st.at_end |= st.offset >= meta.len();
        st.partial.clear();
        st.partial.extend_from_slice(buf);
        if st.partial.is_empty() {
            st.partial = Vec::new();
        }
    }
    st.ready |= st.at_end;
    if !st.ready && meta.len() > st.tail_end {
        let counters = [counter, tail_counter];
        st.tail_end = read_tail(
            &mut file,
            st.tail_end,
            meta.len(),
            f,
            &mut st.tail,
            counters,
        )?;
    }
    st.meta = if st.ready {
        st.usage.meta(f)
    } else {
        st.tail.tail_meta(f)
    };
    st.meta.pending = !st.ready;
    st.mtime = meta.modified().ok();
    Ok(())
}

/// O que a leitura de tras para frente achou numa passada.
#[derive(Default)]
struct TailFound {
    usage: Option<Usage>,
    model: Option<String>,
    at_ms: Option<u64>,
    /// O provider mais novo que a passada viu (o pi o poe na `assistant`).
    provider: Option<String>,
}

impl TailFound {
    /// Aplica uma linha completa, da mais nova para a mais velha. `true`
    /// quando ja tem a ultima linha de uso e o ultimo modelo.
    fn feed(&mut self, f: Format, line: &[u8]) -> bool {
        let mut one = Usage::default();
        f.feed(line, &mut one);
        if self.at_ms.is_none() {
            self.at_ms = one.last_at_ms;
        }
        if self.model.is_none() {
            self.model.clone_from(&one.model);
        }
        if self.provider.is_none() {
            self.provider.clone_from(&one.provider);
        }
        if self.usage.is_none() && one.requests > 0 {
            self.usage = Some(one);
        }
        self.usage.is_some() && self.model.is_some()
    }

    /// Por cima do que passadas anteriores (linhas mais velhas) acharam.
    fn apply(self, tail: &mut Usage) {
        if let Some(u) = self.usage {
            tail.requests = 1;
            tail.context = u.context;
            tail.context_max = u.context;
            // `info.get("model_context_window") or st["window"]`.
            tail.window = u.window.or(tail.window);
        }
        if self.model.is_some() {
            tail.model = self.model;
        }
        if self.at_ms.is_some() {
            tail.last_at_ms = self.at_ms;
        }
        if self.provider.is_some() {
            tail.provider = self.provider;
        }
    }
}

/// Le `[stop, len)` de tras para frente, em blocos de [`TAIL_CHUNK`] e ate
/// [`TAIL_BUDGET`], parando na primeira linha de uso (e de modelo), e aplica o
/// achado em `tail`. `stop` e comeco de linha (0 ou o retorno da passada
/// anterior); a linha sem `\n` no fim ainda esta sendo escrita e fica para a
/// proxima. Devolve o fim da ultima linha completa olhada.
fn read_tail(
    file: &mut File,
    stop: u64,
    len: u64,
    f: Format,
    tail: &mut Usage,
    counters: [&AtomicU64; 2],
) -> std::io::Result<u64> {
    let lo = stop.max(len.saturating_sub(TAIL_BUDGET));
    let mut found = TailFound::default();
    // Fim da ultima linha completa da regiao, quando o `\n` dela ja apareceu.
    let mut end: Option<u64> = None;
    // A regiao `[lo, len)`, preenchida da direita para a esquerda; `buf[at..]`
    // ja foi lido e `buf[at..right]` ainda nao tem `\n` a esquerda.
    let mut buf = vec![0u8; (len - lo) as usize];
    let mut at = buf.len();
    let mut right = buf.len();
    let mut done = false;
    while at > 0 && !done {
        let start = at.saturating_sub(TAIL_CHUNK as usize);
        file.seek(SeekFrom::Start(lo + start as u64))?;
        file.read_exact(&mut buf[start..at])?;
        for c in counters {
            c.fetch_add((at - start) as u64, Ordering::Relaxed);
        }
        at = start;
        while let Some(i) = buf[at..right].iter().rposition(|&b| b == b'\n') {
            let i = at + i;
            if end.is_none() {
                // O que vem depois do ultimo `\n` e linha incompleta.
                end = Some(lo + i as u64 + 1);
            } else if found.feed(f, &buf[i + 1..right]) {
                done = true;
                break;
            }
            right = i;
        }
    }
    // Chegou a `stop`, comeco de linha: o que sobrou e a linha mais velha.
    if !done && lo == stop && end.is_some() && right > 0 {
        found.feed(f, &buf[..right]);
    }
    found.apply(tail);
    Ok(end.unwrap_or(stop))
}

/// Primeira linha completa do arquivo (ate [`MAX_HEADER`]).
fn first_line<'a>(path: &Path, buf: &'a mut Vec<u8>, counter: &AtomicU64) -> Option<&'a [u8]> {
    let mut file = File::open(path).ok()?;
    buf.clear();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = file.read(&mut chunk).ok()?;
        counter.fetch_add(n as u64, Ordering::Relaxed);
        if n == 0 {
            return None;
        }
        if let Some(i) = chunk[..n].iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&chunk[..i]);
            return Some(&buf[..]);
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_HEADER {
            return None;
        }
    }
}

/// `.jsonl` da raiz do agente que o processo tem aberto, sem repeticao.
fn open_transcripts(proc: &ProcRoot, pid: u32, root: &Path, f: Format) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(proc.pid_path(pid, "fd")) else {
        return Vec::new();
    };
    let canon = std::fs::canonicalize(root).ok();
    let mut out: Vec<PathBuf> = Vec::new();
    for ent in rd.flatten() {
        let Ok(target) = std::fs::read_link(ent.path()) else {
            continue;
        };
        let under =
            target.starts_with(root) || canon.as_ref().is_some_and(|c| target.starts_with(c));
        let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ok_name = name.ends_with(".jsonl")
            && match f {
                Format::Codex => name.starts_with("rollout-"),
                _ => true,
            };
        if under && ok_name && !out.contains(&target) {
            out.push(target);
        }
    }
    out
}

/// Um candidato: ele. Varios: o que comecou mais perto do processo, dentro de
/// [`START_WINDOW_MS`] e sem empate. Senao, nenhum.
fn choose(mut found: Vec<(PathBuf, Option<u64>)>, start: u64) -> Option<PathBuf> {
    if found.len() == 1 {
        return found.pop().map(|(p, _)| p);
    }
    let mut near: Vec<(u64, PathBuf)> = found
        .into_iter()
        .filter_map(|(p, s)| Some((s?.abs_diff(start), p)))
        .filter(|(d, _)| *d <= START_WINDOW_MS)
        .collect();
    near.sort();
    match near.as_slice() {
        [(_, p)] => Some(p.clone()),
        [(a, p), (b, _), ..] if a < b => Some(p.clone()),
        _ => None,
    }
}

/// `sessionId` aceito como nome de arquivo: so `[A-Za-z0-9_-]`, sem `..`.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Overlay for Transcripts {
    fn id(&self) -> &str {
        "transcript"
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        let declared = self.enrich_transcript(session, overlay);
        let model = overlay.model.as_deref();
        if let Some(p) = cockpit::provider(&session.agent, declared.as_deref(), model) {
            overlay.provider = Some(p);
        }
    }

    fn configure(&self, config: &SamplerConfig) {
        let bytes = config.transcript_read_budget;
        // O comando de um tiro le tudo, subagentes inclusive: senao
        // `subagents_live` sai `None` enquanto sobra arquivo sem leitura.
        let sub = if bytes == READ_ALL {
            READ_ALL
        } else {
            subagents::READ_BUDGET
        };
        self.read_budget.store(bytes, Ordering::Relaxed);
        self.sub_read_budget.store(sub, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.budget = bytes;
        inner.subagents.budget = sub;
    }
}

impl Transcripts {
    /// O transcript da sessao, se casar; devolve o provider que ele declara.
    fn enrich_transcript(&self, session: &Session, overlay: &mut SessionOverlay) -> Option<String> {
        let f = Format::of(&session.agent)?;
        let root = self.roots.get(f)?;
        let sid = if f == Format::Claude {
            match overlay.agent_session_id.as_deref() {
                Some(id) if valid_session_id(id) => Some(id),
                _ => return None,
            }
        } else {
            None
        };
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // Overlays rodam sessao por sessao: pid repetido e o tick seguinte.
        if inner.tick_pids.contains(&session.pid) || inner.tick_pids.len() >= CACHE_CAP {
            inner.tick_pids.clear();
            inner.budget = self.read_budget.load(Ordering::Relaxed);
            inner.subagents.budget = self.sub_read_budget.load(Ordering::Relaxed);
        }
        inner.tick_pids.push(session.pid);
        let path = self.link(&mut inner, session, f, root, sid, now)?;
        let wall = SystemTime::now();
        let (every, c) = (self.refresh_every, &self.sub_counters);
        let (live, total) = inner.subagents.counts(f, root, &path, every, now, wall, c);
        if total.is_some() {
            overlay.subagents_live = live;
            overlay.subagents_total = total;
        }
        let meta = self.refresh(&mut inner, &path, f, session.cwd.as_deref(), now)?;
        meta.fill(overlay);
        meta.provider.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agentcenter_handoff_cache_is_session_scoped_and_fixture_roots_are_isolated() {
        let root = std::env::temp_dir().join(format!("ac-ho-cache-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("s61-random-op.md"), "HANDOFF_CONTENT_CANARY").unwrap();
        let mut h = Handoffs::default();
        let now = Instant::now();
        let wall = SystemTime::now();
        assert!(h.agentcenter(&root, "a", now, wall).is_some());
        assert!(h.agentcenter(&root, "b", now, wall).is_none());
        let reads = h.index.readdir_count();
        std::fs::remove_file(root.join("s61-random-op.md")).unwrap();
        assert!(
            h.agentcenter(&root, "a", now + Duration::from_secs(1), wall)
                .is_some()
        );
        assert_eq!(h.index.readdir_count(), reads);
        assert!(
            h.agentcenter(&root, "a", now + REFRESH_EVERY, wall)
                .is_none()
        );
        let tr = Transcripts::new(Roots::default(), ProcRoot::default());
        assert!(tr.home.is_none() && tr.agentcenter_handoffs.is_none());
        let tr = tr.with_home(&root);
        assert_eq!(
            tr.agentcenter_handoffs,
            Some(root.join(".local/state/agentcenter/handoffs"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn iso_timestamps_parse_to_epoch_ms() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(
            parse_iso_ms("2026-10-17T02:53:28.810Z"),
            Some(1_792_205_608_810)
        );
        assert_eq!(
            parse_iso_ms("2026-10-16T23:53:28.810-03:00"),
            Some(1_792_205_608_810)
        );
        for bad in [
            "",
            "2026-10-17",
            "2026-13-01T00:00:00Z",
            "2026-10-17T02:53:28",
            "x",
        ] {
            assert_eq!(parse_iso_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn civil_round_trip() {
        for z in [-1, 0, 1, 19_000, 20_743, 100_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(civil_from_days(20_743), (2026, 10, 17));
    }

    #[test]
    fn window_follows_contexto_py() {
        let mut u = Usage {
            context_max: 150_000,
            ..Usage::default()
        };
        assert_eq!(u.window(Format::Pi), 200_000);
        assert_eq!(u.window(Format::Claude), CLAUDE_MIN_WINDOW);
        u.window = Some(258_400);
        assert_eq!(u.window(Format::Codex), 258_400);
        u.context_max = 300_000; // passou da declarada: inferida
        assert_eq!(u.window(Format::Codex), 400_000);
        u.context_max = 1_500_000;
        assert_eq!(u.window(Format::Claude), 2_000_000);
        u.context_max = 3_000_000;
        assert_eq!(u.window(Format::Pi), 2_000_000);
    }

    #[test]
    fn round1_matches_python_round() {
        assert_eq!(round1(12.25), 12.2); // empate exato: par, como o Python
        assert_eq!(round1(0.35), 0.3); // 0.35 e 0.34999...
        assert_eq!(round1(7.06), 7.1);
        assert_eq!(round1(0.0), 0.0);
    }

    #[test]
    fn session_id_must_be_a_plain_name() {
        assert!(valid_session_id("83b69552-35fd-4b00-9fb7-366dd8e24799"));
        for bad in ["", "../x", "a/b", "a.b", "a b", &"x".repeat(129)] {
            assert!(!valid_session_id(bad), "{bad}");
        }
    }

    #[test]
    fn tail_reads_back_to_the_last_usage_line_and_then_only_new_bytes() {
        let t = crate::fixture::TempDir::new("ac-transcript-tail-unit").unwrap();
        let path = t.path().join("t.jsonl");
        let usage = |rid: &str, ctx: u64| {
            format!(
                "{{\"type\":\"assistant\",\"requestId\":\"{rid}\",\"message\":{{\"model\":\"m-{rid}\",\
                 \"usage\":{{\"input_tokens\":{ctx},\"output_tokens\":1}}}}}}\n"
            )
        };
        let other = "{\"type\":\"user\"}\n";
        // A linha mais velha e a de uso: so e lida porque a regiao comeca nela.
        let mut text = usage("r1", 100_000) + other + other;
        let complete = text.len() as u64;
        text.push_str("{\"type\":\"assistant\""); // ainda sendo escrita
        std::fs::write(&path, &text).unwrap();
        let counters = [&AtomicU64::new(0), &AtomicU64::new(0)];
        let mut tail = Usage::default();
        let mut file = File::open(&path).unwrap();
        let end = read_tail(
            &mut file,
            0,
            text.len() as u64,
            Format::Claude,
            &mut tail,
            counters,
        );
        assert_eq!(
            end.unwrap(),
            complete,
            "a linha incompleta fica para depois"
        );
        assert_eq!(tail.context_pct(Format::Claude), 10.0);
        assert_eq!(tail.model.as_deref(), Some("m-r1"));

        // Cresceu: so `[end, len)` e lido, e o mais novo vale.
        text.truncate(complete as usize);
        text.push_str(&(usage("r2", 300_000) + other));
        std::fs::write(&path, &text).unwrap();
        let before = counters[1].load(Ordering::Relaxed);
        let mut file = File::open(&path).unwrap();
        let len = text.len() as u64;
        let end = read_tail(
            &mut file,
            complete,
            len,
            Format::Claude,
            &mut tail,
            counters,
        );
        assert_eq!(end.unwrap(), len);
        assert_eq!(counters[1].load(Ordering::Relaxed) - before, len - complete);
        assert_eq!(tail.context_pct(Format::Claude), 30.0);
        assert_eq!(tail.model.as_deref(), Some("m-r2"));

        // Sem linha de uso nos bytes novos: fica o que ja tinha.
        std::fs::write(&path, text.clone() + other).unwrap();
        let mut file = File::open(&path).unwrap();
        let end = read_tail(
            &mut file,
            len,
            len + other.len() as u64,
            Format::Claude,
            &mut tail,
            counters,
        );
        assert_eq!(end.unwrap(), len + other.len() as u64);
        assert_eq!((tail.context, tail.requests), (300_000, 1));
    }

    #[test]
    fn choose_prefers_unique_then_nearest_start() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(choose(vec![(p("a"), None)], 0), Some(p("a")));
        assert_eq!(choose(vec![], 0), None);
        let two = vec![(p("a"), Some(10_000)), (p("b"), Some(200_000))];
        assert_eq!(choose(two, 5_000), Some(p("a")));
        let tie = vec![(p("a"), Some(10_000)), (p("b"), Some(10_000))];
        assert_eq!(choose(tie, 5_000), None);
        let far = vec![(p("a"), Some(500_000)), (p("b"), Some(900_000))];
        assert_eq!(choose(far, 0), None);
    }
}
