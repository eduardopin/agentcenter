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
//! A recomendacao do cockpit e so a pilula ao lado do estado (decisao do
//! dono, docs/DESIGN.md secao 8); o texto dela vem da mesma [`Row`] da lista.
//! Valor ausente e travessao, nunca 0. Nenhum widget nasce por tick: o
//! detalhe troca rotulos (so os que mudaram) e sincroniza a tabela.

use crate::actions_ui;
use crate::format::{DASH, Group, Row, bytes, human, provider_name, rec_class, sanitize};
use crate::graph::{Strip, nice_max, peak};
use crate::process_table::ProcessTable;
use adw::prelude::*;
use agentcenter_core::actions::Target;
use agentcenter_core::control::herdr::{Endpoint, Target as FocusTarget};
use agentcenter_core::{HISTORY_LEN, History, IoStallSource, Session, SessionHistory, Snapshot};
use std::cell::{Cell, RefCell};
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

/// Todo o texto do detalhe de uma sessao, sem GTK: testavel sem display.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DetailText {
    pub pid: u32,
    pub name: String,
    pub group: u32,
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
    pub session: [Fact; 5],
    /// Vazio no multiplexador (nao tem transcript).
    pub transcript: Option<[Fact; 5]>,
    pub a11y: String,
}

fn n_or_dash(v: Option<u64>) -> String {
    v.map_or_else(|| DASH.to_string(), |n| n.to_string())
}

fn rate(v: Option<f64>) -> String {
    v.map_or_else(|| DASH.to_string(), |b| format!("{}/s", bytes(b)))
}

impl DetailText {
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
            status: row.status,
            rec: row.rec,
            rec_text: row.rec_text,
            rec_tooltip: row.rec_tooltip,
            identity,
            cwd: row.cwd,
            herdr: herdr_text,
            herdr_tooltip,
            session,
            transcript,
            a11y,
        }
    }
}

/// Legendas das faixas de historico, na ordem de [`StripData::of`].
pub const HISTORY_STRIPS: [&str; 4] = ["CPU", "Memory", "Disk", "Stall"];
/// Series por faixa (a segunda e tracejada): memoria = PSS + swap, disco =
/// leitura + escrita.
const HISTORY_SERIES: [usize; 4] = [1, 2, 2, 1];
/// Limite do stall com delay accounting, o mesmo 10 % da pressao (DESIGN.md
/// secao 4): acima dele a leitura fica carmim e o filete marca o limite.
pub const STALL_LIMIT_PCT: f64 = 10.0;
const MIB: f64 = 1024.0 * 1024.0;

/// Uma faixa de historico pronta para desenhar, sem GTK.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StripData {
    pub series: Vec<Vec<f32>>,
    /// Topo da escala.
    pub max: f64,
    pub cap: usize,
    /// Limite na escala (stall com delay accounting).
    pub limit: Option<f64>,
    /// A ultima leitura passou do limite.
    pub alert: bool,
    /// `max 140%`, ou travessao sem leitura.
    pub readout: String,
    pub tooltip: String,
    /// Rotulo do leitor de tela: legenda, janela, maximo, agora e escala.
    pub a11y: String,
}

fn last(v: &[f32]) -> Option<f64> {
    v.last().filter(|x| x.is_finite()).map(|x| f64::from(*x))
}

/// `Last 10 minutes`: o ring guarda [`HISTORY_LEN`] ticks do intervalo dado.
pub fn span_text(interval_ms: u64) -> String {
    let ms = if interval_ms == 0 { 1000 } else { interval_ms };
    let secs = HISTORY_LEN as u64 * ms / 1000;
    if secs >= 120 {
        format!("Last {} minutes", (secs + 30) / 60)
    } else {
        format!("Last {secs} seconds")
    }
}

impl StripData {
    /// As quatro faixas de uma sessao; sem historico, trilhos vazios e
    /// travessao (nunca 0).
    pub fn of(h: Option<&SessionHistory>, source: IoStallSource, span: &str) -> [StripData; 4] {
        let span = span.to_lowercase();
        let Some(h) = h.filter(|h| !h.is_empty()) else {
            return HISTORY_STRIPS.map(|cap| StripData {
                readout: DASH.to_string(),
                tooltip: "No history for this session yet".to_string(),
                a11y: format!("{cap}, {span}: no history yet"),
                max: 1.0,
                cap: HISTORY_LEN,
                ..Default::default()
            });
        };
        let cap = h.cpu_pct.capacity();
        let make = |caption: &str,
                    series: Vec<Vec<f32>>,
                    floor: f64,
                    binary: bool,
                    fmt: &dyn Fn(f64) -> String,
                    names: &[&str]| {
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
                limit: None,
                alert: false,
            }
        };
        let pct = |v: f64| format!("{v:.0}%");
        let size = |v: f64| bytes(v);
        let rate = |v: f64| format!("{}/s", bytes(v));
        let cpu = make(
            "CPU",
            vec![h.cpu_pct.to_vec()],
            100.0,
            false,
            &pct,
            &["CPU, % of one core"],
        );
        let mem = make(
            "Memory",
            vec![h.pss_bytes.to_vec(), h.swap_pss_bytes.to_vec()],
            MIB,
            true,
            &size,
            &["PSS (solid)", "swap PSS (dashed)"],
        );
        let disk = make(
            "Disk",
            vec![h.read_bps.to_vec(), h.write_bps.to_vec()],
            MIB,
            true,
            &rate,
            &["read (solid)", "write (dashed)"],
        );
        let stall_series = vec![h.stall.to_vec()];
        let stall = match source {
            IoStallSource::Delayacct => {
                let now = last(&stall_series[0]);
                let mut s = make(
                    "Stall",
                    stall_series,
                    2.0 * STALL_LIMIT_PCT,
                    false,
                    &pct,
                    &["longest block I/O wait, % of the interval"],
                );
                s.limit = Some(STALL_LIMIT_PCT);
                s.alert = now.is_some_and(|n| n >= STALL_LIMIT_PCT);
                s
            }
            IoStallSource::DState => make(
                "Stall",
                stall_series,
                4.0,
                false,
                &|v: f64| format!("D:{v:.0}"),
                &["threads waiting on disk (estimated)"],
            ),
        };
        [cpu, mem, disk, stall]
    }
}

fn set_label(l: &gtk::Label, text: &str) {
    if l.label() != text {
        l.set_label(text);
    }
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

/// Secao "Last 10 minutes": quatro faixas num `AdwWrapBox` (quatro numa linha
/// no detalhe largo, duas por linha no medio e no estreito). Fora da tela
/// (detalhe recolhido atras da lista) nada e calculado; ao aparecer, desenha
/// o ultimo historico guardado.
struct HistoryView {
    head: gtk::Label,
    note: gtk::Label,
    panel: adw::WrapBox,
    cells: Vec<(gtk::Box, gtk::Label, Strip)>,
    pending: RefCell<Option<Pending>>,
}

impl HistoryView {
    fn new() -> Rc<Self> {
        let head = label(&["detail-section"]);
        head.set_accessible_role(gtk::AccessibleRole::Heading);
        let note = label(&["detail-note"]);
        note.set_wrap(true);
        note.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        let panel = adw::WrapBox::builder()
            .child_spacing(16)
            .line_spacing(10)
            .justify(adw::JustifyMode::Fill)
            .build();
        panel.add_css_class("detail-history");
        let cells = HISTORY_STRIPS
            .iter()
            .zip(HISTORY_SERIES)
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
                let strip = Strip::new(series);
                let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
                cell.add_css_class("history-cell");
                cell.append(&top);
                cell.append(&strip);
                panel.append(&cell);
                (cell, readout, strip)
            })
            .collect();
        let view = Rc::new(Self {
            head,
            note,
            panel,
            cells,
            pending: RefCell::default(),
        });
        let v = Rc::downgrade(&view);
        view.panel.connect_map(move |_| {
            if let Some(v) = v.upgrade() {
                v.draw();
            }
        });
        view
    }

    fn set(&self, p: Pending) {
        self.pending.replace(Some(p));
        if self.panel.is_mapped() {
            self.draw();
        }
    }

    fn draw(&self) {
        let pending = self.pending.borrow();
        let Some(p) = pending.as_ref() else { return };
        let span = span_text(p.interval_ms);
        set_label(&self.head, &span);
        let every = match p.interval_ms {
            0 | 1000 => "One reading per second".to_string(),
            ms => format!("One reading every {:.1} s", ms as f64 / 1000.0),
        };
        set_label(
            &self.note,
            &format!(
                "{every}, newest on the right. Solid: CPU, PSS, disk read, stall; \
                 dashed: swap, disk write."
            ),
        );
        let h = p.history.as_ref().and_then(|h| h.get(p.pid));
        let data = StripData::of(h, p.source, &span);
        for ((cell, readout, strip), d) in self.cells.iter().zip(&data) {
            let series: Vec<&[f32]> = d.series.iter().map(Vec::as_slice).collect();
            strip.set(&series, d.max, d.cap, d.limit);
            set_label(readout, &d.readout);
            set_tip(cell, &d.tooltip);
            set_classes(
                readout,
                if d.alert {
                    &["history-readout", "numeric", "alert"]
                } else {
                    &["history-readout", "numeric"]
                },
            );
            set_classes(strip, if d.alert { &["alert"] } else { &[] });
            strip.update_property(&[gtk::accessible::Property::Label(&d.a11y)]);
        }
    }
}

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
    shown: RefCell<Option<DetailText>>,
    icon: gtk::Image,
    name: gtk::Label,
    state: gtk::Label,
    rec: gtk::Label,
    reason: gtk::Label,
    identity: gtk::Label,
    cwd: gtk::Label,
    herdr: gtk::Label,
    session: Facts,
    transcript_head: gtk::Label,
    transcript: Facts,
    procs_head: gtk::Label,
    history: Rc<HistoryView>,
    table: RefCell<ProcessTable>,
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

        let icon = gtk::Image::new();
        icon.set_pixel_size(20);
        icon.set_valign(gtk::Align::Center);
        icon.set_accessible_role(gtk::AccessibleRole::Presentation);
        let name = label(&["detail-name"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_hexpand(true);
        name.set_accessible_role(gtk::AccessibleRole::Heading);
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
        title.append(&icon);
        title.append(&name);
        title.append(&state);
        title.append(&rec);
        title.append(&actions);

        let identity = label(&["detail-meta"]);
        identity.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let cwd = label(&["detail-meta", "monospace"]);
        cwd.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        cwd.set_hexpand(true);
        let herdr = label(&["detail-meta", "numeric"]);
        let ident = adw::WrapBox::builder()
            .child_spacing(16)
            .line_spacing(2)
            .build();
        ident.append(&identity);
        ident.append(&cwd);
        ident.append(&herdr);

        let session = Facts::new(&SESSION_FACTS);
        let face = gtk::Box::new(gtk::Orientation::Vertical, 6);
        face.add_css_class("detail-face");
        face.append(&title);
        face.append(&ident);
        let reason = label(&["detail-note"]);
        reason.set_wrap(true);
        reason.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        reason.set_selectable(true);
        face.append(&reason);
        face.append(&session.widget);
        let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let focus = gtk::Button::with_label("Focus terminal");
        focus.set_halign(gtk::Align::Start);
        focus.set_size_request(44, 44);
        focus.set_sensitive(false);
        let focus_reason = label(&["detail-note"]);
        focus_reason.set_wrap(true);
        focus_reason.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        focus_reason.set_selectable(true);
        controls.append(&focus);
        controls.append(&focus_reason);
        let session_controls = actions_ui::session_control::SessionControls::new(live);
        controls.append(&session_controls.widget);
        face.append(&controls);
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
            feedback.set_label("Checking terminal identity…");
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
                    reason.set_label(&match result {
                        Ok(()) => {
                            "Terminal pane focused (not desktop window activation).".to_string()
                        }
                        Err(e) => format!(
                            "Terminal focus not confirmed: {e:?}. Refresh and retry explicitly."
                        ),
                    });
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
        let note = label(&["detail-note"]);
        note.set_label(
            "PSS splits shared pages and adds up to the session; RSS counts them in \
             every process, so it is per process and never summed.",
        );
        note.set_wrap(true);
        note.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        let table = ProcessTable::new();
        table.widget.add_css_class("detail-table");
        // O painel tem raio: as linhas sao recortadas nele.
        table.widget.set_overflow(gtk::Overflow::Hidden);
        table.widget.set_min_content_height(180);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("session-detail");
        body.append(&face);
        body.append(&history.head);
        body.append(&history.note);
        body.append(&history.panel);
        body.append(&transcript_head);
        body.append(&transcript.widget);
        body.append(&procs_head);
        body.append(&note);
        body.append(&table.widget);

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
        let bps = table.breakpoints();
        if let Some(medium) = bps.last() {
            medium.add_setter(
                &body,
                "css-classes",
                Some(&["session-detail", "narrow"].to_value()),
            );
        }
        for bp in bps {
            bin.add_breakpoint(bp);
        }

        let stack = gtk::Stack::new();
        stack.add_named(&none, Some(STACK_NONE));
        stack.add_named(&ended, Some(STACK_ENDED));
        stack.add_named(&bin, Some(STACK_SESSION));
        stack.set_visible_child_name(STACK_NONE);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
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
            shown: RefCell::new(None),
            icon,
            name,
            state,
            rec,
            reason,
            identity,
            cwd,
            herdr,
            session,
            transcript_head,
            transcript,
            procs_head,
            history,
            table: RefCell::new(table),
            target,
            actions,
        }
    }

    /// Troca a sessao mostrada e ja desenha com o ultimo snapshot.
    pub fn show(
        &self,
        pid: Option<u32>,
        snap: Option<&Snapshot>,
        history: Option<&Arc<History>>,
        home: Option<&str>,
    ) {
        if self.pid.replace(pid) != pid {
            self.focus_reason.set_label("");
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
            self.table.borrow_mut().clear();
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
                self.table.borrow_mut().clear();
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
                if !self.focusing.get() && self.focus_reason.label().is_empty() {
                    self.focus_reason
                        .set_label("Focus the explicitly identified herdr pane.");
                }
            }
            Err(reason) => {
                self.focus_target.replace(None);
                self.focus.set_sensitive(false);
                self.focus_reason.set_label(reason);
            }
        }
        self.history.set(Pending {
            history: history.cloned(),
            pid,
            source,
            interval_ms: snap.interval_ms,
        });
        let mut table = self.table.borrow_mut();
        table.apply(s, source);
        let head = match table.len() {
            1 => "Processes · 1".to_string(),
            n => format!("Processes · {n}"),
        };
        set_label(&self.procs_head, &head);
        self.set_page(STACK_SESSION);
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
        let icon = if mux {
            "view-grid-symbolic"
        } else {
            "utilities-terminal-symbolic"
        };
        if self.icon.icon_name().as_deref() != Some(icon) {
            self.icon.set_icon_name(Some(icon));
        }
        let tone = match g {
            Group::Busy => "busy",
            Group::Blocked => "blocked",
            _ => "quiet",
        };
        set_classes(&self.icon, &["session-icon", tone]);
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
        self.rec.set_visible(!mux);
        // O core recusa o servidor herdr: sem menu que so falharia.
        self.actions.set_visible(!mux);
        self.target.replace(Some((t.pid, t.name.clone())));

        set_label(&self.identity, &t.identity);
        set_label(&self.cwd, &t.cwd);
        set_tip(&self.cwd, &t.cwd);
        set_label(&self.herdr, &t.herdr);
        set_tip(&self.herdr, &t.herdr_tooltip);
        self.herdr.set_visible(!t.herdr.is_empty());

        self.session.set(&t.session, &SESSION_FACTS);
        self.transcript_head.set_visible(t.transcript.is_some());
        self.transcript.widget.set_visible(t.transcript.is_some());
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

    fn history_of(s: &Session, samples: usize) -> History {
        let mut h = History::new(HISTORY_LEN);
        for _ in 0..samples {
            h.record(std::slice::from_ref(s), |_| 1);
        }
        h
    }

    #[test]
    fn no_history_draws_empty_tracks_and_dashes() {
        let data = StripData::of(None, IoStallSource::Delayacct, "Last 10 minutes");
        for d in &data {
            assert_eq!(d.readout, DASH);
            assert!(d.series.is_empty());
            assert!(
                d.a11y.contains("last 10 minutes: no history yet"),
                "{}",
                d.a11y
            );
        }
    }

    #[test]
    fn strips_scale_to_the_peak_and_flag_the_stall_limit() {
        let mut s = session(AgentRole::Agent);
        s.cpu_pct = Some(140.0);
        s.pss_bytes = Some(704 * 1024 * 1024);
        s.swap_pss_bytes = Some(10 * 1024 * 1024);
        s.stall_pct = Some(12.0);
        let h = history_of(&s, 3);
        let data = StripData::of(h.get(42), IoStallSource::Delayacct, "Last 10 minutes");
        let [cpu, mem, disk, stall] = &data;
        assert_eq!((cpu.max, cpu.readout.as_str()), (200.0, "max 140%"));
        assert_eq!(cpu.series[0].len(), 3);
        assert_eq!(mem.max, 1024.0 * 1024.0 * 1024.0);
        assert_eq!(mem.readout, "max 704 MiB");
        assert!(
            mem.tooltip.contains("swap PSS (dashed): max 10 MiB"),
            "{}",
            mem.tooltip
        );
        // Sem leitura de disco: trilho e travessao, nunca 0.
        assert_eq!(disk.readout, DASH);
        assert_eq!(stall.limit, Some(STALL_LIMIT_PCT));
        assert!(stall.alert, "12% is over the 10% limit");
        assert!(stall.a11y.starts_with("Stall, last 10 minutes: "));

        s.threads_d = Some(3);
        s.stall_pct = None;
        let h = history_of(&s, 2);
        let [.., stall] = StripData::of(h.get(42), IoStallSource::DState, "Last 10 minutes");
        assert_eq!(
            (stall.readout.as_str(), stall.limit, stall.alert),
            ("max D:3", None, false)
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
}
