//! One-line session row (direction B, `direction.md` "Density") and the
//! section header.
//!
//! ```text
//! [glyph] name (ellipsized, full name in the tooltip)   [====-----]   84 %   0.87 GiB  [⋮]
//! ```
//!
//! The row is 34 px tall (`sessions.css`). The glyph carries the state by
//! shape and fill: busy disc, waiting triangle, idle ring, multiplexer 2x2
//! grid ([`glyph`]). The bar shows the list's current metric ([`Metric::of`]):
//! CPU in % of one core, or memory (PSS + swap, the swapped part hatched)
//! against the largest session in the list ([`RowMetric`]). The value next to
//! the bar reads the bar; the last column reads the other metric.
//!
//! The row is built once in the factory's `setup` and bound to its item by
//! expression: GTK swaps the [`SessionObject`] when it recycles the row, and
//! each property `notify` updates only what depends on it. No widget is born
//! per tick, and a bar whose fractions did not move is not reallocated.
//!
//! Colours come from CSS only (sessions.css): the glyph draws in its node's
//! `color`, the bar parts are CSS backgrounds, and the hatch draws in the
//! colour of its own node.

use super::SortKey;
use crate::actions_ui::{self, RowOp};
use crate::format::{DASH, Group, SHARE_BAR_PX, gauge_frac, per_min_text, share_text};
use crate::session_object::SessionObject;
use crate::state_glyph::{StateGlyph, glyph};
use agentcenter_core::actions::Target;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Row height in px, the 1 px rule between rows included; 44 below 1000 sp
/// ([`TOUCH_ROW_HEIGHT_PX`]). The stylesheet sets it
/// (`listview.session-rack > row`); the tests pin it.
#[cfg(test)]
const ROW_HEIGHT_PX: i32 = 40;
#[cfg(test)]
const TOUCH_ROW_HEIGHT_PX: i32 = 44;
/// Bar width in px; [`NARROW_BAR_PX`] when the list is narrow.
pub const BAR_PX: i32 = 120;
pub const NARROW_BAR_PX: i32 = 72;
/// Width of the share cell (bar, `%` and tokens per minute), the context
/// column, the Action column and the menu slot, in px. The axis repeats them.
const SHARE_CELL_PX: i32 = 232;
const CTX_CELL_PX: i32 = 48;
const ACTION_CELL_PX: i32 = 96;
const MENU_CELL_PX: i32 = 32;
/// Height of the sparkline, in px.
const SPARK_HEIGHT_PX: i32 = 20;
/// Glyph box in px.
const GLYPH_PX: i32 = 12;
/// Distance between hatch lines and their width, in px.
const HATCH_STEP_PX: f32 = 4.0;
const HATCH_LINE_PX: f32 = 1.5;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Listeners of the Action button: the session's pid and the operation.
pub type RowHandlers = Rc<RefCell<Vec<Rc<dyn Fn(u32, RowOp)>>>>;

// ---- Pure part ---------------------------------------------------------------

/// The columns that fit a list `width_sp` wide (REVIEW_RR1, "Layout and
/// adaptive"). Name and Action never drop, so they are not listed. With the
/// share bar gone the share shows its `%` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    pub cpu: bool,
    pub memory: bool,
    pub share_bar: bool,
    /// Gone from the screen, kept in the accessible label.
    pub context: bool,
}

impl Columns {
    pub fn at(width_sp: f64) -> Self {
        Columns {
            cpu: width_sp > 900.0,
            memory: width_sp > 720.0,
            share_bar: width_sp > 560.0,
            context: width_sp > 420.0,
        }
    }
}

/// What the bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Cpu,
    Memory,
}

impl Metric {
    /// The list sorted by memory shows memory; every other order shows CPU.
    pub fn of(key: SortKey) -> Self {
        match key {
            SortKey::Memory => Metric::Memory,
            SortKey::State
            | SortKey::Cpu
            | SortKey::Name
            | SortKey::Recommendation
            | SortKey::Spend
            | SortKey::SinceStart
            | SortKey::Context => Metric::Cpu,
        }
    }
}

/// Bar fraction for CPU in % of one core: full at 100 %, `None` without a
/// sample (-1). Above one core the bar stays full and the text tells.
pub fn cpu_fraction(cpu: f64) -> Option<f64> {
    gauge_frac(cpu)
}

/// Bar fractions `(pss, swap)` for memory: the session's PSS and its swapped
/// part (`mem - pss`, where `mem` is PSS + SwapPss) over `scale`, the largest
/// `mem` of the list. `None` without a reading or without a scale. The sum
/// never passes 1, even with a scale from an older tick.
pub fn mem_fractions(pss: i64, mem: i64, scale: i64) -> Option<(f64, f64)> {
    if mem < 0 || scale <= 0 {
        return None;
    }
    let pss = pss.clamp(0, mem);
    let scale = scale as f64;
    let p = (pss as f64 / scale).min(1.0);
    let s = ((mem - pss) as f64 / scale).min(1.0 - p);
    Some((p, s))
}

/// Scale of the memory bars: the largest PSS + swap among the sessions, 0
/// when none has a reading.
pub fn mem_scale(mems: impl IntoIterator<Item = i64>) -> i64 {
    mems.into_iter().max().unwrap_or(0).max(0)
}

/// CPU next to the bar: whole percent with a space, `84 %`; `≥` when the
/// sample missed processes; a dash without a sample.
pub fn cpu_value(cpu: f64, partial: bool) -> String {
    if !(cpu.is_finite() && cpu >= 0.0) {
        return DASH.to_string();
    }
    format!("{}{cpu:.0} %", at_least(partial))
}

fn at_least(partial: bool) -> &'static str {
    if partial { "\u{2265}" } else { "" }
}

/// PSS + swap in GiB with three significant digits, `0.87 GiB`, `12.3 GiB`;
/// `≥` when the reading missed processes; a dash without a reading.
pub fn gib_value(bytes: i64, partial: bool) -> String {
    if bytes < 0 {
        return DASH.to_string();
    }
    let g = bytes as f64 / GIB;
    let n = if g < 9.995 {
        format!("{g:.2}")
    } else if g < 99.95 {
        format!("{g:.1}")
    } else {
        format!("{g:.0}")
    };
    format!("{}{n} GiB", at_least(partial))
}

/// `(value next to the bar, last column)` for the metric.
pub fn readout(
    metric: Metric,
    cpu: f64,
    cpu_partial: bool,
    mem: i64,
    mem_partial: bool,
) -> (String, String) {
    let (c, m) = (cpu_value(cpu, cpu_partial), gib_value(mem, mem_partial));
    match metric {
        Metric::Cpu => (c, m),
        Metric::Memory => (m, c),
    }
}

// ---- Shared state of the rows ------------------------------------------------

mod density {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Density)]
    pub struct Density {
        /// Set by the list's first breakpoint; the one-line row needs no
        /// change there.
        #[property(get, set)]
        compact: Cell<bool>,
        /// Narrow list (<= 420 sp): shorter bar, no context column.
        #[property(get, set)]
        narrow: Cell<bool>,
        /// <= 900 sp: the CPU column goes.
        #[property(get, set)]
        no_cpu: Cell<bool>,
        /// <= 560 sp: the share bar goes, its % stays.
        #[property(get, set)]
        pct_only: Cell<bool>,
        /// < 1000 sp: 44 px rows and the row menu always visible.
        #[property(get, set)]
        touch: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Density {
        const NAME: &'static str = "AgentCenterRowDensity";
        type Type = super::Density;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Density {}
}

glib::wrapper! {
    /// How much of the row fits the current width.
    pub struct Density(ObjectSubclass<density::Density>);
}

impl Default for Density {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Density {
    /// The columns this width keeps. A narrower flag implies the wider ones,
    /// so a list set `narrow` alone also drops the CPU and memory columns.
    pub fn columns(&self) -> Columns {
        let narrow = self.narrow();
        let pct_only = self.pct_only() || narrow;
        let compact = self.compact() || pct_only;
        let no_cpu = self.no_cpu() || compact;
        Columns {
            cpu: !no_cpu,
            memory: !compact,
            share_bar: !pct_only,
            context: !narrow,
        }
    }

    /// `f` now and after each flag that moves the columns.
    pub fn watch_columns(&self, f: impl Fn(Columns) + 'static) {
        f(self.columns());
        let f = Rc::new(f);
        for prop in ["compact", "narrow", "no-cpu", "pct-only"] {
            let f = f.clone();
            self.connect_notify_local(Some(prop), move |d, _| f(d.columns()));
        }
    }
}

mod row_metric {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::RowMetric)]
    pub struct RowMetric {
        /// The bars show memory (the list is sorted by memory).
        #[property(get, set)]
        memory: Cell<bool>,
        /// Largest PSS + swap of the list, in bytes ([`super::mem_scale`]).
        #[property(get, set)]
        scale: Cell<i64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowMetric {
        const NAME: &'static str = "AgentCenterRowMetric";
        type Type = super::RowMetric;
    }

    #[glib::derived_properties]
    impl ObjectImpl for RowMetric {}
}

glib::wrapper! {
    /// The list's metric and memory scale, one object bound to every row.
    pub struct RowMetric(ObjectSubclass<row_metric::RowMetric>);
}

impl Default for RowMetric {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl RowMetric {
    pub fn metric(&self) -> Metric {
        if self.memory() {
            Metric::Memory
        } else {
            Metric::Cpu
        }
    }

    /// Follows the list's order; notifies only on a change.
    pub fn follow(&self, key: SortKey) {
        let memory = Metric::of(key) == Metric::Memory;
        if self.memory() != memory {
            self.set_memory(memory);
        }
    }

    /// Scale from the sessions in `store`; notifies only on a change.
    pub fn rescale(&self, store: &gtk::gio::ListStore) {
        let scale = mem_scale(
            (0..store.n_items())
                .filter_map(|i| store.item(i).and_downcast::<SessionObject>())
                .map(|o| o.mem()),
        );
        if self.scale() != scale {
            self.set_scale(scale);
        }
    }
}

// ---- Hatch -------------------------------------------------------------------

mod hatch {
    use super::*;

    #[derive(Default)]
    pub struct Hatch;

    #[glib::object_subclass]
    impl ObjectSubclass for Hatch {
        const NAME: &'static str = "AgentCenterHatch";
        type Type = super::Hatch;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("hatch");
        }
    }

    impl ObjectImpl for Hatch {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_can_target(false);
            obj.set_accessible_role(gtk::AccessibleRole::Presentation);
            obj.set_overflow(gtk::Overflow::Hidden);
        }
    }

    impl WidgetImpl for Hatch {
        /// The CSS background (the state fill) is already drawn; the lines go
        /// on top in the node's own colour (white at the hatch opacity).
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
    /// Swapped-out part of a memory bar.
    pub struct Hatch(ObjectSubclass<hatch::Hatch>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

// ---- Metric bar --------------------------------------------------------------

mod metric_bar {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::MetricBar)]
    pub struct MetricBar {
        #[property(get, set)]
        cpu: Cell<f64>,
        #[property(get, set)]
        cpu_partial: Cell<bool>,
        #[property(get, set)]
        pss: Cell<i64>,
        #[property(get, set)]
        mem: Cell<i64>,
        #[property(get, set)]
        mem_partial: Cell<bool>,
        #[property(get, set)]
        group: Cell<u32>,
        #[property(get, set)]
        memory: Cell<bool>,
        #[property(get, set)]
        scale: Cell<i64>,
        #[property(get, set)]
        cpu_tooltip: RefCell<String>,
        #[property(get, set)]
        mem_text: RefCell<String>,
        pub fractions: Cell<Option<(f64, f64)>>,
        pub fill: RefCell<Option<gtk::Box>>,
        pub hatch: RefCell<Option<super::Hatch>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MetricBar {
        const NAME: &'static str = "AgentCenterMetricBar";
        type Type = super::MetricBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("metricbar");
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for MetricBar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_valign(gtk::Align::Center);
            obj.set_can_target(false);
            let fill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            fill.add_css_class("fill");
            fill.set_parent(&*obj);
            let hatch: super::Hatch = glib::Object::new();
            hatch.set_parent(&*obj);
            self.fill.replace(Some(fill));
            self.hatch.replace(Some(hatch));
        }

        fn notify(&self, pspec: &glib::ParamSpec) {
            self.parent_notify(pspec);
            // Only its own inputs: `css-classes` and the other widget
            // properties do not move the bar.
            if pspec.owner_type() == super::MetricBar::static_type() {
                self.obj().refresh();
            }
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

    impl WidgetImpl for MetricBar {
        fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            // Width from `width-request`, height from the CSS `min-height`.
            (0, 0, -1, -1)
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
    /// Bar of the row's metric: a track (CSS background), the PSS or CPU
    /// part (`metricbar > box.fill`) and the hatched swap part
    /// (`metricbar > hatch`), both in the state fill (`metricbar.busy` ...).
    pub struct MetricBar(ObjectSubclass<metric_bar::MetricBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MetricBar {
    fn new() -> Self {
        glib::Object::new()
    }

    pub fn metric(&self) -> Metric {
        if self.memory() {
            Metric::Memory
        } else {
            Metric::Cpu
        }
    }

    /// `(pss or cpu part, swap part)` as drawn; `None` is the bare track.
    pub fn fractions(&self) -> Option<(f64, f64)> {
        self.imp().fractions.get()
    }

    /// `(value next to the bar, last column)`.
    pub fn readout(&self) -> (String, String) {
        readout(
            self.metric(),
            self.cpu(),
            self.cpu_partial(),
            self.mem(),
            self.mem_partial(),
        )
    }

    /// The tooltip of the bar's column, for the metric shown.
    pub fn tooltip(&self) -> String {
        match self.metric() {
            Metric::Cpu => self.cpu_tooltip(),
            Metric::Memory => format!("Memory (PSS + swap): {}", self.mem_text()),
        }
    }

    /// Recomputes the fractions from the properties; reallocates only when
    /// they moved.
    fn refresh(&self) {
        let fractions = match self.metric() {
            Metric::Cpu => cpu_fraction(self.cpu()).map(|f| (f, 0.0)),
            Metric::Memory => mem_fractions(self.pss(), self.mem(), self.scale()),
        };
        if self.imp().fractions.replace(fractions) != fractions {
            self.queue_allocate();
        }
        let (_, tone) = glyph(self.group());
        if !self.has_css_class(tone) {
            self.set_css_classes(&[tone]);
        }
    }
}

// ---- Row ---------------------------------------------------------------------

fn bind(item: &gtk::ListItem, prop: &str, target: &impl IsA<glib::Object>, target_prop: &str) {
    item.property_expression("item")
        .chain_property::<SessionObject>(prop)
        .bind(target, target_prop, gtk::Widget::NONE);
}

/// Binds `target_prop` to whether the item's text property starts with `≥`
/// (a reading that missed processes, see `format.rs`).
fn bind_partial(item: &gtk::ListItem, prop: &str, target: &impl IsA<glib::Object>, tp: &str) {
    item.property_expression("item")
        .chain_property::<SessionObject>(prop)
        .chain_closure_with_callback(|args| {
            args[1]
                .get::<String>()
                .is_ok_and(|s| s.starts_with('\u{2265}'))
        })
        .bind(target, tp, gtk::Widget::NONE);
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn is_agent(g: u32) -> bool {
    Group::from_rank(g) != Group::Multiplexer
}

/// Builds the row in the list item: rank, glyph, name with the cockpit
/// reason, the 30-minute share, context %, the CPU and memory figures, the
/// Action button and the session menu.
pub fn setup(item: &gtk::ListItem, density: &Density, metric: &RowMetric, handlers: &RowHandlers) {
    // The glyph follows the bar's `group` (see `sync` below).
    let glyph = StateGlyph::new(GLYPH_PX);

    // Rank in the current order, `#1`; empty before the first sort.
    let rank = label(&["row-rank"]);
    rank.set_xalign(1.0);
    rank.set_width_chars(3);
    item.property_expression("item")
        .chain_property::<SessionObject>("rank")
        .chain_closure_with_callback(|args| {
            super::reorder::rank_text(args[1].get::<u32>().unwrap_or(0))
        })
        .bind(&rank, "label", gtk::Widget::NONE);

    let name = label(&["row-name"]);
    name.set_ellipsize(pango::EllipsizeMode::End);
    name.set_xalign(0.0);
    name.set_width_chars(1);
    bind(item, "name", &name, "label");
    bind(item, "name", &name, "tooltip-text");
    // Why the cockpit asks for the action, under the name.
    let reason = label(&["row-reason"]);
    reason.set_ellipsize(pango::EllipsizeMode::End);
    reason.set_xalign(0.0);
    reason.set_width_chars(1);
    bind(item, "reason", &reason, "label");
    item.property_expression("item")
        .chain_property::<SessionObject>("reason")
        .chain_closure_with_callback(|args| !args[1].get::<String>().unwrap_or_default().is_empty())
        .bind(&reason, "visible", gtk::Widget::NONE);
    let namebox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    namebox.set_hexpand(true);
    namebox.set_valign(gtk::Align::Center);
    namebox.append(&name);
    namebox.append(&reason);

    // The 30-minute share: a token sparkline (tokens per minute over the
    // last 30 minutes), `34 %`, `2.1k/min`; `no transcript` alone when the
    // spend is unknown. The sparkline replaces the old share bar.
    let points: Rc<RefCell<Vec<u64>>> = Rc::default();
    let said: Rc<RefCell<String>> = Rc::default();
    let track = gtk::DrawingArea::new();
    track.add_css_class("row-spark");
    track.set_content_width(SHARE_BAR_PX);
    track.set_content_height(SPARK_HEIGHT_PX);
    track.set_valign(gtk::Align::Center);
    track.set_accessible_role(gtk::AccessibleRole::Img);
    {
        let points = points.clone();
        track.set_draw_func(move |area, cr, w, h| {
            let c = area.color();
            draw_spark(cr, &points.borrow(), f64::from(w), f64::from(h), &c);
        });
    }
    let share = label(&["row-share"]);
    share.set_xalign(1.0);
    share.set_ellipsize(pango::EllipsizeMode::End);
    share.set_hexpand(true);
    share.set_width_chars(1);
    let permin = label(&["row-permin"]);
    permin.set_xalign(1.0);
    let sharecell = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    sharecell.add_css_class("row-sharecell");
    sharecell.set_size_request(SHARE_CELL_PX, -1);
    sharecell.append(&track);
    sharecell.append(&share);
    sharecell.append(&permin);
    let paint = {
        let (points, said) = (points.clone(), said.clone());
        let (share, permin, cell, track) = (
            share.downgrade(),
            permin.downgrade(),
            sharecell.downgrade(),
            track.downgrade(),
        );
        let density = density.downgrade();
        let item = item.downgrade();
        move || {
            let (Some(share), Some(permin), Some(cell), Some(track)) = (
                share.upgrade(),
                permin.upgrade(),
                cell.upgrade(),
                track.upgrade(),
            ) else {
                return;
            };
            let (Some(item), Some(density)) = (item.upgrade(), density.upgrade()) else {
                return;
            };
            let Some(o) = item.item().and_downcast::<SessionObject>() else {
                return;
            };
            let known = |v: f64| (v >= 0.0).then_some(v);
            let (sh, pm) = (known(o.share()), known(o.per_min()));
            // With the bar gone (<= 560 sp) the `%` keeps a bar's width.
            let bar_on = density.columns().share_bar;
            track.set_visible(bar_on);
            cell.set_size_request(if bar_on { SHARE_CELL_PX } else { SHARE_BAR_PX }, -1);
            // The series moves once a minute while this runs on every tick
            // of the share: touch the widget only when its words change.
            let series = parse_spark(&o.spark());
            let words = spark_words(&series, o.spend() < 0);
            if *said.borrow() != words {
                track.set_tooltip_text(Some(&words));
                track.update_property(&[gtk::accessible::Property::Label(&words)]);
                if series.is_empty() {
                    track.add_css_class("spark-empty");
                } else {
                    track.remove_css_class("spark-empty");
                }
                *points.borrow_mut() = series;
                track.queue_draw();
                *said.borrow_mut() = words;
            } else if *points.borrow() != series {
                *points.borrow_mut() = series;
                track.queue_draw();
            }
            share.set_label(&share_text(sh));
            let rate = per_min_text(pm);
            permin.set_label(&rate);
            permin.set_visible(bar_on && sh.is_some() && !rate.is_empty());
            let tip = match sh {
                Some(_) => format!("{} of the last 30 minutes' spend, {}", share.label(), rate),
                None => "No transcript: the spend of this session is unknown".to_string(),
            };
            cell.set_tooltip_text(Some(&tip));
        }
    };
    let paint = Rc::new(paint);
    for prop in ["share", "per-min", "spark", "spend"] {
        let paint = paint.clone();
        item.property_expression("item")
            .chain_property::<SessionObject>(prop)
            .watch(gtk::Widget::NONE, move || paint());
    }
    // A recycled row is another session: item changes repaint too.
    {
        let paint = paint.clone();
        item.connect_item_notify(move |_| paint());
    }

    // Context % in its own column; it leaves at <= 420 sp but stays in the
    // accessible label of the row.
    let ctx = label(&["row-ctx"]);
    ctx.set_xalign(1.0);
    ctx.set_size_request(CTX_CELL_PX, -1);
    bind(item, "ctx-text", &ctx, "label");
    bind(item, "ctx-tooltip", &ctx, "tooltip-text");

    let bar = MetricBar::new();
    for (from, to) in [
        ("cpu", "cpu"),
        ("pss", "pss"),
        ("mem", "mem"),
        ("group", "group"),
        ("cpu-tooltip", "cpu-tooltip"),
        ("mem-text", "mem-text"),
    ] {
        bind(item, from, &bar, to);
    }
    bind_partial(item, "cpu-text", &bar, "cpu-partial");
    bind_partial(item, "mem-text", &bar, "mem-partial");
    metric
        .bind_property("memory", &bar, "memory")
        .sync_create()
        .build();
    metric
        .bind_property("scale", &bar, "scale")
        .sync_create()
        .build();
    density
        .bind_property("narrow", &bar, "width-request")
        .transform_to(|_, narrow: bool| Some(if narrow { NARROW_BAR_PX } else { BAR_PX }))
        .sync_create()
        .build();

    let value = label(&["row-value"]);
    value.set_xalign(1.0);
    let other = label(&["row-other"]);
    other.set_xalign(1.0);
    // CPU and memory are the bar and the last column in one order and the
    // other way round in the other; the classes follow the metric.
    let lead = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    lead.append(&bar);
    lead.append(&value);
    let figures = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    figures.add_css_class("row-figures");
    figures.append(&lead);
    figures.append(&other);
    let place_columns = {
        let (lead, other) = (lead.downgrade(), other.downgrade());
        let (density, metric) = (density.downgrade(), metric.downgrade());
        move || {
            let (Some(lead), Some(other), Some(density), Some(metric)) = (
                lead.upgrade(),
                other.upgrade(),
                density.upgrade(),
                metric.upgrade(),
            ) else {
                return;
            };
            let c = density.columns();
            let memory = metric.metric() == Metric::Memory;
            let (lead_class, other_class) = if memory {
                ("col-mem", "col-cpu")
            } else {
                ("col-cpu", "col-mem")
            };
            for (w, add, drop) in [
                (lead.upcast_ref::<gtk::Widget>(), lead_class, other_class),
                (other.upcast_ref(), other_class, lead_class),
            ] {
                w.remove_css_class(drop);
                w.add_css_class(add);
            }
            let (lead_on, other_on) = if memory {
                (c.memory, c.cpu)
            } else {
                (c.cpu, c.memory)
            };
            lead.set_visible(lead_on);
            other.set_visible(other_on);
        }
    };
    let place_columns = Rc::new(place_columns);
    {
        let pc = place_columns.clone();
        density.watch_columns(move |_| pc());
        let pc = place_columns.clone();
        metric.connect_memory_notify(move |_| pc());
    }
    {
        let (ctx, paint) = (ctx.downgrade(), paint.clone());
        density.watch_columns(move |c| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_visible(c.context);
            }
            paint();
        });
    }

    // The labels follow the bar, which holds every input of the readout.
    let (v, o, f, gl) = (
        value.downgrade(),
        other.downgrade(),
        figures.downgrade(),
        glyph.downgrade(),
    );
    let sync = move |bar: &MetricBar| {
        let (Some(v), Some(o), Some(f), Some(gl)) =
            (v.upgrade(), o.upgrade(), f.upgrade(), gl.upgrade())
        else {
            return;
        };
        let (text, rest) = bar.readout();
        v.set_label(&text);
        o.set_label(&rest);
        // Fixed columns, so the bars line up from row to row.
        let (vw, ow) = match bar.metric() {
            Metric::Cpu => (5, 8),
            Metric::Memory => (8, 5),
        };
        v.set_width_chars(vw);
        o.set_width_chars(ow);
        let tip = bar.tooltip();
        if f.tooltip_text().as_deref() != Some(tip.as_str()) {
            f.set_tooltip_text(Some(&tip));
        }
        gl.show(bar.group());
    };
    sync(&bar);
    bar.connect_notify_local(None, move |bar, _| sync(bar));

    // Session actions (C12): the menu reads the row's session when it opens,
    // because GTK recycles the row for another session. The slot keeps the
    // column when a multiplexer row hides the button.
    let weak = item.downgrade();
    let actions = actions_ui::menu_button("Session actions", move || {
        let o = weak.upgrade()?.item().and_downcast::<SessionObject>()?;
        Some((Target::Session(o.pid()), o.name()))
    });
    item.property_expression("item")
        .chain_property::<SessionObject>("group")
        .chain_closure_with_callback(|args| is_agent(args[1].get::<u32>().unwrap_or_default()))
        .bind(&actions, "visible", gtk::Widget::NONE);
    let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    slot.set_size_request(MENU_CELL_PX, -1);
    slot.append(&actions);

    // The Action column: one button for the verdict, nothing for OK.
    let action = action_button(item, handlers);
    let action_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    action_slot.add_css_class("row-action-slot");
    action_slot.set_size_request(ACTION_CELL_PX, -1);
    action_slot.set_valign(gtk::Align::Center);
    // An explicit false: the button's hexpand must not spread the slot.
    action_slot.set_hexpand(false);
    action_slot.append(&action);

    let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    root.add_css_class("session-row");
    root.append(&rank);
    root.append(&glyph);
    root.append(&namebox);
    root.append(&sharecell);
    root.append(&ctx);
    root.append(&figures);
    root.append(&action_slot);
    root.append(&slot);
    item.set_child(Some(&root));
    // The screen reader hears the whole row in one sentence.
    bind(item, "a11y-label", item, "accessible-label");
}

/// The Action button of a row: flat, outlined, 16 px icon and a 12 px
/// semibold label, fixed to the 96 px column. It reads the row's session when
/// clicked, because GTK recycles the row for another one.
fn action_button(item: &gtk::ListItem, handlers: &RowHandlers) -> gtk::Button {
    let icon = gtk::Image::new();
    icon.set_pixel_size(16);
    let text = label(&["row-action-label"]);
    text.set_ellipsize(pango::EllipsizeMode::End);
    // The natural width stays 1 char, so the 96 px column is not outgrown.
    text.set_width_chars(1);
    text.set_max_width_chars(1);
    text.set_hexpand(true);
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    inner.append(&icon);
    inner.append(&text);
    let b = gtk::Button::new();
    b.set_child(Some(&inner));
    b.add_css_class("flat");
    b.add_css_class("row-action");
    b.set_hexpand(true);
    b.set_valign(gtk::Align::Center);
    let op_of = |args: &[glib::Value]| RowOp::for_rec(args[1].get::<u32>().unwrap_or_default());
    let rec = || {
        item.property_expression("item")
            .chain_property::<SessionObject>("rec")
    };
    rec()
        .chain_closure_with_callback(move |args| op_of(args).is_some())
        .bind(&b, "visible", gtk::Widget::NONE);
    rec()
        .chain_closure_with_callback(move |args| {
            op_of(args).map_or(String::new(), |o| o.label().to_string())
        })
        .bind(&text, "label", gtk::Widget::NONE);
    rec()
        .chain_closure_with_callback(move |args| {
            op_of(args).map_or(String::new(), |o| o.label().to_string())
        })
        .bind(&b, "tooltip-text", gtk::Widget::NONE);
    rec()
        .chain_closure_with_callback(move |args| {
            op_of(args).map_or(String::new(), |o| o.icon().to_string())
        })
        .bind(&icon, "icon-name", gtk::Widget::NONE);
    // `accessible-label` is a property of the list item, not of a widget, so
    // binding it on the button fails (a null watch aborts the setup): set the
    // ATK-style property from a watch instead.
    {
        let weak = b.downgrade();
        let weak_item = item.downgrade();
        let apply = move || {
            let (Some(b), Some(item)) = (weak.upgrade(), weak_item.upgrade()) else {
                return;
            };
            let rec = item
                .item()
                .and_downcast::<SessionObject>()
                .map_or(0, |o| o.rec());
            let text = RowOp::for_rec(rec).map_or("", |o| o.label());
            b.update_property(&[gtk::accessible::Property::Label(text)]);
        };
        rec().watch(gtk::Widget::NONE, apply.clone());
        item.connect_item_notify(move |_| apply());
    }
    let (weak, handlers) = (item.downgrade(), handlers.clone());
    b.connect_clicked(move |_| {
        let Some(o) = weak
            .upgrade()
            .and_then(|i| i.item().and_downcast::<SessionObject>())
        else {
            return;
        };
        let Some(op) = RowOp::for_rec(o.rec()) else {
            return;
        };
        // A listener may add listeners: run a copy.
        let all: Vec<_> = handlers.borrow().clone();
        for f in all {
            f(o.pid(), op);
        }
    });
    b
}

/// The scale of the CPU bar, `0 50 100 %`, above the list. It repeats the
/// row's column structure (bar, value, last column, menu slot) so its scale
/// sits exactly over the bars: the placeholders take the width of the
/// labels they stand for.
pub fn axis(density: &Density) -> gtk::Box {
    let scale = gtk::CenterBox::new();
    scale.add_css_class("axis-scale");
    scale.set_valign(gtk::Align::Center);
    for (text, set) in [
        (
            "0",
            gtk::CenterBox::set_start_widget as fn(&gtk::CenterBox, Option<&gtk::Label>),
        ),
        ("50", gtk::CenterBox::set_center_widget),
        ("100 %", gtk::CenterBox::set_end_widget),
    ] {
        let l = gtk::Label::new(Some(text));
        l.add_css_class("axis-tick");
        // The 72 px bar has no room for three ticks.
        if text == "50" {
            density
                .bind_property("narrow", &l, "visible")
                .invert_boolean()
                .sync_create()
                .build();
        }
        set(&scale, Some(&l));
    }
    density
        .bind_property("narrow", &scale, "width-request")
        .transform_to(|_, narrow: bool| Some(if narrow { NARROW_BAR_PX } else { BAR_PX }))
        .sync_create()
        .build();
    let value = gtk::Label::new(None);
    value.add_css_class("axis-value");
    value.set_width_chars(5);
    let other = gtk::Label::new(None);
    other.add_css_class("axis-other");
    other.set_width_chars(8);
    let figures = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    figures.append(&scale);
    figures.append(&value);
    figures.append(&other);
    // The scale belongs to the CPU bar: it goes with the CPU column, the
    // placeholder of the memory column with the memory one.
    {
        let (scale, value, other) = (scale.downgrade(), value.downgrade(), other.downgrade());
        density.watch_columns(move |c| {
            if let (Some(scale), Some(value), Some(other)) =
                (scale.upgrade(), value.upgrade(), other.upgrade())
            {
                scale.set_visible(c.cpu);
                value.set_visible(c.cpu);
                other.set_visible(c.memory);
            }
        });
    }
    // Placeholders of the columns between the name and the bars, and after.
    let spaced = |px: i32| {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        b.set_size_request(px, -1);
        b
    };
    let share = spaced(SHARE_CELL_PX);
    let ctx = spaced(CTX_CELL_PX);
    {
        let (share, ctx) = (share.downgrade(), ctx.downgrade());
        density.watch_columns(move |c| {
            if let (Some(share), Some(ctx)) = (share.upgrade(), ctx.upgrade()) {
                share.set_size_request(
                    if c.share_bar {
                        SHARE_CELL_PX
                    } else {
                        SHARE_BAR_PX
                    },
                    -1,
                );
                ctx.set_visible(c.context);
            }
        });
    }
    let action = spaced(ACTION_CELL_PX);
    let slot = spaced(MENU_CELL_PX);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let axis = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    axis.add_css_class("list-axis");
    axis.set_accessible_role(gtk::AccessibleRole::Presentation);
    axis.append(&spacer);
    axis.append(&share);
    axis.append(&ctx);
    axis.append(&figures);
    axis.append(&action);
    axis.append(&slot);
    axis
}

/// Section header: title and count, `Blocked · 3`. The title comes from
/// `title(first item)`; the count is the `GtkListHeader`'s own.
pub fn setup_header(
    header: &gtk::ListHeader,
    title: impl Fn(&SessionObject) -> (String, bool) + 'static,
) {
    let l = label(&["session-section"]);
    l.set_accessible_role(gtk::AccessibleRole::Heading);
    header.set_child(Some(&l));
    let refresh = move |h: &gtk::ListHeader| {
        let Some(obj) = h.item().and_downcast::<SessionObject>() else {
            return;
        };
        let (t, alert) = title(&obj);
        let text = format!("{t} · {}", h.n_items());
        if l.label() != text {
            l.set_label(&text);
        }
        if alert != l.has_css_class("alert") {
            if alert {
                l.add_css_class("alert");
            } else {
                l.remove_css_class("alert");
            }
        }
    };
    let r = std::rc::Rc::new(refresh);
    let r2 = r.clone();
    header.connect_item_notify(move |h| r(h));
    header.connect_n_items_notify(move |h| r2(h));
}

/// Series of the `spark` property: tokens per minute, comma separated.
fn parse_spark(text: &str) -> Vec<u64> {
    text.split(',').filter_map(|v| v.parse().ok()).collect()
}

/// What the sparkline shows, in words, for the tooltip and the screen
/// reader. An empty series says why it draws nothing; it never claims zero.
fn spark_words(series: &[u64], unknown: bool) -> String {
    let (Some(&last), Some(&peak)) = (series.last(), series.iter().max()) else {
        return if unknown {
            "No token history: no transcript for this session".to_string()
        } else {
            "No token history yet".to_string()
        };
    };
    let rate = |v: u64| per_min_text(Some(v as f64));
    format!(
        "Tokens per minute, last 30 minutes: peak {}, now {}",
        rate(peak),
        rate(last)
    )
}

/// The line and the area under it, oldest point left. A short series sits
/// at the right edge: the minutes before it are unknown, not zero.
fn draw_spark(cr: &gtk::cairo::Context, series: &[u64], w: f64, h: f64, c: &gtk::gdk::RGBA) {
    if series.is_empty() {
        return;
    }
    let peak = series.iter().copied().max().unwrap_or(0).max(1) as f64;
    let step = w / (crate::spend::HISTORY_POINTS as f64 - 1.0);
    let pad = 1.5;
    let at = |i: usize, v: u64| {
        let x = w - (series.len() - 1 - i) as f64 * step;
        (x, h - pad - (h - 2.0 * pad) * v as f64 / peak)
    };
    let mut pts: Vec<(f64, f64)> = series.iter().enumerate().map(|(i, &v)| at(i, v)).collect();
    if pts.len() == 1 {
        // One minute so far: a short level stroke instead of a bare dot.
        pts.insert(0, (pts[0].0 - step, pts[0].1));
    }
    let rgb = (
        f64::from(c.red()),
        f64::from(c.green()),
        f64::from(c.blue()),
    );
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    cr.move_to(first.0, h);
    for &(x, y) in &pts {
        cr.line_to(x, y);
    }
    cr.line_to(last.0, h);
    cr.close_path();
    let g = gtk::cairo::LinearGradient::new(0.0, 0.0, 0.0, h);
    g.add_color_stop_rgba(0.0, rgb.0, rgb.1, rgb.2, 0.6);
    g.add_color_stop_rgba(1.0, rgb.0, rgb.1, rgb.2, 0.05);
    let _ = cr.set_source(&g);
    let _ = cr.fill();
    cr.move_to(first.0, first.1);
    for &(x, y) in &pts[1..] {
        cr.line_to(x, y);
    }
    cr.set_source_rgb(rgb.0, rgb.1, rgb.2);
    cr.set_line_width(1.3);
    let _ = cr.stroke();
}

#[cfg(test)]
mod tests {
    use super::super::{SessionList, SessionModel};
    use super::*;
    use crate::state_glyph::Shape;
    use agentcenter_core::Snapshot;

    const DASH_S: &str = "\u{2014}";

    #[test]
    fn cpu_fraction_fills_at_one_core_and_has_no_bar_without_a_sample() {
        assert_eq!(cpu_fraction(50.0), Some(0.5));
        assert_eq!(cpu_fraction(0.0), Some(0.0));
        assert_eq!(cpu_fraction(100.0), Some(1.0));
        assert_eq!(cpu_fraction(250.0), Some(1.0), "clamped at one core");
        assert_eq!(cpu_fraction(-1.0), None);
        assert_eq!(cpu_fraction(f64::NAN), None);
    }

    #[test]
    fn mem_fractions_split_the_swap_and_scale_to_the_largest_session() {
        let g = 1 << 30;
        let (p, s) = mem_fractions(g, 3 * g / 2, 3 * g).unwrap();
        assert!((p - 1.0 / 3.0).abs() < 1e-9 && (s - 1.0 / 6.0).abs() < 1e-9);
        // The largest session fills the bar.
        let (p, s) = mem_fractions(2 * g, 3 * g, 3 * g).unwrap();
        assert!((p + s - 1.0).abs() < 1e-9 && (s - 1.0 / 3.0).abs() < 1e-9);
        // No PSS reading but a swap one: all of it is swap.
        assert_eq!(mem_fractions(-1, g, 2 * g), Some((0.0, 0.5)));
        // A scale from an older tick never overflows the bar.
        let (p, s) = mem_fractions(3 * g, 4 * g, 2 * g).unwrap();
        assert_eq!((p, s), (1.0, 0.0));
        let (p, s) = mem_fractions(g, 4 * g, 2 * g).unwrap();
        assert!((p - 0.5).abs() < 1e-9 && (s - 0.5).abs() < 1e-9, "{p} {s}");
        assert_eq!(mem_fractions(g, -1, 2 * g), None, "no reading");
        assert_eq!(mem_fractions(g, g, 0), None, "no scale");
    }

    #[test]
    fn mem_scale_is_the_largest_session() {
        assert_eq!(mem_scale([1, -1, 5, 3]), 5);
        assert_eq!(mem_scale([-1, -1]), 0);
        assert_eq!(mem_scale([]), 0);
    }

    #[test]
    fn values_are_whole_percent_and_three_digit_gib() {
        assert_eq!(cpu_value(84.4, false), "84 %");
        assert_eq!(cpu_value(7.48, false), "7 %");
        assert_eq!(cpu_value(250.0, false), "250 %");
        assert_eq!(cpu_value(12.0, true), "\u{2265}12 %");
        assert_eq!(cpu_value(-1.0, false), DASH_S);
        let g = (1u64 << 30) as f64;
        assert_eq!(gib_value((0.87 * g) as i64, false), "0.87 GiB");
        assert_eq!(gib_value((12.34 * g) as i64, false), "12.3 GiB");
        assert_eq!(gib_value((123.4 * g) as i64, false), "123 GiB");
        assert_eq!(gib_value(5 << 20, false), "0.00 GiB");
        assert_eq!(gib_value((1.5 * g) as i64, true), "\u{2265}1.50 GiB");
        assert_eq!(gib_value(-1, false), DASH_S);
    }

    #[test]
    fn readout_puts_the_bar_metric_next_to_the_bar() {
        let g = (1u64 << 30) as f64;
        let mem = (0.87 * g) as i64;
        assert_eq!(
            readout(Metric::Cpu, 84.0, false, mem, false),
            ("84 %".to_string(), "0.87 GiB".to_string())
        );
        assert_eq!(
            readout(Metric::Memory, 84.0, false, mem, false),
            ("0.87 GiB".to_string(), "84 %".to_string())
        );
    }

    #[test]
    fn glyph_shape_and_tone_follow_the_state() {
        let of = |g: Group| glyph(g.rank());
        assert_eq!(of(Group::Busy), (Shape::Disc, "busy"));
        assert_eq!(of(Group::Blocked), (Shape::Triangle, "waiting"));
        assert_eq!(of(Group::Idle), (Shape::Ring, "idle"));
        assert_eq!(of(Group::Multiplexer), (Shape::Grid, "mux"));
        assert_eq!(of(Group::Unknown), (Shape::Ring, "unknown"));
    }

    #[test]
    fn only_the_memory_order_shows_memory() {
        for k in SortKey::ALL {
            let want = if k == SortKey::Memory {
                Metric::Memory
            } else {
                Metric::Cpu
            };
            assert_eq!(Metric::of(k), want, "{k:?}");
        }
    }

    // ---- GTK ---------------------------------------------------------------

    fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut out = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(w) = child {
            out.extend(descendants(&w));
            child = w.next_sibling();
        }
        out
    }

    fn settle() {
        let ctx = glib::MainContext::default();
        for _ in 0..40 {
            while ctx.pending() {
                ctx.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn fixture(name: &str) -> Snapshot {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/snapshots")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
    }

    fn load_css() {
        let css = gtk::CssProvider::new();
        css.load_from_string(concat!(
            include_str!("../resources/tokens.css"),
            include_str!("../resources/sessions.css"),
        ));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    /// The `row` widgets of a list view and the row's own box in each.
    fn rows(view: &gtk::ListView) -> Vec<(gtk::Widget, gtk::Widget)> {
        descendants(view.upcast_ref())
            .into_iter()
            .filter(|w| w.css_name() == "row")
            .filter_map(|r| {
                let inner = descendants(&r)
                    .into_iter()
                    .find(|w| w.has_css_class("session-row"))?;
                Some((r, inner))
            })
            .collect()
    }

    fn find<T: IsA<gtk::Widget>>(row: &gtk::Widget, class: Option<&str>) -> Option<T> {
        descendants(row)
            .into_iter()
            .filter(|w| class.is_none_or(|c| w.has_css_class(c)))
            .find_map(|w| w.downcast::<T>().ok())
    }

    fn model_of(names: &[&str]) -> SessionModel {
        let mut m = SessionModel::default();
        for n in names {
            let snap = fixture(n);
            m.apply(&snap, None);
        }
        m
    }

    /// One line of 34 px per session, the whole sentence as the accessible
    /// label, the name ellipsized with the full name in the tooltip, the
    /// glyph of the state, and a row that fits the narrow list.
    #[test]
    fn one_line_rows_fit_label_and_ellipsize() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            load_css();
            let mut objs = Vec::new();
            // Long names first: the Broadway screen caps the window height,
            // so only the top rows are drawn and measured.
            for n in ["nomes-longos", "tipico-40"] {
                let m = model_of(&[n]);
                for i in 0..m.store().n_items() {
                    objs.push(m.store().item(i).and_downcast::<SessionObject>().unwrap());
                }
            }
            let store = gtk::gio::ListStore::new::<SessionObject>();
            store.extend_from_slice(&objs);
            let density = Density::default();
            let metric = RowMetric::default();
            metric.rescale(&store);
            let items: std::rc::Rc<std::cell::RefCell<Vec<gtk::ListItem>>> = Default::default();
            let factory = gtk::SignalListItemFactory::new();
            let (d, mt, its) = (density.clone(), metric.clone(), items.clone());
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                setup(item, &d, &mt, &Default::default());
                its.borrow_mut().push(item.clone());
            });
            let view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(store))), Some(factory));
            view.add_css_class("session-rack");
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
            scroll.set_child(Some(&view));
            let win = gtk::Window::new();
            win.set_default_size(560, 3000);
            win.set_child(Some(&scroll));
            win.present();
            settle();

            let rs = rows(&view);
            assert!(rs.len() >= 40, "{} rows realized", rs.len());
            let shown: Vec<_> = rs.iter().filter(|(r, _)| r.is_drawable()).collect();
            // The Broadway screen caps the window height.
            assert!(shown.len() >= 15, "{} rows on screen", shown.len());
            for (row, _) in shown {
                assert_eq!(row.height(), ROW_HEIGHT_PX, "one line, 40 px");
            }
            let mut a11y = 0;
            for item in items.borrow().iter() {
                let Some(o) = item.item().and_downcast::<SessionObject>() else {
                    continue;
                };
                let label: String = item.property("accessible-label");
                assert_eq!(label, o.a11y_label());
                a11y += 1;
                let row = item.child().expect("row");
                let name = find::<gtk::Label>(&row, Some("row-name")).expect("name");
                assert_eq!(name.label(), o.name());
                assert_eq!(name.tooltip_text().as_deref(), Some(o.name().as_str()));
                assert_eq!(name.ellipsize(), pango::EllipsizeMode::End);
                let g = find::<StateGlyph>(&row, None).expect("glyph");
                assert_eq!(g.shape(), glyph(o.group()).0, "{}", o.name());
                assert!(g.has_css_class(glyph(o.group()).1));
                let bar = find::<MetricBar>(&row, None).expect("bar");
                assert_eq!(bar.metric(), Metric::Cpu);
                assert_eq!(bar.fractions(), cpu_fraction(o.cpu()).map(|f| (f, 0.0)));
                let value = find::<gtk::Label>(&row, Some("row-value")).expect("value");
                assert_eq!(
                    value.label(),
                    cpu_value(o.cpu(), o.cpu_text().starts_with('\u{2265}'))
                );
                if bar.is_drawable() {
                    assert_eq!(bar.width(), BAR_PX);
                }
            }
            assert!(a11y >= 40);
            let ellipsized = rs
                .iter()
                .filter_map(|(r, _)| find::<gtk::Label>(r, Some("row-name")))
                .filter(|l| l.is_drawable() && l.layout().is_ellipsized())
                .count();
            assert!(ellipsized >= 1, "the long names are cut, not overflowing");

            // Narrow list: the row still fits the 360 px window (12 px of
            // row padding on each side).
            density.set_narrow(true);
            settle();
            let widest = rs
                .iter()
                .map(|(_, inner)| inner.measure(gtk::Orientation::Horizontal, -1).0)
                .max()
                .unwrap_or(0);
            assert!(widest <= 360 - 24, "narrow row needs {widest}px");
            win.destroy();
        });
    }

    /// The bar follows the list's order: CPU in the state fill, then memory
    /// with the hatched swap against the largest session, and back.
    #[test]
    fn bars_switch_metric_with_the_sort_key() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            load_css();
            let mut model = model_of(&[]);
            model.apply(&fixture("tipico-40"), None);
            let list = SessionList::new(&model);
            list.update(&model.apply(&fixture("tipico-40"), None));
            let win = gtk::Window::new();
            win.set_default_size(900, 3000);
            win.set_child(Some(&super::super::scrolled(&list)));
            win.present();
            settle();
            let scale = mem_scale(
                (0..model.store().n_items())
                    .filter_map(|i| model.store().item(i).and_downcast::<SessionObject>())
                    .map(|o| o.mem()),
            );
            assert!(scale > 0);
            let root = super::super::scrolled(&list);
            let check = |metric: Metric| {
                // Broadway without a browser does not advance the frame
                // clock: allocate here so the bars that moved get their parts.
                root.allocate(root.width(), root.height(), -1, None);
                let mut swapped = 0;
                let rs = rows(&list.view);
                assert!(rs.len() >= 20, "{} rows", rs.len());
                for (row, _) in rs {
                    let bar = find::<MetricBar>(&row, None).expect("bar");
                    assert_eq!(bar.metric(), metric);
                    // The bar's inputs are bound from the item (checked
                    // against the objects in the test above).
                    assert_eq!(bar.scale(), scale);
                    let want = match metric {
                        Metric::Cpu => cpu_fraction(bar.cpu()).map(|f| (f, 0.0)),
                        Metric::Memory => mem_fractions(bar.pss(), bar.mem(), scale),
                    };
                    assert_eq!(bar.fractions(), want);
                    let value = find::<gtk::Label>(&row, Some("row-value")).unwrap();
                    let unit = if metric == Metric::Memory { "GiB" } else { "%" };
                    assert!(value.label().ends_with(unit), "{}", value.label());
                    let hatch = find::<Hatch>(&row, None).unwrap();
                    if bar.is_drawable() && bar.fractions().is_some_and(|(_, s)| s > 0.0) {
                        swapped += 1;
                        let w = f64::from(bar.width());
                        let (p, sw) = bar.fractions().unwrap();
                        let want = ((p + sw) * w).round() - (p * w).round();
                        assert_eq!(f64::from(hatch.width()), want, "hatched part");
                        let a = hatch.color().alpha();
                        assert!((0.2..0.5).contains(&a), "hatch alpha {a}");
                    }
                }
                swapped
            };
            assert_eq!(check(Metric::Cpu), 0, "no hatch in the CPU bar");
            list.set_sort_key(SortKey::Memory);
            settle();
            assert!(check(Metric::Memory) >= 10, "the fixture swaps");
            list.set_sort_key(SortKey::Name);
            settle();
            check(Metric::Cpu);
            win.destroy();
        });
    }

    // ---- Design fixes (direction B list) ---------------------------------------

    fn show(list: &SessionList, width: i32) -> gtk::Window {
        let win = gtk::Window::new();
        win.set_default_size(width, 3000);
        win.set_child(Some(&super::super::scrolled(list)));
        win.present();
        settle();
        win
    }

    fn labels_of(root: &impl IsA<gtk::Widget>) -> Vec<String> {
        descendants(root.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.label().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// The saved order defaults to the 30-minute spend (REVIEW_RR1): the list
    /// opens on that order with the menu on it, the rows still show the CPU
    /// bar, and the scale axis sits above them. The axis belongs to the CPU
    /// bar only.
    #[test]
    fn the_list_opens_on_spend_with_the_scale_axis_over_the_bars() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let list = SessionList::new(&SessionModel::default());
            assert_eq!(list.key.get(), SortKey::Spend);
            assert_eq!(list.sort_menu.selected(), 0);
            assert_eq!(labels_of(&list.axis), ["0", "50", "100 %"]);
            assert!(list.axis.is_visible());
            assert!(
                list.view.header_factory().is_none(),
                "a flat order has no section headers"
            );
            list.set_sort_key(SortKey::Memory);
            assert!(!list.axis.is_visible(), "the memory bar has no 0-100 scale");
            list.set_sort_key(SortKey::Cpu);
            assert!(list.axis.is_visible());
        });
    }

    /// The axis spans exactly the bar column of the rows.
    #[test]
    fn the_axis_spans_the_bar_column() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            load_css();
            let mut model = model_of(&[]);
            let list = SessionList::new(&model);
            list.update(&model.apply(&fixture("tipico-40"), None));
            let win = show(&list, 1000);
            let scale = descendants(list.axis.upcast_ref())
                .into_iter()
                .find(|w| w.has_css_class("axis-scale"))
                .expect("the axis scale");
            let (row, _) = rows(&list.view).into_iter().next().expect("a row");
            let bar = find::<MetricBar>(&row, None).expect("bar");
            let at = |w: &gtk::Widget| {
                w.compute_point(&win, &gtk::graphene::Point::zero())
                    .unwrap()
                    .x()
            };
            assert_eq!(at(&scale), at(bar.upcast_ref()));
            assert_eq!(scale.width(), bar.width());
            win.destroy();
        });
    }

    /// The agent filter has nothing to choose with one agent type, and then
    /// takes no room; with several it is back.
    #[test]
    fn the_agent_filter_shows_only_when_there_is_a_choice() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let mut model = model_of(&[]);
            let list = SessionList::new(&model);
            list.update(&model.apply(&fixture("uma"), None));
            assert!(!list.agent_menu.is_visible(), "one agent type");
            list.update(&model.apply(&fixture("tipico-40"), None));
            assert!(list.agent_menu.is_visible(), "four agent types");
        });
    }

    /// The last column (context %, since the memory column goes at <= 720 sp)
    /// stays down to the 450 px list beside the map at 1280; only the 360 px
    /// window drops it.
    #[test]
    fn the_second_column_survives_the_list_beside_the_map() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            load_css();
            let mut model = model_of(&[]);
            let list = SessionList::new(&model);
            list.update(&model.apply(&fixture("tipico-40"), None));
            let other_visible = |width: i32| {
                let win = show(&list, width);
                let (row, _) = rows(&list.view).into_iter().next().expect("a row");
                let other = find::<gtk::Label>(&row, Some("row-ctx")).expect("ctx");
                let v = other.is_drawable();
                win.destroy();
                v
            };
            assert!(other_visible(450));
            assert!(!other_visible(360));
        });
    }

    /// The row's actions menu stays (07-gui.md) but only shows on hover,
    /// selection or focus, so 40 rows are not 40 kebabs.
    #[test]
    fn the_row_menu_is_quiet_until_hover_selection_or_focus() {
        let css = include_str!("../resources/sessions.css");
        let rest = css
            .split("listview.session-rack .ac-actions")
            .nth(1)
            .expect("a rule for the row menu");
        assert!(rest.trim_start().starts_with('{'), "the base rule hides it");
        assert!(rest.contains("opacity: 0"));
        for state in [":hover", ":selected", ":focus-within"] {
            assert!(
                css.contains(&format!("session-rack > row{state} .ac-actions")),
                "{state} shows it"
            );
        }
    }

    /// A narrow list has a 72 px bar: the axis drops its middle tick.
    #[test]
    fn the_narrow_axis_drops_the_middle_tick() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let density = Density::default();
            let axis = axis(&density);
            let tick = |text: &str| {
                descendants(axis.upcast_ref())
                    .into_iter()
                    .filter_map(|w| w.downcast::<gtk::Label>().ok())
                    .find(|l| l.label() == text)
                    .expect("tick")
            };
            assert!(tick("50").is_visible());
            density.set_narrow(true);
            assert!(!tick("50").is_visible());
            // Narrow is <= 420 sp, where the CPU column (and so its scale)
            // is already gone: no tick shows.
            assert!(!tick("0").is_visible() && !tick("100 %").is_visible());
        });
    }

    // ---- RR1 list: columns, share bar, Action ----------------------------------

    #[test]
    fn columns_drop_in_the_spec_order_by_list_width() {
        let at = |w| Columns::at(w);
        let all = Columns {
            cpu: true,
            memory: true,
            share_bar: true,
            context: true,
        };
        assert_eq!(at(1500.0), all);
        assert_eq!(at(901.0), all);
        assert_eq!(at(900.0), Columns { cpu: false, ..all }, "<= 900 CPU");
        assert_eq!(at(721.0), Columns { cpu: false, ..all });
        let no_mem = Columns {
            cpu: false,
            memory: false,
            ..all
        };
        assert_eq!(at(720.0), no_mem, "<= 720 memory");
        assert_eq!(at(561.0), no_mem);
        let pct_only = Columns {
            share_bar: false,
            ..no_mem
        };
        assert_eq!(at(560.0), pct_only, "<= 560 share bar shows % only");
        assert_eq!(at(421.0), pct_only);
        assert_eq!(
            at(420.0),
            Columns {
                context: false,
                ..pct_only
            },
            "<= 420 context % leaves the screen"
        );
        assert!(!at(300.0).context);
    }

    /// A list with the `cockpit` sessions in a window of `width`, the spend of
    /// four of them set the way `set_spend` does.
    fn cockpit_window(width: i32) -> (SessionList, gtk::Window) {
        load_css();
        let mut model = model_of(&[]);
        let list = SessionList::new(&model);
        list.update(&model.apply(&fixture("cockpit"), None));
        for (name, share, per_min) in [
            ("atlas-api-auth-refactor", 0.34, 2100.0),
            ("ledger-migration-v4", 0.001, 30.0),
            ("harbor-docs", 0.0, 0.0),
            ("orbit-web-perf-pass", -1.0, -1.0),
        ] {
            let o = object_named(&list, name);
            o.set_share(share);
            o.set_per_min(per_min);
        }
        let win = show(&list, width);
        (list, win)
    }

    fn object_named(list: &SessionList, name: &str) -> SessionObject {
        (0..list.store.n_items())
            .filter_map(|i| list.store.item(i).and_downcast::<SessionObject>())
            .find(|o| o.name() == name)
            .unwrap_or_else(|| panic!("{name}"))
    }

    fn row_of(list: &SessionList, name: &str) -> gtk::Widget {
        rows(&list.view)
            .into_iter()
            .map(|(r, _)| r)
            .find(|r| find::<gtk::Label>(r, Some("row-name")).is_some_and(|l| l.label() == name))
            .unwrap_or_else(|| panic!("no row for {name}"))
    }

    fn text_in(row: &gtk::Widget, class: &str) -> String {
        find::<gtk::Label>(row, Some(class))
            .unwrap_or_else(|| panic!("no .{class}"))
            .label()
            .to_string()
    }

    /// The 30-minute share cell: a 96x20 sparkline where the bar was, the
    /// `%` and the tokens per minute beside it, and `no transcript` when the
    /// spend is unknown. The old bar's fill (33 px, the 2 px floor) is gone
    /// with the bar.
    #[test]
    fn the_share_cell_draws_the_sparkline_the_percent_and_the_rate() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            let size_of = |name: &str| {
                let spark = spark_of(&list, name);
                assert!(find::<gtk::Widget>(&row_of(&list, name), Some("share-bar")).is_none());
                (spark.width(), spark.height())
            };
            assert_eq!(size_of("atlas-api-auth-refactor"), (96, 20));
            assert_eq!(size_of("harbor-docs"), (96, 20), "drawn empty, still there");
            let atlas = row_of(&list, "atlas-api-auth-refactor");
            assert_eq!(text_in(&atlas, "row-share"), "34 %");
            assert_eq!(text_in(&atlas, "row-permin"), "2.1k/min");
            assert_eq!(text_in(&row_of(&list, "harbor-docs"), "row-share"), "0 %");
            let orbit = row_of(&list, "orbit-web-perf-pass");
            assert_eq!(text_in(&orbit, "row-share"), "no transcript");
            assert!(
                !find::<gtk::Widget>(&orbit, Some("row-permin"))
                    .unwrap()
                    .is_drawable(),
                "no rate without a transcript"
            );
            win.destroy();
        });
    }

    /// A tracker that has watched the cockpit sessions for a few minutes:
    /// atlas spent in bursts, harbor appears only in the last snapshot (no growth to draw yet),
    /// orbit has no transcript, so its burn is unknown.
    fn spark_tracker(list: &SessionList, atlas_burns: &[u64]) -> crate::spend::SpendTracker {
        use crate::spend::{BUCKET_MS, Input, SpendTracker};
        let key = |name: &str| {
            let o = object_named(list, name);
            (o.pid(), u64::try_from(o.starttime()).ok())
        };
        let (atlas, harbor, orbit) = (
            key("atlas-api-auth-refactor"),
            key("harbor-docs"),
            key("orbit-web-perf-pass"),
        );
        let mut t = SpendTracker::default();
        let mut burn = 1_000;
        // Minute 0 is the baseline and the last snapshot only closes the
        // minute before it, so the series is exactly `atlas_burns`.
        let last = atlas_burns.len() + 1;
        for m in 0..=last {
            burn += m
                .checked_sub(1)
                .and_then(|i| atlas_burns.get(i))
                .unwrap_or(&0);
            let at = m as u64 * BUCKET_MS;
            let mut rows = vec![Input {
                key: atlas,
                since_start: Some(burn),
                spend_30m: Some(burn),
            }];
            if m >= atlas_burns.len() {
                rows.push(Input {
                    key: harbor,
                    since_start: Some(50),
                    spend_30m: Some(0),
                });
            }
            rows.push(Input {
                key: orbit,
                since_start: None,
                spend_30m: None,
            });
            t.update(at, rows);
        }
        t
    }

    fn spark_of(list: &SessionList, name: &str) -> gtk::Widget {
        find::<gtk::Widget>(&row_of(list, name), Some("row-spark"))
            .unwrap_or_else(|| panic!("no .row-spark in the row of {name}"))
    }

    /// The row carries a 30-minute token sparkline beside the share: it is
    /// there for every session, draws a line only when there is a series,
    /// and says in words what it shows or why it shows nothing.
    #[test]
    fn each_row_has_a_token_sparkline_with_words_for_screen_readers() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            list.set_spend(&spark_tracker(&list, &[600, 1_200, 0, 6_000]));
            settle();
            let atlas = spark_of(&list, "atlas-api-auth-refactor");
            assert!(atlas.is_drawable(), "a series is drawn");
            assert!(!atlas.has_css_class("spark-empty"));
            assert!(
                atlas.width() >= 48 && atlas.height() >= 12,
                "{}x{}",
                atlas.width(),
                atlas.height()
            );
            assert_eq!(
                atlas.accessible_role(),
                gtk::AccessibleRole::Img,
                "a picture with a label, not decoration"
            );
            let words = atlas.tooltip_text().expect("words").to_string();
            assert!(words.contains("last 30 minutes"), "{words}");
            assert!(words.contains("peak 6.0k/min"), "{words}");
            win.destroy();
        });
    }

    /// An unknown spend draws no line and never a flat zero; a session with
    /// one reading only has nothing to draw yet and says so.
    #[test]
    fn no_series_means_no_line_and_words_never_a_flat_zero() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            list.set_spend(&spark_tracker(&list, &[600, 1_200]));
            settle();
            let orbit = spark_of(&list, "orbit-web-perf-pass");
            assert!(orbit.has_css_class("spark-empty"), "no line is drawn");
            let words = orbit.tooltip_text().expect("words").to_string();
            assert!(words.contains("No token history"), "{words}");
            assert!(words.contains("transcript"), "unknown spend: {words}");
            assert!(!words.contains("0/min"), "no zero is claimed: {words}");
            let harbor = spark_of(&list, "harbor-docs");
            assert!(harbor.has_css_class("spark-empty"));
            let words = harbor.tooltip_text().expect("words").to_string();
            assert!(words.contains("No token history"), "{words}");
            assert!(
                !words.contains("transcript"),
                "known burn, just new: {words}"
            );
            win.destroy();
        });
    }

    /// The line follows the data: a later tracker with a bigger burst shows
    /// in the same row, and a session that stopped having a series loses its
    /// line at once.
    #[test]
    fn the_sparkline_follows_the_tracker_on_every_update() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            list.set_spend(&spark_tracker(&list, &[600, 1_200]));
            settle();
            let atlas = spark_of(&list, "atlas-api-auth-refactor");
            let before = atlas.tooltip_text().expect("words").to_string();
            assert!(before.contains("peak 1.2k/min"), "{before}");
            list.set_spend(&spark_tracker(&list, &[600, 1_200, 30_000]));
            settle();
            let after = spark_of(&list, "atlas-api-auth-refactor");
            let words = after.tooltip_text().expect("words").to_string();
            assert!(words.contains("peak 30.0k/min"), "{words}");
            assert!(!after.has_css_class("spark-empty"));
            // The session goes unknown (no transcript any more): the line goes.
            list.set_spend(&crate::spend::SpendTracker::default());
            settle();
            let gone = spark_of(&list, "atlas-api-auth-refactor");
            assert!(gone.has_css_class("spark-empty"));
            assert!(!gone.tooltip_text().unwrap().contains("peak"));
            win.destroy();
        });
    }

    /// Pixels of `w` as the renderer draws it (BGRA, premultiplied).
    fn pixels_of(w: &gtk::Widget) -> Vec<u8> {
        let (pw, ph) = (w.width(), w.height());
        let paintable = gtk::WidgetPaintable::new(Some(w));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(pw), f64::from(ph));
        let node = snapshot.to_node().expect("rendered node");
        let native = w.native().expect("native");
        let texture = native
            .renderer()
            .expect("renderer")
            .render_texture(&node, None);
        let stride = texture.width() as usize * 4;
        let mut buf = vec![0u8; stride * texture.height() as usize];
        texture.download(&mut buf, stride);
        buf
    }

    fn drawn(buf: &[u8]) -> usize {
        buf.iter().skip(3).step_by(4).filter(|&&a| a > 0).count()
    }

    /// A series change must reach the screen: the row is painted empty,
    /// then with a line, then with a line of another shape whose words are
    /// the same (so only the second queue_draw can show it).
    #[test]
    fn a_series_change_is_drawn() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            let name = "atlas-api-auth-refactor";
            list.set_spend(&crate::spend::SpendTracker::default());
            settle();
            let empty = pixels_of(&spark_of(&list, name));
            assert!(spark_of(&list, name).has_css_class("spark-empty"));
            assert_eq!(drawn(&empty), 0, "no series, nothing drawn");
            list.set_spend(&spark_tracker(&list, &[600, 1_200, 600]));
            settle();
            let first = pixels_of(&spark_of(&list, name));
            assert!(!spark_of(&list, name).has_css_class("spark-empty"));
            assert!(drawn(&first) > 20, "a line is drawn: {}", drawn(&first));
            // Same peak and same last value, so the words do not change.
            let words = spark_of(&list, name).tooltip_text();
            list.set_spend(&spark_tracker(&list, &[600, 1_200, 0, 600]));
            settle();
            assert_eq!(spark_of(&list, name).tooltip_text(), words);
            let second = pixels_of(&spark_of(&list, name));
            assert!(first != second, "the new shape is on screen");
            win.destroy();
        });
    }

    /// The sparkline takes the place of the bar's width at wide lists and
    /// leaves with it under 560 sp; the `%` stays.
    #[test]
    fn the_sparkline_leaves_with_the_share_bar_in_a_narrow_list() {
        for width in [1600, 900, 560, 420] {
            gtk::test_synced(move || {
                adw::init().expect("GTK display (Broadway in CI)");
                let (list, win) = cockpit_window(width);
                list.set_spend(&spark_tracker(&list, &[600, 1_200]));
                settle();
                let spark = spark_of(&list, "atlas-api-auth-refactor");
                assert_eq!(
                    spark.is_drawable(),
                    Columns::at(f64::from(width)).share_bar,
                    "sparkline at {width}"
                );
                win.destroy();
            });
        }
    }

    /// The context % has its own column; the cockpit reason sits under the
    /// name, only for a session that has one.
    #[test]
    fn the_context_percent_and_the_reason_have_their_cells() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            let ledger = row_of(&list, "ledger-migration-v4");
            assert_eq!(
                text_in(&ledger, "row-ctx"),
                object_named(&list, "ledger-migration-v4").ctx_text()
            );
            assert_eq!(text_in(&ledger, "row-reason"), "ctx 62 %");
            let atlas = row_of(&list, "atlas-api-auth-refactor");
            assert!(
                !find::<gtk::Widget>(&atlas, Some("row-reason"))
                    .unwrap()
                    .is_drawable(),
                "nothing to say, nothing drawn"
            );
            win.destroy();
        });
    }

    /// The columns leave by the list's own width, in the order of the spec;
    /// Name and Action stay, and the context % stays in the spoken label.
    #[test]
    fn columns_leave_the_screen_by_list_width_and_name_and_action_stay() {
        for width in [1600, 1000, 900, 720, 560, 420] {
            gtk::test_synced(move || {
                adw::init().expect("GTK display (Broadway in CI)");
                let (list, win) = cockpit_window(width);
                let want = Columns::at(f64::from(width));
                let ledger = row_of(&list, "ledger-migration-v4");
                let shown = |class: &str| {
                    find::<gtk::Widget>(&ledger, Some(class))
                        .unwrap_or_else(|| panic!("no .{class} at {width}"))
                        .is_drawable()
                };
                assert_eq!(shown("col-cpu"), want.cpu, "CPU at {width}");
                assert_eq!(shown("col-mem"), want.memory, "memory at {width}");
                assert_eq!(shown("row-spark"), want.share_bar, "sparkline at {width}");
                assert!(shown("row-share"), "the % stays at {width}");
                assert_eq!(shown("row-ctx"), want.context, "context at {width}");
                assert!(shown("row-name"), "Name stays at {width}");
                assert!(shown("row-action"), "Action stays at {width}");
                let o = object_named(&list, "ledger-migration-v4");
                assert!(
                    o.a11y_label().contains("context 62%"),
                    "spoken: {}",
                    o.a11y_label()
                );
                win.destroy();
            });
        }
    }

    /// Rows are 40 px, 44 below 1000 sp.
    #[test]
    fn rows_are_40_px_and_44_below_1000_sp() {
        for (width, want) in [(1100, ROW_HEIGHT_PX), (900, TOUCH_ROW_HEIGHT_PX)] {
            gtk::test_synced(move || {
                adw::init().expect("GTK display (Broadway in CI)");
                let (list, win) = cockpit_window(width);
                let row = row_of(&list, "ledger-migration-v4");
                assert_eq!(row.height(), want, "at {width} sp");
                win.destroy();
            });
        }
    }

    /// One button per verdict that asks for something: its label and icon,
    /// a 96 px column, at least 34 px tall; OK and no-data rows have none.
    #[test]
    fn the_action_button_follows_the_verdict() {
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            for (name, want) in [
                ("ledger-migration-v4", Some("Compact context")),
                ("orbit-web-perf-pass", Some("Write handoff")),
                ("quill-flaky-tests", None),
                ("harbor-docs", Some("End\u{2026}")),
                ("atlas-api-auth-refactor", None),
                ("relay-i18n", None),
            ] {
                let row = row_of(&list, name);
                let b = find::<gtk::Button>(&row, Some("row-action")).expect("slot button");
                match want {
                    None => assert!(!b.is_drawable(), "{name}: empty cell"),
                    Some(label) => {
                        assert!(b.is_drawable(), "{name}");
                        assert_eq!(text_in(b.upcast_ref(), "row-action-label"), label);
                        assert_eq!(b.tooltip_text().as_deref(), Some(label));
                        assert!(b.has_css_class("flat"));
                        assert!(b.width() <= 96, "{name}: {} px", b.width());
                        assert!(b.height() >= 34, "{name}: {} px", b.height());
                        assert!(b.is_sensitive(), "no disabled buttons");
                        let slot = b.parent().expect("column");
                        assert_eq!(slot.width(), 96, "fixed 96 px column");
                    }
                }
            }
            win.destroy();
        });
    }

    /// The button runs the operation of its own row's session, also after GTK
    /// recycled the row for another one.
    #[test]
    fn the_action_button_invokes_the_operation_for_its_session() {
        use crate::actions_ui::RowOp;
        gtk::test_synced(|| {
            adw::init().expect("GTK display (Broadway in CI)");
            let (list, win) = cockpit_window(1600);
            let calls: std::rc::Rc<std::cell::RefCell<Vec<(u32, RowOp)>>> = Default::default();
            let sink = calls.clone();
            list.connect_row_action(move |pid, op| sink.borrow_mut().push((pid, op)));
            for name in ["ledger-migration-v4", "orbit-web-perf-pass", "harbor-docs"] {
                let row = row_of(&list, name);
                find::<gtk::Button>(&row, Some("row-action"))
                    .expect("button")
                    .emit_clicked();
            }
            let pid = |n: &str| object_named(&list, n).pid();
            assert_eq!(
                *calls.borrow(),
                [
                    (pid("ledger-migration-v4"), RowOp::Compact),
                    (pid("orbit-web-perf-pass"), RowOp::Handoff),
                    (pid("harbor-docs"), RowOp::End),
                ]
            );
            win.destroy();
        });
    }

    /// The selected row is neutral with a 2 px leading bar (it read as busy).
    #[test]
    fn the_selected_row_is_neutral_with_a_leading_bar() {
        let tokens = include_str!("../resources/tokens.css");
        assert!(tokens.contains("--ac-selection: #dde2ee;"), "light");
        assert!(tokens.contains("--ac-selection: #2a2d3a;"), "dark");
        let css = include_str!("../resources/sessions.css");
        let at = css
            .find("listview.session-rack > row:selected {")
            .expect("rule");
        let rule = &css[at..css[at..].find('}').map(|e| at + e).unwrap()];
        assert!(rule.contains("inset 2px 0 0"), "{rule}");
    }
}
