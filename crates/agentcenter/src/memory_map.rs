//! Memory map widget (direction B, RFC-AC-001): every session as a tile sized
//! by PSS + swap, grouped by state. Free RAM is not drawn here: the machine
//! strip's RAM cell shows it as a bar ([`crate::machine_strip`]). The layout
//! is [`crate::treemap`]; this file is the input and the drawing.
//!
//! - Input: [`MapInput::from_snapshot`], a pure function of the snapshot.
//! - Each tile is a child widget (CSS `memorymap > tile.<state>`): GTK paints
//!   the fill and the radius from memory_map.css, the tile draws the state
//!   glyph and its labels in its CSS `color` (the on-fill text token), and a
//!   `hatch` child, at `--ac-hatch-opacity`, covers the swapped-out share at
//!   the bottom. Fonts come from hidden label nodes (`label.name`,
//!   `label.value`, ...), so the stylesheet owns them; Pango measures them.
//! - A session tile is a focusable button that speaks "name, 1.09 GiB, busy".
//!   Arrow keys move to the nearest tile ([`crate::treemap::nearest`]);
//!   Enter, space or a click calls [`MemoryMap::connect_activated`] and
//!   selects the tile (a 2 px ink ring, the map's CSS `color`).
//! - Reclaim mode: idle tiles turn teal and toggle a check
//!   ([`MemoryMap::connect_check_toggled`]); the rest dim, unlabelled and out
//!   of the focus chain. Nothing merges there: every idle session is a target.
//! - Stability ([`DriftStability`]): a new snapshot lays out again only when
//!   a tile's share drifts more than 20 % or a session starts or ends, at
//!   most every 30 s, and never while the pointer or the focus is in the map;
//!   otherwise the rects stay and the numbers, hatch and labels update in
//!   place. What waited applies when both leave.

use crate::format::sanitize;
use crate::treemap::{
    self, Dir, DriftStability, Item, LabelFit, Layout, Rect, TileKind, TileState, label_fit,
};
use agentcenter_core::Snapshot;
use agentcenter_core::adapters::AgentRole;
use agentcenter_core::session::Session;
use agentcenter_core::summary::{SessionState, session_state};
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// Half the gutter between tiles, px.
const INSET_PX: f64 = 1.0;
/// Tile corner radius, px.
const RADIUS_PX: f32 = 4.0;
/// Selection ring width, px.
const RING_PX: f32 = 2.0;
/// Hatch period and stripe width, px.
const HATCH_STEP_PX: f32 = 6.0;
const HATCH_LINE_PX: f32 = 2.4;
/// Text rows inside a tile: top of the first and second name line, and the
/// gap from a line's top to the value's top.
const ROW1_Y: f32 = 3.0;
const ROW2_Y: f32 = 19.0;
const VALUE_DY: f32 = 18.0;

/// One session on the map.
#[derive(Debug, Clone, PartialEq)]
pub struct MapSession {
    pub pid: u32,
    pub name: String,
    pub state: TileState,
    pub pss: u64,
    pub swap: u64,
}

impl MapSession {
    /// PSS + swap: the tile's area.
    pub fn bytes(&self) -> u64 {
        self.pss + self.swap
    }
}

/// What the map draws: built from a snapshot by [`MapInput::from_snapshot`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MapInput {
    pub sessions: Vec<MapSession>,
    /// `MemAvailable`; 0 when unknown.
    pub free_bytes: u64,
    /// Clock of the stability rule.
    pub taken_at_ms: u64,
}

fn text(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| sanitize(t).into_owned())
}

/// First words a person types to wake an agent: not a name.
const FILLERS: [&str; 22] = [
    "ok",
    "okay",
    "yes",
    "no",
    "y",
    "n",
    "hi",
    "hey",
    "hello",
    "thanks",
    "thank you",
    "continue",
    "go on",
    "go",
    "proceed",
    "next",
    "test",
    "testing",
    "retry",
    "again",
    "help",
    "please",
];

/// A title worth a tile: not a slash command and not filler such as `ok` or
/// `continue` (what a session's first prompt often is). Fewer than two
/// letters or digits is never a name either.
fn is_name(t: &str) -> bool {
    let t = t.trim();
    let plain = t
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    !t.starts_with('/')
        && t.chars().filter(|c| c.is_alphanumeric()).count() >= 2
        && !FILLERS.contains(&plain.as_str())
}

fn title(v: Option<&str>) -> Option<String> {
    text(v).filter(|t| is_name(t))
}

/// Tile name of a session: the transcript title or summary (the overlay
/// name), else the herdr pane title, else `<cwd basename> - <pane>`. A
/// slash command or a filler first prompt is skipped at every step. A
/// multiplexer is named by its agent; the cwd stays out of it.
pub fn session_name(s: &Session) -> String {
    let mux = s.role == AgentRole::Multiplexer;
    let ov = s.overlay.as_ref();
    let herdr = ov.and_then(|o| o.herdr.as_ref());
    let agent = sanitize(&s.agent).into_owned();
    if mux {
        return title(ov.and_then(|o| o.name.as_deref()))
            .or_else(|| title(herdr.and_then(|h| h.title.as_deref())))
            .unwrap_or_else(|| format!("{agent} multiplexer"));
    }
    title(ov.and_then(|o| o.name.as_deref()))
        .or_else(|| title(herdr.and_then(|h| h.title.as_deref())))
        .or_else(|| {
            let cwd = s.cwd.as_deref()?.trim_end_matches('/');
            let base = text(cwd.rsplit('/').next())?;
            Some(match text(herdr.and_then(|h| h.pane_id.as_deref())) {
                Some(pane) => format!("{base} - {pane}"),
                None => base,
            })
        })
        .unwrap_or(agent)
}

impl MapInput {
    /// Agents and multiplexers of `snap`. State from the core's
    /// [`session_state`]; an unknown state sits with idle (no status
    /// published, the same fallback the reclaim list starts from). Name:
    /// [`session_name`]. Absent
    /// PSS or swap counts 0 for the area.
    pub fn from_snapshot(snap: &Snapshot) -> Self {
        let sessions = snap
            .sessions
            .iter()
            .map(|s| {
                let state = match (s.role, session_state(s)) {
                    (AgentRole::Multiplexer, _) => TileState::Multiplexer,
                    (AgentRole::Agent, SessionState::Blocked) => TileState::Waiting,
                    (AgentRole::Agent, SessionState::Busy) => TileState::Busy,
                    (AgentRole::Agent, SessionState::Idle | SessionState::Unknown) => {
                        TileState::Idle
                    }
                };
                let name = session_name(s);
                MapSession {
                    pid: s.pid,
                    name,
                    state,
                    pss: s.pss_bytes.unwrap_or(0),
                    swap: s.swap_pss_bytes.unwrap_or(0),
                }
            })
            .collect();
        Self {
            sessions,
            free_bytes: snap.machine.mem_available_bytes.unwrap_or(0),
            taken_at_ms: snap.taken_at_ms,
        }
    }

    /// Pid and bytes of every session: what the stability rule watches.
    pub fn weights(&self) -> Vec<(u32, f64)> {
        self.sessions
            .iter()
            .map(|s| (s.pid, s.bytes() as f64))
            .collect()
    }

    /// Layout items: pid, state and weight in bytes.
    pub fn items(&self) -> Vec<Item> {
        self.sessions
            .iter()
            .map(|s| Item {
                id: s.pid,
                state: s.state,
                weight: s.bytes() as f64,
            })
            .collect()
    }

    fn by_pid(&self) -> BTreeMap<u32, &MapSession> {
        self.sessions.iter().map(|s| (s.pid, s)).collect()
    }
}

/// `1.09 GiB`: two decimals, the tile value.
pub fn gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB)
}

/// `8.7 GiB`: one decimal, the free RAM readout.
pub fn gib1(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB)
}

/// Accessible label of a session tile: `name, 1.09 GiB, busy`.
pub fn spoken(name: &str, bytes: u64, state: TileState) -> String {
    format!("{name}, {}, {}", gib(bytes), state.word())
}

/// Swapped-out share of `pss + swap`, 0 when both are 0.
fn swap_share(pss: u64, swap: u64) -> f64 {
    let total = pss + swap;
    if total == 0 {
        0.0
    } else {
        swap as f64 / total as f64
    }
}

// ---- Tile -------------------------------------------------------------------

/// What a tile stands for. The default is an empty merge: no pid.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Session(u32),
    Merged(Vec<u32>),
}

impl Default for Kind {
    fn default() -> Self {
        Kind::Merged(Vec::new())
    }
}

/// State glyph, drawn in the tile's text colour.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) enum Glyph {
    #[default]
    None,
    Disc,
    Triangle,
    Ring,
    Grid,
    Check,
}

impl Glyph {
    pub(crate) fn of(state: TileState) -> Self {
        match state {
            TileState::Busy => Glyph::Disc,
            TileState::Waiting => Glyph::Triangle,
            TileState::Idle => Glyph::Ring,
            TileState::Multiplexer => Glyph::Grid,
        }
    }
}

/// Everything a tile draws; set by the map, read by the tile's snapshot.
#[derive(Debug, Clone, Default)]
struct Shown {
    kind: Kind,
    name: String,
    value: String,
    swap_share: f64,
    glyph: Glyph,
    /// Dimmed context tile on the Reclaim page: fill only.
    dim: bool,
}

mod tile_imp {
    use super::*;

    #[derive(Default)]
    pub struct MapTile {
        pub(super) shown: RefCell<Shown>,
        /// The accessible label last announced.
        pub(super) spoken: RefCell<String>,
        pub hatch: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MapTile {
        const NAME: &'static str = "AcMapTile";
        type Type = super::MapTile;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("tile");
        }
    }

    impl ObjectImpl for MapTile {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            let hatch: gtk::Widget = glib::Object::new::<super::Hatch>().upcast();
            hatch.set_parent(&*obj);
            self.hatch.replace(Some(hatch));
        }

        fn dispose(&self) {
            if let Some(h) = self.hatch.take() {
                h.unparent();
            }
        }
    }

    impl WidgetImpl for MapTile {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let shown = self.shown.borrow();
            let share = if shown.dim { 0.0 } else { shown.swap_share };
            let hh = (f64::from(height) * share).round() as i32;
            if let Some(h) = self.hatch.borrow().as_ref() {
                h.measure(gtk::Orientation::Horizontal, -1);
                h.size_allocate(&gtk::Allocation::new(0, height - hh, width, hh), -1);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let obj = self.obj();
            let Some(map) = obj.parent().and_downcast::<super::MemoryMap>() else {
                return;
            };
            let shown = self.shown.borrow();
            let (w, h) = (f64::from(obj.width()), f64::from(obj.height()));
            let color = obj.color();
            match &shown.kind {
                _ if shown.dim => {}
                _ => super::draw_labels(snapshot, &map, &shown, w, h, &color),
            }
        }
    }
}

glib::wrapper! {
    /// One tile of the map. CSS: `memorymap > tile.<state>`, plus `.merged`,
    /// `.reclaim`, `.dim`, `.checked`, `.selected`.
    pub struct MapTile(ObjectSubclass<tile_imp::MapTile>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MapTile {
    fn new(role: gtk::AccessibleRole) -> Self {
        glib::Object::builder()
            .property("accessible-role", role)
            .build()
    }

    fn kind(&self) -> Kind {
        self.imp().shown.borrow().kind.clone()
    }

    /// Replaces what the tile shows; repaints, and re-allocates the hatch
    /// only when its share moved.
    fn show(&self, next: Shown, label: &str) {
        let prev = self.imp().shown.replace(next);
        let now = self.imp().shown.borrow();
        // The label carries the state, which `Shown` does not: compare it.
        if *self.imp().spoken.borrow() != label {
            self.imp().spoken.replace(label.to_owned());
            self.update_property(&[gtk::accessible::Property::Label(label)]);
        }
        if prev.swap_share != now.swap_share || prev.dim != now.dim {
            self.queue_allocate();
        }
        self.queue_draw();
    }
}

mod hatch_imp {
    use super::*;

    #[derive(Default)]
    pub struct Hatch;

    #[glib::object_subclass]
    impl ObjectSubclass for Hatch {
        const NAME: &'static str = "AcMapHatch";
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
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        /// Diagonal stripes ("/") in the node's colour; the CSS opacity
        /// (`--ac-hatch-opacity`) makes them a hatch over the fill.
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let path = gsk::PathBuilder::new();
            let mut x = -h;
            while x < w + h {
                path.move_to(x, h);
                path.line_to(x + h, 0.0);
                x += HATCH_STEP_PX;
            }
            let stroke = gsk::Stroke::new(HATCH_LINE_PX);
            snapshot.append_stroke(&path.to_path(), &stroke, &obj.color());
        }
    }
}

glib::wrapper! {
    /// Swapped-out share of a tile: `tile > hatch`.
    pub struct Hatch(ObjectSubclass<hatch_imp::Hatch>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn layout_of(carrier: &gtk::Label, text: &str) -> pango::Layout {
    carrier.create_pango_layout(Some(text))
}

fn width_of(layout: &pango::Layout) -> f64 {
    f64::from(layout.pixel_size().0)
}

fn put(snapshot: &gtk::Snapshot, layout: &pango::Layout, x: f32, y: f32, color: &gdk::RGBA) {
    snapshot.save();
    snapshot.translate(&graphene::Point::new(x, y));
    snapshot.append_layout(layout, color);
    snapshot.restore();
}

/// Glyph, name and value of a session or merged tile, by [`label_fit`].
fn draw_labels(
    snapshot: &gtk::Snapshot,
    map: &MemoryMap,
    shown: &Shown,
    w: f64,
    h: f64,
    color: &gdk::RGBA,
) {
    let fonts = map.imp().fonts.borrow();
    let Some([small, big, value_font]) = fonts.as_ref().map(|f| f.each()) else {
        return;
    };
    // 12 px bold below 96 px tiles, 13 px from there.
    let name_font = if treemap::name_px(w) >= 13.0 {
        big
    } else {
        small
    };
    let value = layout_of(value_font, &shown.value);
    let fit = label_fit(&shown.name, w, h, width_of(&value), |s| {
        width_of(&layout_of(name_font, s))
    });
    let pad = treemap::TILE_PAD_PX as f32;
    let at = match fit {
        LabelFit::Nothing => return,
        LabelFit::Glyph => ((w - treemap::GLYPH_PX) as f32 / 2.0).min(pad),
        _ => pad,
    };
    draw_glyph(snapshot, shown.glyph, at, pad, color);
    let line = |text: &str, x: f32, y: f32, room: f64| {
        let l = layout_of(name_font, text);
        l.set_width((room.max(0.0) * f64::from(pango::SCALE)) as i32);
        l.set_ellipsize(pango::EllipsizeMode::End);
        put(snapshot, &l, x, y, color);
    };
    let x1 = (treemap::LINE1_INSET_PX - treemap::TILE_PAD_PX) as f32;
    let value_y = match fit {
        LabelFit::Nothing | LabelFit::Glyph => return,
        LabelFit::Inline => {
            line(&shown.name, x1, ROW1_Y, w - treemap::LINE1_INSET_PX);
            let after = x1 + width_of(&layout_of(name_font, &shown.name)) as f32;
            put(
                snapshot,
                &value,
                after + treemap::INLINE_GAP_PX as f32,
                ROW1_Y,
                color,
            );
            return;
        }
        LabelFit::GlyphValue => ROW1_Y + VALUE_DY,
        LabelFit::Stacked { value: with_value } => {
            // Glyph row, then the name across the whole tile.
            let name_y = ROW1_Y + VALUE_DY;
            line(&shown.name, pad, name_y, w - treemap::LINE2_INSET_PX);
            if !with_value {
                return;
            }
            name_y + VALUE_DY
        }
        LabelFit::Name => {
            line(&shown.name, x1, ROW1_Y, w - treemap::LINE1_INSET_PX);
            ROW1_Y + VALUE_DY
        }
        LabelFit::Wrapped(split) => {
            line(
                &shown.name[..split],
                x1,
                ROW1_Y,
                w - treemap::LINE1_INSET_PX,
            );
            line(
                &shown.name[split..],
                pad,
                ROW2_Y,
                w - treemap::LINE2_INSET_PX,
            );
            ROW2_Y + VALUE_DY
        }
    };
    put(snapshot, &value, pad, value_y, color);
    // A merged tile: one small outlined square per session, while they fit.
    if let Kind::Merged(ids) = &shown.kind {
        let top = value_y + VALUE_DY + 4.0;
        let per_row = (((w - 16.0) / 12.0).floor() as usize).max(1);
        let rows = ((h as f32 - top - 4.0) / 12.0).floor().max(0.0) as usize;
        let stroke = gsk::Stroke::new(1.2);
        for i in 0..ids.len().min(per_row * rows) {
            let x = pad + (i % per_row) as f32 * 12.0;
            let y = top + (i / per_row) as f32 * 12.0;
            let p = gsk::PathBuilder::new();
            p.add_rounded_rect(&gsk::RoundedRect::from_rect(
                graphene::Rect::new(x, y, 8.0, 8.0),
                1.5,
            ));
            snapshot.append_stroke(&p.to_path(), &stroke, color);
        }
    }
}

/// A state glyph of [`treemap::GLYPH_PX`] with its top-left at `(x, y)`.
pub(crate) fn draw_glyph(
    snapshot: &gtk::Snapshot,
    glyph: Glyph,
    x: f32,
    y: f32,
    color: &gdk::RGBA,
) {
    let s = treemap::GLYPH_PX as f32;
    let (cx, cy, r) = (x + s / 2.0, y + s / 2.0, s / 2.0);
    let p = gsk::PathBuilder::new();
    match glyph {
        Glyph::None => return,
        Glyph::Disc => p.add_circle(&graphene::Point::new(cx, cy), r * 0.8),
        Glyph::Triangle => {
            p.move_to(cx, y + 0.5);
            p.line_to(x + s, y + s - 0.5);
            p.line_to(x, y + s - 0.5);
            p.close();
        }
        Glyph::Grid => {
            let q = s * 0.42;
            for (dx, dy) in [(0.0, 0.0), (s - q, 0.0), (0.0, s - q), (s - q, s - q)] {
                p.add_rect(&graphene::Rect::new(x + dx, y + dy, q, q));
            }
        }
        Glyph::Ring | Glyph::Check => {
            if glyph == Glyph::Ring {
                p.add_circle(&graphene::Point::new(cx, cy), r * 0.8 - 1.0);
            } else {
                p.move_to(x + s * 0.15, y + s * 0.55);
                p.line_to(x + s * 0.42, y + s * 0.8);
                p.line_to(x + s * 0.88, y + s * 0.25);
            }
            let stroke = gsk::Stroke::new(if glyph == Glyph::Ring { 2.0 } else { 2.4 });
            stroke.set_line_cap(gsk::LineCap::Round);
            stroke.set_line_join(gsk::LineJoin::Round);
            snapshot.append_stroke(&p.to_path(), &stroke, color);
            return;
        }
    }
    snapshot.append_fill(&p.to_path(), gsk::FillRule::Winding, color);
}

// ---- Map --------------------------------------------------------------------

/// Hidden label nodes that carry the fonts from memory_map.css.
struct Fonts {
    name: gtk::Label,
    name_big: gtk::Label,
    value: gtk::Label,
}

impl Fonts {
    fn each(&self) -> [&gtk::Label; 3] {
        [&self.name, &self.name_big, &self.value]
    }
}

type Activated = Rc<dyn Fn(u32)>;
type Toggled = Rc<dyn Fn(u32, bool)>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MemoryMap {
        pub input: RefCell<MapInput>,
        pub layout: RefCell<Layout>,
        /// Size the layout was made for.
        pub laid: Cell<(i32, i32)>,
        pub relayout_queued: Cell<bool>,
        pub stability: RefCell<DriftStability>,
        /// Pointer or focus in the map: the layout stays put.
        pub hovered: Cell<bool>,
        pub has_focus: Cell<bool>,
        /// Session tiles by pid; kept across layouts so focus survives.
        pub sessions: RefCell<BTreeMap<u32, super::MapTile>>,
        pub merged: RefCell<Vec<super::MapTile>>,
        pub(super) fonts: RefCell<Option<Fonts>>,
        pub reclaim: Cell<bool>,
        pub checked: RefCell<BTreeSet<u32>>,
        /// Reclaim targets set by the page; `None`: every idle tile.
        pub targets: RefCell<Option<BTreeSet<u32>>>,
        pub selected: Cell<Option<u32>>,
        pub on_activated: RefCell<Vec<Activated>>,
        pub on_toggled: RefCell<Vec<Toggled>>,
        pub motion: Rc<crate::motion::TileMotion>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MemoryMap {
        const NAME: &'static str = "AcMemoryMap";
        type Type = super::MemoryMap;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("memorymap");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }
    }

    impl ObjectImpl for MemoryMap {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            let carrier = |class: &str| {
                let l = gtk::Label::new(None);
                l.add_css_class(class);
                l.set_visible(false);
                l.set_parent(&*obj);
                l
            };
            self.fonts.replace(Some(Fonts {
                name: carrier("name"),
                name_big: carrier("name-big"),
                value: carrier("value"),
            }));

            let keys = gtk::EventControllerKey::new();
            let weak = obj.downgrade();
            keys.connect_key_pressed(move |_, key, _, _| {
                let handled = weak.upgrade().is_some_and(|m| m.on_key(key));
                if handled {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            obj.add_controller(keys);

            // The layout never moves under the pointer or the focus; what
            // waited applies when both leave.
            let motion = gtk::EventControllerMotion::new();
            let weak = obj.downgrade();
            motion.connect_enter(move |_, _, _| {
                if let Some(m) = weak.upgrade() {
                    m.imp().hovered.set(true);
                }
            });
            let weak = obj.downgrade();
            motion.connect_leave(move |_| {
                if let Some(m) = weak.upgrade() {
                    m.imp().hovered.set(false);
                    m.apply();
                }
            });
            obj.add_controller(motion);
            let focus = gtk::EventControllerFocus::new();
            let weak = obj.downgrade();
            focus.connect_contains_focus_notify(move |c| {
                if let Some(m) = weak.upgrade() {
                    m.imp().has_focus.set(c.contains_focus());
                    m.apply();
                }
            });
            obj.add_controller(focus);
        }

        fn dispose(&self) {
            self.motion.dispose();
            for t in std::mem::take(&mut *self.sessions.borrow_mut()).into_values() {
                t.unparent();
            }
            for t in self.merged.take() {
                t.unparent();
            }
            if let Some(f) = self.fonts.take() {
                for l in f.each() {
                    l.unparent();
                }
            }
        }
    }

    impl WidgetImpl for MemoryMap {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let obj = self.obj();
            let (lw, lh) = self.laid.get();
            if (lw, lh) != (width, height) && !self.relayout_queued.replace(true) {
                // The tile set depends on the size (the merge rule is in
                // px²): lay out again after this frame, outside allocation.
                let weak = obj.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(m) = weak.upgrade() {
                        m.imp().relayout_queued.set(false);
                        m.imp().stability.borrow_mut().invalidate();
                        m.relayout();
                    }
                });
            }
            // Until then, the last layout stretched to the new size.
            let sx = if lw > 0 {
                f64::from(width) / f64::from(lw)
            } else {
                0.0
            };
            let sy = if lh > 0 {
                f64::from(height) / f64::from(lh)
            } else {
                0.0
            };
            let obj = self.obj();
            let map: &gtk::Widget = obj.upcast_ref();
            let alloc = |t: &super::MapTile, r: Rect| {
                let r = Rect::new(r.x * sx, r.y * sy, r.w * sx, r.h * sy);
                let (x, y, w, h) = r.snap();
                let i = INSET_PX as i32;
                // Where the tile belongs; the spring says where it is now.
                let to = Rect::new(
                    f64::from(x + i),
                    f64::from(y + i),
                    f64::from((w - 2 * i).max(0)),
                    f64::from((h - 2 * i).max(0)),
                );
                crate::motion::allocate(t, self.motion.place(map, t, to));
            };
            let layout = self.layout.borrow();
            let sessions = self.sessions.borrow();
            let mut merged = self.merged.borrow().clone().into_iter();
            for tile in &layout.tiles {
                match &tile.kind {
                    TileKind::Session(pid) => {
                        if let Some(t) = sessions.get(pid) {
                            alloc(t, tile.rect);
                        }
                    }
                    TileKind::Merged(_) => {
                        if let Some(t) = merged.next() {
                            alloc(&t, tile.rect);
                        }
                    }
                }
            }
            for (t, now) in self.motion.leaving() {
                crate::motion::allocate(&t, now);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let obj = self.obj();
            if self.reclaim.get() {
                return;
            }
            let Some(pid) = self.selected.get() else {
                return;
            };
            let sessions = self.sessions.borrow();
            let Some(tile) = sessions.get(&pid).filter(|t| t.is_visible()) else {
                return;
            };
            let Some(b) = tile.compute_bounds(&*obj) else {
                return;
            };
            let ring = gsk::RoundedRect::from_rect(
                graphene::Rect::new(
                    b.x() - INSET_PX as f32,
                    b.y() - INSET_PX as f32,
                    b.width() + 2.0 * INSET_PX as f32,
                    b.height() + 2.0 * INSET_PX as f32,
                ),
                RADIUS_PX + 1.0,
            );
            let c = obj.color();
            snapshot.append_border(&ring, &[RING_PX; 4], &[c, c, c, c]);
        }
    }
}

glib::wrapper! {
    /// The memory map. CSS: `memorymap` (its `color` is the selection ring),
    /// `memorymap > tile`, `tile > hatch`, and the font carriers
    /// `memorymap > label.name|name-big|value`.
    pub struct MemoryMap(ObjectSubclass<imp::MemoryMap>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MemoryMap {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMap {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// New data. Lays out again when the stability rule says so and nothing
    /// holds the map still ([`DriftStability`]); otherwise only the numbers
    /// move.
    pub fn set_input(&self, input: MapInput) {
        self.imp().input.replace(input);
        self.apply();
    }

    /// Pointer or focus in the map, after a first layout: no relayout.
    /// A spring is moving.
    #[cfg(test)]
    fn motion_active(&self) -> bool {
        self.imp().motion.active()
    }

    /// A tick callback is registered.
    #[cfg(test)]
    fn motion_ticking(&self) -> bool {
        self.imp().motion.ticking()
    }

    fn held(&self) -> bool {
        let imp = self.imp();
        (imp.hovered.get() || imp.has_focus.get()) && imp.laid.get().0 > 0
    }

    /// A waiting session sits in a merged tile: it needs a tile of its own
    /// at once, the stability rule does not apply (a session that asks for
    /// the person is never hidden for 30 s).
    fn buried_waiting(&self) -> bool {
        let imp = self.imp();
        let input = imp.input.borrow();
        let waiting: BTreeSet<u32> = input
            .sessions
            .iter()
            .filter(|s| s.state == TileState::Waiting)
            .map(|s| s.pid)
            .collect();
        imp.layout.borrow().tiles.iter().any(|t| match &t.kind {
            TileKind::Merged(ids) => ids.iter().any(|id| waiting.contains(id)),
            _ => false,
        })
    }

    /// Takes the current input: relayout when the rule allows and nothing
    /// holds the map, otherwise the numbers move in place. The held layout
    /// keeps its due state, so leaving applies what waited.
    fn apply(&self) {
        let imp = self.imp();
        if self.held() {
            self.refresh();
            return;
        }
        if self.buried_waiting() {
            imp.stability.borrow_mut().invalidate();
        }
        let (weights, now) = {
            let input = imp.input.borrow();
            (input.weights(), input.taken_at_ms)
        };
        if imp.stability.borrow_mut().should_relayout(&weights, now) {
            self.relayout();
        } else {
            self.refresh();
        }
    }

    /// Reclaim mode on or off: idle tiles become checkable targets.
    pub fn set_reclaim(&self, on: bool) {
        let imp = self.imp();
        if imp.reclaim.replace(on) != on {
            imp.stability.borrow_mut().invalidate();
            self.relayout();
        }
    }

    /// The sessions the Reclaim page may end (the core's reclaim rule),
    /// which can differ from the idle tiles: an unknown state counts only
    /// when quiet, and the caller's own session never counts. `None` makes
    /// every idle tile a target.
    pub fn set_targets(&self, pids: Option<&[u32]>) {
        self.imp()
            .targets
            .replace(pids.map(|p| p.iter().copied().collect()));
        self.refresh();
    }

    /// The checked sessions on the Reclaim page.
    pub fn set_checked(&self, pids: &[u32]) {
        self.imp().checked.replace(pids.iter().copied().collect());
        self.refresh();
    }

    pub fn checked(&self) -> Vec<u32> {
        self.imp().checked.borrow().iter().copied().collect()
    }

    /// The session with the selection ring (the one the detail shows).
    pub fn set_selected(&self, pid: Option<u32>) {
        self.imp().selected.set(pid);
        self.refresh();
    }

    pub fn selected(&self) -> Option<u32> {
        self.imp().selected.get()
    }

    /// Enter, space or click on a session tile (outside reclaim mode).
    pub fn connect_activated(&self, f: impl Fn(u32) + 'static) {
        self.imp().on_activated.borrow_mut().push(Rc::new(f));
    }

    /// A check toggled on the Reclaim page: `(pid, checked)`.
    pub fn connect_check_toggled(&self, f: impl Fn(u32, bool) + 'static) {
        self.imp().on_toggled.borrow_mut().push(Rc::new(f));
    }

    /// Visible tiles, in layout order.
    pub fn tiles(&self) -> Vec<gtk::Widget> {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        let sessions = imp.sessions.borrow();
        let mut merged = imp.merged.borrow().clone().into_iter();
        layout
            .tiles
            .iter()
            .filter_map(|t| match &t.kind {
                TileKind::Session(pid) => sessions.get(pid).map(|w| w.clone().upcast()),
                TileKind::Merged(_) => merged.next().map(|w| w.upcast()),
            })
            .collect()
    }

    /// The pid a tile stands for; `None` for a merged tile.
    pub fn tile_pid(tile: &gtk::Widget) -> Option<u32> {
        match tile.downcast_ref::<MapTile>()?.kind() {
            Kind::Session(pid) => Some(pid),
            _ => None,
        }
    }

    /// A new layout for the current size, mode and input; tiles follow it.
    fn relayout(&self) {
        let imp = self.imp();
        let (w, h) = (self.width(), self.height());
        imp.laid.set((w, h));
        let input = imp.input.borrow().clone();
        imp.stability
            .borrow_mut()
            .rebase(&input.weights(), input.taken_at_ms);
        let bounds = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        let layout = treemap::layout(&input.items(), bounds, !imp.reclaim.get());

        // Session tiles: one per pid, kept while the pid lives.
        let live: BTreeSet<u32> = input.sessions.iter().map(|s| s.pid).collect();
        {
            let mut sessions = imp.sessions.borrow_mut();
            sessions.retain(|pid, t| {
                let keep = live.contains(pid);
                if !keep {
                    imp.motion.retire(self.upcast_ref(), t);
                }
                keep
            });
            for pid in &live {
                sessions
                    .entry(*pid)
                    .or_insert_with(|| self.new_session_tile());
            }
        }
        let n_merged = layout
            .tiles
            .iter()
            .filter(|t| matches!(t.kind, TileKind::Merged(_)))
            .count();
        {
            let mut merged = imp.merged.borrow_mut();
            while merged.len() > n_merged {
                if let Some(t) = merged.pop() {
                    imp.motion.retire(self.upcast_ref(), &t);
                }
            }
            while merged.len() < n_merged {
                let t = self.new_merged_tile();
                merged.push(t);
            }
        }
        imp.layout.replace(layout);
        self.refresh();
        self.queue_allocate();
    }

    /// A merged "N small" tile: focusable and clickable like the others;
    /// activating it opens its largest session.
    fn new_merged_tile(&self) -> MapTile {
        let t = MapTile::new(gtk::AccessibleRole::Button);
        t.add_css_class("merged");
        t.set_parent(self);
        self.connect_click(&t);
        t
    }

    fn new_session_tile(&self) -> MapTile {
        let t = MapTile::new(gtk::AccessibleRole::Button);
        t.set_parent(self);
        self.connect_click(&t);
        t
    }

    fn connect_click(&self, t: &MapTile) {
        let click = gtk::GestureClick::new();
        let (weak_map, weak_tile) = (self.downgrade(), t.downgrade());
        click.connect_released(move |_, _, _, _| {
            if let (Some(m), Some(t)) = (weak_map.upgrade(), weak_tile.upgrade()) {
                t.grab_focus();
                m.activate_tile(&t);
            }
        });
        t.add_controller(click);
    }

    /// Numbers, classes, labels and visibility of every tile from the
    /// current input and the current layout's rects.
    fn refresh(&self) {
        let imp = self.imp();
        let input = imp.input.borrow();
        let by_pid = input.by_pid();
        let layout = imp.layout.borrow();
        let reclaim = imp.reclaim.get();
        let checked = imp.checked.borrow();
        let selected = imp.selected.get();
        let sessions = imp.sessions.borrow();
        let mut shown_pids = BTreeSet::new();
        let mut merged = imp.merged.borrow().clone().into_iter();

        let targets = imp.targets.borrow();
        for tile in &layout.tiles {
            // The rects wait for the stability rule; the state is always
            // the live one, so a tile never says "busy" for a waiting session.
            let state = match &tile.kind {
                TileKind::Session(pid) => by_pid.get(pid).map_or(tile.state, |s| s.state),
                _ => tile.state,
            };
            let target = reclaim
                && match (&*targets, &tile.kind) {
                    (Some(t), TileKind::Session(pid)) => t.contains(pid),
                    (Some(_), TileKind::Merged(_)) => false,
                    (None, _) => state == TileState::Idle,
                };
            let dim = reclaim && !target;
            let (widget, shown, label) = match &tile.kind {
                TileKind::Session(pid) => {
                    let (Some(w), Some(s)) = (sessions.get(pid), by_pid.get(pid)) else {
                        continue;
                    };
                    shown_pids.insert(*pid);
                    let is_checked = target && checked.contains(pid);
                    let glyph = if is_checked {
                        Glyph::Check
                    } else {
                        Glyph::of(state)
                    };
                    w.set_focusable(!dim);
                    set_class(w, "checked", is_checked);
                    set_class(w, "selected", !reclaim && selected == Some(*pid));
                    let shown = Shown {
                        kind: Kind::Session(*pid),
                        name: s.name.clone(),
                        value: gib(s.bytes()),
                        swap_share: swap_share(s.pss, s.swap),
                        glyph,
                        dim,
                    };
                    (w.clone(), shown, spoken(&s.name, s.bytes(), state))
                }
                TileKind::Merged(ids) => {
                    let Some(w) = merged.next() else { continue };
                    w.set_focusable(!reclaim);
                    let members: Vec<&&MapSession> =
                        ids.iter().filter_map(|id| by_pid.get(id)).collect();
                    let pss: u64 = members.iter().map(|s| s.pss).sum();
                    let swap: u64 = members.iter().map(|s| s.swap).sum();
                    let name = format!("{} small", ids.len());
                    let label = format!(
                        "{} small {} sessions, {}",
                        ids.len(),
                        state.word(),
                        gib(pss + swap)
                    );
                    let shown = Shown {
                        kind: Kind::Merged(ids.clone()),
                        name,
                        value: gib(pss + swap),
                        swap_share: swap_share(pss, swap),
                        glyph: Glyph::of(state),
                        dim,
                    };
                    (w, shown, label)
                }
            };
            for st in TileState::ORDER {
                set_class(&widget, st.word(), st == state);
            }
            set_class(&widget, "reclaim", target);
            set_class(&widget, "dim", dim);
            widget.set_visible(true);
            widget
                .downcast_ref::<MapTile>()
                .expect("tile")
                .show(shown, &label);
        }
        for (pid, w) in sessions.iter() {
            if !shown_pids.contains(pid) {
                w.set_visible(false);
                w.set_focusable(false);
            }
        }
        self.queue_draw();
    }

    /// Focusable session tiles and their bounds in the map.
    fn focus_targets(&self) -> Vec<(MapTile, Rect)> {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        let sessions = imp.sessions.borrow();
        let mut merged = imp.merged.borrow().clone().into_iter();
        layout
            .tiles
            .iter()
            .filter_map(|t| match t.kind {
                TileKind::Session(pid) => sessions.get(&pid).cloned(),
                TileKind::Merged(_) => merged.next(),
            })
            .filter(|t| t.is_focusable() && t.is_visible())
            .filter_map(|t| {
                let b = t.compute_bounds(self)?;
                let r = Rect::new(
                    b.x().into(),
                    b.y().into(),
                    b.width().into(),
                    b.height().into(),
                );
                Some((t, r))
            })
            .collect()
    }

    fn on_key(&self, key: gdk::Key) -> bool {
        let dir = match key {
            gdk::Key::Left | gdk::Key::KP_Left => Some(Dir::Left),
            gdk::Key::Right | gdk::Key::KP_Right => Some(Dir::Right),
            gdk::Key::Up | gdk::Key::KP_Up => Some(Dir::Up),
            gdk::Key::Down | gdk::Key::KP_Down => Some(Dir::Down),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter | gdk::Key::space => None,
            _ => return false,
        };
        let Some(focused) = self.focus_child().and_downcast::<MapTile>() else {
            return false;
        };
        let Some(dir) = dir else {
            return self.activate_tile(&focused);
        };
        let targets = self.focus_targets();
        let Some(from) = targets.iter().position(|(t, _)| *t == focused) else {
            return false;
        };
        let rects: Vec<Rect> = targets.iter().map(|(_, r)| *r).collect();
        match treemap::nearest(&rects, from, dir) {
            Some(i) => targets[i].0.grab_focus(),
            None => false,
        }
    }

    /// Enter or click on `tile`: open it, or on the Reclaim page toggle its
    /// check. Returns whether it acted.
    fn activate_tile(&self, tile: &MapTile) -> bool {
        let imp = self.imp();
        let pid = match tile.kind() {
            Kind::Session(pid) => pid,
            Kind::Merged(ids) if !imp.reclaim.get() => {
                let input = imp.input.borrow();
                let by_pid = input.by_pid();
                let Some(largest) = ids
                    .iter()
                    .filter_map(|id| by_pid.get(id))
                    .max_by_key(|s| (s.bytes(), std::cmp::Reverse(s.pid)))
                    .map(|s| s.pid)
                else {
                    return false;
                };
                largest
            }
            _ => return false,
        };
        if imp.reclaim.get() {
            if !tile.has_css_class("reclaim") {
                return false;
            }
            let on = {
                let mut checked = imp.checked.borrow_mut();
                if checked.remove(&pid) {
                    false
                } else {
                    checked.insert(pid);
                    true
                }
            };
            self.refresh();
            let callbacks = imp.on_toggled.borrow().clone();
            for f in callbacks {
                f(pid, on);
            }
        } else {
            self.set_selected(Some(pid));
            let callbacks = imp.on_activated.borrow().clone();
            for f in callbacks {
                f(pid);
            }
        }
        true
    }
}

fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::treemap::{Dir, Rect, nearest};
    use std::cell::RefCell;
    use std::rc::Rc;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("parse")
    }

    #[test]
    fn from_snapshot_on_tipico_40() {
        let snap = fixture("tipico-40.json");
        let m = MapInput::from_snapshot(&snap);
        assert_eq!(m.sessions.len(), 40, "38 agents and 2 multiplexers");
        assert_eq!(m.free_bytes, snap.machine.mem_available_bytes.unwrap());
        assert_eq!(m.taken_at_ms, snap.taken_at_ms);
        let count = |s: TileState| m.sessions.iter().filter(|x| x.state == s).count();
        assert_eq!(
            [
                TileState::Waiting,
                TileState::Busy,
                TileState::Idle,
                TileState::Multiplexer
            ]
            .map(count),
            [3, 9, 26, 2]
        );
        let kiln = m.sessions.iter().find(|s| s.pid == 20680).expect("kiln");
        assert_eq!(kiln.name, "kiln-cache-layer", "overlay name");
        assert_eq!(kiln.state, TileState::Idle);
        let src = snap.sessions.iter().find(|s| s.pid == 20680).unwrap();
        assert_eq!(kiln.pss, src.pss_bytes.unwrap());
        assert_eq!(kiln.swap, src.swap_pss_bytes.unwrap());
        let mux = m.sessions.iter().find(|s| s.pid == 21520).expect("mux");
        assert_eq!(mux.state, TileState::Multiplexer);
        assert_eq!(
            mux.name, "herdr multiplexer",
            "no overlay name: the agent, not the cwd"
        );
        let blocked = m.sessions.iter().find(|s| s.pid == 20120).expect("blocked");
        assert_eq!(blocked.state, TileState::Waiting);
        let items = m.items();
        assert_eq!(items.len(), 40);
        assert!(items.iter().all(|i| i.weight > 0.0));
        assert_eq!(
            items.iter().find(|i| i.id == 20680).unwrap().weight,
            kiln.bytes() as f64
        );
    }

    #[test]
    fn from_snapshot_on_vazio_and_name_fallbacks() {
        let m = MapInput::from_snapshot(&fixture("vazio.json"));
        assert!(m.sessions.is_empty());
        assert!(m.items().is_empty());

        let mut snap = fixture("uma.json");
        let s = &mut snap.sessions[0];
        s.overlay = None;
        s.cwd = Some("/home/dev/src/kiln/".into());
        s.pss_bytes = None;
        s.swap_pss_bytes = Some(GIB);
        let m = MapInput::from_snapshot(&snap);
        assert_eq!(m.sessions[0].name, "kiln", "cwd basename");
        assert_eq!(m.sessions[0].pss, 0, "absent counts 0 for the area");
        assert_eq!(m.sessions[0].bytes(), GIB);
        assert_eq!(
            m.sessions[0].state,
            TileState::Idle,
            "unknown state sits with idle"
        );
    }

    #[test]
    fn values_and_spoken_labels() {
        assert_eq!(gib(GIB + GIB / 10), "1.10 GiB");
        assert_eq!(gib(GIB * 54 / 100 + 1), "0.54 GiB");
        assert_eq!(gib1(9_341_553_868), "8.7 GiB");
        assert_eq!(gib1(0), "0.0 GiB");
        assert_eq!(
            spoken("kiln-cache-layer", GIB * 109 / 100, TileState::Busy),
            "kiln-cache-layer, 1.09 GiB, busy"
        );
        assert_eq!(
            spoken("herdr", GIB, TileState::Waiting),
            "herdr, 1.00 GiB, waiting"
        );
    }

    // ---- GTK ----------------------------------------------------------------

    fn settle() {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
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
        ));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
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
        (!msg.is_null()).then(|| unsafe { glib::GString::from_glib_full(msg) }.to_string())
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

    fn bounds(map: &MemoryMap, w: &gtk::Widget) -> Rect {
        let b = w.compute_bounds(map).expect("allocated");
        Rect::new(
            b.x().into(),
            b.y().into(),
            b.width().into(),
            b.height().into(),
        )
    }

    #[test]
    fn tiles_speak_traverse_with_arrows_and_activate() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let input = MapInput::from_snapshot(&fixture("tipico-40.json"));
            let map = MemoryMap::new();
            let activated = Rc::new(RefCell::new(Vec::new()));
            let a = activated.clone();
            map.connect_activated(move |pid| a.borrow_mut().push(pid));
            let toggled = Rc::new(RefCell::new(Vec::new()));
            let t = toggled.clone();
            map.connect_check_toggled(move |pid, on| t.borrow_mut().push((pid, on)));
            map.set_input(input.clone());
            let win = gtk::Window::new();
            win.set_default_size(760, 466);
            win.set_child(Some(&map));
            win.present();
            settle();

            // Every session tile is a focusable button that speaks its label.
            let tiles: Vec<gtk::Widget> = map
                .tiles()
                .into_iter()
                .filter(|t| MemoryMap::tile_pid(t).is_some())
                .collect();
            assert!(tiles.len() >= 30, "{} session tiles", tiles.len());
            for tile in &tiles {
                let pid = MemoryMap::tile_pid(tile).unwrap();
                let s = input.sessions.iter().find(|s| s.pid == pid).unwrap();
                assert!(tile.is_focusable(), "{pid}");
                assert_eq!(tile.accessible_role(), gtk::AccessibleRole::Button);
                assert_eq!(
                    spoken_label(tile, &spoken(&s.name, s.bytes(), s.state)),
                    None
                );
            }
            // Free RAM is a bar in the page header, not a tile; the sessions
            // reach the right edge of the map.
            assert!(
                map.tiles().iter().all(|t| !t.has_css_class("free")),
                "no free tile"
            );
            // The layout's rects, not the widgets': tiles glide to them.
            let right = map
                .imp()
                .layout
                .borrow()
                .tiles
                .iter()
                .map(|t| t.rect.right())
                .fold(0.0, f64::max);
            assert!(
                right >= f64::from(map.width()) - 2.0,
                "tiles end at {right} of {} px",
                map.width()
            );

            // Arrows move to the nearest tile, as the pure rule says.
            let rects: Vec<Rect> = tiles.iter().map(|t| bounds(&map, t)).collect();
            tiles[0].grab_focus();
            let mut at = 0;
            for dir in [Dir::Right, Dir::Down, Dir::Down, Dir::Left, Dir::Up] {
                let key = match dir {
                    Dir::Right => gtk::gdk::Key::Right,
                    Dir::Left => gtk::gdk::Key::Left,
                    Dir::Up => gtk::gdk::Key::Up,
                    Dir::Down => gtk::gdk::Key::Down,
                };
                let next = nearest(&rects, at, dir);
                assert_eq!(press(&map, key), next.is_some(), "{dir:?} handled at {at}");
                at = next.unwrap_or(at);
                assert_eq!(
                    map.focus_child().as_ref(),
                    Some(&tiles[at]),
                    "focus after {dir:?}"
                );
            }

            // Enter calls back with the pid and selects the tile.
            assert!(press(&map, gtk::gdk::Key::Return));
            let pid = MemoryMap::tile_pid(&tiles[at]).unwrap();
            assert_eq!(*activated.borrow(), vec![pid]);
            assert_eq!(map.selected(), Some(pid));
            assert!(tiles[at].has_css_class("selected"));

            // Reclaim: idle tiles are teal targets, the rest dimmed and out
            // of the tab order; Enter toggles the check.
            map.set_reclaim(true);
            settle();
            let tiles = map.tiles();
            let idle: Vec<&gtk::Widget> = tiles
                .iter()
                .filter(|t| t.has_css_class("reclaim"))
                .collect();
            assert_eq!(
                idle.len(),
                26,
                "every idle session is a target, none merged"
            );
            for t in &tiles {
                if t.has_css_class("dim") {
                    assert!(!t.is_focusable());
                }
            }
            idle[0].grab_focus();
            let ipid = MemoryMap::tile_pid(idle[0]).unwrap();
            assert!(press(&map, gtk::gdk::Key::Return));
            assert!(press(&map, gtk::gdk::Key::space));
            assert_eq!(*toggled.borrow(), vec![(ipid, true), (ipid, false)]);
            assert!(map.checked().is_empty());
            map.set_checked(&[ipid]);
            assert!(idle[0].has_css_class("checked"));
            assert_eq!(
                *activated.borrow(),
                vec![pid],
                "reclaim does not open the detail"
            );
        });
    }

    #[test]
    fn state_flip_alone_updates_the_spoken_label() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let mut input = MapInput::from_snapshot(&fixture("tipico-40.json"));
            let map = MemoryMap::new();
            map.set_input(input.clone());
            let win = gtk::Window::new();
            win.set_default_size(760, 466);
            win.set_child(Some(&map));
            win.present();
            settle();
            let s = input
                .sessions
                .iter_mut()
                .find(|s| s.state == TileState::Busy)
                .expect("a busy session");
            let (pid, name, bytes) = (s.pid, s.name.clone(), s.bytes());
            s.state = TileState::Waiting;
            feed(&map, input);
            settle();
            let tile = map
                .tiles()
                .into_iter()
                .find(|t| MemoryMap::tile_pid(t) == Some(pid))
                .expect("tile");
            assert_eq!(
                spoken_label(&tile, &spoken(&name, bytes, TileState::Waiting)),
                None
            );
        });
    }

    #[test]
    fn merged_small_tiles_are_focusable_and_open_their_largest_session() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let input = MapInput::from_snapshot(&fixture("tipico-40.json"));
            let map = MemoryMap::new();
            let activated = Rc::new(RefCell::new(Vec::new()));
            let a = activated.clone();
            map.connect_activated(move |pid| a.borrow_mut().push(pid));
            map.set_input(input.clone());
            let win = gtk::Window::new();
            win.set_default_size(620, 380); // the free column is gone: 760 px no longer merges
            win.set_child(Some(&map));
            win.present();
            settle();

            let merged: Vec<gtk::Widget> = map
                .tiles()
                .into_iter()
                .filter(|t| t.has_css_class("merged"))
                .collect();
            assert!(!merged.is_empty(), "tipico-40 merges its small sessions");
            for tile in &merged {
                assert!(tile.is_focusable(), "every tile is focusable");
                assert_eq!(tile.accessible_role(), gtk::AccessibleRole::Button);
            }
            let tile = &merged[0];
            let Kind::Merged(ids) = tile.downcast_ref::<MapTile>().unwrap().kind() else {
                panic!("a merged tile");
            };
            let by_pid = input.by_pid();
            let total: u64 = ids.iter().map(|id| by_pid[id].bytes()).sum();
            let largest = ids
                .iter()
                .max_by_key(|id| (by_pid[id].bytes(), std::cmp::Reverse(**id)))
                .copied()
                .unwrap();
            let state = by_pid[&ids[0]].state;
            assert_eq!(
                spoken_label(
                    tile,
                    &format!(
                        "{} small {} sessions, {}",
                        ids.len(),
                        state.word(),
                        gib(total)
                    )
                ),
                None,
                "the label names the count and the total"
            );
            assert!(tile.grab_focus());
            assert!(press(&map, gtk::gdk::Key::Return));
            assert_eq!(*activated.borrow(), vec![largest]);
            assert_eq!(map.selected(), Some(largest));
        });
    }

    // ---- names ----------------------------------------------------------------

    use agentcenter_core::overlay::{HerdrOverlay, SessionOverlay};

    /// An agent session of `uma.json` with the given overlay name, herdr
    /// title and pane, and cwd.
    fn named(name: Option<&str>, title: Option<&str>, pane: Option<&str>, cwd: &str) -> Session {
        let mut snap = fixture("uma.json");
        let mut s = snap.sessions.remove(0);
        s.cwd = Some(cwd.into());
        s.overlay = Some(SessionOverlay {
            name: name.map(Into::into),
            herdr: Some(HerdrOverlay {
                pane_id: pane.map(Into::into),
                title: title.map(Into::into),
                ..HerdrOverlay::default()
            }),
            ..SessionOverlay::default()
        });
        s
    }

    #[test]
    fn a_transcript_title_names_the_tile() {
        let s = named(
            Some("Fix the login redirect"),
            Some("pane title"),
            Some("w1-3"),
            "/home/dev/src/kiln",
        );
        assert_eq!(session_name(&s), "Fix the login redirect");
    }

    #[test]
    fn without_a_title_the_name_is_cwd_basename_dash_pane() {
        let s = named(None, None, Some("w1-3"), "/home/dev/src/kiln/");
        assert_eq!(session_name(&s), "kiln - w1-3");
        let s = named(None, None, None, "/home/dev/src/kiln");
        assert_eq!(session_name(&s), "kiln", "no pane to add");
    }

    #[test]
    fn a_slash_command_is_never_the_name() {
        for bad in ["/handoff pickup", "/clear", "  /agenfk  ", "/"] {
            let s = named(Some(bad), None, Some("w2-1"), "/home/dev/src/atlas");
            assert_eq!(session_name(&s), "atlas - w2-1", "{bad:?}");
        }
    }

    #[test]
    fn a_filler_first_prompt_is_never_the_name() {
        for bad in ["ok", "Continue", "hi", "Hello!", "yes", "go on", "?", "a"] {
            let s = named(Some(bad), None, Some("w2-1"), "/home/dev/src/atlas");
            assert_eq!(session_name(&s), "atlas - w2-1", "{bad:?}");
        }
        // A short but real title stays.
        let s = named(Some("Fix CI"), None, None, "/home/dev/src/atlas");
        assert_eq!(session_name(&s), "Fix CI");
    }

    #[test]
    fn a_rejected_title_falls_to_the_herdr_title_then_to_the_cwd() {
        let s = named(
            Some("/handoff pickup"),
            Some("Billing refactor"),
            Some("w1-3"),
            "/home/dev/src/kiln",
        );
        assert_eq!(session_name(&s), "Billing refactor");
        let s = named(
            Some("/handoff pickup"),
            Some("/clear"),
            Some("w1-3"),
            "/home/dev/src/kiln",
        );
        assert_eq!(session_name(&s), "kiln - w1-3");
    }

    #[test]
    fn names_are_sanitised_and_the_multiplexer_keeps_its_own() {
        let s = named(
            Some("Fix\u{202e}auth\u{1b}[31m"),
            None,
            None,
            "/home/dev/src/kiln",
        );
        let n = session_name(&s);
        assert!(!n.chars().any(|c| c.is_control() || c == '\u{202e}'), "{n}");
        assert!(n.starts_with("Fix"), "{n}");
        let mut mux = named(Some("/clear"), None, Some("w1-1"), "/home/dev/src/kiln");
        mux.role = AgentRole::Multiplexer;
        mux.agent = "herdr".into();
        assert_eq!(session_name(&mux), "herdr multiplexer");
    }

    #[test]
    fn from_snapshot_names_through_the_same_rule() {
        let mut snap = fixture("uma.json");
        let s = &mut snap.sessions[0];
        *s = named(
            Some("/handoff pickup"),
            None,
            Some("w9-2"),
            "/home/dev/src/kiln",
        );
        assert_eq!(
            MapInput::from_snapshot(&snap).sessions[0].name,
            "kiln - w9-2"
        );
    }

    // ---- stable layout between ticks -----------------------------------------

    fn busy(pid: u32, gib: f64) -> MapSession {
        MapSession {
            pid,
            name: format!("s{pid}"),
            state: TileState::Busy,
            pss: (gib * GIB as f64) as u64,
            swap: 0,
        }
    }

    fn tick(at_ms: u64, sessions: &[(u32, f64)]) -> MapInput {
        MapInput {
            sessions: sessions.iter().map(|&(p, g)| busy(p, g)).collect(),
            free_bytes: 0,
            taken_at_ms: at_ms,
        }
    }

    /// Every tile's kind and rect, as text: equal text, same layout.
    fn rects(map: &MemoryMap) -> String {
        format!(
            "{:?}",
            map.imp()
                .layout
                .borrow()
                .tiles
                .iter()
                .map(|t| (t.kind.clone(), t.rect))
                .collect::<Vec<_>>()
        )
    }

    /// The first window of a fresh process can take longer than settle()'s
    /// 300 ms to get its allocation; a layout read before that has 0x0 rects.
    fn wait_allocated(map: &impl IsA<gtk::Widget>) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while map.width() <= 0 || map.height() <= 0 {
            assert!(
                std::time::Instant::now() < until,
                "the map got no allocation within 5 s"
            );
            let context = glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        settle();
    }

    fn mapped(first: MapInput) -> (MemoryMap, gtk::Window) {
        adw::init().expect("GTK display (Broadway)");
        load_css();
        let map = MemoryMap::new();
        map.set_input(first);
        let win = gtk::Window::new();
        win.set_default_size(640, 400);
        win.set_child(Some(&map));
        win.present();
        settle();
        wait_allocated(&map);
        // A window that resizes after it shows moves the tiles on their
        // springs; the tests read rects once they arrived.
        crate::motion::testing::until_idle(|| !map.motion_active() && !map.motion_ticking());
        // The window hands the first tile the focus and Broadway's pointer
        // may sit over the map; the tests start with both outside it.
        gtk::prelude::GtkWindowExt::set_focus(&win, None::<&gtk::Widget>);
        settle();
        map.imp().hovered.set(false);
        map.imp().has_focus.set(false);
        (map, win)
    }

    /// Delivers `input` with the pointer and the focus outside the map:
    /// Broadway's pointer can enter late, and a held map would hide a
    /// relayout the rule allows. Layout is synchronous: no settle needed.
    fn feed(map: &MemoryMap, input: MapInput) {
        map.imp().hovered.set(false);
        map.imp().has_focus.set(false);
        map.set_input(input);
    }

    const BASE: &[(u32, f64)] = &[(1, 10.0), (2, 10.5), (3, 20.0)];

    fn tile_rects(map: &MemoryMap) -> Vec<(u32, Rect)> {
        map.tiles()
            .iter()
            .filter_map(|t| Some((MemoryMap::tile_pid(t)?, bounds(map, t))))
            .collect()
    }

    /// What `ending` saw: rects before, every frame, rects after, ticked.
    type Ending = (
        Vec<(u32, Rect)>,
        Vec<Vec<(u32, Rect)>>,
        Vec<(u32, Rect)>,
        bool,
    );

    /// Session 2 ends 31 s in; the map lays out again and the rest grow.
    fn ending(animations: bool) -> Ending {
        let (map, _win) = mapped(tick(100_000, BASE));
        assert!(!map.motion_active() && !map.motion_ticking(), "idle first");
        let before = tile_rects(&map);
        feed(&map, tick(131_000, &[(1, 10.0), (3, 20.0)]));
        let mut ticked = false;
        let mut frames = Vec::new();
        crate::motion::testing::pump(if animations { 1200 } else { 500 }, || {
            ticked |= map.motion_ticking();
            frames.push(tile_rects(&map));
        });
        assert!(!map.motion_active() && !map.motion_ticking(), "settled");
        (before, frames, tile_rects(&map), ticked)
    }

    #[test]
    fn a_relayout_moves_tiles_on_a_spring_and_the_ticks_stop_when_settled() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            crate::motion::testing::animated(true, || {
                let (before, frames, after, ticked) = ending(true);
                assert!(ticked, "a tick callback ran while the springs moved");
                let at = |v: &[(u32, Rect)]| v.iter().find(|(p, _)| *p == 3).unwrap().1;
                assert_ne!(at(&before), at(&after), "the freed area moved tile 3");
                let between = frames.iter().any(|f| {
                    f.iter()
                        .find(|(p, _)| *p == 3)
                        .is_some_and(|(_, r)| *r != at(&before) && *r != at(&after))
                });
                assert!(between, "tile 3 was seen between its old and new rect");
            });
        });
    }

    #[test]
    fn with_animations_off_the_tiles_jump_and_nothing_ticks() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            crate::motion::testing::animated(false, || {
                let (_before, frames, after, ticked) = ending(false);
                assert!(!ticked, "no tick callback");
                // Every frame after the first allocation is the final one.
                let settled = frames.iter().skip(frames.len() / 4).all(|f| *f == after);
                assert!(settled, "the tiles jumped to the layout");
            });
        });
    }

    #[test]
    fn a_size_order_flip_under_20_percent_keeps_the_layout() {
        gtk::test_synced(|| {
            let (map, _win) = mapped(tick(100_000, BASE));
            let before = rects(&map);
            // 1 and 2 swap places in the size order: +6 % and -5 %.
            feed(&map, tick(140_000, &[(1, 10.6), (2, 10.0), (3, 20.0)]));
            assert_eq!(rects(&map), before);
        });
    }

    #[test]
    fn a_session_starting_or_ending_relayouts_after_30_seconds_not_before() {
        gtk::test_synced(|| {
            let (map, _win) = mapped(tick(100_000, BASE));
            let before = rects(&map);
            let four = [(1, 10.0), (2, 10.5), (3, 20.0), (4, 5.0)];
            feed(&map, tick(110_000, &four));
            assert_eq!(rects(&map), before, "a start inside 30 s waits");
            feed(&map, tick(131_000, &four));
            let after_start = rects(&map);
            assert_ne!(after_start, before, "30 s after the layout");
            assert!(after_start.contains("Session(4)"));
            feed(&map, tick(140_000, BASE));
            assert_eq!(rects(&map), after_start, "an end inside 30 s waits");
            feed(&map, tick(162_000, BASE));
            assert!(!rects(&map).contains("Session(4)"), "the end applies");
        });
    }

    #[test]
    fn a_drift_past_20_percent_relayouts_once_30_seconds_passed() {
        gtk::test_synced(|| {
            let (map, _win) = mapped(tick(100_000, BASE));
            let before = rects(&map);
            let grown = [(1, 10.0), (2, 10.5), (3, 40.0)];
            feed(&map, tick(120_000, &grown));
            assert_eq!(rects(&map), before, "inside 30 s");
            feed(&map, tick(131_000, &grown));
            assert_ne!(rects(&map), before);
        });
    }

    #[test]
    fn the_layout_waits_under_the_pointer_and_applies_on_leave() {
        gtk::test_synced(|| {
            let (map, _win) = mapped(tick(100_000, BASE));
            let before = rects(&map);
            map.imp().hovered.set(true);
            let four = tick(200_000, &[(1, 10.0), (2, 10.5), (3, 20.0), (4, 5.0)]);
            map.set_input(four);
            settle();
            assert_eq!(rects(&map), before, "held under the pointer");
            map.imp().hovered.set(false);
            map.apply();
            settle();
            assert!(rects(&map).contains("Session(4)"), "applied on leave");
        });
    }

    #[test]
    fn the_layout_waits_under_focus_and_applies_when_it_leaves() {
        gtk::test_synced(|| {
            let (map, _win) = mapped(tick(100_000, BASE));
            let before = rects(&map);
            // Set the flag the focus controller sets: a real grab_focus only
            // reaches the controller while Broadway has this window active,
            // which other tests in the same process change.
            map.imp().has_focus.set(true);
            map.set_input(tick(200_000, &[(1, 10.0), (3, 20.0)]));
            settle();
            assert_eq!(rects(&map), before, "held under focus");
            map.imp().has_focus.set(false);
            map.apply();
            settle();
            assert!(!rects(&map).contains("Session(2)"), "applied on leave");
        });
    }
}
