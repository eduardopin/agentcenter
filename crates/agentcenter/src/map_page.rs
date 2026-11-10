//! Map page (direction B, 07-gui.md "Memory map"): the heading, the verdict
//! line, the "Waiting for you" chips and the Reclaim button above the
//! [`MemoryMap`], and the legend below it. Under 600 px
//! ([`MapPage::set_compact`]) the map gives way to the [`MemoryBar`], a 24 px
//! one-dimensional strip with the group totals under it.
//!
//! The text is pure ([`heading`], [`verdict`], [`chips`], [`reclaim_label`],
//! [`compact_alert`], [`strip_segments`], ...) and tested without a display.

use crate::machine_history::Readout;
use crate::machine_strip::{STALL_LIMIT_PCT, SWAP_LIMIT_PCT, Triangle};
use crate::memory_map::{Glyph, Hatch, MapInput, MapSession, MemoryMap, draw_glyph, gib, gib1};
use crate::token_map::{TokenInput, TokenMap};
use crate::treemap::{GLYPH_PX, TileState};
use adw::prelude::*;
use agentcenter_core::{Snapshot, reclaim as core_reclaim};
use gtk::glib;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Legend and chip glyph box, px.
const MARK_PX: i32 = 12;
/// Memory bar height and the gap between its segments, px.
const BAR_H_PX: i32 = 24;
const BAR_GAP_PX: i32 = 2;
/// Narrowest segment drawn, and the narrowest that gets its glyph, px.
const SEG_MIN_PX: i32 = 4;
const SEG_GLYPH_MIN_PX: i32 = 22;

/// One stacked segment of the [`MemoryBar`]: a state group.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub state: TileState,
    pub bytes: u64,
    /// Share of the whole bar, 0..=1.
    pub fraction: f64,
    /// Swapped-out share of the segment's bytes.
    pub swap_share: f64,
}

/// Floor of the map's height: the paned's top child is 220 px at the least
/// and the list keeps five rows below it, so the map has to fit in what is
/// left of that after its heading and the status row.
const MAP_MIN_H_PX: i32 = 160;

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn total(input: &MapInput) -> u64 {
    input.sessions.iter().map(MapSession::bytes).sum()
}

fn of_state(input: &MapInput, state: TileState) -> impl Iterator<Item = &MapSession> {
    input.sessions.iter().filter(move |s| s.state == state)
}

/// `28.6 GiB held by 38 sessions and 2 multiplexers`; `no sessions` when
/// empty. Sessions are the agents; the bytes are everyone's PSS + swap.
pub fn heading(input: &MapInput) -> String {
    if input.sessions.is_empty() {
        return "no sessions".to_owned();
    }
    let mux = of_state(input, TileState::Multiplexer).count();
    let who = match (input.sessions.len() - mux, mux) {
        (a, 0) => plural(a, "session", "sessions"),
        (0, m) => plural(m, "multiplexer", "multiplexers"),
        (a, m) => format!(
            "{} and {}",
            plural(a, "session", "sessions"),
            plural(m, "multiplexer", "multiplexers")
        ),
    };
    format!("{} held by {who}", gib1(total(input)))
}

/// The compact heading's right side: `28.6 GiB in sessions`.
pub fn compact_heading(input: &MapInput) -> String {
    if input.sessions.is_empty() {
        "no sessions".to_owned()
    } else {
        format!("{} in sessions", gib1(total(input)))
    }
}

/// Weighted tokens as `812`, `12.3k`, `4.1M` or `1.2B`.
pub fn tokens_text(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

fn token_total(input: &TokenInput) -> u64 {
    input.sessions.iter().map(|s| s.since_start).sum()
}

/// `41.2M weighted tokens across 12 sessions`; `no sessions` when empty.
pub fn token_heading(input: &TokenInput) -> String {
    if input.sessions.is_empty() {
        return "no sessions".to_owned();
    }
    format!(
        "{} weighted tokens across {}",
        tokens_text(token_total(input)),
        plural(input.sessions.len(), "session", "sessions")
    )
}

/// The token strip's heading under 600 px: `41.2M tokens`.
pub fn token_compact_heading(input: &TokenInput) -> String {
    if input.sessions.is_empty() {
        "no sessions".to_owned()
    } else {
        format!("{} tokens", tokens_text(token_total(input)))
    }
}

/// The token strip: the state groups in map order, sized by weighted tokens
/// since start; groups without tokens are left out.
pub fn token_segments(input: &TokenInput) -> Vec<Segment> {
    let parts: Vec<(TileState, u64)> = TileState::ORDER
        .iter()
        .map(|&st| {
            let sum = input
                .sessions
                .iter()
                .filter(|s| s.state == st)
                .map(|s| s.since_start)
                .sum();
            (st, sum)
        })
        .filter(|p| p.1 > 0)
        .collect();
    let all: u64 = parts.iter().map(|p| p.1).sum();
    parts
        .into_iter()
        .map(|(st, bytes)| Segment {
            state: st,
            bytes,
            fraction: bytes as f64 / all as f64,
            swap_share: 0.0,
        })
        .collect()
}

/// Window height (sp) up to which the machine strip is one text line, and up
/// to which the maps become strips.
pub const SHORT_MAX_SP: f64 = 760.0;
pub const TINY_MAX_SP: f64 = 560.0;

/// Height tier of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightTier {
    Tall,
    Short,
    Tiny,
}

pub fn height_tier(sp: f64) -> HeightTier {
    if sp <= TINY_MAX_SP {
        HeightTier::Tiny
    } else if sp <= SHORT_MAX_SP {
        HeightTier::Short
    } else {
        HeightTier::Tall
    }
}

/// Width (sp) of the maps region up to which one map shows, and under which
/// the maps are strips.
pub const TOGGLE_MAX_SP: f64 = 999.0;
pub const STRIPS_MAX_SP: f64 = 599.0;

/// Swap and memory stall readings over the machine strip's limits.
fn over(r: &Readout) -> (Option<f64>, Option<f64>) {
    (
        r.swap_pct.filter(|p| *p > SWAP_LIMIT_PCT),
        r.stall_some.filter(|p| *p > STALL_LIMIT_PCT),
    )
}

/// `Tight: swap 73 % full, tasks stalled on memory 12.5 % of the last 10 s`,
/// naming only the readings over their limit; `None` when none is.
pub fn verdict(r: &Readout) -> Option<String> {
    let (swap, stall) = over(r);
    let parts: Vec<String> = [
        swap.map(|p| format!("swap {p:.0} % full")),
        stall.map(|p| format!("tasks stalled on memory {p:.1} % of the last 10 s")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| format!("Tight: {}", parts.join(", ")))
}

/// The "Waiting for you" chips: `(pid, name, "0.54 GiB")`, largest first,
/// a tie in pid order.
pub fn chips(input: &MapInput) -> Vec<(u32, String, String)> {
    let mut waiting: Vec<&MapSession> = of_state(input, TileState::Waiting).collect();
    waiting.sort_by(|a, b| b.bytes().cmp(&a.bytes()).then(a.pid.cmp(&b.pid)));
    waiting
        .into_iter()
        .map(|s| (s.pid, s.name.clone(), gib(s.bytes())))
        .collect()
}

/// `Reclaim 19.0 GiB from 26 idle`; `None` without idle sessions. The
/// sessions are the core's reclaim rule, the same the Reclaim page lists
/// (unknown state only when quiet over the window, never the app's own
/// session), so the button and the page agree.
pub fn reclaim_label(r: &core_reclaim::Reclaim) -> Option<String> {
    (!r.sessions.is_empty()).then(|| {
        format!(
            "Reclaim {} from {} idle",
            gib1(r.total_bytes),
            r.sessions.len()
        )
    })
}

/// The core rule on one snapshot with no CPU window: declared idle only
/// (an unknown state needs the window to prove it quiet).
fn reclaim_without_window(snap: &Snapshot) -> core_reclaim::Reclaim {
    core_reclaim::reclaim(
        snap,
        &core_reclaim::CpuWindow::new(),
        Some(std::process::id()),
    )
}

/// The compact page's alert line: `3 waiting for you · swap 73 % full`.
/// One machine reason at most, to fit 360 px: swap, else memory stall.
pub fn compact_alert(input: &MapInput, r: &Readout) -> Option<String> {
    let waiting = of_state(input, TileState::Waiting).count();
    let (swap, stall) = over(r);
    let machine = swap
        .map(|p| format!("swap {p:.0} % full"))
        .or_else(|| stall.map(|p| format!("memory stall {p:.1} %")));
    let parts: Vec<String> = [
        (waiting > 0).then(|| format!("{waiting} waiting for you")),
        machine,
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// The legend's note: what a tile's area is, and the RAM + swap in use
/// that no session holds (`Other processes: 20.4 GiB`), when known.
pub fn legend_note(input: &MapInput, r: &Readout) -> String {
    let area = "Tile area = PSS + swap.";
    match r.mem_used {
        Some(used) => {
            let other = (used + r.swap_used.unwrap_or(0)).saturating_sub(total(input));
            format!("{area} Other processes: {}", gib1(other))
        }
        None => area.to_owned(),
    }
}

/// The memory bar: the four state groups in map order; groups without
/// bytes are left out. Free RAM is the machine strip's RAM gauge.
pub fn strip_segments(input: &MapInput) -> Vec<Segment> {
    let mut parts: Vec<(TileState, u64, u64)> = TileState::ORDER
        .iter()
        .map(|&st| {
            let (pss, swap) = of_state(input, st).fold((0, 0), |(p, w), s| (p + s.pss, w + s.swap));
            (st, pss + swap, swap)
        })
        .collect();
    parts.retain(|p| p.1 > 0);
    let all: u64 = parts.iter().map(|p| p.1).sum();
    parts
        .into_iter()
        .map(|(state, bytes, swap)| Segment {
            state,
            bytes,
            fraction: bytes as f64 / all as f64,
            swap_share: swap as f64 / bytes as f64,
        })
        .collect()
}

/// A segment's total under the bar: `19.0`.
pub fn segment_total(s: &Segment) -> String {
    crate::machine_strip::gib(s.bytes)
}

fn title_case(state: TileState) -> &'static str {
    match state {
        TileState::Waiting => "Waiting",
        TileState::Busy => "Busy",
        TileState::Idle => "Idle",
        TileState::Multiplexer => "Multiplexer",
    }
}

fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}

// ---- Legend glyph -------------------------------------------------------------

/// What a [`LegendGlyph`] shows.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Mark {
    State(TileState),
    /// A hatched swatch: the swapped-out share.
    #[default]
    Swapped,
}

mod legend_imp {
    use super::*;

    #[derive(Default)]
    pub struct LegendGlyph {
        pub mark: Cell<Mark>,
        pub hatch: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LegendGlyph {
        const NAME: &'static str = "AcLegendGlyph";
        type Type = super::LegendGlyph;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("legendglyph");
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for LegendGlyph {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_can_target(false);
            obj.set_valign(gtk::Align::Center);
            obj.set_overflow(gtk::Overflow::Hidden);
            let hatch: gtk::Widget = glib::Object::new::<Hatch>().upcast();
            hatch.set_parent(&*obj);
            self.hatch.replace(Some(hatch));
        }

        fn dispose(&self) {
            if let Some(h) = self.hatch.take() {
                h.unparent();
            }
        }
    }

    impl WidgetImpl for LegendGlyph {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (MARK_PX, MARK_PX, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let full = self.mark.get() == Mark::Swapped;
            if let Some(h) = self.hatch.borrow().as_ref() {
                h.measure(gtk::Orientation::Horizontal, -1);
                let (w, hh) = if full { (width, height) } else { (0, 0) };
                h.size_allocate(&gtk::Allocation::new(0, 0, w, hh), -1);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let color = obj.color();
            match self.mark.get() {
                Mark::State(s) => {
                    let g = GLYPH_PX as f32;
                    draw_glyph(snapshot, Glyph::of(s), (w - g) / 2.0, (h - g) / 2.0, &color);
                }
                Mark::Swapped => {}
            }
        }
    }
}

glib::wrapper! {
    /// A 12 px legend mark: a state glyph in its fill colour, the hatched
    /// swatch. CSS: `legendglyph.<state>|swapped`;
    /// a bare one (in a bar segment) draws in the inherited colour.
    pub struct LegendGlyph(ObjectSubclass<legend_imp::LegendGlyph>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LegendGlyph {
    /// A mark with its colour class.
    pub fn new(mark: Mark) -> Self {
        let g = Self::bare(mark);
        g.add_css_class(match mark {
            Mark::State(s) => s.word(),
            Mark::Swapped => "swapped",
        });
        g
    }

    /// A mark without a colour class: it draws in its parent's colour.
    pub fn bare(mark: Mark) -> Self {
        let g: Self = glib::Object::new();
        g.imp().mark.set(mark);
        g
    }
}

// ---- Memory bar ---------------------------------------------------------------

mod bar_imp {
    use super::*;

    #[derive(Default)]
    pub struct BarStrip {
        pub segments: RefCell<Vec<Segment>>,
        /// Bar height in px; 0 means the 24 px default.
        pub height: Cell<i32>,
        /// One child per slot: the four states in map order.
        pub slots: RefCell<Vec<(gtk::Widget, LegendGlyph, gtk::Widget)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BarStrip {
        const NAME: &'static str = "AcMemoryBarStrip";
        type Type = super::BarStrip;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("memorybar");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for BarStrip {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            let marks = TileState::ORDER.map(Mark::State);
            let slots = marks
                .into_iter()
                .map(|mark| {
                    let seg: gtk::Widget = gtk::Box::builder()
                        .css_name("segment")
                        .overflow(gtk::Overflow::Hidden)
                        .build()
                        .upcast();
                    seg.add_css_class(match mark {
                        Mark::State(s) => s.word(),
                        Mark::Swapped => "swapped",
                    });
                    seg.set_parent(&*obj);
                    let glyph = LegendGlyph::bare(mark);
                    glyph.set_parent(&seg);
                    glyph.set_visible(matches!(mark, Mark::State(_)));
                    let hatch: gtk::Widget = glib::Object::new::<Hatch>().upcast();
                    hatch.set_parent(&seg);
                    (seg, glyph, hatch)
                })
                .collect();
            self.slots.replace(slots);
        }

        fn dispose(&self) {
            for (seg, glyph, hatch) in self.slots.take() {
                glyph.unparent();
                hatch.unparent();
                seg.unparent();
            }
        }
    }

    impl WidgetImpl for BarStrip {
        fn measure(&self, o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match o {
                gtk::Orientation::Vertical => {
                    let h = match self.height.get() {
                        0 => BAR_H_PX,
                        h => h,
                    };
                    (h, h, -1, -1)
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let segments = self.segments.borrow();
            let slots = self.slots.borrow();
            let n = segments.len() as i32;
            let room = (width - BAR_GAP_PX * (n - 1).max(0)).max(0);
            let mut x = 0;
            for (i, (seg, glyph, hatch)) in slots.iter().enumerate() {
                let state = TileState::ORDER.get(i).copied();
                let Some(s) = segments.iter().find(|s| Some(s.state) == state) else {
                    seg.set_child_visible(false);
                    continue;
                };
                let last = segments.last() == Some(s);
                let w = if last {
                    (width - x).max(0)
                } else {
                    ((s.fraction * f64::from(room)).round() as i32).max(SEG_MIN_PX)
                };
                seg.set_child_visible(true);
                seg.measure(gtk::Orientation::Horizontal, -1);
                seg.size_allocate(&gtk::Allocation::new(x, 0, w, height), -1);
                // Inside the segment: the glyph at the left, the swapped-out
                // share hatched at the right end.
                glyph.measure(gtk::Orientation::Horizontal, -1);
                glyph.set_child_visible(state.is_some() && w >= SEG_GLYPH_MIN_PX);
                let gy = (height - MARK_PX) / 2;
                glyph.size_allocate(&gtk::Allocation::new(6, gy, MARK_PX, MARK_PX), -1);
                let hw = (f64::from(w) * s.swap_share).round() as i32;
                hatch.measure(gtk::Orientation::Horizontal, -1);
                hatch.size_allocate(&gtk::Allocation::new(w - hw, 0, hw, height), -1);
                x += w + BAR_GAP_PX;
            }
        }
    }
}

glib::wrapper! {
    /// The 24 px stacked strip. CSS: `memorybar > segment.<state>`,
    /// `segment > legendglyph`, `segment > hatch`.
    pub struct BarStrip(ObjectSubclass<bar_imp::BarStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BarStrip {
    /// A strip `height` px tall (the capacity bar is 8).
    fn with_height(height: i32) -> Self {
        let strip: Self = glib::Object::new();
        strip.imp().height.set(height);
        strip
    }

    fn set_segments(&self, segments: Vec<Segment>) {
        let spoken: Vec<String> = segments
            .iter()
            .map(|s| format!("{} {}", s.state.word(), gib1(s.bytes)))
            .collect();
        let label = format!("Memory: {}", spoken.join(", "));
        self.set_segments_labelled(segments, &label);
    }

    fn set_segments_labelled(&self, segments: Vec<Segment>, label: &str) {
        self.update_property(&[gtk::accessible::Property::Label(label)]);
        self.imp().segments.replace(segments);
        self.queue_allocate();
        self.queue_draw();
    }
}

/// The one-dimensional memory map under 600 px: the [`BarStrip`] and the
/// group totals as text below it (`▲ 1.2  ● 6.7  ○ 19.0  ▦ 1.7`).
pub struct MemoryBar {
    widget: gtk::Box,
    strip: BarStrip,
    /// Per slot (states in map order): the pair and its label.
    totals: Vec<(gtk::Box, gtk::Label)>,
}

impl MemoryBar {
    pub fn new() -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        widget.add_css_class("memory-bar");
        let strip: BarStrip = glib::Object::new();
        widget.append(&strip);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("totals");
        let marks = TileState::ORDER.map(Mark::State);
        let totals = marks
            .into_iter()
            .map(|mark| {
                let pair = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                pair.append(&LegendGlyph::new(mark));
                let label = gtk::Label::new(None);
                pair.append(&label);
                row.append(&pair);
                (pair, label)
            })
            .collect();
        widget.append(&row);
        Self {
            widget,
            strip,
            totals,
        }
    }

    pub fn update(&self, input: &MapInput) {
        let segments = strip_segments(input);
        for (i, (pair, label)) in self.totals.iter().enumerate() {
            let state = TileState::ORDER.get(i).copied();
            let seg = segments.iter().find(|s| Some(s.state) == state);
            pair.set_visible(seg.is_some());
            if let Some(s) = seg {
                label.set_label(&segment_total(s));
            }
        }
        self.strip.set_segments(segments);
    }
}

// ---- Page ---------------------------------------------------------------------

type Open = Rc<dyn Fn(u32, Source)>;
type Reclaim = Rc<dyn Fn()>;

#[derive(Default)]
struct Shared {
    on_open: RefCell<Vec<Open>>,
    on_reclaim: RefCell<Vec<Reclaim>>,
    on_layout: RefCell<Vec<Reclaim>>,
}

impl Shared {
    fn open(&self, pid: u32, source: Source) {
        let callbacks = self.on_open.borrow().clone();
        for f in callbacks {
            f(pid, source);
        }
    }
}

/// Width tier of the maps region (RFC layout): both maps, one map with a
/// Tokens | Memory toggle, or two labelled one-dimensional strips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    #[default]
    Wide,
    Toggle,
    Strips,
}

/// The map the toggle tier shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shown {
    #[default]
    Tokens,
    Memory,
}

/// Which map a tile or chip came from, so Esc can focus it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Token,
    Memory,
    Chip,
}

/// The maps region: a status row over both maps (verdict, waiting chips,
/// Reclaim), the token map and the memory map with their headings, the
/// legend. The wrapper derefs to [`Ui`] so the breakpoint handlers can hold a
/// weak reference to the same state.
pub struct MapPage(Rc<Ui>);

impl std::ops::Deref for MapPage {
    type Target = Ui;
    fn deref(&self) -> &Ui {
        &self.0
    }
}

pub struct Ui {
    root: gtk::Box,
    bin: adw::BreakpointBin,
    toggle: adw::ToggleGroup,
    status_row: gtk::Box,
    maps_row: gtk::Box,
    tokens_col: gtk::Box,
    memory_col: gtk::Box,
    token_map: TokenMap,
    token_legend: gtk::Widget,
    token_heading: gtk::Label,
    token_strip: BarStrip,
    token_input: RefCell<TokenInput>,
    capacity: BarStrip,
    heading: gtk::Label,
    verdict_row: gtk::Box,
    verdict: gtk::Label,
    alert_row: gtk::Box,
    alert: gtk::Label,
    chips_row: gtk::Box,
    chips: adw::WrapBox,
    shown_chips: RefCell<Vec<(u32, String, String)>>,
    /// The chip buttons and their GiB labels, in `shown_chips` order.
    chip_widgets: RefCell<Vec<(gtk::Button, gtk::Label)>>,
    reclaim: gtk::Button,
    reclaim_label: gtk::Label,
    map: MemoryMap,
    map_frame: gtk::Overlay,
    empty_text: gtk::Label,
    bar: MemoryBar,
    legend: gtk::Box,
    note: gtk::Label,
    tier: Cell<Tier>,
    shown: Cell<Shown>,
    height_sp: Cell<f64>,
    last: RefCell<(MapInput, Readout)>,
    /// The Reclaim page's own list for this tick, when the window has one.
    window_reclaim: RefCell<Option<core_reclaim::Reclaim>>,
    reclaim_text: RefCell<Option<String>>,
    shared: Rc<Shared>,
}

impl Default for MapPage {
    fn default() -> Self {
        Self::new()
    }
}

fn alert_line() -> (gtk::Box, gtk::Label) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("verdict");
    let glyph = Triangle::default();
    glyph.set_valign(gtk::Align::Center);
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    row.append(&glyph);
    row.append(&label);
    row.set_visible(false);
    (row, label)
}

/// A column heading: the bold title and the lede beside it.
fn head_row(title: &str) -> (gtk::Box, gtk::Label) {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.add_css_class("head");
    let title = gtk::Label::new(Some(title));
    title.add_css_class("title");
    let lede = gtk::Label::new(None);
    lede.add_css_class("lede"); // not `heading`: Adwaita makes that bold
    lede.set_hexpand(true);
    lede.set_xalign(0.0);
    lede.set_wrap(true);
    lede.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    head.append(&title);
    head.append(&lede);
    (head, lede)
}

impl MapPage {
    pub fn new() -> Self {
        let ui = Rc::new(Ui::build());
        Ui::wire(&ui);
        Self(ui)
    }
}

impl Ui {
    fn build() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("map-page");
        // Window-level breakpoints do not combine, so the region's own width
        // picks the tier here. The minimum is the narrowest window's.
        let bin = adw::BreakpointBin::new();
        bin.set_size_request(280, 1);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 10);
        bin.set_child(Some(&inner));
        root.append(&bin);

        let toggle = adw::ToggleGroup::new();
        toggle.add(
            adw::Toggle::builder()
                .name("tokens")
                .label("Tokens")
                .build(),
        );
        toggle.add(
            adw::Toggle::builder()
                .name("memory")
                .label("Memory")
                .build(),
        );
        toggle.set_active_name(Some("tokens"));
        toggle.set_halign(gtk::Align::Start);
        toggle.set_visible(false);

        // One row over both maps: the Tight verdict, the waiting chips, Reclaim.
        let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_row.add_css_class("status");
        let (verdict_row, verdict) = alert_line();
        // The sentence is capped so the chips (natural width) and Reclaim keep
        // one line from 1000 sp up; the tooltip carries all of it.
        verdict.set_max_width_chars(16);
        let (alert_row, alert) = alert_line();
        let reclaim_label = gtk::Label::new(None);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.append(&gtk::Image::from_icon_name("folder-download-symbolic"));
        content.append(&reclaim_label);
        let reclaim = gtk::Button::builder()
            .child(&content)
            .valign(gtk::Align::Start)
            .visible(false)
            .build();
        reclaim.add_css_class("reclaim");

        let chips_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        chips_row.add_css_class("chips");
        chips_row.set_hexpand(true);
        let caption = gtk::Label::new(Some("Waiting for you"));
        caption.add_css_class("caption");
        caption.set_valign(gtk::Align::Center);
        let chips = adw::WrapBox::builder()
            .child_spacing(8)
            .line_spacing(6)
            .hexpand(true)
            .build();
        chips_row.append(&caption);
        chips_row.append(&chips);
        chips_row.set_visible(false);
        status_row.append(&verdict_row);
        status_row.append(&chips_row);
        status_row.append(&reclaim);

        // Left half: the token map. In strips it is a labelled 24 px strip.
        let tokens_col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let (token_head, token_heading) = head_row("Tokens");
        let token_map = TokenMap::new();
        token_map.set_size_request(-1, MAP_MIN_H_PX);
        token_map.set_vexpand(true);
        let token_strip = BarStrip::with_height(BAR_H_PX);
        token_strip.set_visible(false);
        tokens_col.append(&token_head);
        tokens_col.append(&token_map);
        let token_legend = crate::token_map::legend();
        tokens_col.append(&token_legend);
        tokens_col.append(&token_strip);

        // Right half: the memory map under an 8 px capacity bar.
        let memory_col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let (head, heading) = head_row("Memory");
        let capacity = BarStrip::with_height(8);
        capacity.add_css_class("capacity");
        let map = MemoryMap::new();
        map.set_size_request(-1, MAP_MIN_H_PX);
        let bar = MemoryBar::new();
        bar.widget.set_visible(false);
        // The empty page: the free tile alone with the text centred over it.
        let empty_text = gtk::Label::new(Some("No AI sessions running"));
        empty_text.add_css_class("empty-text");
        empty_text.set_halign(gtk::Align::Center);
        empty_text.set_valign(gtk::Align::Center);
        empty_text.set_can_target(false);
        empty_text.set_visible(false);
        let map_frame = gtk::Overlay::new();
        map_frame.set_child(Some(&map));
        map_frame.add_overlay(&empty_text);
        map_frame.set_vexpand(true);
        memory_col.append(&head);
        memory_col.append(&capacity);
        memory_col.append(&map_frame);
        memory_col.append(&bar.widget);

        let maps_row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        maps_row.set_homogeneous(true);
        maps_row.set_vexpand(true);
        maps_row.append(&tokens_col);
        maps_row.append(&memory_col);

        let legend = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        legend.add_css_class("legend");
        let marks = TileState::ORDER.map(|s| (Mark::State(s), title_case(s)));
        for (mark, word) in marks.into_iter().chain([(Mark::Swapped, "Swapped out")]) {
            let pair = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            pair.append(&LegendGlyph::new(mark));
            pair.append(&gtk::Label::new(Some(word)));
            legend.append(&pair);
        }
        let note = gtk::Label::new(None);
        note.add_css_class("note");
        note.set_hexpand(true);
        note.set_xalign(0.0);
        note.set_ellipsize(gtk::pango::EllipsizeMode::End);
        legend.append(&note);

        inner.append(&toggle);
        inner.append(&status_row);
        inner.append(&alert_row);
        inner.append(&maps_row);
        inner.append(&legend);

        let shared = Rc::new(Shared::default());
        let weak = Rc::downgrade(&shared);
        map.connect_activated(move |pid| {
            if let Some(s) = weak.upgrade() {
                s.open(pid, Source::Memory);
            }
        });
        let weak = Rc::downgrade(&shared);
        token_map.connect_activated(move |pid| {
            if let Some(s) = weak.upgrade() {
                s.open(pid, Source::Token);
            }
        });
        let weak = Rc::downgrade(&shared);
        reclaim.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                let callbacks = s.on_reclaim.borrow().clone();
                for f in callbacks {
                    f();
                }
            }
        });
        Self {
            root,
            bin,
            toggle,
            status_row,
            maps_row,
            tokens_col,
            memory_col,
            token_map,
            token_legend,
            token_heading,
            token_strip,
            token_input: RefCell::default(),
            capacity,
            heading,
            verdict_row,
            verdict,
            alert_row,
            alert,
            chips_row,
            chips,
            shown_chips: RefCell::default(),
            chip_widgets: RefCell::default(),
            window_reclaim: RefCell::default(),
            reclaim_text: RefCell::default(),
            reclaim,
            reclaim_label,
            map,
            map_frame,
            empty_text,
            bar,
            legend,
            note,
            tier: Cell::new(Tier::Wide),
            shown: Cell::new(Shown::Tokens),
            height_sp: Cell::new(f64::MAX),
            last: RefCell::default(),
            shared,
        }
    }

    /// The region's own breakpoints and the toggle. Weak references: the
    /// widgets must not keep the state alive.
    fn wire(ui: &Rc<Self>) {
        let length = |max: f64| {
            adw::Breakpoint::new(adw::BreakpointCondition::new_length(
                adw::BreakpointConditionLengthType::MaxWidth,
                max,
                adw::LengthUnit::Sp,
            ))
        };
        let (mid, narrow) = (length(TOGGLE_MAX_SP), length(STRIPS_MAX_SP));
        // Only the last matching breakpoint applies, so the tier follows
        // whichever of the two is on.
        let on = Rc::new(Cell::new((false, false)));
        for (bp, which) in [(&mid, 0), (&narrow, 1)] {
            for apply in [true, false] {
                let (weak, on) = (Rc::downgrade(ui), on.clone());
                let handler = move |_: &adw::Breakpoint| {
                    let mut flags = on.get();
                    if which == 0 {
                        flags.0 = apply;
                    } else {
                        flags.1 = apply;
                    }
                    on.set(flags);
                    if let Some(ui) = weak.upgrade() {
                        ui.set_tier(match flags {
                            (_, true) => Tier::Strips,
                            (true, false) => Tier::Toggle,
                            _ => Tier::Wide,
                        });
                    }
                };
                if apply {
                    bp.connect_apply(handler);
                } else {
                    bp.connect_unapply(handler);
                }
            }
        }
        ui.bin.add_breakpoint(mid);
        ui.bin.add_breakpoint(narrow);
        let weak = Rc::downgrade(ui);
        ui.toggle.connect_active_name_notify(move |t| {
            if let Some(ui) = weak.upgrade() {
                ui.set_shown(if t.active_name().as_deref() == Some("memory") {
                    Shown::Memory
                } else {
                    Shown::Tokens
                });
            }
        });
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// The height the page needs at `width`. The bin reports the height of
    /// its reference layout, short of what the child takes once the strips
    /// are on, so the child is asked directly.
    pub fn needed_height(&self, width: i32) -> i32 {
        let (_, page, _, _) = self.root.measure(gtk::Orientation::Vertical, width);
        let (_, bin, _, _) = self.bin.measure(gtk::Orientation::Vertical, width);
        let child = self
            .bin
            .child()
            .map_or(0, |c| c.measure(gtk::Orientation::Vertical, width).1);
        // The bin allocates its child no more than the reference layout's
        // height and clips the rest; ask for what the child needs.
        self.bin.set_size_request(280, child);
        page + (child - bin).max(0)
    }

    /// New snapshot: the map, the bar, the chips and every line of text.
    pub fn update(&self, snap: &Snapshot, r: &Readout) {
        let input = MapInput::from_snapshot(snap);
        self.map.set_input(input.clone());
        self.bar.update(&input);
        self.capacity.set_segments(strip_segments(&input));
        self.set_chips(chips(&input));
        let reclaim = self
            .window_reclaim
            .take()
            .unwrap_or_else(|| reclaim_without_window(snap));
        self.reclaim_text.replace(reclaim_label(&reclaim));
        self.last.replace((input, *r));
        self.render();
    }

    /// The token map's input for this tick, built from the spend tracker.
    pub fn set_token_input(&self, input: TokenInput) {
        self.token_map.set_input(input.clone());
        let spoken = format!("Tokens: {}", token_heading(&input));
        self.token_strip
            .set_segments_labelled(token_segments(&input), &spoken);
        self.token_input.replace(input);
        self.render();
    }

    #[cfg(test)]
    pub fn token_input(&self) -> TokenInput {
        self.token_input.borrow().clone()
    }

    /// The Reclaim page's list for the next [`MapPage::update`]: the button
    /// then counts the same sessions. Without it the button counts the
    /// declared idle ones only.
    pub fn set_window_reclaim(&self, reclaim: Option<core_reclaim::Reclaim>) {
        self.window_reclaim.replace(reclaim);
    }

    /// A tile or a chip asked to open a session.
    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    pub fn connect_open(&self, f: impl Fn(u32) + 'static) {
        self.connect_open_from(move |pid, _| f(pid));
    }

    /// Like [`Ui::connect_open`], with the map or chip that asked.
    pub fn connect_open_from(&self, f: impl Fn(u32, Source) + 'static) {
        self.shared.on_open.borrow_mut().push(Rc::new(f));
    }

    /// The page was rendered: maps turned into strips or back, or new chips and
    /// lines (the paned re-clamps).
    pub fn connect_layout_changed(&self, f: impl Fn() + 'static) {
        self.shared.on_layout.borrow_mut().push(Rc::new(f));
    }

    /// The Reclaim button.
    pub fn connect_reclaim(&self, f: impl Fn() + 'static) {
        self.shared.on_reclaim.borrow_mut().push(Rc::new(f));
    }

    /// The session the detail shows: both maps' selection ring.
    pub fn set_selected(&self, pid: Option<u32>) {
        self.map.set_selected(pid);
        self.token_map.set_selected(pid);
    }

    /// Focus goes back to the map (or chip row) a session was opened from.
    /// `false` when that map is not on screen (the caller falls back to the
    /// list row).
    pub fn focus_source(&self, source: Source, pid: Option<u32>) -> bool {
        match source {
            Source::Token if self.token_map.is_mapped() => self.token_map.grab_focus(),
            Source::Memory if self.map.is_mapped() => {
                let tile = pid.and_then(|p| {
                    self.map
                        .tiles()
                        .into_iter()
                        .find(|t| MemoryMap::tile_pid(t) == Some(p))
                });
                match tile {
                    Some(t) => t.grab_focus(),
                    None => self.map.grab_focus(),
                }
            }
            _ => false,
        }
    }

    /// Strips (width under 600 sp, or a window 560 sp tall or less).
    pub fn is_compact(&self) -> bool {
        self.strips()
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    pub fn tier(&self) -> Tier {
        self.tier.get()
    }

    /// The width tier; the bin's breakpoints call this.
    pub fn set_tier(&self, tier: Tier) {
        if self.tier.replace(tier) != tier {
            self.render();
        }
    }

    /// Window height in sp: 560 or less turns the maps into strips.
    pub fn set_height_sp(&self, sp: f64) {
        if self.height_sp.replace(sp) != sp {
            self.render();
        }
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    pub fn shown_map(&self) -> Shown {
        self.shown.get()
    }

    /// The map the toggle tier shows.
    pub fn set_shown(&self, shown: Shown) {
        if self.shown.replace(shown) != shown {
            self.toggle.set_active_name(Some(match shown {
                Shown::Tokens => "tokens",
                Shown::Memory => "memory",
            }));
            self.render();
        }
    }

    fn strips(&self) -> bool {
        self.tier.get() == Tier::Strips || height_tier(self.height_sp.get()) == HeightTier::Tiny
    }

    #[cfg(test)]
    pub fn token_map(&self) -> &TokenMap {
        &self.token_map
    }
    #[cfg(test)]
    pub fn memory_map(&self) -> &MemoryMap {
        &self.map
    }
    #[cfg(test)]
    pub fn capacity_bar(&self) -> &BarStrip {
        &self.capacity
    }
    #[cfg(test)]
    pub fn toggle(&self) -> &adw::ToggleGroup {
        &self.toggle
    }
    #[cfg(test)]
    pub fn status_row(&self) -> &gtk::Box {
        &self.status_row
    }
    #[cfg(test)]
    pub fn chips_row(&self) -> &gtk::Box {
        &self.chips_row
    }
    #[cfg(test)]
    pub fn reclaim_button(&self) -> &gtk::Button {
        &self.reclaim
    }

    /// Rebuilds the chips only when the set or the names changed; a new GiB
    /// text is written in place, so keyboard focus on a chip survives a tick.
    fn set_chips(&self, next: Vec<(u32, String, String)>) {
        if *self.shown_chips.borrow() == next {
            return;
        }
        let same_chips = {
            let shown = self.shown_chips.borrow();
            shown.len() == next.len()
                && shown
                    .iter()
                    .zip(&next)
                    .all(|(a, b)| a.0 == b.0 && a.1 == b.1)
        };
        if same_chips {
            for ((button, label), (_, name, value)) in self.chip_widgets.borrow().iter().zip(&next)
            {
                label.set_label(value);
                button.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{name}, {value}, waiting for you"
                ))]);
            }
            self.shown_chips.replace(next);
            return;
        }
        self.chips.remove_all();
        let mut widgets = Vec::new();
        for (pid, name, value) in &next {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            content.append(&LegendGlyph::new(Mark::State(TileState::Waiting)));
            // A long name gives way to the value: the chip's least width must
            // not push a 360 px window wider than itself.
            let name_label = gtk::Label::new(Some(name));
            name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            content.append(&name_label);
            let v = gtk::Label::new(Some(value));
            v.add_css_class("value");
            content.append(&v);
            let chip = gtk::Button::builder().child(&content).build();
            chip.add_css_class("chip");
            chip.set_tooltip_text(Some(name));
            chip.update_property(&[gtk::accessible::Property::Label(&format!(
                "{name}, {value}, waiting for you"
            ))]);
            let weak = Rc::downgrade(&self.shared);
            let pid = *pid;
            chip.connect_clicked(move |_| {
                if let Some(s) = weak.upgrade() {
                    s.open(pid, Source::Chip);
                }
            });
            self.chips.append(&chip);
            widgets.push((chip, v));
        }
        self.chip_widgets.replace(widgets);
        self.shown_chips.replace(next);
    }

    /// Text and visibility from the last snapshot, the tiers and the toggle.
    fn render(&self) {
        let strips = self.strips();
        let tier = self.tier.get();
        let (input, r) = &*self.last.borrow();
        let empty = input.sessions.is_empty();
        let token_input = self.token_input.borrow();
        let both = tier == Tier::Wide;
        let show_tokens = strips || both || self.shown.get() == Shown::Tokens;
        let show_memory = strips || both || self.shown.get() == Shown::Memory;
        if strips {
            self.heading.set_label(&compact_heading(input));
            self.heading.set_xalign(1.0);
            self.token_heading
                .set_label(&token_compact_heading(&token_input));
            self.token_heading.set_xalign(1.0);
        } else {
            self.heading
                .set_label(&format!("\u{b7} {}", heading(input)));
            self.heading.set_xalign(0.0);
            self.token_heading
                .set_label(&format!("\u{b7} {}", token_heading(&token_input)));
            self.token_heading.set_xalign(0.0);
        }
        set_class(&self.heading, "compact", strips);
        set_class(&self.token_heading, "compact", strips);
        let verdict_text = verdict(r);
        self.verdict
            .set_label(verdict_text.as_deref().unwrap_or(""));
        self.verdict.set_tooltip_text(verdict_text.as_deref());
        self.verdict_row
            .set_visible(!strips && verdict_text.is_some());
        let alert_text = compact_alert(input, r);
        self.alert.set_label(alert_text.as_deref().unwrap_or(""));
        self.alert_row.set_visible(strips && alert_text.is_some());
        // The chips stay on narrow pages (they wrap in their row); only a
        // window too short for the maps drops them with the rest of the row.
        let chips_shown = height_tier(self.height_sp.get()) != HeightTier::Tiny
            && !self.shown_chips.borrow().is_empty();
        self.chips_row.set_visible(chips_shown);
        let reclaim = self.reclaim_text.borrow();
        self.reclaim_label
            .set_label(reclaim.as_deref().unwrap_or(""));
        self.reclaim.set_visible(!strips && reclaim.is_some());
        self.status_row.set_visible(!strips || chips_shown);
        // One line only where the three parts fit; below that they stack, and
        // the chips wrap inside their own row.
        self.status_row.set_orientation(if both {
            gtk::Orientation::Horizontal
        } else {
            gtk::Orientation::Vertical
        });
        set_class(&self.root, "strips", strips);
        self.toggle.set_visible(tier == Tier::Toggle && !strips);
        self.maps_row.set_homogeneous(!strips);
        self.maps_row.set_orientation(if strips {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        });
        self.maps_row.set_vexpand(!strips);
        // Stacked strips close up: with the wide 10 px between the page's
        // lines too, the list at 360 x 640 loses its second whole row.
        if let Some(inner) = self.bin.child().and_downcast::<gtk::Box>() {
            inner.set_spacing(if strips { 6 } else { 10 });
        }
        // Narrow, a Tokens strip with no spend is a heading and "no sessions":
        // a row the memory strip needs more.
        let idle_tokens = strips && token_input.sessions.is_empty();
        self.tokens_col.set_visible(show_tokens && !idle_tokens);
        if !strips {
            self.bin.set_size_request(280, 1); // `needed_height` raised it
        }
        self.memory_col.set_visible(show_memory);
        self.token_map.set_visible(!strips);
        self.token_legend.set_visible(!strips);
        self.token_strip.set_visible(strips);
        self.map.set_visible(!strips);
        self.map_frame.set_visible(!strips);
        self.capacity.set_visible(!strips && both);
        self.empty_text.set_visible(empty);
        self.bar.widget.set_visible(strips);
        self.note.set_label(&legend_note(input, r));
        self.legend.set_visible(!strips && show_memory && !empty);
        // Every render, not only a flip: the chips and lines a snapshot brings
        // change the height the strips need, and the paned follows it.
        let callbacks = self.shared.on_layout.borrow().clone();
        for f in callbacks {
            f();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_history::tests::fixture;
    use std::cell::RefCell;
    use std::rc::Rc;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn input(name: &str) -> MapInput {
        MapInput::from_snapshot(&fixture(name))
    }

    fn readout(name: &str) -> Readout {
        Readout::of(&fixture(name))
    }

    fn session(pid: u32, state: TileState, pss: u64, swap: u64) -> MapSession {
        MapSession {
            pid,
            name: format!("s{pid}"),
            state,
            pss,
            swap,
        }
    }

    fn made(sessions: Vec<MapSession>) -> MapInput {
        MapInput {
            sessions,
            free_bytes: 2 * GIB,
            taken_at_ms: 0,
        }
    }

    /// Swap and stall on their own; every other reading absent.
    fn pressure(swap: Option<f64>, stall: Option<f64>) -> Readout {
        Readout {
            swap_pct: swap,
            stall_some: stall,
            ..Readout::default()
        }
    }

    // ---- Pure text ------------------------------------------------------------

    #[test]
    fn heading_reads_like_the_mockup_on_tipico_40() {
        assert_eq!(
            heading(&input("tipico-40")),
            "28.6 GiB held by 38 sessions and 2 multiplexers"
        );
        assert_eq!(compact_heading(&input("tipico-40")), "28.6 GiB in sessions");
    }

    #[test]
    fn heading_singular_forms_and_no_multiplexer_clause() {
        let one = made(vec![
            session(1, TileState::Idle, GIB, 0),
            session(2, TileState::Multiplexer, GIB / 2, 0),
        ]);
        assert_eq!(heading(&one), "1.5 GiB held by 1 session and 1 multiplexer");
        let no_mux = made(vec![
            session(1, TileState::Busy, GIB, 0),
            session(2, TileState::Waiting, 0, GIB),
        ]);
        assert_eq!(heading(&no_mux), "2.0 GiB held by 2 sessions");
        let only_mux = made(vec![
            session(1, TileState::Multiplexer, GIB, 0),
            session(2, TileState::Multiplexer, GIB, 0),
        ]);
        assert_eq!(heading(&only_mux), "2.0 GiB held by 2 multiplexers");
    }

    #[test]
    fn heading_on_the_empty_snapshot() {
        assert_eq!(heading(&input("vazio")), "no sessions");
        assert_eq!(compact_heading(&input("vazio")), "no sessions");
    }

    #[test]
    fn verdict_lists_only_what_is_over_its_limit() {
        assert_eq!(
            verdict(&readout("tipico-40")).as_deref(),
            Some("Tight: swap 73 % full, tasks stalled on memory 12.5 % of the last 10 s")
        );
        assert_eq!(
            verdict(&pressure(Some(73.0), Some(2.0))).as_deref(),
            Some("Tight: swap 73 % full")
        );
        assert_eq!(
            verdict(&pressure(Some(10.0), Some(12.5))).as_deref(),
            Some("Tight: tasks stalled on memory 12.5 % of the last 10 s")
        );
    }

    #[test]
    fn verdict_is_none_when_nothing_is_over() {
        assert_eq!(verdict(&pressure(Some(20.0), Some(1.0))), None);
        assert_eq!(verdict(&pressure(None, None)), None, "unknown is not tight");
        assert_eq!(
            verdict(&pressure(Some(50.0), Some(10.0))),
            None,
            "the limits are strictly over, as on the machine strip"
        );
        assert_eq!(verdict(&readout("vazio")), None);
    }

    #[test]
    fn chips_are_the_waiting_sessions_largest_first() {
        let got = chips(&input("tipico-40"));
        let names: Vec<(&str, &str)> = got
            .iter()
            .map(|(_, n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("ledger-auth-refactor", "0.54 GiB"),
                ("atlas-api-export-csv", "0.39 GiB"),
                ("sable-export-csv", "0.28 GiB"),
            ]
        );
        let waiting: Vec<u32> = input("tipico-40")
            .sessions
            .iter()
            .filter(|s| s.state == TileState::Waiting)
            .map(|s| s.pid)
            .collect();
        assert!(got.iter().all(|(pid, ..)| waiting.contains(pid)));

        let tie = made(vec![
            session(9, TileState::Waiting, GIB, 0),
            session(3, TileState::Waiting, GIB, 0),
            session(5, TileState::Busy, 4 * GIB, 0),
        ]);
        let pids: Vec<u32> = chips(&tie).iter().map(|c| c.0).collect();
        assert_eq!(pids, [3, 9], "a tie keeps pid order; busy is no chip");
        assert!(chips(&input("vazio")).is_empty());
    }

    #[test]
    fn reclaim_label_counts_the_core_reclaim_list() {
        let rule = |snap: &Snapshot| reclaim_without_window(snap);
        assert_eq!(
            reclaim_label(&rule(&fixture("tipico-40"))).as_deref(),
            Some("Reclaim 19.0 GiB from 26 idle")
        );
        assert_eq!(reclaim_label(&rule(&fixture("vazio"))), None);
        let none = core_reclaim::Reclaim {
            total_bytes: 0,
            sessions: Vec::new(),
        };
        assert_eq!(reclaim_label(&none), None);
    }

    #[test]
    fn compact_alert_joins_waiting_and_one_machine_reason() {
        assert_eq!(
            compact_alert(&input("tipico-40"), &readout("tipico-40")).as_deref(),
            Some("3 waiting for you \u{b7} swap 73 % full")
        );
        let calm_list = made(vec![session(1, TileState::Busy, GIB, 0)]);
        assert_eq!(
            compact_alert(&calm_list, &pressure(Some(73.0), None)).as_deref(),
            Some("swap 73 % full")
        );
        let one = made(vec![session(1, TileState::Waiting, GIB, 0)]);
        assert_eq!(
            compact_alert(&one, &pressure(Some(5.0), Some(12.5))).as_deref(),
            Some("1 waiting for you \u{b7} memory stall 12.5 %")
        );
        assert_eq!(
            compact_alert(&one, &pressure(Some(5.0), Some(1.0))).as_deref(),
            Some("1 waiting for you")
        );
        assert_eq!(compact_alert(&calm_list, &pressure(Some(5.0), None)), None);
        assert_eq!(compact_alert(&input("vazio"), &readout("vazio")), None);
    }

    #[test]
    fn legend_note_names_the_other_processes() {
        assert_eq!(
            legend_note(&input("tipico-40"), &readout("tipico-40")),
            "Tile area = PSS + swap. Other processes: 20.4 GiB"
        );
        assert_eq!(
            legend_note(&input("tipico-40"), &Readout::default()),
            "Tile area = PSS + swap.",
            "no RAM reading, no remainder"
        );
    }

    #[test]
    fn strip_segments_on_tipico_40() {
        let m = input("tipico-40");
        let segs = strip_segments(&m);
        let states: Vec<TileState> = segs.iter().map(|s| s.state).collect();
        assert_eq!(
            states,
            [
                TileState::Waiting,
                TileState::Busy,
                TileState::Idle,
                TileState::Multiplexer
            ],
            "no free-RAM segment"
        );
        let totals: Vec<String> = segs.iter().map(segment_total).collect();
        assert_eq!(totals, ["1.2", "6.7", "19.0", "1.7"]);
        let sum: f64 = segs.iter().map(|s| s.fraction).sum();
        assert!((sum - 1.0).abs() < 1e-9, "fractions sum to 1: {sum}");
        let all: u64 = segs.iter().map(|s| s.bytes).sum();
        for s in &segs {
            assert!((s.fraction - s.bytes as f64 / all as f64).abs() < 1e-12);
        }
        let idle = &segs[2];
        let (pss, swap) = m
            .sessions
            .iter()
            .filter(|s| s.state == TileState::Idle)
            .fold((0, 0), |(p, w), s| (p + s.pss, w + s.swap));
        assert_eq!(idle.bytes, pss + swap);
        assert!((idle.swap_share - swap as f64 / (pss + swap) as f64).abs() < 1e-12);
        assert!(idle.swap_share > 0.0);
    }

    #[test]
    fn strip_segments_drop_empty_groups() {
        assert!(
            strip_segments(&input("vazio")).is_empty(),
            "free RAM alone is no bar"
        );
        let busy = made(vec![session(1, TileState::Busy, 2 * GIB, 0)]);
        let segs = strip_segments(&busy);
        assert_eq!(
            segs.iter().map(|s| s.state).collect::<Vec<_>>(),
            [TileState::Busy]
        );
        assert_eq!(segs[0].fraction, 1.0);
        assert_eq!(segs[0].swap_share, 0.0);
        let nothing = MapInput::default();
        assert!(strip_segments(&nothing).is_empty(), "no bytes, no bar");
    }

    // ---- GTK ------------------------------------------------------------------

    fn settle() {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
        let context = gtk::glib::MainContext::default();
        while std::time::Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Visible in the page: the widget and every ancestor up to the root.
    fn shown(w: &impl IsA<gtk::Widget>, page: &MapPage) -> bool {
        let root: gtk::Widget = page.widget().clone().upcast();
        let mut at: Option<gtk::Widget> = Some(w.as_ref().clone());
        while let Some(x) = at {
            if !x.is_visible() {
                return false;
            }
            if x == root {
                return true;
            }
            at = x.parent();
        }
        false
    }

    fn chip_buttons(page: &MapPage) -> Vec<gtk::Button> {
        let mut out = Vec::new();
        let mut child = page.chips.first_child();
        while let Some(c) = child {
            if let Some(b) = c
                .first_child()
                .and_downcast::<gtk::Button>()
                .or_else(|| c.clone().downcast::<gtk::Button>().ok())
            {
                out.push(b);
            }
            child = c.next_sibling();
        }
        out
    }

    /// Loads tokens.css and map_page.css; returns their parse errors.
    fn load_css() -> Vec<String> {
        let css = gtk::CssProvider::new();
        let errors = Rc::new(RefCell::new(Vec::new()));
        let e = errors.clone();
        css.connect_parsing_error(move |_, section, err| {
            e.borrow_mut().push(format!("{}: {err}", section.to_str()));
        });
        css.load_from_string(concat!(
            include_str!("../resources/tokens.css"),
            include_str!("../resources/map_page.css"),
        ));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        errors.take()
    }

    #[test]
    fn update_on_tipico_40_shows_chips_reclaim_and_the_map() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            assert_eq!(load_css(), Vec::<String>::new(), "map_page.css parses");
            let page = MapPage::new();
            let win = gtk::Window::new();
            win.set_default_size(1100, 620);
            win.set_child(Some(page.widget()));
            win.present();
            let opened = Rc::new(RefCell::new(Vec::new()));
            let o = opened.clone();
            page.connect_open(move |pid| o.borrow_mut().push(pid));
            let reclaimed = Rc::new(RefCell::new(0));
            let r = reclaimed.clone();
            page.connect_reclaim(move || *r.borrow_mut() += 1);

            let snap = fixture("tipico-40");
            page.update(&snap, &Readout::of(&snap));
            settle();

            let buttons = chip_buttons(&page);
            assert_eq!(buttons.len(), 3, "one chip per waiting session");
            assert!(shown(&page.chips, &page));
            assert!(buttons[0].has_css_class("chip"));
            assert!(shown(&page.reclaim, &page), "Reclaim button visible");
            assert!(shown(&page.map, &page), "the MemoryMap is on the page");
            assert!(!shown(&page.bar.widget, &page), "no bar at full width");
            // At half the page width the map merges small sessions, so the tile
            // count is not the snapshot's; a waiting session (a chip) never merges.
            let tiles = page.map.tiles();
            assert!(tiles.len() > 1, "the map got the snapshot");
            for (pid, ..) in chips(&MapInput::from_snapshot(&snap)) {
                assert!(
                    tiles.iter().any(|t| MemoryMap::tile_pid(t) == Some(pid)),
                    "waiting session {pid} has its own tile"
                );
            }

            // A chip opens its session; the button calls back.
            buttons[0].emit_clicked();
            let first = chips(&MapInput::from_snapshot(&snap))[0].0;
            assert_eq!(*opened.borrow(), vec![first]);
            page.reclaim.emit_clicked();
            assert_eq!(*reclaimed.borrow(), 1);

            // The same snapshot again keeps the chip widgets (focus survives).
            page.update(&snap, &Readout::of(&snap));
            assert_eq!(chip_buttons(&page), buttons);

            page.set_selected(Some(first));
            assert_eq!(page.map.selected(), Some(first));

            // Under 600 px the bar takes the map's place.
            page.set_tier(Tier::Strips);
            settle();
            assert!(shown(&page.bar.widget, &page), "MemoryBar in compact");
            assert!(!shown(&page.map, &page), "no map in compact");
            assert!(
                !shown(&page.reclaim, &page),
                "reclaim lives in the bottom bar"
            );
            page.set_tier(Tier::Wide);
            assert!(shown(&page.map, &page));
            assert!(!shown(&page.bar.widget, &page));
            win.destroy();
        });
    }

    /// The page in a window `width` px wide with the stylesheet, tipico-40.
    fn presented(width: i32) -> (MapPage, gtk::Window) {
        assert_eq!(load_css(), Vec::<String>::new(), "map_page.css parses");
        let page = MapPage::new();
        let win = gtk::Window::new();
        win.set_default_size(width, 700);
        win.set_child(Some(page.widget()));
        win.present();
        let snap = fixture("tipico-40");
        page.update(&snap, &Readout::of(&snap));
        settle();
        (page, win)
    }

    /// The heading reads "Memory" in bold and the rest in regular weight,
    /// whole, at the width the 1280 window gives the page (about 800 px).
    #[test]
    fn heading_is_regular_weight_and_never_ellipsized_beside_reclaim() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let (page, win) = presented(1100);
            let weight = |l: &gtk::Label| l.pango_context().font_description().unwrap().weight();
            // The memory column's heading row now leads with its own title.
            let title = page
                .memory_col
                .first_child()
                .and_then(|head| head.first_child())
                .and_downcast::<gtk::Label>()
                .expect("the Memory title leads the heading row");
            assert_eq!(title.label(), "Memory");
            assert!(weight(&title) >= gtk::pango::Weight::Bold, "title is bold");
            assert_eq!(weight(&page.heading), gtk::pango::Weight::Normal);
            assert!(!page.heading.layout().is_ellipsized(), "no ellipsis");
            assert!(shown(&page.reclaim, &page));
            win.destroy();
        });
    }

    /// Chips are compact pills at their natural width that wrap inline.
    #[test]
    fn chips_are_natural_width_pills_that_wrap_without_leaving_the_page() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let (page, win) = presented(1280);
            let buttons = chip_buttons(&page);
            assert_eq!(buttons.len(), 3);
            for b in &buttons {
                let natural = b.measure(gtk::Orientation::Horizontal, -1).1;
                assert!(b.width() <= natural, "{:?} is stretched", b.tooltip_text());
            }
            let row = |b: &gtk::Button| b.compute_bounds(page.widget()).unwrap().y().round();
            assert_eq!(
                row(&buttons[0]),
                row(&buttons[2]),
                "three pills share a line at 1280 px"
            );
            win.destroy();

            let (page, win) = presented(420);
            let buttons = chip_buttons(&page);
            let rows: std::collections::BTreeSet<_> = buttons
                .iter()
                .map(|b| b.compute_bounds(page.widget()).unwrap().y().round() as i32)
                .collect();
            assert!(rows.len() >= 2, "420 px wraps the pills: {rows:?}");
            for b in &buttons {
                let r = b.compute_bounds(page.widget()).unwrap();
                assert!(r.x() + r.width() <= 421.0, "a pill leaves the page");
            }
            let pill = buttons[0].height();
            assert!(pill <= 26, "pill is {pill} px tall");
            win.destroy();
        });
    }

    /// The map keeps a floor so the collapsed layout (840 px) does not
    /// squeeze it to a strip.
    #[test]
    fn the_map_keeps_a_floor_height() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let (page, win) = presented(1100);
            let floor = page.map_frame.measure(gtk::Orientation::Vertical, -1).0;
            assert!(floor >= MAP_MIN_H_PX, "map floor is {floor} px");
            win.destroy();
        });
    }

    /// The empty page: "No AI sessions running" centred over the map.
    #[test]
    fn empty_snapshot_shows_the_centred_text_and_a_busy_one_does_not() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let page = MapPage::new();
            let win = gtk::Window::new();
            win.set_default_size(1100, 620);
            win.set_child(Some(page.widget()));
            win.present();
            let text = |page: &MapPage| {
                let mut found = None;
                let mut stack = vec![page.widget().clone().upcast::<gtk::Widget>()];
                while let Some(w) = stack.pop() {
                    if let Some(l) = w.downcast_ref::<gtk::Label>()
                        && l.label() == "No AI sessions running"
                    {
                        found = Some(l.clone());
                    }
                    let mut c = w.first_child();
                    while let Some(x) = c {
                        c = x.next_sibling();
                        stack.push(x);
                    }
                }
                found
            };
            let snap = fixture("vazio");
            page.update(&snap, &Readout::of(&snap));
            settle();
            let label = text(&page).expect("the empty text exists");
            assert!(shown(&label, &page), "visible on the empty snapshot");
            assert_eq!(label.halign(), gtk::Align::Center);
            assert_eq!(label.valign(), gtk::Align::Center);
            let busy = fixture("tipico-40");
            page.update(&busy, &Readout::of(&busy));
            assert!(
                text(&page).is_none_or(|l| !shown(&l, &page)),
                "hidden with sessions"
            );
            win.destroy();
        });
    }

    #[test]
    fn update_on_the_empty_snapshot_hides_chips_and_reclaim() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let page = MapPage::new();
            let win = gtk::Window::new();
            win.set_child(Some(page.widget()));
            win.present();
            let tipico = fixture("tipico-40");
            page.update(&tipico, &Readout::of(&tipico));
            let snap = fixture("vazio");
            page.update(&snap, &Readout::of(&snap));
            assert!(chip_buttons(&page).is_empty());
            assert!(!shown(&page.chips, &page), "no waiting row");
            assert!(!shown(&page.reclaim, &page), "nothing to reclaim");
            assert!(
                shown(&page.map, &page),
                "the map stays under the empty text"
            );
            win.destroy();
        });
    }

    /// A tick's GiB text changes: the chip widgets (and the keyboard focus
    /// on one) stay, only their text moves.
    #[test]
    fn a_new_gib_text_keeps_the_chip_and_its_focus() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let (page, win) = presented(1100);
            let before = chip_buttons(&page);
            assert_eq!(before.len(), 3);
            assert!(before[0].grab_focus());
            let (pid, old, _) = chips(&MapInput::from_snapshot(&fixture("tipico-40")))[0].clone();
            let mut snap = fixture("tipico-40");
            let s = snap.sessions.iter_mut().find(|s| s.pid == pid).unwrap();
            s.pss_bytes = s.pss_bytes.map(|b| b + GIB / 20);
            page.update(&snap, &Readout::of(&snap));
            settle();
            let want = chips(&MapInput::from_snapshot(&snap))[0].clone();
            assert_eq!(want.0, pid, "the order did not change");
            assert_ne!(want.2, old.clone(), "the text did change ({old})");
            let after = chip_buttons(&page);
            assert_eq!(after, before, "same widgets");
            assert!(after[0].has_focus(), "focus survived the tick");
            let mut texts = Vec::new();
            let mut at = descendants_of(after[0].upcast_ref());
            texts.append(&mut at);
            assert!(texts.contains(&want.2), "{texts:?}");
            win.destroy();
        });
    }

    fn descendants_of(w: &gtk::Widget) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            out.push(l.label().to_string());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            out.extend(descendants_of(&c));
            child = c.next_sibling();
        }
        out
    }

    /// The button counts what the Reclaim page lists: a session of unknown
    /// state is idle only when quiet over the full window.
    #[test]
    fn reclaim_button_follows_the_reclaim_rule() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            assert_eq!(load_css(), Vec::<String>::new());
            let page = MapPage::new();
            let win = gtk::Window::new();
            win.set_child(Some(page.widget()));
            win.present();
            let mut one: serde_json::Value = serde_json::to_value(fixture("uma")).unwrap();
            one["interval_ms"] = serde_json::json!(1000);
            one["sessions"][0]["overlay"] = serde_json::Value::Null;
            one["sessions"][0]["cpu_pct"] = serde_json::json!(80.0);
            let snap: Snapshot = serde_json::from_value(one).unwrap();
            page.update(&snap, &Readout::of(&snap));
            assert_eq!(
                MapInput::from_snapshot(&snap).sessions[0].state,
                TileState::Idle,
                "the map still files unknown with idle"
            );
            assert!(
                !shown(&page.reclaim, &page),
                "an unknown, busy-looking session is not reclaimable"
            );
            win.destroy();
        });
    }

    /// The legend's dashed "RAM available" entry went with the column.
    #[test]
    fn the_wide_legend_has_no_ram_available_entry() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let (page, win) = presented(1100);
            let mut found = 0;
            let mut child = page.legend.first_child();
            while let Some(pair) = child {
                let mut c = pair.first_child();
                while let Some(x) = c {
                    if x.downcast_ref::<gtk::Label>()
                        .is_some_and(|l| l.label() == "RAM available")
                    {
                        found += 1;
                    }
                    c = x.next_sibling();
                }
                child = pair.next_sibling();
            }
            assert_eq!(found, 0);
            win.destroy();
        });
    }
}
