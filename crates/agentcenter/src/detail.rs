//! Detalhe da sessao selecionada (C11): a pagina de conteudo do
//! `AdwNavigationSplitView`. Ao lado da lista a partir de 840 px; abaixo,
//! empurrada por cima dela, com o proprio cabecalho e o botao de voltar
//! (Esc, Alt+Esquerda e o botao voltar do mouse sao do AdwNavigationView).
//!
//! ```text
//! [icone] nome da sessao (elipse no fim)                  [Busy] [HANDOFF]
//!         claude · interactive        ~/src/kiln (elipse no inicio)   herdr w4:p3
//!         Processes 11  CPU 23%  Memory 704 MiB + 582 MiB swap  Disk read … · write …  Stall 1%
//! Transcript
//!         Model claude-opus-5-5 (Anthropic)  Context 86% · 620k of 1.0M  Tokens in … · out …
//!         Tool calls 40 total · 2 in flight · 3.1/min  Subagents 1 live · 4 total
//! Processes · 11
//!         tabela (process_table.rs)
//! ```
//!
//! Section order (REVIEW_RR1): title and verdict pill with its reason line,
//! Handoff and Compact, where the session runs (agent, model, terminal,
//! folder), the transcript's context and token panel, then the resource
//! figures, charts and processes. The "Session details" fold keeps only IDs.
//! Suspend, Resume, Terminate and Kill live in the title row's menu alone.
//! Collapsed, the page header repeats Handoff and Compact beside Back.
//!
//! A recomendacao do cockpit e so a pilula ao lado do estado (decisao do
//! dono, docs/DESIGN.md secao 8); o texto dela vem da mesma [`Row`] da lista.
//! Valor ausente e travessao, nunca 0. Nenhum widget nasce por tick: o
//! detalhe troca rotulos (so os que mudaram) e sincroniza a tabela.

use crate::actions_ui;
use crate::format::{DASH, Group, Row, bytes, human, provider_name, rec_class, sanitize};
use crate::graph::{Strip, nice_max, peak, runs_at_least};
use crate::process_table::ProcessBars;
use crate::state_glyph::StateGlyph;
use crate::timeline::{DETAIL_SIZE, Scale, Timeline, scale_max, stacked};
use adw::prelude::*;
use agentcenter_core::actions::Target;
use agentcenter_core::cockpit::{LOOP_REPEATS, Thresholds};
use agentcenter_core::control::herdr::{Endpoint, Target as FocusTarget};
use agentcenter_core::{HISTORY_LEN, History, IoStallSource, Session, SessionHistory, Snapshot};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// Tag da pagina de conteudo no split view (`navigation.push`).
pub const PAGE_TAG: &str = "detail";

const STACK_NONE: &str = "none";
const STACK_ENDED: &str = "ended";
const STACK_SESSION: &str = "session";

/// Um fato do cabecalho: legenda, valor e tooltip (vazio sem tooltip).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Fact {
    pub value: String,
    pub tooltip: String,
}

impl Fact {
    fn new(value: impl Into<String>, tooltip: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            tooltip: tooltip.into(),
        }
    }
}

/// Legendas dos fatos, na ordem de [`DetailText::session`].
pub const SESSION_FACTS: [&str; 5] = ["Processes", "CPU", "Memory", "Disk", "Stall"];
/// Legendas dos fatos do transcript, na ordem de [`DetailText::transcript`].
pub const TRANSCRIPT_FACTS: [&str; 5] = ["Model", "Context", "Tokens", "Tool calls", "Subagents"];
/// Captions of where the session runs, in the order of [`DetailText::runs`].
pub const RUNS_FACTS: [&str; 4] = ["Agent", "Model", "Terminal", "Folder"];

/// Todo o texto do detalhe de uma sessao, sem GTK: testavel sem display.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DetailText {
    pub pid: u32,
    pub name: String,
    pub group: u32,
    /// Agent name alone (`claude`, `herdr`), for the header's meta line.
    pub agent: String,
    /// Model the transcript names; empty until it does.
    pub model: String,
    /// Texto da pilula de estado; vazio no multiplexador.
    pub status: String,
    pub rec: u32,
    pub rec_text: String,
    pub rec_tooltip: String,
    /// `agente · tipo` (`claude · interactive`, `herdr · multiplexer`).
    pub identity: String,
    pub cwd: String,
    /// `herdr w4:p3`; vazio fora do herdr.
    pub herdr: String,
    pub herdr_tooltip: String,
    /// Id que o proprio agente da a sessao (uuid do Claude Code, id do
    /// arquivo de sessao do pi, id do Codex); travessao sem ele.
    pub session_id: Fact,
    /// O id falado com a legenda: um uuid solto nao diz nada ao leitor.
    pub session_id_a11y: String,
    pub session: [Fact; 5],
    /// Where the session runs: agent, model, terminal, folder.
    pub runs: [Fact; 4],
    /// Vazio no multiplexador (nao tem transcript).
    pub transcript: Option<[Fact; 5]>,
    pub a11y: String,
}

/// Header glyph box, px.
const GLYPH_PX: i32 = 16;

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(w) = child {
        out.extend(descendants(&w));
        child = w.next_sibling();
    }
    out
}

fn n_or_dash(v: Option<u64>) -> String {
    v.map_or_else(|| DASH.to_string(), |n| n.to_string())
}

fn rate(v: Option<f64>) -> String {
    v.map_or_else(|| DASH.to_string(), |b| format!("{}/s", bytes(b)))
}

impl DetailText {
    /// The header's one meta line: agent, model, pid, cwd, herdr place,
    /// process count and context. What is not known yet is left out: a dash
    /// in a sentence reads as a value.
    pub fn meta_line(&self) -> String {
        let known = |v: &str| !v.is_empty() && v != DASH;
        let context = self
            .transcript
            .as_ref()
            .map(|t| t[1].value.split(" \u{b7} ").next().unwrap_or_default())
            .filter(|c| known(c))
            .map(|c| format!("context {c}"));
        let procs = &self.session[0].value;
        [
            Some(self.agent.clone()),
            Some(self.model.clone()),
            Some(format!("pid {}", self.pid)),
            Some(self.cwd.clone()),
            Some(self.herdr.clone()),
            Some(format!(
                "{procs} {}",
                if procs == "1" { "process" } else { "processes" }
            )),
            context,
        ]
        .into_iter()
        .flatten()
        .filter(|p| known(p))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
    }

    pub fn of(s: &Session, source: IoStallSource, home: Option<&str>) -> Self {
        let row = Row::from_session(s, source, home);
        let ov = s.overlay.as_ref();
        let mux = Group::from_rank(row.group) == Group::Multiplexer;
        let agent = sanitize(&s.agent).into_owned();
        let kind = ov
            .and_then(|o| o.kind.as_deref())
            .map(|k| sanitize(k).into_owned())
            .filter(|k| !k.trim().is_empty());
        let identity = match (mux, kind) {
            (true, _) => format!("{agent} · multiplexer"),
            (false, Some(k)) => format!("{agent} · {k}"),
            (false, None) => agent.clone(),
        };

        let herdr = ov.and_then(|o| o.herdr.as_ref());
        let (herdr_text, herdr_tooltip) = match herdr {
            None => (String::new(), String::new()),
            Some(h) => {
                let f = |v: Option<&str>| {
                    v.map_or_else(|| DASH.to_string(), |t| sanitize(t).into_owned())
                };
                (
                    if row.place.is_empty() {
                        "herdr".to_string()
                    } else {
                        format!("herdr {}", row.place)
                    },
                    format!(
                        "herdr: workspace {}, tab {}, pane {}, status {}",
                        f(h.workspace_id.as_deref()),
                        f(h.tab_id.as_deref()),
                        f(h.pane_id.as_deref()),
                        f(h.status.as_deref()),
                    ),
                )
            }
        };

        let session_id = ov
            .and_then(|o| o.agent_session_id.as_deref())
            .map(|i| sanitize(i).into_owned())
            .filter(|i| !i.trim().is_empty());
        let (session_id, session_id_a11y) = match session_id {
            Some(id) => (
                Fact::new(id.clone(), format!("Session id reported by {agent}")),
                format!("Session id {id}"),
            ),
            None => (
                Fact::new(DASH, "The agent has not reported a session id yet"),
                "Session id unavailable".to_string(),
            ),
        };

        let procs = s.procs.to_string();
        let stall = match (source, s.stall_pct, s.threads_d) {
            (IoStallSource::Delayacct, Some(p), _) => Fact::new(
                format!("{p:.0}%"),
                "Longest wait on block I/O among the session's processes, % of the interval",
            ),
            (IoStallSource::DState, _, Some(d)) => Fact::new(
                format!("D:{d}"),
                "Threads waiting on disk (delay accounting is off: estimated)",
            ),
            _ => Fact::new(DASH, "No I/O stall reading"),
        };
        let session = [
            Fact::new(procs, "The agent process and its descendants"),
            Fact::new(row.cpu_text.clone(), row.cpu_tooltip.clone()),
            Fact::new(
                row.mem_text.clone(),
                "PSS (and swap PSS): shared pages split between processes, so it adds up",
            ),
            Fact::new(
                format!("read {} · write {}", rate(s.read_bps), rate(s.write_bps)),
                "Block I/O of the session, summed over its processes",
            ),
            stall,
        ];

        let model_name = ov
            .and_then(|o| o.model.as_deref())
            .map(|m| sanitize(m).into_owned())
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_default();
        let transcript = (!mux).then(|| {
            let model = ov
                .and_then(|o| o.model.as_deref())
                .map(|m| sanitize(m).into_owned())
                .filter(|m| !m.trim().is_empty());
            let provider = ov
                .and_then(|o| o.provider.as_deref())
                .map(provider_name)
                .or_else(|| {
                    agentcenter_core::cockpit::provider(&s.agent, None, model.as_deref())
                        .map(|p| provider_name(&p))
                });
            let model = match (model, provider) {
                (Some(m), Some(p)) => Fact::new(format!("{m} ({p})"), ""),
                (Some(m), None) => Fact::new(m, ""),
                (None, _) => Fact::new(DASH, "The transcript has not named a model yet"),
            };
            let cockpit = ov.and_then(|o| o.cockpit.as_ref());
            let context = match (ov.and_then(|o| o.context_pct), cockpit) {
                (None, _) => Fact::new(DASH, "No context reading for this session yet"),
                (Some(c), Some(k)) if k.window > 0 => Fact::new(
                    format!("{c:.0}% · {} of {}", human(k.ctx_tokens), human(k.window)),
                    if k.window_inferred {
                        "Context window used; the window size is inferred from the model"
                    } else {
                        "Context window used"
                    },
                ),
                (Some(c), _) => Fact::new(format!("{c:.0}%"), "Context window used"),
            };
            let (tin, tout) = (ov.and_then(|o| o.tokens_in), ov.and_then(|o| o.tokens_out));
            let tokens = match (tin, tout) {
                (None, None) => Fact::new(DASH, "Token counts come after the transcript is read"),
                (i, o) => Fact::new(
                    format!(
                        "in {} · out {}",
                        i.map_or_else(|| DASH.to_string(), human),
                        o.map_or_else(|| DASH.to_string(), human)
                    ),
                    "Tokens in (with cache) and out, whole transcript",
                ),
            };
            let (total, open, per_min) = (
                ov.and_then(|o| o.tool_calls),
                ov.and_then(|o| o.tool_calls_open),
                ov.and_then(|o| o.tool_calls_per_min),
            );
            let tools = match (total, open, per_min) {
                (None, None, None) => Fact::new(DASH, row.tools_tooltip.clone()),
                (t, o, m) => Fact::new(
                    format!(
                        "{} total · {} in flight · {}/min",
                        n_or_dash(t),
                        n_or_dash(o),
                        m.map_or_else(|| DASH.to_string(), |v| format!("{v:.1}"))
                    ),
                    "Per minute over the last 5 minutes",
                ),
            };
            let subs = match (
                ov.and_then(|o| o.subagents_live),
                ov.and_then(|o| o.subagents_total),
            ) {
                (None, None) => Fact::new(DASH, row.sub_tooltip.clone()),
                (l, t) => Fact::new(
                    format!("{} live · {} total", n_or_dash(l), n_or_dash(t)),
                    "",
                ),
            };
            [model, context, tokens, tools, subs]
        });

        let runs = [
            Fact::new(agent.clone(), "The agent this session runs"),
            Fact::new(
                if model_name.is_empty() {
                    DASH.to_string()
                } else {
                    model_name.clone()
                },
                "The model the transcript names",
            ),
            if mux {
                Fact::new("the multiplexer itself", "This is the herdr server")
            } else if herdr_text.is_empty() {
                Fact::new(
                    "terminal unknown",
                    "No herdr pane was matched; tmux and IDE terminals are not sampled",
                )
            } else {
                Fact::new(herdr_text.clone(), herdr_tooltip.clone())
            },
            Fact::new(
                if row.cwd.is_empty() {
                    DASH.to_string()
                } else {
                    row.cwd.clone()
                },
                "Working directory of the session",
            ),
        ];
        let a11y = format!(
            "{}, {identity}, {}",
            row.name,
            if mux {
                "multiplexer"
            } else {
                row.status.as_str()
            }
        );
        Self {
            pid: s.pid,
            name: row.name,
            group: row.group,
            agent: agent.clone(),
            model: model_name,
            status: row.status,
            rec: row.rec,
            rec_text: row.rec_text,
            rec_tooltip: row.rec_tooltip,
            identity,
            cwd: row.cwd,
            herdr: herdr_text,
            herdr_tooltip,
            session_id,
            session_id_a11y,
            session,
            runs,
            transcript,
            a11y,
        }
    }
}

/// Titles of the four kernel charts, in the order of [`ChartData::of`].
pub const CHART_TITLES: [&str; 4] = ["CPU", "Memory", "Disk", "I/O stall"];
/// Limite do stall com delay accounting, o mesmo 10 % da pressao (DESIGN.md
/// secao 4): acima dele a leitura fica carmim e o filete marca o limite.
pub const STALL_LIMIT_PCT: f64 = 10.0;
/// Floor of the CPU scale: a quiet session does not fill the chart.
const CPU_FLOOR_PCT: f64 = 10.0;
/// Floor of the stall scale: twice the limit, so its line sits mid-chart.
const STALL_FLOOR_PCT: f64 = 2.0 * STALL_LIMIT_PCT;
const KIB: f64 = 1024.0;
const MIB: f64 = 1024.0 * KIB;
const GIB: f64 = 1024.0 * MIB;
/// Smallest width of a detail chart: two fit side by side in the detail at
/// 1280 (about 360 px each, `timeline::DETAIL_SIZE`), one per line at 360.
const CHART_MIN_WIDTH: i32 = 240;
/// Detail width under which it tightens (sp).
const NARROW_MAX_SP: f64 = 480.0;

/// CSS class of a state fill (tokens.css `--ac-mark-fill-*`).
fn tone(g: Group) -> &'static str {
    match g {
        Group::Busy => "busy",
        Group::Blocked => "waiting",
        Group::Idle => "idle",
        Group::Multiplexer => "mux",
        Group::Unknown => "unknown",
    }
}

/// Legendas das faixas do transcript (CC9), na ordem de [`StripData::cockpit`].
pub const COCKPIT_STRIPS: [&str; 3] = ["Context", "Burn", "Tool calls/min"];
/// Series por faixa do transcript: contexto = contexto + releitura (tracejada).
const COCKPIT_SERIES: [usize; 3] = [2, 1, 1];
/// Piso da escala de tool calls/min: um ritmo calmo nao enche a faixa.
const TOOL_CALLS_FLOOR: f64 = 10.0;

/// Uma faixa de historico pronta para desenhar, sem GTK.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StripData {
    pub series: Vec<Vec<f32>>,
    /// Topo da escala.
    pub max: f64,
    pub cap: usize,
    /// Limites na escala (stall com delay accounting; contexto 70/90; burn).
    pub limits: Vec<f64>,
    /// Trechos marcados no rodape (indices de amostra): o sinal de loop.
    pub bands: Vec<Range<usize>>,
    /// A ultima leitura passou do limite (ou esta em loop).
    pub alert: bool,
    /// `max 140%`, `LOOP ×5`, ou travessao sem leitura.
    pub readout: String,
    pub tooltip: String,
    /// Rotulo do leitor de tela: legenda, janela, maximo, agora e escala.
    pub a11y: String,
}

fn last(v: &[f32]) -> Option<f64> {
    v.last().filter(|x| x.is_finite()).map(|x| f64::from(*x))
}

/// Seconds the ring covers at a tick of `interval_ms` (0 is the default 1 s).
fn span_secs(interval_ms: u64) -> u64 {
    let ms = if interval_ms == 0 { 1000 } else { interval_ms };
    HISTORY_LEN as u64 * ms / 1000
}

/// `Last 10 minutes`: o ring guarda [`HISTORY_LEN`] ticks do intervalo dado.
pub fn span_text(interval_ms: u64) -> String {
    let secs = span_secs(interval_ms);
    if secs >= 120 {
        format!("Last {} minutes", (secs + 30) / 60)
    } else {
        format!("Last {secs} seconds")
    }
}

/// Faixa sem historico: trilho vazio e travessao, nunca 0.
fn no_history(caption: &str, span: &str) -> StripData {
    StripData {
        readout: DASH.to_string(),
        tooltip: "No history for this session yet".to_string(),
        a11y: format!("{caption}, {span}: no history yet"),
        max: 1.0,
        cap: HISTORY_LEN,
        ..Default::default()
    }
}

/// Escala e texto de uma faixa: topo "redondo" que cobre o pico (nunca abaixo
/// de `floor`), `max` e `now` de cada serie, e a leitura (`max` do pico).
fn make_strip(
    caption: &str,
    span: &str,
    cap: usize,
    series: Vec<Vec<f32>>,
    (floor, binary): (f64, bool),
    fmt: &dyn Fn(f64) -> String,
    names: &[&str],
) -> StripData {
    let all = peak(series.iter().map(Vec::as_slice));
    let max = nice_max(all.unwrap_or(f64::NAN), floor, binary);
    let peaks: Vec<String> = series
        .iter()
        .zip(names)
        .map(|(s, n)| {
            let p = peak([s.as_slice()]).map_or_else(|| DASH.to_string(), fmt);
            let now = last(s).map_or_else(|| DASH.to_string(), fmt);
            format!("{n}: max {p}, now {now}")
        })
        .collect();
    let detail = peaks.join("; ");
    StripData {
        readout: all.map_or_else(|| DASH.to_string(), |p| format!("max {}", fmt(p))),
        tooltip: format!("{detail}. Scale 0 to {}", fmt(max)),
        a11y: format!("{caption}, {span}: {detail}; scale 0 to {}", fmt(max)),
        series,
        max,
        cap,
        limits: Vec::new(),
        bands: Vec::new(),
        alert: false,
    }
}

/// Acrescenta os limites ao texto (tooltip e leitor de tela), na ordem dada.
fn with_marks(mut d: StripData, marks: &[(&str, f64)], fmt: &dyn Fn(f64) -> String) -> StripData {
    if marks.is_empty() {
        return d;
    }
    let said: Vec<String> = marks
        .iter()
        .map(|(n, v)| format!("{n} at {}", fmt(*v)))
        .collect();
    let said = said.join(", ");
    d.tooltip = format!("{}. Marks: {said}", d.tooltip);
    d.a11y = format!("{}; marks: {said}", d.a11y);
    d.limits = marks.iter().map(|(_, v)| *v).collect();
    d
}

/// Unit for byte quantities, picked by the peak: `(name, divisor,
/// decimals)`. GiB from 1 GiB up, MiB below.
fn mem_unit(peak: Option<f64>) -> (&'static str, f64, usize) {
    if peak.is_some_and(|p| p >= GIB) {
        ("GiB", GIB, 2)
    } else {
        ("MiB", MIB, 0)
    }
}

/// Unit for byte rates, picked by the peak: MiB/s from 1 MiB/s up, KiB/s
/// below.
fn rate_unit(peak: Option<f64>) -> (&'static str, f64, usize) {
    if peak.is_some_and(|p| p >= MIB) {
        ("MiB/s", MIB, 1)
    } else {
        ("KiB/s", KIB, 0)
    }
}

/// The series in the chart's unit; a missing sample stays `NaN`.
fn in_unit(v: &[f32], div: f64) -> Vec<f32> {
    v.iter().map(|x| (f64::from(*x) / div) as f32).collect()
}

fn num(v: Option<f64>, decimals: usize) -> String {
    v.map_or_else(|| DASH.to_string(), |v| format!("{v:.decimals$}"))
}

fn mean(v: &[f32]) -> Option<f64> {
    let (sum, n) = v
        .iter()
        .filter(|x| x.is_finite())
        .fold((0.0, 0u32), |(s, n), x| (s + f64::from(*x), n + 1));
    (n > 0).then(|| sum / f64::from(n))
}

/// One kernel chart of the detail ready to draw, without GTK: the series in
/// the unit shown, the scale, the threshold, and the texts of the card.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartData {
    pub title: &'static str,
    /// Unit next to the title (`% of one core`, `GiB`, `MiB/s`, `% of time`).
    pub unit: String,
    /// Solid series, in `unit`.
    pub primary: Vec<f32>,
    /// Dashed series (disk write).
    pub second: Option<Vec<f32>>,
    /// Hatched band stacked on `primary` (swapped-out memory).
    pub band: Option<Vec<f32>>,
    pub cap: usize,
    pub scale: Scale,
    /// Dashed limit line (stall with delay accounting).
    pub threshold: Option<f64>,
    /// The newest reading is over the threshold.
    pub alert: bool,
    /// Big number on the right: the newest reading (`19 %`, `1.10 GiB`,
    /// `2.4 / 0.5`); a dash without one, never 0.
    pub value: String,
    /// Line under the title (`max 36 % · mean 23 %`, what solid and hatched
    /// mean).
    pub sub: String,
    pub tooltip: String,
    /// Screen reader label: title, window, now, the line under and the scale.
    pub a11y: String,
}

impl ChartData {
    /// The four kernel charts of a session: CPU, memory (PSS with swap
    /// hatched on top), disk (read solid, write dashed) and I/O stall. With
    /// no history, empty tracks and dashes.
    pub fn of(h: Option<&SessionHistory>, source: IoStallSource, span: &str) -> [ChartData; 4] {
        let span = span.to_lowercase();
        let blank = |title: &'static str, unit: &str, scale: Scale| ChartData {
            title,
            unit: unit.to_string(),
            primary: Vec::new(),
            second: None,
            band: None,
            cap: HISTORY_LEN,
            scale,
            threshold: None,
            alert: false,
            value: DASH.to_string(),
            sub: String::new(),
            tooltip: String::new(),
            a11y: String::new(),
        };
        let nice = |floor: f64, binary: bool| Scale::Nice { floor, binary };
        let stall_unit = match source {
            IoStallSource::Delayacct => "% of time",
            IoStallSource::DState => "threads in D",
        };
        let Some(h) = h.filter(|h| !h.is_empty()) else {
            return [
                blank("CPU", "% of one core", nice(CPU_FLOOR_PCT, false)),
                blank("Memory", "MiB", nice(1.0, true)),
                blank("Disk", "KiB/s", nice(1.0, true)),
                blank("I/O stall", stall_unit, nice(STALL_FLOOR_PCT, false)),
            ]
            .map(|mut d| {
                d.sub = "No history for this session yet".to_string();
                d.tooltip = d.sub.clone();
                d.a11y = format!("{}, {span}: no history yet", d.title);
                d
            });
        };
        let cap = h.cpu_pct.capacity();
        let finish = |mut d: ChartData| {
            d.cap = cap;
            let top = d.top();
            let scale = format!("scale 0 to {} {}", crate::timeline::tick_label(top), d.unit);
            d.tooltip = format!("{}. {}", d.sub, capitalize(&scale));
            d.a11y = format!("{}, {span}: now {}; {}; {scale}", d.title, d.value, d.sub);
            d
        };

        let cpu = h.cpu_pct.to_vec();
        let pct = |v: Option<f64>| v.map_or_else(|| DASH.to_string(), |v| format!("{v:.0} %"));
        let cpu = ChartData {
            value: pct(last(&cpu)),
            sub: format!(
                "max {} \u{b7} mean {}",
                pct(peak([cpu.as_slice()])),
                pct(mean(&cpu))
            ),
            primary: cpu,
            ..blank("CPU", "% of one core", nice(CPU_FLOOR_PCT, false))
        };

        let (pss, swap) = (h.pss_bytes.to_vec(), h.swap_pss_bytes.to_vec());
        let (unit, div, dp) = mem_unit(peak([stacked(&pss, &swap).as_slice(), &pss]));
        let (pss, swap) = (in_unit(&pss, div), in_unit(&swap, div));
        let (ram, out) = (last(&pss), last(&swap));
        let total = ram.map(|r| r + out.unwrap_or(0.0));
        let mem = ChartData {
            value: total.map_or_else(|| DASH.to_string(), |t| format!("{t:.dp$} {unit}")),
            sub: format!(
                "solid: in RAM {} \u{b7} hatched: swapped out {}",
                num(ram, dp),
                num(out, dp)
            ),
            primary: pss,
            band: Some(swap),
            ..blank("Memory", unit, nice(1.0, true))
        };

        let (read, write) = (h.read_bps.to_vec(), h.write_bps.to_vec());
        let (unit, div, dp) = rate_unit(peak([read.as_slice(), &write]));
        let (read, write) = (in_unit(&read, div), in_unit(&write, div));
        let disk = ChartData {
            value: match (last(&read), last(&write)) {
                (None, None) => DASH.to_string(),
                (r, w) => format!("{} / {}", num(r, dp), num(w, dp)),
            },
            sub: "solid: read \u{b7} dashed: write".to_string(),
            primary: read,
            second: Some(write),
            ..blank("Disk", unit, nice(1.0, true))
        };

        let stall = h.stall.to_vec();
        let now = last(&stall);
        let stall = match source {
            IoStallSource::Delayacct => ChartData {
                value: pct(now),
                sub: "time this session waited on disk".to_string(),
                threshold: Some(STALL_LIMIT_PCT),
                alert: now.is_some_and(|n| n >= STALL_LIMIT_PCT),
                primary: stall,
                ..blank("I/O stall", stall_unit, nice(STALL_FLOOR_PCT, false))
            },
            IoStallSource::DState => ChartData {
                value: now.map_or_else(|| DASH.to_string(), |n| format!("D:{n:.0}")),
                sub: "threads waiting on disk (estimated)".to_string(),
                primary: stall,
                ..blank("I/O stall", stall_unit, nice(4.0, false))
            },
        };
        [cpu, mem, disk, stall].map(finish)
    }

    /// Top of the scale the timeline will draw this chart on.
    pub fn top(&self) -> f64 {
        let total = self
            .band
            .as_ref()
            .map(|b| stacked(&self.primary, b))
            .unwrap_or_default();
        let second = self.second.as_deref().unwrap_or(&[]);
        scale_max(self.scale, peak([self.primary.as_slice(), second, &total]))
    }
}

impl StripData {
    /// As tres faixas do transcript (CC9): contexto com releitura, burn e
    /// tool calls/min com o sinal de loop. So o que o sampler gravou do
    /// overlay; amostra sem fonte quebra a linha. Limites de `th`: piso da
    /// releitura e handoff no contexto, burn cheio no burn.
    pub fn cockpit(h: Option<&SessionHistory>, th: &Thresholds, span: &str) -> [StripData; 3] {
        let span = span.to_lowercase();
        let Some(h) = h.filter(|h| !h.is_empty()) else {
            return COCKPIT_STRIPS.map(|cap| no_history(cap, &span));
        };
        let cap = h.ctx_pct.capacity();
        let pct = |v: f64| format!("{v:.0}%");

        let ctx = h.ctx_pct.to_vec();
        let ctx_now = last(&ctx);
        let reread: Vec<f32> = h.reread_share.iter().map(|v| v * 100.0).collect();
        let handoff = f64::from(th.handoff_now);
        let floor = f64::from(th.reread_floor_pct.min(th.handoff_now));
        let mut marks = vec![("re-read floor", floor), ("handoff", handoff)];
        marks.dedup_by(|a, b| a.1 == b.1);
        let mut context = with_marks(
            make_strip(
                "Context",
                &span,
                cap,
                vec![ctx, reread],
                (100.0, false),
                &pct,
                &[
                    "context, % of the window (solid)",
                    "re-read, % of weighted burn (dashed)",
                ],
            ),
            &marks,
            &pct,
        );
        context.alert = ctx_now.is_some_and(|n| n >= handoff);
        // A leitura e a do contexto: a releitura tem o proprio "max" no texto.
        context.readout = peak([context.series[0].as_slice()])
            .map_or_else(|| DASH.to_string(), |p| format!("max {}", pct(p)));

        let full = f64::from(th.burn_full_pct);
        let burn_series = h.burn_pct.to_vec();
        let burn_now = last(&burn_series);
        let mut burn = with_marks(
            make_strip(
                "Burn",
                &span,
                cap,
                vec![burn_series],
                (2.0 * full, false),
                &|v: f64| format!("{v:.1}%"),
                &["burn, % of the session budget"],
            ),
            &[("full burn", full)],
            &pct,
        );
        burn.alert = burn_now.is_some_and(|n| n >= full);

        let per_min = |v: f64| format!("{v:.1}/min");
        let mut tools = make_strip(
            "Tool calls/min",
            &span,
            cap,
            vec![h.tool_calls_per_min.to_vec()],
            (TOOL_CALLS_FLOOR, false),
            &per_min,
            &["tool calls per minute, last 5 minutes"],
        );
        let loops = h.loop_repeats.to_vec();
        tools.bands = runs_at_least(&loops, LOOP_REPEATS as f32);
        let loop_now = last(&loops);
        let signal = match (loops.iter().any(|v| v.is_finite()), loop_now) {
            (false, _) => "loop signal not read in this window".to_string(),
            (true, Some(n)) if n >= f64::from(LOOP_REPEATS) => format!(
                "in a loop now: the same tool call repeated {n:.0} times; \
                 loop marked under the line in {}",
                stretches(tools.bands.len())
            ),
            (true, _) if tools.bands.is_empty() => "no loop in this window".to_string(),
            (true, _) => format!(
                "loop marked under the line in {}, not now",
                stretches(tools.bands.len())
            ),
        };
        if let Some(n) = loop_now.filter(|n| *n >= f64::from(LOOP_REPEATS)) {
            tools.readout = format!("LOOP ×{n:.0}");
            tools.alert = true;
        }
        tools.tooltip = format!("{}. {}", tools.tooltip, capitalize(&signal));
        tools.a11y = format!("{}; {signal}", tools.a11y);
        [context, burn, tools]
    }
}

fn stretches(n: usize) -> String {
    if n == 1 {
        "1 stretch".to_string()
    } else {
        format!("{n} stretches")
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

fn set_label(l: &gtk::Label, text: &str) {
    if l.label() != text {
        l.set_label(text);
    }
}

/// A feedback line that takes no room while it is empty.
fn set_feedback(l: &gtk::Label, text: &str) {
    set_label(l, text);
    l.set_visible(!text.is_empty());
}

fn set_tip(w: &impl IsA<gtk::Widget>, tip: &str) {
    let tip = (!tip.is_empty()).then_some(tip);
    if w.tooltip_text().as_deref() != tip {
        w.set_tooltip_text(tip);
    }
}

fn set_classes(w: &impl IsA<gtk::Widget>, classes: &[&str]) {
    let now = w.css_classes();
    if now.len() != classes.len() || now.iter().zip(classes).any(|(a, b)| a != b) {
        w.set_css_classes(classes);
    }
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// Fatos em linha, `Legenda valor`, que quebram como a faixa de contadores.
struct Facts {
    widget: adw::WrapBox,
    items: Vec<(gtk::Box, gtk::Label)>,
}

impl Facts {
    fn new(captions: &[&str]) -> Self {
        let widget = adw::WrapBox::builder()
            .child_spacing(20)
            .line_spacing(4)
            .build();
        widget.add_css_class("detail-facts");
        let items = captions
            .iter()
            .map(|c| {
                let cap = label(&["detail-caption"]);
                cap.set_label(c);
                let value = label(&["detail-value", "numeric"]);
                value.set_wrap(true);
                value.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                b.append(&cap);
                b.append(&value);
                widget.append(&b);
                (b, value)
            })
            .collect();
        Self { widget, items }
    }

    fn set(&self, facts: &[Fact], captions: &[&str]) {
        for (((b, value), f), cap) in self.items.iter().zip(facts).zip(captions) {
            set_label(value, &f.value);
            set_tip(b, &f.tooltip);
            let spoken = format!("{cap} {}", f.value);
            b.update_property(&[gtk::accessible::Property::Label(&spoken)]);
        }
    }
}

/// Ultimo historico recebido e a sessao dele, desenhado quando a secao
/// estiver na tela.
struct Pending {
    history: Option<Arc<History>>,
    pid: u32,
    source: IoStallSource,
    interval_ms: u64,
}

/// Uma celula de faixa: legenda e leitura em cima, faixa embaixo, dentro de um
/// filho do `GtkFlowBox` (focavel: Tab entra no painel, as setas andam pelas
/// faixas e o leitor de tela le o resumo da celula).
struct StripCell {
    child: gtk::FlowBoxChild,
    readout: gtk::Label,
    strip: Strip,
}

/// Painel de faixas: `GtkFlowBox` sem selecao, celulas da mesma largura, ate
/// `per_line` numa linha. Vao de 8 px mais os 3 px de padding de cada
/// `flowboxchild` (libadwaita) = 14 px entre faixas, e duas celulas de 150 px
/// (`.history-cell`) cabem nos 320 px de miolo do detalhe em 360. O anel de
/// foco e o do libadwaita no `flowboxchild`, com a cor do token de foco
/// (tokens.css); nada aqui e ativavel.
fn strip_panel(
    captions: &[&str],
    series: &[usize],
    per_line: u32,
) -> (gtk::FlowBox, Vec<StripCell>) {
    let panel = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .column_spacing(8)
        .row_spacing(8)
        .min_children_per_line(1)
        .max_children_per_line(per_line)
        .build();
    panel.add_css_class("detail-history");
    let cells = captions
        .iter()
        .zip(series)
        .map(|(caption, series)| {
            let cap = label(&["detail-caption"]);
            cap.set_label(caption);
            cap.set_hexpand(true);
            cap.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let readout = label(&["history-readout", "numeric"]);
            readout.set_xalign(1.0);
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            top.append(&cap);
            top.append(&readout);
            let strip = Strip::new(*series);
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
            cell.add_css_class("history-cell");
            cell.append(&top);
            cell.append(&strip);
            let child = gtk::FlowBoxChild::new();
            child.set_child(Some(&cell));
            panel.append(&child);
            StripCell {
                child,
                readout,
                strip,
            }
        })
        .collect();
    (panel, cells)
}

fn draw_cells(cells: &[StripCell], data: &[StripData]) {
    for (c, d) in cells.iter().zip(data) {
        let series: Vec<&[f32]> = d.series.iter().map(Vec::as_slice).collect();
        c.strip
            .set_marked(&series, d.max, d.cap, &d.limits, &d.bands);
        set_label(&c.readout, &d.readout);
        set_tip(&c.child, &d.tooltip);
        set_classes(
            &c.readout,
            if d.alert {
                &["history-readout", "numeric", "alert"]
            } else {
                &["history-readout", "numeric"]
            },
        );
        set_classes(&c.strip, if d.alert { &["alert"] } else { &[] });
        c.strip
            .update_property(&[gtk::accessible::Property::Label(&d.a11y)]);
        c.child
            .update_property(&[gtk::accessible::Property::Label(&d.a11y)]);
    }
}

/// One kernel chart card: title, unit and the newest reading on top, the
/// line under it, and a [`Timeline`] with its axis.
struct ChartCell {
    child: gtk::FlowBoxChild,
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests read the title"))]
    title: gtk::Label,
    unit: gtk::Label,
    value: gtk::Label,
    sub: gtk::Label,
    timeline: Timeline,
}

/// The four charts, two per line where they fit, one per line in a narrow
/// detail. Same focus behaviour as the strip panel.
fn chart_panel() -> (gtk::FlowBox, Vec<ChartCell>) {
    let panel = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .column_spacing(8)
        .row_spacing(8)
        .min_children_per_line(1)
        .max_children_per_line(2)
        .build();
    panel.add_css_class("detail-charts");
    let charts = CHART_TITLES
        .iter()
        .map(|t| {
            let title = label(&["chart-title"]);
            title.set_label(t);
            let unit = label(&["chart-unit"]);
            unit.set_hexpand(true);
            unit.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let value = label(&["chart-value", "numeric"]);
            value.set_xalign(1.0);
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            top.set_valign(gtk::Align::Baseline);
            top.append(&title);
            top.append(&unit);
            top.append(&value);
            let sub = label(&["chart-sub"]);
            sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let timeline = Timeline::new(DETAIL_SIZE.0, DETAIL_SIZE.1);
            timeline.set_size_request(CHART_MIN_WIDTH, DETAIL_SIZE.1);
            timeline.set_hexpand(true);
            timeline.set_y_labels(true);
            timeline.set_margin_top(8);
            let card = gtk::Box::new(gtk::Orientation::Vertical, 2);
            card.add_css_class("chart-card");
            card.append(&top);
            card.append(&sub);
            card.append(&timeline);
            let child = gtk::FlowBoxChild::new();
            child.set_child(Some(&card));
            panel.append(&child);
            ChartCell {
                child,
                title,
                unit,
                value,
                sub,
                timeline,
            }
        })
        .collect();
    (panel, charts)
}

fn draw_charts(cells: &[ChartCell], data: &[ChartData], span_secs: u64) {
    for (c, d) in cells.iter().zip(data) {
        set_label(&c.unit, &d.unit);
        set_label(&c.value, &d.value);
        set_label(&c.sub, &d.sub);
        set_classes(
            &c.value,
            if d.alert {
                &["chart-value", "numeric", "alert"]
            } else {
                &["chart-value", "numeric"]
            },
        );
        let t = &c.timeline;
        // The labels are born with the 10-minute text and hidden: compare the
        // visibility too, not the text alone.
        if !t.axis_visible()
            || t.labels().1.first().map(String::as_str)
                != Some(&crate::timeline::axis_labels(span_secs)[0])
        {
            t.set_axis(Some(span_secs));
        }
        t.set_scale(d.scale);
        t.set_threshold(d.threshold);
        t.set_alert(d.alert);
        t.set_values(&d.primary, d.second.as_deref(), d.band.as_deref(), d.cap);
        set_tip(&c.child, &d.tooltip);
        t.update_property(&[gtk::accessible::Property::Label(&d.a11y)]);
        c.child
            .update_property(&[gtk::accessible::Property::Label(&d.a11y)]);
    }
}

/// Quatro faixas do kernel (duas por linha) e, abaixo dos fatos
/// do transcript, tres faixas do cockpit (CC9). Fora da tela (detalhe
/// recolhido atras da lista) nada e calculado; ao aparecer, desenha o ultimo
/// historico guardado.
struct HistoryView {
    panel: gtk::FlowBox,
    charts: Vec<ChartCell>,
    /// Faixas do transcript: ficam sob os fatos do transcript e somem com ele
    /// (multiplexador).
    cockpit_note: gtk::Label,
    cockpit_panel: gtk::FlowBox,
    cockpit_cells: Vec<StripCell>,
    /// Os limiares do ambiente, os mesmos que o overlay do sampler le
    /// (`Thresholds::from_process_env`).
    thresholds: Thresholds,
    pending: RefCell<Option<Pending>>,
}

impl HistoryView {
    fn new() -> Rc<Self> {
        let (panel, charts) = chart_panel();
        let cockpit_note = label(&["detail-note"]);
        // Separa a nota dos fatos do transcript logo acima.
        cockpit_note.set_margin_top(8);
        cockpit_note.set_wrap(true);
        cockpit_note.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        let (cockpit_panel, cockpit_cells) = strip_panel(&COCKPIT_STRIPS, &COCKPIT_SERIES, 3);
        let view = Rc::new(Self {
            panel,
            charts,
            cockpit_note,
            cockpit_panel,
            cockpit_cells,
            thresholds: Thresholds::from_process_env(),
            pending: RefCell::default(),
        });
        for p in [&view.panel, &view.cockpit_panel] {
            let v = Rc::downgrade(&view);
            p.connect_map(move |_| {
                if let Some(v) = v.upgrade() {
                    v.draw();
                }
            });
        }
        view
    }

    fn set(&self, p: Pending) {
        self.pending.replace(Some(p));
        if self.panel.is_mapped() || self.cockpit_panel.is_mapped() {
            self.draw();
        }
    }

    /// Mostra ou esconde as faixas do transcript (com os fatos dele).
    fn set_transcript_visible(&self, on: bool) {
        self.cockpit_note.set_visible(on);
        self.cockpit_panel.set_visible(on);
    }

    fn draw(&self) {
        let pending = self.pending.borrow();
        let Some(p) = pending.as_ref() else { return };
        let span = span_text(p.interval_ms);
        let every = match p.interval_ms {
            0 | 1000 => "One reading per second".to_string(),
            ms => format!("One reading every {:.1} s", ms as f64 / 1000.0),
        };
        set_label(
            &self.cockpit_note,
            &format!(
                "{span} from the transcript. Solid: context, burn, tool calls; \
                 dashed: re-read share. Lines across mark the limits; a bar \
                 under the line marks a loop."
            ),
        );
        let h = p.history.as_ref().and_then(|h| h.get(p.pid));
        // The window is the time axis under each chart; the cadence is in
        // the tooltip.
        let mut charts = ChartData::of(h, p.source, &span);
        for d in &mut charts {
            d.tooltip = format!("{} {every}, newest on the right.", d.tooltip);
        }
        draw_charts(&self.charts, &charts, span_secs(p.interval_ms));
        draw_cells(
            &self.cockpit_cells,
            &StripData::cockpit(h, &self.thresholds, &span),
        );
    }
}

/// How long a list Action waits for the newly selected session's control:
/// selecting a session rediscovers its capabilities off-thread, and the
/// control shows only once that read returns.
const REQUEST_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct Detail {
    /// A pagina de conteudo do split view.
    pub page: adw::NavigationPage,
    /// O cabecalho da pagina (voltar e titulo): so aparece com o split
    /// recolhido; aberto, o cabecalho da janela basta.
    pub toolbar: adw::ToolbarView,
    stack: gtk::Stack,
    ended: adw::StatusPage,
    pid: Cell<Option<u32>>,
    starttime: Cell<Option<u64>>,
    native_session_id: RefCell<Option<String>>,
    live: bool,
    session_controls: actions_ui::session_control::SessionControls,
    focus: gtk::Button,
    focus_reason: gtk::Label,
    focus_target: Rc<RefCell<Option<(Endpoint, FocusTarget)>>>,
    focusing: Rc<Cell<bool>>,
    /// The feedback line holds the reason the focus button is off.
    reason_shown: Cell<bool>,
    /// The list Action request in flight; a newer one ends the older poll.
    request_gen: Cell<u64>,
    shown: RefCell<Option<DetailText>>,
    glyph: StateGlyph,
    /// The header's one meta line ([`DetailText::meta_line`]).
    meta: gtk::Label,
    /// Session id, facts and controls, folded under the header.
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests read the expander"))]
    more: gtk::Expander,
    name: gtk::Label,
    state: gtk::Label,
    rec: gtk::Label,
    reason: gtk::Label,
    session_id_row: gtk::Box,
    session_id: gtk::Label,
    session: Facts,
    runs: Facts,
    pid_value: gtk::Label,
    transcript_head: gtk::Label,
    transcript: Facts,
    procs_head: gtk::Label,
    history: Rc<HistoryView>,
    procs: Rc<ProcessBars>,
    /// Alvo do menu de acoes do cabecalho: pid e nome da sessao mostrada.
    target: Rc<RefCell<Option<(u32, String)>>>,
    actions: gtk::MenuButton,
}

impl Detail {
    pub fn new(live: bool) -> Self {
        let none = adw::StatusPage::builder()
            .icon_name("view-list-symbolic")
            .title("No Session Selected")
            .description("Choose a session in the list to see its processes and transcript.")
            .build();
        none.add_css_class("compact");
        let ended = adw::StatusPage::builder()
            .icon_name("process-stop-symbolic")
            .title("Session Ended")
            .build();
        ended.add_css_class("compact");

        let glyph = StateGlyph::new(GLYPH_PX);
        let meta = label(&["detail-meta"]);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
        meta.set_lines(1);
        meta.set_xalign(0.0);
        let name = label(&["detail-name"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_accessible_role(gtk::AccessibleRole::Heading);
        name.set_selectable(true);
        let state = label(&["ac-pill"]);
        state.set_valign(gtk::Align::Center);
        let rec = label(&["ac-pill"]);
        rec.set_valign(gtk::Align::Center);
        let target: Rc<RefCell<Option<(u32, String)>>> = Rc::default();
        let t = target.clone();
        let actions = actions_ui::menu_button("Session actions", move || {
            t.borrow()
                .as_ref()
                .map(|(pid, name)| (Target::Session(*pid), name.clone()))
        });
        let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        title.add_css_class("detail-title");
        title.append(&glyph);
        title.append(&name);
        title.append(&state);
        title.append(&rec);
        // The pills hug the name; the buttons go to the far edge.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        title.append(&spacer);

        // Selecionavel para copiar; quebra em vez de elipse, que cortaria o
        // que se copia.
        let session_id_caption = label(&["detail-caption"]);
        session_id_caption.set_label("Session id");
        session_id_caption.set_accessible_role(gtk::AccessibleRole::Presentation);
        let session_id = label(&["detail-value", "monospace"]);
        session_id.set_selectable(true);
        session_id.set_wrap(true);
        session_id.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        let session_id_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        session_id_row.append(&session_id_caption);
        session_id_row.append(&session_id);
        let pid_caption = label(&["detail-caption"]);
        pid_caption.set_label("Pid");
        pid_caption.set_accessible_role(gtk::AccessibleRole::Presentation);
        let pid_value = label(&["detail-value", "numeric"]);
        pid_value.set_selectable(true);
        let pid_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pid_row.append(&pid_caption);
        pid_row.append(&pid_value);

        let session = Facts::new(&SESSION_FACTS);
        let face = gtk::Box::new(gtk::Orientation::Vertical, 6);
        face.add_css_class("detail-face");
        let reason = label(&["detail-note"]);
        reason.set_xalign(0.0);
        reason.set_wrap(true);
        reason.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        reason.set_selectable(true);
        let tail = gtk::Box::new(gtk::Orientation::Vertical, 6);
        tail.add_css_class("detail-tail");
        // The fold keeps identifiers only; everything else is in the open.
        tail.append(&session_id_row);
        tail.append(&pid_row);
        let runs = Facts::new(&RUNS_FACTS);
        runs.widget.add_css_class("detail-block");
        let focus = gtk::Button::with_label("Focus terminal");
        focus.set_halign(gtk::Align::Start);
        focus.set_size_request(44, 44);
        focus.set_sensitive(false);
        let focus_reason = label(&["detail-note"]);
        focus_reason.set_wrap(true);
        focus_reason.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        focus_reason.set_selectable(true);
        focus_reason.set_visible(false);
        let session_controls = actions_ui::session_control::SessionControls::new(live);
        // The page header repeats Handoff and Compact (shown only while the
        // detail is a pushed page): the public controls API has no second
        // instance, so each copy forwards its click to the real button.
        let header = adw::HeaderBar::new();
        for (short, long) in [("Compact", "Compact…"), ("Handoff", "Handoff…")] {
            let target = descendants(session_controls.widget.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| b.label().as_deref() == Some(long));
            let Some(target) = target else { continue };
            let copy = gtk::Button::with_label(short);
            target
                .bind_property("sensitive", &copy, "sensitive")
                .sync_create()
                .build();
            copy.connect_clicked(move |_| target.emit_clicked());
            header.pack_end(&copy);
        }
        // Focus sits on the title row beside the actions menu; its feedback
        // (the reason it is off, or the result) stays under the meta line.
        title.append(&focus);
        title.append(&actions);
        let more = gtk::Expander::new(Some("Session details"));
        more.add_css_class("detail-more");
        more.set_child(Some(&tail));
        face.append(&title);
        face.append(&meta);
        face.append(&focus_reason);
        face.append(&reason);
        let focus_target: Rc<RefCell<Option<(Endpoint, FocusTarget)>>> = Rc::default();
        let focusing = Rc::new(Cell::new(false));
        let (current, busy, feedback) =
            (focus_target.clone(), focusing.clone(), focus_reason.clone());
        focus.connect_clicked(move |button| {
            if busy.replace(true) {
                return;
            }
            let Some((endpoint, target)) = current.borrow().clone() else {
                busy.set(false);
                return;
            };
            button.set_sensitive(false);
            set_feedback(&feedback, "Checking terminal identity…");
            let identity = (target.pid, target.starttime);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = endpoint.focus(&target, std::time::Duration::from_secs(3));
                let _ = tx.send(result);
            });
            let (current, busy, reason) = (current.clone(), busy.clone(), feedback.clone());
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                let result = match rx.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        return gtk::glib::ControlFlow::Continue;
                    }
                    Err(_) => Err(agentcenter_core::control::herdr::Error::Unknown),
                };
                busy.set(false);
                if current
                    .borrow()
                    .as_ref()
                    .is_some_and(|(_, t)| (t.pid, t.starttime) == identity)
                {
                    set_feedback(
                        &reason,
                        &match result {
                            Ok(()) => {
                                "Terminal pane focused (not desktop window activation).".to_string()
                            }
                            Err(e) => format!(
                                "Terminal focus not confirmed: {e:?}. Refresh and retry explicitly."
                            ),
                        },
                    );
                }
                gtk::glib::ControlFlow::Break
            });
        });

        let history = HistoryView::new();

        let transcript_head = label(&["detail-section"]);
        transcript_head.set_label("Transcript");
        transcript_head.set_accessible_role(gtk::AccessibleRole::Heading);
        let transcript = Facts::new(&TRANSCRIPT_FACTS);
        transcript.widget.add_css_class("detail-block");

        let procs_head = label(&["detail-section"]);
        procs_head.set_accessible_role(gtk::AccessibleRole::Heading);
        // One short caption on the heading's row; the accounting is its tooltip.
        let note = label(&["detail-note"]);
        note.set_label("memory: in RAM + swapped out (hatched)");
        note.set_tooltip_text(Some(
            "Memory: in RAM + swapped out (hatched), against the largest process. PSS \
             splits shared pages and adds up to the session; RSS, per process and never \
             summed, is in each row's tooltip.",
        ));
        note.set_hexpand(true);
        note.set_xalign(1.0);
        note.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let procs_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        procs_row.add_css_class("detail-procs-head");
        procs_row.append(&procs_head);
        procs_row.append(&note);
        let procs = Rc::new(ProcessBars::new(live));
        procs.widget.add_css_class("detail-procs");

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("session-detail");
        // REVIEW_RR1 order: header with the verdict and its reason, Handoff
        // and Compact, where it runs, the context and token panel, then the
        // resource figures, charts and processes; the folded IDs last.
        body.append(&face);
        body.append(&session_controls.widget);
        body.append(&runs.widget);
        body.append(&transcript_head);
        body.append(&transcript.widget);
        body.append(&history.cockpit_note);
        body.append(&history.cockpit_panel);
        body.append(&session.widget);
        body.append(&history.panel);
        body.append(&procs_row);
        body.append(&procs.widget);
        body.append(&more);

        // As colunas seguem a largura do detalhe, nao a da janela.
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&body)
            .build();
        let bin = adw::BreakpointBin::builder()
            .child(&scroll)
            .width_request(300)
            .height_request(240)
            .build();
        // [largo, medio]: no medio (o ultimo que casa vence no bin) o corpo
        // tambem aperta o ar (sessions.css, `.session-detail.narrow`).
        // Narrow detail: less air (sessions.css, `.session-detail.narrow`)
        // and process rows without pid and state.
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            NARROW_MAX_SP,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(
            &body,
            "css-classes",
            Some(&["session-detail", "narrow"].to_value()),
        );
        // The title row has no room for the long label: the name keeps it.
        narrow.add_setter(&focus, "label", Some(&"Focus".to_value()));
        for on in [true, false] {
            let p = Rc::downgrade(&procs);
            let f = move |_: &adw::Breakpoint| {
                if let Some(p) = p.upgrade() {
                    p.set_compact(on);
                }
            };
            if on {
                narrow.connect_apply(f);
            } else {
                narrow.connect_unapply(f);
            }
        }
        bin.add_breakpoint(narrow);

        let stack = gtk::Stack::new();
        stack.add_named(&none, Some(STACK_NONE));
        stack.add_named(&ended, Some(STACK_ENDED));
        stack.add_named(&bin, Some(STACK_SESSION));
        stack.set_visible_child_name(STACK_NONE);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_css_class("detail-view");
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Session")
            .tag(PAGE_TAG)
            .child(&toolbar)
            .build();

        Self {
            page,
            toolbar,
            stack,
            ended,
            pid: Cell::new(None),
            starttime: Cell::new(None),
            native_session_id: RefCell::new(None),
            live,
            session_controls,
            focus,
            focus_reason,
            focus_target,
            focusing,
            reason_shown: Cell::new(false),
            request_gen: Cell::new(0),
            shown: RefCell::new(None),
            glyph,
            meta,
            more,
            name,
            state,
            rec,
            reason,
            session_id_row,
            session_id,
            session,
            runs,
            pid_value,
            transcript_head,
            transcript,
            procs_head,
            history,
            procs,
            target,
            actions,
        }
    }

    /// Troca a sessao mostrada e ja desenha com o ultimo snapshot.
    /// Starts the list's Action for session `pid` through the same control
    /// button the detail shows, so the confirmation and checks stay in one
    /// place. The row's click selects `pid`, and the detail follows that
    /// selection in an idle, then rediscovers the control off-thread: the
    /// request waits until the detail shows `pid` with its control up, never
    /// pressing another session's. `unavailable` runs when that does not
    /// happen within [`REQUEST_WAIT`], and at once for End, which has no
    /// control here. A newer request replaces a pending one.
    pub fn request(
        self: &Rc<Self>,
        pid: u32,
        op: actions_ui::RowOp,
        unavailable: impl FnOnce() + 'static,
    ) {
        self.request_within(pid, op, REQUEST_WAIT, unavailable);
    }

    fn request_within(
        self: &Rc<Self>,
        pid: u32,
        op: actions_ui::RowOp,
        wait: std::time::Duration,
        unavailable: impl FnOnce() + 'static,
    ) {
        let generation = self.request_gen.get().wrapping_add(1);
        self.request_gen.set(generation);
        if !matches!(op, actions_ui::RowOp::Compact | actions_ui::RowOp::Handoff) {
            unavailable();
            return;
        }
        if self.pid.get() == Some(pid) && self.click_control(op) {
            return;
        }
        let until = std::time::Instant::now() + wait;
        let weak = Rc::downgrade(self);
        let mut unavailable = Some(unavailable);
        // Set once the detail shows `pid`: moving away after that drops the
        // request, the user picked another session.
        let mut reached = self.pid.get() == Some(pid);
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let Some(detail) = weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if detail.request_gen.get() != generation {
                return gtk::glib::ControlFlow::Break;
            }
            if detail.pid.get() == Some(pid) {
                reached = true;
                if detail.click_control(op) {
                    return gtk::glib::ControlFlow::Break;
                }
            } else if reached {
                return gtk::glib::ControlFlow::Break;
            }
            if std::time::Instant::now() < until {
                return gtk::glib::ControlFlow::Continue;
            }
            if let Some(f) = unavailable.take() {
                f();
            }
            gtk::glib::ControlFlow::Break
        });
    }

    /// The session the detail shows, for the window's tests.
    #[cfg(test)]
    pub fn shown_pid(&self) -> Option<u32> {
        self.pid.get()
    }

    fn click_control(&self, op: actions_ui::RowOp) -> bool {
        let long = match op {
            actions_ui::RowOp::Compact => "Compact…",
            actions_ui::RowOp::Handoff => "Handoff…",
            _ => return false,
        };
        let target = descendants(self.session_controls.widget.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some(long) && b.is_sensitive());
        target.map(|b| b.emit_clicked()).is_some()
    }

    pub fn show(
        &self,
        pid: Option<u32>,
        snap: Option<&Snapshot>,
        history: Option<&Arc<History>>,
        home: Option<&str>,
    ) {
        if self.pid.replace(pid) != pid {
            set_feedback(&self.focus_reason, "");
            self.reason_shown.set(false);
            self.focus_target.replace(None);
            self.focus.set_sensitive(false);
            self.starttime.set(
                snap.and_then(|s| s.sessions.iter().find(|s| Some(s.pid) == pid))
                    .and_then(|s| s.starttime),
            );
            self.native_session_id.replace(
                snap.and_then(|s| s.sessions.iter().find(|s| Some(s.pid) == pid))
                    .and_then(|s| s.overlay.as_ref())
                    .and_then(|o| o.agent_session_id.clone()),
            );
            self.procs.clear();
            self.shown.replace(None);
        }
        match snap {
            Some(s) => self.apply(s, history, home),
            None if pid.is_none() => {
                self.session_controls.set_session(None);
                self.set_page(STACK_NONE);
            }
            None => {}
        }
    }

    /// Um tick: atualiza a sessao mostrada; se ela sumiu, diz que acabou.
    /// `history` e o historico do sampler deste tick (`None` sem ele: faixas
    /// vazias).
    pub fn apply(&self, snap: &Snapshot, history: Option<&Arc<History>>, home: Option<&str>) {
        let Some(pid) = self.pid.get() else {
            self.session_controls.set_session(None);
            self.set_page(STACK_NONE);
            return;
        };
        let source = snap.machine.io_stall_source;
        let Some(s) = snap.sessions.iter().find(|s| {
            s.pid == pid
                && s.starttime == self.starttime.get()
                && self.native_session_id.borrow().as_deref().is_none_or(|id| {
                    s.overlay
                        .as_ref()
                        .and_then(|o| o.agent_session_id.as_deref())
                        == Some(id)
                })
        }) else {
            self.session_controls.set_session(None);
            self.focus_target.replace(None);
            self.focus.set_sensitive(false);
            self.target.replace(None);
            self.actions.set_sensitive(false);
            if self.stack.visible_child_name().as_deref() != Some(STACK_ENDED) {
                let name = self.shown.borrow().as_ref().map_or_else(
                    || format!("pid {pid}"),
                    |t| format!("{} (pid {pid})", t.name),
                );
                self.ended
                    .set_description(Some(&gtk::glib::markup_escape_text(&format!(
                        "{name} is no longer running. Choose another session in the list."
                    ))));
                self.procs.clear();
                self.set_page(STACK_ENDED);
            }
            return;
        };
        if self.native_session_id.borrow().is_none() {
            self.native_session_id
                .replace(s.overlay.as_ref().and_then(|o| o.agent_session_id.clone()));
        }
        self.session_controls.set_session(Some(s));
        let t = DetailText::of(s, source, home);
        if self.shown.borrow().as_ref() != Some(&t) {
            self.render(&t);
            self.shown.replace(Some(t));
        }
        self.actions
            .set_sensitive(self.live && s.starttime.is_some());
        match focus_capability(snap, s, self.live) {
            Ok(target) => {
                self.focus_target.replace(Some(target));
                self.focus.set_sensitive(!self.focusing.get());
                self.set_focus_state(None);
            }
            Err(reason) => {
                self.focus_target.replace(None);
                self.focus.set_sensitive(false);
                self.set_focus_state(Some(reason));
            }
        }
        self.history.set(Pending {
            history: history.cloned(),
            pid,
            source,
            interval_ms: snap.interval_ms,
        });
        self.procs.apply(s, source, tone(Group::of(s)));
        let head = match self.procs.len() {
            1 => "Processes · 1".to_string(),
            n => format!("Processes · {n}"),
        };
        set_label(&self.procs_head, &head);
        self.set_page(STACK_SESSION);
    }

    /// Feedback of the focus action under the meta line; the line is there
    /// only while there is something to say.
    #[cfg(test)]
    fn show_focus_feedback(&self, text: &str) {
        set_feedback(&self.focus_reason, text);
    }

    /// Why the button works or does not: its tooltip and spoken description,
    /// not a standing line under the header.
    fn set_focus_hint(&self, hint: &str) {
        if self.focus.tooltip_text().as_deref() != Some(hint) {
            self.focus.set_tooltip_text(Some(hint));
            self.focus
                .update_property(&[gtk::accessible::Property::Description(hint)]);
        }
    }

    /// The focus button on or off. Off, the reason is visible text (a tooltip
    /// alone does not reach keyboard or touch); on, the generic hint is only
    /// the tooltip and the line goes away, leaving a click's feedback alone.
    fn set_focus_state(&self, off_reason: Option<&str>) {
        match off_reason {
            Some(reason) => {
                self.set_focus_hint(reason);
                set_feedback(&self.focus_reason, reason);
                self.reason_shown.set(true);
            }
            None => {
                self.set_focus_hint("Focus the explicitly identified herdr pane.");
                if self.reason_shown.replace(false) {
                    set_feedback(&self.focus_reason, "");
                }
            }
        }
    }

    fn set_page(&self, name: &str) {
        if self.stack.visible_child_name().as_deref() != Some(name) {
            self.stack.set_visible_child_name(name);
        }
        let title = match name {
            STACK_SESSION => self
                .shown
                .borrow()
                .as_ref()
                .map_or_else(|| "Session".to_string(), |t| t.name.clone()),
            STACK_ENDED => "Session Ended".to_string(),
            _ => "Session".to_string(),
        };
        if self.page.title() != title {
            self.page.set_title(&title);
        }
    }

    fn render(&self, t: &DetailText) {
        let g = Group::from_rank(t.group);
        let mux = g == Group::Multiplexer;
        self.glyph.show(t.group);
        set_label(&self.name, &t.name);
        set_tip(&self.name, &t.name);

        let state = match g {
            Group::Busy => "busy",
            Group::Blocked => "blocked",
            Group::Idle => "idle",
            _ => "unknown",
        };
        set_label(&self.state, &t.status);
        set_classes(&self.state, &["ac-pill", state]);
        self.state.set_visible(!mux);
        set_label(&self.rec, &t.rec_text);
        set_tip(&self.rec, &t.rec_tooltip);
        set_label(
            &self.reason,
            &format!("Monitor recommendation · {}. {}", t.rec_text, t.rec_tooltip),
        );
        self.reason.set_visible(!mux);
        set_classes(&self.rec, &["ac-pill", "rec", rec_class(t.rec)]);
        self.rec.set_visible(!mux && t.rec_text != DASH);
        // O core recusa o servidor herdr: sem menu que so falharia.
        self.actions.set_visible(!mux);
        // O herdr nunca reporta id de sessao: a linha so confundiria.
        if let Some(row) = self.session_id.parent() {
            row.set_visible(!mux);
        }
        self.target.replace(Some((t.pid, t.name.clone())));

        set_label(&self.meta, &t.meta_line());
        set_tip(&self.meta, &t.meta_line());
        self.runs.set(&t.runs, &RUNS_FACTS);
        set_label(&self.pid_value, &t.pid.to_string());
        set_label(&self.session_id, &t.session_id.value);
        set_tip(&self.session_id_row, &t.session_id.tooltip);
        self.session_id
            .update_property(&[gtk::accessible::Property::Label(&t.session_id_a11y)]);

        self.session.set(&t.session, &SESSION_FACTS);
        self.transcript_head.set_visible(t.transcript.is_some());
        self.transcript.widget.set_visible(t.transcript.is_some());
        self.history.set_transcript_visible(t.transcript.is_some());
        if let Some(f) = &t.transcript {
            self.transcript.set(f, &TRANSCRIPT_FACTS);
        }
        // O titulo (nome) e um heading; a descricao diz agente, tipo e estado.
        self.name
            .update_property(&[gtk::accessible::Property::Description(&t.a11y)]);
    }
}

/// Pure capability discovery. The core revalidates kernel identity, peer and pane
/// immediately before the effect; a snapshot alone never authorizes execution.
fn focus_capability(
    snap: &Snapshot,
    s: &Session,
    live: bool,
) -> Result<(Endpoint, FocusTarget), &'static str> {
    if !live {
        return Err("Terminal actions are disabled for fixtures.");
    }
    if s.starttime.is_none() {
        return Err("Kernel identity unavailable; terminal focus is disabled.");
    }
    let target = FocusTarget::from_session(s)
        .ok_or("Explicit workspace, tab and pane identity unavailable.")?;
    if [&target.workspace_id, &target.tab_id, &target.pane_id]
        .iter()
        .any(|id| id.is_empty() || matches!(id.as_str(), "current" | "default"))
    {
        return Err("Explicit terminal identity unavailable; no default target is allowed.");
    }
    let servers: Vec<_> = snap
        .sessions
        .iter()
        .filter(|s| {
            s.agent == "herdr"
                && s.cmdline
                    .as_deref()
                    .is_some_and(|c| c.split_whitespace().any(|a| a == "server"))
                && s.starttime.is_some()
        })
        .collect();
    if servers.len() != 1 {
        return Err("Herdr server identity is missing or ambiguous; terminal focus is disabled.");
    }
    let socket = agentcenter_core::overlay::herdr::socket_path(|k| std::env::var_os(k))
        .ok_or("Herdr socket path unavailable; terminal focus is disabled.")?;
    Ok((
        Endpoint {
            socket,
            server_pid: servers[0].pid,
            server_starttime: servers[0].starttime.unwrap(),
        },
        target,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_glyph::Shape;
    use agentcenter_core::adapters::AgentRole;
    use agentcenter_core::overlay::{HerdrOverlay, SessionOverlay};

    fn session(role: AgentRole) -> Session {
        let mut s = Session::aggregate("claude".into(), role, 42, 16, Vec::new());
        s.cwd = Some("/home/dev/src/kiln".into());
        s
    }

    /// Detail must pin kernel identity across refresh, disappearance and explicit refocus.
    #[test]
    fn cc2_regression_detail_pins_kernel_identity_until_explicit_reselection() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let mut s = session(AgentRole::Agent);
            s.starttime = Some(123);
            let mut snap = Snapshot {
                schema_version: agentcenter_core::snapshot::SCHEMA_VERSION,
                taken_at_ms: 0,
                interval_ms: 1000,
                machine: agentcenter_core::Machine {
                    cores: 16,
                    cpu_pct: None,
                    io_stall_source: IoStallSource::Delayacct,
                    processes: 0,
                    session_processes: 0,
                    mem_total_bytes: None,
                    mem_available_bytes: None,
                    swap_total_bytes: None,
                    swap_free_bytes: None,
                    load_avg: None,
                    psi: None,
                },
                sessions: vec![s],
                summary: Default::default(),
            };
            let detail = Detail::new(false);
            detail.show(Some(42), Some(&snap), None, None);
            assert_eq!(detail.starttime.get(), Some(123));
            assert_eq!(
                detail.stack.visible_child_name().as_deref(),
                Some(STACK_SESSION)
            );
            assert!(
                !detail.focus.is_sensitive(),
                "fixtures must never enable focus"
            );
            snap.sessions[0].starttime = Some(124);
            detail.apply(&snap, None, None);
            assert_eq!(
                detail.stack.visible_child_name().as_deref(),
                Some(STACK_ENDED)
            );
            assert!(detail.focus_target.borrow().is_none());
            detail.show(Some(42), Some(&snap), None, None);
            assert_eq!(
                detail.starttime.get(),
                Some(123),
                "same-PID refresh must not repin"
            );
            detail.show(None, Some(&snap), None, None);
            detail.show(Some(42), Some(&snap), None, None);
            assert_eq!(detail.starttime.get(), Some(124));
            assert_eq!(
                detail.stack.visible_child_name().as_deref(),
                Some(STACK_SESSION)
            );
            snap.sessions[0].overlay = Some(SessionOverlay {
                agent_session_id: Some("original".into()),
                ..Default::default()
            });
            detail.apply(&snap, None, None);
            assert_eq!(
                detail.native_session_id.borrow().as_deref(),
                Some("original")
            );
            snap.sessions[0].overlay.as_mut().unwrap().agent_session_id =
                Some("replacement".into());
            detail.apply(&snap, None, None);
            assert_eq!(
                detail.stack.visible_child_name().as_deref(),
                Some(STACK_ENDED)
            );
            assert!(detail.focus_target.borrow().is_none());
            assert!(!detail.actions.is_sensitive());
            detail.show(Some(42), Some(&snap), None, None);
            assert_eq!(
                detail.native_session_id.borrow().as_deref(),
                Some("original")
            );
            detail.show(None, Some(&snap), None, None);
            detail.show(Some(42), Some(&snap), None, None);
            assert_eq!(
                detail.native_session_id.borrow().as_deref(),
                Some("replacement")
            );
            assert_eq!(
                detail.stack.visible_child_name().as_deref(),
                Some(STACK_SESSION)
            );
            snap.sessions.clear();
            detail.apply(&snap, None, None);
            assert_eq!(
                detail.stack.visible_child_name().as_deref(),
                Some(STACK_ENDED)
            );
            assert!(!detail.focus.is_sensitive());
            assert!(!detail.actions.is_sensitive());
        });
    }

    #[test]
    fn missing_transcript_fields_are_dashes() {
        let t = DetailText::of(
            &session(AgentRole::Agent),
            IoStallSource::Delayacct,
            Some("/home/dev"),
        );
        let f = t.transcript.expect("an agent has a transcript block");
        for fact in &f {
            assert_eq!(fact.value, DASH, "{fact:?}");
        }
        assert_eq!(t.cwd, "~/src/kiln");
        assert_eq!(t.session[4].value, DASH, "no stall reading");
        assert!(t.herdr.is_empty());
    }

    #[test]
    fn transcript_metadata_reads_the_overlay() {
        let mut s = session(AgentRole::Agent);
        s.overlay = Some(SessionOverlay {
            model: Some("claude-opus-5-5".into()),
            provider: Some("anthropic".into()),
            kind: Some("interactive".into()),
            context_pct: Some(86.1),
            tokens_in: Some(9_980_000),
            tokens_out: Some(120_000),
            tool_calls: Some(40),
            tool_calls_open: Some(2),
            tool_calls_per_min: Some(2.4),
            subagents_live: Some(1),
            subagents_total: Some(4),
            herdr: Some(HerdrOverlay {
                pane_id: Some("w4:p3".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        s.stall_pct = Some(12.4);
        let t = DetailText::of(&s, IoStallSource::Delayacct, None);
        let f = t.transcript.unwrap();
        let v: Vec<&str> = f.iter().map(|f| f.value.as_str()).collect();
        assert_eq!(
            v,
            [
                "claude-opus-5-5 (Anthropic)",
                "86%",
                "in 10.0M · out 120.0k",
                "40 total · 2 in flight · 2.4/min",
                "1 live · 4 total",
            ]
        );
        assert_eq!(t.identity, "claude · interactive");
        assert_eq!(t.herdr, "herdr w4:p3");
        assert_eq!(t.session[4].value, "12%");
    }

    #[test]
    fn partial_tool_calls_keep_a_dash_for_the_missing_part() {
        let mut s = session(AgentRole::Agent);
        s.overlay = Some(SessionOverlay {
            tool_calls_open: Some(0),
            ..Default::default()
        });
        let t = DetailText::of(&s, IoStallSource::Delayacct, None);
        assert_eq!(
            t.transcript.unwrap()[3].value,
            format!("{DASH} total · 0 in flight · {DASH}/min"),
            "0 in flight is a reading, the others are absent"
        );
    }

    #[test]
    fn multiplexer_has_no_transcript_and_no_state_pill() {
        let mut s = session(AgentRole::Multiplexer);
        s.agent = "herdr".into();
        let t = DetailText::of(&s, IoStallSource::DState, None);
        assert!(t.transcript.is_none());
        assert!(t.status.is_empty());
        assert_eq!(t.identity, "herdr · multiplexer");
    }

    /// Native session id of the fixtures: a Claude Code uuid shape, made up.
    const SID: &str = "5d2c8e1a-7b3f-4c9e-a1d0-6f4b2e8c9a17";

    /// The detail names the session as the list does and carries the id the
    /// agent itself reports (Claude Code uuid, pi session file id, Codex id).
    #[test]
    fn identity_shows_the_list_name_and_the_native_session_id() {
        for agent in ["claude", "pi", "codex"] {
            let mut s = session(AgentRole::Agent);
            s.agent = agent.into();
            s.overlay = Some(SessionOverlay {
                name: Some("refactor sampler".into()),
                agent_session_id: Some(SID.into()),
                ..Default::default()
            });
            let t = DetailText::of(&s, IoStallSource::Delayacct, None);
            let row = Row::from_session(&s, IoStallSource::Delayacct, None);
            assert_eq!(t.name, row.name, "{agent}: the name the list shows");
            assert_eq!(t.name, "refactor sampler");
            assert_eq!(t.session_id.value, SID, "{agent}");
            assert!(!t.session_id.tooltip.is_empty(), "{agent}");
            assert_eq!(t.session_id_a11y, format!("Session id {SID}"), "{agent}");
        }
    }

    #[test]
    fn missing_or_blank_session_id_is_a_dash_and_hostile_text_is_sanitized() {
        let s = session(AgentRole::Agent);
        let t = DetailText::of(&s, IoStallSource::Delayacct, None);
        assert_eq!(
            t.name, "kiln",
            "no overlay name: the list falls back to the cwd"
        );
        assert_eq!(t.session_id.value, DASH, "absent is a dash, never empty");
        assert!(!t.session_id.tooltip.is_empty());
        assert_eq!(t.session_id_a11y, "Session id unavailable");

        let mut blank = session(AgentRole::Agent);
        blank.overlay = Some(SessionOverlay {
            agent_session_id: Some("   ".into()),
            ..Default::default()
        });
        let t = DetailText::of(&blank, IoStallSource::Delayacct, None);
        assert_eq!(t.session_id.value, DASH, "blank is not an id");

        let mut hostile = session(AgentRole::Agent);
        hostile.overlay = Some(SessionOverlay {
            agent_session_id: Some("abc\u{202e}def".into()),
            ..Default::default()
        });
        let t = DetailText::of(&hostile, IoStallSource::Delayacct, None);
        assert_eq!(
            t.session_id.value, "abc\\u{202e}def",
            "bidi override escaped"
        );
    }

    fn history_of(s: &Session, samples: usize) -> History {
        let mut h = History::new(HISTORY_LEN);
        for _ in 0..samples {
            h.record(std::slice::from_ref(s), |_| 1);
        }
        h
    }

    #[test]
    fn no_history_draws_empty_tracks_and_dashes() {
        let data = ChartData::of(None, IoStallSource::Delayacct, "Last 10 minutes");
        assert_eq!(data.each_ref().map(|d| d.title), CHART_TITLES);
        for d in &data {
            assert_eq!(d.value, DASH);
            assert!(d.primary.is_empty());
            assert!(!d.alert);
            assert!(
                d.a11y.contains("last 10 minutes: no history yet"),
                "{}",
                d.a11y
            );
        }
    }

    #[test]
    fn strips_scale_to_the_peak_and_flag_the_stall_limit() {
        use crate::timeline::Scale;
        let mut s = session(AgentRole::Agent);
        s.cpu_pct = Some(140.0);
        s.pss_bytes = Some(704 * 1024 * 1024);
        s.swap_pss_bytes = Some(10 * 1024 * 1024);
        s.stall_pct = Some(12.0);
        let h = history_of(&s, 3);
        let data = ChartData::of(h.get(42), IoStallSource::Delayacct, "Last 10 minutes");
        let [cpu, mem, disk, stall] = &data;
        assert_eq!(
            cpu.scale,
            Scale::Nice {
                floor: 10.0,
                binary: false
            }
        );
        assert_eq!(cpu.top(), 200.0);
        assert_eq!(cpu.primary.len(), 3);
        assert_eq!(
            (cpu.value.as_str(), cpu.sub.as_str()),
            ("140 %", "max 140 % \u{b7} mean 140 %")
        );
        // Below 1 GiB the chart counts in MiB, on the unit scale.
        assert_eq!(mem.unit, "MiB");
        assert_eq!(
            (mem.primary[0], mem.band.as_deref()),
            (704.0, Some(&[10.0f32; 3][..]))
        );
        // 714 MiB reads 0..800 (steps of 1, 1.5, 2, 3, 4, 6, 8), no longer a
        // power of two: the mock's 1.1 GiB reads 0..1.5.
        assert_eq!(mem.top(), 800.0);
        assert_eq!(mem.value, "714 MiB");
        assert_eq!(mem.sub, "solid: in RAM 704 \u{b7} hatched: swapped out 10");
        // No disk reading: track and dash, never 0.
        assert_eq!(disk.value, DASH);
        assert!(disk.second.is_some(), "write is the dashed series");
        assert_eq!(stall.threshold, Some(STALL_LIMIT_PCT));
        assert_eq!(stall.top(), 20.0, "floor 20 keeps the 10 % line mid-chart");
        assert_eq!(stall.value, "12 %");
        assert!(stall.alert, "12% is over the 10% limit");
        assert!(
            stall.a11y.starts_with("I/O stall, last 10 minutes: "),
            "{}",
            stall.a11y
        );

        s.threads_d = Some(3);
        s.stall_pct = None;
        let h = history_of(&s, 2);
        let [.., stall] = ChartData::of(h.get(42), IoStallSource::DState, "Last 10 minutes");
        assert_eq!(
            (stall.value.as_str(), stall.threshold, stall.alert),
            ("D:3", None, false)
        );
    }

    /// From 1 GiB up the memory chart counts in GiB, two decimals, like the
    /// mock (`1.10 GiB`, in RAM 0.55, swapped out 0.55).
    #[test]
    fn memory_counts_in_gib_from_one_gib() {
        let mut s = session(AgentRole::Agent);
        s.pss_bytes = Some(1536 * 1024 * 1024);
        s.swap_pss_bytes = Some(512 * 1024 * 1024);
        let h = history_of(&s, 2);
        let [_, mem, ..] = ChartData::of(h.get(42), IoStallSource::Delayacct, "Last 10 minutes");
        assert_eq!(mem.unit, "GiB");
        assert_eq!(mem.primary, [1.5, 1.5]);
        assert_eq!(mem.value, "2.00 GiB");
        assert_eq!(
            mem.sub,
            "solid: in RAM 1.50 \u{b7} hatched: swapped out 0.50"
        );
        assert_eq!(mem.top(), 2.0);
    }

    /// Disk: read solid, write dashed, both now in one unit picked by the
    /// peak (`2.4 / 0.5` MiB/s; KiB/s below 1 MiB/s).
    #[test]
    fn disk_reads_read_over_write_in_one_unit() {
        let mut s = session(AgentRole::Agent);
        s.read_bps = Some(2.4 * MIB);
        s.write_bps = Some(0.5 * MIB);
        let h = history_of(&s, 2);
        let [_, _, disk, _] = ChartData::of(h.get(42), IoStallSource::Delayacct, "Last 10 minutes");
        assert_eq!(
            (disk.unit.as_str(), disk.value.as_str()),
            ("MiB/s", "2.4 / 0.5")
        );
        assert_eq!(disk.sub, "solid: read \u{b7} dashed: write");
        assert_eq!(disk.second.as_deref(), Some(&[0.5f32, 0.5][..]));

        s.read_bps = Some(300.0 * 1024.0);
        s.write_bps = Some(0.0);
        let h = history_of(&s, 2);
        let [_, _, disk, _] = ChartData::of(h.get(42), IoStallSource::Delayacct, "Last 10 minutes");
        assert_eq!(
            (disk.unit.as_str(), disk.value.as_str()),
            ("KiB/s", "300 / 0")
        );
    }

    #[test]
    fn span_follows_the_tick() {
        assert_eq!(span_text(1000), "Last 10 minutes");
        assert_eq!(span_text(0), "Last 10 minutes");
        assert_eq!(span_text(2000), "Last 20 minutes");
        assert_eq!(span_text(200), "Last 2 minutes");
        assert_eq!(span_text(100), "Last 60 seconds");
    }

    #[test]
    fn d_state_fallback_says_threads_waiting() {
        let mut s = session(AgentRole::Agent);
        s.threads_d = Some(3);
        let t = DetailText::of(&s, IoStallSource::DState, None);
        assert_eq!(t.session[4].value, "D:3");
    }

    // ---- Faixas do transcript (CC9) ---------------------------------------

    use agentcenter_core::cockpit::{Cockpit, Loops, Tokens};

    fn cockpit(loops: Loops) -> Cockpit {
        let t = Tokens {
            input: 1_000_000,
            cache_write: 200_000,
            cache_read: 3_000_000,
            output: 50_000,
            ctx: 600_000,
            window: 1_000_000,
            ..Default::default()
        };
        Cockpit::compute_with_loops(&Thresholds::default(), &t, loops, None, None)
    }

    fn loops(repeats: u32) -> Loops {
        Loops {
            max_streak: 1,
            repeated_max: repeats,
        }
    }

    /// Uma sessao com o overlay dado em cada tick; `None` = tick sem overlay.
    fn cockpit_history(ticks: &[Option<SessionOverlay>]) -> History {
        let mut h = History::new(HISTORY_LEN);
        for o in ticks {
            let mut s = session(AgentRole::Agent);
            s.overlay = o.clone();
            h.record(std::slice::from_ref(&s), |_| 1);
        }
        h
    }

    fn reading(ctx: f64, per_min: f64, repeats: u32) -> Option<SessionOverlay> {
        Some(SessionOverlay {
            context_pct: Some(ctx),
            tool_calls_per_min: Some(per_min),
            cockpit: Some(cockpit(loops(repeats))),
            ..Default::default()
        })
    }

    /// Sessao com loop: tres ticks sem overlay (lacuna), dois sem loop, tres
    /// em loop, um fora e dois em loop de novo (o agora).
    fn looping() -> History {
        let mut ticks = vec![None, None, None];
        ticks.extend([reading(40.0, 2.0, 1), reading(45.0, 3.0, 2)]);
        ticks.extend((0..3).map(|_| reading(60.0, 12.0, 5)));
        ticks.push(reading(62.0, 4.0, 1));
        ticks.extend((0..2).map(|_| reading(92.0, 14.0, 6)));
        cockpit_history(&ticks)
    }

    #[test]
    fn cockpit_strips_mark_loops_limits_and_gaps() {
        let h = looping();
        let th = Thresholds::default();
        let [ctx, burn, tools] = StripData::cockpit(h.get(42), &th, "Last 10 minutes");

        // Contexto: limites 70 e 90 dos Thresholds, escala 0..100, carmim
        // acima do handoff; a lacuna do comeco e NaN, nunca 0.
        assert_eq!(ctx.limits, [70.0, 90.0]);
        assert_eq!(ctx.max, 100.0);
        assert!(ctx.alert, "92% >= 90%");
        assert_eq!(ctx.series.len(), 2, "contexto e releitura");
        assert!(ctx.series[0][..3].iter().all(|v| v.is_nan()));
        assert!(ctx.series[1][..3].iter().all(|v| v.is_nan()));
        let share = cockpit(loops(1)).reread_share as f32 * 100.0;
        assert_eq!(ctx.series[1][3], share, "releitura em %");
        assert!(
            ctx.a11y
                .contains("marks: re-read floor at 70%, handoff at 90%"),
            "{}",
            ctx.a11y
        );
        assert!(ctx.a11y.starts_with("Context, last 10 minutes: "));

        // Burn: limite no burn cheio, escala do dobro.
        assert_eq!(burn.limits, [50.0]);
        assert_eq!(burn.max, 100.0);
        assert!(
            burn.tooltip.contains("full burn at 50%"),
            "{}",
            burn.tooltip
        );

        // Tool calls/min: dois trechos de loop, o ultimo e o agora.
        assert_eq!(tools.bands, vec![5..8, 9..11]);
        assert_eq!(tools.readout, "LOOP ×6");
        assert!(tools.alert);
        assert_eq!(tools.max, 20.0, "pico 14/min, escala 1-2-5");
        assert!(
            tools.a11y.contains("the same tool call repeated 6 times")
                && tools.a11y.contains("in 2 stretches"),
            "{}",
            tools.a11y
        );
    }

    #[test]
    fn cockpit_strips_without_cockpit_say_unavailable_never_zero() {
        // Overlay com contexto e ritmo, mas sem cockpit (varredura pendente).
        let pending = SessionOverlay {
            context_pct: Some(30.0),
            tool_calls_per_min: Some(1.0),
            cockpit_pending: true,
            ..Default::default()
        };
        let h = cockpit_history(&[Some(pending.clone()), Some(pending)]);
        let [ctx, burn, tools] =
            StripData::cockpit(h.get(42), &Thresholds::default(), "Last 10 minutes");
        assert_eq!(ctx.readout, "max 30%");
        let h2 = cockpit_history(&[reading(10.0, 0.0, 1)]);
        let [c2, ..] = StripData::cockpit(h2.get(42), &Thresholds::default(), "x");
        assert_eq!(c2.readout, "max 10%", "a releitura (maior) nao e a leitura");
        assert!(
            ctx.series[1].iter().all(|v| v.is_nan()),
            "releitura ausente"
        );
        assert!(
            ctx.tooltip
                .contains("re-read, % of weighted burn (dashed): max —, now —")
        );
        assert_eq!(burn.readout, DASH, "sem cockpit: travessao, nunca 0%");
        assert!(!burn.alert);
        assert!(tools.bands.is_empty());
        assert_eq!(tools.readout, "max 1.0/min");
        assert!(
            tools.a11y.ends_with("loop signal not read in this window"),
            "{}",
            tools.a11y
        );

        // Sem historico nenhum: as tres faixas vazias.
        for d in StripData::cockpit(None, &Thresholds::default(), "Last 10 minutes") {
            assert_eq!(d.readout, DASH);
            assert!(d.series.is_empty() && d.bands.is_empty() && d.limits.is_empty());
        }
    }

    #[test]
    fn cockpit_strips_of_a_session_just_opened_start_at_the_right_edge() {
        let h = cockpit_history(&[reading(10.0, 0.0, 1)]);
        let [ctx, _, tools] =
            StripData::cockpit(h.get(42), &Thresholds::default(), "Last 10 minutes");
        assert_eq!(ctx.series[0], [10.0]);
        assert_eq!(
            ctx.cap, HISTORY_LEN,
            "uma amostra de 600: nada antes do app"
        );
        assert_eq!(tools.readout, "max 0.0/min", "0/min lido e 0");
        assert!(
            tools.a11y.ends_with("no loop in this window"),
            "{}",
            tools.a11y
        );
        let p = crate::graph::points(&ctx.series[0], ctx.cap, ctx.max, 599.0, 30.0);
        assert_eq!(p, vec![vec![(599.0, 27.0)]], "um ponto na borda direita");
    }

    fn snapshot_of(sessions: Vec<Session>) -> Snapshot {
        Snapshot {
            schema_version: agentcenter_core::snapshot::SCHEMA_VERSION,
            taken_at_ms: 0,
            interval_ms: 1000,
            machine: agentcenter_core::Machine {
                cores: 16,
                cpu_pct: None,
                io_stall_source: IoStallSource::Delayacct,
                processes: 0,
                session_processes: 0,
                mem_total_bytes: None,
                mem_available_bytes: None,
                swap_total_bytes: None,
                swap_free_bytes: None,
                load_avg: None,
                psi: None,
            },
            sessions,
            summary: Default::default(),
        }
    }

    /// Criterio 2 do CC9: o detalhe desenha as faixas do transcript da
    /// fixture de historico (loop, sem cockpit, recem-aberta), as celulas
    /// sao focaveis com o resumo como rotulo, e o multiplexador nao as mostra.
    #[test]
    fn detail_draws_cockpit_strips_from_a_history_fixture() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let mut s = session(AgentRole::Agent);
            s.starttime = Some(1);
            s.overlay = reading(92.0, 14.0, 6);
            let snap = snapshot_of(vec![s.clone()]);
            let detail = Detail::new(false);
            let cells = &detail.history.cockpit_cells;

            // Loop.
            let h = Arc::new(looping());
            detail.show(Some(42), Some(&snap), Some(&h), None);
            detail.history.draw();
            assert!(detail.history.cockpit_panel.is_visible());
            assert_eq!(cells.len(), COCKPIT_STRIPS.len());
            let tools = &cells[2];
            assert_eq!(tools.readout.label(), "LOOP ×6");
            assert!(tools.readout.has_css_class("alert"));
            assert!(tools.strip.has_css_class("alert"));
            assert!(tools.child.is_focusable(), "o teclado chega a faixa");
            let tip = tools.child.tooltip_text().unwrap_or_default();
            assert!(tip.contains("In a loop now"), "{tip}");
            assert!(cells[0].readout.has_css_class("alert"), "92% no handoff");
            assert_eq!(
                detail.history.cockpit_note.label(),
                "Last 10 minutes from the transcript. Solid: context, burn, tool calls; \
                 dashed: re-read share. Lines across mark the limits; a bar under the line \
                 marks a loop."
            );

            // Sem cockpit (overlay sem nada): travessoes, sem alerta.
            let mut bare = s.clone();
            bare.overlay = Some(SessionOverlay::default());
            let h = Arc::new(cockpit_history(&vec![Some(SessionOverlay::default()); 4]));
            detail.apply(&snapshot_of(vec![bare]), Some(&h), None);
            detail.history.draw();
            for c in cells {
                assert_eq!(c.readout.label(), DASH);
                assert!(!c.readout.has_css_class("alert"));
            }

            // Recem-aberta: uma amostra, sem loop.
            let h = Arc::new(cockpit_history(&[reading(10.0, 0.0, 1)]));
            detail.apply(&snap, Some(&h), None);
            detail.history.draw();
            assert_eq!(cells[0].readout.label(), "max 10%");
            assert_eq!(cells[2].readout.label(), "max 0.0/min");

            // Multiplexador: sem transcript, sem faixas do transcript.
            let mut mux = session(AgentRole::Multiplexer);
            mux.agent = "herdr".into();
            mux.pid = 7;
            mux.starttime = Some(1);
            let snap = snapshot_of(vec![mux]);
            detail.show(Some(7), Some(&snap), None, None);
            assert!(!detail.history.cockpit_panel.is_visible());
            assert!(!detail.history.cockpit_note.is_visible());
        });
    }

    /// The detail of the typical fixture's busiest session draws four
    /// 10-minute timelines (axis, stall threshold) and one hatched bar per
    /// process, largest memory first; the action menus only exist live.
    #[test]
    fn detail_draws_four_timelines_and_process_bars_from_the_typical_fixture() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let snap = crate::machine_history::tests::fixture("tipico-40");
            let pid = crate::window::pick_detail("busiest", &snap).expect("a session");
            let s = snap.sessions.iter().find(|s| s.pid == pid).unwrap();
            let mut h = History::new(HISTORY_LEN);
            for _ in 0..5 {
                h.record(&snap.sessions, |_| 1);
            }
            let h = Arc::new(h);
            for live in [true, false] {
                let detail = Detail::new(live);
                detail.show(Some(pid), Some(&snap), Some(&h), None);
                detail.history.draw();
                let charts = &detail.history.charts;
                assert_eq!(charts.len(), CHART_TITLES.len());
                for (c, title) in charts.iter().zip(CHART_TITLES) {
                    assert_eq!(c.title.label(), title);
                    assert_eq!(c.timeline.height_request(), crate::timeline::DETAIL_SIZE.1);
                    let (_, axis) = c.timeline.labels();
                    assert_eq!(axis, ["\u{2212}10 min", "\u{2212}5", "now"]);
                }
                assert_ne!(charts[0].value.label(), DASH, "CPU has readings");
                assert!(charts[1].value.label().ends_with("iB"), "memory has a unit");
                assert_eq!(charts[3].timeline.threshold(), Some(STALL_LIMIT_PCT));

                let rows = detail.procs.rows();
                assert_eq!(rows.len(), s.processes.len());
                let largest = s
                    .processes
                    .iter()
                    .max_by_key(|p| p.pss_bytes.unwrap_or(0) + p.swap_pss_bytes.unwrap_or(0))
                    .unwrap();
                assert_eq!(rows[0].pid, largest.pid, "largest memory first");
                let (p, w) = rows[0].bar.fractions().expect("a bar");
                assert!((p + w - 1.0).abs() < 1e-9, "the largest fills the bar");
                assert!(
                    rows.iter()
                        .any(|r| r.bar.fractions().is_some_and(|(_, w)| w > 0.0)),
                    "some process has swapped memory, hatched"
                );
                assert_eq!(
                    rows.iter().all(|r| r.menu.is_some()),
                    live,
                    "per-process actions only live"
                );
                assert_eq!(
                    detail.procs_head.label(),
                    format!("Processes \u{b7} {}", s.processes.len())
                );
            }
        });
    }

    /// The labels existed from birth with the 10-minute text, so a check on
    /// their text passed while the axis stayed hidden: the charts must show it.
    #[test]
    fn detail_charts_show_the_time_axis_and_the_strip_does_not() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let snap = crate::machine_history::tests::fixture("tipico-40");
            let pid = crate::window::pick_detail("busiest", &snap).expect("a session");
            let mut h = History::new(HISTORY_LEN);
            h.record(&snap.sessions, |_| 1);
            let detail = Detail::new(false);
            detail.show(Some(pid), Some(&snap), Some(&Arc::new(h)), None);
            detail.history.draw();
            for c in &detail.history.charts {
                assert!(c.timeline.axis_visible(), "{}", c.title.label());
            }
            let strip = Timeline::new(crate::timeline::STRIP_SIZE.0, crate::timeline::STRIP_SIZE.1);
            assert!(!strip.axis_visible(), "the machine strip stays bare");
        });
    }

    /// The panel shows name and native id as text a person can select and
    /// copy, and the id carries a spoken label (a bare uuid means nothing).
    #[test]
    fn detail_name_and_session_id_are_selectable_and_labelled() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let mut s = session(AgentRole::Agent);
            s.starttime = Some(1);
            s.overlay = Some(SessionOverlay {
                name: Some("refactor sampler".into()),
                agent_session_id: Some(SID.into()),
                ..Default::default()
            });
            let detail = Detail::new(false);
            detail.show(Some(42), Some(&snapshot_of(vec![s])), None, None);
            assert_eq!(detail.name.label(), "refactor sampler");
            assert!(detail.name.is_selectable(), "the name can be copied");
            assert_eq!(detail.session_id.label(), SID);
            assert!(detail.session_id.is_selectable(), "the id can be copied");
            assert!(detail.session_id.is_visible());
            // Copied whole: wrapped, never cut by an ellipsis.
            assert_eq!(
                detail.session_id.ellipsize(),
                gtk::pango::EllipsizeMode::None
            );
            assert!(detail.session_id.wraps());
            // The id is folded under "Session details" since the header
            // redesign: it is part of the panel once that is open.
            detail.more.set_expanded(true);
            assert!(
                detail.session_id.is_ancestor(&detail.page),
                "the id row is part of the panel"
            );
            assert_eq!(
                spoken_label(&detail.session_id, &format!("Session id {SID}")),
                None,
                "the id is announced with its caption"
            );

            // Another session, no id yet: a dash, never the previous id.
            let mut bare = session(AgentRole::Agent);
            bare.pid = 43;
            bare.starttime = Some(1);
            detail.show(Some(43), Some(&snapshot_of(vec![bare])), None, None);
            assert_eq!(detail.name.label(), "kiln");
            assert_eq!(detail.session_id.label(), DASH);
            assert_eq!(
                spoken_label(&detail.session_id, "Session id unavailable"),
                None
            );

            // A multiplexer row has no session id to show.
            let mut mux = session(AgentRole::Multiplexer);
            mux.pid = 44;
            mux.starttime = Some(1);
            detail.show(Some(44), Some(&snapshot_of(vec![mux])), None, None);
            assert!(!detail.session_id.parent().unwrap().is_visible());
        });
    }

    fn described() -> Session {
        let mut s = session(AgentRole::Agent);
        s.cwd = Some("/home/dev/src/atlas".into());
        s.overlay = Some(SessionOverlay {
            model: Some("claude-opus-5-5".into()),
            context_pct: Some(44.0),
            kind: Some("interactive".into()),
            ..Default::default()
        });
        s
    }

    /// The header's one meta line, as the B mock reads it.
    #[test]
    fn header_meta_line_reads_like_the_mock() {
        let t = DetailText::of(&described(), IoStallSource::Delayacct, Some("/home/dev"));
        assert_eq!(
            t.meta_line(),
            "claude \u{b7} claude-opus-5-5 \u{b7} pid 42 \u{b7} ~/src/atlas \u{b7} 0 processes \u{b7} context 44%"
        );
    }

    /// What the transcript has not said is left out, never shown as a dash.
    #[test]
    fn header_meta_line_skips_what_is_not_known() {
        let t = DetailText::of(
            &session(AgentRole::Agent),
            IoStallSource::Delayacct,
            Some("/home/dev"),
        );
        assert_eq!(
            t.meta_line(),
            "claude \u{b7} pid 42 \u{b7} ~/src/kiln \u{b7} 0 processes"
        );
        let mut mux = session(AgentRole::Multiplexer);
        mux.agent = "herdr".into();
        let t = DetailText::of(&mux, IoStallSource::Delayacct, Some("/home/dev"));
        assert!(
            t.meta_line().starts_with("herdr \u{b7} pid 42"),
            "{}",
            t.meta_line()
        );
        assert!(!t.meta_line().contains("context"));
    }

    /// Header: state glyph, 20 px name, state word and one meta line on top;
    /// the focus button sits in the title row, and the long tail (id, facts,
    /// controls) is folded behind an expander that starts closed.
    #[test]
    fn detail_header_has_glyph_name_one_meta_line_and_folded_tail() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Detail::new(false);
            let mut busy = described();
            busy.starttime = Some(1);
            let snap = snapshot_of(vec![busy]);
            detail.show(Some(42), Some(&snap), None, None);
            let text = DetailText::of(&snap.sessions[0], IoStallSource::Delayacct, None);
            assert_eq!(detail.meta.label(), text.meta_line());
            assert!(detail.meta.is_visible());
            assert_eq!(detail.meta.lines(), 1);
            assert_eq!(
                detail.meta.ellipsize(),
                gtk::pango::EllipsizeMode::End,
                "one line, never wrapped"
            );
            assert!(detail.glyph.is_ancestor(&detail.page));
            assert_eq!(
                detail.glyph.parent(),
                detail.name.parent(),
                "glyph and name share the title row"
            );
            assert_eq!(
                detail.focus.parent(),
                detail.name.parent(),
                "the focus button is on the title row"
            );
            assert!(!detail.more.is_expanded(), "the tail starts folded");
            // GTK parents the child of a closed expander only when it opens.
            detail.more.set_expanded(true);
            assert!(detail.session_id.is_ancestor(&detail.more));
            assert!(detail.more.is_ancestor(&detail.page));

            // Glyph shape follows the state, as in the list.
            for (group, shape) in [
                (Group::Busy, Shape::Disc),
                (Group::Blocked, Shape::Triangle),
                (Group::Idle, Shape::Ring),
                (Group::Multiplexer, Shape::Grid),
            ] {
                let mut t = text.clone();
                t.group = group.rank();
                detail.render(&t);
                assert_eq!(detail.glyph.shape(), shape, "{group:?}");
            }
        });
    }

    fn shown_detail(live: bool) -> Detail {
        let detail = Detail::new(live);
        let mut busy = described();
        busy.starttime = Some(1);
        let snap = snapshot_of(vec![busy]);
        detail.show(Some(42), Some(&snap), None, None);
        detail
    }

    /// Index, among the body's children, of the one that holds `w`.
    fn slot(detail: &Detail, w: &impl IsA<gtk::Widget>) -> usize {
        let body = detail
            .name
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let mut child = body.first_child();
        let mut i = 0;
        while let Some(c) = child {
            if w.upcast_ref::<gtk::Widget>() == &c || w.is_ancestor(&c) {
                return i;
            }
            child = c.next_sibling();
            i += 1;
        }
        panic!("not in the detail body");
    }

    fn control_button(detail: &Detail, label: &str) -> gtk::Button {
        descendants(detail.session_controls.widget.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no {label} button in the session controls"))
    }

    /// REVIEW_RR1 order: title, verdict pill and its reason line, Handoff and
    /// Compact, the context and token panel, then charts and processes.
    #[test]
    fn detail_sections_follow_the_review_order() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            let title = detail.name.parent().expect("title row");
            let face = title.parent().expect("header");
            assert!(
                detail.rec.is_ancestor(&title),
                "the verdict pill is on the title row"
            );
            assert!(
                detail.reason.is_ancestor(&face),
                "the reason line sits under the title, in the header"
            );
            let order = [
                slot(&detail, &face),
                slot(&detail, &detail.session_controls.widget),
                slot(&detail, &detail.runs.widget),
                slot(&detail, &detail.transcript_head),
                slot(&detail, &detail.history.cockpit_panel),
                slot(&detail, &detail.history.panel),
                slot(&detail, detail.procs_head.parent().as_ref().unwrap()),
                slot(&detail, &detail.procs.widget),
                slot(&detail, &detail.more),
            ];
            assert!(
                order.windows(2).all(|w| w[0] < w[1]),
                "header < controls < where it runs < context < token strips < charts < processes < IDs: {order:?}"
            );
            assert!(
                !detail.more.is_ancestor(&face),
                "the tail is not in the header"
            );
        });
    }

    #[test]
    fn handoff_and_compact_sit_above_the_context_panel_and_out_of_the_fold() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            for label in ["Handoff…", "Compact…"] {
                let b = control_button(&detail, label);
                assert!(!b.is_ancestor(&detail.more), "{label} is not folded away");
                assert!(
                    slot(&detail, &b) < slot(&detail, &detail.transcript_head),
                    "{label} comes before the context panel"
                );
            }
        });
    }

    /// The page header carries the same two actions as the controls: its
    /// buttons forward the click, so confirmation and discovery stay in one
    /// place.
    #[test]
    fn header_handoff_and_compact_forward_to_the_session_controls() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            let header = descendants(detail.toolbar.upcast_ref())
                .into_iter()
                .find(|w| w.is::<adw::HeaderBar>())
                .expect("the page header");
            for (short, long) in [("Handoff", "Handoff…"), ("Compact", "Compact…")] {
                let copy = descendants(&header)
                    .into_iter()
                    .filter_map(|w| w.downcast::<gtk::Button>().ok())
                    .find(|b| b.label().as_deref() == Some(short))
                    .unwrap_or_else(|| panic!("no {short} in the header"));
                let target = control_button(&detail, long);
                let clicks = Rc::new(Cell::new(0));
                let seen = clicks.clone();
                target.connect_clicked(move |_| seen.set(seen.get() + 1));
                copy.emit_clicked();
                assert_eq!(clicks.get(), 1, "{short} reaches the control");
                assert_eq!(
                    copy.is_sensitive(),
                    target.is_sensitive(),
                    "{short} is on or off with the control"
                );
            }
        });
    }

    /// Pumps the main loop for `ms`, so timeouts and idles run.
    fn pump(ms: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        let context = gtk::glib::MainContext::default();
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn request_clicks_the_control_of_the_list_action() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Rc::new(shown_detail(false));
            for (op, long) in [
                (actions_ui::RowOp::Compact, "Compact…"),
                (actions_ui::RowOp::Handoff, "Handoff…"),
            ] {
                let target = control_button(&detail, long);
                let clicks = Rc::new(Cell::new(0));
                let seen = clicks.clone();
                target.connect_clicked(move |_| seen.set(seen.get() + 1));
                let missed = Rc::new(Cell::new(0));
                let m = missed.clone();
                target.set_sensitive(true);
                detail.request(42, op, move || m.set(m.get() + 1));
                assert_eq!(clicks.get(), 1, "{long} on: clicked at once");
                assert_eq!(missed.get(), 0, "{long} on: no toast");
            }
            let missed = Rc::new(Cell::new(0));
            let m = missed.clone();
            detail.request(42, actions_ui::RowOp::End, move || m.set(m.get() + 1));
            assert_eq!(missed.get(), 1, "End has no control here");
        });
    }

    /// The owner's bug (25/10): the Action click selects the session, whose
    /// control is still rediscovering (hidden, insensitive). The request
    /// waits for it instead of reporting it unavailable.
    #[test]
    fn request_waits_for_the_control_of_a_session_still_discovering() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Rc::new(shown_detail(false));
            let target = control_button(&detail, "Compact…");
            let clicks = Rc::new(Cell::new(0));
            let seen = clicks.clone();
            target.connect_clicked(move |_| seen.set(seen.get() + 1));
            target.set_sensitive(false);
            let missed = Rc::new(Cell::new(0));
            let m = missed.clone();
            detail.request_within(
                42,
                actions_ui::RowOp::Compact,
                std::time::Duration::from_millis(600),
                move || m.set(m.get() + 1),
            );
            assert_eq!(clicks.get(), 0, "not yet: still discovering");
            let t = target.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                t.set_sensitive(true)
            });
            pump(900);
            assert_eq!(clicks.get(), 1, "clicked once the control came up");
            assert_eq!(missed.get(), 0, "no toast");
        });
    }

    #[test]
    fn request_reports_unavailable_when_the_control_never_comes_up() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Rc::new(shown_detail(false));
            let target = control_button(&detail, "Handoff…");
            let clicks = Rc::new(Cell::new(0));
            let seen = clicks.clone();
            target.connect_clicked(move |_| seen.set(seen.get() + 1));
            target.set_sensitive(false);
            let missed = Rc::new(Cell::new(0));
            let m = missed.clone();
            detail.request_within(
                42,
                actions_ui::RowOp::Handoff,
                std::time::Duration::from_millis(300),
                move || m.set(m.get() + 1),
            );
            assert_eq!(missed.get(), 0, "not before the wait ends");
            pump(600);
            assert_eq!((clicks.get(), missed.get()), (0, 1), "one toast, no click");
            target.set_sensitive(true);
            pump(200);
            assert_eq!(clicks.get(), 0, "the wait is over: no late click");
        });
    }

    /// Another session selected while waiting: the request is dropped, never
    /// run on the wrong session.
    #[test]
    fn request_is_dropped_when_another_session_is_shown() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Rc::new(shown_detail(false));
            let target = control_button(&detail, "Compact…");
            let clicks = Rc::new(Cell::new(0));
            let seen = clicks.clone();
            target.connect_clicked(move |_| seen.set(seen.get() + 1));
            target.set_sensitive(false);
            let missed = Rc::new(Cell::new(0));
            let m = missed.clone();
            detail.request_within(
                42,
                actions_ui::RowOp::Compact,
                std::time::Duration::from_millis(600),
                move || m.set(m.get() + 1),
            );
            detail.pid.set(Some(43));
            target.set_sensitive(true);
            pump(800);
            assert_eq!((clicks.get(), missed.get()), (0, 0), "dropped silently");
        });
    }

    /// The row clicked is not the one shown yet (the selection reaches the
    /// detail in an idle): the shown session's ready control is never
    /// pressed; the request runs once the detail shows the row's session.
    #[test]
    fn request_waits_for_the_detail_to_show_the_rows_session() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Rc::new(shown_detail(false));
            let target = control_button(&detail, "Compact…");
            let clicks = Rc::new(Cell::new(0));
            let seen = clicks.clone();
            target.connect_clicked(move |_| seen.set(seen.get() + 1));
            target.set_sensitive(true);
            let missed = Rc::new(Cell::new(0));
            let m = missed.clone();
            detail.request_within(
                43,
                actions_ui::RowOp::Compact,
                std::time::Duration::from_millis(600),
                move || m.set(m.get() + 1),
            );
            pump(150);
            assert_eq!(clicks.get(), 0, "42's control is not 43's");
            detail.pid.set(Some(43));
            pump(200);
            assert_eq!(
                (clicks.get(), missed.get()),
                (1, 0),
                "43 shown: clicked once"
            );
        });
    }

    /// Two clicks (or Compact then Handoff) while waiting: one request, the
    /// newer.
    #[test]
    fn a_newer_request_replaces_the_pending_one() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Rc::new(shown_detail(false));
            let compact = control_button(&detail, "Compact…");
            let handoff = control_button(&detail, "Handoff…");
            let clicks = Rc::new(RefCell::new(Vec::new()));
            for (b, name) in [(&compact, "compact"), (&handoff, "handoff")] {
                let seen = clicks.clone();
                b.connect_clicked(move |_| seen.borrow_mut().push(name));
                b.set_sensitive(false);
            }
            let wait = std::time::Duration::from_millis(600);
            detail.request_within(42, actions_ui::RowOp::Compact, wait, || {});
            detail.request_within(42, actions_ui::RowOp::Compact, wait, || {});
            detail.request_within(42, actions_ui::RowOp::Handoff, wait, || {});
            compact.set_sensitive(true);
            handoff.set_sensitive(true);
            pump(300);
            assert_eq!(*clicks.borrow(), ["handoff"], "only the newest runs");
        });
    }

    fn herdr_session() -> Session {
        let snap: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/uma.json")).unwrap();
        snap.sessions[0].clone()
    }

    #[test]
    fn where_it_runs_names_agent_model_herdr_pane_and_folder() {
        let s = herdr_session();
        let t = DetailText::of(&s, IoStallSource::Delayacct, Some("/home/dev"));
        let [agent, model, terminal, folder] = &t.runs;
        assert_eq!(agent.value, "claude");
        assert_eq!(model.value, "claude-haiku-5");
        assert!(terminal.value.starts_with("herdr "), "{}", terminal.value);
        assert!(
            terminal.value.contains(&t.herdr["herdr ".len()..]),
            "the pane the header names: {}",
            terminal.value
        );
        assert_eq!(folder.value, "~/src/atlas");
        assert!(
            !terminal.tooltip.is_empty(),
            "workspace, tab and pane in the tooltip"
        );
    }

    #[test]
    fn where_it_runs_says_terminal_unknown_for_a_bare_session() {
        let mut s = herdr_session();
        s.overlay.as_mut().unwrap().herdr = None;
        s.overlay.as_mut().unwrap().model = None;
        let t = DetailText::of(&s, IoStallSource::Delayacct, Some("/home/dev"));
        let [agent, model, terminal, folder] = &t.runs;
        assert_eq!(agent.value, "claude");
        assert_eq!(model.value, DASH, "no model yet is a dash, never blank");
        assert_eq!(terminal.value, "terminal unknown");
        assert_eq!(folder.value, "~/src/atlas");
    }

    #[test]
    fn where_it_runs_rows_are_drawn_between_the_controls_and_the_context() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = Detail::new(false);
            let mut s = herdr_session();
            s.overlay.as_mut().unwrap().herdr = None;
            let mut snap = snapshot_of(vec![s.clone()]);
            snap.sessions[0].starttime = Some(1);
            detail.show(Some(s.pid), Some(&snap), None, Some("/home/dev"));
            let texts: Vec<String> = descendants(detail.runs.widget.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .map(|l| l.label().to_string())
                .collect();
            for want in ["Agent", "Model", "Terminal", "Folder", "terminal unknown"] {
                assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
            }
            assert!(
                slot(&detail, &detail.session_controls.widget) < slot(&detail, &detail.runs.widget)
                    && slot(&detail, &detail.runs.widget) < slot(&detail, &detail.transcript_head),
            );
        });
    }

    /// The fold keeps identifiers only: the session id and the pid. Folder,
    /// terminal, facts, reason and controls live in the open.
    #[test]
    fn session_details_expander_keeps_only_ids() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            detail.more.set_expanded(true);
            assert!(detail.session_id.is_ancestor(&detail.more));
            assert!(detail.pid_value.is_ancestor(&detail.more));
            assert_eq!(detail.pid_value.label(), "42");
            let tail = detail.more.child().expect("the folded content");
            for (name, w) in [
                ("runs", detail.runs.widget.upcast_ref::<gtk::Widget>()),
                ("kernel facts", detail.session.widget.upcast_ref()),
                ("reason", detail.reason.upcast_ref()),
                ("controls", detail.session_controls.widget.upcast_ref()),
            ] {
                assert!(!w.is_ancestor(&tail), "{name} is not an ID");
            }
        });
    }

    /// One action area: the title row's menu holds Suspend, Resume, Terminate
    /// and Kill; no second row of session buttons follows the processes.
    #[test]
    fn one_action_area_the_menu_holds_suspend_resume_terminate_kill() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let live = shown_detail(true);
            let stray: Vec<String> = descendants(live.history.panel.parent().unwrap().upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .filter_map(|b| b.label())
                .map(|l| l.to_string())
                .filter(|l| ["Suspend…", "Resume…", "End…", "Kill…"].contains(&l.as_str()))
                .collect();
            assert!(stray.is_empty(), "no loose action buttons: {stray:?}");
            // The button builds this same menu when it opens (a popup needs a
            // rooted window, which the headless detail has not).
            assert_eq!(live.actions.parent(), live.name.parent());
            assert!(live.actions.is_visible() && live.actions.is_sensitive());
            let model = actions_ui::menu(Target::Session(42), "atlas");
            let mut labels = Vec::new();
            for i in 0..model.n_items() {
                if let Some(section) = model.item_link(i, "section") {
                    for j in 0..section.n_items() {
                        labels.extend(
                            section
                                .item_attribute_value(j, "label", None)
                                .and_then(|v| v.get::<String>()),
                        );
                    }
                }
            }
            assert_eq!(labels, ["Suspend…", "Resume…", "End…", "Kill…"]);
        });
    }

    /// The note shares the processes heading's row, one short line.
    #[test]
    fn processes_heading_has_one_short_note() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            let note = detail
                .procs_head
                .next_sibling()
                .and_then(|w| w.downcast::<gtk::Label>().ok());
            let note = note.expect("a note beside the heading");
            assert!(note.label().chars().count() < 60, "{}", note.label());
            assert!(!note.wraps());
        });
    }

    /// The line under the header exists only with something to say: the
    /// reason the button is off (also its tooltip) or an action's feedback.
    #[test]
    fn focus_feedback_line_shows_only_with_feedback() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            let tip = detail.focus.tooltip_text().expect("why it is off");
            assert!(tip.contains("fixtures"), "{tip}");
            assert!(detail.focus_reason.is_visible(), "off: the reason shows");
            detail.set_focus_state(None);
            assert!(!detail.focus_reason.is_visible(), "on, nothing to say yet");
            detail.show_focus_feedback("Terminal pane focused.");
            assert!(detail.focus_reason.is_visible());
            detail.show_focus_feedback("");
            assert!(!detail.focus_reason.is_visible());
        });
    }

    /// The state pill sits against the name (the mock), and a recommendation
    /// pill with nothing to recommend is not drawn as a lone dash.
    #[test]
    fn state_pill_hugs_the_name_and_an_empty_rec_pill_hides() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let detail = shown_detail(false);
            assert!(
                !detail.name.hexpands(),
                "the name does not push the pill away"
            );
            assert_eq!(detail.rec.label(), DASH);
            assert!(!detail.rec.is_visible());
            assert!(detail.state.is_visible());
        });
    }

    /// `None` when the widget's accessible label is `expected`; otherwise
    /// GTK's own mismatch message. gtk4-rs has no reader for the value.
    fn spoken_label(w: &impl IsA<gtk::Accessible>, expected: &str) -> Option<String> {
        use gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
        let expected = std::ffi::CString::new(expected).unwrap();
        // SAFETY: a live widget, a NUL-terminated string for the Label
        // property's one variadic argument; GTK returns NULL or an owned string.
        let msg = unsafe {
            gtk::ffi::gtk_test_accessible_check_property(
                w.as_ref().to_glib_none().0,
                gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
                expected.as_ptr(),
            )
        };
        (!msg.is_null()).then(|| unsafe { gtk::glib::GString::from_glib_full(msg) }.to_string())
    }
}
