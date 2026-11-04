//! Token map (RFC-AC-002): one tile per agent session, area = weighted
//! tokens since start, colour = the violet heat bin of the 30-minute spend
//! ([`crate::spend`]), a context % bar and a Compact or Handoff tag.
//!
//! - Input: [`TokenInput::from_snapshot`], a pure function of the snapshot
//!   and the shared [`SpendTracker`].
//! - Each tile is a child widget (CSS `tokenmap > tile.heat-N`): GTK paints
//!   the heat fill from token_map.css, the tile draws the state glyph, name,
//!   value, tag and context bar in its CSS `color` (the on-fill text token).
//!   Tiles never wear the memory map's state fills; the state is the glyph's
//!   shape. Fonts come from hidden label nodes, as in the memory map.
//! - What a tile carries depends on its size ([`anatomy`]).
//! - Bins b2 and up are never merged into an "N small" tile and keep a
//!   64x40 px floor ([`token_layout`]).
//! - Stability ([`TokenStability`]): the layout changes only when a tile's
//!   share drifts more than 20 % or a session starts or ends, at most every
//!   30 s, and never while the pointer or the focus is in the map.

use crate::memory_map::{Glyph, MapInput, draw_glyph};
use crate::spend::{Key, SpendTracker};
use crate::treemap::{self, Dir, Item, Layout, Rect, TileKind, TileState};
use agentcenter_core::Snapshot;
use agentcenter_core::adapters::AgentRole;
use agentcenter_core::cockpit::{Recommendation, ctx_pct};
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango, prelude::*};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// Tag a tile carries when the cockpit asks for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Compact,
    Handoff,
}

impl Tag {
    fn word(self) -> &'static str {
        match self {
            Tag::Compact => "Compact",
            Tag::Handoff => "Handoff",
        }
    }
}

/// One session on the token map.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenSession {
    pub pid: u32,
    pub starttime: Option<u64>,
    pub name: String,
    pub state: TileState,
    /// Weighted tokens since start: the tile's area.
    pub since_start: u64,
    /// Heat bin 0..=4 ([`SpendTracker::bin`]); `None` when the spend is unknown.
    pub bin: Option<u8>,
    pub ctx_pct: Option<f64>,
    pub tag: Option<Tag>,
}

/// What the map draws.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TokenInput {
    pub sessions: Vec<TokenSession>,
    /// Clock of the stability rule.
    pub taken_at_ms: u64,
}

impl TokenInput {
    /// Agent sessions of `snap` with a known since-start spend, coloured by
    /// `spend` (already updated with the same snapshot). Name and state are
    /// the memory map's, so a session reads the same on both maps; the
    /// context % is the last request's over the window.
    pub fn from_snapshot(snap: &Snapshot, spend: &SpendTracker) -> Self {
        let map = MapInput::from_snapshot(snap);
        let sessions = snap
            .sessions
            .iter()
            .filter(|s| s.role == AgentRole::Agent)
            .filter_map(|s| {
                let key: Key = (s.pid, s.starttime);
                let since_start = spend.since_start(key)?;
                let cockpit = s.overlay.as_ref()?.cockpit.as_ref()?;
                let m = map.sessions.iter().find(|m| m.pid == s.pid)?;
                Some(TokenSession {
                    pid: s.pid,
                    starttime: s.starttime,
                    name: m.name.clone(),
                    state: m.state,
                    since_start,
                    bin: spend.bin(key),
                    ctx_pct: (cockpit.window > 0)
                        .then(|| ctx_pct(cockpit.ctx_tokens, cockpit.window)),
                    tag: match cockpit.recommendation {
                        Recommendation::Compactar => Some(Tag::Compact),
                        Recommendation::Handoff => Some(Tag::Handoff),
                        _ => None,
                    },
                })
            })
            .collect();
        Self {
            sessions,
            taken_at_ms: snap.taken_at_ms,
        }
    }

    fn items(&self) -> Vec<Item> {
        self.sessions
            .iter()
            .map(|s| Item {
                id: s.pid,
                state: s.state,
                weight: s.since_start as f64,
            })
            .collect()
    }

    fn weights(&self) -> Vec<(u32, f64)> {
        self.sessions
            .iter()
            .map(|s| (s.pid, s.since_start as f64))
            .collect()
    }
}

// ---- words and numbers ----------------------------------------------------------

/// `1.2M`, `340k`, `950`: tokens in a tile.
pub fn tok_short(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1e3),
        10_000..=999_499 => format!("{:.0}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// CSS class of a heat bin; an unknown spend has its own, a bin past 4 is 4.
pub fn heat_class(bin: Option<u8>) -> &'static str {
    match bin {
        None => "heat-none",
        Some(0) => "heat-0",
        Some(1) => "heat-1",
        Some(2) => "heat-2",
        Some(3) => "heat-3",
        Some(_) => "heat-4",
    }
}

fn heat_words(bin: Option<u8>) -> &'static str {
    match bin {
        None => "spend unknown",
        Some(0) => "no spend in 30 minutes",
        Some(1) => "under 1k tokens a minute",
        Some(2) => "1k to 5k tokens a minute",
        Some(3) => "5k to 20k tokens a minute",
        Some(_) => "over 20k tokens a minute",
    }
}

/// Accessible label of a tile: everything it draws, in words.
pub fn spoken(s: &TokenSession) -> String {
    let mut out = format!(
        "{}, {} tokens since start, {}",
        s.name,
        tok_short(s.since_start),
        heat_words(s.bin)
    );
    if let Some(c) = s.ctx_pct {
        out.push_str(&format!(", context {c:.0} %"));
    }
    if let Some(t) = s.tag {
        out.push_str(&format!(", {} suggested", t.word().to_lowercase()));
    }
    out.push_str(&format!(", {}", s.state.word()));
    out
}

// ---- anatomy -----------------------------------------------------------------------

/// How a tile's tag is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagShape {
    None,
    Icon,
    Pill,
}

/// What a tile of a given size can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anatomy {
    pub glyph: bool,
    pub name: bool,
    /// Room for the "1.2M tok" row; the draw also checks the text fits.
    pub value: bool,
    pub bar: bool,
    pub tag: TagShape,
    pub name_lines: u8,
}

const GLYPH_MIN_PX: f64 = 22.0;
/// The name is drawn from this width up.
const NAME_MIN_W_PX: f64 = treemap::NAME_MIN_W_PX;
const VALUE_MIN_H_PX: f64 = 40.0;
/// Under this, in either direction, no tag, bar or icon.
const TAG_MIN_PX: f64 = 40.0;
const PILL_MIN_W_PX: f64 = 120.0;
/// Name row, value row, a 18 px pill row, the bar and the padding.
const PILL_MIN_H_PX: f64 = 68.0;
const TWO_LINES_MIN_H_PX: f64 = 58.0;
/// With a pill row the second name line needs 14 px more.
const TWO_LINES_PILL_MIN_H_PX: f64 = 82.0;
const PILL_H: f32 = 18.0;
const ICON_PX: f32 = 14.0;
const BAR_H: f32 = 3.0;
const BAR_INSET: f32 = 6.0;
const ROW1_Y: f32 = 3.0;
const PAD: f32 = 8.0;
const NAME_X: f32 = (treemap::LINE1_INSET_PX - treemap::TILE_PAD_PX) as f32;

pub fn anatomy(w: f64, h: f64) -> Anatomy {
    let tag = if w < TAG_MIN_PX || h < TAG_MIN_PX {
        TagShape::None
    } else if w >= PILL_MIN_W_PX && h >= PILL_MIN_H_PX {
        TagShape::Pill
    } else {
        TagShape::Icon
    };
    let two = if tag == TagShape::Pill {
        h >= TWO_LINES_PILL_MIN_H_PX
    } else {
        h >= TWO_LINES_MIN_H_PX
    };
    Anatomy {
        glyph: w >= GLYPH_MIN_PX && h >= GLYPH_MIN_PX,
        name: w >= NAME_MIN_W_PX && h >= GLYPH_MIN_PX,
        value: h >= VALUE_MIN_H_PX,
        bar: w >= TAG_MIN_PX && h >= TAG_MIN_PX,
        tag,
        name_lines: if two { 2 } else { 1 },
    }
}

// ---- layout ---------------------------------------------------------------------------------

/// Pixels around a tile that the widget keeps free as a gap.
const INSET_PX: i32 = 1;

/// The map's tiles in `bounds`: area = tokens since start, grouped by state;
/// small tiles of bin 0, 1 or unknown merge per state, b2+ never do and get
/// the 64x40 floor on the drawn tile (the gap is added to the floor).
pub fn token_layout(sessions: &[TokenSession], bounds: Rect) -> Layout {
    let items: Vec<Item> = sessions
        .iter()
        .map(|s| Item {
            id: s.pid,
            state: s.state,
            weight: s.since_start as f64,
        })
        .collect();
    let hot: BTreeSet<u32> = sessions
        .iter()
        .filter(|s| s.bin.is_some_and(|b| b >= 2))
        .map(|s| s.pid)
        .collect();
    let gap = f64::from(2 * INSET_PX);
    treemap::layout_pinned_floor(
        &items,
        bounds,
        &|id| hot.contains(&id),
        (
            treemap::PINNED_MIN_W_PX + gap,
            treemap::PINNED_MIN_H_PX + gap,
        ),
    )
}

// The rule moved to treemap.rs so both maps share it; the names stay here.
#[allow(unused_imports)]
pub use treemap::{DRIFT, DriftStability as TokenStability, RELAYOUT_MIN_MS, name_px};

// ---- legend -------------------------------------------------------------------------------------

/// Five 12x12 swatches, coolest to hottest, and what the area means. The
/// page places it next to the map.
pub fn legend() -> gtk::Widget {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(2)
        .css_name("tokenlegend")
        .build();
    row.set_accessible_role(gtk::AccessibleRole::Group);
    row.update_property(&[gtk::accessible::Property::Label(
        "Colour is spend over the last 30 minutes, darker is less; area is tokens since start",
    )]);
    for bin in 0..=4u8 {
        let sw = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        sw.add_css_class("swatch");
        sw.add_css_class(heat_class(Some(bin)));
        sw.set_size_request(12, 12);
        sw.set_valign(gtk::Align::Center);
        sw.set_tooltip_text(Some(heat_words(Some(bin))));
        row.append(&sw);
    }
    let caption = gtk::Label::new(Some("area = tokens since start"));
    caption.add_css_class("caption");
    caption.set_margin_start(8);
    row.append(&caption);
    row.upcast()
}

// ---- tile ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
enum Kind {
    #[default]
    None,
    Session(u32),
    Merged(Vec<u32>),
}

/// Everything a tile draws; set by the map, read by the tile's snapshot.
#[derive(Debug, Clone, Default)]
struct Shown {
    kind: Kind,
    name: String,
    value: String,
    glyph: Glyph,
    ctx: Option<f64>,
    tag: Option<Tag>,
}

mod tile_imp {
    use super::*;

    #[derive(Default)]
    pub struct TokenTile {
        pub(super) shown: RefCell<Shown>,
        pub(super) spoken: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TokenTile {
        const NAME: &'static str = "AcTokenTile";
        type Type = super::TokenTile;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("tile");
        }
    }

    impl ObjectImpl for TokenTile {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }
    }

    impl WidgetImpl for TokenTile {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let obj = self.obj();
            let Some(map) = obj.parent().and_downcast::<super::TokenMap>() else {
                return;
            };
            let (w, h) = (f64::from(obj.width()), f64::from(obj.height()));
            super::draw_tile(snapshot, &map, &self.shown.borrow(), w, h, &obj.color());
        }
    }
}

glib::wrapper! {
    /// One tile of the map. CSS: `tokenmap > tile.heat-N`, plus `.merged`
    /// and `.selected`.
    pub struct TokenTile(ObjectSubclass<tile_imp::TokenTile>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TokenTile {
    fn new() -> Self {
        glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Button)
            .build()
    }

    fn kind(&self) -> Kind {
        self.imp().shown.borrow().kind.clone()
    }

    fn show(&self, next: Shown, label: &str) {
        self.imp().shown.replace(next);
        if *self.imp().spoken.borrow() != label {
            self.imp().spoken.replace(label.to_owned());
            self.update_property(&[gtk::accessible::Property::Label(label)]);
        }
        self.queue_draw();
    }
}

fn layout_of(carrier: &gtk::Label, text: &str) -> pango::Layout {
    carrier.create_pango_layout(Some(text))
}

fn put(snapshot: &gtk::Snapshot, layout: &pango::Layout, x: f32, y: f32, color: &gdk::RGBA) {
    snapshot.save();
    snapshot.translate(&graphene::Point::new(x, y));
    snapshot.append_layout(layout, color);
    snapshot.restore();
}

fn with_alpha(c: &gdk::RGBA, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), c.alpha() * a)
}

/// The 14 px tag icon at `(x, y)`: two chevrons closing in for Compact, an
/// arrow leaving a bar for Handoff.
fn draw_tag_icon(snapshot: &gtk::Snapshot, tag: Tag, x: f32, y: f32, color: &gdk::RGBA) {
    let p = gsk::PathBuilder::new();
    match tag {
        Tag::Compact => {
            p.move_to(x + 2.0, y + 1.5);
            p.line_to(x + 7.0, y + 6.0);
            p.line_to(x + 12.0, y + 1.5);
            p.move_to(x + 2.0, y + 12.5);
            p.line_to(x + 7.0, y + 8.0);
            p.line_to(x + 12.0, y + 12.5);
        }
        Tag::Handoff => {
            p.move_to(x + 1.0, y + 2.0);
            p.line_to(x + 1.0, y + 12.0);
            p.move_to(x + 4.0, y + 7.0);
            p.line_to(x + 13.0, y + 7.0);
            p.move_to(x + 9.5, y + 3.5);
            p.line_to(x + 13.0, y + 7.0);
            p.line_to(x + 9.5, y + 10.5);
        }
    }
    let stroke = gsk::Stroke::new(1.6);
    stroke.set_line_cap(gsk::LineCap::Round);
    stroke.set_line_join(gsk::LineJoin::Round);
    snapshot.append_stroke(&p.to_path(), &stroke, color);
}

/// Glyph, name, value, tag and context bar of a tile, by [`anatomy`].
fn draw_tile(
    snapshot: &gtk::Snapshot,
    map: &TokenMap,
    shown: &Shown,
    w: f64,
    h: f64,
    color: &gdk::RGBA,
) {
    let fonts = map.imp().fonts.borrow();
    let Some(f) = fonts.as_ref() else { return };
    let a = anatomy(w, h);
    if a.glyph {
        let at = if a.name {
            PAD
        } else {
            ((w - treemap::GLYPH_PX) as f32 / 2.0).min(PAD)
        };
        draw_glyph(
            snapshot,
            shown.glyph,
            at,
            PAD.min(h as f32 / 2.0 - 5.5),
            color,
        );
    }
    if !a.name {
        return;
    }
    let tag = shown.tag.filter(|_| a.tag != TagShape::None);
    let icon = tag.filter(|_| a.tag == TagShape::Icon);
    let room = w as f32 - NAME_X - PAD - icon.map_or(0.0, |_| ICON_PX + 4.0);
    let name = layout_of(
        if name_px(w) >= 13.0 {
            &f.name_big
        } else {
            &f.name
        },
        &shown.name,
    );
    name.set_width((f64::from(room.max(0.0)) * f64::from(pango::SCALE)) as i32);
    name.set_wrap(pango::WrapMode::WordChar);
    name.set_ellipsize(pango::EllipsizeMode::End);
    name.set_height(-i32::from(a.name_lines));
    put(snapshot, &name, NAME_X, ROW1_Y, color);
    if let Some(t) = icon {
        draw_tag_icon(snapshot, t, w as f32 - PAD - ICON_PX, ROW1_Y, color);
    }
    let name_h = name.pixel_size().1 as f32;
    if a.value {
        let value = layout_of(&f.value, &shown.value);
        if f64::from(value.pixel_size().0) <= w - f64::from(2.0 * PAD) {
            put(snapshot, &value, PAD, ROW1_Y + name_h + 1.0, color);
        }
    }
    if a.bar
        && let Some(c) = shown.ctx
    {
        let (x, y) = (BAR_INSET, h as f32 - BAR_INSET - BAR_H);
        let full = w as f32 - 2.0 * BAR_INSET;
        let fill = full * (c.clamp(0.0, 100.0) / 100.0) as f32;
        let round = |width: f32| {
            gsk::RoundedRect::from_rect(graphene::Rect::new(x, y, width, BAR_H), BAR_H / 2.0)
        };
        let track = gsk::PathBuilder::new();
        track.add_rounded_rect(&round(full));
        snapshot.append_fill(
            &track.to_path(),
            gsk::FillRule::Winding,
            &with_alpha(color, 0.28),
        );
        if fill >= 1.0 {
            let bar = gsk::PathBuilder::new();
            bar.add_rounded_rect(&round(fill.max(BAR_H)));
            snapshot.append_fill(&bar.to_path(), gsk::FillRule::Winding, color);
        }
    }
    if let (Some(t), TagShape::Pill) = (tag, a.tag) {
        let label = layout_of(&f.tag, t.word());
        let lw = label.pixel_size().0 as f32;
        let (pw, y) = (lw + 14.0, h as f32 - BAR_INSET - BAR_H - 4.0 - PILL_H);
        let p = gsk::PathBuilder::new();
        p.add_rounded_rect(&gsk::RoundedRect::from_rect(
            graphene::Rect::new(PAD + 0.5, y + 0.5, pw - 1.0, PILL_H - 1.0),
            PILL_H / 2.0,
        ));
        snapshot.append_stroke(&p.to_path(), &gsk::Stroke::new(1.2), color);
        let ly = y + (PILL_H - label.pixel_size().1 as f32) / 2.0;
        put(snapshot, &label, PAD + 7.0, ly, color);
    }
}

// ---- map ---------------------------------------------------------------------------------------------

/// Hidden label nodes that carry the fonts from token_map.css.
struct Fonts {
    name: gtk::Label,
    name_big: gtk::Label,
    value: gtk::Label,
    tag: gtk::Label,
}

impl Fonts {
    fn each(&self) -> [&gtk::Label; 4] {
        [&self.name, &self.name_big, &self.value, &self.tag]
    }
}

type Activated = Rc<dyn Fn(u32)>;
/// A tile and the rect its layout gave it.
type Placed = (TokenTile, Rect);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TokenMap {
        pub input: RefCell<TokenInput>,
        pub layout: RefCell<Layout>,
        /// Size the layout was made for.
        pub laid: Cell<(i32, i32)>,
        pub relayout_queued: Cell<bool>,
        pub stability: RefCell<TokenStability>,
        /// Session tiles by pid; kept across layouts so focus survives.
        pub sessions: RefCell<BTreeMap<u32, super::TokenTile>>,
        pub merged: RefCell<Vec<super::TokenTile>>,
        pub(super) fonts: RefCell<Option<Fonts>>,
        pub selected: Cell<Option<u32>>,
        pub on_activated: RefCell<Vec<Activated>>,
        pub hovered: Cell<bool>,
        pub has_focus: Cell<bool>,
        pub motion: Rc<crate::motion::TileMotion>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TokenMap {
        const NAME: &'static str = "AcTokenMap";
        type Type = super::TokenMap;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("tokenmap");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }
    }

    impl ObjectImpl for TokenMap {
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
                tag: carrier("tag"),
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

    impl WidgetImpl for TokenMap {
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
                        m.relayout();
                    }
                });
            }
            // Until then, the last layout stretched to the new size.
            let scale = |new: i32, old: i32| {
                if old > 0 {
                    f64::from(new) / f64::from(old)
                } else {
                    0.0
                }
            };
            let (sx, sy) = (scale(width, lw), scale(height, lh));
            let map: &gtk::Widget = obj.upcast_ref();
            for (t, r) in obj.placed() {
                let r = Rect::new(r.x * sx, r.y * sy, r.w * sx, r.h * sy);
                let (x, y, w, h) = r.snap();
                // Where the tile belongs; the spring says where it is now.
                let to = Rect::new(
                    f64::from(x + INSET_PX),
                    f64::from(y + INSET_PX),
                    f64::from((w - 2 * INSET_PX).max(0)),
                    f64::from((h - 2 * INSET_PX).max(0)),
                );
                let now = self.motion.place(map, &t, to);
                crate::motion::allocate(&t, now);
            }
            for (t, now) in self.motion.leaving() {
                crate::motion::allocate(&t, now);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.parent_snapshot(snapshot);
            let obj = self.obj();
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
            let g = INSET_PX as f32;
            let ring = gsk::RoundedRect::from_rect(
                graphene::Rect::new(
                    b.x() - g,
                    b.y() - g,
                    b.width() + 2.0 * g,
                    b.height() + 2.0 * g,
                ),
                RADIUS_PX + 1.0,
            );
            let c = obj.color();
            snapshot.append_border(&ring, &[RING_PX; 4], &[c, c, c, c]);
        }
    }
}

const RADIUS_PX: f32 = 4.0;
const RING_PX: f32 = 2.0;

glib::wrapper! {
    /// The token map. CSS: `tokenmap` (its `color` is the selection ring),
    /// `tokenmap > tile.heat-N`, and the font carriers
    /// `tokenmap > label.name|name-big|value|tag`.
    pub struct TokenMap(ObjectSubclass<imp::TokenMap>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TokenMap {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenMap {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// New data; relayout only when the stability rule allows.
    pub fn set_input(&self, input: TokenInput) {
        self.imp().input.replace(input);
        self.apply();
    }

    pub fn set_selected(&self, pid: Option<u32>) {
        self.imp().selected.set(pid);
        self.refresh();
        self.queue_draw();
    }

    /// Enter, space or a click on a tile: the session's pid.
    pub fn connect_activated(&self, f: impl Fn(u32) + 'static) {
        self.imp().on_activated.borrow_mut().push(Rc::new(f));
    }

    /// Visible tiles, in layout order.
    pub fn tiles(&self) -> Vec<gtk::Widget> {
        self.placed()
            .into_iter()
            .map(|(t, _)| t.upcast())
            .filter(|t: &gtk::Widget| t.is_visible())
            .collect()
    }

    /// The pid a tile stands for; `None` for a merged tile.
    pub fn tile_pid(tile: &gtk::Widget) -> Option<u32> {
        match tile.downcast_ref::<TokenTile>()?.kind() {
            Kind::Session(pid) => Some(pid),
            _ => None,
        }
    }

    /// Tiles in layout order with their rects (merged tiles by position).
    fn placed(&self) -> Vec<Placed> {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        let sessions = imp.sessions.borrow();
        let mut merged = imp.merged.borrow().clone().into_iter();
        layout
            .tiles
            .iter()
            .filter_map(|t| {
                let w = match &t.kind {
                    TileKind::Session(pid) => sessions.get(pid).cloned(),
                    TileKind::Merged(_) => merged.next(),
                }?;
                Some((w, t.rect))
            })
            .collect()
    }

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

    /// Pointer or focus in the map, after a first layout: no relayout.
    fn held(&self) -> bool {
        let imp = self.imp();
        (imp.hovered.get() || imp.has_focus.get()) && imp.laid.get().0 > 0
    }

    /// Takes the current input: relayout when the rule allows and nothing
    /// holds the map still, otherwise the numbers move in place.
    fn apply(&self) {
        let imp = self.imp();
        if self.held() {
            self.refresh();
            return;
        }
        let weights = imp.input.borrow().weights();
        let now = imp.input.borrow().taken_at_ms;
        if imp.stability.borrow_mut().should_relayout(&weights, now) {
            self.relayout();
        } else {
            self.refresh();
        }
    }

    /// A new layout for the current size and input; tiles follow it.
    fn relayout(&self) {
        let imp = self.imp();
        let (w, h) = (self.width(), self.height());
        imp.laid.set((w, h));
        let input = imp.input.borrow().clone();
        imp.stability
            .borrow_mut()
            .rebase(&input.weights(), input.taken_at_ms);
        let bounds = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        let layout = token_layout(&input.sessions, bounds);
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
                sessions.entry(*pid).or_insert_with(|| self.new_tile(false));
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
                merged.push(self.new_tile(true));
            }
        }
        imp.layout.replace(layout);
        self.refresh();
        self.queue_allocate();
    }

    fn new_tile(&self, merged: bool) -> TokenTile {
        let t = TokenTile::new();
        if merged {
            t.add_css_class("merged");
        }
        t.set_parent(self);
        let click = gtk::GestureClick::new();
        let (weak_map, weak_tile) = (self.downgrade(), t.downgrade());
        click.connect_released(move |_, _, _, _| {
            if let (Some(m), Some(t)) = (weak_map.upgrade(), weak_tile.upgrade()) {
                t.grab_focus();
                m.activate_tile(&t);
            }
        });
        t.add_controller(click);
        t
    }

    /// Numbers, classes, labels and visibility of every tile from the
    /// current input and the current layout's rects. A tile whose session
    /// left stays out until the next layout; a session without a tile waits
    /// for it.
    fn refresh(&self) {
        let imp = self.imp();
        let input = imp.input.borrow();
        let by_pid: BTreeMap<u32, &TokenSession> =
            input.sessions.iter().map(|s| (s.pid, s)).collect();
        let layout = imp.layout.borrow();
        let selected = imp.selected.get();
        let sessions = imp.sessions.borrow();
        let mut shown_pids = BTreeSet::new();
        let mut merged = imp.merged.borrow().clone().into_iter();
        for tile in &layout.tiles {
            let (widget, shown, label, bin) = match &tile.kind {
                TileKind::Session(pid) => {
                    let (Some(w), Some(s)) = (sessions.get(pid), by_pid.get(pid)) else {
                        continue;
                    };
                    shown_pids.insert(*pid);
                    set_class(w, "selected", selected == Some(*pid));
                    let shown = Shown {
                        kind: Kind::Session(*pid),
                        name: s.name.clone(),
                        value: format!("{} tok", tok_short(s.since_start)),
                        glyph: Glyph::of(s.state),
                        ctx: s.ctx_pct,
                        tag: s.tag,
                    };
                    (w.clone(), shown, spoken(s), s.bin)
                }
                TileKind::Merged(ids) => {
                    let Some(w) = merged.next() else { continue };
                    let members: Vec<&&TokenSession> =
                        ids.iter().filter_map(|id| by_pid.get(id)).collect();
                    if members.is_empty() {
                        w.set_visible(false);
                        w.set_focusable(false);
                        continue;
                    }
                    let since: u64 = members.iter().map(|s| s.since_start).sum();
                    // The hottest member colours the tile; none is b2+.
                    let bin = members.iter().filter_map(|s| s.bin).max();
                    let label = format!(
                        "{} small sessions, {} tokens since start, {}",
                        members.len(),
                        tok_short(since),
                        heat_words(bin)
                    );
                    let shown = Shown {
                        kind: Kind::Merged(members.iter().map(|s| s.pid).collect()),
                        name: format!("{} small", members.len()),
                        value: format!("{} tok", tok_short(since)),
                        glyph: Glyph::of(tile.state),
                        ..Shown::default()
                    };
                    set_class(&w, "selected", false);
                    (w, shown, label, bin)
                }
            };
            for b in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
                set_class(&widget, heat_class(b), b == bin);
            }
            widget.set_visible(true);
            widget.set_focusable(true);
            widget.show(shown, &label);
        }
        for (pid, w) in sessions.iter() {
            if !shown_pids.contains(pid) {
                w.set_visible(false);
                w.set_focusable(false);
            }
        }
        self.queue_draw();
    }

    fn focus_targets(&self) -> Vec<(TokenTile, Rect)> {
        self.placed()
            .into_iter()
            .filter(|(t, _)| t.is_focusable() && t.is_visible())
            .filter_map(|(t, _)| {
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
        let Some(focused) = self.focus_child().and_downcast::<TokenTile>() else {
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

    /// Enter or click on `tile`: select it and tell the page. A merged tile
    /// opens its biggest session.
    fn activate_tile(&self, tile: &TokenTile) -> bool {
        let imp = self.imp();
        let pid = match tile.kind() {
            Kind::Session(pid) => pid,
            Kind::Merged(ids) => {
                let input = imp.input.borrow();
                let Some(biggest) = input
                    .sessions
                    .iter()
                    .filter(|s| ids.contains(&s.pid))
                    .max_by_key(|s| (s.since_start, std::cmp::Reverse(s.pid)))
                    .map(|s| s.pid)
                else {
                    return false;
                };
                biggest
            }
            Kind::None => return false,
        };
        self.set_selected(Some(pid));
        let callbacks = imp.on_activated.borrow().clone();
        for f in callbacks {
            f(pid);
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
    use crate::spend::{self, SpendTracker};
    use crate::treemap::TileKind;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("parse")
    }

    fn tracked(snap: &Snapshot) -> SpendTracker {
        let mut t = SpendTracker::default();
        t.update(snap.taken_at_ms, spend::inputs(snap));
        t
    }

    fn sess(pid: u32, state: TileState, since: u64, bin: Option<u8>) -> TokenSession {
        TokenSession {
            pid,
            starttime: None,
            name: format!("session-{pid}"),
            state,
            since_start: since,
            bin,
            ctx_pct: Some(40.0),
            tag: None,
        }
    }

    // ---- input ---------------------------------------------------------------

    #[test]
    fn from_snapshot_keeps_agents_with_a_cockpit_and_reads_spend_context_and_tag() {
        let snap = fixture("cockpit.json");
        let spend = tracked(&snap);
        let input = TokenInput::from_snapshot(&snap, &spend);
        assert_eq!(input.taken_at_ms, snap.taken_at_ms);
        let pids: Vec<u32> = input.sessions.iter().map(|s| s.pid).collect();
        assert_eq!(
            pids,
            [50000, 50040, 50080, 50120, 50160],
            "the multiplexer and the agents without a transcript are out"
        );
        let by = |pid: u32| input.sessions.iter().find(|s| s.pid == pid).unwrap();
        assert_eq!(by(50080).since_start, 8_600_000);
        assert_eq!(by(50000).since_start, 1_115_000);
        assert_eq!(by(50040).name, "ledger-migration-v4");
        assert_eq!(by(50040).state, TileState::Idle);
        assert_eq!(by(50080).state, TileState::Waiting);
        assert_eq!(by(50160).state, TileState::Busy);
        assert_eq!(
            [50000, 50040, 50080, 50120, 50160].map(|p| by(p).bin),
            [Some(2), Some(2), Some(3), Some(1), Some(1)],
            "the bin is the tracker's: 30-minute spend over 30 minutes"
        );
        let ctx = |p: u32| by(p).ctx_pct.unwrap();
        assert!((ctx(50040) - 62.0).abs() < 1e-9);
        assert!((ctx(50080) - 100.0 * 200_000.0 / 258_400.0).abs() < 1e-9);
        assert_eq!(
            [50000, 50040, 50080, 50120, 50160].map(|p| by(p).tag),
            [None, Some(Tag::Compact), Some(Tag::Handoff), None, None],
            "only Compactar and Handoff verdicts tag a tile"
        );
    }

    #[test]
    fn from_snapshot_without_cockpit_data_is_empty() {
        let snap = fixture("vazio.json");
        let input = TokenInput::from_snapshot(&snap, &tracked(&snap));
        assert!(input.sessions.is_empty());
        let snap = fixture("tipico-40.json");
        let input = TokenInput::from_snapshot(&snap, &tracked(&snap));
        assert!(
            input
                .sessions
                .iter()
                .all(|s| s.since_start > 0 || s.bin.is_some()),
            "only sessions with a known since-start spend are kept"
        );
    }

    // ---- numbers and words ------------------------------------------------------

    #[test]
    fn tokens_are_short() {
        assert_eq!(tok_short(0), "0");
        assert_eq!(tok_short(950), "950");
        assert_eq!(tok_short(1_500), "1.5k");
        assert_eq!(tok_short(340_000), "340k");
        assert_eq!(tok_short(999_999), "1.0M");
        assert_eq!(tok_short(1_115_000), "1.1M");
        assert_eq!(tok_short(12_300_000), "12.3M");
    }

    #[test]
    fn bins_map_to_palette_classes() {
        assert_eq!(
            [0, 1, 2, 3, 4].map(|b| heat_class(Some(b))),
            ["heat-0", "heat-1", "heat-2", "heat-3", "heat-4"]
        );
        assert_eq!(heat_class(None), "heat-none");
        assert_eq!(heat_class(Some(9)), "heat-4", "out of range clamps");
    }

    #[test]
    fn the_accessible_label_carries_everything_the_tile_draws() {
        let mut s = sess(7, TileState::Idle, 2_705_000, Some(2));
        s.name = "ledger-migration-v4".into();
        s.ctx_pct = Some(62.0);
        s.tag = Some(Tag::Compact);
        assert_eq!(
            spoken(&s),
            "ledger-migration-v4, 2.7M tokens since start, 1k to 5k tokens a minute, \
             context 62 %, compact suggested, idle"
        );
        s.tag = Some(Tag::Handoff);
        s.bin = Some(4);
        s.state = TileState::Waiting;
        assert_eq!(
            spoken(&s),
            "ledger-migration-v4, 2.7M tokens since start, over 20k tokens a minute, \
             context 62 %, handoff suggested, waiting"
        );
        s.tag = None;
        s.bin = None;
        s.ctx_pct = None;
        assert_eq!(
            spoken(&s),
            "ledger-migration-v4, 2.7M tokens since start, spend unknown, waiting"
        );
        s.bin = Some(0);
        assert!(spoken(&s).contains("no spend in 30 minutes"));
    }

    // ---- anatomy -----------------------------------------------------------------

    #[test]
    fn the_name_shows_from_36_px_wide() {
        assert!(!anatomy(35.0, 80.0).name);
        assert!(anatomy(36.0, 80.0).name);
        assert!(!anatomy(36.0, 21.0).name, "and needs a row of height");
        assert!(anatomy(22.0, 22.0).glyph);
        assert!(!anatomy(21.0, 80.0).glyph);
        assert_eq!(name_px(95.0), 12.0, "12 px bold below 96 px tiles");
        assert_eq!(name_px(96.0), 13.0, "13 px from there");
    }

    #[test]
    fn the_tag_is_a_pill_from_120_px_an_icon_below_and_nothing_under_40x40() {
        assert_eq!(anatomy(120.0, 80.0).tag, TagShape::Pill);
        assert_eq!(anatomy(119.0, 80.0).tag, TagShape::Icon);
        assert_eq!(anatomy(40.0, 40.0).tag, TagShape::Icon);
        assert_eq!(anatomy(39.0, 200.0).tag, TagShape::None);
        assert_eq!(anatomy(300.0, 39.0).tag, TagShape::None);
        assert_eq!(
            anatomy(300.0, 60.0).tag,
            TagShape::Icon,
            "a pill needs its own 18 px row under name and value"
        );
        assert_eq!(anatomy(300.0, 68.0).tag, TagShape::Pill);
    }

    #[test]
    fn value_bar_and_name_rows_follow_the_height() {
        assert!(!anatomy(100.0, 39.0).value);
        assert!(anatomy(100.0, 40.0).value);
        assert!(!anatomy(39.0, 100.0).bar);
        assert!(anatomy(40.0, 40.0).bar);
        assert_eq!(anatomy(60.0, 57.0).name_lines, 1);
        assert_eq!(anatomy(60.0, 58.0).name_lines, 2);
        assert_eq!(
            anatomy(200.0, 70.0).name_lines,
            1,
            "the pill row takes the second name line"
        );
        assert_eq!(anatomy(200.0, 82.0).name_lines, 2);
    }

    // ---- layout ----------------------------------------------------------------------

    #[test]
    fn small_cool_tiles_merge_but_b2_and_up_never_do_and_keep_64x40() {
        let bounds = Rect::new(0.0, 0.0, 400.0, 240.0);
        let mut v = vec![sess(1, TileState::Idle, 1_000_000, Some(3))];
        for pid in 10..20 {
            v.push(sess(
                pid,
                TileState::Idle,
                5_000,
                Some(if pid % 2 == 0 { 0 } else { 1 }),
            ));
        }
        for (pid, bin) in [(30, 2), (31, 3), (32, 4)] {
            v.push(sess(pid, TileState::Idle, 5_000, Some(bin)));
        }
        let l = token_layout(&v, bounds);
        for pid in [30, 31, 32] {
            let t = l
                .tiles
                .iter()
                .find(|t| t.kind == TileKind::Session(pid))
                .unwrap_or_else(|| panic!("{pid} keeps its own tile"));
            assert!(
                t.rect.w >= 63.99 && t.rect.h >= 39.99,
                "{pid}: {:?}",
                t.rect
            );
        }
        let merged: Vec<usize> = l
            .tiles
            .iter()
            .filter_map(|t| match &t.kind {
                TileKind::Merged(ids) => Some(ids.len()),
                _ => None,
            })
            .collect();
        assert_eq!(merged, [10], "the ten b0/b1 sessions merge into one tile");
    }

    #[test]
    fn unknown_spend_merges_like_a_cool_tile() {
        let bounds = Rect::new(0.0, 0.0, 400.0, 240.0);
        let mut v = vec![sess(1, TileState::Idle, 1_000_000, Some(3))];
        for pid in 10..14 {
            v.push(sess(pid, TileState::Idle, 5_000, None));
        }
        let l = token_layout(&v, bounds);
        assert!(
            l.tiles
                .iter()
                .any(|t| matches!(&t.kind, TileKind::Merged(m) if m.len() == 4))
        );
    }

    #[test]
    fn tile_area_is_tokens_since_start() {
        let bounds = Rect::new(0.0, 0.0, 600.0, 300.0);
        let v = vec![
            sess(1, TileState::Busy, 3_000_000, Some(0)),
            sess(2, TileState::Busy, 1_000_000, Some(4)),
        ];
        let l = token_layout(&v, bounds);
        let area = |pid| {
            l.tiles
                .iter()
                .find(|t| t.kind == TileKind::Session(pid))
                .unwrap()
                .rect
                .area()
        };
        assert!((area(1) / area(2) - 3.0).abs() < 0.01, "heat does not size");
    }

    // ---- relayout rule ----------------------------------------------------------------

    fn w3(a: f64) -> Vec<(u32, f64)> {
        vec![(1, a), (2, 100.0), (3, 100.0)]
    }

    #[test]
    fn relayout_waits_for_a_20_percent_drift_and_has_no_timer_of_its_own() {
        let mut st = TokenStability::default();
        assert!(st.should_relayout(&w3(100.0), 0), "first call");
        assert!(!st.should_relayout(&w3(100.0), 1_000));
        assert!(!st.should_relayout(&w3(100.0), 120_000), "no timer");
        // Tile 1's share goes 0.333 -> 0.385 (+15 %): below the line, even
        // long after the 30 s.
        assert!(!st.should_relayout(&w3(125.0), 200_000));
        // -> 0.412 (+24 %): over the line, but not within 30 s of the last
        // layout (at 0).
        let mut fresh = TokenStability::default();
        assert!(fresh.should_relayout(&w3(100.0), 0));
        assert!(!fresh.should_relayout(&w3(140.0), 20_000));
    }

    #[test]
    fn a_drift_past_the_line_relayouts_once_30_seconds_passed_and_resets_the_base() {
        let mut st = TokenStability::default();
        assert!(st.should_relayout(&w3(100.0), 100_000));
        assert!(!st.should_relayout(&w3(140.0), 129_999), "29.999 s");
        assert!(st.should_relayout(&w3(140.0), 130_000), "30 s");
        assert!(
            !st.should_relayout(&w3(140.0), 200_000),
            "the new shares are the base"
        );
    }

    #[test]
    fn a_session_start_or_end_relayouts_after_30_seconds_not_before() {
        let mut st = TokenStability::default();
        assert!(st.should_relayout(&w3(100.0), 0));
        let mut four = w3(100.0);
        four.push((4, 100.0));
        assert!(!st.should_relayout(&four, 10_000), "start inside 30 s");
        assert!(st.should_relayout(&four, 30_000), "start");
        assert!(!st.should_relayout(&four, 40_000));
        assert!(!st.should_relayout(&w3(100.0), 50_000), "end inside 30 s");
        assert!(st.should_relayout(&w3(100.0), 60_000), "end");
    }

    #[test]
    fn invalidate_relayouts_at_once_and_a_clock_going_back_too() {
        let mut st = TokenStability::default();
        assert!(st.should_relayout(&w3(100.0), 50_000));
        assert!(!st.should_relayout(&w3(100.0), 51_000));
        st.invalidate();
        assert!(st.should_relayout(&w3(100.0), 52_000), "a resize");
        assert!(
            st.should_relayout(&w3(100.0), 1_000),
            "a fixture restarting"
        );
    }

    // ---- palette and legend --------------------------------------------------------------

    const CSS: &str = include_str!("../resources/token_map.css");

    #[test]
    fn the_stylesheet_carries_the_heat_palette_in_both_themes() {
        for hex in [
            // dark fills, then light fills
            "#443a5e", "#51406d", "#7857ab", "#a47de3", "#ccb1fe", "#eae5f4", "#c7b8e5", "#a486d7",
            "#7d4dbe", "#57288f", // b0 and b1 inset borders
            "#77738a", "#8a8698",
        ] {
            assert!(CSS.contains(hex), "{hex}");
        }
        assert!(CSS.contains("@define-color"));
        for bin in 0..=4 {
            assert!(CSS.contains(&format!("tile.heat-{bin}")), "heat-{bin}");
        }
    }

    #[test]
    fn the_darkest_violet_bin_is_lifted_off_the_panel() {
        let dark = CSS.split("prefers-color-scheme: light").next().unwrap();
        assert!(dark.contains("@define-color ac_heat_0 #443a5e;"));
        assert!(
            !CSS.contains("#302b39"),
            "the old b0 is gone from both themes"
        );
    }

    // ---- GTK -------------------------------------------------------------------------------

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

    fn provider(dark: bool) -> gtk::CssProvider {
        let css = gtk::CssProvider::new();
        css.set_prefers_color_scheme(if dark {
            gtk::InterfaceColorScheme::Dark
        } else {
            gtk::InterfaceColorScheme::Light
        });
        css.load_from_string(concat!(
            include_str!("../resources/tokens.css"),
            include_str!("../resources/token_map.css"),
        ));
        css
    }

    fn load_css() {
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider(true),
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    fn spoken_label(w: &impl IsA<gtk::Accessible>, expected: &str) -> Option<String> {
        use gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
        let expected = std::ffi::CString::new(expected).unwrap();
        // SAFETY: a live widget and a NUL-terminated string for the Label
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

    fn press(map: &TokenMap, key: gtk::gdk::Key) -> bool {
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

    fn rect_of(map: &TokenMap, w: &gtk::Widget) -> Rect {
        let b = w.compute_bounds(map).expect("allocated");
        Rect::new(
            b.x().into(),
            b.y().into(),
            b.width().into(),
            b.height().into(),
        )
    }

    fn shown(map: &TokenMap) -> Vec<(u32, Rect)> {
        map.tiles()
            .iter()
            .filter(|t| t.is_visible())
            .filter_map(|t| Some((TokenMap::tile_pid(t)?, rect_of(map, t))))
            .collect()
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

    fn window_with(map: &TokenMap) -> gtk::Window {
        let win = gtk::Window::new();
        win.set_default_size(760, 400);
        win.set_child(Some(map));
        win.present();
        settle();
        wait_allocated(map);
        // A window that resizes after it shows moves the tiles on their
        // springs; the tests read rects once they arrived.
        crate::motion::testing::until_idle(|| !map.motion_active() && !map.motion_ticking());
        win
    }

    #[test]
    fn tiles_speak_wear_their_heat_class_and_activate() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let snap = fixture("cockpit.json");
            let input = TokenInput::from_snapshot(&snap, &tracked(&snap));
            let map = TokenMap::new();
            let activated = Rc::new(RefCell::new(Vec::new()));
            let a = activated.clone();
            map.connect_activated(move |pid| a.borrow_mut().push(pid));
            map.set_input(input.clone());
            let _win = window_with(&map);

            let tiles = map.tiles();
            assert_eq!(tiles.len(), 5);
            for s in &input.sessions {
                let tile = tiles
                    .iter()
                    .find(|t| TokenMap::tile_pid(t) == Some(s.pid))
                    .unwrap_or_else(|| panic!("tile of {}", s.pid));
                assert_eq!(tile.accessible_role(), gtk::AccessibleRole::Button);
                assert!(tile.is_focusable());
                assert_eq!(spoken_label(tile, &spoken(s)), None, "{}", s.pid);
                assert!(tile.has_css_class(heat_class(s.bin)), "{}", s.pid);
                for state in ["busy", "waiting", "idle", "multiplexer"] {
                    assert!(
                        !tile.has_css_class(state),
                        "{}: heat is the fill, never the state",
                        s.pid
                    );
                }
            }
            let (pid, _) = shown(&map)[0];
            let tile = tiles
                .iter()
                .find(|t| TokenMap::tile_pid(t) == Some(pid))
                .unwrap();
            tile.grab_focus();
            assert!(press(&map, gtk::gdk::Key::Return));
            assert_eq!(*activated.borrow(), [pid]);
            map.set_selected(Some(pid));
            assert!(tile.has_css_class("selected"));
            map.set_selected(None);
            assert!(!tile.has_css_class("selected"));
        });
    }

    #[test]
    fn tiles_take_their_area_from_tokens_since_start() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let snap = fixture("cockpit.json");
            let input = TokenInput::from_snapshot(&snap, &tracked(&snap));
            let map = TokenMap::new();
            map.set_input(input);
            let _win = window_with(&map);
            let by: std::collections::BTreeMap<u32, Rect> = shown(&map).into_iter().collect();
            assert!(by[&50080].area() > by[&50040].area());
            assert!(by[&50040].area() > by[&50000].area());
        });
    }

    type Shot = Vec<(u32, Rect)>;

    fn two_ticks(drop_one: bool, second_at: u64, second_since: u64) -> (Shot, Shot) {
        let snap = fixture("cockpit.json");
        let mut input = TokenInput::from_snapshot(&snap, &tracked(&snap));
        input.taken_at_ms = 1_000_000;
        let map = TokenMap::new();
        map.set_input(input.clone());
        let _win = window_with(&map);
        let before = shown(&map);
        input.taken_at_ms = second_at;
        if drop_one {
            input.sessions.retain(|s| s.pid != 50000);
        } else {
            input
                .sessions
                .iter_mut()
                .find(|s| s.pid == 50080)
                .unwrap()
                .since_start = second_since;
        }
        map.set_input(input);
        settle();
        (before, shown(&map))
    }

    #[test]
    fn the_widget_keeps_its_rects_through_a_small_drift_and_a_quick_session_end() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            // +10 % on the biggest tile, 31 s later: under the 20 % line.
            let (before, after) = two_ticks(false, 1_031_000, 9_460_000);
            assert_eq!(before, after);
            // A session ends 10 s in: the layout stays until 30 s.
            let (before, after) = two_ticks(true, 1_010_000, 0);
            let kept: Vec<_> = before.iter().filter(|(p, _)| *p != 50000).collect();
            for (pid, r) in kept {
                assert_eq!(after.iter().find(|(p, _)| p == pid).map(|x| x.1), Some(*r));
            }
            assert!(after.iter().all(|(p, _)| *p != 50000), "its tile is gone");
        });
    }

    #[test]
    fn the_widget_relayouts_after_a_session_ends_and_30_seconds() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let (before, after) = two_ticks(true, 1_030_000, 0);
            let b = |v: &Shot, pid| v.iter().find(|(p, _)| *p == pid).unwrap().1;
            assert!(
                b(&after, 50080).area() > b(&before, 50080).area(),
                "the freed area goes to the others"
            );
        });
    }

    /// A cockpit map after its first layout, motion idle, and the same
    /// input 31 s later with the session 50000 gone: the map lays out again.
    fn ended_session() -> (TokenMap, gtk::Window, TokenInput) {
        let snap = fixture("cockpit.json");
        let mut input = TokenInput::from_snapshot(&snap, &tracked(&snap));
        input.taken_at_ms = 1_000_000;
        let map = TokenMap::new();
        map.set_input(input.clone());
        let win = window_with(&map);
        assert!(!map.motion_active() && !map.motion_ticking(), "idle first");
        // Broadway's pointer or the window's focus can sit on the map, and a
        // held map would hide the relayout the rule allows.
        gtk::prelude::GtkWindowExt::set_focus(&win, None::<&gtk::Widget>);
        crate::motion::testing::pump(50, || {});
        map.imp().hovered.set(false);
        map.imp().has_focus.set(false);
        input.taken_at_ms = 1_031_000;
        input.sessions.retain(|s| s.pid != 50000);
        (map, win, input)
    }

    #[test]
    fn a_relayout_moves_tiles_on_a_spring_and_the_ticks_stop_when_settled() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            crate::motion::testing::animated(true, || {
                let (map, _win, input) = ended_session();
                let before = shown(&map);
                let gone = map
                    .tiles()
                    .into_iter()
                    .find(|t| TokenMap::tile_pid(t) == Some(50000))
                    .expect("tile of 50000");
                map.set_input(input);
                assert!(gone.parent().is_some(), "the ended tile fades, not cut");
                let mut frames = Vec::new();
                let mut ticked = false;
                crate::motion::testing::pump(1200, || {
                    ticked |= map.motion_ticking();
                    frames.push(shown(&map));
                });
                let after = shown(&map);
                assert!(ticked, "a tick callback ran while the springs moved");
                assert!(!map.motion_active() && !map.motion_ticking(), "settled");
                assert!(gone.parent().is_none(), "the ended tile is gone at the end");
                let moved = |pid: u32| {
                    let at = |v: &Shot| v.iter().find(|(p, _)| *p == pid).map(|x| x.1);
                    (at(&before), at(&after))
                };
                let (b, a) = moved(50080);
                let (b, a) = (b.unwrap(), a.unwrap());
                assert_ne!(b, a, "the freed area moved the tile");
                let between = frames.iter().any(|f| {
                    f.iter()
                        .find(|(p, _)| *p == 50080)
                        .is_some_and(|(_, r)| *r != b && *r != a)
                });
                assert!(between, "the tile was seen between its old and new rect");
            });
        });
    }

    #[test]
    fn with_animations_off_the_tiles_jump_and_nothing_ticks() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            crate::motion::testing::animated(false, || {
                let (map, _win, input) = ended_session();
                let gone = map
                    .tiles()
                    .into_iter()
                    .find(|t| TokenMap::tile_pid(t) == Some(50000))
                    .expect("tile of 50000");
                map.set_input(input);
                assert!(gone.parent().is_none(), "unparented at once");
                let mut ticked = false;
                crate::motion::testing::pump(150, || ticked |= map.motion_ticking());
                let first = shown(&map);
                crate::motion::testing::pump(500, || ticked |= map.motion_ticking());
                assert!(!ticked && !map.motion_active());
                assert_eq!(first, shown(&map), "already where the layout puts it");
            });
        });
    }

    #[test]
    fn colours_follow_the_theme_in_the_palette() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            let display = gtk::gdk::Display::default().expect("display");
            let map = TokenMap::new();
            let tile = {
                let snap = fixture("cockpit.json");
                map.set_input(TokenInput::from_snapshot(&snap, &tracked(&snap)));
                let _ = window_with(&map);
                map.tiles()
                    .into_iter()
                    .find(|t| t.has_css_class("heat-3"))
                    .expect("a b3 tile")
            };
            let want = |hex: &str| gtk::gdk::RGBA::parse(hex).unwrap();
            for (dark, text) in [(true, "#15161c"), (false, "#ffffff")] {
                let p = provider(dark);
                gtk::style_context_add_provider_for_display(
                    &display,
                    &p,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
                settle();
                assert_eq!(tile.color(), want(text), "b3 text, dark: {dark}");
                gtk::style_context_remove_provider_for_display(&display, &p);
            }
        });
    }

    #[test]
    fn the_legend_has_five_12px_swatches_and_the_area_caption() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway)");
            load_css();
            let legend = legend();
            let mut swatches = Vec::new();
            let mut captions = Vec::new();
            let mut stack = vec![legend.clone()];
            while let Some(w) = stack.pop() {
                if w.has_css_class("swatch") {
                    swatches.push(w.clone());
                }
                if let Some(l) = w.downcast_ref::<gtk::Label>() {
                    captions.push(l.text().to_string());
                }
                let mut c = w.first_child();
                while let Some(x) = c {
                    c = x.next_sibling();
                    stack.push(x);
                }
            }
            assert_eq!(swatches.len(), 5);
            for s in &swatches {
                assert_eq!((s.width_request(), s.height_request()), (12, 12));
            }
            for bin in 0..=4u8 {
                assert!(
                    swatches
                        .iter()
                        .any(|s| s.has_css_class(heat_class(Some(bin)))),
                    "swatch of bin {bin}"
                );
            }
            assert!(captions.iter().any(|c| c == "area = tokens since start"));
        });
    }
}
