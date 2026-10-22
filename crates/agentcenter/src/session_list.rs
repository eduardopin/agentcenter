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

#[path = "session_row.rs"]
mod session_row;

use crate::format::{Group, REC_HANDOFF, Row};
use crate::session_object::{Changed, SessionObject};
use adw::prelude::*;
use agentcenter_core::Snapshot;
use gtk::gio;
use session_row::Density;
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
    /// Secoes por estado, memoria dentro de cada uma (layout B).
    #[default]
    State,
    Memory,
    Cpu,
    Name,
    /// HANDOFF primeiro, depois contexto e burn, maior primeiro (contexto.py).
    Recommendation,
}

impl SortKey {
    pub const ALL: [SortKey; 5] = [
        SortKey::State,
        SortKey::Memory,
        SortKey::Cpu,
        SortKey::Name,
        SortKey::Recommendation,
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionFilter {
    #[default]
    All,
    Group(Group),
    IoBound,
    Recommendation(u32),
}

impl SessionFilter {
    pub fn matches(self, o: &SessionObject) -> bool {
        match self {
            Self::All => true,
            Self::Group(g) => Group::from_rank(o.group()) == g,
            Self::IoBound => !is_mux(o) && o.io_bound(),
            Self::Recommendation(r) => !is_mux(o) && o.rec() == r,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All sessions",
            Self::Group(g) => g.title(),
            Self::IoBound => "I/O-bound",
            Self::Recommendation(0) => "No cockpit data",
            Self::Recommendation(1) => "Reading transcript",
            Self::Recommendation(2) => "Cockpit OK",
            Self::Recommendation(3) => "Compact",
            Self::Recommendation(4) => "Handoff",
            Self::Recommendation(5) => "Cockpit idle",
            Self::Recommendation(6) => "Loop",
            Self::Recommendation(_) => "Recommendation",
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
        _ => u32::from(is_mux(o)),
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
pub const DENSITY_MAX_WIDTH_SP: [(&str, f64); 2] = [("compact", 720.0), ("narrow", 600.0)];

const ALL_AGENTS: &str = "All agents";

pub struct SessionList {
    root: adw::BreakpointBin,
    view: gtk::ListView,
    store: gio::ListStore,
    sorter: gtk::CustomSorter,
    filter: gtk::CustomFilter,
    active_filter: Rc<Cell<SessionFilter>>,
    clear_filter: gtk::Button,
    selection: gtk::SingleSelection,
    key: Rc<Cell<SortKey>>,
    agents: gtk::StringList,
    agent_menu: gtk::DropDown,
    sort_menu: gtk::DropDown,
    scroll: gtk::Adjustment,
}

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
        let active_filter = Rc::new(Cell::new(SessionFilter::All));

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
        let named = active_filter.clone();
        let filter = gtk::CustomFilter::new(move |o| {
            o.downcast_ref::<SessionObject>()
                .is_some_and(|o| shows(ag.borrow().as_deref(), o) && named.get().matches(o))
        });
        let filtered = gtk::FilterListModel::new(Some(model.store().clone()), Some(filter.clone()));
        let sort = gtk::SortListModel::new(Some(filtered.clone()), Some(sorter.clone()));
        sort.set_section_sorter(Some(&sections));
        let selection = gtk::SingleSelection::new(Some(sort));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);

        let density = Density::default();
        let rows = gtk::SignalListItemFactory::new();
        let d = density.clone();
        rows.connect_setup(move |_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                session_row::setup(item, &d);
            }
        });
        let view = gtk::ListView::new(Some(selection.clone()), Some(rows));
        view.set_header_factory(Some(&header_factory(key.clone())));
        view.add_css_class("session-rack");
        // Tab sai da lista; as setas andam entre as linhas.
        view.set_tab_behavior(gtk::ListTabBehavior::Item);
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.update_property(&[gtk::accessible::Property::Label("Agent sessions")]);

        // Barra da lista: ordem e filtro por tipo de agente.
        let labels: Vec<&str> = SortKey::ALL.iter().map(|k| k.label()).collect();
        let (sort_menu, _) = dropdown(&labels, "Sort sessions by");
        let (agent_menu, agents) = dropdown(&[ALL_AGENTS], "Show agent type");
        let caption = gtk::Label::new(Some("Sort"));
        caption.add_css_class("session-caption");
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("session-controls");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        bar.append(&spacer);
        bar.append(&caption);
        bar.append(&sort_menu);
        bar.append(&agent_menu);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&bar);
        let filter_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        filter_bar.add_css_class("session-controls");
        let filter_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        filter_label.set_width_chars(1);
        let clear_filter = gtk::Button::with_label("Clear");
        clear_filter.set_size_request(44, 44);
        clear_filter.update_property(&[gtk::accessible::Property::Label("Clear session filter")]);
        filter_bar.append(&filter_label);
        filter_bar.append(&clear_filter);
        page.append(&filter_bar);
        page.append(&scrolled);
        let refresh: Rc<dyn Fn()> = {
            let (named, label, clear, filtered) = (
                active_filter.clone(),
                filter_label,
                clear_filter.clone(),
                filtered.downgrade(),
            );
            Rc::new(move || {
                let Some(filtered) = filtered.upgrade() else {
                    return;
                };
                let active = named.get();
                label.set_label(&format!(
                    "{} · {} results",
                    active.label(),
                    filtered.n_items()
                ));
                clear.set_visible(active != SessionFilter::All);
            })
        };
        refresh();
        let update_count = refresh.clone();
        filtered.connect_items_changed(move |_, _, _, _| update_count());
        let update_name = refresh.clone();
        filter.connect_changed(move |_, _| update_name());
        let (named, f, sel, view_for_clear) = (
            active_filter.clone(),
            filter.clone(),
            selection.clone(),
            view.clone(),
        );
        clear_filter.connect_clicked(move |_| {
            sel.set_selected(gtk::INVALID_LIST_POSITION);
            named.set(SessionFilter::All);
            f.changed(gtk::FilterChange::Different);
            view_for_clear.grab_focus();
        });
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

        let (k, s, v) = (key.clone(), sorter.clone(), view.clone());
        sort_menu.connect_selected_notify(move |dd| {
            let new = SortKey::ALL
                .get(dd.selected() as usize)
                .copied()
                .unwrap_or_default();
            if new == k.get() {
                return;
            }
            k.set(new);
            s.changed(gtk::SorterChange::Different);
            // Os titulos de secao dependem da ordem: refaz os cabecalhos.
            v.set_header_factory(Some(&header_factory(k.clone())));
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

        let scroll = scrolled.vadjustment();
        Self {
            root,
            view,
            store: model.store().clone(),
            sorter,
            filter,
            active_filter,
            clear_filter,
            selection,
            key,
            agents,
            agent_menu,
            sort_menu,
            scroll,
        }
    }

    pub fn set_filter(&self, filter: SessionFilter) {
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        self.active_filter.set(filter);
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn has_filter(&self) -> bool {
        self.active_filter.get() != SessionFilter::All
    }

    pub fn focus_filtered(&self) {
        if self.selection.n_items() == 0 && self.has_filter() {
            self.clear_filter.grab_focus();
        } else {
            self.view.grab_focus();
        }
    }

    /// Escolhe a ordem pelo menu (o mesmo caminho do clique).
    pub fn set_sort_key(&self, key: SortKey) {
        if let Some(pos) = SortKey::ALL.iter().position(|k| *k == key) {
            self.sort_menu.set_selected(pos as u32);
        }
    }

    /// `f` a cada ordem nova escolhida no menu.
    pub fn connect_sort_changed(&self, f: impl Fn(SortKey) + 'static) {
        self.sort_menu.connect_selected_notify(move |dd| {
            if let Some(k) = SortKey::ALL.get(dd.selected() as usize) {
                f(*k);
            }
        });
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
        let pos = chosen
            .and_then(|c| new.iter().position(|a| *a == c.as_str()))
            .map_or(0, |p| p as u32 + 1);
        self.agent_menu.set_selected(pos);
        // set_selected(0) sobre um 0 que ja era nao notifica: o filtro confere.
        self.filter.changed(gtk::FilterChange::Different);
    }

    /// Reordena so se alguma sessao mudou uma chave da ordem atual. A selecao
    /// acompanha a sessao, nao a posicao.
    pub fn update(&self, applied: &Applied) {
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
            let (adj, view, sel) = (
                self.scroll.clone(),
                self.view.clone(),
                self.selection.clone(),
            );
            gtk::glib::idle_add_local_once(move || match sel.selected() {
                gtk::INVALID_LIST_POSITION => adj.set_value(adj.lower()),
                pos => scroll_when_laid_out(&view, &adj, pos),
            });
        }
        if applied.is_empty() || !applied.touches(self.key.get().props()) {
            return;
        }
        let selected = self.selection.selected_item();
        self.sorter.changed(gtk::SorterChange::Different);
        let Some(sel) = selected else { return };
        if self.selection.selected_item().as_ref() != Some(&sel) {
            let n = self.selection.n_items();
            if let Some(pos) = (0..n).find(|&i| self.selection.item(i).as_ref() == Some(&sel)) {
                self.selection.set_selected(pos);
            }
        }
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
    /// alert or agent filter hides it. The caller must validate its identity.
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
        self.set_filter(SessionFilter::All);
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
            scroll_when_laid_out(&self.view, &self.scroll, pos);
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
fn scroll_when_laid_out(view: &gtk::ListView, adj: &gtk::Adjustment, pos: u32) {
    if view.height() > 0 && adj.upper() > 0.0 {
        view.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
        return;
    }
    let id: Rc<RefCell<Option<gtk::glib::SignalHandlerId>>> = Rc::default();
    let (v, slot) = (view.clone(), id.clone());
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
    id.replace(Some(handler));
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

    /// Zero results must retain a named filter; Clear must not choose an unrelated row.
    #[test]
    fn cc2_regression_counter_zero_and_clear_do_not_select_a_target() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let mut m = SessionModel::default();
            m.apply(&snap(vec![(10, 1)]), None);
            let list = SessionList::new(&m);
            let opened = Rc::new(Cell::new(0));
            let calls = opened.clone();
            list.connect_open(move |_| calls.set(calls.get() + 1));
            let filter = SessionFilter::Group(Group::Blocked);
            list.set_filter(filter);
            assert_eq!(list.selection.n_items(), 0);
            assert_eq!(list.active_filter.get(), filter);
            assert!(list.clear_filter.is_visible());
            assert!(list.selection.selected_item().is_none());
            assert!(!list.reveal_pid(999));
            assert_eq!(list.active_filter.get(), filter);
            list.clear_filter.emit_clicked();
            assert_eq!(list.active_filter.get(), SessionFilter::All);
            assert_eq!(list.selection.n_items(), 1);
            assert!(list.selection.selected_item().is_none());
            assert_eq!(opened.get(), 0);
        });
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
    fn flat_orders_keep_multiplexers_apart_at_the_end() {
        assert_eq!(sorted(SortKey::Memory, &rack()), [1, 5, 3, 2, 6, 4]);
        assert_eq!(sorted(SortKey::Cpu, &rack()), [3, 5, 2, 1, 6, 4]);
        // Sem diferenciar maiusculas: Alpha, s1, s2, s3, s6.
        assert_eq!(sorted(SortKey::Name, &rack()), [5, 1, 2, 3, 6, 4]);
        // HANDOFF primeiro, depois contexto maior primeiro, ausente no fim.
        assert_eq!(sorted(SortKey::Recommendation, &rack()), [5, 2, 3, 1, 6, 4]);
        assert_eq!(section_title(SortKey::Cpu, &rack()[0]).0, "Agent sessions");
    }

    #[test]
    fn ties_break_by_pid() {
        let a = obj(9, "claude", Group::Idle, |r| r.mem = 5);
        let b = obj(8, "claude", Group::Idle, |r| r.mem = 5);
        assert_eq!(sorted(SortKey::Memory, &[a, b]), [8, 9]);
    }

    #[test]
    fn named_filters_match_and_compose_with_agent_type() {
        let rows = rack();
        let pids = |filter: SessionFilter, agent: Option<&str>| -> Vec<u32> {
            rows.iter()
                .filter(|o| shows(agent, o) && filter.matches(o))
                .map(|o| o.pid())
                .collect()
        };
        assert_eq!(pids(SessionFilter::Group(Group::Busy), None), [3, 5]);
        assert_eq!(pids(SessionFilter::Group(Group::Busy), Some("pi")), [5]);
        assert!(pids(SessionFilter::Group(Group::Blocked), Some("pi")).is_empty());
        assert_eq!(pids(SessionFilter::Recommendation(REC_HANDOFF), None), [5]);
        let io = obj(7, "pi", Group::Busy, |r| r.io_bound = true);
        assert!(SessionFilter::IoBound.matches(&io));
        assert!(!SessionFilter::IoBound.matches(&rows[0]));
        assert_eq!(pids(SessionFilter::All, None).len(), rows.len());
    }

    /// Refiltering live rows and clearing a zero-result filter must not keep stale membership.
    #[test]
    fn named_filter_updates_and_clear_zero_results() {
        let o = obj(8, "pi", Group::Busy, |_| ());
        let mut row = Row {
            pid: 8,
            agent: "pi".into(),
            name: "s8".into(),
            group: Group::Busy.rank(),
            ..Row::default()
        };
        let accepts = |f: SessionFilter, agent: Option<&str>| shows(agent, &o) && f.matches(&o);
        assert!(accepts(SessionFilter::Group(Group::Busy), Some("pi")));
        assert!(!accepts(SessionFilter::IoBound, Some("pi")));
        row.io_bound = true;
        row.rec = REC_HANDOFF;
        row.group = Group::Blocked.rank();
        o.apply(&row);
        assert!(!accepts(SessionFilter::Group(Group::Busy), Some("pi")));
        assert!(accepts(SessionFilter::Group(Group::Blocked), Some("pi")));
        assert!(accepts(SessionFilter::IoBound, Some("pi")));
        assert!(!accepts(SessionFilter::IoBound, Some("claude")));
        assert!(accepts(
            SessionFilter::Recommendation(REC_HANDOFF),
            Some("pi")
        ));
        assert!(!accepts(
            SessionFilter::Recommendation(REC_HANDOFF),
            Some("claude")
        ));
        assert!(!accepts(SessionFilter::Group(Group::Busy), None));
        assert!(
            accepts(SessionFilter::All, None),
            "Clear recovers from zero results"
        );
        row.io_bound = false;
        row.rec = 0;
        o.apply(&row);
        assert!(!accepts(SessionFilter::IoBound, Some("pi")));
        assert!(!accepts(
            SessionFilter::Recommendation(REC_HANDOFF),
            Some("pi")
        ));
        row.group = Group::Multiplexer.rank();
        row.io_bound = true;
        row.rec = REC_HANDOFF;
        o.apply(&row);
        assert!(!accepts(SessionFilter::IoBound, None));
        assert!(!accepts(SessionFilter::Recommendation(REC_HANDOFF), None));
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
}
