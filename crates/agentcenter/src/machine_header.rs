//! Faixa da maquina (layout B de docs/DESIGN.md): tres reguas, CPU, RAM e
//! swap, cada uma dividida em agentes, resto da maquina e livre, com leitura
//! e capacidade em texto; embaixo, uma faixa de contadores que quebra linha
//! com a largura (sessoes por agente e por estado, atividade paralela,
//! I/O-bound, tool calls, subagentes, pressao e o cockpit).
//!
//! So leitura: nada aqui recebe foco. Os widgets sao todos criados em
//! [`MachineHeader::new`]; [`MachineHeader::apply`] so troca texto, classe e
//! a parte cheia das reguas, e so quando o valor muda. O texto sai de
//! [`Model::of`], funcao pura testada sem display.
//!
//! Valor sem fonte (tool calls e subagentes sem transcript, o cockpit sem
//! nenhuma sessao com as contas) sai como [`ABSENT`] com tooltip dizendo que
//! nao ha fonte: nunca 0. O cockpit (C21) vem de `summary.cockpit*`: as
//! sessoes por recomendacao somam `summary.sessions` na faixa.

// A regua mora em gauge.rs, ao lado; declarada aqui (e nao em main.rs) porque
// so a faixa a usa.
#[path = "gauge.rs"]
pub mod gauge;

use crate::format::{ABSENT, Group, REC_HANDOFF, REC_NONE, REC_PENDING, human, used_of};
use crate::session_list::SessionFilter;
use adw::prelude::*;
use agentcenter_core::{Machine, PsiResource, Snapshot, Summary};
use gauge::{Fill, Rule, bytes_in_unit_of, pct0, pct1, used_and_capacity};

/// PSI `some avg10` a partir do qual a leitura fica em carmim e a regua de
/// pressao marca o limite: um decimo dos ultimos 10 s com alguma tarefa
/// parada esperando o recurso. Escolha de interface, nao limite do kernel.
pub const PRESSURE_WARN_PCT: f64 = 10.0;

/// Abaixo desta largura da grade de reguas (a janela menos margem e recuo do
/// painel, 56 px), a faixa empilha e aperta: menos ar, leitura menor, os
/// grupos de contadores um embaixo do outro e quebrando por dentro. Nada some,
/// so a coluna "agents N" das reguas (a fatia fica no segmento e no valor
/// falado). Na janela: abaixo de ~600 sp.
pub const NARROW: &str = "max-width: 540sp";

/// Tooltip de valor que a fonte ainda nao publica para este agente.
pub const NO_SOURCE: &str = "No source for this agent yet";
/// Tooltip do bloco do cockpit sem nenhuma sessao com as contas.
pub const NO_COCKPIT_SOURCE: &str = "No session has cockpit data yet";
/// Contexto (% da janela) em que o cockpit manda fazer o handoff: o padrao de
/// `SQUAD_CTX_PCT`, a segunda marca da regua de contexto maximo.
pub const CTX_HANDOFF_PCT: f64 = 90.0;

/// Cor de um texto da faixa (classe CSS de summary.css).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    /// Secundario: zero ou contexto.
    Dim,
    /// Ambar: trabalho em andamento.
    Busy,
    /// Carmim: precisa de voce.
    Alert,
}

/// Um contador da faixa.
#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    pub text: String,
    pub tone: Tone,
    /// Tooltip quando o valor nao tem fonte; `None` com valor.
    pub absent: Option<&'static str>,
    pub visible: bool,
    /// Rotulo para o leitor de tela quando o texto curto nao basta (numero
    /// e estado por extenso); `None` le o proprio texto.
    pub spoken: Option<String>,
}

impl Counter {
    fn plain(text: String, tone: Tone) -> Self {
        Counter {
            text,
            tone,
            absent: None,
            visible: true,
            spoken: None,
        }
    }

    fn absent(what: &str, why: &'static str) -> Self {
        Counter {
            text: format!("{ABSENT} {what}"),
            tone: Tone::Dim,
            absent: Some(why),
            visible: true,
            spoken: Some(format!("{what}: no source")),
        }
    }

    /// Sessoes numa recomendacao do cockpit: `1 handoff` na tela,
    /// `1 session recommended for handoff, needs you` para o leitor.
    fn rec(n: usize, what: &str, tone: Tone, meaning: &str) -> Self {
        let needs = if tone == Tone::Alert && n > 0 {
            ", needs you"
        } else {
            ""
        };
        Counter {
            spoken: Some(format!(
                "{} {meaning}{needs}",
                plural(n, "session", "sessions")
            )),
            ..Counter::count(n, what, tone)
        }
    }

    /// `n what`, em `tone` se `n > 0`, secundario no zero.
    fn count(n: usize, what: &str, tone: Tone) -> Self {
        Counter::plain(format!("{n} {what}"), if n > 0 { tone } else { Tone::Dim })
    }
}

/// Uma regua e o texto dela.
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub fill: Option<Fill>,
    pub alert: bool,
    /// Leitura, em negrito: `29.3`, `42%`, ou [`ABSENT`].
    pub value: String,
    /// Capacidade, secundaria: `of 38.0 GiB`, `of 16 cores`.
    pub capacity: String,
    /// Fatia dos agentes: `agents 20.0 GiB`; vazio onde nao ha fatia.
    pub agents: String,
    /// Valor para o leitor de tela: leitura, capacidade, fatia, limite.
    pub spoken: String,
    /// Tooltip quando a leitura nao tem fonte.
    pub absent: Option<&'static str>,
}

impl Gauge {
    fn absent(capacity: String, why: &'static str) -> Self {
        Gauge {
            fill: None,
            alert: false,
            value: ABSENT.into(),
            capacity,
            agents: String::new(),
            spoken: "no reading".into(),
            absent: Some(why),
        }
    }
}

/// Contadores na ordem dos rotulos da faixa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Sessions,
    ByAgent,
    Busy,
    Blocked,
    Idle,
    Unknown,
    Active,
    IoBound,
    ToolsOpen,
    ToolsPerMin,
    Subagents,
    Handoff,
    Loop,
    Compact,
    CockpitIdle,
    CockpitOk,
    Reading,
    NoData,
    Burn,
}

pub const SLOTS: usize = Slot::Burn as usize + 1;

fn slot_filter(slot: Slot) -> Option<SessionFilter> {
    Some(match slot {
        Slot::Sessions => SessionFilter::All,
        Slot::Busy => SessionFilter::Group(Group::Busy),
        Slot::Blocked => SessionFilter::Group(Group::Blocked),
        Slot::Idle => SessionFilter::Group(Group::Idle),
        Slot::Unknown => SessionFilter::Group(Group::Unknown),
        Slot::IoBound => SessionFilter::IoBound,
        Slot::Handoff => SessionFilter::Recommendation(REC_HANDOFF),
        Slot::Loop => SessionFilter::Recommendation(6),
        Slot::Compact => SessionFilter::Recommendation(3),
        Slot::CockpitIdle => SessionFilter::Recommendation(5),
        Slot::CockpitOk => SessionFilter::Recommendation(2),
        Slot::Reading => SessionFilter::Recommendation(REC_PENDING),
        Slot::NoData => SessionFilter::Recommendation(REC_NONE),
        _ => return None,
    })
}

/// Os slots na ordem do enum, para andar junto com os contadores.
pub const SLOT_ORDER: [Slot; SLOTS] = [
    Slot::Sessions,
    Slot::ByAgent,
    Slot::Busy,
    Slot::Blocked,
    Slot::Idle,
    Slot::Unknown,
    Slot::Active,
    Slot::IoBound,
    Slot::ToolsOpen,
    Slot::ToolsPerMin,
    Slot::Subagents,
    Slot::Handoff,
    Slot::Loop,
    Slot::Compact,
    Slot::CockpitIdle,
    Slot::CockpitOk,
    Slot::Reading,
    Slot::NoData,
    Slot::Burn,
];

/// Reguas grandes, na ordem da grade.
pub const RULES: [(&str, &str); 3] = [
    (
        "CPU",
        "Whole machine, all cores. Amber: the CPU of the session processes. \
         Source: kernel stat, per process and for the machine.",
    ),
    (
        "RAM",
        "In use: total minus available. Amber: the PSS of the session processes. \
         Source: kernel meminfo and smaps_rollup.",
    ),
    (
        "Swap",
        "In use: total minus free. Amber: the swap PSS of the session processes. \
         Source: kernel meminfo and smaps_rollup.",
    ),
];

/// Reguas pequenas da faixa: pressao (com fonte) e o cockpit (sem fonte
/// ainda). Rotulo, tooltip e marcas de limite em fracao.
pub const MINI: [(&str, &str, &[f64]); 4] = [
    (
        "memory",
        "Memory pressure: share of the last 10 s in which some task waited for \
         memory (PSI some avg10). The mark is the 10 % limit.",
        &[PRESSURE_WARN_PCT / 100.0],
    ),
    (
        "I/O",
        "I/O pressure: share of the last 10 s in which some task waited for disk \
         (PSI some avg10). The mark is the 10 % limit.",
        &[PRESSURE_WARN_PCT / 100.0],
    ),
    (
        "cache hit",
        "Prompt cache hit of the sessions with cockpit data: cache reads over \
         all input tokens, weighted by tokens.",
        &[],
    ),
    (
        "context max",
        "Fullest context window among the sessions. Marks at 50 % (compact) \
         and 90 % (handoff).",
        &[0.5, 0.9],
    ),
];

/// Tudo o que a faixa mostra num tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub rules: [Gauge; 3],
    pub mini: [Gauge; 4],
    pub counters: [Counter; SLOTS],
}

/// Tooltip fixo de cada contador com valor (o de ausencia vale por cima).
pub fn slot_tip(s: Slot) -> Option<&'static str> {
    Some(match s {
        Slot::Active => {
            "Child processes of the sessions that are running or waiting on disk \
             (R or D), or using at least 1 % of a core, against the cores of the machine."
        }
        Slot::IoBound => "Sessions flagged I/O-bound: waiting on disk more than computing.",
        Slot::Handoff => {
            "Sessions the cockpit sends to a handoff (pill HANDOFF): context over the \
             limit, or rereading more than it is worth."
        }
        Slot::Loop => "Sessions repeating the same call (pill LOOP).",
        Slot::Compact => "Sessions the cockpit asks to compact (pill COMPACTAR).",
        Slot::CockpitIdle => "Sessions with no activity for more than 90 min (pill OCIOSA).",
        Slot::CockpitOk => "Sessions that can go on as they are (pill OK).",
        Slot::Reading => {
            "Sessions whose transcript is still being read: the cockpit comes after \
             the first full pass."
        }
        Slot::NoData => {
            "Sessions without cockpit data: no transcript matched, or no request \
             counted in it."
        }
        Slot::Burn => {
            "Weighted burn of the sessions with cockpit data: input + 1.25 x cache \
             write + 0.1 x cache read + 5 x output tokens."
        }
        _ => return None,
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn cpu(m: &Machine, s: &Summary) -> Gauge {
    let cores = m.cores.max(1);
    let capacity = format!("of {}", plural(cores as usize, "core", "cores"));
    let Some(used) = m.cpu_pct else {
        return Gauge::absent(capacity, NO_SOURCE);
    };
    // sessions_cpu_pct e % de um nucleo somado; a regua e % da maquina.
    let agents = s.sessions_cpu_pct.map(|c| c / f64::from(cores));
    let fill = Fill::of(Some(used), agents, Some(100.0));
    let agents_text = match (agents, fill) {
        (Some(_), Some(f)) => format!("agents {}", pct0(f.agents * 100.0)),
        _ => format!("agents {ABSENT}"),
    };
    Gauge {
        fill,
        alert: false,
        value: pct0(used),
        spoken: format!("{} {capacity} used, {agents_text}", pct0(used)),
        capacity,
        agents: agents_text,
        absent: None,
    }
}

fn bytes_gauge(used: Option<u64>, total: Option<u64>, agents: Option<u64>, none: &str) -> Gauge {
    match (used, total) {
        (_, Some(0)) => Gauge {
            fill: None,
            alert: false,
            value: none.into(),
            capacity: String::new(),
            agents: String::new(),
            spoken: none.into(),
            absent: None,
        },
        (Some(u), Some(t)) => {
            let (value, capacity) = used_and_capacity(u, t);
            let agents_text = match agents {
                Some(a) => format!("agents {}", bytes_in_unit_of(a.min(u), t)),
                None => format!("agents {ABSENT}"),
            };
            Gauge {
                fill: Fill::of(Some(u as f64), agents.map(|a| a as f64), Some(t as f64)),
                alert: false,
                spoken: format!("{} used, {agents_text}", used_of(u, t)),
                value,
                capacity,
                agents: agents_text,
                absent: None,
            }
        }
        _ => Gauge::absent(String::new(), NO_SOURCE),
    }
}

fn pressure(r: Option<PsiResource>) -> Gauge {
    let Some(p) = r.and_then(|r| r.some_avg10) else {
        return Gauge::absent(String::new(), "No pressure source (PSI) on this kernel");
    };
    let alert = p >= PRESSURE_WARN_PCT;
    Gauge {
        fill: Fill::single(Some(p), 100.0),
        alert,
        value: pct1(p),
        capacity: String::new(),
        agents: String::new(),
        spoken: format!(
            "{} of the last 10 s, {} the {} limit",
            pct1(p),
            if alert { "over" } else { "under" },
            pct0(PRESSURE_WARN_PCT)
        ),
        absent: None,
    }
}

/// Cache hit agregado (0..=1, 3 casas) como regua de uma leitura so.
fn cache_hit(v: Option<f64>) -> Gauge {
    let Some(c) = v else {
        return Gauge::absent(String::new(), NO_COCKPIT_SOURCE);
    };
    let pct = c * 100.0;
    Gauge {
        fill: Fill::single(Some(pct), 100.0),
        alert: false,
        value: pct1(pct),
        capacity: String::new(),
        agents: String::new(),
        spoken: format!(
            "{} of the input tokens read from the prompt cache",
            pct1(pct)
        ),
        absent: None,
    }
}

/// Contexto mais cheio; carmim a partir da marca do handoff.
fn ctx_max(v: Option<f64>) -> Gauge {
    let Some(p) = v else {
        return Gauge::absent(String::new(), NO_COCKPIT_SOURCE);
    };
    let alert = p >= CTX_HANDOFF_PCT;
    Gauge {
        fill: Fill::single(Some(p), 100.0),
        alert,
        value: pct1(p),
        capacity: String::new(),
        agents: String::new(),
        spoken: format!(
            "{} of the window in the fullest session, {} the {} handoff mark",
            pct1(p),
            if alert { "over" } else { "under" },
            pct0(CTX_HANDOFF_PCT)
        ),
        absent: None,
    }
}

/// `claude 31 · codex 5 · pi 2`: mais sessoes primeiro, empate por nome.
fn by_agent(s: &Summary) -> String {
    let mut v: Vec<_> = s.by_agent.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

impl Model {
    pub fn of(snap: &Snapshot) -> Self {
        let (m, s) = (&snap.machine, &snap.summary);
        let mem_used = m
            .mem_total_bytes
            .zip(m.mem_available_bytes)
            .map(|(t, a)| t.saturating_sub(a));
        let swap_used = m
            .swap_total_bytes
            .zip(m.swap_free_bytes)
            .map(|(t, f)| t.saturating_sub(f));
        let psi = m.psi.as_ref();

        let sessions = match s.sessions {
            0 => "No sessions".to_string(),
            n => plural(n, "session", "sessions"),
        };
        let agents = by_agent(s);
        let st = &s.status;
        let k = &s.cockpit;
        // Sem dado e sem varredura em curso: a varredura pendente ja e "reading".
        let no_data = k.sem_dado.saturating_sub(s.cockpit_pending);
        let counters = [
            Counter::plain(sessions, Tone::Plain),
            Counter {
                visible: !agents.is_empty(),
                ..Counter::plain(agents, Tone::Dim)
            },
            Counter::count(st.busy, "busy", Tone::Busy),
            Counter::count(st.blocked, "blocked", Tone::Alert),
            Counter::count(st.idle, "idle", Tone::Plain),
            Counter {
                visible: st.unknown > 0,
                ..Counter::count(st.unknown, "unknown", Tone::Plain)
            },
            Counter::plain(
                format!(
                    "{} active of {}",
                    s.working_child_processes,
                    plural(m.cores as usize, "core", "cores")
                ),
                Tone::Plain,
            ),
            Counter::count(s.io_bound_sessions, "I/O-bound", Tone::Alert),
            match s.tool_calls_open {
                Some(n) => Counter::plain(format!("{n} in flight"), Tone::Plain),
                None => Counter::absent("in flight", NO_SOURCE),
            },
            match s.tool_calls_per_min {
                Some(r) => Counter::plain(format!("{r:.0} per min"), Tone::Plain),
                None => Counter::absent("per min", NO_SOURCE),
            },
            match s.subagents_live {
                Some(n) => Counter::plain(format!("{n} live"), Tone::Plain),
                None => Counter::absent("live", NO_SOURCE),
            },
            // Cockpit (C21): as sessoes por recomendacao, na ordem de urgencia.
            // Carmim so no que precisa de voce; OK fica neutro. Os visiveis
            // somam `sessions` (loop e sem dado somem no zero).
            Counter::rec(k.handoff, "handoff", Tone::Alert, "recommended for handoff"),
            Counter {
                visible: k.loop_ > 0,
                ..Counter::rec(k.loop_, "loop", Tone::Alert, "in a loop")
            },
            Counter::rec(
                k.compactar,
                "compact",
                Tone::Alert,
                "recommended to compact",
            ),
            Counter::rec(k.ociosa, "idle", Tone::Plain, "idle for the cockpit"),
            Counter::rec(k.continuar, "ok", Tone::Plain, "fine to continue"),
            Counter::rec(
                s.cockpit_pending,
                "reading",
                Tone::Dim,
                "still reading the transcript",
            ),
            Counter {
                visible: no_data > 0,
                ..Counter::rec(no_data, "no data", Tone::Dim, "without cockpit data")
            },
            match s.sessions_burn_weighted {
                Some(b) => Counter {
                    spoken: Some(format!("weighted burn {}", human(b))),
                    ..Counter::plain(format!("burn {}", human(b)), Tone::Plain)
                },
                None => Counter::absent("burn", NO_COCKPIT_SOURCE),
            },
        ];
        Model {
            rules: [
                cpu(m, s),
                bytes_gauge(mem_used, m.mem_total_bytes, s.sessions_pss_bytes, "none"),
                bytes_gauge(
                    swap_used,
                    m.swap_total_bytes,
                    s.sessions_swap_pss_bytes,
                    "No swap",
                ),
            ],
            mini: [
                pressure(psi.and_then(|p| p.memory)),
                pressure(psi.and_then(|p| p.io)),
                cache_hit(s.cache_hit),
                ctx_max(s.ctx_pct_max),
            ],
            counters,
        }
    }
}

// ---- Widgets ----------------------------------------------------------------

fn set_text(l: &gtk::Label, s: &str) {
    if l.label() != s {
        l.set_label(s);
    }
}

fn set_tone(l: &gtk::Label, tone: Tone) {
    for (class, on) in [
        ("dim", tone == Tone::Dim),
        ("busy", tone == Tone::Busy),
        ("alert", tone == Tone::Alert),
    ] {
        if l.has_css_class(class) != on {
            if on {
                l.add_css_class(class);
            } else {
                l.remove_css_class(class);
            }
        }
    }
}

/// Tooltip de ausencia por cima do tooltip fixo do widget.
fn set_tip(w: &impl IsA<gtk::Widget>, absent: Option<&str>, fixed: Option<&str>) {
    let want = absent.or(fixed);
    if w.tooltip_text().as_deref() != want {
        w.set_tooltip_text(want);
    }
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().xalign(0.0).build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

struct RuleRow {
    rule: Rule,
    value: gtk::Label,
    capacity: gtk::Label,
    agents: Option<gtk::Label>,
    tip: &'static str,
}

impl RuleRow {
    fn apply(&self, g: &Gauge) {
        self.rule.set(g.fill, g.alert);
        self.rule.set_value_text(&g.spoken);
        set_text(&self.value, &g.value);
        set_tone(&self.value, if g.alert { Tone::Alert } else { Tone::Plain });
        set_tip(&self.value, g.absent, Some(self.tip));
        set_text(&self.capacity, &g.capacity);
        if let Some(a) = &self.agents {
            set_text(a, &g.agents);
            // Sem fatia medida, o texto fica secundario: ambar e so trabalho.
            let absent = g.agents.ends_with(ABSENT);
            set_tone(a, if absent { Tone::Dim } else { Tone::Busy });
        }
    }
}

pub struct MachineHeader {
    pub widget: gtk::Box,
    rules: Vec<RuleRow>,
    mini: Vec<RuleRow>,
    counters: Vec<gtk::Label>,
    counter_buttons: Vec<Option<gtk::Button>>,
}

impl Default for MachineHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineHeader {
    pub fn new() -> Self {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
        panel.add_css_class("machine-header");
        // O painel nao recebe foco; os filtros abaixo sao botoes nativos.
        panel.set_focusable(false);

        // Reguas grandes numa grade: rotulos, leituras e capacidades
        // alinhados em coluna, como num painel de instrumentos.
        let grid = gtk::Grid::builder()
            .column_spacing(12)
            .row_spacing(6)
            .build();
        // Na janela estreita nada some da faixa: ela empilha e aperta. So a
        // coluna "agents N" das reguas sai, porque nao cabe numa linha de
        // 360 px; a fatia continua no segmento ambar e no valor falado da
        // regua (`Gauge::spoken`), que o leitor de tela le.
        let mut narrow_hide: Vec<gtk::Widget> = Vec::new();
        let mut rules = Vec::new();
        for (row, (name, tip)) in RULES.iter().enumerate() {
            let row = row as i32;
            let caption = label(&["gauge-label"]);
            caption.set_label(name);
            caption.set_tooltip_text(Some(tip));
            let rule = Rule::new(name, &[]);
            rule.set_hexpand(true);
            rule.set_valign(gtk::Align::Center);
            rule.set_tooltip_text(Some(tip));
            let value = label(&["readout", "numeric"]);
            value.set_xalign(1.0);
            // Largura fixa em caracteres: o numero que muda nao empurra a
            // coluna a cada tick.
            value.set_width_chars(5);
            let capacity = label(&["capacity", "numeric"]);
            capacity.set_width_chars(11);
            let agents = label(&["agents", "numeric"]);
            agents.set_width_chars(15);
            agents.set_tooltip_text(Some(tip));
            grid.attach(&caption, 0, row, 1, 1);
            grid.attach(&rule, 1, row, 1, 1);
            grid.attach(&value, 2, row, 1, 1);
            grid.attach(&capacity, 3, row, 1, 1);
            grid.attach(&agents, 4, row, 1, 1);
            narrow_hide.push(agents.clone().upcast());
            rules.push(RuleRow {
                rule,
                value,
                capacity,
                agents: Some(agents),
                tip,
            });
        }
        // O breakpoint mora num AdwBreakpointBin so em volta da grade, com
        // minimo em em no CSS para acompanhar a fonte: o bin nao negocia
        // altura por largura, e a faixa de
        // contadores (que quebra linha) cortaria embaixo. Nao vai na janela:
        // o libadwaita aplica so o ultimo breakpoint que vale, e os da lista
        // (session_list.rs) deixariam de valer.
        let bin = adw::BreakpointBin::builder()
            .child(&grid)
            .width_request(240)
            // libadwaita requires the explicit property as well as the
            // scalable CSS minimum; CSS alone leaves first layout undefined.
            .height_request(84)
            .build();
        bin.add_css_class("machine-rules");
        panel.append(&bin);

        let strip = adw::WrapBox::builder()
            .child_spacing(24)
            .line_spacing(6)
            .build();
        let counters: Vec<gtk::Label> =
            (0..SLOTS).map(|_| label(&["counter", "numeric"])).collect();
        let counter_buttons: Vec<Option<gtk::Button>> = SLOT_ORDER
            .iter()
            .map(|slot| {
                slot_filter(*slot).map(|_| {
                    let button = gtk::Button::builder()
                        .child(&counters[*slot as usize])
                        .build();
                    button.add_css_class("flat");
                    button.add_css_class("counter-filter");
                    button
                })
            })
            .collect();
        let c = |s: Slot| -> gtk::Widget {
            counter_buttons[s as usize].as_ref().map_or_else(
                || counters[s as usize].clone().upcast(),
                |b| b.clone().upcast(),
            )
        };
        let mut mini = Vec::new();
        let mut mini_item = |i: usize| -> gtk::Box {
            let (name, tip, marks) = MINI[i];
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let caption = label(&["counter", "dim"]);
            caption.set_label(name);
            let rule = Rule::new(name, marks);
            rule.add_css_class("mini");
            rule.set_valign(gtk::Align::Center);
            let value = label(&["counter", "numeric"]);
            for w in [caption.upcast_ref::<gtk::Widget>(), rule.upcast_ref()] {
                w.set_tooltip_text(Some(tip));
            }
            b.append(&caption);
            b.append(&rule);
            b.append(&value);
            mini.push(RuleRow {
                rule,
                value,
                capacity: gtk::Label::new(None),
                agents: None,
                tip,
            });
            b
        };
        // Grupos: legenda secundaria e os contadores. A faixa quebra entre
        // grupos; um grupo mais largo que a janela (o cockpit em 360 px)
        // quebra dentro dele, com a legenda sempre colada no primeiro item.
        let mut groups: Vec<adw::WrapBox> = Vec::new();
        let mut group = |caption: Option<&str>, items: &[gtk::Widget]| -> adw::WrapBox {
            let b = adw::WrapBox::builder()
                .child_spacing(10)
                .line_spacing(2)
                .build();
            b.add_css_class("counter-group");
            let mut items = items.iter();
            if let Some(t) = caption {
                let l = label(&["group-label"]);
                l.set_label(t);
                let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                head.append(&l);
                if let Some(first) = items.next() {
                    head.append(first);
                }
                b.append(&head);
            }
            for w in items {
                b.append(w);
            }
            groups.push(b.clone());
            b
        };
        let w = |widget: gtk::Widget| widget;
        // O rotulo por agente some sem sessao (visibilidade do dado), por
        // isso o embrulho.
        let by_agent = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        by_agent.append(&c(Slot::ByAgent));

        let sessions = group(None, &[w(c(Slot::Sessions)), by_agent.upcast()]);
        let states = group(
            None,
            &[
                w(c(Slot::Busy)),
                w(c(Slot::Blocked)),
                w(c(Slot::Idle)),
                w(c(Slot::Unknown)),
            ],
        );
        let active = group(None, &[w(c(Slot::Active)), w(c(Slot::IoBound))]);
        let tools = group(
            Some("Tool calls"),
            &[w(c(Slot::ToolsOpen)), w(c(Slot::ToolsPerMin))],
        );
        let subagents = group(Some("Subagents"), &[w(c(Slot::Subagents))]);
        let pressure = group(
            Some("Pressure"),
            &[mini_item(0).upcast(), mini_item(1).upcast()],
        );
        let cockpit = group(
            Some("Cockpit"),
            &[
                w(c(Slot::Handoff)),
                w(c(Slot::Loop)),
                w(c(Slot::Compact)),
                w(c(Slot::CockpitIdle)),
                w(c(Slot::CockpitOk)),
                w(c(Slot::Reading)),
                w(c(Slot::NoData)),
                w(c(Slot::Burn)),
                mini_item(2).upcast(),
                mini_item(3).upcast(),
            ],
        );
        for g in [
            &sessions, &states, &active, &pressure, &tools, &subagents, &cockpit,
        ] {
            strip.append(g);
        }
        panel.append(&strip);

        let bp = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(NARROW).expect("valid breakpoint condition"),
        );
        for w in &narrow_hide {
            bp.add_setter(w, "visible", Some(&false.to_value()));
        }
        // Estreita: menos ar entre linhas e grupos, leitura menor (summary.css,
        // `.machine-header.narrow`).
        bp.add_setter(
            &panel,
            "css-classes",
            Some(&["machine-header", "narrow"].to_value()),
        );
        bp.add_setter(&panel, "spacing", Some(&6i32.to_value()));
        bp.add_setter(&grid, "row-spacing", Some(&2i32.to_value()));
        bp.add_setter(&grid, "column-spacing", Some(&8i32.to_value()));
        bp.add_setter(&strip, "child-spacing", Some(&14i32.to_value()));
        bp.add_setter(&strip, "line-spacing", Some(&2i32.to_value()));
        for g in &groups {
            bp.add_setter(g, "child-spacing", Some(&8i32.to_value()));
        }
        bin.add_breakpoint(bp);

        Self {
            widget: panel,
            rules,
            mini,
            counters,
            counter_buttons,
        }
    }

    pub fn connect_filter(&self, f: impl Fn(SessionFilter) + 'static) {
        let f = std::rc::Rc::new(f);
        for slot in SLOT_ORDER {
            if let (Some(button), Some(target)) =
                (&self.counter_buttons[slot as usize], slot_filter(slot))
            {
                let f = f.clone();
                button.connect_clicked(move |_| f(target));
            }
        }
    }

    /// Aplica o frame. So o que mudou toca o widget.
    pub fn apply(&self, snap: &Snapshot) {
        let model = Model::of(snap);
        for (row, g) in self.rules.iter().zip(&model.rules) {
            row.apply(g);
        }
        for (row, g) in self.mini.iter().zip(&model.mini) {
            row.apply(g);
        }
        for ((l, c), slot) in self.counters.iter().zip(&model.counters).zip(SLOT_ORDER) {
            // O rotulo falado deriva do texto: so muda quando o texto muda.
            if l.label() != c.text.as_str() {
                l.set_label(&c.text);
                let spoken = c.spoken.as_deref().unwrap_or(&c.text);
                l.update_property(&[gtk::accessible::Property::Label(spoken)]);
            }
            set_tone(l, c.tone);
            if l.is_visible() != c.visible {
                l.set_visible(c.visible);
            }
            set_tip(l, c.absent, slot_tip(slot));
            if let Some(button) = &self.counter_buttons[slot as usize] {
                button.set_visible(c.visible);
                let spoken = c.spoken.as_deref().unwrap_or(&c.text);
                button.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{spoken}. Show matching sessions"
                ))]);
                set_tip(button, c.absent, slot_tip(slot));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::{IoStallSource, Psi};

    fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
    }

    fn text(m: &Model, s: Slot) -> &str {
        &m.counters[s as usize].text
    }

    #[test]
    fn typical_fixture_reads_like_its_summary() {
        // jq '.machine, .summary' tipico-40.json
        let m = Model::of(&fixture("tipico-40"));
        let [cpu, ram, swap] = &m.rules;
        assert_eq!(
            (cpu.value.as_str(), cpu.capacity.as_str()),
            ("42%", "of 16 cores")
        );
        // 618.34 % de um nucleo / 16 nucleos = 38.6 % da maquina.
        assert_eq!(cpu.agents, "agents 39%");
        assert_eq!(
            (ram.value.as_str(), ram.capacity.as_str()),
            ("29.3", "of 38.0 GiB")
        );
        assert_eq!(ram.agents, "agents 20.0 GiB");
        assert_eq!(
            (swap.value.as_str(), swap.capacity.as_str()),
            ("19.7", "of 27.0 GiB")
        );
        assert_eq!(swap.agents, "agents 8.6 GiB");
        let f = ram.fill.expect("ram fill");
        assert!((f.agents - 21_484_273_664.0 / 40_802_189_312.0).abs() < 1e-9);
        assert!((f.total() - 31_460_635_444.0 / 40_802_189_312.0).abs() < 1e-9);

        assert_eq!(text(&m, Slot::Sessions), "38 sessions");
        assert_eq!(text(&m, Slot::ByAgent), "claude 31 · codex 5 · pi 2");
        assert_eq!(text(&m, Slot::Busy), "9 busy");
        assert_eq!(text(&m, Slot::Blocked), "3 blocked");
        assert_eq!(text(&m, Slot::Idle), "26 idle");
        assert!(!m.counters[Slot::Unknown as usize].visible);
        assert_eq!(text(&m, Slot::Active), "80 active of 16 cores");
        assert_eq!(text(&m, Slot::IoBound), "3 I/O-bound");
        assert_eq!(m.counters[Slot::Blocked as usize].tone, Tone::Alert);
        assert_eq!(m.counters[Slot::Busy as usize].tone, Tone::Busy);

        let [mem, io, ..] = &m.mini;
        assert_eq!((mem.value.as_str(), mem.alert), ("12.5%", true));
        assert_eq!((io.value.as_str(), io.alert), ("3.1%", false));
    }

    #[test]
    fn sources_not_published_yet_are_dashes_with_a_tooltip() {
        let m = Model::of(&fixture("tipico-40"));
        for s in [Slot::ToolsOpen, Slot::ToolsPerMin, Slot::Subagents] {
            let c = &m.counters[s as usize];
            assert!(c.text.starts_with(ABSENT), "{s:?}: {}", c.text);
            assert_eq!(c.absent, Some(NO_SOURCE), "{s:?}");
            assert!(!c.text.contains('0'), "{s:?} is never zero: {}", c.text);
        }
        // tipico-40 nao tem cockpit: burn e as reguas do cockpit em travessao.
        let burn = &m.counters[Slot::Burn as usize];
        assert_eq!(
            (burn.text.as_str(), burn.absent),
            ("- burn", Some(NO_COCKPIT_SOURCE))
        );
        let cache = &m.mini[2];
        assert_eq!((cache.value.as_str(), cache.fill), (ABSENT, None));
        // O contexto maximo vem de overlay.context_pct, nao do cockpit.
        let ctx = &m.mini[3];
        assert_eq!((ctx.value.as_str(), ctx.alert), ("96.4%", true));
        assert_eq!(text(&m, Slot::Reading), "5 reading");
        assert_eq!(text(&m, Slot::NoData), "33 no data");
    }

    #[test]
    fn cockpit_fixture_reads_like_its_summary() {
        // jq '.summary.cockpit, .summary.cockpit_pending,
        //     .summary.sessions_burn_weighted, .summary.cache_hit,
        //     .summary.ctx_pct_max' cockpit.json
        // {continuar 1, compactar 1, handoff 1, ociosa 1, loop 1, sem_dado 2},
        // 1, 12962500, 0.982, 77.4
        let m = Model::of(&fixture("cockpit"));
        assert_eq!(text(&m, Slot::Handoff), "1 handoff");
        assert_eq!(text(&m, Slot::Loop), "1 loop");
        assert_eq!(text(&m, Slot::Compact), "1 compact");
        assert_eq!(text(&m, Slot::CockpitIdle), "1 idle");
        assert_eq!(text(&m, Slot::CockpitOk), "1 ok");
        assert_eq!(text(&m, Slot::Reading), "1 reading");
        assert_eq!(text(&m, Slot::NoData), "1 no data");
        assert_eq!(text(&m, Slot::Burn), "burn 13.0M");
        for s in [Slot::Handoff, Slot::Loop, Slot::Compact] {
            assert_eq!(m.counters[s as usize].tone, Tone::Alert, "{s:?}");
        }
        // Continuar fica neutro; pendente e sem dado, secundarios.
        assert_eq!(m.counters[Slot::CockpitOk as usize].tone, Tone::Plain);
        assert_eq!(m.counters[Slot::Reading as usize].tone, Tone::Dim);
        // Os contadores visiveis do cockpit somam as sessoes.
        let sum: usize = [
            Slot::Handoff,
            Slot::Loop,
            Slot::Compact,
            Slot::CockpitIdle,
            Slot::CockpitOk,
            Slot::Reading,
            Slot::NoData,
        ]
        .iter()
        .map(|s| &m.counters[*s as usize])
        .filter(|c| c.visible)
        .map(|c| c.text.split(' ').next().unwrap().parse::<usize>().unwrap())
        .sum();
        assert_eq!(sum, 7);
        let [_, _, cache, ctx] = &m.mini;
        assert_eq!((cache.value.as_str(), cache.alert), ("98.2%", false));
        assert!((cache.fill.expect("cache").total() - 0.982).abs() < 1e-9);
        assert_eq!((ctx.value.as_str(), ctx.alert), ("77.4%", false));
        assert!(
            ctx.spoken.contains("under the 90% handoff mark"),
            "{}",
            ctx.spoken
        );
    }

    #[test]
    fn handoff_counter_speaks_number_and_state() {
        let m = Model::of(&fixture("cockpit"));
        assert_eq!(
            m.counters[Slot::Handoff as usize].spoken.as_deref(),
            Some("1 session recommended for handoff, needs you")
        );
        assert_eq!(
            m.counters[Slot::CockpitOk as usize].spoken.as_deref(),
            Some("1 session fine to continue")
        );
        let mut snap = fixture("cockpit");
        snap.summary.cockpit.handoff = 0;
        snap.summary.ctx_pct_max = Some(91.0);
        let m = Model::of(&snap);
        let h = &m.counters[Slot::Handoff as usize];
        assert_eq!((h.text.as_str(), h.tone), ("0 handoff", Tone::Dim));
        assert_eq!(
            h.spoken.as_deref(),
            Some("0 sessions recommended for handoff")
        );
        assert!(m.mini[3].alert, "context max over the handoff mark");
    }

    #[test]
    fn slot_order_follows_the_enum() {
        for (i, s) in SLOT_ORDER.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
    }

    #[test]
    fn published_tool_calls_and_subagents_are_counted() {
        let mut snap = fixture("uma");
        snap.summary.tool_calls_open = Some(7);
        snap.summary.tool_calls_per_min = Some(41.6);
        snap.summary.subagents_live = Some(3);
        let m = Model::of(&snap);
        assert_eq!(text(&m, Slot::ToolsOpen), "7 in flight");
        assert_eq!(text(&m, Slot::ToolsPerMin), "42 per min");
        assert_eq!(text(&m, Slot::Subagents), "3 live");
        assert_eq!(m.counters[Slot::ToolsOpen as usize].absent, None);
        assert_eq!(text(&m, Slot::Sessions), "1 session");
    }

    #[test]
    fn empty_machine_has_rules_without_agents_share() {
        let m = Model::of(&fixture("vazio"));
        assert_eq!(text(&m, Slot::Sessions), "No sessions");
        assert!(!m.counters[Slot::ByAgent as usize].visible);
        assert_eq!(text(&m, Slot::Busy), "0 busy");
        assert_eq!(m.counters[Slot::Busy as usize].tone, Tone::Dim);
        let ram = &m.rules[1];
        assert_eq!(ram.agents, format!("agents {ABSENT}"));
        assert_eq!(ram.fill.expect("ram").agents, 0.0);
    }

    fn machine() -> Machine {
        Machine {
            cores: 16,
            cpu_pct: Some(52.4),
            io_stall_source: IoStallSource::Delayacct,
            processes: 850,
            session_processes: 300,
            mem_total_bytes: Some(38 << 30),
            mem_available_bytes: Some(19 << 30),
            swap_total_bytes: Some(27 << 30),
            swap_free_bytes: Some(3 << 30),
            load_avg: None,
            psi: Some(Psi {
                cpu: None,
                memory: Some(PsiResource {
                    some_avg10: Some(12.5),
                    ..PsiResource::default()
                }),
                io: None,
            }),
        }
    }

    #[test]
    fn missing_machine_sources_read_as_dash_and_no_swap_as_no_swap() {
        let mut snap = fixture("vazio");
        snap.machine = machine();
        snap.machine.cpu_pct = None;
        snap.machine.mem_available_bytes = None;
        snap.machine.swap_total_bytes = Some(0);
        let m = Model::of(&snap);
        assert_eq!((m.rules[0].value.as_str(), m.rules[0].fill), (ABSENT, None));
        assert_eq!(m.rules[0].absent, Some(NO_SOURCE));
        assert_eq!((m.rules[1].value.as_str(), m.rules[1].fill), (ABSENT, None));
        assert_eq!(
            (m.rules[2].value.as_str(), m.rules[2].fill),
            ("No swap", None)
        );
        assert_eq!(m.mini[1].value, ABSENT);
        assert!(!m.mini[1].alert);
    }

    #[test]
    fn same_snapshot_gives_the_same_model() {
        // apply() so toca o widget quando o modelo muda: o modo fixture
        // repete o mesmo snapshot e nada pode redesenhar.
        let snap = fixture("tipico-40");
        assert_eq!(Model::of(&snap), Model::of(&snap));
    }

    #[test]
    fn spoken_value_carries_reading_capacity_and_share() {
        let m = Model::of(&fixture("tipico-40"));
        assert_eq!(m.rules[1].spoken, "29.3 of 38.0 GiB used, agents 20.0 GiB");
        assert_eq!(
            m.mini[0].spoken,
            "12.5% of the last 10 s, over the 10% limit"
        );
    }
}
