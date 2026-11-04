//! Processes of the selected session (direction B): one row per process,
//! largest memory first, with a hatched memory bar (PSS solid, swapped-out
//! PSS hatched, like the map and the list) against the largest process of
//! the session. Comm and pid in mono, the state as a word, CPU in `n %`, and
//! `in RAM + swapped out` after the bar; the per-process action menu exists
//! only live.
//!
//! RSS aparece por processo e nunca somado: conta as paginas compartilhadas em
//! cada processo que as mapeia. PSS divide essas paginas e soma para a sessao;
//! a RSS e o resto da contabilidade ([`ProcRow`]) ficam no tooltip da linha.
//!
//! Rows are reused across ticks; a widget is born only when the session
//! gains processes. Columns line up through size groups, measured by Pango.

use crate::actions_ui;
use crate::format::{DASH, bytes, cpu_pct, human, sanitize};
use agentcenter_core::actions::Target;
use agentcenter_core::session::{IO_BOUND_D_THREADS, IO_BOUND_STALL_PCT};
use agentcenter_core::{IoStallSource, ProcessMetrics, Session};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

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

// ---- Pure part ---------------------------------------------------------------

/// `(PSS part, swap part)` of a process bar against `scale` (the largest PSS +
/// swap of the session); `None` (bare track) without a reading or a scale.
/// A stale scale never overflows: the swap part gives way.
pub fn bar_fractions(pss: Option<u64>, swap: Option<u64>, scale: u64) -> Option<(f64, f64)> {
    if scale == 0 || (pss.is_none() && swap.is_none()) {
        return None;
    }
    let s = scale as f64;
    let p = (pss.unwrap_or(0) as f64 / s).min(1.0);
    let w = (swap.unwrap_or(0) as f64 / s).min(1.0 - p);
    Some((p, w))
}

/// The `/proc/<pid>/stat` state letter as a word.
pub fn state_word(c: char) -> &'static str {
    match c {
        'R' => "running",
        'S' => "sleeping",
        'D' => "disk wait",
        'Z' => "zombie",
        'T' => "stopped",
        't' => "traced",
        'I' => "idle",
        'X' | 'x' => "dead",
        _ => "?",
    }
}

/// Text and bar of one process row, without GTK.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcBar {
    pub pid: u32,
    pub comm: String,
    pub pid_text: String,
    pub state: &'static str,
    pub cpu_text: String,
    /// `436 MiB + 523 MiB` (in RAM + swapped out); just the PSS when nothing
    /// is swapped; a dash without a reading.
    pub mem_text: String,
    pub fractions: Option<(f64, f64)>,
    /// The whole accounting of the process ([`ProcRow`]): RSS, disk, stall,
    /// threads, context switches.
    pub tooltip: String,
    pub a11y: String,
}

fn mem_of(p: &ProcessMetrics) -> u64 {
    p.pss_bytes.unwrap_or(0) + p.swap_pss_bytes.unwrap_or(0)
}

/// The rows of a session, largest PSS + swap first (ties by pid).
pub fn proc_bars(s: &Session, source: IoStallSource) -> Vec<ProcBar> {
    let scale = s.processes.iter().map(mem_of).max().unwrap_or(0);
    let mut procs: Vec<&ProcessMetrics> = s.processes.iter().collect();
    procs.sort_by(|a, b| mem_of(b).cmp(&mem_of(a)).then(a.pid.cmp(&b.pid)));
    procs
        .into_iter()
        .map(|p| {
            let size = |v: Option<u64>| v.map_or_else(|| DASH.to_string(), |b| bytes(b as f64));
            let (ram, out) = (size(p.pss_bytes), size(p.swap_pss_bytes));
            let mem_text = match (p.pss_bytes, p.swap_pss_bytes) {
                (None, None) => DASH.to_string(),
                (_, None | Some(0)) => ram.clone(),
                _ => format!("{ram} + {out}"),
            };
            let cpu_text = p
                .cpu_pct
                .map_or_else(|| DASH.to_string(), |c| format!("{c:.0} %"));
            let comm = sanitize(&p.comm).into_owned();
            let state = state_word(p.state);
            let a11y = format!(
                "{comm}, pid {}, {state}, CPU {cpu_text}, in RAM {ram}, swapped out {out}",
                p.pid
            );
            ProcBar {
                pid: p.pid,
                pid_text: p.pid.to_string(),
                state,
                cpu_text,
                mem_text,
                fractions: bar_fractions(p.pss_bytes, p.swap_pss_bytes, scale),
                tooltip: ProcRow::of(p, s.pss_bytes, source).a11y,
                a11y,
                comm,
            }
        })
        .collect()
}

// ---- Memory bar --------------------------------------------------------------

/// Width of the bar where it fits (the mock's 220 px), and the least it
/// shrinks to in a narrow detail.
pub const BAR_WIDTH: i32 = 220;
const BAR_MIN_WIDTH: i32 = 48;

/// Distance between hatch lines and their width, px (as the list's bars).
const HATCH_STEP_PX: f32 = 4.0;
const HATCH_LINE_PX: f32 = 1.5;

mod hatch {
    use super::*;

    #[derive(Default)]
    pub struct Hatch;

    #[glib::object_subclass]
    impl ObjectSubclass for Hatch {
        const NAME: &'static str = "AgentCenterProcHatch";
        type Type = super::Hatch;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("hatch");
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for Hatch {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_can_target(false);
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }
    }

    impl WidgetImpl for Hatch {
        /// The CSS background (the state fill) is drawn; the diagonal lines
        /// go on top in the node's own colour (white at the hatch opacity).
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let p = gsk::PathBuilder::new();
            let mut x = -h;
            while x < w {
                p.move_to(x, h);
                p.line_to(x + h, 0.0);
                x += HATCH_STEP_PX;
            }
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, w, h));
            snapshot.append_stroke(&p.to_path(), &gsk::Stroke::new(HATCH_LINE_PX), &obj.color());
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    /// Swapped-out part of a process bar.
    pub struct Hatch(ObjectSubclass<hatch::Hatch>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

mod proc_mem_bar {
    use super::*;

    #[derive(Default)]
    pub struct ProcMemBar {
        pub fractions: Cell<Option<(f64, f64)>>,
        pub fill: RefCell<Option<gtk::Box>>,
        pub hatch: RefCell<Option<Hatch>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ProcMemBar {
        const NAME: &'static str = "AgentCenterProcMemBar";
        type Type = super::ProcMemBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("procbar");
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for ProcMemBar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_valign(gtk::Align::Center);
            obj.set_can_target(false);
            let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            fill.add_css_class("fill");
            fill.set_parent(&*obj);
            let hatch: Hatch = glib::Object::new();
            hatch.set_parent(&*obj);
            self.fill.replace(Some(fill));
            self.hatch.replace(Some(hatch));
        }

        fn dispose(&self) {
            if let Some(f) = self.fill.take() {
                f.unparent();
            }
            if let Some(h) = self.hatch.take() {
                h.unparent();
            }
        }
    }

    impl WidgetImpl for ProcMemBar {
        fn measure(&self, o: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            // Height from the CSS `min-height`.
            match o {
                gtk::Orientation::Horizontal => (BAR_MIN_WIDTH, BAR_WIDTH, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let (p, s) = self.fractions.get().unwrap_or((0.0, 0.0));
            let w = f64::from(width);
            let x1 = (p * w).round() as i32;
            let x2 = ((p + s) * w).round() as i32;
            if let Some(f) = self.fill.borrow().as_ref() {
                f.size_allocate(&gtk::Allocation::new(0, 0, x1, height), -1);
            }
            if let Some(h) = self.hatch.borrow().as_ref() {
                h.size_allocate(&gtk::Allocation::new(x1, 0, x2 - x1, height), -1);
            }
        }
    }
}

glib::wrapper! {
    /// Memory bar of a process: track (CSS background), PSS part
    /// (`procbar > box.fill`) and hatched swap part (`procbar > hatch`), in
    /// the session's state fill (`procbar.busy` ...).
    pub struct ProcMemBar(ObjectSubclass<proc_mem_bar::ProcMemBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ProcMemBar {
    fn new() -> Self {
        glib::Object::new()
    }

    /// `(PSS part, swap part)` as drawn; `None` is the bare track.
    pub fn fractions(&self) -> Option<(f64, f64)> {
        self.imp().fractions.get()
    }

    fn set(&self, fractions: Option<(f64, f64)>, tone: &str) {
        if self.imp().fractions.replace(fractions) != fractions {
            self.queue_allocate();
        }
        if !self.has_css_class(tone) {
            self.set_css_classes(&[tone]);
        }
    }
}

// ---- Rows --------------------------------------------------------------------

/// One row's widgets.
pub struct BarRow {
    pub pid: u32,
    pub bar: ProcMemBar,
    /// Process actions (C12); only live.
    #[cfg_attr(not(test), expect(dead_code, reason = "the tests read the menu"))]
    pub menu: Option<gtk::MenuButton>,
    root: gtk::Box,
    comm: gtk::Label,
    pid_label: gtk::Label,
    state: gtk::Label,
    cpu: gtk::Label,
    mem: gtk::Label,
    target: Rc<RefCell<Option<(u32, String)>>>,
    /// The accessible label last written to `root`.
    spoken: RefCell<String>,
}

fn cell(classes: &[&str], xalign: f32, group: &gtk::SizeGroup) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(xalign);
    for c in classes {
        l.add_css_class(c);
    }
    group.add_widget(&l);
    l
}

fn set_text(l: &gtk::Label, t: &str) {
    if l.label() != t {
        l.set_label(t);
    }
}

/// Stores `new` in `cache` and says whether it differed: an accessible label
/// is rewritten (and announced again) only when its text changed.
fn replace_if_changed(cache: &RefCell<String>, new: &str) -> bool {
    if *cache.borrow() == new {
        return false;
    }
    cache.replace(new.to_string());
    true
}

/// The process rows of the detail.
pub struct ProcessBars {
    pub widget: gtk::Box,
    live: bool,
    /// comm, pid, state, CPU, memory text: one size group per column.
    groups: [gtk::SizeGroup; 5],
    rows: RefCell<Vec<BarRow>>,
    compact: Cell<bool>,
}

impl ProcessBars {
    pub fn new(live: bool) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("process-bars");
        widget.update_property(&[gtk::accessible::Property::Label("Processes of the session")]);
        Self {
            widget,
            live,
            groups: std::array::from_fn(|_| gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal)),
            rows: RefCell::default(),
            compact: Cell::new(false),
        }
    }

    fn row(&self) -> BarRow {
        let [g_comm, g_pid, g_state, g_cpu, g_mem] = &self.groups;
        let comm = cell(&["proc-comm", "numeric"], 0.0, g_comm);
        comm.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let pid_label = cell(&["proc-pid", "numeric"], 0.0, g_pid);
        let state = cell(&["proc-state"], 0.0, g_state);
        let cpu = cell(&["proc-cpu", "numeric"], 1.0, g_cpu);
        let bar = ProcMemBar::new();
        let mem = cell(&["proc-mem", "numeric"], 0.0, g_mem);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.add_css_class("proc-row");
        root.set_focusable(true);
        for w in [&comm, &pid_label, &state, &cpu] {
            root.append(w);
        }
        root.append(&bar);
        root.append(&mem);
        let target = Rc::new(RefCell::new(None));
        let menu = self.live.then(|| {
            let t = Rc::clone(&target);
            let b = actions_ui::menu_button("Process actions", move || {
                let (pid, name) = t.borrow().clone()?;
                Some((Target::Process(pid), name))
            });
            root.append(&b);
            b
        });
        let compact = self.compact.get();
        pid_label.set_visible(!compact);
        state.set_visible(!compact);
        BarRow {
            pid: 0,
            bar,
            menu,
            root,
            comm,
            pid_label,
            state,
            cpu,
            mem,
            target,
            spoken: RefCell::default(),
        }
    }

    /// Shows the session's processes in `tone` (the session's state fill
    /// class); reuses the rows it has.
    pub fn apply(&self, s: &Session, source: IoStallSource, tone: &str) {
        let bars = proc_bars(s, source);
        let mut rows = self.rows.borrow_mut();
        // A row belongs to its pid: rows of gone processes leave, new pids
        // get a row, and the survivors are reordered in place, so a popover
        // open on a row keeps pointing at its own process.
        let mut old = std::mem::take(&mut *rows);
        for b in &bars {
            let r = match old.iter().position(|r| r.pid == b.pid) {
                Some(i) => old.remove(i),
                None => {
                    let mut r = self.row();
                    r.pid = b.pid;
                    self.widget.append(&r.root);
                    r
                }
            };
            rows.push(r);
        }
        for r in old {
            self.widget.remove(&r.root);
        }
        let mut prev: Option<gtk::Widget> = None;
        for (r, b) in rows.iter().zip(&bars) {
            if r.root.prev_sibling() != prev {
                self.widget.reorder_child_after(&r.root, prev.as_ref());
            }
            prev = Some(r.root.clone().upcast());
            set_text(&r.comm, &b.comm);
            set_text(&r.pid_label, &b.pid_text);
            set_text(&r.state, b.state);
            set_text(&r.cpu, &b.cpu_text);
            set_text(&r.mem, &b.mem_text);
            r.bar.set(b.fractions, tone);
            if r.root.tooltip_text().as_deref() != Some(b.tooltip.as_str()) {
                r.root.set_tooltip_text(Some(&b.tooltip));
            }
            if replace_if_changed(&r.spoken, &b.a11y) {
                r.root
                    .update_property(&[gtk::accessible::Property::Label(&b.a11y)]);
            }
            r.target
                .replace(Some((b.pid, format!("{} (pid {})", b.comm, b.pid))));
        }
    }

    /// Narrow detail: pid and state go (they stay in the tooltip and the
    /// spoken label).
    pub fn set_compact(&self, on: bool) {
        self.compact.set(on);
        for r in self.rows.borrow().iter() {
            r.pid_label.set_visible(!on);
            r.state.set_visible(!on);
        }
    }

    #[cfg(test)]
    pub fn rows(&self) -> std::cell::Ref<'_, Vec<BarRow>> {
        self.rows.borrow()
    }

    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    pub fn clear(&self) {
        for r in self.rows.borrow_mut().drain(..) {
            self.widget.remove(&r.root);
        }
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
    fn bar_fractions_split_pss_and_swap_against_the_largest_process() {
        let m = 1u64 << 20;
        let (p, w) = bar_fractions(Some(436 * m), Some(523 * m), 959 * m).unwrap();
        assert!((p - 436.0 / 959.0).abs() < 1e-9 && (w - 523.0 / 959.0).abs() < 1e-9);
        assert_eq!(bar_fractions(Some(m), None, 4 * m), Some((0.25, 0.0)));
        assert_eq!(bar_fractions(None, Some(m), 4 * m), Some((0.0, 0.25)));
        assert_eq!(
            bar_fractions(None, None, 4 * m),
            None,
            "no reading, bare track"
        );
        assert_eq!(bar_fractions(Some(m), Some(m), 0), None, "no scale");
        // A stale scale never overflows the bar: the swap part gives way.
        assert_eq!(
            bar_fractions(Some(3 * m), Some(3 * m), 4 * m),
            Some((0.75, 0.25))
        );
    }

    fn with_memory(
        pid: u32,
        comm: &str,
        state: char,
        pss: Option<u64>,
        swap: Option<u64>,
    ) -> ProcessMetrics {
        let mut p = proc_(pid);
        p.comm = comm.into();
        p.state = state;
        (p.pss_bytes, p.swap_pss_bytes) = (pss, swap);
        p
    }

    /// The rows of the mock: largest PSS + swap first, `436 MiB + 523 MiB`,
    /// the state as a word, CPU in `n %`; a process with no reading keeps a
    /// dash and a bare track.
    #[test]
    fn proc_bars_sort_by_memory_and_read_like_the_mock() {
        let m = 1u64 << 20;
        let s = agentcenter_core::Session::aggregate(
            "claude".into(),
            agentcenter_core::adapters::AgentRole::Agent,
            7,
            16,
            vec![
                with_memory(7, "node", 'S', Some(50 * m), Some(0)),
                with_memory(8, "claude", 'S', Some(436 * m), Some(523 * m)),
                with_memory(9, "cargo", 'R', Some(38 * m), Some(15 * m)),
                with_memory(10, "git", 'D', None, None),
            ],
        );
        let bars = proc_bars(&s, IoStallSource::Delayacct);
        let pids: Vec<u32> = bars.iter().map(|b| b.pid).collect();
        assert_eq!(pids, [8, 9, 7, 10]);
        let top = &bars[0];
        assert_eq!(
            (
                top.comm.as_str(),
                top.pid_text.as_str(),
                top.state,
                top.cpu_text.as_str()
            ),
            ("claude", "8", "sleeping", "12 %")
        );
        assert_eq!(top.mem_text, "436 MiB + 523 MiB");
        let (p, w) = top.fractions.unwrap();
        assert!((p + w - 1.0).abs() < 1e-9, "the largest fills the bar");
        assert_eq!(bars[1].state, "running");
        assert_eq!(bars[2].mem_text, "50 MiB", "nothing swapped, no `+ 0`");
        assert_eq!(bars[2].fractions.unwrap().1, 0.0);
        assert_eq!((bars[3].mem_text.as_str(), bars[3].fractions), (DASH, None));
        assert_eq!(bars[3].state, "disk wait");
        assert!(
            top.a11y.starts_with(
                "claude, pid 8, sleeping, CPU 12 %, in RAM 436 MiB, swapped out 523 MiB"
            ),
            "{}",
            top.a11y
        );
        assert!(top.tooltip.contains("RSS 200 MiB"), "{}", top.tooltip);
    }

    fn session_of(procs: Vec<ProcessMetrics>) -> Session {
        Session::aggregate(
            "claude".into(),
            agentcenter_core::adapters::AgentRole::Agent,
            7,
            16,
            procs,
        )
    }

    /// A row belongs to a process, not to a position: after a re-sort the
    /// widget (and the open popover of its menu button) follows its pid.
    #[test]
    fn a_row_keeps_its_process_when_the_order_changes() {
        gtk::test_synced(|| {
            adw::init().expect("isolated test display required");
            let m = 1u64 << 20;
            let bars = ProcessBars::new(true);
            let first = session_of(vec![
                with_memory(7, "node", 'S', Some(50 * m), Some(0)),
                with_memory(8, "claude", 'S', Some(436 * m), Some(0)),
                with_memory(9, "cargo", 'R', Some(38 * m), Some(0)),
            ]);
            bars.apply(&first, IoStallSource::Delayacct, "busy");
            let held = |pid: u32| {
                let rows = bars.rows();
                let r = rows.iter().find(|r| r.pid == pid).expect("row");
                (
                    r.root.clone(),
                    r.menu.clone().expect("live menu"),
                    r.target.clone(),
                )
            };
            let (root9, menu9, target9) = held(9);
            // pid 9 now outgrows everything: it moves to the top.
            let second = session_of(vec![
                with_memory(7, "node", 'S', Some(50 * m), Some(0)),
                with_memory(8, "claude", 'S', Some(436 * m), Some(0)),
                with_memory(9, "cargo", 'R', Some(900 * m), Some(0)),
            ]);
            bars.apply(&second, IoStallSource::Delayacct, "busy");
            let rows = bars.rows();
            assert_eq!(rows.iter().map(|r| r.pid).collect::<Vec<_>>(), [9, 8, 7]);
            assert_eq!(rows[0].root, root9, "the widget moved with its process");
            assert_eq!(rows[0].menu.as_ref(), Some(&menu9));
            assert_eq!(
                target9.borrow().as_ref().map(|(pid, _)| *pid),
                Some(9),
                "the menu still acts on pid 9"
            );
            assert_eq!(bars.widget.first_child().as_ref(), Some(root9.upcast_ref()));
            let mut order = Vec::new();
            let mut w = bars.widget.first_child();
            while let Some(c) = w {
                order.push(c.clone());
                w = c.next_sibling();
            }
            let want: Vec<gtk::Widget> = rows.iter().map(|r| r.root.clone().upcast()).collect();
            assert_eq!(order, want, "the container order is the row order");
        });
    }

    #[test]
    fn a_label_is_written_only_when_its_text_changed() {
        let cache = RefCell::new(String::new());
        assert!(replace_if_changed(&cache, "a, pid 1"));
        assert!(
            !replace_if_changed(&cache, "a, pid 1"),
            "same text, no write"
        );
        assert!(replace_if_changed(&cache, "a, pid 2"));
    }

    #[test]
    fn state_letters_become_words() {
        let words: Vec<&str> = ['R', 'S', 'D', 'Z', 'T', 't', 'I', 'X', '?']
            .into_iter()
            .map(state_word)
            .collect();
        assert_eq!(
            words,
            [
                "running",
                "sleeping",
                "disk wait",
                "zombie",
                "stopped",
                "traced",
                "idle",
                "dead",
                "?"
            ]
        );
    }
}
