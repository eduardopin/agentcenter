//! Tabela de processos da sessao selecionada (C11): um `GtkColumnView`, o
//! unico lugar da GUI onde uma tabela e legitima (docs/DESIGN.md: a lista e
//! um rack, o detalhe e a contabilidade do kernel, processo a processo).
//!
//! RSS aparece por processo e nunca somado: conta as paginas compartilhadas em
//! cada processo que as mapeia. PSS divide essas paginas e soma para a sessao;
//! por isso a barra ambar da coluna PSS e a fatia do processo no PSS da
//! sessao, e a RSS nao tem barra.
//!
//! Como a lista: o modelo e sincronizado por pid a cada tick, objeto que
//! existe so troca a propriedade que mudou, e a celula, ligada por expressao
//! no `setup`, se redesenha sozinha. Nenhum widget nasce por tick.
//!
//! Colunas saem por largura ([`Column::tier`]), pelos breakpoints do bin do
//! detalhe; o que sai continua no rotulo acessivel da linha e, somado, no
//! cabecalho do detalhe (disco, stall, threads).

use crate::actions_ui;
use crate::format::{DASH, bytes, cpu_pct, human, sanitize};
use agentcenter_core::actions::Target;
use agentcenter_core::session::{IO_BOUND_D_THREADS, IO_BOUND_STALL_PCT};
use agentcenter_core::{IoStallSource, ProcessMetrics, Session};
use gtk::gio;
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use std::collections::{HashMap, HashSet};

/// Texto e chaves de uma linha da tabela, sem GTK: testavel sem display.
/// Chave de valor ausente e -1, abaixo de qualquer medida (como a lista).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProcRow {
    pub pid: u32,
    pub comm: String,
    pub pid_text: String,
    pub cpu: f64,
    pub cpu_text: String,
    pub pss: f64,
    pub pss_text: String,
    /// Fatia do PSS da sessao, 0..=1 (0 sem medida).
    pub pss_share: f64,
    pub rss: f64,
    pub rss_text: String,
    pub swap: f64,
    pub swap_text: String,
    pub read: f64,
    pub read_text: String,
    pub write: f64,
    pub write_text: String,
    pub stall: f64,
    pub stall_text: String,
    pub stall_hot: bool,
    pub threads: f64,
    pub threads_text: String,
    pub ctxsw: f64,
    pub ctxsw_text: String,
    pub a11y: String,
}

fn opt(v: Option<f64>, text: impl Fn(f64) -> String) -> (f64, String) {
    v.map_or_else(|| (-1.0, DASH.to_string()), |x| (x, text(x)))
}

fn rate(b: f64) -> String {
    format!("{}/s", bytes(b))
}

impl ProcRow {
    /// `session_pss`: soma do PSS da sessao, a capacidade da barra.
    pub fn of(p: &ProcessMetrics, session_pss: Option<u64>, source: IoStallSource) -> Self {
        let (cpu, cpu_text) = opt(p.cpu_pct, cpu_pct);
        let b = |v: Option<u64>| opt(v.map(|x| x as f64), bytes);
        let (pss, pss_text) = b(p.pss_bytes);
        let (rss, rss_text) = b(p.rss_bytes);
        let (swap, swap_text) = b(p.swap_pss_bytes);
        let (read, read_text) = opt(p.read_bps, rate);
        let (write, write_text) = opt(p.write_bps, rate);
        let pss_share = match (p.pss_bytes, session_pss) {
            (Some(v), Some(t)) if t > 0 => (v as f64 / t as f64).clamp(0.0, 1.0),
            _ => 0.0,
        };
        // Stall: % do intervalo esperando disco (delayacct), ou threads em D
        // no fallback, como a faixa (`D:n`). Quente no limiar do io_bound.
        let (stall, stall_text, stall_hot) = match (source, p.stall_pct, p.threads_d) {
            (IoStallSource::Delayacct, Some(s), _) => {
                (s, format!("{s:.0}%"), s >= IO_BOUND_STALL_PCT)
            }
            (IoStallSource::DState, _, Some(d)) => {
                (f64::from(d), format!("D:{d}"), d >= IO_BOUND_D_THREADS)
            }
            _ => (-1.0, DASH.to_string(), false),
        };
        let ctxsw = p.voluntary_ctxt_switches + p.nonvoluntary_ctxt_switches;
        let ctxsw_text = format!(
            "{} / {}",
            human(p.voluntary_ctxt_switches),
            human(p.nonvoluntary_ctxt_switches)
        );
        let comm = sanitize(&p.comm).into_owned();
        let a11y = format!(
            "{comm}, pid {}, CPU {cpu_text}, PSS {pss_text}, RSS {rss_text}, swap PSS {swap_text}, \
             read {read_text}, write {write_text}, stall {stall_text}, {} threads, \
             context switches {ctxsw_text} voluntary and involuntary",
            p.pid, p.threads
        );
        Self {
            pid: p.pid,
            pid_text: format!("pid {}", p.pid),
            comm,
            cpu,
            cpu_text,
            pss,
            pss_text,
            pss_share,
            rss,
            rss_text,
            swap,
            swap_text,
            read,
            read_text,
            write,
            write_text,
            stall,
            stall_text,
            stall_hot,
            threads: f64::from(p.threads),
            threads_text: p.threads.to_string(),
            ctxsw: ctxsw as f64,
            ctxsw_text,
            a11y,
        }
    }
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ProcessObject)]
    pub struct ProcessObject {
        #[property(get, set)]
        pub(super) pid: Cell<u32>,
        #[property(get, set)]
        pub(super) comm: RefCell<String>,
        #[property(get, set)]
        pub(super) pid_text: RefCell<String>,
        #[property(get, set)]
        pub(super) cpu: Cell<f64>,
        #[property(get, set)]
        pub(super) cpu_text: RefCell<String>,
        #[property(get, set)]
        pub(super) pss: Cell<f64>,
        #[property(get, set)]
        pub(super) pss_text: RefCell<String>,
        #[property(get, set)]
        pub(super) pss_share: Cell<f64>,
        #[property(get, set)]
        pub(super) rss: Cell<f64>,
        #[property(get, set)]
        pub(super) rss_text: RefCell<String>,
        #[property(get, set)]
        pub(super) swap: Cell<f64>,
        #[property(get, set)]
        pub(super) swap_text: RefCell<String>,
        #[property(get, set)]
        pub(super) read: Cell<f64>,
        #[property(get, set)]
        pub(super) read_text: RefCell<String>,
        #[property(get, set)]
        pub(super) write: Cell<f64>,
        #[property(get, set)]
        pub(super) write_text: RefCell<String>,
        #[property(get, set)]
        pub(super) stall: Cell<f64>,
        #[property(get, set)]
        pub(super) stall_text: RefCell<String>,
        #[property(get, set)]
        pub(super) stall_hot: Cell<bool>,
        #[property(get, set)]
        pub(super) threads: Cell<f64>,
        #[property(get, set)]
        pub(super) threads_text: RefCell<String>,
        #[property(get, set)]
        pub(super) ctxsw: Cell<f64>,
        #[property(get, set)]
        pub(super) ctxsw_text: RefCell<String>,
        #[property(get, set)]
        pub(super) a11y: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProcessObject {
        const NAME: &'static str = "AgentCenterProcessObject";
        type Type = super::ProcessObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ProcessObject {}
}

glib::wrapper! {
    pub struct ProcessObject(ObjectSubclass<imp::ProcessObject>);
}

impl ProcessObject {
    pub fn new(row: &ProcRow) -> Self {
        let o: Self = glib::Object::new();
        o.apply(row);
        o
    }

    /// Troca so o que mudou; `true` se algo mudou.
    pub fn apply(&self, r: &ProcRow) -> bool {
        let _freeze = self.freeze_notify();
        let mut changed = false;
        macro_rules! sync {
            ($get:ident, $set:ident, $v:expr) => {
                if self.$get() != $v {
                    self.$set($v);
                    changed = true;
                }
            };
        }
        sync!(pid, set_pid, r.pid);
        sync!(comm, set_comm, r.comm.as_str());
        sync!(pid_text, set_pid_text, r.pid_text.as_str());
        sync!(cpu, set_cpu, r.cpu);
        sync!(cpu_text, set_cpu_text, r.cpu_text.as_str());
        sync!(pss, set_pss, r.pss);
        sync!(pss_text, set_pss_text, r.pss_text.as_str());
        sync!(pss_share, set_pss_share, r.pss_share);
        sync!(rss, set_rss, r.rss);
        sync!(rss_text, set_rss_text, r.rss_text.as_str());
        sync!(swap, set_swap, r.swap);
        sync!(swap_text, set_swap_text, r.swap_text.as_str());
        sync!(read, set_read, r.read);
        sync!(read_text, set_read_text, r.read_text.as_str());
        sync!(write, set_write, r.write);
        sync!(write_text, set_write_text, r.write_text.as_str());
        sync!(stall, set_stall, r.stall);
        sync!(stall_text, set_stall_text, r.stall_text.as_str());
        sync!(stall_hot, set_stall_hot, r.stall_hot);
        sync!(threads, set_threads, r.threads);
        sync!(threads_text, set_threads_text, r.threads_text.as_str());
        sync!(ctxsw, set_ctxsw, r.ctxsw);
        sync!(ctxsw_text, set_ctxsw_text, r.ctxsw_text.as_str());
        sync!(a11y, set_a11y, r.a11y.as_str());
        changed
    }
}

/// O modelo, sem widget: testavel sem display.
pub struct ProcessModel {
    store: gio::ListStore,
    by_pid: HashMap<u32, ProcessObject>,
}

impl Default for ProcessModel {
    fn default() -> Self {
        Self {
            store: gio::ListStore::new::<ProcessObject>(),
            by_pid: HashMap::new(),
        }
    }
}

impl ProcessModel {
    pub fn store(&self) -> &gio::ListStore {
        &self.store
    }

    pub fn len(&self) -> usize {
        self.by_pid.len()
    }

    /// Sincroniza com os processos da sessao. `true` se algo mudou (a ordem
    /// da tabela pode ter mudado).
    pub fn apply(&mut self, s: &Session, source: IoStallSource) -> bool {
        let live: HashSet<u32> = s.processes.iter().map(|p| p.pid).collect();
        let mut changed = false;
        if self.by_pid.keys().any(|p| !live.contains(p)) {
            for i in (0..self.store.n_items()).rev() {
                let Some(o) = self.store.item(i).and_downcast::<ProcessObject>() else {
                    continue;
                };
                if !live.contains(&o.pid()) {
                    self.store.remove(i);
                    self.by_pid.remove(&o.pid());
                    changed = true;
                }
            }
        }
        let mut new = Vec::new();
        for p in &s.processes {
            let row = ProcRow::of(p, s.pss_bytes, source);
            match self.by_pid.get(&p.pid) {
                Some(o) => changed |= o.apply(&row),
                None => {
                    let o = ProcessObject::new(&row);
                    self.by_pid.insert(p.pid, o.clone());
                    new.push(o);
                }
            }
        }
        if !new.is_empty() {
            self.store.splice(self.store.n_items(), 0, &new);
            changed = true;
        }
        changed
    }

    pub fn clear(&mut self) {
        if !self.by_pid.is_empty() {
            self.by_pid.clear();
            self.store.remove_all();
        }
    }
}

/// Uma coluna: id (classe CSS e nome no teste), titulo, propriedade de texto,
/// chave numerica da ordem e a faixa de largura em que aparece.
pub struct Column {
    pub id: &'static str,
    pub title: &'static str,
    pub text: &'static str,
    pub key: &'static str,
    /// 0: sempre; 1: some abaixo de [`MEDIUM_MAX_SP`]; 2: some abaixo de
    /// [`WIDE_MAX_SP`]; 3: some abaixo de [`FULL_MAX_SP`].
    pub tier: u8,
}

/// Largura do detalhe (sp) abaixo da qual a coluna de faixa 3 sai: trocas de
/// contexto. Com a coluna de acoes do C12 a tabela inteira pede ~886 px; sem
/// esta faixa, o detalhe de 840 px rolava de lado.
pub const FULL_MAX_SP: f64 = 900.0;
/// Abaixo desta saem tambem as de faixa 2: disco, stall e threads.
pub const WIDE_MAX_SP: f64 = 780.0;
/// Abaixo desta sai tambem o Swap PSS: ficam processo, CPU, PSS e RSS.
pub const MEDIUM_MAX_SP: f64 = 480.0;
// `breakpoints` depende da ordem: do mais largo ao mais estreito.
const _: () = assert!(FULL_MAX_SP > WIDE_MAX_SP && WIDE_MAX_SP > MEDIUM_MAX_SP);

pub const COLUMNS: [Column; 10] = [
    Column {
        id: "process",
        title: "Process",
        text: "comm",
        key: "comm",
        tier: 0,
    },
    Column {
        id: "cpu",
        title: "CPU",
        text: "cpu-text",
        key: "cpu",
        tier: 0,
    },
    Column {
        id: "pss",
        title: "PSS",
        text: "pss-text",
        key: "pss",
        tier: 0,
    },
    Column {
        id: "rss",
        title: "RSS",
        text: "rss-text",
        key: "rss",
        tier: 0,
    },
    Column {
        id: "swap",
        title: "Swap PSS",
        text: "swap-text",
        key: "swap",
        tier: 1,
    },
    Column {
        id: "read",
        title: "Read",
        text: "read-text",
        key: "read",
        tier: 2,
    },
    Column {
        id: "write",
        title: "Write",
        text: "write-text",
        key: "write",
        tier: 2,
    },
    Column {
        id: "stall",
        title: "Stall",
        text: "stall-text",
        key: "stall",
        tier: 2,
    },
    Column {
        id: "threads",
        title: "Threads",
        text: "threads-text",
        key: "threads",
        tier: 2,
    },
    Column {
        id: "ctxsw",
        title: "Ctx switches",
        text: "ctxsw-text",
        key: "ctxsw",
        tier: 3,
    },
];

fn bind(item: &gtk::ListItem, prop: &str, target: &impl IsA<glib::Object>, target_prop: &str) {
    item.property_expression("item")
        .chain_property::<ProcessObject>(prop)
        .bind(target, target_prop, gtk::Widget::NONE);
}

/// Barra da fatia de PSS: o mesmo `levelbar.meter` das linhas (sessions.css),
/// sem as marcas de nivel do GTK.
fn share_meter() -> gtk::LevelBar {
    let bar = gtk::LevelBar::new();
    bar.set_min_value(0.0);
    bar.set_max_value(1.0);
    for name in [
        gtk::LEVEL_BAR_OFFSET_LOW,
        gtk::LEVEL_BAR_OFFSET_HIGH,
        gtk::LEVEL_BAR_OFFSET_FULL,
    ] {
        bar.remove_offset_value(Some(name));
    }
    // Keep the numerical PSS value and all five narrow columns visible.
    // A 40px decorative share track made the table + margins measure 380px.
    bar.set_size_request(20, -1);
    bar.set_valign(gtk::Align::Center);
    bar.add_css_class("meter");
    bar.set_accessible_role(gtk::AccessibleRole::Presentation);
    bar
}

fn cell_factory(c: &'static Column) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let value = gtk::Label::new(None);
        value.add_css_class("proc-value");
        match c.id {
            "process" => {
                value.add_css_class("monospace");
                value.set_xalign(0.0);
                value.set_ellipsize(gtk::pango::EllipsizeMode::End);
                bind(item, "comm", &value, "label");
                bind(item, "comm", &value, "tooltip-text");
                let pid = gtk::Label::new(None);
                pid.set_xalign(0.0);
                pid.add_css_class("proc-pid");
                pid.add_css_class("monospace");
                bind(item, "pid-text", &pid, "label");
                let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
                b.append(&value);
                b.append(&pid);
                item.set_child(Some(&b));
            }
            "pss" => {
                value.set_xalign(1.0);
                value.set_hexpand(true);
                bind(item, c.text, &value, "label");
                let bar = share_meter();
                bind(item, "pss-share", &bar, "value");
                let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                b.append(&bar);
                b.append(&value);
                b.set_tooltip_text(Some(
                    "PSS, and the bar: this process's share of the session's PSS",
                ));
                item.set_child(Some(&b));
            }
            id => {
                value.set_xalign(1.0);
                bind(item, c.text, &value, "label");
                if id == "stall" {
                    item.property_expression("item")
                        .chain_property::<ProcessObject>("stall-hot")
                        .chain_closure_with_callback(|args| {
                            let hot = args[1].get::<bool>().unwrap_or_default();
                            let mut v = vec!["proc-value".to_string()];
                            if hot {
                                v.push("hot".into());
                            }
                            v
                        })
                        .bind(&value, "css-classes", gtk::Widget::NONE);
                }
                item.set_child(Some(&value));
            }
        }
    });
    f
}

fn sorter(c: &'static Column) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let (Some(a), Some(b)) = (
            a.downcast_ref::<ProcessObject>(),
            b.downcast_ref::<ProcessObject>(),
        ) else {
            return gtk::Ordering::Equal;
        };
        let o = if c.key == "comm" {
            a.comm().to_lowercase().cmp(&b.comm().to_lowercase())
        } else {
            let k = |o: &ProcessObject| o.property::<f64>(c.key);
            k(a).total_cmp(&k(b))
        };
        o.then_with(|| a.pid().cmp(&b.pid())).into()
    })
}

pub struct ProcessTable {
    pub widget: gtk::ScrolledWindow,
    pub columns: Vec<(u8, gtk::ColumnViewColumn)>,
    model: ProcessModel,
    sort: gtk::SortListModel,
}

impl ProcessTable {
    pub fn new() -> Self {
        let model = ProcessModel::default();
        let view = gtk::ColumnView::new(None::<gtk::NoSelection>);
        view.add_css_class("process-table");
        view.set_reorderable(false);
        view.set_show_column_separators(false);
        view.set_tab_behavior(gtk::ListTabBehavior::Item);
        view.update_property(&[gtk::accessible::Property::Label("Processes of the session")]);
        let mut columns = Vec::new();
        for c in &COLUMNS {
            let col = gtk::ColumnViewColumn::new(Some(c.title), Some(cell_factory(c)));
            col.set_id(Some(c.id));
            col.set_sorter(Some(&sorter(c)));
            col.set_resizable(false);
            if c.id == "process" {
                col.set_expand(true);
            }
            view.append_column(&col);
            columns.push((c.tier, col));
        }
        // Acoes por processo (C12): fora de COLUMNS, sem titulo nem ordem.
        let acts = gtk::SignalListItemFactory::new();
        acts.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let weak = item.downgrade();
            let b = actions_ui::menu_button("Process actions", move || {
                let o = weak.upgrade()?.item().and_downcast::<ProcessObject>()?;
                Some((
                    Target::Process(o.pid()),
                    format!("{} (pid {})", o.comm(), o.pid()),
                ))
            });
            item.set_child(Some(&b));
        });
        let col = gtk::ColumnViewColumn::new(None, Some(acts));
        col.set_id(Some("actions"));
        col.set_resizable(false);
        view.append_column(&col);

        let rows = gtk::SignalListItemFactory::new();
        rows.connect_setup(|_, row| {
            if let Some(row) = row.downcast_ref::<gtk::ColumnViewRow>() {
                row.set_activatable(false);
                row.set_selectable(false);
                row.property_expression("item")
                    .chain_property::<ProcessObject>("a11y")
                    .bind(row, "accessible-label", gtk::Widget::NONE);
            }
        });
        view.set_row_factory(Some(&rows));

        let sort = gtk::SortListModel::new(Some(model.store().clone()), view.sorter());
        view.set_model(Some(&gtk::NoSelection::new(Some(sort.clone()))));
        // Padrao: maior PSS primeiro (quem segura a memoria da sessao).
        if let Some((_, pss)) = columns
            .iter()
            .find(|(_, c)| c.id().as_deref() == Some("pss"))
        {
            view.sort_by_column(Some(pss), gtk::SortType::Descending);
        }

        let widget = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        Self {
            widget,
            columns,
            model,
            sort,
        }
    }

    /// Breakpoints do bin do detalhe: as colunas que saem em cada largura.
    pub fn breakpoints(&self) -> Vec<adw::Breakpoint> {
        // Do mais largo ao mais estreito: no bin vence o ultimo que casa, e
        // cada um esconde tudo da sua faixa para cima.
        [(FULL_MAX_SP, 3u8), (WIDE_MAX_SP, 2u8), (MEDIUM_MAX_SP, 1u8)]
            .iter()
            .map(|&(max, from)| {
                let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                    adw::BreakpointConditionLengthType::MaxWidth,
                    max,
                    adw::LengthUnit::Sp,
                ));
                for (tier, col) in &self.columns {
                    if *tier >= from {
                        bp.add_setter(col, "visible", Some(&false.to_value()));
                    }
                }
                bp
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.model.len()
    }

    /// Aplica os processos da sessao; reordena so se algo mudou.
    pub fn apply(&mut self, s: &Session, source: IoStallSource) {
        if self.model.apply(s, source)
            && let Some(sorter) = self.sort.sorter()
        {
            sorter.changed(gtk::SorterChange::Different);
        }
    }

    pub fn clear(&mut self) {
        self.model.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_(pid: u32) -> ProcessMetrics {
        ProcessMetrics {
            pid,
            ppid: 1,
            comm: "node".into(),
            state: 'S',
            uid: 1000,
            threads: 3,
            cpu_pct: Some(12.5),
            rss_bytes: Some(200 << 20),
            pss_bytes: Some(50 << 20),
            swap_bytes: Some(0),
            swap_pss_bytes: Some(0),
            read_bps: Some(0.0),
            write_bps: Some(131_072.0),
            rchar_bps: None,
            wchar_bps: None,
            stall_pct: Some(25.0),
            threads_d: None,
            voluntary_ctxt_switches: 64_421,
            nonvoluntary_ctxt_switches: 6_471,
        }
    }

    #[test]
    fn row_shows_rss_per_process_and_pss_share_of_the_session() {
        let r = ProcRow::of(&proc_(7), Some(200 << 20), IoStallSource::Delayacct);
        assert_eq!(r.rss_text, "200 MiB");
        // O formato de toda a GUI (format::bytes): inteiro de 10 em diante,
        // uma casa abaixo de 10.
        assert_eq!(r.pss_text, "50 MiB");
        let mut small = proc_(8);
        small.pss_bytes = Some(5 << 20);
        let s = ProcRow::of(&small, Some(200 << 20), IoStallSource::Delayacct);
        assert_eq!(s.pss_text, "5.0 MiB");
        assert!((r.pss_share - 0.25).abs() < 1e-9, "{}", r.pss_share);
        assert_eq!(r.cpu_text, "12%");
        assert_eq!(r.write_text, "128 KiB/s");
        assert_eq!((r.stall_text.as_str(), r.stall_hot), ("25%", true));
        assert_eq!(r.ctxsw_text, "64.4k / 6.5k");
        assert_eq!(r.pid_text, "pid 7");
        assert!(r.a11y.contains("RSS 200 MiB"), "{}", r.a11y);
    }

    #[test]
    fn absent_values_are_a_dash_never_zero() {
        let mut p = proc_(7);
        (p.cpu_pct, p.rss_bytes, p.pss_bytes, p.swap_pss_bytes) = (None, None, None, None);
        (p.read_bps, p.write_bps, p.stall_pct) = (None, None, None);
        let r = ProcRow::of(&p, Some(1 << 20), IoStallSource::Delayacct);
        for t in [
            &r.cpu_text,
            &r.pss_text,
            &r.rss_text,
            &r.swap_text,
            &r.read_text,
            &r.write_text,
            &r.stall_text,
        ] {
            assert_eq!(t, DASH);
        }
        assert_eq!(
            (r.cpu, r.pss, r.stall, r.pss_share),
            (-1.0, -1.0, -1.0, 0.0)
        );
    }

    #[test]
    fn d_state_fallback_counts_threads_waiting_on_disk() {
        let mut p = proc_(7);
        (p.stall_pct, p.threads_d) = (None, Some(2));
        let r = ProcRow::of(&p, None, IoStallSource::DState);
        assert_eq!((r.stall_text.as_str(), r.stall_hot), ("D:2", true));
        assert_eq!(r.pss_share, 0.0, "no session PSS, no bar");
    }

    #[test]
    fn model_keeps_objects_by_pid_and_reports_change() {
        let mut s = agentcenter_core::Session::aggregate(
            "claude".into(),
            agentcenter_core::adapters::AgentRole::Agent,
            10,
            16,
            vec![proc_(10), proc_(11)],
        );
        let mut m = ProcessModel::default();
        assert!(m.apply(&s, IoStallSource::Delayacct));
        let first = m.store().item(0).unwrap();
        assert!(
            !m.apply(&s, IoStallSource::Delayacct),
            "same tick, no change"
        );
        s.processes[0].cpu_pct = Some(50.0);
        assert!(m.apply(&s, IoStallSource::Delayacct));
        assert_eq!(m.store().item(0).unwrap(), first, "object reused");
        s.processes.remove(1);
        assert!(m.apply(&s, IoStallSource::Delayacct));
        assert_eq!(m.len(), 1);
        m.clear();
        assert_eq!(m.store().n_items(), 0);
    }

    #[test]
    fn every_column_reads_real_properties() {
        let o = ProcessObject::new(&ProcRow::default());
        for c in &COLUMNS {
            assert!(o.find_property(c.text).is_some(), "{}", c.text);
            assert!(o.find_property(c.key).is_some(), "{}", c.key);
        }
        assert!(
            COLUMNS.iter().filter(|c| c.tier == 0).count() == 4,
            "process, CPU, PSS and RSS stay at every width"
        );
    }

    #[test]
    fn context_switches_leave_first_below_the_full_width() {
        // A coluna de trocas de contexto e a unica da faixa 3: sai primeiro,
        // abaixo de FULL_MAX_SP, para o detalhe de 840 px nao rolar de lado.
        let top: Vec<_> = COLUMNS
            .iter()
            .filter(|c| c.tier == 3)
            .map(|c| c.id)
            .collect();
        assert_eq!(top, ["ctxsw"]);
        assert!(COLUMNS.iter().all(|c| c.tier <= 3));
    }
}
