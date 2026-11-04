//! Lista de sessoes: `gio::ListStore` de [`SessionObject`] -> filtro por tipo
//! de agente -> ordenacao com secoes -> `GtkListView` de linhas ricas
//! ([`session_row`]). O modelo e sincronizado por pid a cada tick: objeto
//! existente e atualizado, sessao nova entra, sessao que sumiu sai. Nenhuma
//! linha e recriada para mudar numero.
//!
//! Secoes: na ordem "State" (a padrao, layout B do docs/DESIGN.md), uma por
//! estado, bloqueadas primeiro, e os multiplexadores no fim; nas outras ordens,
//! sessoes de agente e multiplexadores, cada um ordenado pela chave. A ordem e
//! o filtro sao funcoes puras ([`order_within`], [`section`], [`shows`]) que o
//! `CustomSorter`/`CustomFilter` do GTK chama: o teste exercita o mesmo codigo.
//!
//! O `SortListModel` nao observa propriedades: a lista so e reordenada quando
//! alguma sessao mudou o valor de uma chave da ordem atual. Linha que so
//! mudou outro campo recebe apenas o `notify` do proprio campo.
//!
//! Medido no C10 (cairo, 39 sessoes, 2 x 20 s, thread principal): reordenar
//! tudo custa 0,10-0,15 % de um nucleo; tirar e repor so as linhas fora do
//! lugar custa 0,65-0,70 %, porque remover e inserir religa a linha, enquanto
//! o resort do `SortListModel` emite so a faixa que mudou e reaproveita os
//! widgets. Por isso o resort e inteiro, mas so quando a chave mexeu.

#[path = "reorder.rs"]
mod reorder;
#[path = "session_row.rs"]
mod session_row;

use crate::format::{Group, REC_HANDOFF, Row};
use crate::session_object::{Changed, SessionObject};
use adw::prelude::*;
use agentcenter_core::Snapshot;
use gtk::gio;
use session_row::{Density, Metric, RowMetric};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

/// O modelo, sem widget: testavel sem display.
pub struct SessionModel {
    store: gio::ListStore,
    by_pid: HashMap<u32, SessionObject>,
    identities: HashMap<u32, (Option<u64>, Option<String>)>,
}

impl Default for SessionModel {
    fn default() -> Self {
        Self {
            store: gio::ListStore::new::<SessionObject>(),
            by_pid: HashMap::new(),
            identities: HashMap::new(),
        }
    }
}

impl SessionModel {
    pub fn store(&self) -> &gio::ListStore {
        &self.store
    }

    pub fn len(&self) -> usize {
        self.by_pid.len()
    }

    /// Aplica o snapshot. Sessao nova entra no fim do store (o `SortListModel`
    /// a poe no lugar); a que sumiu sai. Devolve as atualizadas e o que mudou
    /// em cada uma.
    pub fn apply(&mut self, snap: &Snapshot, home: Option<&str>) -> Applied {
        let source = snap.machine.io_stall_source;
        let identities: HashMap<_, _> = snap
            .sessions
            .iter()
            .map(|s| {
                (
                    s.pid,
                    (
                        s.starttime,
                        s.overlay.as_ref().and_then(|o| o.agent_session_id.clone()),
                    ),
                )
            })
            .collect();
        let live: HashSet<u32> = identities.keys().copied().collect();
        let mut out = Applied::default();

        if self.identities != identities {
            // De tras para frente: remover nao desloca o que falta visitar.
            for i in (0..self.store.n_items()).rev() {
                let Some(obj) = self.store.item(i).and_downcast::<SessionObject>() else {
                    continue;
                };
                if !live.contains(&obj.pid())
                    || self.identities.get(&obj.pid()) != identities.get(&obj.pid())
                {
                    self.store.remove(i);
                    self.by_pid.remove(&obj.pid());
                    out.removed += 1;
                }
            }
        }

        self.identities = identities;
        let mut new = Vec::new();
        for s in &snap.sessions {
            let row =
                Row::from_session_in(s, source, home, &crate::format::Scale::of(&snap.machine));
            match self.by_pid.get(&s.pid) {
                Some(obj) => {
                    let c = obj.apply(&row);
                    if !c.is_empty() {
                        out.updated.push((obj.clone(), c));
                    }
                }
                None => {
                    let obj = SessionObject::new(&row);
                    obj.set_starttime(s.starttime.map_or(-1, |t| t as i64));
                    self.by_pid.insert(s.pid, obj.clone());
                    new.push(obj);
                }
            }
        }
        out.added = new.len();
        if !new.is_empty() {
            // Um items-changed so para todas as novas.
            self.store.splice(self.store.n_items(), 0, &new);
        }
        out
    }
}

/// O que um [`SessionModel::apply`] mexeu.
#[derive(Default)]
pub struct Applied {
    pub added: usize,
    pub removed: usize,
    /// Sessoes ja existentes com algum campo diferente, e quais campos.
    pub updated: Vec<(SessionObject, Changed)>,
}

impl Applied {
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.updated.is_empty()
    }

    /// Alguma atualizada mudou uma chave de `keys` (a ordem pode ter mudado).
    /// Sessao que entra ou sai nao conta: o `SortListModel` ja a poe no lugar.
    pub fn touches(&self, keys: Changed) -> bool {
        self.updated.iter().any(|(_, c)| c.intersects(keys))
    }
}

/// Ordem da lista, escolhida no menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Secoes por estado, memoria dentro de cada uma.
    State,
    Memory,
    Cpu,
    Name,
    /// HANDOFF primeiro, depois contexto e burn, maior primeiro (contexto.py).
    Recommendation,
    /// Weighted tokens of the last 30 minutes, then memory (REVIEW_RR1). The
    /// default of `sort-order`: one flat list, the biggest spender first.
    #[default]
    Spend,
    /// Weighted tokens since the session started.
    SinceStart,
    /// Context window use, fullest first.
    Context,
}

impl SortKey {
    pub const ALL: [SortKey; 8] = [
        SortKey::State,
        SortKey::Memory,
        SortKey::Cpu,
        SortKey::Name,
        SortKey::Recommendation,
        SortKey::Spend,
        SortKey::SinceStart,
        SortKey::Context,
    ];

    /// What the sort menu offers, in menu order. State and Recommendation
    /// stay valid ids so an old setting reads back, but are not offered.
    pub const OPTIONS: [SortKey; 6] = [
        SortKey::Spend,
        SortKey::SinceStart,
        SortKey::Context,
        SortKey::Name,
        SortKey::Cpu,
        SortKey::Memory,
    ];

    /// Valor da chave `sort-order` do GSettings (prefs.rs): estavel, nao muda
    /// com o rotulo.
    pub fn id(self) -> &'static str {
        match self {
            SortKey::State => "state",
            SortKey::Memory => "memory",
            SortKey::Cpu => "cpu",
            SortKey::Name => "name",
            SortKey::Recommendation => "recommendation",
            SortKey::Spend => "spend",
            SortKey::SinceStart => "since-start",
            SortKey::Context => "context",
        }
    }

    /// Inverso de [`SortKey::id`]; id desconhecido e `None`.
    pub fn from_id(id: &str) -> Option<SortKey> {
        SortKey::ALL.into_iter().find(|k| k.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            SortKey::State => "State",
            SortKey::Memory => "Memory",
            SortKey::Cpu => "CPU",
            SortKey::Name => "Name",
            SortKey::Recommendation => "Recommendation",
            SortKey::Spend => "30-min spend",
            SortKey::SinceStart => "Since start",
            SortKey::Context => "Context",
        }
    }

    /// The order inside a sentence: `Sorted by 30-min spend`.
    pub fn phrase(self) -> &'static str {
        match self {
            SortKey::Cpu => "CPU",
            SortKey::Spend => "30-min spend",
            SortKey::State => "state",
            SortKey::Memory => "memory",
            SortKey::Name => "name",
            SortKey::Recommendation => "recommendation",
            SortKey::SinceStart => "since start",
            SortKey::Context => "context",
        }
    }

    /// Propriedades que, mudando, podem mudar a ordem.
    pub fn props(self) -> Changed {
        match self {
            SortKey::State => Changed::of("group") | Changed::of("mem"),
            SortKey::Memory => Changed::of("mem"),
            SortKey::Cpu => Changed::of("cpu"),
            SortKey::Name => Changed::of("name"),
            SortKey::Recommendation => {
                Changed::of("rec") | Changed::of("ctx") | Changed::of("burn")
            }
            // Spend arrives through `set_spend`, not through a row's props.
            SortKey::Spend | SortKey::SinceStart => Changed::default(),
            SortKey::Context => Changed::of("ctx"),
        }
    }
}

fn is_mux(o: &SessionObject) -> bool {
    Group::from_rank(o.group()) == Group::Multiplexer
}

/// Secao do item: o estado na ordem "State", senao agente (0) ou
/// multiplexador (1). Os multiplexadores ficam sempre a parte, no fim.
pub fn section(key: SortKey, o: &SessionObject) -> u32 {
    match key {
        SortKey::State => o.group(),
        // One line per session, the multiplexers among them (direction B).
        SortKey::Cpu | SortKey::Memory => 0,
        SortKey::Spend | SortKey::SinceStart | SortKey::Context => 0,
        _ => u32::from(is_mux(o)),
    }
}

/// CPU and Memory lists have no section headers.
fn flat(key: SortKey) -> bool {
    matches!(
        key,
        SortKey::Cpu | SortKey::Memory | SortKey::Spend | SortKey::SinceStart | SortKey::Context
    )
}

/// List heading for the order: `CPU by session` or `Memory by session`.
pub fn heading(key: SortKey) -> &'static str {
    match key {
        SortKey::Memory => "Memory by session",
        SortKey::Spend => "Sessions by 30-min spend",
        SortKey::SinceStart => "Sessions by spend since start",
        SortKey::Context => "Sessions by context",
        _ => "CPU by session",
    }
}

/// Sub line under the heading: `% of one core · sessions use 6.2 of 16 cores`
/// or `PSS + swap · 28.6 GiB`.
pub fn subheading(key: SortKey, snap: &Snapshot) -> String {
    if key == SortKey::Memory {
        let held: u64 = crate::memory_map::MapInput::from_snapshot(snap)
            .sessions
            .iter()
            .map(|s| s.bytes())
            .sum();
        return format!("PSS + swap \u{b7} {}", crate::memory_map::gib1(held));
    }
    match key {
        SortKey::Spend => return "weighted tokens, last 30 min".to_owned(),
        SortKey::SinceStart => return "weighted tokens since start".to_owned(),
        SortKey::Context => return "context window in use".to_owned(),
        _ => {}
    }
    match snap.summary.sessions_cpu_pct {
        Some(pct) => format!(
            "% of one core \u{b7} sessions use {:.1} of {} cores",
            pct / 100.0,
            snap.machine.cores
        ),
        None => "% of one core".to_owned(),
    }
}

/// Titulo da secao do item e se ela pede atencao (bloqueadas).
pub fn section_title(key: SortKey, o: &SessionObject) -> (String, bool) {
    let g = Group::from_rank(o.group());
    match (key, g) {
        (_, Group::Multiplexer) => (g.title().into(), false),
        (SortKey::State, g) => (g.title().into(), g == Group::Blocked),
        _ => ("Agent sessions".into(), false),
    }
}

/// Ordem dentro da secao; pid desempata, para a ordem nao depender da chegada.
pub fn order_within(key: SortKey, a: &SessionObject, b: &SessionObject) -> Ordering {
    let by = match key {
        SortKey::State | SortKey::Memory => b.mem().cmp(&a.mem()),
        SortKey::Cpu => b.cpu().total_cmp(&a.cpu()),
        SortKey::Name => a.name().to_lowercase().cmp(&b.name().to_lowercase()),
        SortKey::Recommendation => (a.rec() != REC_HANDOFF)
            .cmp(&(b.rec() != REC_HANDOFF))
            .then_with(|| b.ctx().total_cmp(&a.ctx()))
            .then_with(|| b.burn().total_cmp(&a.burn())),
        // Unknown (-1) sinks below a known zero; the pid closes the tie.
        SortKey::Spend => b
            .spend()
            .cmp(&a.spend())
            .then_with(|| b.mem().cmp(&a.mem())),
        SortKey::SinceStart => b
            .since()
            .cmp(&a.since())
            .then_with(|| b.mem().cmp(&a.mem())),
        SortKey::Context => b.ctx().total_cmp(&a.ctx()),
    };
    by.then_with(|| a.pid().cmp(&b.pid()))
}

/// A ordem completa da lista: secao, depois dentro dela.
#[cfg(test)]
pub fn order(key: SortKey, a: &SessionObject, b: &SessionObject) -> Ordering {
    section(key, a)
        .cmp(&section(key, b))
        .then_with(|| order_within(key, a, b))
}

/// Filtro por tipo de agente: `None` mostra tudo; um agente esconde os
/// multiplexadores, que nao sao agente.
pub fn shows(agent: Option<&str>, o: &SessionObject) -> bool {
    agent.is_none_or(|a| !is_mux(o) && o.agent() == a)
}

fn gtk_order(o: Ordering) -> gtk::Ordering {
    match o {
        Ordering::Less => gtk::Ordering::Smaller,
        Ordering::Equal => gtk::Ordering::Equal,
        Ordering::Greater => gtk::Ordering::Larger,
    }
}

fn objects(a: &gtk::glib::Object, b: &gtk::glib::Object) -> Option<(SessionObject, SessionObject)> {
    Some((a.clone().downcast().ok()?, b.clone().downcast().ok()?))
}

/// Largura maxima (sp) de cada densidade, da mais larga a mais estreita.
/// Abaixo de 720 sp o bloco do cockpit e os subagentes saem da linha (ficam no
/// tooltip); abaixo de 600 sp saem as barras, o swap, o pane e as tool calls,
/// e a linha mostra nome, estado, recomendacao, CPU, memoria e contexto.
/// A largura e a da propria lista (um `AdwBreakpointBin` em volta dela), nao
/// a da janela: com o detalhe ao lado (C11) a lista e a barra lateral.
// A linha compacta ainda pede ~576px; trocar antes disso evita corte no rack
// de 563px da janela 1280px com o dashboard ao lado.
// The 450 px list beside the map at 1280 keeps both figure columns; only the
// 360 px window drops one.
// Column drops (REVIEW_RR1): <= 900 CPU (`no-cpu`), <= 720 memory
// (`compact`), <= 560 the share bar (`pct-only`), <= 420 context % (`narrow`);
// under 1000 the rows grow to 44 px (`touch`).
pub const DENSITY_MAX_WIDTH_SP: [(&str, f64); 5] = [
    ("touch", 999.0),
    ("no-cpu", 900.0),
    ("compact", 720.0),
    ("pct-only", 560.0),
    ("narrow", 420.0),
];

const ALL_AGENTS: &str = "All agents";

/// The handler of the scroll that waits for a layout, if any.
type PendingScroll = Rc<RefCell<Option<gtk::glib::SignalHandlerId>>>;

/// Milliseconds on the monotonic clock, the gate's time.
fn now_ms() -> u64 {
    (gtk::glib::monotonic_time() / 1000) as u64
}

/// When and how the list changes its order: the gate, its holds, and the
/// "Sorted by ..." line. Shared with the 1 s timer and the focus/pointer
/// handlers, which hold it weakly.
struct Order {
    gate: RefCell<reorder::Gate>,
    key: Rc<Cell<SortKey>>,
    dialog: Cell<bool>,
    operation: Cell<bool>,
    pointer: gtk::EventControllerMotion,
    focus: gtk::EventControllerFocus,
    sorter: gtk::CustomSorter,
    selection: gtk::SingleSelection,
    view: gtk::ListView,
    scroll: gtk::Adjustment,
    note: gtk::Label,
}

impl Order {
    fn holds(&self) -> reorder::Holds {
        reorder::Holds {
            pointer: self.pointer.contains_pointer(),
            focus: self.focus.contains_focus(),
            dialog: self.dialog.get(),
            operation: self.operation.get(),
        }
    }

    /// The order of the data differs from the screen: it applies at the next
    /// moment the gate allows.
    fn want(&self, now: u64) {
        self.gate.borrow_mut().want();
        self.poll(now);
    }

    fn poll(&self, now: u64) {
        let go = self.gate.borrow_mut().poll(now, self.holds());
        if go {
            self.apply();
        }
        self.refresh_note(now);
    }

    /// An explicit choice (the sort menu, F5): applies now, whatever holds.
    fn force(&self, now: u64) {
        self.gate.borrow_mut().force(now);
        self.apply();
        self.refresh_note(now);
    }

    fn refresh_note(&self, now: u64) {
        let text = self.note_text(now);
        if self.note.label() != text {
            self.note.set_label(&text);
        }
    }

    fn note_text(&self, now: u64) -> String {
        reorder::sorted_by_text(self.key.get().phrase(), self.gate.borrow().age_s(now))
    }

    /// The screen row of the selected session, from the list's own children.
    fn selected_row_height(&self) -> Option<f64> {
        let mut child = self.view.first_child();
        while let Some(w) = child {
            if w.state_flags().contains(gtk::StateFlags::SELECTED) && w.height() > 0 {
                return Some(f64::from(w.height()));
            }
            child = w.next_sibling();
        }
        None
    }

    /// Re-sorts, numbers the rows, and keeps the selected session where it
    /// was on screen: the selection follows the session, and the scroll
    /// follows the selection.
    fn apply(&self) {
        let selected = self.selection.selected_item();
        let old_pos = self.selection.selected();
        let row_h = self.selected_row_height();
        self.sorter.changed(gtk::SorterChange::Different);
        if let Some(sel) = selected {
            if self.selection.selected_item().as_ref() != Some(&sel) {
                let n = self.selection.n_items();
                if let Some(pos) = (0..n).find(|&i| self.selection.item(i).as_ref() == Some(&sel)) {
                    self.selection.set_selected(pos);
                }
            }
            let new_pos = self.selection.selected();
            if let Some(h) = row_h
                && new_pos != gtk::INVALID_LIST_POSITION
                && old_pos != gtk::INVALID_LIST_POSITION
            {
                self.scroll.set_value(reorder::keep_position(
                    self.scroll.value(),
                    old_pos,
                    new_pos,
                    h,
                ));
            }
        }
        for i in 0..self.selection.n_items() {
            if let Some(o) = self.selection.item(i).and_downcast::<SessionObject>()
                && o.rank() != i + 1
            {
                o.set_rank(i + 1);
            }
        }
    }
}

pub struct SessionList {
    root: adw::BreakpointBin,
    view: gtk::ListView,
    store: gio::ListStore,
    filter: gtk::CustomFilter,
    selection: gtk::SingleSelection,
    key: Rc<Cell<SortKey>>,
    metric: RowMetric,
    agents: gtk::StringList,
    agent_menu: gtk::DropDown,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    axis: gtk::Box,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    sort_menu: gtk::DropDown,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    heading: gtk::Label,
    sub: gtk::Label,
    sub_inline: gtk::Label,
    choose: Rc<dyn Fn(SortKey)>,
    on_sort: SortListeners,
    row_handlers: session_row::RowHandlers,
    scroll: gtk::Adjustment,
    order: Rc<Order>,
    /// The one `changed` handler waiting for a layout, see [`scroll_when_laid_out`].
    pending_scroll: PendingScroll,
}

type SortListeners = Rc<RefCell<Vec<Rc<dyn Fn(SortKey)>>>>;

/// Um breakpoint por densidade, do mais largo ao mais estreito: com varios
/// valendo, o libadwaita aplica o ultimo, entao cada um liga tambem as
/// densidades mais largas.
fn breakpoints(density: &Density) -> Vec<adw::Breakpoint> {
    DENSITY_MAX_WIDTH_SP
        .iter()
        .enumerate()
        .map(|(i, &(_, max))| {
            let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                max,
                adw::LengthUnit::Sp,
            ));
            for &(flag, _) in &DENSITY_MAX_WIDTH_SP[..=i] {
                bp.add_setter(density, flag, Some(&true.to_value()));
            }
            bp
        })
        .collect()
}

/// Largura minima da lista: a do bin dos breakpoints (o libadwaita pede um
/// minimo explicito) e a da barra lateral do detalhe.
pub const MIN_WIDTH_PX: i32 = 300;

fn header_factory(key: Rc<Cell<SortKey>>) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, item| {
        let Some(header) = item.downcast_ref::<gtk::ListHeader>() else {
            return;
        };
        let key = key.clone();
        session_row::setup_header(header, move |o| section_title(key.get(), o));
    });
    f
}

fn dropdown(items: &[&str], a11y: &str) -> (gtk::DropDown, gtk::StringList) {
    let list = gtk::StringList::new(items);
    let dd = gtk::DropDown::new(Some(list.clone()), gtk::Expression::NONE);
    dd.set_valign(gtk::Align::Center);
    dd.update_property(&[gtk::accessible::Property::Label(a11y)]);
    dd.set_tooltip_text(Some(a11y));
    (dd, list)
}

impl SessionList {
    pub fn new(model: &SessionModel) -> Self {
        let key = Rc::new(Cell::new(SortKey::default()));
        let agent: Rc<RefCell<Option<String>>> = Rc::default();

        let k = key.clone();
        let sorter = gtk::CustomSorter::new(move |a, b| {
            objects(a, b).map_or(gtk::Ordering::Equal, |(a, b)| {
                gtk_order(order_within(k.get(), &a, &b))
            })
        });
        let k = key.clone();
        let sections = gtk::CustomSorter::new(move |a, b| {
            objects(a, b).map_or(gtk::Ordering::Equal, |(a, b)| {
                gtk_order(section(k.get(), &a).cmp(&section(k.get(), &b)))
            })
        });
        let ag = agent.clone();
        let filter = gtk::CustomFilter::new(move |o| {
            o.downcast_ref::<SessionObject>()
                .is_some_and(|o| shows(ag.borrow().as_deref(), o))
        });
        let filtered = gtk::FilterListModel::new(Some(model.store().clone()), Some(filter.clone()));
        let sort = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
        sort.set_section_sorter(Some(&sections));
        let selection = gtk::SingleSelection::new(Some(sort));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let density = Density::default();
        let metric = RowMetric::default();
        metric.rescale(model.store());
        let rows = gtk::SignalListItemFactory::new();
        let row_handlers = session_row::RowHandlers::default();
        let (d, m, h) = (density.clone(), metric.clone(), row_handlers.clone());
        rows.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                session_row::setup(item, &d, &m, &h);
            }
        });
        let view = gtk::ListView::new(Some(selection.clone()), Some(rows));
        if !flat(key.get()) {
            view.set_header_factory(Some(&header_factory(key.clone())));
        }
        view.add_css_class("session-rack");
        // Tab sai da lista; as setas andam entre as linhas.
        view.set_tab_behavior(gtk::ListTabBehavior::Item);
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.update_property(&[gtk::accessible::Property::Label("Agent sessions")]);

        // Barra da lista: ordem e filtro por tipo de agente.
        let (agent_menu, agents) = dropdown(&[ALL_AGENTS], "Show agent type");
        // Heading, the CPU | Memory toggle that sets order and bar, and a
        // sub line with the unit (and the agent filter when there is a choice).
        let title = gtk::Label::new(Some(heading(key.get())));
        title.add_css_class("list-heading");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_accessible_role(gtk::AccessibleRole::Heading);
        let labels = SortKey::OPTIONS.map(SortKey::label);
        let (sort_menu, _) = dropdown(&labels, "Sort sessions by");
        sort_menu.add_css_class("order-menu");
        sort_menu.set_valign(gtk::Align::Center);
        sort_menu.set_halign(gtk::Align::End);
        if let Some(i) = SortKey::OPTIONS.iter().position(|k| *k == key.get()) {
            sort_menu.set_selected(i as u32);
        }
        let note = gtk::Label::new(None);
        note.add_css_class("list-sub");
        note.add_css_class("order-note");
        note.set_xalign(0.0);
        note.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let sub = gtk::Label::new(None);
        sub.add_css_class("list-sub");
        sub.set_xalign(0.0);
        sub.set_hexpand(true);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        // A wide list (the page below the map) has the unit on the heading's
        // line; a compact one gives it a line of its own.
        let sub_inline = gtk::Label::new(None);
        sub_inline.add_css_class("list-sub");
        sub_inline.set_xalign(0.0);
        sub_inline.set_hexpand(true);
        sub_inline.set_ellipsize(gtk::pango::EllipsizeMode::End);
        density
            .bind_property("compact", &sub, "visible")
            .sync_create()
            .build();
        density
            .bind_property("compact", &sub_inline, "visible")
            .invert_boolean()
            .sync_create()
            .build();
        title.set_hexpand(false);
        // Nothing to choose with one agent type: no control (see `refresh_agents`).
        agent_menu.set_visible(false);
        agent_menu.add_css_class("agent-filter");
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.append(&title);
        top.append(&sub_inline);
        top.append(&sort_menu);
        let under = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        under.append(&sub);
        under.append(&agent_menu);
        let note_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        note_line.append(&note);
        let axis = session_row::axis(&density);
        let bar = gtk::Box::new(gtk::Orientation::Vertical, 4);
        bar.add_css_class("session-controls");
        bar.append(&top);
        bar.append(&under);
        bar.append(&note_line);
        bar.append(&axis);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&bar);
        page.append(&scrolled);
        // A densidade segue a largura da lista, nao a da janela: ao lado do
        // detalhe a lista e estreita mesmo numa janela larga.
        let root = adw::BreakpointBin::builder()
            .child(&page)
            .width_request(MIN_WIDTH_PX)
            .height_request(120)
            .build();
        for bp in breakpoints(&density) {
            root.add_breakpoint(bp);
        }

        // Pointer over the list and focus inside it hold the order still.
        let pointer = gtk::EventControllerMotion::new();
        scrolled.add_controller(pointer.clone());
        let focus = gtk::EventControllerFocus::new();
        scrolled.add_controller(focus.clone());
        let order = Rc::new(Order {
            gate: RefCell::default(),
            key: key.clone(),
            dialog: Cell::new(false),
            operation: Cell::new(false),
            pointer: pointer.clone(),
            focus: focus.clone(),
            sorter: sorter.clone(),
            selection: selection.clone(),
            view: view.clone(),
            scroll: scrolled.vadjustment(),
            note: note.clone(),
        });
        order.refresh_note(now_ms());
        // A hold that ends lets the waiting reorder through.
        let weak = Rc::downgrade(&order);
        pointer.connect_leave(move |_| {
            if let Some(o) = weak.upgrade() {
                o.poll(now_ms());
            }
        });
        let weak = Rc::downgrade(&order);
        focus.connect_leave(move |_| {
            if let Some(o) = weak.upgrade() {
                o.poll(now_ms());
            }
        });
        // Also each second: the age in the note, and a hold that ended
        // without an event.
        let weak = Rc::downgrade(&order);
        gtk::glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(o) => {
                o.poll(now_ms());
                gtk::glib::ControlFlow::Continue
            }
            None => gtk::glib::ControlFlow::Break,
        });
        // F5 reorders now, from anywhere in the list's region.
        let f5 = gtk::ShortcutController::new();
        f5.set_scope(gtk::ShortcutScope::Local);
        let weak = Rc::downgrade(&order);
        f5.add_shortcut(gtk::Shortcut::new(
            Some(gtk::KeyvalTrigger::new(
                gtk::gdk::Key::F5,
                gtk::gdk::ModifierType::empty(),
            )),
            Some(gtk::CallbackAction::new(move |_, _| {
                if let Some(o) = weak.upgrade() {
                    o.force(now_ms());
                }
                gtk::glib::Propagation::Stop
            })),
        ));
        root.add_controller(f5);

        let on_sort: SortListeners = Rc::default();
        let choose: Rc<dyn Fn(SortKey)> = {
            let (k, o, v, m, h, a) = (
                key.clone(),
                order.clone(),
                view.clone(),
                metric.clone(),
                title.clone(),
                axis.clone(),
            );
            let callbacks = on_sort.clone();
            Rc::new(move |new| {
                if new == k.get() {
                    return;
                }
                k.set(new);
                m.follow(new);
                o.force(now_ms());
                h.set_label(heading(new));
                a.set_visible(Metric::of(new) == Metric::Cpu);
                // Section titles depend on the order; the flat orders have none.
                v.set_header_factory(
                    if flat(new) {
                        None
                    } else {
                        Some(header_factory(k.clone()))
                    }
                    .as_ref(),
                );
                let callbacks = callbacks.borrow().clone();
                for f in callbacks {
                    f(new);
                }
            })
        };
        // `syncing`: set_sort_key moves the menu without the user's choice.
        let syncing = Rc::new(Cell::new(false));
        {
            let (choose, syncing) = (choose.clone(), syncing.clone());
            sort_menu.connect_selected_notify(move |dd| {
                if let Some(k) = SortKey::OPTIONS.get(dd.selected() as usize)
                    && !syncing.get()
                {
                    choose(*k);
                }
            });
        }
        let menu = (sort_menu.clone(), syncing);
        let choose_for_key = choose.clone();
        let press: Rc<dyn Fn(SortKey)> = Rc::new(move |new| {
            let (dd, syncing) = &menu;
            if let Some(i) = SortKey::OPTIONS.iter().position(|k| *k == new) {
                syncing.set(true);
                dd.set_selected(i as u32);
                syncing.set(false);
            }
            choose_for_key(new);
        });
        let (ag, f) = (agent.clone(), filter.clone());
        agent_menu.connect_selected_notify(move |dd| {
            let chosen = match dd.selected() {
                0 | gtk::INVALID_LIST_POSITION => None,
                _ => dd
                    .selected_item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|s| s.string().to_string()),
            };
            if *ag.borrow() != chosen {
                *ag.borrow_mut() = chosen;
                f.changed(gtk::FilterChange::Different);
            }
        });

        // Under 1000 sp the rows are 44 px and the row menu stays visible.
        {
            let view = view.downgrade();
            density.connect_touch_notify(move |d| {
                if let Some(view) = view.upgrade() {
                    if d.touch() {
                        view.add_css_class("touch-rows");
                    } else {
                        view.remove_css_class("touch-rows");
                    }
                }
            });
        }
        // End is the window's own `win.act` flow, the same dialog as the
        // row menu's; the other operations are the window's to answer.
        {
            let (view, store) = (view.downgrade(), model.store().downgrade());
            row_handlers.borrow_mut().push(Rc::new(move |pid, op| {
                if op != crate::actions_ui::RowOp::End {
                    return;
                }
                let (Some(view), Some(store)) = (view.upgrade(), store.upgrade()) else {
                    return;
                };
                let name = (0..store.n_items())
                    .filter_map(|i| store.item(i).and_downcast::<SessionObject>())
                    .find(|o| o.pid() == pid)
                    .map(|o| o.name())
                    .unwrap_or_default();
                let _ = view.activate_action(
                    &format!("win.{}", crate::actions_ui::WIN_ACTION),
                    Some(&crate::actions_ui::end_target(pid, &name)),
                );
            }));
        }

        let scroll = scrolled.vadjustment();
        Self {
            root,
            view,
            store: model.store().clone(),
            filter,
            selection,
            key,
            metric,
            agents,
            agent_menu,
            axis,
            sort_menu,
            heading: title,
            sub,
            sub_inline,
            choose: press,
            on_sort,
            row_handlers,
            scroll,
            order,
            pending_scroll: PendingScroll::default(),
        }
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    /// A session row is selected.
    pub fn has_selection(&self) -> bool {
        self.selection.selected() != gtk::INVALID_LIST_POSITION
    }

    /// Spend of the same snapshot, for the spend orders and the share bar. The window calls it after each snapshot, after
    /// `spend.update(now, spend::inputs(&snap))`. A new order of the data
    /// reaches the screen through the 30 s gate.
    pub fn set_spend(&self, spend: &crate::spend::SpendTracker) {
        self.set_spend_now(spend, now_ms());
    }

    fn set_spend_now(&self, spend: &crate::spend::SpendTracker, now: u64) {
        let mut changed = false;
        for i in 0..self.store.n_items() {
            let Some(o) = self.store.item(i).and_downcast::<SessionObject>() else {
                continue;
            };
            let key = (o.pid(), u64::try_from(o.starttime()).ok());
            let known = |v: Option<u64>| v.map_or(-1, |v| i64::try_from(v).unwrap_or(i64::MAX));
            let (spend_30m, since) = (known(spend.spend_30m(key)), known(spend.since_start(key)));
            if o.spend() != spend_30m {
                o.set_spend(spend_30m);
                changed = true;
            }
            if o.since() != since {
                o.set_since(since);
                changed = true;
            }
            // The share bar reads these two; they are not an order key.
            let share = spend.share(key).unwrap_or(-1.0);
            let per_min = spend.per_min(key).unwrap_or(-1.0);
            if o.share() != share {
                o.set_share(share);
            }
            if o.per_min() != per_min {
                o.set_per_min(per_min);
            }
            // The series moves once a minute: compare before notifying.
            let spark = spend.series(key).map_or(String::new(), |s| {
                s.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            });
            if o.spark() != spark {
                o.set_spark(spark);
            }
        }
        if changed && matches!(self.key.get(), SortKey::Spend | SortKey::SinceStart) {
            self.order.gate.borrow_mut().want();
        }
        self.order.poll(now);
    }

    /// A dialog is open over the list: no reorder until it closes.
    pub fn set_dialog_open(&self, open: bool) {
        self.order.dialog.set(open);
        self.order.poll(now_ms());
    }

    /// An operation runs on a session: no reorder until it ends.
    #[allow(
        dead_code,
        reason = "the operation state is private to SessionControls (story 6bbb6431, frozen until the owner decides)"
    )]
    pub fn set_operation_running(&self, running: bool) {
        self.order.operation.set(running);
        self.order.poll(now_ms());
    }

    /// F5: the order refreshes now, whatever holds the list.
    #[allow(
        dead_code,
        reason = "F5 is handled inside the list; kept for the window's menu"
    )]
    pub fn refresh_order(&self) {
        self.order.force(now_ms());
    }

    #[cfg(test)]
    fn set_spend_at(&self, spend: &crate::spend::SpendTracker, now_ms: u64) {
        self.set_spend_now(spend, now_ms);
    }

    #[cfg(test)]
    fn poll_at(&self, now_ms: u64) {
        self.order.poll(now_ms);
    }

    #[cfg(test)]
    fn refresh_order_at(&self, now_ms: u64) {
        self.order.force(now_ms);
    }

    /// Pids in the order the list shows them.
    #[cfg(test)]
    fn shown_pids(&self) -> Vec<u32> {
        (0..self.selection.n_items())
            .filter_map(|i| self.selection.item(i).and_downcast::<SessionObject>())
            .map(|o| o.pid())
            .collect()
    }

    #[cfg(test)]
    fn sorted_text(&self, now_ms: u64) -> String {
        self.order.note_text(now_ms)
    }

    /// Picks the order the way a click on the toggle does.
    pub fn set_sort_key(&self, key: SortKey) {
        (self.choose)(key);
    }

    /// `f` with the pid and the operation of a row's Action button. End
    /// (`win.act`) is wired by the list itself; the window answers the rest.
    pub fn connect_row_action(&self, f: impl Fn(u32, crate::actions_ui::RowOp) + 'static) {
        self.row_handlers.borrow_mut().push(Rc::new(f));
    }

    /// `f` for each new order, from the toggle or from `set_sort_key`.
    pub fn connect_sort_changed(&self, f: impl Fn(SortKey) + 'static) {
        self.on_sort.borrow_mut().push(Rc::new(f));
    }

    /// The sub line under the heading, from the latest snapshot.
    pub fn describe(&self, snap: &Snapshot) {
        let text = subheading(self.key.get(), snap);
        for l in [&self.sub, &self.sub_inline] {
            if l.label() != text {
                l.set_label(&text);
            }
        }
    }

    /// Opcoes do filtro: os tipos de agente presentes, em ordem alfabetica.
    /// Se o escolhido sumiu, volta para todos.
    fn refresh_agents(&self) {
        let present: BTreeSet<String> = (0..self.store.n_items())
            .filter_map(|i| self.store.item(i).and_downcast::<SessionObject>())
            .filter(|o| !is_mux(o))
            .map(|o| o.agent())
            .collect();
        let shown: Vec<String> = (1..self.agents.n_items())
            .filter_map(|i| self.agents.string(i).map(|s| s.to_string()))
            .collect();
        if shown.iter().eq(present.iter()) {
            return;
        }
        let chosen = (self.agent_menu.selected() > 0)
            .then(|| self.agents.string(self.agent_menu.selected()))
            .flatten();
        let new: Vec<&str> = present.iter().map(String::as_str).collect();
        self.agents.splice(1, self.agents.n_items() - 1, &new);
        self.agent_menu.set_visible(new.len() > 1);
        // With one type left the dropdown is hidden: its filter must not stay.
        let pos = chosen
            .filter(|_| new.len() > 1)
            .and_then(|c| new.iter().position(|a| *a == c.as_str()))
            .map_or(0, |p| p as u32 + 1);
        self.agent_menu.set_selected(pos);
        // set_selected(0) sobre um 0 que ja era nao notifica: o filtro confere.
        self.filter.changed(gtk::FilterChange::Different);
    }

    /// Reorders, through the 30 s gate, only when a session changed a key of
    /// the current order. The selection follows the session, not the position.
    pub fn update(&self, applied: &Applied) {
        if !applied.is_empty() {
            self.metric.rescale(&self.store);
        }
        if applied.touches(Changed::of("group") | Changed::of("rec") | Changed::of("io-bound")) {
            self.filter.changed(gtk::FilterChange::Different);
        }
        if applied.added > 0 || applied.removed > 0 {
            self.refresh_agents();
        }
        if applied.added > 0 && self.store.n_items() as usize == applied.added {
            // Primeira carga: a lista ancora no primeiro item e deixaria o
            // cabecalho da primeira secao acima da dobra. Topo depois do layout,
            // ou a linha selecionada se ja houver uma (AGENTCENTER_DETAIL
            // seleciona no mesmo tick): o topo a deixaria fora da vista.
            let (adj, view, sel, pending) = (
                self.scroll.clone(),
                self.view.clone(),
                self.selection.clone(),
                self.pending_scroll.clone(),
            );
            gtk::glib::idle_add_local_once(move || match sel.selected() {
                gtk::INVALID_LIST_POSITION => adj.set_value(adj.lower()),
                pos => scroll_when_laid_out(&view, &adj, pos, &pending),
            });
        }
        if applied.is_empty() || !applied.touches(self.key.get().props()) {
            self.order.poll(now_ms());
            return;
        }
        self.order.want(now_ms());
    }
}

/// API da selecao (C11): o detalhe e o sampler seguem a linha selecionada.
impl SessionList {
    /// `f` com o pid selecionado depois de cada mudanca, uma vez por volta do
    /// laco: o resort de um tick (sorter.changed + set_selected em
    /// [`SessionList::update`]) passa por "nenhuma" e volta, e o detalhe nao
    /// precisa ver o meio do caminho. So chama se o pid mudou.
    pub fn connect_selected(&self, f: impl Fn(Option<u32>) + 'static) {
        let f = Rc::new(f);
        let last: Rc<Cell<Option<u32>>> = Rc::default();
        let queued = Rc::new(Cell::new(false));
        self.selection.connect_selected_item_notify(move |sel| {
            if queued.replace(true) {
                return;
            }
            let (f, last, queued, sel) = (f.clone(), last.clone(), queued.clone(), sel.clone());
            gtk::glib::idle_add_local_once(move || {
                queued.set(false);
                let pid = sel
                    .selected_item()
                    .and_downcast::<SessionObject>()
                    .map(|o| o.pid());
                if last.replace(pid) != pid {
                    f(pid);
                }
            });
        });
    }

    /// `f` com o pid da linha ativada: Enter ou duplo clique, e o clique
    /// simples numa linha (o clique so seleciona no GTK; a pagina empurrada
    /// em 360 px precisa abrir com um toque). Seta nao ativa: andar pela
    /// lista com o teclado nao abre o detalhe em cima dela.
    pub fn connect_open(&self, f: impl Fn(u32) + 'static) {
        let f = Rc::new(f);
        let g = f.clone();
        self.view.connect_activate(move |view, pos| {
            if let Some(o) = view
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<SessionObject>()
            {
                g(o.pid());
            }
        });
        // Fase de captura: corre antes do `released` da linha, que seleciona
        // e reivindica o evento. Aqui so se marca que o toque caiu numa linha;
        // a selecao ja feita e lida depois, fora do evento.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let sel = self.selection.clone();
        click.connect_released(move |g, n, x, y| {
            let Some(view) = g.widget() else { return };
            if n != 1 || !on_row(&view, x, y) {
                return;
            }
            let (f, sel) = (f.clone(), sel.clone());
            gtk::glib::idle_add_local_once(move || {
                if let Some(o) = sel.selected_item().and_downcast::<SessionObject>() {
                    f(o.pid());
                }
            });
        });
        self.view.add_controller(click);
    }

    /// Open an explicitly validated navigation target even if a previous
    /// agent filter hides it. The caller must validate its identity.
    pub fn reveal_pid(&self, pid: u32) -> bool {
        let present = (0..self.store.n_items()).any(|i| {
            self.store
                .item(i)
                .and_downcast::<SessionObject>()
                .is_some_and(|o| o.pid() == pid)
        });
        if !present {
            return false;
        }
        if self.select_pid(pid) {
            return true;
        }
        self.agent_menu.set_selected(0);
        self.select_pid(pid)
    }

    /// Seleciona a sessao `pid` (rola ate ela). `false` se nao esta na lista.
    pub fn select_pid(&self, pid: u32) -> bool {
        let n = self.selection.n_items();
        let Some(pos) = (0..n).find(|&i| {
            self.selection
                .item(i)
                .and_downcast::<SessionObject>()
                .is_some_and(|o| o.pid() == pid)
        }) else {
            return false;
        };
        self.view.scroll_to(pos, gtk::ListScrollFlags::SELECT, None);
        true
    }

    /// Foco de volta na linha selecionada (ao voltar do detalhe por teclado).
    pub fn focus_selected(&self) {
        let pos = self.selection.selected();
        if pos != gtk::INVALID_LIST_POSITION {
            self.view.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
            scroll_when_laid_out(&self.view, &self.scroll, pos, &self.pending_scroll);
            // FOCUS so move o foco por dentro da lista quando ela ja o tem;
            // voltando do detalhe ele esta fora (no seletor de ordem). O
            // grab_focus da lista foca o item que o scroll_to acabou de marcar.
            if self.view.focus_child().is_none() {
                self.view.grab_focus();
            }
        } else {
            self.view.grab_focus();
        }
    }
}

/// `scroll_to` numa lista ainda sem layout (escondida atras do detalhe em
/// 360 px, ou antes do primeiro quadro) nao rola: a ancora se perde quando o
/// layout chega e a linha fica fora da vista. Sem altura, refaz no idle depois
/// da primeira configuracao da rolagem com conteudo.
fn scroll_when_laid_out(
    view: &gtk::ListView,
    adj: &gtk::Adjustment,
    pos: u32,
    pending: &PendingScroll,
) {
    // A list that stays hidden never emits `changed`: a handler per call would
    // pile up. The newest position wins, so the older handler goes.
    if let Some(old) = pending.take() {
        adj.disconnect(old);
    }
    if view.height() > 0 && adj.upper() > 0.0 {
        view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
        return;
    }
    let (v, slot) = (view.clone(), pending.clone());
    let handler = adj.connect_changed(move |adj| {
        if adj.upper() <= 0.0 {
            return;
        }
        if let Some(id) = slot.take() {
            adj.disconnect(id);
        }
        // Fora do size_allocate que emitiu o changed.
        let v = v.clone();
        gtk::glib::idle_add_local_once(move || v.scroll_to(pos, gtk::ListScrollFlags::NONE, None));
    });
    pending.replace(Some(handler));
}

/// O ponto (x, y) da lista cai dentro de uma linha (e nao num cabecalho de
/// secao ou no vazio abaixo da ultima).
fn on_row(view: &gtk::Widget, x: f64, y: f64) -> bool {
    let mut w = view.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(c) = w {
        // O botao de acoes da linha abre o proprio menu, nao o detalhe.
        if c == *view || c.is::<gtk::Button>() || c.is::<gtk::MenuButton>() {
            return false;
        }
        if c.css_name() == "row" && c.parent().as_ref() == Some(view) {
            return true;
        }
        w = c.parent();
    }
    false
}

/// A pagina da lista: a barra de ordem e filtro sobre a lista rolavel, sem
/// rolagem lateral (a linha elipsa e esconde partes pela densidade, que segue
/// a largura da propria lista).
pub fn scrolled(list: &SessionList) -> adw::BreakpointBin {
    list.root.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::adapters::AgentRole;
    use agentcenter_core::{IoStallSource, Machine, Session};

    fn snap(sessions: Vec<(u32, u64)>) -> Snapshot {
        Snapshot {
            schema_version: 4,
            taken_at_ms: 0,
            interval_ms: 1000,
            machine: Machine {
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
            sessions: sessions
                .into_iter()
                .map(|(pid, pss)| {
                    let mut s =
                        Session::aggregate("claude".into(), AgentRole::Agent, pid, 16, Vec::new());
                    s.pss_bytes = Some(pss);
                    s
                })
                .collect(),
            summary: Default::default(),
        }
    }

    fn pids(m: &SessionModel) -> Vec<u32> {
        (0..m.store().n_items())
            .filter_map(|i| m.store().item(i).and_downcast::<SessionObject>())
            .map(|o| o.pid())
            .collect()
    }

    #[test]
    fn same_session_keeps_its_object_across_ticks() {
        let mut m = SessionModel::default();
        assert_eq!(m.apply(&snap(vec![(10, 1), (20, 2)]), None).added, 2);
        let before = m.store().item(0).unwrap();
        let a = m.apply(&snap(vec![(10, 5 << 20), (20, 2)]), None);
        assert_eq!((a.added, a.removed, a.updated.len()), (0, 0, 1));
        let after = m.store().item(0).unwrap();
        assert_eq!(before, after, "the row object is reused, not rebuilt");
        assert_eq!(
            after.downcast::<SessionObject>().unwrap().pss_text(),
            "5.0 MiB"
        );
    }

    /// CC8: PSS e Swap da linha tem escala real com a capacidade do tick.
    #[test]
    fn rows_scale_pss_and_swap_by_the_machine_of_the_same_tick() {
        let mut m = SessionModel::default();
        let mut sn = snap(vec![(10, 1 << 30)]);
        sn.machine.mem_total_bytes = Some(4 << 30);
        sn.machine.swap_total_bytes = Some(0);
        m.apply(&sn, None);
        let o = m.store().item(0).and_downcast::<SessionObject>().unwrap();
        assert!((o.pss_pct() - 25.0).abs() < 1e-9, "{}", o.pss_pct());
        assert!(!o.has_swap(), "machine without swap");
        sn.machine.mem_total_bytes = None;
        m.apply(&sn, None);
        assert!(o.pss_pct() < 0.0, "unknown capacity: track only");
    }

    /// Reusing the selected row object transfers selection to a replacement process.
    #[test]
    fn cc2_regression_kernel_replacement_must_not_reuse_selected_row() {
        let mut m = SessionModel::default();
        let mut first = snap(vec![(10, 1)]);
        first.sessions[0].starttime = Some(123);
        m.apply(&first, None);
        let selected = m.store().item(0).unwrap();
        first.sessions[0].starttime = Some(124);
        m.apply(&first, None);
        assert_ne!(
            selected,
            m.store().item(0).unwrap(),
            "PID reuse must not keep the selected object attached to the replacement"
        );
        first.sessions[0].overlay = Some(agentcenter_core::overlay::SessionOverlay {
            agent_session_id: Some("original".into()),
            ..Default::default()
        });
        m.apply(&first, None);
        let native = m.store().item(0).unwrap();
        first.sessions[0].overlay.as_mut().unwrap().agent_session_id = Some("replacement".into());
        let changed = m.apply(&first, None);
        assert_eq!((changed.added, changed.removed), (1, 1));
        assert_ne!(native, m.store().item(0).unwrap());
        let stable = m.store().item(0).unwrap();
        m.apply(&first, None);
        assert_eq!(stable, m.store().item(0).unwrap());
    }

    #[test]
    fn unchanged_tick_reports_no_change() {
        let mut m = SessionModel::default();
        m.apply(&snap(vec![(10, 1)]), None);
        assert!(m.apply(&snap(vec![(10, 1)]), None).is_empty());
    }

    #[test]
    fn only_a_change_in_the_sort_key_asks_for_a_resort() {
        let mut m = SessionModel::default();
        m.apply(&snap(vec![(10, 1), (20, 2), (30, 3)]), None);
        let a = m.apply(&snap(vec![(10, 1), (20, 9 << 20), (30, 3)]), None);
        assert_eq!(a.updated.len(), 1, "only session 20 changed");
        assert!(a.touches(Changed::of("pss")), "sorted by PSS: resort");
        assert!(
            !a.touches(Changed::of("cpu")),
            "sorted by CPU: pss moved, order did not"
        );
        assert!(a.touches(Changed::of("cpu") | Changed::of("pss")));
        let still = m.apply(&snap(vec![(10, 1), (20, 9 << 20), (30, 3)]), None);
        assert!(!still.touches(Changed::of("pss")), "quiet tick: no resort");
    }

    #[test]
    fn sessions_come_and_go_by_pid() {
        let mut m = SessionModel::default();
        m.apply(&snap(vec![(10, 1), (20, 2), (30, 3)]), None);
        let a = m.apply(&snap(vec![(30, 3), (40, 4)]), None);
        assert_eq!((a.added, a.removed), (1, 2));
        assert_eq!(pids(&m), vec![30, 40]);
        assert_eq!(m.len(), 2);
        m.apply(&snap(vec![]), None);
        assert_eq!(m.store().n_items(), 0);
    }

    fn obj(pid: u32, agent: &str, group: Group, f: impl FnOnce(&mut Row)) -> SessionObject {
        let mut r = Row {
            pid,
            agent: agent.into(),
            name: format!("s{pid}"),
            group: group.rank(),
            ..Row::default()
        };
        f(&mut r);
        SessionObject::new(&r)
    }

    #[test]
    fn sort_ids_round_trip_and_match_the_schema() {
        let schema = include_str!("../../../data/io.github.eduardopin.AgentCenter.gschema.xml");
        for k in SortKey::ALL {
            assert_eq!(SortKey::from_id(k.id()), Some(k));
            assert!(
                schema.contains(&format!("<choice value=\"{}\"/>", k.id())),
                "{} is not a choice of sort-order",
                k.id()
            );
        }
        assert_eq!(SortKey::from_id("size"), None);
    }

    fn sorted(key: SortKey, objs: &[SessionObject]) -> Vec<u32> {
        let mut v = objs.to_vec();
        v.sort_by(|a, b| order(key, a, b));
        v.iter().map(|o| o.pid()).collect()
    }

    fn rack() -> Vec<SessionObject> {
        vec![
            obj(1, "claude", Group::Idle, |r| {
                (r.mem, r.cpu, r.ctx) = (900, 1.0, 30.0)
            }),
            obj(2, "claude", Group::Blocked, |r| {
                (r.mem, r.cpu, r.ctx) = (100, 5.0, 95.0)
            }),
            obj(3, "codex", Group::Busy, |r| {
                (r.mem, r.cpu, r.ctx) = (500, 80.0, 60.0)
            }),
            obj(4, "herdr", Group::Multiplexer, |r| {
                (r.mem, r.cpu) = (2000, 90.0)
            }),
            obj(5, "pi", Group::Busy, |r| {
                (r.mem, r.cpu, r.ctx) = (700, 20.0, 10.0);
                r.name = "Alpha".into();
                r.rec = REC_HANDOFF;
            }),
            obj(6, "claude", Group::Unknown, |r| {
                (r.mem, r.cpu, r.ctx) = (-1, -1.0, -1.0)
            }),
        ]
    }

    #[test]
    fn state_order_is_blocked_busy_idle_unknown_then_multiplexers() {
        // Busy: 5 (700) antes de 3 (500), memoria dentro da secao.
        assert_eq!(sorted(SortKey::State, &rack()), [2, 5, 3, 1, 6, 4]);
        let titles: Vec<String> = rack()
            .iter()
            .map(|o| section_title(SortKey::State, o).0)
            .collect();
        assert_eq!(
            titles,
            ["Idle", "Blocked", "Busy", "Multiplexers", "Busy", "Unknown"]
        );
        assert!(
            section_title(SortKey::State, &rack()[1]).1,
            "blocked asks for attention"
        );
    }

    #[test]
    fn name_and_recommendation_orders_keep_multiplexers_apart_at_the_end() {
        // Sem diferenciar maiusculas: Alpha, s1, s2, s3, s6.
        assert_eq!(sorted(SortKey::Name, &rack()), [5, 1, 2, 3, 6, 4]);
        // HANDOFF primeiro, depois contexto maior primeiro, ausente no fim.
        assert_eq!(sorted(SortKey::Recommendation, &rack()), [5, 2, 3, 1, 6, 4]);
        assert_eq!(section_title(SortKey::Name, &rack()[0]).0, "Agent sessions");
    }

    #[test]
    fn ties_break_by_pid() {
        let a = obj(9, "claude", Group::Idle, |r| r.mem = 5);
        let b = obj(8, "claude", Group::Idle, |r| r.mem = 5);
        assert_eq!(sorted(SortKey::Memory, &[a, b]), [8, 9]);
    }

    #[test]
    fn filter_by_agent_type_hides_others_and_multiplexers() {
        let shown = |f: Option<&str>| -> Vec<u32> {
            rack()
                .iter()
                .filter(|o| shows(f, o))
                .map(|o| o.pid())
                .collect()
        };
        assert_eq!(shown(None), [1, 2, 3, 4, 5, 6]);
        assert_eq!(shown(Some("claude")), [1, 2, 6]);
        assert_eq!(
            shown(Some("herdr")),
            Vec::<u32>::new(),
            "a multiplexer is not an agent type"
        );
        assert_eq!(shown(Some("gemini")), Vec::<u32>::new());
    }

    #[test]
    fn each_sort_key_resorts_only_on_its_own_props() {
        let mut m = SessionModel::default();
        m.apply(&snap(vec![(10, 1), (20, 2)]), None);
        let a = m.apply(&snap(vec![(10, 1), (20, 7 << 20)]), None);
        assert!(a.touches(SortKey::Memory.props()) && a.touches(SortKey::State.props()));
        for k in [SortKey::Cpu, SortKey::Name, SortKey::Recommendation] {
            assert!(!a.touches(k.props()), "{k:?}: memory moved, order did not");
        }
    }

    #[test]
    fn recommendation_change_updates_the_same_object() {
        // O cockpit chega com o C21; aqui a linha muda direto, como fara o tick.
        let o = obj(1, "claude", Group::Busy, |_| ());
        let mut r = Row {
            pid: 1,
            agent: "claude".into(),
            name: "s1".into(),
            group: Group::Busy.rank(),
            ..Row::default()
        };
        assert!(o.apply(&r).is_empty());
        r.rec = REC_HANDOFF;
        r.rec_text = "HANDOFF".into();
        let c = o.apply(&r);
        assert_eq!(c, Changed::of("rec") | Changed::of("rec-text"));
        assert_eq!(o.rec_text(), "HANDOFF");
        let applied = Applied {
            updated: vec![(o.clone(), c)],
            ..Applied::default()
        };
        assert!(
            applied.touches(SortKey::Recommendation.props()),
            "resort by recommendation"
        );
        assert!(
            !applied.touches(SortKey::Memory.props()),
            "no resort by memory"
        );
    }

    #[test]
    fn state_change_moves_the_session_to_its_new_section() {
        let mut m = SessionModel::default();
        let mut s = snap(vec![(10, 1)]);
        m.apply(&s, None);
        let before = m.store().item(0).unwrap();
        s.sessions[0].overlay = Some(agentcenter_core::overlay::SessionOverlay {
            status: Some(agentcenter_core::overlay::OverlayStatus::Blocked),
            ..Default::default()
        });
        let a = m.apply(&s, None);
        assert!(a.touches(SortKey::State.props()));
        let after = m.store().item(0).unwrap();
        assert_eq!(before, after, "same object, new section");
        let o = after.downcast::<SessionObject>().unwrap();
        assert_eq!(
            (section(SortKey::State, &o), o.status().as_str()),
            (Group::Blocked.rank(), "Blocked")
        );
    }

    #[test]
    fn cpu_and_memory_orders_are_flat_and_interleave_multiplexers() {
        // Direction B: one line per session, the multiplexer among them.
        assert_eq!(sorted(SortKey::Memory, &rack()), [4, 1, 5, 3, 2, 6]);
        assert_eq!(sorted(SortKey::Cpu, &rack()), [4, 3, 5, 2, 1, 6]);
        for o in rack() {
            assert_eq!(section(SortKey::Cpu, &o), 0);
            assert_eq!(section(SortKey::Memory, &o), 0);
        }
        // The other orders keep the multiplexers apart.
        assert_eq!(section(SortKey::Name, &rack()[3]), 1);
    }

    fn tipico() -> Snapshot {
        serde_json::from_str(include_str!("../tests/fixtures/snapshots/tipico-40.json")).unwrap()
    }

    #[test]
    fn heading_and_subheading_follow_the_order() {
        assert_eq!(heading(SortKey::Cpu), "CPU by session");
        assert_eq!(heading(SortKey::Memory), "Memory by session");
        assert_eq!(heading(SortKey::State), "CPU by session");
        let snap = tipico();
        assert_eq!(
            subheading(SortKey::Cpu, &snap),
            "% of one core \u{b7} sessions use 6.2 of 16 cores"
        );
        assert_eq!(
            subheading(SortKey::Memory, &snap),
            "PSS + swap \u{b7} 28.6 GiB"
        );
        let mut bare = snap;
        bare.summary.sessions_cpu_pct = None;
        assert_eq!(subheading(SortKey::Cpu, &bare), "% of one core");
    }

    /// The sort menu sets the order, and `set_sort_key` moves it.
    #[test]
    fn the_sort_menu_sets_the_order_and_follows_set_sort_key() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let list = SessionList::new(&SessionModel::default());
            let seen = Rc::new(RefCell::new(Vec::new()));
            let s = seen.clone();
            list.connect_sort_changed(move |k| s.borrow_mut().push(k));
            // The list opens on the spend, so choosing it again is no change.
            list.set_sort_key(SortKey::Spend);
            assert_eq!(list.sort_menu.selected(), 0);
            let at = |k: SortKey| SortKey::OPTIONS.iter().position(|o| *o == k).unwrap() as u32;
            list.sort_menu.set_selected(at(SortKey::Memory));
            assert_eq!(list.key.get(), SortKey::Memory);
            assert_eq!(list.heading.label(), "Memory by session");
            list.sort_menu.set_selected(at(SortKey::Cpu));
            assert_eq!(list.key.get(), SortKey::Cpu);
            assert_eq!(list.heading.label(), "CPU by session");
            assert_eq!(*seen.borrow(), vec![SortKey::Memory, SortKey::Cpu]);
            list.set_sort_key(SortKey::Name);
            assert_eq!(list.sort_menu.selected(), at(SortKey::Name));
            assert_eq!(*seen.borrow().last().unwrap(), SortKey::Name);
        });
    }

    /// Two agent types and a multiplexer; `drop_first` leaves only the codex one.
    fn mixed(drop_first: bool) -> Snapshot {
        let mut s = snap(vec![(1, 5), (2, 6)]);
        s.sessions[1].agent = "codex".into();
        let mut mux = Session::aggregate("herdr".into(), AgentRole::Multiplexer, 3, 16, Vec::new());
        mux.pss_bytes = Some(7);
        s.sessions.push(mux);
        if drop_first {
            s.sessions.remove(0);
        }
        s
    }

    /// The agent dropdown hides when one type is left: the filter goes with it,
    /// or the multiplexers stay hidden behind a control nobody can reach.
    #[test]
    fn filter_resets_when_the_agent_dropdown_hides() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let mut m = SessionModel::default();
            let list = SessionList::new(&m);
            list.update(&m.apply(&mixed(false), None));
            assert!(list.agent_menu.is_visible());
            list.agent_menu.set_selected(2);
            assert_eq!(list.selection.n_items(), 1, "codex only");
            list.update(&m.apply(&mixed(true), None));
            assert!(!list.agent_menu.is_visible());
            assert_eq!(list.agent_menu.selected(), 0, "back on All");
            assert_eq!(list.selection.n_items(), 2, "codex and the multiplexer");
        });
    }

    /// Handlers on the vertical adjustment currently connected to `changed`.
    fn changed_handlers(adj: &gtk::Adjustment) -> u32 {
        use gtk::glib::object::ObjectType;
        use gtk::glib::translate::IntoGlib;
        // SAFETY: a live adjustment; the matched handlers are only counted by
        // the return of the disconnect, which the test does last.
        unsafe {
            let id = gtk::glib::gobject_ffi::g_signal_lookup(
                c"changed".as_ptr(),
                gtk::Adjustment::static_type().into_glib(),
            );
            gtk::glib::gobject_ffi::g_signal_handlers_disconnect_matched(
                adj.as_ptr() as *mut _,
                gtk::glib::gobject_ffi::G_SIGNAL_MATCH_ID,
                id,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        }
    }

    /// A list nobody laid out yet keeps one pending scroll, not one per call.
    #[test]
    fn scrolling_a_hidden_list_keeps_one_pending_handler() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let mut m = SessionModel::default();
            let list = SessionList::new(&m);
            m.apply(&snap(vec![(1, 5)]), None);
            list.selection.set_selected(0);
            assert_eq!(list.scroll.upper(), 0.0, "no layout in this test");
            // The scrolled window has handlers of its own: a fresh list is the baseline.
            let base = changed_handlers(&SessionList::new(&SessionModel::default()).scroll);
            for _ in 0..3 {
                list.focus_selected();
            }
            assert_eq!(changed_handlers(&list.scroll), base + 1);
        });
    }
}

/// The order of the list by spend, and when it may change (REVIEW_RR1).
#[cfg(test)]
mod order_tests {
    use super::*;
    use crate::spend::{self, SpendTracker};

    const S: u64 = 1000;

    fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
    }

    fn obj(pid: u32, name: &str, f: impl FnOnce(&SessionObject)) -> SessionObject {
        let o = SessionObject::new(&Row {
            pid,
            agent: "claude".into(),
            name: name.into(),
            group: Group::Busy.rank(),
            ..Row::default()
        });
        f(&o);
        o
    }

    fn sorted(key: SortKey, objs: &[SessionObject]) -> Vec<u32> {
        let mut v: Vec<_> = objs.iter().collect();
        v.sort_by(|a, b| order(key, a, b));
        v.iter().map(|o| o.pid()).collect()
    }

    /// Spend inputs for the first sessions of `snap`, in order, others unknown.
    fn spends(snap: &Snapshot, values: &[Option<u64>]) -> Vec<spend::Input> {
        let mut inputs = spend::inputs(snap);
        for (i, input) in inputs.iter_mut().enumerate() {
            input.spend_30m = values.get(i).copied().flatten();
            input.since_start = input.spend_30m.map(|v| v * 3);
        }
        inputs
    }

    fn tracker(now_ms: u64, inputs: Vec<spend::Input>) -> SpendTracker {
        let mut t = SpendTracker::default();
        t.update(now_ms, inputs);
        t
    }

    #[test]
    fn the_default_order_is_the_30_minute_spend() {
        assert_eq!(SortKey::default(), SortKey::Spend);
        assert_eq!(
            SortKey::OPTIONS.map(SortKey::label),
            [
                "30-min spend",
                "Since start",
                "Context",
                "Name",
                "CPU",
                "Memory"
            ]
        );
        assert_eq!(SortKey::Spend.phrase(), "30-min spend");
        assert_eq!(SortKey::from_id("spend"), Some(SortKey::Spend));
    }

    #[test]
    fn spend_order_is_spend_then_memory_then_pid_with_unknown_last() {
        let objs = [
            obj(1, "a", |o| {
                o.set_spend(10);
                o.set_mem(5);
            }),
            obj(2, "b", |o| {
                o.set_spend(50);
                o.set_mem(1);
            }),
            // Same spend as 1, more memory: first of the two.
            obj(3, "c", |o| {
                o.set_spend(10);
                o.set_mem(9);
            }),
            // Unknown spend sinks below a known zero.
            obj(4, "d", |o| o.set_mem(99)),
            obj(5, "e", |o| o.set_spend(0)),
            // Same spend and memory as 1: the pid decides.
            obj(6, "f", |o| {
                o.set_spend(10);
                o.set_mem(5);
            }),
        ];
        assert_eq!(sorted(SortKey::Spend, &objs), [2, 3, 1, 6, 5, 4]);
    }

    #[test]
    fn since_start_context_name_cpu_and_memory_orders() {
        let objs = [
            obj(1, "Bravo", |o| {
                o.set_since(5);
                o.set_ctx(10.0);
                o.set_cpu(3.0);
                o.set_mem(30);
            }),
            obj(2, "alpha", |o| {
                o.set_since(50);
                o.set_ctx(90.0);
                o.set_cpu(1.0);
                o.set_mem(10);
            }),
            obj(3, "charlie", |o| {
                o.set_since(50);
                o.set_ctx(90.0);
                o.set_cpu(3.0);
                o.set_mem(20);
            }),
        ];
        // Equal since-start: memory, then pid.
        assert_eq!(sorted(SortKey::SinceStart, &objs), [3, 2, 1]);
        // Equal context: the pid keeps the order stable.
        assert_eq!(sorted(SortKey::Context, &objs), [2, 3, 1]);
        assert_eq!(sorted(SortKey::Name, &objs), [2, 1, 3]);
        assert_eq!(sorted(SortKey::Cpu, &objs), [1, 3, 2]);
        assert_eq!(sorted(SortKey::Memory, &objs), [1, 3, 2]);
        for k in [SortKey::Spend, SortKey::SinceStart, SortKey::Context] {
            assert_eq!(section(k, &objs[0]), 0, "{k:?} is one flat list");
        }
    }

    #[test]
    fn unknown_since_start_sinks_below_a_known_zero() {
        let objs = [obj(1, "a", |_| ()), obj(2, "b", |o| o.set_since(0))];
        assert_eq!(sorted(SortKey::SinceStart, &objs), [2, 1]);
    }

    #[test]
    fn the_model_keys_each_object_by_pid_and_start_time() {
        let mut snap = fixture("cockpit");
        snap.sessions[0].starttime = Some(77);
        let mut model = SessionModel::default();
        model.apply(&snap, None);
        let by_pid = |pid| {
            (0..model.store().n_items())
                .filter_map(|i| model.store().item(i).and_downcast::<SessionObject>())
                .find(|o| o.pid() == pid)
                .unwrap()
        };
        assert_eq!(by_pid(snap.sessions[0].pid).starttime(), 77);
        assert_eq!(by_pid(snap.sessions[1].pid).starttime(), -1);
    }

    /// A list with the `cockpit` sessions; the first three get the spends.
    fn cockpit_list() -> (SessionModel, SessionList, Snapshot) {
        let snap = fixture("cockpit");
        let mut model = SessionModel::default();
        model.apply(&snap, None);
        let list = SessionList::new(&model);
        (model, list, snap)
    }

    fn pid(snap: &Snapshot, i: usize) -> u32 {
        snap.sessions[i].pid
    }

    #[test]
    fn set_spend_orders_the_list_by_spend_and_numbers_the_rows() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let (_m, list, snap) = cockpit_list();
            let t = tracker(0, spends(&snap, &[Some(10), Some(300), Some(20)]));
            list.set_spend_at(&t, 0);
            let shown = list.shown_pids();
            assert_eq!(
                shown[..3],
                [pid(&snap, 1), pid(&snap, 2), pid(&snap, 0)],
                "spend 300, 20, 10 first"
            );
            let ranks: Vec<u32> = (0..list.selection.n_items())
                .filter_map(|i| list.selection.item(i).and_downcast::<SessionObject>())
                .map(|o| o.rank())
                .collect();
            assert_eq!(ranks, (1..=snap.sessions.len() as u32).collect::<Vec<_>>());
        });
    }

    #[test]
    fn a_new_spend_order_waits_for_the_thirty_second_gate() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let (_m, list, snap) = cockpit_list();
            list.set_spend_at(
                &tracker(0, spends(&snap, &[Some(10), Some(300), Some(20)])),
                0,
            );
            let first = list.shown_pids();
            // Session 0 now spends the most: the data changes, the screen does not.
            let t = tracker(10 * S, spends(&snap, &[Some(900), Some(300), Some(20)]));
            list.set_spend_at(&t, 10 * S);
            assert_eq!(list.shown_pids(), first, "10 s after the last reorder");
            list.poll_at(29 * S);
            assert_eq!(list.shown_pids(), first);
            list.poll_at(30 * S);
            assert_eq!(list.shown_pids()[0], pid(&snap, 0));
        });
    }

    #[test]
    fn a_dialog_or_a_running_operation_holds_the_reorder_until_it_ends() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            for operation in [false, true] {
                let (_m, list, snap) = cockpit_list();
                list.set_spend_at(
                    &tracker(0, spends(&snap, &[Some(10), Some(300), Some(20)])),
                    0,
                );
                let first = list.shown_pids();
                let hold = |on| {
                    if operation {
                        list.set_operation_running(on)
                    } else {
                        list.set_dialog_open(on)
                    }
                };
                hold(true);
                let t = tracker(100 * S, spends(&snap, &[Some(900), Some(300), Some(20)]));
                list.set_spend_at(&t, 100 * S);
                list.poll_at(200 * S);
                assert_eq!(list.shown_pids(), first, "held (operation: {operation})");
                hold(false);
                assert_ne!(list.shown_pids(), first, "applies when the hold ends");
                assert_eq!(list.shown_pids()[0], pid(&snap, 0));
            }
        });
    }

    #[test]
    fn f5_reorders_now_and_restarts_the_age() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let (_m, list, snap) = cockpit_list();
            list.set_spend_at(
                &tracker(0, spends(&snap, &[Some(10), Some(300), Some(20)])),
                0,
            );
            list.set_dialog_open(true);
            let t = tracker(5 * S, spends(&snap, &[Some(900), Some(300), Some(20)]));
            list.set_spend_at(&t, 5 * S);
            assert_eq!(
                list.sorted_text(5 * S),
                "Sorted by 30-min spend - updated 5 s ago"
            );
            list.refresh_order_at(6 * S);
            assert_eq!(
                list.shown_pids()[0],
                pid(&snap, 0),
                "dialog does not stop F5"
            );
            assert_eq!(
                list.sorted_text(6 * S),
                "Sorted by 30-min spend - updated 0 s ago"
            );
            assert_eq!(
                list.sorted_text(18 * S),
                "Sorted by 30-min spend - updated 12 s ago"
            );
        });
    }

    #[test]
    fn the_selected_session_stays_selected_when_the_order_changes() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let (_m, list, snap) = cockpit_list();
            list.set_spend_at(
                &tracker(0, spends(&snap, &[Some(10), Some(300), Some(20)])),
                0,
            );
            assert!(list.select_pid(pid(&snap, 0)));
            let t = tracker(40 * S, spends(&snap, &[Some(900), Some(300), Some(20)]));
            list.set_spend_at(&t, 40 * S);
            let sel = list
                .selection
                .selected_item()
                .and_downcast::<SessionObject>();
            assert_eq!(sel.map(|o| o.pid()), Some(pid(&snap, 0)));
            assert_eq!(
                list.shown_pids()[0],
                pid(&snap, 0),
                "and it moved to the top"
            );
        });
    }

    #[test]
    fn forty_sessions_change_order_at_most_once_per_thirty_seconds() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let snap = fixture("tipico-40");
            let mut model = SessionModel::default();
            model.apply(&snap, None);
            let list = SessionList::new(&model);
            assert_eq!(snap.sessions.len(), 40);
            let mut seed = 0x2545_F491_4F6C_DD1Du64;
            let mut last = list.shown_pids();
            let mut changes = 0;
            // Two minutes of one-second ticks, every spend moving every tick.
            for t in 0..120u64 {
                let values: Vec<Option<u64>> = (0..40)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        Some(seed % 100_000)
                    })
                    .collect();
                list.set_spend_at(&tracker(t * S, spends(&snap, &values)), t * S);
                let now = list.shown_pids();
                if now != last {
                    changes += 1;
                    last = now;
                }
            }
            // At 0, 30, 60 and 90 s.
            assert!(changes <= 4, "{changes} reorders in 120 s");
            assert!(
                changes >= 3,
                "the gate must also let reorders through: {changes}"
            );
        });
    }
}
