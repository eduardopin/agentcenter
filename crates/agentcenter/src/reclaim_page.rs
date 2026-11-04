//! Reclaim page (C12, direction B): the memory map in reclaim mode, the idle
//! sessions as a checklist, and "End N sessions…". Nothing ends without the
//! confirmation of the window's action flow, and nothing here fires alone.
//!
//! Idle is the core's rule ([`agentcenter_core::reclaim`]), the same as
//! `agentcenter-cli reclaim`: the window is the last [`DEFAULT_WINDOW_S`]
//! ticks with rates, kept as `Arc<Snapshot>` (the feed's object, no copy).
//! The `CpuWindow` is rebuilt from that window on every tick, so it slides.
//! Until it fills, the page says it is watching, like the CLI waits 10 s.
//!
//! ```text
//! 26 idle sessions hold 19.0 GiB            Idle sessions by memory     26
//! ┌map: candidates teal, checkable;┐        [x] kiln-cache-layer  ▮▮▮▯ 1.09 GiB >
//! │ the rest dimmed, no labels     │        [ ] ledger-i18n       ▮▮▯▯ 0.94 GiB >
//! └────────────────────────────────┘           [End 1 session… · frees 1.1 GiB]
//! ```
//!
//! The checks live here and are pushed to both views: a tile click
//! ([`MemoryMap::connect_check_toggled`]) updates the row, a row check
//! updates the tile, and both update the action bar.

use crate::actions_ui;
use crate::format::{DASH, Row, bytes, cpu_pct, sanitize};
use crate::memory_map::{MapInput, MemoryMap, gib, gib1};
use adw::prelude::*;
use agentcenter_core::actions::{Action, Target};
use agentcenter_core::reclaim::{self, DEFAULT_WINDOW_S, IDLE_MAX_CPU_PCT};
use agentcenter_core::{CpuWindow, IdleBy, Reclaim, Snapshot};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

/// Kernel identity guards live navigation; overlay fallback is fixture-only at the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionLink {
    pub pid: u32,
    pub overlay_id: Option<String>,
    pub starttime: Option<u64>,
}

impl SessionLink {
    pub fn matches(&self, snap: &Snapshot) -> bool {
        if let Some(start) = self.starttime {
            return snap.sessions.iter().any(|s| {
                s.pid == self.pid
                    && s.starttime == Some(start)
                    && self
                        .overlay_id
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .is_none_or(|id| {
                            s.overlay
                                .as_ref()
                                .and_then(|o| o.agent_session_id.as_deref())
                                == Some(id)
                        })
            });
        }
        let Some(id) = self.overlay_id.as_deref().filter(|id| !id.is_empty()) else {
            return false;
        };
        snap.sessions.iter().any(|s| {
            s.pid == self.pid
                && s.overlay
                    .as_ref()
                    .and_then(|o| o.agent_session_id.as_deref())
                    == Some(id)
        })
    }
}

/// Ticks da janela, os mesmos do `agentcenter-cli reclaim` padrao.
pub const WINDOW_TICKS: usize = DEFAULT_WINDOW_S as usize;

/// Empty state: the window is full and no session is idle.
pub const EMPTY_TEXT: &str = "No idle sessions. Every session is working or waiting for you.";
/// List column width at full size, and the page width under which the
/// list goes below the map.
const SIDE_WIDTH_PX: i32 = 440;
const NARROW_MAX_WIDTH_PX: f64 = 720.0;
const MIN_WIDTH_PX: i32 = 300;
const MIN_HEIGHT_PX: i32 = 240;
const MAP_MIN_HEIGHT_PX: i32 = 160;
const NARROW_MAP_PX: i32 = 96;
/// Memory bar in a row: width (narrow page: [`NARROW_BAR_PX`]) and height.
const BAR_PX: i32 = 150;
const NARROW_BAR_PX: i32 = 120;
const BAR_HEIGHT_PX: i32 = 8;
/// Hatch period and stripe width, px (as on the map).
const HATCH_STEP_PX: f64 = 6.0;
const HATCH_LINE_PX: f64 = 2.4;
/// "End N sessions…": wait for the window's flow to free itself after a
/// dialog closed; stop when a request brought no dialog in this time.
const NEXT_REQUEST_DELAY: Duration = Duration::from_millis(300);
const NO_DIALOG_GRACE: Duration = Duration::from_secs(2);

const STACK_WATCHING: &str = "watching";
const STACK_EMPTY: &str = "empty";
const STACK_LIST: &str = "list";

/// Janela deslizante dos ultimos [`WINDOW_TICKS`] snapshots com taxas.
#[derive(Default)]
pub struct Ring {
    snaps: VecDeque<Arc<Snapshot>>,
}

impl Ring {
    /// Guarda o tick; o primeiro do sampler (sem taxas) nao conta.
    pub fn push(&mut self, snap: &Arc<Snapshot>) {
        if snap.interval_ms == 0 {
            return;
        }
        if self.snaps.len() == WINDOW_TICKS {
            self.snaps.pop_front();
        }
        self.snaps.push_back(Arc::clone(snap));
    }

    pub fn len(&self) -> usize {
        self.snaps.len()
    }

    /// O reclaim da janela cheia, como o CLI: o ultimo tick e o `CpuWindow`
    /// dos [`WINDOW_TICKS`]. `None` ate encher.
    pub fn reclaim(&self, own_pid: Option<u32>) -> Option<Reclaim> {
        if self.snaps.len() < WINDOW_TICKS {
            return None;
        }
        let mut w = CpuWindow::new();
        for s in &self.snaps {
            w.observe(s);
        }
        let last = self.snaps.back()?;
        Some(reclaim::reclaim(last, &w, own_pid))
    }
}

/// Texto de uma linha, sem GTK.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateText {
    pub pid: u32,
    pub link: SessionLink,
    pub name: String,
    pub meta: String,
    pub why: String,
    pub memory: String,
    pub memory_tooltip: String,
    /// PSS + SwapPss, the bytes ending the session gives back.
    pub bytes: u64,
    /// Bar length: this session against the largest candidate, 0..=1.
    pub share: f64,
    /// Swapped-out part of `bytes`, 0..=1 (hatched in the bar).
    pub swap_share: f64,
    pub a11y: String,
}

/// O texto de cada candidata, na ordem do core. O nome e o caminho vem da
/// mesma [`Row`] da lista, para a sessao se chamar igual nas duas paginas.
pub fn texts(r: &Reclaim, snap: &Snapshot, home: Option<&str>) -> Vec<CandidateText> {
    let source = snap.machine.io_stall_source;
    let largest = r
        .sessions
        .iter()
        .map(|c| c.memory_bytes())
        .max()
        .unwrap_or(0);
    r.sessions
        .iter()
        .map(|c| {
            let session = snap.sessions.iter().find(|s| s.pid == c.pid);
            let row = session.map(|s| Row::from_session(s, source, home));
            let name = row
                .as_ref()
                .map_or_else(|| sanitize(&c.agent).into_owned(), |r| r.name.clone());
            let cwd = row.as_ref().map(|r| r.cwd.clone()).unwrap_or_default();
            let agent = sanitize(&c.agent);
            let meta = if cwd.is_empty() {
                format!("{agent} · pid {}", c.pid)
            } else {
                format!("{agent} · {cwd} · pid {}", c.pid)
            };
            let why = match (c.idle_by, c.max_cpu_pct) {
                (IdleBy::Overlay, _) => "Idle by its own status".to_string(),
                (IdleBy::Cpu, Some(m)) => {
                    format!("Quiet: CPU at most {} for {DEFAULT_WINDOW_S} s", cpu_pct(m))
                }
                (IdleBy::Cpu, None) => format!("Quiet: CPU under {IDLE_MAX_CPU_PCT:.0} %"),
            };
            let star = if c.memory_partial {
                " (some processes unreadable)"
            } else {
                ""
            };
            let total = c.memory_bytes();
            let memory = gib(total);
            let memory_tooltip = format!(
                "PSS {} + swap PSS {}{star}",
                bytes(c.pss_bytes as f64),
                c.swap_pss_bytes
                    .map_or_else(|| DASH.to_string(), |b| bytes(b as f64)),
            );
            let share = if largest > 0 {
                (total as f64 / largest as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let swap_share = if total > 0 {
                (c.swap_pss_bytes.unwrap_or(0) as f64 / total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let a11y = format!("{name}, {meta}, {why}, frees {memory}");
            CandidateText {
                pid: c.pid,
                link: SessionLink {
                    pid: c.pid,
                    starttime: session.and_then(|s| s.starttime),
                    overlay_id: session
                        .and_then(|s| s.overlay.as_ref())
                        .and_then(|o| o.agent_session_id.clone()),
                },
                name,
                meta,
                why,
                memory,
                memory_tooltip,
                bytes: total,
                share,
                swap_share,
                a11y,
            }
        })
        .collect()
}

/// Page header: "26 idle sessions hold 19.0 GiB".
pub fn header_text(n: usize, bytes: u64) -> String {
    if n == 1 {
        format!("1 idle session holds {}", gib1(bytes))
    } else {
        format!("{n} idle sessions hold {}", gib1(bytes))
    }
}

/// Action bar button: "End 3 sessions… · frees 2.8 GiB". With nothing
/// checked the button is off and says how to start.
pub fn action_text(n: usize, bytes: u64) -> String {
    match n {
        0 => "Check idle sessions to end them".to_string(),
        1 => format!("End 1 session… · frees {}", gib1(bytes)),
        _ => format!("End {n} sessions… · frees {}", gib1(bytes)),
    }
}

/// Bytes the checked candidates give back; a checked pid that is no longer
/// a candidate counts nothing.
pub fn checked_total(texts: &[CandidateText], checked: &BTreeSet<u32>) -> u64 {
    texts
        .iter()
        .filter(|t| checked.contains(&t.pid))
        .map(|t| t.bytes)
        .sum()
}

/// One session "End N sessions…" still has to ask about: what was checked,
/// with the identity that was on screen when it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub pid: u32,
    pub name: String,
    pub link: SessionLink,
}

/// What "End N sessions…" asks to end, in list order.
pub fn end_queue(texts: &[CandidateText], checked: &BTreeSet<u32>) -> VecDeque<Pending> {
    texts
        .iter()
        .filter(|t| checked.contains(&t.pid))
        .map(|t| Pending {
            pid: t.pid,
            name: t.name.clone(),
            link: t.link.clone(),
        })
        .collect()
}

/// Drops the queued sessions that are no longer candidates, or whose pid now
/// belongs to another process (the start time moved), so no prompt is raised
/// for them. The name is the one on screen now.
pub fn prune_queue(queue: &mut VecDeque<Pending>, texts: &[CandidateText]) {
    queue.retain_mut(|p| match texts.iter().find(|t| t.pid == p.pid) {
        Some(t) if t.link == p.link => {
            p.name.clone_from(&t.name);
            true
        }
        _ => false,
    });
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn set_label(l: &gtk::Label, t: &str) {
    if l.label() != t {
        l.set_label(t);
    }
}

/// Diagonal stripes in the area's CSS `color` over its CSS background: the
/// swapped-out part of a bar, and the legend swatch.
fn hatch(classes: &[&str]) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.add_css_class("hatch");
    for c in classes {
        area.add_css_class(c);
    }
    area.set_accessible_role(gtk::AccessibleRole::Presentation);
    area.set_draw_func(|area, cr, w, h| {
        let c = area.color();
        cr.set_source_rgba(
            c.red().into(),
            c.green().into(),
            c.blue().into(),
            c.alpha().into(),
        );
        cr.set_line_width(HATCH_LINE_PX);
        let (w, h) = (f64::from(w), f64::from(h));
        let mut x = -h;
        while x < w {
            cr.move_to(x, h);
            cr.line_to(x + h, 0.0);
            x += HATCH_STEP_PX;
        }
        let _ = cr.stroke();
    });
    area
}

/// A memory bar: the track, PSS solid, then the swapped part hatched.
struct Bar {
    track: gtk::Box,
    pss: gtk::Box,
    swap: gtk::DrawingArea,
    width: Cell<i32>,
    last: Cell<(i32, i32)>,
}

impl Bar {
    fn new(width: i32) -> Self {
        let track = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        track.add_css_class("reclaim-bar");
        track.set_overflow(gtk::Overflow::Hidden);
        track.set_valign(gtk::Align::Center);
        track.set_size_request(width, BAR_HEIGHT_PX);
        track.set_accessible_role(gtk::AccessibleRole::Presentation);
        let pss = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pss.add_css_class("fill");
        let swap = hatch(&["swap"]);
        track.append(&pss);
        track.append(&swap);
        Self {
            track,
            pss,
            swap,
            width: Cell::new(width),
            last: Cell::new((-1, -1)),
        }
    }

    fn set_width(&self, width: i32) {
        self.width.set(width);
        self.track.set_size_request(width, BAR_HEIGHT_PX);
    }

    fn set(&self, share: f64, swap_share: f64) {
        let total = (share * f64::from(self.width.get())).round() as i32;
        let swap = (f64::from(total) * swap_share).round() as i32;
        let parts = (total - swap, swap);
        if self.last.replace(parts) != parts {
            self.pss.set_size_request(parts.0, BAR_HEIGHT_PX);
            self.swap.set_size_request(parts.1, BAR_HEIGHT_PX);
            self.swap.set_visible(parts.1 > 0);
        }
    }
}

type OpenSession = Rc<dyn Fn(&SessionLink, &gtk::Button)>;

struct RowWidgets {
    pid: Rc<Cell<u32>>,
    row: gtk::ListBoxRow,
    check: gtk::CheckButton,
    name: gtk::Label,
    bar: Bar,
    memory: gtk::Label,
    view: gtk::Button,
    link: Rc<RefCell<Option<SessionLink>>>,
}

impl RowWidgets {
    fn new(open: OpenSession, inner: Weak<Inner>, bar_px: i32) -> Self {
        let name = label(&["reclaim-name"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name.set_hexpand(true);
        let check = gtk::CheckButton::new();
        check.set_child(Some(&name));
        check.set_hexpand(true);
        let pid = Rc::new(Cell::new(0));
        let p = pid.clone();
        check.connect_toggled(move |c| {
            if let Some(inner) = inner.upgrade()
                && !inner.syncing.get()
            {
                inner.set_check(p.get(), c.is_active(), Origin::List);
            }
        });
        let bar = Bar::new(bar_px);
        let memory = label(&["reclaim-value", "numeric"]);
        memory.set_xalign(1.0);
        let view = gtk::Button::from_icon_name("go-next-symbolic");
        view.add_css_class("flat");
        view.set_tooltip_text(Some("View session"));
        view.set_valign(gtk::Align::Center);
        let link: Rc<RefCell<Option<SessionLink>>> = Rc::default();
        let target = link.clone();
        view.connect_clicked(move |button| {
            if let Some(link) = target.borrow().as_ref() {
                open(link, button);
            }
        });
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.add_css_class("reclaim-line");
        b.append(&check);
        b.append(&bar.track);
        b.append(&memory);
        b.append(&view);
        let row = gtk::ListBoxRow::builder()
            .child(&b)
            .activatable(false)
            .selectable(false)
            .build();
        Self {
            pid,
            row,
            check,
            name,
            bar,
            memory,
            view,
            link,
        }
    }

    fn set(&self, t: &CandidateText) {
        self.pid.set(t.pid);
        self.link.replace(Some(t.link.clone()));
        self.view
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "View session {}",
                t.name
            ))]);
        set_label(&self.name, &t.name);
        self.check
            .set_tooltip_text(Some(&format!("{}\n{}\n{}", t.name, t.meta, t.why)));
        self.check
            .update_property(&[gtk::accessible::Property::Label(&t.a11y)]);
        set_label(&self.memory, &t.memory);
        self.memory.set_tooltip_text(Some(&t.memory_tooltip));
        self.bar.set(t.share, t.swap_share);
    }
}

/// Where a check changed: the map already shows it, or the list does.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    Map,
    List,
    Page,
}

/// State the map, the rows and the action bar share.
struct Inner {
    widget: gtk::Box,
    map: MemoryMap,
    list: gtk::ListBox,
    rows: RefCell<Vec<RowWidgets>>,
    texts: RefCell<Vec<CandidateText>>,
    /// Row position by pid; the list sorts on it, rows are never rebuilt.
    rank: Rc<RefCell<HashMap<u32, usize>>>,
    checked: RefCell<BTreeSet<u32>>,
    /// Set while the page writes checks, so the widgets' own signals do not
    /// echo back.
    syncing: Cell<bool>,
    end: gtk::Button,
    bar_px: Cell<i32>,
    open: RefCell<OpenSession>,
    /// Sessions "End N sessions…" still has to ask about, one confirmation
    /// each, in list order.
    queue: RefCell<VecDeque<Pending>>,
    /// The last request is out and has not reached its dialog's close.
    awaiting: Cell<bool>,
    seen_dialog: Cell<bool>,
    /// The open dialog was answered with Confirm.
    confirmed: Cell<bool>,
    request: Cell<u64>,
    watching_dialogs: Cell<bool>,
}

impl Inner {
    fn set_check(&self, pid: u32, on: bool, from: Origin) {
        if !self.texts.borrow().iter().any(|t| t.pid == pid) {
            return;
        }
        {
            let mut checked = self.checked.borrow_mut();
            if on {
                checked.insert(pid);
            } else {
                checked.remove(&pid);
            }
        }
        self.sync(from);
    }

    /// Map checks, row checks and the action bar from `checked`.
    fn sync(&self, from: Origin) {
        self.syncing.set(true);
        let checked: Vec<u32> = self.checked.borrow().iter().copied().collect();
        if from != Origin::Map {
            self.map.set_checked(&checked);
        }
        for r in self.rows.borrow().iter() {
            let on = checked.contains(&r.pid.get());
            if r.check.is_active() != on {
                r.check.set_active(on);
            }
        }
        self.syncing.set(false);
        let texts = self.texts.borrow();
        let set = self.checked.borrow();
        let n = texts.iter().filter(|t| set.contains(&t.pid)).count();
        let text = action_text(n, checked_total(&texts, &set));
        if self.end.label().as_deref() != Some(text.as_str()) {
            self.end.set_label(&text);
        }
        self.end.set_sensitive(n > 0);
    }

    /// "End N sessions…": the page's existing per-session flow (`win.act`
    /// with [`Action::Term`]), once per checked session. The window runs one
    /// flow at a time, so the next request goes out after the previous
    /// confirmation dialog closed; a request that brings no dialog (refused,
    /// or actions off) stops the queue, and its toast says why.
    fn start_end(self: &Rc<Self>) {
        if self.awaiting.get() {
            return;
        }
        let targets = end_queue(&self.texts.borrow(), &self.checked.borrow());
        self.queue.replace(targets);
        self.watch_dialogs();
        self.fire_next();
    }

    fn fire_next(self: &Rc<Self>) {
        // The page moved on since the click: only sessions still on screen
        // get a prompt.
        prune_queue(&mut self.queue.borrow_mut(), &self.texts.borrow());
        let Some(Pending { pid, name, .. }) = self.queue.borrow_mut().pop_front() else {
            return;
        };
        let request = self.request.get() + 1;
        self.request.set(request);
        self.awaiting.set(true);
        self.seen_dialog.set(false);
        self.confirmed.set(false);
        let target = actions_ui::action_target(Action::Term, Target::Session(pid), &name);
        if self
            .widget
            .activate_action(&format!("win.{}", actions_ui::WIN_ACTION), Some(&target))
            .is_err()
        {
            self.stop_queue();
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(NO_DIALOG_GRACE, move || {
            if let Some(inner) = weak.upgrade()
                && inner.request.get() == request
                && inner.awaiting.get()
                && !inner.seen_dialog.get()
            {
                inner.stop_queue();
            }
        });
    }

    fn stop_queue(&self) {
        self.queue.borrow_mut().clear();
        self.awaiting.set(false);
    }

    fn on_dialog(self: &Rc<Self>, dialog: Option<adw::Dialog>) {
        if !self.awaiting.get() {
            return;
        }
        if let Some(dialog) = dialog {
            self.seen_dialog.set(true);
            if let Some(alert) = dialog.downcast_ref::<adw::AlertDialog>() {
                let weak = Rc::downgrade(self);
                alert.connect_response(None, move |_, id| {
                    if let Some(inner) = weak.upgrade() {
                        inner.confirmed.set(id == actions_ui::Response::CONFIRM);
                    }
                });
            }
            return;
        }
        if !self.seen_dialog.get() {
            return;
        }
        // Cancel, Escape or a click outside: the person said no, so the
        // sessions still queued are not asked about.
        if !self.confirmed.get() {
            self.stop_queue();
            return;
        }
        self.awaiting.set(false);
        if self.queue.borrow().is_empty() {
            return;
        }
        // The window's flow frees itself after the signal is sent, a moment
        // after the dialog closes.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(NEXT_REQUEST_DELAY, move || {
            if let Some(inner) = weak.upgrade() {
                inner.fire_next();
            }
        });
    }

    fn watch_dialogs(self: &Rc<Self>) {
        if self.watching_dialogs.get() {
            return;
        }
        let Some(root) = self.widget.root() else {
            return;
        };
        let weak = Rc::downgrade(self);
        let notify = move |dialog: Option<adw::Dialog>| {
            if let Some(inner) = weak.upgrade() {
                inner.on_dialog(dialog);
            }
        };
        if let Some(w) = root.downcast_ref::<adw::ApplicationWindow>() {
            w.connect_visible_dialog_notify(move |w| notify(w.visible_dialog()));
        } else if let Some(w) = root.downcast_ref::<adw::Window>() {
            w.connect_visible_dialog_notify(move |w| notify(w.visible_dialog()));
        } else {
            return;
        }
        self.watching_dialogs.set(true);
    }
}

fn legend_item(swatch: &gtk::Widget, text: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.add_css_class("reclaim-legend-item");
    swatch.set_valign(gtk::Align::Center);
    b.append(swatch);
    b.append(&label(&["reclaim-legend"]));
    if let Some(l) = b.last_child().and_downcast::<gtk::Label>() {
        l.set_label(text);
    }
    b
}

fn swatch(class: &str) -> gtk::Widget {
    let s = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    s.add_css_class("reclaim-swatch");
    s.add_css_class(class);
    s.set_size_request(14, 14);
    s.upcast()
}

pub struct ReclaimPage {
    pub widget: gtk::Box,
    stack: gtk::Stack,
    watching: adw::StatusPage,
    header: gtk::Label,
    count: gtk::Label,
    pub scroll: gtk::ScrolledWindow,
    inner: Rc<Inner>,
    ring: RefCell<Ring>,
    own: u32,
    /// Pids da primeira janela cheia, uma vez, para scripts/reclaim-parity.sh.
    report: Cell<bool>,
}

impl ReclaimPage {
    pub fn new() -> Self {
        let header = label(&["reclaim-header", "numeric"]);
        header.set_wrap(true);
        let note = label(&["reclaim-note"]);
        note.set_label(
            "Ending a session frees its PSS and swap; pages it shares with other sessions \
             may stay, so the total is an upper bound. Nothing ends until you confirm.",
        );
        note.set_wrap(true);
        note.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        note.set_max_width_chars(60);

        let map = MemoryMap::new();
        map.set_reclaim(true);
        map.set_size_request(-1, MAP_MIN_HEIGHT_PX);

        let legend = adw::WrapBox::builder()
            .child_spacing(16)
            .line_spacing(4)
            .build();
        legend.add_css_class("reclaim-legend-row");
        legend.append(&legend_item(&swatch("target"), "Idle, can be ended"));
        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.add_css_class("reclaim-legend-check");
        legend.append(&legend_item(check.upcast_ref(), "Selected"));
        let hatched = hatch(&["reclaim-swatch", "swapped"]);
        hatched.set_size_request(14, 14);
        legend.append(&legend_item(hatched.upcast_ref(), "Swapped out"));
        legend.append(&legend_item(
            &swatch("context"),
            "Busy, waiting or multiplexer",
        ));

        let main = gtk::Box::new(gtk::Orientation::Vertical, 8);
        main.add_css_class("reclaim-main");
        main.set_hexpand(true);
        main.append(&header);
        main.append(&note);
        main.append(&map);
        main.append(&legend);

        let title = label(&["reclaim-title"]);
        title.set_label("Idle sessions by memory");
        title.set_hexpand(true);
        let count = label(&["reclaim-count", "numeric"]);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.append(&title);
        head.append(&count);
        let rule = label(&["reclaim-note"]);
        rule.set_label(&format!(
            "Idle: the agent reports idle, or its CPU stayed under {IDLE_MAX_CPU_PCT:.0} % of a \
             core for {DEFAULT_WINDOW_S} s. Busy and waiting sessions never show here."
        ));
        rule.set_wrap(true);
        rule.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        // A wrapping label asks for its whole sentence on one line; capped,
        // the column keeps its own width and the map gets the rest.
        rule.set_max_width_chars(40);

        let list = gtk::ListBox::new();
        list.add_css_class("reclaim-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.update_property(&[gtk::accessible::Property::Label("Idle sessions")]);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        scroll.add_css_class("reclaim-scroll");

        let end = gtk::Button::with_label(&action_text(0, 0));
        end.add_css_class("destructive-action");
        end.add_css_class("reclaim-end-all");
        end.set_halign(gtk::Align::End);
        end.set_hexpand(true);
        end.set_sensitive(false);
        let actionbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actionbar.add_css_class("reclaim-actionbar");
        actionbar.append(&end);

        let side = gtk::Box::new(gtk::Orientation::Vertical, 6);
        side.add_css_class("reclaim-side");
        side.set_hexpand(false);
        side.set_size_request(SIDE_WIDTH_PX, -1);
        side.append(&head);
        side.append(&rule);
        side.append(&scroll);
        side.append(&actionbar);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.add_css_class("reclaim-content");
        content.append(&main);
        content.append(&side);
        let bin = adw::BreakpointBin::builder()
            .child(&content)
            .width_request(MIN_WIDTH_PX)
            .height_request(MIN_HEIGHT_PX)
            .build();
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            NARROW_MAX_WIDTH_PX,
            adw::LengthUnit::Px,
        ));
        narrow.add_setter(
            &content,
            "orientation",
            Some(&gtk::Orientation::Vertical.to_value()),
        );
        narrow.add_setter(&side, "width-request", Some(&(-1).to_value()));
        narrow.add_setter(&main, "vexpand", Some(&false.to_value()));
        narrow.add_setter(&map, "vexpand", Some(&false.to_value()));
        narrow.add_setter(&map, "height-request", Some(&NARROW_MAP_PX.to_value()));
        // The note, the legend and the rule are for the wide page: at 360 px
        // they push the list and the action bar below the fold.
        for w in [
            note.upcast_ref::<gtk::Widget>(),
            legend.upcast_ref(),
            rule.upcast_ref(),
        ] {
            narrow.add_setter(w, "visible", Some(&false.to_value()));
        }
        narrow.add_setter(
            &content,
            "css-classes",
            Some(&["reclaim-content", "narrow"].to_value()),
        );
        bin.add_breakpoint(narrow.clone());

        let watching = adw::StatusPage::builder()
            .icon_name("edit-clear-all-symbolic")
            .title("Watching for Idle Sessions")
            .build();
        watching.add_css_class("compact");
        let empty = label(&["reclaim-empty"]);
        empty.set_label(EMPTY_TEXT);
        empty.set_wrap(true);
        empty.set_justify(gtk::Justification::Center);
        empty.set_xalign(0.5);
        empty.set_valign(gtk::Align::Center);
        empty.set_halign(gtk::Align::Center);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&watching, Some(STACK_WATCHING));
        stack.add_named(&empty, Some(STACK_EMPTY));
        stack.add_named(&bin, Some(STACK_LIST));

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("reclaim-page");
        widget.append(&stack);

        let rank: Rc<RefCell<HashMap<u32, usize>>> = Rc::default();
        let order = rank.clone();
        list.set_sort_func(move |a, b| {
            let order = order.borrow();
            let at = |r: &gtk::ListBoxRow| {
                r.widget_name()
                    .parse::<u32>()
                    .ok()
                    .and_then(|p| order.get(&p).copied())
                    .unwrap_or(usize::MAX)
            };
            at(a).cmp(&at(b)).into()
        });
        let inner = Rc::new(Inner {
            widget: widget.clone(),
            map: map.clone(),
            list,
            rows: RefCell::default(),
            texts: RefCell::default(),
            rank: rank.clone(),
            checked: RefCell::default(),
            syncing: Cell::new(false),
            end: end.clone(),
            bar_px: Cell::new(BAR_PX),
            open: RefCell::new(Rc::new(|_, _| {})),
            queue: RefCell::default(),
            awaiting: Cell::new(false),
            seen_dialog: Cell::new(false),
            confirmed: Cell::new(false),
            request: Cell::new(0),
            watching_dialogs: Cell::new(false),
        });
        let weak = Rc::downgrade(&inner);
        map.connect_check_toggled(move |pid, on| {
            if let Some(inner) = weak.upgrade() {
                inner.set_check(pid, on, Origin::Map);
            }
        });
        let weak = Rc::downgrade(&inner);
        end.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.start_end();
            }
        });
        let (w, n) = (Rc::downgrade(&inner), narrow);
        let resize = move |px: i32| {
            if let Some(inner) = w.upgrade() {
                inner.bar_px.set(px);
                for r in inner.rows.borrow().iter() {
                    r.bar.set_width(px);
                }
                let texts = inner.texts.borrow();
                for (r, t) in inner.rows.borrow().iter().zip(texts.iter()) {
                    r.bar.set(t.share, t.swap_share);
                }
            }
        };
        let r = resize.clone();
        n.connect_apply(move |_| r(NARROW_BAR_PX));
        n.connect_unapply(move |_| resize(BAR_PX));

        let page = Self {
            widget,
            stack,
            watching,
            header,
            count,
            scroll,
            inner,
            ring: RefCell::default(),
            own: std::process::id(),
            report: Cell::new(true),
        };
        page.set_watching(0);
        page
    }

    pub fn connect_open(&self, open: impl Fn(&SessionLink, &gtk::Button) + 'static) {
        self.inner.open.replace(Rc::new(open));
    }

    fn set_watching(&self, have: usize) {
        let left = WINDOW_TICKS.saturating_sub(have);
        self.watching.set_description(Some(&format!(
            "A session counts as idle after {DEFAULT_WINDOW_S} s of CPU readings. \
             {left} s to go."
        )));
        if self.stack.visible_child_name().as_deref() != Some(STACK_WATCHING) {
            self.stack.set_visible_child_name(STACK_WATCHING);
        }
    }

    /// Todo tick, visivel ou nao: a janela precisa estar cheia quando a
    /// pagina abrir.
    pub fn observe(&self, snap: &Arc<Snapshot>) {
        self.ring.borrow_mut().push(snap);
    }

    /// Redesenha com a janela atual. So quando a pagina esta visivel.
    pub fn refresh(&self, home: Option<&str>) {
        let ring = self.ring.borrow();
        let Some(r) = ring.reclaim(Some(self.own)) else {
            self.set_watching(ring.len());
            return;
        };
        let Some(snap) = ring.snaps.back().cloned() else {
            return;
        };
        drop(ring);
        if self.report.replace(false) {
            let pids: Vec<String> = r.sessions.iter().map(|c| c.pid.to_string()).collect();
            println!(
                "reclaim_pids={} reclaim_total={}",
                pids.join(","),
                r.total_bytes
            );
        }
        let texts = texts(&r, &snap, home);
        set_label(&self.header, &header_text(texts.len(), r.total_bytes));
        set_label(&self.count, &texts.len().to_string());
        let inner = &self.inner;
        let pids: Vec<u32> = texts.iter().map(|t| t.pid).collect();
        inner.map.set_targets(Some(&pids));
        inner.map.set_input(MapInput::from_snapshot(&snap));
        {
            let mut rows = inner.rows.borrow_mut();
            // Rows follow their session, not their position: a memory
            // reorder moves the row widgets, so focus and checks stay put.
            let mut old: HashMap<u32, RowWidgets> =
                rows.drain(..).map(|w| (w.pid.get(), w)).collect();
            for t in &texts {
                let w = old.remove(&t.pid).unwrap_or_else(|| {
                    let w = RowWidgets::new(
                        inner.open.borrow().clone(),
                        Rc::downgrade(inner),
                        inner.bar_px.get(),
                    );
                    w.row.set_widget_name(&t.pid.to_string());
                    inner.list.append(&w.row);
                    w
                });
                w.set(t);
                rows.push(w);
            }
            for (_, gone) in old {
                inner.list.remove(&gone.row);
            }
            let rank: HashMap<u32, usize> =
                texts.iter().enumerate().map(|(i, t)| (t.pid, i)).collect();
            let moved = rows
                .iter()
                .enumerate()
                .any(|(i, w)| w.row.index() != i as i32);
            inner.rank.replace(rank);
            if moved {
                inner.list.invalidate_sort();
            }
        }
        // A session that left the candidates (ended, or busy again) loses
        // its check: "End N" only ever names sessions on screen.
        inner.checked.borrow_mut().retain(|p| pids.contains(p));
        inner.texts.replace(texts);
        inner.sync(Origin::Page);
        let page = if pids.is_empty() {
            STACK_EMPTY
        } else {
            STACK_LIST
        };
        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::adapters::AgentRole;
    use agentcenter_core::overlay::{OverlayStatus, SessionOverlay};
    use agentcenter_core::session::ProcessMetrics;
    use agentcenter_core::{IoStallSource, Machine, Session};

    fn snap(interval_ms: u64, sessions: Vec<Session>) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            schema_version: agentcenter_core::snapshot::SCHEMA_VERSION,
            taken_at_ms: 0,
            interval_ms,
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
            sessions,
            summary: Default::default(),
        })
    }

    fn session(pid: u32, cpu: f64, pss: u64, status: Option<OverlayStatus>) -> Session {
        let p = ProcessMetrics {
            pid,
            ppid: 1,
            comm: "claude".into(),
            state: 'S',
            uid: 1000,
            threads: 1,
            cpu_pct: Some(cpu),
            rss_bytes: Some(pss),
            pss_bytes: Some(pss),
            swap_bytes: Some(0),
            swap_pss_bytes: Some(0),
            read_bps: Some(0.0),
            write_bps: Some(0.0),
            rchar_bps: Some(0.0),
            wchar_bps: Some(0.0),
            stall_pct: Some(0.0),
            threads_d: None,
            voluntary_ctxt_switches: 0,
            nonvoluntary_ctxt_switches: 0,
        };
        let mut s = Session::aggregate("claude".into(), AgentRole::Agent, pid, 16, vec![p]);
        s.overlay = status.map(|st| SessionOverlay {
            status: Some(st),
            ..Default::default()
        });
        s
    }

    #[test]
    fn kernel_link_survives_missing_overlay_but_rejects_pid_reuse() {
        let mut s = session(10, 0.0, 1, None);
        s.starttime = Some(123);
        let link = SessionLink {
            pid: 10,
            starttime: Some(123),
            overlay_id: None,
        };
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        s.starttime = Some(124);
        assert!(!link.matches(&snap(1000, vec![s.clone()])));
        s.starttime = None;
        assert!(!link.matches(&snap(1000, vec![s])));
        assert!(!link.matches(&snap(1000, vec![])));
    }

    /// A native session can change without restarting its hosting process.
    #[test]
    fn cc2_regression_native_session_replacement_is_not_the_same_link() {
        let mut s = session(10, 0.0, 1, Some(OverlayStatus::Idle));
        s.starttime = Some(123);
        s.overlay.as_mut().unwrap().agent_session_id = Some("original".into());
        let link = SessionLink {
            pid: 10,
            starttime: Some(123),
            overlay_id: Some("original".into()),
        };
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        s.overlay.as_mut().unwrap().agent_session_id = Some("replacement".into());
        assert!(
            !link.matches(&snap(1000, vec![s])),
            "same PID/starttime must not silently navigate to a different native session"
        );
    }

    #[test]
    fn navigation_link_rejects_missing_or_replaced_overlay() {
        let mut s = session(10, 0.0, 1, Some(OverlayStatus::Idle));
        s.overlay.as_mut().unwrap().agent_session_id = Some("original".into());
        let link = SessionLink {
            pid: 10,
            starttime: None,
            overlay_id: Some("original".into()),
        };
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        s.overlay.as_mut().unwrap().agent_session_id = Some("replacement".into());
        assert!(!link.matches(&snap(1000, vec![s])));
        assert!(!link.matches(&snap(1000, vec![])));
        assert!(
            !SessionLink {
                pid: 10,
                starttime: None,
                overlay_id: None
            }
            .matches(&snap(1000, vec![session(10, 0.0, 1, None)]))
        );
    }

    /// Overlay equality alone must not navigate to another PID or accept absent identity.
    #[test]
    fn navigation_requires_both_pid_and_nonempty_overlay_identity() {
        let mut s = session(20, 0.0, 1, Some(OverlayStatus::Idle));
        s.overlay.as_mut().unwrap().agent_session_id = Some("same".into());
        let link = SessionLink {
            pid: 10,
            starttime: None,
            overlay_id: Some("same".into()),
        };
        assert!(!link.matches(&snap(1000, vec![s.clone()])));
        s.pid = 10;
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        for id in [None, Some(String::new())] {
            s.overlay.as_mut().unwrap().agent_session_id = id.clone();
            assert!(!link.matches(&snap(1000, vec![s.clone()])));
            assert!(
                !SessionLink {
                    pid: 10,
                    starttime: None,
                    overlay_id: id
                }
                .matches(&snap(1000, vec![s.clone()]))
            );
        }
        s.overlay = None;
        assert!(!link.matches(&snap(1000, vec![s])));
        // This checks overlay navigation only, not kernel starttime/PID-reuse safety.
    }

    #[test]
    fn ring_waits_for_a_full_window_and_then_slides() {
        let quiet = vec![session(10, 0.1, 1 << 30, None)];
        let mut ring = Ring::default();
        ring.push(&snap(0, quiet.clone()));
        assert_eq!(ring.len(), 0, "the tick without rates does not count");
        for _ in 0..WINDOW_TICKS - 1 {
            ring.push(&snap(1000, quiet.clone()));
        }
        assert!(
            ring.reclaim(None).is_none(),
            "9 of 10 ticks: still watching"
        );
        ring.push(&snap(1000, quiet.clone()));
        assert_eq!(ring.reclaim(None).unwrap().sessions.len(), 1);
        // Um tick com CPU alta fica na janela por 10 ticks, depois sai.
        ring.push(&snap(1000, vec![session(10, 30.0, 1 << 30, None)]));
        for i in 0..WINDOW_TICKS {
            assert!(ring.reclaim(None).unwrap().sessions.is_empty(), "tick {i}");
            ring.push(&snap(1000, quiet.clone()));
        }
        assert_eq!(ring.reclaim(None).unwrap().sessions.len(), 1);
        assert_eq!(ring.len(), WINDOW_TICKS);
    }

    #[test]
    fn ring_matches_the_core_rule_over_the_same_ticks() {
        // A mesma sequencia que o CLI veria: o reclaim da janela e o do core.
        let ticks: Vec<Vec<Session>> = (0..WINDOW_TICKS)
            .map(|i| {
                vec![
                    session(10, 0.2, 2 << 30, None),
                    session(20, if i == 3 { 5.0 } else { 0.0 }, 3 << 30, None),
                    session(30, 40.0, 1 << 30, Some(OverlayStatus::Idle)),
                    session(40, 0.0, 4 << 30, Some(OverlayStatus::Busy)),
                ]
            })
            .collect();
        let mut ring = Ring::default();
        let mut w = CpuWindow::new();
        for t in &ticks {
            let s = snap(1000, t.clone());
            ring.push(&s);
            w.observe(&s);
        }
        let last = snap(1000, ticks[WINDOW_TICKS - 1].clone());
        assert_eq!(
            ring.reclaim(Some(1)),
            Some(reclaim::reclaim(&last, &w, Some(1)))
        );
        let pids: Vec<u32> = ring
            .reclaim(Some(1))
            .unwrap()
            .sessions
            .iter()
            .map(|c| c.pid)
            .collect();
        assert_eq!(pids, [10, 30]);
    }

    #[test]
    fn texts_say_why_and_how_much_with_the_list_name() {
        let mut s = session(10, 0.25, 3 << 30, None);
        s.cwd = Some("/home/dev/src/kiln".into());
        let o = session(30, 50.0, 1 << 30, Some(OverlayStatus::Idle));
        let ticks = vec![s, o];
        let mut ring = Ring::default();
        for _ in 0..WINDOW_TICKS {
            ring.push(&snap(1000, ticks.clone()));
        }
        let r = ring.reclaim(None).unwrap();
        let last = snap(1000, ticks);
        let t = texts(&r, &last, Some("/home/dev"));
        assert_eq!(t[0].pid, 10);
        assert_eq!(t[0].meta, "claude · ~/src/kiln · pid 10");
        assert!(t[0].why.starts_with("Quiet: CPU at most"), "{}", t[0].why);
        // The bar is against the largest candidate (the map's scale), no
        // longer a share of the total.
        assert!((t[0].share - 1.0).abs() < 1e-9);
        assert!((t[1].share - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(t[0].bytes, 3 << 30);
        assert_eq!(t[1].why, "Idle by its own status");
        assert_eq!(
            header_text(t.len(), r.total_bytes),
            "2 idle sessions hold 4.0 GiB"
        );
    }

    #[test]
    fn header_and_action_bar_say_how_many_and_how_much() {
        let gib = |x: f64| (x * (1u64 << 30) as f64) as u64;
        assert_eq!(header_text(26, gib(19.0)), "26 idle sessions hold 19.0 GiB");
        assert_eq!(header_text(1, gib(1.09)), "1 idle session holds 1.1 GiB");
        assert_eq!(header_text(0, 0), "0 idle sessions hold 0.0 GiB");
        assert_eq!(action_text(3, gib(3.0)), "End 3 sessions… · frees 3.0 GiB");
        assert_eq!(action_text(1, gib(0.94)), "End 1 session… · frees 0.9 GiB");
        assert_eq!(action_text(0, 0), "Check idle sessions to end them");
    }

    #[test]
    fn checked_total_and_end_queue_count_only_checked_candidates() {
        let ticks = vec![
            session(10, 0.0, 3 << 30, None),
            session(20, 0.0, 1 << 30, Some(OverlayStatus::Idle)),
            session(30, 0.0, 2 << 30, Some(OverlayStatus::Busy)),
        ];
        let mut ring = Ring::default();
        for _ in 0..WINDOW_TICKS {
            ring.push(&snap(1000, ticks.clone()));
        }
        let r = ring.reclaim(None).unwrap();
        let t = texts(&r, &snap(1000, ticks), None);
        let checked: BTreeSet<u32> = [20, 30, 999].into();
        assert_eq!(
            checked_total(&t, &checked),
            1 << 30,
            "busy 30 and gone 999 count 0"
        );
        assert_eq!(checked_total(&t, &BTreeSet::new()), 0);
        let all: BTreeSet<u32> = [20, 10].into();
        assert_eq!(checked_total(&t, &all), 4 << 30);
        assert_eq!(
            end_queue(&t, &all)
                .iter()
                .map(|p| p.pid)
                .collect::<Vec<_>>(),
            [10, 20],
            "list order, largest first"
        );
    }

    // ---- GTK ----------------------------------------------------------------

    fn fixture(name: &str) -> Arc<Snapshot> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(name);
        Arc::new(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
    }

    fn settle(ms: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn load_css() {
        let css = gtk::CssProvider::new();
        css.load_from_string(concat!(
            include_str!("../resources/tokens.css"),
            include_str!("../resources/memory_map.css"),
            include_str!("../resources/reclaim.css"),
        ));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    /// The page in a window, its reclaim window full of `name`.
    fn shown(name: &str) -> (ReclaimPage, adw::Window, Arc<Snapshot>) {
        shown_as(name, None)
    }

    /// As [`shown`], with the page running inside session `own`.
    fn shown_as(name: &str, own: Option<u32>) -> (ReclaimPage, adw::Window, Arc<Snapshot>) {
        load_css();
        let mut page = ReclaimPage::new();
        if let Some(own) = own {
            page.own = own;
        }
        let snap = fixture(name);
        for _ in 0..WINDOW_TICKS {
            page.observe(&snap);
        }
        let win = adw::Window::new();
        win.set_default_size(1000, 700);
        win.set_content(Some(&page.widget));
        win.present();
        page.refresh(None);
        settle(300);
        (page, win, snap)
    }

    fn core_pids(page: &ReclaimPage, snap: &Snapshot) -> BTreeSet<u32> {
        let mut w = CpuWindow::new();
        for _ in 0..WINDOW_TICKS {
            w.observe(snap);
        }
        reclaim::reclaim(snap, &w, Some(page.own))
            .sessions
            .iter()
            .map(|c| c.pid)
            .collect()
    }

    fn target_tiles(page: &ReclaimPage) -> Vec<gtk::Widget> {
        page.inner
            .map
            .tiles()
            .into_iter()
            .filter(|t| t.has_css_class("reclaim"))
            .collect()
    }

    fn press(map: &MemoryMap, key: gtk::gdk::Key) -> bool {
        use gtk::glib::translate::IntoGlib;
        let keys = map.observe_controllers();
        let ctl = (0..keys.n_items())
            .filter_map(|i| keys.item(i).and_downcast::<gtk::EventControllerKey>())
            .next()
            .expect("key controller on the map");
        ctl.emit_by_name::<bool>(
            "key-pressed",
            &[&key.into_glib(), &0u32, &gtk::gdk::ModifierType::empty()],
        )
    }

    fn row_checked(page: &ReclaimPage, pid: u32) -> bool {
        page.inner
            .rows
            .borrow()
            .iter()
            .find(|r| r.pid.get() == pid)
            .expect("row for pid")
            .check
            .is_active()
    }

    #[test]
    fn tipico_40_shows_one_reclaimable_tile_per_idle_session() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, snap) = shown("tipico-40.json");
            let want = core_pids(&page, &snap);
            assert!(want.len() >= 26, "{} candidates", want.len());
            let tiles = target_tiles(&page);
            let got: BTreeSet<u32> = tiles.iter().filter_map(MemoryMap::tile_pid).collect();
            assert_eq!(tiles.len(), want.len(), "one tile per idle session");
            assert_eq!(got, want);
            for t in page.inner.map.tiles() {
                if MemoryMap::tile_pid(&t).is_some_and(|p| !want.contains(&p)) {
                    assert!(t.has_css_class("dim"), "every other session is context");
                }
            }
            assert_eq!(page.inner.rows.borrow().len(), want.len());
            let total: u64 = page.inner.texts.borrow().iter().map(|t| t.bytes).sum();
            assert_eq!(page.header.label(), header_text(want.len(), total));
            assert!(
                page.header.label().ends_with("idle sessions hold 19.0 GiB"),
                "{}",
                page.header.label()
            );
            assert_eq!(page.stack.visible_child_name().as_deref(), Some(STACK_LIST));
            assert!(!page.inner.end.is_sensitive());
            win.close();
        });
    }

    #[test]
    fn toggling_a_tile_checks_its_row_and_updates_the_action_bar() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, _) = shown("tipico-40.json");
            let tile = target_tiles(&page).into_iter().next().expect("a target");
            let pid = MemoryMap::tile_pid(&tile).unwrap();
            let bytes = page
                .inner
                .texts
                .borrow()
                .iter()
                .find(|t| t.pid == pid)
                .unwrap()
                .bytes;
            assert!(!row_checked(&page, pid));
            tile.grab_focus();
            assert!(press(&page.inner.map, gtk::gdk::Key::Return));
            assert!(row_checked(&page, pid), "the tile check reaches the list");
            assert_eq!(
                page.inner.end.label().as_deref(),
                Some(action_text(1, bytes).as_str())
            );
            assert!(
                page.inner
                    .end
                    .label()
                    .unwrap()
                    .starts_with("End 1 session… · frees ")
            );
            assert!(page.inner.end.is_sensitive());

            // And back: the row's check clears the tile and the bar.
            let rows = page.inner.rows.borrow();
            let row = rows.iter().find(|r| r.pid.get() == pid).unwrap();
            row.check.set_active(false);
            drop(rows);
            assert!(page.inner.map.checked().is_empty());
            assert!(!tile.has_css_class("checked"));
            assert_eq!(
                page.inner.end.label().as_deref(),
                Some(action_text(0, 0).as_str())
            );
            assert!(!page.inner.end.is_sensitive());
            win.close();
        });
    }

    /// The map's idle tiles are not the rule: the session AgentCenter runs
    /// in is idle on the map but never a target, and clicking it does not
    /// check anything.
    #[test]
    fn only_the_core_candidates_are_targets_on_the_map() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let snap = fixture("tipico-40.json");
            let input = MapInput::from_snapshot(&snap);
            let mine = input
                .sessions
                .iter()
                .find(|s| s.state == crate::treemap::TileState::Idle)
                .expect("an idle session")
                .pid;
            let (page, win, _) = shown_as("tipico-40.json", Some(mine));
            let targets: BTreeSet<u32> = target_tiles(&page)
                .iter()
                .filter_map(MemoryMap::tile_pid)
                .collect();
            assert!(!targets.contains(&mine), "own session is not a target");
            assert_eq!(targets, core_pids(&page, &snap));
            let tile = page
                .inner
                .map
                .tiles()
                .into_iter()
                .find(|t| MemoryMap::tile_pid(t) == Some(mine))
                .unwrap();
            assert!(tile.has_css_class("dim"));
            win.close();
        });
    }

    #[test]
    fn empty_fixture_shows_the_empty_text() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, _) = shown("vazio.json");
            assert_eq!(
                page.stack.visible_child_name().as_deref(),
                Some(STACK_EMPTY)
            );
            let empty = page
                .stack
                .visible_child()
                .and_downcast::<gtk::Label>()
                .expect("empty label");
            assert_eq!(empty.label(), EMPTY_TEXT);
            assert!(empty.is_mapped());
            win.close();
        });
    }

    /// First widget under `root` carrying the CSS class.
    fn by_class(root: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
        if root.has_css_class(class) {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(c) = child {
            if let Some(found) = by_class(&c, class) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    /// The page in a `w` x `h` window.
    fn shown_at(w: i32, h: i32) -> (ReclaimPage, adw::Window) {
        load_css();
        let page = ReclaimPage::new();
        let snap = fixture("tipico-40.json");
        for _ in 0..WINDOW_TICKS {
            page.observe(&snap);
        }
        let win = adw::Window::new();
        win.set_default_size(w, h);
        win.set_content(Some(&page.widget));
        win.present();
        page.refresh(None);
        settle(400);
        (page, win)
    }

    #[test]
    fn at_360_px_the_list_and_the_action_bar_stay_in_view() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win) = shown_at(360, 420);
            let root = page.widget.clone().upcast::<gtk::Widget>();
            for class in ["reclaim-note", "reclaim-legend-row"] {
                let w = by_class(&root, class).expect(class);
                assert!(!w.is_visible(), "{class} hidden when narrow");
            }
            assert_eq!(page.inner.map.height(), 96, "map height");
            assert_eq!(
                page.inner.rows.borrow()[0].bar.track.width(),
                120,
                "row bar width"
            );
            let content = by_class(&root, "reclaim-content").unwrap();
            assert!(
                content.has_css_class("narrow"),
                "narrow class on the content"
            );
            let h = page.widget.height();
            let end = page.inner.end.compute_bounds(&page.widget).unwrap();
            assert!(
                f64::from(end.y() + end.height()) <= f64::from(h) + 0.5,
                "action bar inside the page: bar bottom {} of {h}",
                end.y() + end.height()
            );
            let scroll = by_class(&root, "reclaim-scroll").unwrap();
            assert!(
                scroll.height() >= 100,
                "three rows visible: {}",
                scroll.height()
            );
            win.close();
        });
    }

    /// The widget tree gets its style on the first frame only on the private
    /// Broadway display, so the rule is checked on a tree built narrow.
    #[test]
    fn narrow_content_sets_the_header_to_20_px_and_wide_to_28() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let mut sizes = Vec::new();
            for content_classes in [&["reclaim-content", "narrow"][..], &["reclaim-content"][..]] {
                let header = label(&["reclaim-header", "numeric"]);
                let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
                content.set_css_classes(content_classes);
                content.append(&header);
                let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
                page.add_css_class("reclaim-page");
                page.append(&content);
                let win = adw::Window::new();
                win.set_content(Some(&page));
                win.present();
                settle(200);
                let size = header.layout().context().font_description().unwrap().size();
                sizes.push(size / gtk::pango::SCALE);
                win.close();
            }
            assert_eq!(sizes, vec![20, 28], "px sizes: narrow, wide");
        });
    }

    #[test]
    fn at_1280_px_the_note_the_legend_and_the_rule_show() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win) = shown_at(1280, 800);
            let root = page.widget.clone().upcast::<gtk::Widget>();
            for class in ["reclaim-note", "reclaim-legend-row"] {
                assert!(by_class(&root, class).unwrap().is_visible(), "{class}");
            }
            assert_eq!(
                page.inner.rows.borrow()[0].bar.track.width(),
                BAR_PX,
                "row bar width"
            );
            win.close();
        });
    }

    type Asked = Rc<RefCell<Vec<(Action, Target, String)>>>;
    type Dialogs = Rc<RefCell<Vec<adw::AlertDialog>>>;

    /// The window's flow: `win.act` with an (sbus) target, one dialog per
    /// request, as `actions_ui` raises it.
    fn fake_flow(win: &adw::Window) -> (Asked, Dialogs) {
        let asked: Asked = Rc::default();
        let dialogs: Dialogs = Rc::default();
        let act = gtk::gio::SimpleAction::new(
            actions_ui::WIN_ACTION,
            Some(glib::VariantTy::new("(sbus)").unwrap()),
        );
        let (a, d, parent) = (asked.clone(), dialogs.clone(), win.clone());
        act.connect_activate(move |_, v| {
            a.borrow_mut()
                .push(actions_ui::parse_action_target(v.unwrap()).unwrap());
            let dialog = adw::AlertDialog::new(Some("End?"), None);
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("confirm", "End");
            dialog.present(Some(&parent));
            d.borrow_mut().push(dialog);
        });
        let group = gtk::gio::SimpleActionGroup::new();
        group.add_action(&act);
        win.insert_action_group("win", Some(&group));
        (asked, dialogs)
    }

    /// The user picks `id` and the dialog goes away.
    fn answer(dialog: &adw::AlertDialog, id: &str) {
        dialog.emit_by_name::<()>("response", &[&id]);
        dialog.force_close();
    }

    /// A tick after `snap` with `change` applied to its sessions, as the
    /// sampler would deliver it, and the page redrawn.
    fn tick_changed(page: &ReclaimPage, snap: &Snapshot, change: impl Fn(&mut Session)) {
        let mut next = snap.clone();
        next.sessions.iter_mut().for_each(change);
        page.observe(&Arc::new(next));
        page.refresh(None);
    }

    fn first_pids(page: &ReclaimPage, n: usize) -> Vec<u32> {
        let pids = page
            .inner
            .texts
            .borrow()
            .iter()
            .take(n)
            .map(|t| t.pid)
            .collect();
        for r in page.inner.rows.borrow().iter().take(n) {
            r.check.set_active(true);
        }
        pids
    }

    #[test]
    fn end_asks_the_window_flow_once_per_checked_session_in_turn() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, _) = shown("tipico-40.json");
            let (asked, dialogs) = fake_flow(&win);
            let first = first_pids(&page, 2);
            assert_eq!(page.inner.map.checked().len(), 2, "rows reach the map");
            page.inner.end.emit_clicked();
            settle(100);
            assert_eq!(asked.borrow().len(), 1, "one confirmation at a time");
            assert_eq!(asked.borrow()[0].1, Target::Session(first[0]));
            answer(&dialogs.borrow()[0], "confirm");
            settle(600);
            assert_eq!(asked.borrow().len(), 2, "the next after the first closed");
            assert_eq!(asked.borrow()[1].1, Target::Session(first[1]));
            answer(&dialogs.borrow()[1], "confirm");
            settle(600);
            assert_eq!(asked.borrow().len(), 2, "nothing past the checked sessions");
            win.close();
        });
    }

    #[test]
    fn cancel_on_a_dialog_drops_the_rest_of_the_queue() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, _) = shown("tipico-40.json");
            let (asked, dialogs) = fake_flow(&win);
            first_pids(&page, 3);
            page.inner.end.emit_clicked();
            settle(100);
            answer(&dialogs.borrow()[0], "cancel");
            settle(600);
            assert_eq!(asked.borrow().len(), 1, "Cancel does not go on to dialog 2");
            assert!(page.inner.queue.borrow().is_empty());
            // Escape or a click outside closes without any response.
            page.inner.end.emit_clicked();
            settle(100);
            assert_eq!(asked.borrow().len(), 2);
            dialogs.borrow()[1].force_close();
            settle(600);
            assert_eq!(asked.borrow().len(), 2, "a bare close is not a confirm");
            win.close();
        });
    }

    #[test]
    fn a_queued_session_that_went_busy_is_not_asked_about() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, snap) = shown("tipico-40.json");
            let (asked, dialogs) = fake_flow(&win);
            let first = first_pids(&page, 3);
            page.inner.end.emit_clicked();
            settle(100);
            tick_changed(&page, &snap, |s| {
                if s.pid == first[1] {
                    s.overlay = Some(SessionOverlay {
                        status: Some(OverlayStatus::Busy),
                        ..Default::default()
                    });
                }
            });
            answer(&dialogs.borrow()[0], "confirm");
            settle(600);
            assert_eq!(asked.borrow().len(), 2);
            assert_eq!(
                asked.borrow()[1].1,
                Target::Session(first[2]),
                "the busy one is skipped, the next goes on"
            );
            win.close();
        });
    }

    #[test]
    fn a_reused_pid_is_not_asked_about_and_nothing_stale_is_named() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, snap) = shown("tipico-40.json");
            let (asked, dialogs) = fake_flow(&win);
            let first = first_pids(&page, 3);
            page.inner.end.emit_clicked();
            settle(100);
            // Same pid, another process: the start time moved. Four ticks
            // would be needed to be idle again, so it also leaves the list.
            tick_changed(&page, &snap, |s| {
                if s.pid == first[1] {
                    s.starttime = Some(s.starttime.unwrap_or(0) + 1);
                }
            });
            answer(&dialogs.borrow()[0], "confirm");
            settle(600);
            assert_eq!(asked.borrow()[1].1, Target::Session(first[2]));
            win.close();
        });
    }

    fn candidates(ticks: Vec<Session>) -> Vec<CandidateText> {
        let mut ring = Ring::default();
        for _ in 0..WINDOW_TICKS {
            ring.push(&snap(1000, ticks.clone()));
        }
        let r = ring.reclaim(None).unwrap();
        texts(&r, &snap(1000, ticks), None)
    }

    #[test]
    fn prune_queue_keeps_only_the_sessions_still_candidates_with_the_same_start() {
        let mut a = session(10, 0.0, 3 << 30, None);
        a.starttime = Some(1);
        let mut b = session(20, 0.0, 2 << 30, None);
        b.starttime = Some(2);
        let mut c = session(30, 0.0, 1 << 30, None);
        c.starttime = Some(3);
        let before = candidates(vec![a.clone(), b.clone(), c.clone()]);
        let all: BTreeSet<u32> = [10, 20, 30].into();
        let mut queue = end_queue(&before, &all);
        assert_eq!(queue.len(), 3);

        let busy = session(10, 0.0, 3 << 30, Some(OverlayStatus::Busy));
        b.starttime = Some(99);
        let after = candidates(vec![busy, b, c]);
        prune_queue(&mut queue, &after);
        assert_eq!(
            queue.iter().map(|p| p.pid).collect::<Vec<_>>(),
            [30],
            "10 went busy, 20 is another process now"
        );
        prune_queue(&mut queue, &[]);
        assert!(queue.is_empty(), "nothing on screen, nothing to ask");
    }

    #[test]
    fn a_reorder_keeps_the_row_widgets_and_the_focused_one() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let (page, win, snap) = shown("tipico-40.json");
            let pids: Vec<u32> = page.inner.texts.borrow().iter().map(|t| t.pid).collect();
            assert!(pids.len() >= 3);
            let widgets = |page: &ReclaimPage| -> Vec<(u32, gtk::CheckButton)> {
                page.inner
                    .rows
                    .borrow()
                    .iter()
                    .map(|r| (r.pid.get(), r.check.clone()))
                    .collect()
            };
            let before = widgets(&page);
            let second = before[1].1.clone();
            second.grab_focus();
            assert!(second.has_focus());
            second.set_active(true);
            // The top session shrinks below the rest: the order changes, the
            // membership does not.
            tick_changed(&page, &snap, |s| {
                if s.pid == pids[0] {
                    s.pss_bytes = Some(1);
                    s.swap_pss_bytes = Some(0);
                }
            });
            settle(100);
            let order: Vec<u32> = page.inner.texts.borrow().iter().map(|t| t.pid).collect();
            assert_ne!(order, pids, "the memory order really moved");
            let after = widgets(&page);
            assert_eq!(after.iter().map(|(p, _)| *p).collect::<Vec<_>>(), order);
            for (pid, w) in &after {
                let old = before.iter().find(|(p, _)| p == pid).unwrap();
                assert!(&old.1 == w, "row {pid} is the same widget");
            }
            assert!(second.has_focus(), "focus stays on its row");
            assert!(second.is_active(), "and so does its check");
            let on_screen: Vec<u32> = (0..order.len() as i32)
                .map(|i| {
                    let row = page.inner.list.row_at_index(i).unwrap();
                    page.inner
                        .rows
                        .borrow()
                        .iter()
                        .find(|r| r.row == row)
                        .unwrap()
                        .pid
                        .get()
                })
                .collect();
            assert_eq!(on_screen, order, "the list shows the new order");
            win.close();
        });
    }
}
