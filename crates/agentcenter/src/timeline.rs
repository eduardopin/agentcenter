//! Timeline (direction B): the reusable 10-minute line and area chart, used
//! at 150x40 in the machine strip ([`STRIP_SIZE`]) and at 360x112 in the
//! session detail ([`DETAIL_SIZE`]). The right edge is now, the left edge
//! `cap` samples back; a missing sample (`NaN`) breaks the line and never
//! becomes 0.
//!
//! What it draws, all with GtkSnapshot paths (no cairo):
//! - three horizontal gridlines (top, middle, zero);
//! - the primary series as a line with a light area under it, and a value
//!   dot on the newest sample at the right edge;
//! - optionally a threshold: a dashed line, with the part of the series above
//!   it (line and area down to the threshold) in the alert colour;
//! - optionally a second series as a dashed line (disk write next to read);
//! - optionally a stacked band on top of the primary series, hatched (swap
//!   over PSS): the dot then sits on the total;
//! - optionally the time axis ("-10 min", "-5", "now") and the y labels of the
//!   three gridlines, as GtkLabels (Pango measures them, CSS sets the font).
//!
//! A series is ink by default. A chart that carries the `ok` class (the
//! machine strip) draws green under the limit and crimson over it, and one
//! with the `area` class fills under the series with a gradient instead of
//! the flat area. Colours come from the CSS `color` of small pen nodes
//! (`timeline > .pen.series|.grid|.alert|.ok`, see resources/timeline.css), so
//! a theme switch needs no code. The widget only
//! repaints when values, scale or decorations change ([`Timeline::set_values`]
//! says whether it did).
//!
//! The parts without GTK ([`split_above`], [`stacked`], [`band_polygon`],
//! [`tick_label`], [`axis_labels`], [`scale_max`], ...) are tested without a
//! display. Line placement reuses [`crate::graph::points`].

use crate::graph::{nice_max, peak, points};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;

/// Size in the machine strip (B chart catalogue, "Machine timeline").
pub const STRIP_SIZE: (i32, i32) = (150, 40);
/// Size in the session detail (B chart catalogue, "Session timelines").
pub const DETAIL_SIZE: (i32, i32) = (360, 112);
/// Stroke width of the series, px.
pub const LINE_PX: f32 = 1.5;
/// Dash and gap of the second series, px.
pub const DASH_PX: [f32; 2] = [4.0, 3.0];
/// Dash and gap of the threshold line, px.
pub const THRESHOLD_DASH_PX: [f32; 2] = [3.0, 3.0];
/// Alpha of the area under the series, over the pen colour: the line carries
/// the data, the area only gives the shape weight.
pub const AREA_ALPHA: f32 = 0.12;
/// Alpha at the top and at the baseline of the gradient area (class `area`).
pub const GRADIENT_ALPHA: (f32, f32) = (0.55, 0.0);
/// Alpha of the hatch stripes of the stacked band, over the pen colour.
pub const HATCH_ALPHA: f32 = 0.45;
/// Hatch period and stripe width, px.
pub const HATCH_STEP: f32 = 6.0;
pub const HATCH_PX: f32 = 2.0;
/// Gap between the y labels and the plot, px.
pub const Y_LABEL_GAP: i32 = 6;

/// Top of the y scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// Always this value (percentages: 100).
    Fixed(f64),
    /// The smallest "round" value over the peak ([`nice_max`]), never below
    /// `floor`; `binary` is for bytes already in GiB or MiB and steps
    /// 1, 1.5, 2, 3, 4, 6, 8 (`UNIT_STEPS`).
    Nice { floor: f64, binary: bool },
}

impl Default for Scale {
    fn default() -> Self {
        Scale::Fixed(100.0)
    }
}

/// Top of the scale for a series whose highest reading is `peak`.
pub fn scale_max(scale: Scale, peak: Option<f64>) -> f64 {
    match scale {
        Scale::Fixed(max) => max,
        Scale::Nice {
            floor,
            binary: false,
        } => nice_max(peak.unwrap_or(f64::NAN), floor, false),
        Scale::Nice {
            floor,
            binary: true,
        } => nice_unit_max(peak.unwrap_or(f64::NAN), floor),
    }
}

/// Steps of a scale for bytes already converted to GiB or MiB: the middle
/// gridline stays a round number (0.75, 1.5, 3, ...) and a 1.1 GiB session
/// reads 0..1.5, not 0..2.
const UNIT_STEPS: [f64; 7] = [1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];

/// The smallest `step x 10^k` of [`UNIT_STEPS`] (or 10^k) over `max`, never
/// below `floor`.
fn nice_unit_max(max: f64, floor: f64) -> f64 {
    let floor = floor.max(f64::MIN_POSITIVE);
    if !max.is_finite() || max <= floor {
        return floor;
    }
    let base = 10f64.powf(max.log10().floor());
    let m = max / base;
    UNIT_STEPS
        .into_iter()
        .find(|s| m <= *s + 1e-9)
        .unwrap_or(10.0)
        * base
}

/// Plot y (px from the top) of `v` on a scale `0..=max` of `h` px; above the
/// scale it stays on the top edge.
pub fn value_y(v: f64, max: f64, h: f32) -> f32 {
    if !(max.is_finite() && max > 0.0) {
        return 0.0;
    }
    let frac = (v / max).clamp(0.0, 1.0) as f32;
    h - frac * h
}

/// Plot y of the three gridlines: top, middle and zero.
pub fn gridline_ys(h: f32) -> [f32; 3] {
    [0.0, h / 2.0, h]
}

/// Radius of the value dot, px: the spec's 3 px dot at every chart size.
pub fn dot_radius(_height: i32) -> f32 {
    3.0
}

/// Where the series is drawn inside a plot of `w` x `h` px: `(top, width,
/// height)`. The dot needs its radius free on the right and on top; the
/// stroke needs one px under the zero line.
pub fn inner(w: f32, h: f32) -> (f32, f32, f32) {
    let r = dot_radius(h as i32);
    (r, (w - r).max(0.0), (h - r - 1.0).max(0.0))
}

/// The pieces of a polyline (`run`, plot coordinates) that lie above a
/// threshold drawn at `y_at` (above = smaller y). A piece starts and ends at
/// the interpolated crossing, so the alert colour begins exactly on the
/// threshold line. A point on the line is not above it.
pub fn split_above(run: &[(f32, f32)], y_at: f32) -> Vec<Vec<(f32, f32)>> {
    let above = |p: &(f32, f32)| p.1 < y_at;
    let cross = |a: (f32, f32), b: (f32, f32)| {
        let t = (y_at - a.1) / (b.1 - a.1);
        (a.0 + t * (b.0 - a.0), y_at)
    };
    let mut out = Vec::new();
    let mut cur = Vec::new();
    if let Some(first) = run.first().filter(|p| above(p)) {
        cur.push(*first);
    }
    for w in run.windows(2) {
        let (a, b) = (w[0], w[1]);
        match (above(&a), above(&b)) {
            (true, true) => cur.push(b),
            (true, false) => {
                cur.push(cross(a, b));
                out.push(std::mem::take(&mut cur));
            }
            (false, true) => {
                cur.push(cross(a, b));
                cur.push(b);
            }
            (false, false) => {}
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `lower + upper`, sample by sample: the top of a stacked band. A gap in
/// either is a gap in the sum.
pub fn stacked(lower: &[f32], upper: &[f32]) -> Vec<f32> {
    lower
        .iter()
        .zip(upper)
        .map(|(a, b)| {
            if a.is_finite() && b.is_finite() {
                a + b
            } else {
                f32::NAN
            }
        })
        .collect()
}

/// Index ranges where both series have a reading: the band exists there.
pub fn both_finite(a: &[f32], b: &[f32]) -> Vec<Range<usize>> {
    let ok: Vec<f32> = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            if x.is_finite() && y.is_finite() {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    crate::graph::runs_at_least(&ok, 1.0)
}

/// Closed polygon of the band between `lower` and `total` over `range`, in the
/// coordinates of [`points`]: along the total forward, back along the lower.
pub fn band_polygon(
    lower: &[f32],
    total: &[f32],
    range: Range<usize>,
    cap: usize,
    max: f64,
    w: f32,
    h: f32,
) -> Vec<(f32, f32)> {
    let len = lower.len().min(total.len());
    if range.start >= range.end || range.end > len {
        return Vec::new();
    }
    let cap = cap.max(len).max(2);
    let step = w / (cap - 1) as f32;
    let first = cap - len;
    let x = |i: usize| (first + i) as f32 * step;
    let mut poly: Vec<(f32, f32)> = range
        .clone()
        .map(|i| (x(i), value_y(f64::from(total[i]), max, h)))
        .collect();
    poly.extend(
        range
            .rev()
            .map(|i| (x(i), value_y(f64::from(lower[i]), max, h))),
    );
    poly
}

/// Newest sample as a point on the right edge; `None` when it is missing.
pub fn newest_point(values: &[f32], max: f64, w: f32, h: f32) -> Option<(f32, f32)> {
    let v = *values.last()?;
    v.is_finite().then(|| (w, value_y(f64::from(v), max, h)))
}

/// Gridline label: no trailing zeros (`0`, `0.5`, `1`, `12.5`, `100`).
pub fn tick_label(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

/// Labels of the time axis for a window of `span_secs`: left, middle, right.
/// Minutes from one minute up (`-10 min`, `-5`, `now`), seconds below.
pub fn axis_labels(span_secs: u64) -> [String; 3] {
    let (span, unit) = if span_secs >= 60 {
        (span_secs as f64 / 60.0, "min")
    } else {
        (span_secs as f64, "s")
    };
    [
        format!("\u{2212}{} {unit}", tick_label(span)),
        format!("\u{2212}{}", tick_label(span / 2.0)),
        "now".to_owned(),
    ]
}

/// Equal bit for bit: here a missing sample equals a missing sample.
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Pen nodes, in the order of `pens`.
#[derive(Clone, Copy)]
enum Pen {
    Series = 0,
    Grid = 1,
    Alert = 2,
    Ok = 3,
}
const PEN_CLASSES: [&str; 4] = ["series", "grid", "alert", "ok"];

/// Values the widget last drew.
#[derive(Default)]
struct Data {
    primary: Vec<f32>,
    second: Vec<f32>,
    /// Top of the stacked band (`primary + band`), empty without a band.
    total: Vec<f32>,
    cap: usize,
    max: f64,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Timeline {
        pub(super) data: RefCell<Data>,
        pub scale: Cell<Scale>,
        pub threshold: Cell<Option<f64>>,
        pub pens: RefCell<Vec<gtk::Widget>>,
        /// Left, middle, right.
        pub axis: RefCell<Vec<gtk::Label>>,
        /// Top, middle, zero.
        pub ylabels: RefCell<Vec<gtk::Label>>,
        /// Plot rectangle from the last allocation: x, y, width, height.
        pub plot: Cell<(f32, f32, f32, f32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Timeline {
        const NAME: &'static str = "AcTimeline";
        type Type = super::Timeline;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("timeline");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Timeline {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(false);
            obj.set_overflow(gtk::Overflow::Hidden);
            let pens = PEN_CLASSES
                .iter()
                .map(|c| {
                    let p = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    p.add_css_class("pen");
                    p.add_css_class(c);
                    p.set_can_target(false);
                    p.set_parent(&*obj);
                    p.upcast::<gtk::Widget>()
                })
                .collect();
            self.pens.replace(pens);
            let label = |class: &str, text: &str| {
                let l = gtk::Label::new(Some(text));
                l.add_css_class(class);
                l.set_can_target(false);
                l.set_visible(false);
                l.set_parent(&*obj);
                l
            };
            let [a, b, c] = axis_labels(agentcenter_core::HISTORY_LEN as u64);
            self.axis.replace(vec![
                label("axis", &a),
                label("axis", &b),
                label("axis", &c),
            ]);
            self.ylabels.replace(vec![
                label("ylabel", "100"),
                label("ylabel", "50"),
                label("ylabel", "0"),
            ]);
            self.data.borrow_mut().max = 100.0;
        }

        fn dispose(&self) {
            for p in self.pens.take() {
                p.unparent();
            }
            for l in self.axis.take().into_iter().chain(self.ylabels.take()) {
                l.unparent();
            }
        }
    }

    impl WidgetImpl for Timeline {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            // The size comes from `set_size_request` (STRIP_SIZE, DETAIL_SIZE).
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let natural = |w: &gtk::Widget, o| w.measure(o, -1).1;
            for p in self.pens.borrow().iter() {
                p.measure(gtk::Orientation::Horizontal, -1);
                p.size_allocate(&gtk::Allocation::new(0, 0, 0, 0), -1);
            }
            let ylabels = self.ylabels.borrow();
            let axis = self.axis.borrow();
            let y_on = ylabels.first().is_some_and(|l| l.is_visible());
            let axis_on = axis.first().is_some_and(|l| l.is_visible());
            let ylw = if y_on {
                ylabels
                    .iter()
                    .map(|l| natural(l.upcast_ref(), gtk::Orientation::Horizontal))
                    .max()
                    .unwrap_or(0)
                    + Y_LABEL_GAP
            } else {
                0
            };
            let axh = if axis_on {
                axis.iter()
                    .map(|l| natural(l.upcast_ref(), gtk::Orientation::Vertical))
                    .max()
                    .unwrap_or(0)
                    + 2
            } else {
                0
            };
            let (pw, ph) = ((width - ylw).max(0), (height - axh).max(0));
            self.plot.set((ylw as f32, 0.0, pw as f32, ph as f32));
            let (top, iw, ih) = inner(pw as f32, ph as f32);
            let ys = gridline_ys(ih);
            for (l, gy) in ylabels.iter().zip(ys) {
                let (lw, lh) = (
                    natural(l.upcast_ref(), gtk::Orientation::Horizontal),
                    natural(l.upcast_ref(), gtk::Orientation::Vertical),
                );
                let y = ((top + gy) as i32 - lh / 2).clamp(0, (height - lh).max(0));
                let x = (ylw - Y_LABEL_GAP - lw).max(0);
                l.size_allocate(&gtk::Allocation::new(x, y, lw, lh), -1);
            }
            for (i, l) in axis.iter().enumerate() {
                let (lw, lh) = (
                    natural(l.upcast_ref(), gtk::Orientation::Horizontal),
                    natural(l.upcast_ref(), gtk::Orientation::Vertical),
                );
                let x = match i {
                    0 => ylw,
                    1 => ylw + (iw / 2.0) as i32 - lw / 2,
                    _ => ylw + iw as i32 - lw,
                };
                let x = x.clamp(0, (width - lw).max(0));
                l.size_allocate(&gtk::Allocation::new(x, height - axh + 2, lw, lh), -1);
            }
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let obj = self.obj();
            let (px, py, pw, ph) = self.plot.get();
            if pw > 0.0 && ph > 0.0 {
                s.save();
                s.translate(&gtk::graphene::Point::new(px, py));
                self.draw_plot(s, pw, ph, dot_radius(ph as i32));
                s.restore();
            }
            for l in self
                .axis
                .borrow()
                .iter()
                .chain(self.ylabels.borrow().iter())
            {
                obj.snapshot_child(l, s);
            }
        }
    }

    impl Timeline {
        fn pen(&self, p: Pen) -> gtk::gdk::RGBA {
            self.pens.borrow()[p as usize].color()
        }

        fn draw_plot(&self, s: &gtk::Snapshot, pw: f32, ph: f32, r: f32) {
            let obj = self.obj();
            let data = self.data.borrow();
            let (top, iw, ih) = inner(pw, ph);
            if iw <= 0.0 || ih <= 0.0 {
                return;
            }
            let ink = if obj.has_css_class("ok") && !obj.has_css_class("alert") {
                self.pen(Pen::Ok)
            } else {
                self.pen(Pen::Series)
            };
            let gradient = obj.has_css_class("area");
            let alert = self.pen(Pen::Alert);
            let grid = self.pen(Pen::Grid);
            s.save();
            s.translate(&gtk::graphene::Point::new(0.0, top));
            for gy in gridline_ys(ih) {
                let y = gy.round().min(ih);
                s.append_color(&grid, &gtk::graphene::Rect::new(0.0, y, pw, 1.0));
            }
            let (cap, max) = (data.cap, data.max);
            let lines = points(&data.primary, cap, max, iw, ih);

            // Stacked band: hatch between the primary and the total.
            if !data.total.is_empty() {
                let mut hatch = ink;
                hatch.set_alpha(ink.alpha() * HATCH_ALPHA);
                for r in both_finite(&data.primary, &data.total) {
                    let poly = band_polygon(&data.primary, &data.total, r, cap, max, iw, ih);
                    let Some(path) = closed(&poly) else { continue };
                    s.push_fill(&path, gtk::gsk::FillRule::Winding);
                    let stripes = gtk::gsk::PathBuilder::new();
                    let mut x = -ih;
                    while x < iw + ih {
                        stripes.move_to(x, ih);
                        stripes.line_to(x + ih, 0.0);
                        x += HATCH_STEP;
                    }
                    s.append_stroke(&stripes.to_path(), &gtk::gsk::Stroke::new(HATCH_PX), &hatch);
                    s.pop();
                }
            }

            let mut area = ink;
            area.set_alpha(ink.alpha() * AREA_ALPHA);
            let stroke = gtk::gsk::Stroke::new(LINE_PX);
            stroke.set_line_join(gtk::gsk::LineJoin::Round);
            for run in &lines {
                if gradient {
                    gradient_under(s, run, ih, &ink);
                } else {
                    fill_under(s, run, ih, &area);
                }
                stroke_run(s, run, &stroke, &ink);
            }
            if !data.total.is_empty() {
                for run in points(&data.total, cap, max, iw, ih) {
                    stroke_run(s, &run, &stroke, &ink);
                }
            }
            if !data.second.is_empty() {
                let dashed = gtk::gsk::Stroke::new(LINE_PX);
                dashed.set_dash(&DASH_PX);
                for run in points(&data.second, cap, max, iw, ih) {
                    stroke_run(s, &run, &dashed, &ink);
                }
            }

            // Threshold: dashed line, and what lies above it in the alert colour.
            let mut over = false;
            if let Some(t) = self.threshold.get().filter(|t| *t <= max && max > 0.0) {
                let yt = value_y(t, max, ih);
                let mut fill = alert;
                fill.set_alpha(alert.alpha() * AREA_ALPHA * 2.0);
                for run in &lines {
                    for piece in split_above(run, yt) {
                        fill_under(s, &piece, yt, &fill);
                        stroke_run(s, &piece, &stroke, &alert);
                    }
                }
                let dash = gtk::gsk::Stroke::new(1.0);
                dash.set_dash(&THRESHOLD_DASH_PX);
                let b = gtk::gsk::PathBuilder::new();
                b.move_to(0.0, yt.round() + 0.5);
                b.line_to(pw, yt.round() + 0.5);
                s.append_stroke(&b.to_path(), &dash, &alert);
                over = data.primary.last().is_some_and(|v| f64::from(*v) > t);
            }

            let top_series = if data.total.is_empty() {
                &data.primary
            } else {
                &data.total
            };
            if let Some((x, y)) = newest_point(top_series, max, iw, ih) {
                let dot = gtk::gsk::PathBuilder::new();
                dot.add_circle(&gtk::graphene::Point::new(x, y), r);
                s.append_fill(
                    &dot.to_path(),
                    gtk::gsk::FillRule::Winding,
                    if over { &alert } else { &ink },
                );
            }
            s.restore();
        }
    }

    fn closed(poly: &[(f32, f32)]) -> Option<gtk::gsk::Path> {
        let (first, rest) = poly.split_first()?;
        let b = gtk::gsk::PathBuilder::new();
        b.move_to(first.0, first.1);
        for (x, y) in rest {
            b.line_to(*x, *y);
        }
        b.close();
        Some(b.to_path())
    }

    /// Area between a polyline and the horizontal line at `base`.
    fn fill_under(s: &gtk::Snapshot, run: &[(f32, f32)], base: f32, color: &gtk::gdk::RGBA) {
        if run.len() < 2 {
            return;
        }
        let mut poly = Vec::with_capacity(run.len() + 2);
        poly.push((run[0].0, base));
        poly.extend_from_slice(run);
        poly.push((run[run.len() - 1].0, base));
        if let Some(p) = closed(&poly) {
            s.append_fill(&p, gtk::gsk::FillRule::Winding, color);
        }
    }

    /// Area under a polyline fading from [`GRADIENT_ALPHA`] at the top of the
    /// plot to the baseline.
    fn gradient_under(s: &gtk::Snapshot, run: &[(f32, f32)], base: f32, color: &gtk::gdk::RGBA) {
        if run.len() < 2 {
            return;
        }
        let mut poly = Vec::with_capacity(run.len() + 2);
        poly.push((run[0].0, base));
        poly.extend_from_slice(run);
        poly.push((run[run.len() - 1].0, base));
        let Some(p) = closed(&poly) else { return };
        let stop = |at: f32, a: f32| {
            let mut c = *color;
            c.set_alpha(color.alpha() * a);
            gtk::gsk::ColorStop::new(at, c)
        };
        let right = run.iter().fold(0.0_f32, |m, p| m.max(p.0));
        s.push_fill(&p, gtk::gsk::FillRule::Winding);
        s.append_linear_gradient(
            &gtk::graphene::Rect::new(0.0, 0.0, right + 1.0, base),
            &gtk::graphene::Point::new(0.0, 0.0),
            &gtk::graphene::Point::new(0.0, base),
            &[stop(0.0, GRADIENT_ALPHA.0), stop(1.0, GRADIENT_ALPHA.1)],
        );
        s.pop();
    }

    fn stroke_run(
        s: &gtk::Snapshot,
        run: &[(f32, f32)],
        stroke: &gtk::gsk::Stroke,
        color: &gtk::gdk::RGBA,
    ) {
        match run {
            [] => {}
            [(x, y)] => s.append_color(
                color,
                &gtk::graphene::Rect::new(x - LINE_PX, y - LINE_PX / 2.0, LINE_PX * 2.0, LINE_PX),
            ),
            [(x0, y0), rest @ ..] => {
                let b = gtk::gsk::PathBuilder::new();
                b.move_to(*x0, *y0);
                for (x, y) in rest {
                    b.line_to(*x, *y);
                }
                s.append_stroke(&b.to_path(), stroke, color);
            }
        }
    }
}

glib::wrapper! {
    /// The 10-minute chart. CSS: `timeline`, `timeline > .pen.series|.grid|
    /// .alert|.ok` (colours), `timeline > label.axis|.ylabel`; `timeline.alert`
    /// turns the whole series to the alert colour.
    pub struct Timeline(ObjectSubclass<imp::Timeline>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Timeline {
    /// A chart of `width` x `height` px (see [`STRIP_SIZE`], [`DETAIL_SIZE`]).
    pub fn new(width: i32, height: i32) -> Self {
        let t: Self = glib::Object::new();
        t.set_size_request(width, height);
        t
    }

    /// Scale of the y axis; takes effect on the next [`Timeline::set_values`].
    pub fn set_scale(&self, scale: Scale) {
        self.imp().scale.set(scale);
    }

    /// Threshold on the primary series, in its unit; `None` for none.
    pub fn set_threshold(&self, at: Option<f64>) {
        if self.imp().threshold.replace(at) != at {
            self.queue_draw();
        }
    }

    pub fn threshold(&self) -> Option<f64> {
        self.imp().threshold.get()
    }

    /// Whole series in the alert colour (the reading is over its limit).
    pub fn set_alert(&self, on: bool) {
        if self.has_css_class("alert") != on {
            if on {
                self.add_css_class("alert");
            } else {
                self.remove_css_class("alert");
            }
        }
    }

    /// Time axis under the plot, for a window of `span_secs`; `None` hides it.
    pub fn set_axis(&self, span_secs: Option<u64>) {
        let axis = self.imp().axis.borrow();
        if let Some(span) = span_secs {
            for (l, t) in axis.iter().zip(axis_labels(span)) {
                l.set_label(&t);
            }
        }
        for l in axis.iter() {
            l.set_visible(span_secs.is_some());
        }
        self.queue_allocate();
    }

    /// Whether the time axis is shown.
    pub fn axis_visible(&self) -> bool {
        self.imp()
            .axis
            .borrow()
            .first()
            .is_some_and(|l| l.is_visible())
    }

    /// Labels of the three gridlines on the left.
    pub fn set_y_labels(&self, on: bool) {
        for l in self.imp().ylabels.borrow().iter() {
            l.set_visible(on);
        }
        self.queue_allocate();
    }

    /// Texts of the y labels (top, middle, zero) and of the time axis.
    pub fn labels(&self) -> (Vec<String>, Vec<String>) {
        let imp = self.imp();
        let t = |ls: &Vec<gtk::Label>| ls.iter().map(|l| l.label().to_string()).collect();
        (t(&imp.ylabels.borrow()), t(&imp.axis.borrow()))
    }

    /// Top of the scale the last values were drawn on.
    pub fn scale_top(&self) -> f64 {
        self.imp().data.borrow().max
    }

    /// New values, in chronological order (`Ring::to_vec`), `cap` samples
    /// wide. `second` is the dashed series, `band` the hatched band stacked on
    /// `primary`. Repaints only if something changed; returns whether it did.
    pub fn set_values(
        &self,
        primary: &[f32],
        second: Option<&[f32]>,
        band: Option<&[f32]>,
        cap: usize,
    ) -> bool {
        let imp = self.imp();
        let second = second.unwrap_or(&[]);
        let total = band.map(|b| stacked(primary, b)).unwrap_or_default();
        let top = peak([primary, second, &total[..]]);
        let max = scale_max(imp.scale.get(), top);
        {
            let d = imp.data.borrow();
            if d.cap == cap
                && d.max == max
                && same(&d.primary, primary)
                && same(&d.second, second)
                && same(&d.total, &total)
            {
                return false;
            }
        }
        let rescaled = imp.data.borrow().max != max;
        imp.data.replace(Data {
            primary: primary.to_vec(),
            second: second.to_vec(),
            total,
            cap,
            max,
        });
        if rescaled {
            let ls = imp.ylabels.borrow();
            for (l, v) in ls.iter().zip([max, max / 2.0, 0.0]) {
                l.set_label(&tick_label(v));
            }
            self.queue_allocate();
        }
        self.queue_draw();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::HISTORY_LEN;

    #[test]
    fn scale_fixed_or_nice() {
        assert_eq!(scale_max(Scale::Fixed(100.0), Some(140.0)), 100.0);
        assert_eq!(scale_max(Scale::Fixed(100.0), None), 100.0);
        let nice = Scale::Nice {
            floor: 20.0,
            binary: false,
        };
        assert_eq!(scale_max(nice, Some(12.5)), 20.0, "floor holds");
        assert_eq!(scale_max(nice, Some(31.0)), 50.0);
        assert_eq!(scale_max(nice, None), 20.0, "no reading: floor");
        let bin = Scale::Nice {
            floor: 1.0,
            binary: true,
        };
        // Memory and disk are already in GiB / MiB: 1.1 GiB reads 0, 0.5, 1,
        // 1.5 like the mock, not 0..2.
        assert_eq!(scale_max(bin, Some(1.1)), 1.5);
        assert_eq!(scale_max(bin, Some(0.91)), 1.0);
        assert_eq!(scale_max(bin, Some(1.6)), 2.0);
        assert_eq!(scale_max(bin, Some(2.5)), 3.0);
        assert_eq!(scale_max(bin, Some(5.0)), 6.0);
        assert_eq!(scale_max(bin, Some(7.0)), 8.0);
        assert_eq!(scale_max(bin, Some(9.0)), 10.0);
        assert_eq!(scale_max(bin, Some(430.0)), 600.0);
        assert_eq!(scale_max(bin, None), 1.0, "no reading: floor");
    }

    #[test]
    fn value_y_maps_the_scale_and_clamps() {
        assert_eq!(value_y(0.0, 100.0, 40.0), 40.0);
        assert_eq!(value_y(100.0, 100.0, 40.0), 0.0);
        assert_eq!(value_y(25.0, 100.0, 40.0), 30.0);
        assert_eq!(value_y(250.0, 100.0, 40.0), 0.0, "above stays on top");
        assert_eq!(value_y(-5.0, 100.0, 40.0), 40.0);
        assert_eq!(value_y(5.0, 0.0, 40.0), 0.0, "no scale: full height");
    }

    #[test]
    fn three_gridlines_top_middle_zero() {
        assert_eq!(gridline_ys(84.0), [0.0, 42.0, 84.0]);
    }

    #[test]
    fn the_area_is_twelve_percent_as_the_spec_says() {
        assert_eq!(AREA_ALPHA, 0.12);
    }

    #[test]
    fn inner_leaves_room_for_the_dot() {
        // Spec 07-gui.md "Timeline": a 3 px dot, whatever the chart size.
        assert_eq!(dot_radius(40), 3.0);
        assert_eq!(dot_radius(84), 3.0);
        let (top, w, h) = inner(150.0, 40.0);
        assert_eq!((top, w, h), (3.0, 147.0, 36.0));
        let (top, w, h) = inner(326.0, 84.0);
        assert_eq!((top, w, h), (3.0, 323.0, 80.0));
        let (_, w, h) = inner(2.0, 2.0);
        assert!(w >= 0.0 && h >= 0.0, "never negative");
    }

    /// The alert colour starts exactly on the threshold line: a piece above
    /// it begins and ends at the interpolated crossing.
    #[test]
    fn split_above_cuts_at_the_crossing() {
        // Threshold at y 10 (smaller y = higher value).
        let run = [(0.0, 20.0), (10.0, 0.0), (20.0, 0.0), (30.0, 20.0)];
        assert_eq!(
            split_above(&run, 10.0),
            vec![vec![(5.0, 10.0), (10.0, 0.0), (20.0, 0.0), (25.0, 10.0)]]
        );
        // Two separate excursions.
        let run = [(0.0, 5.0), (10.0, 15.0), (20.0, 5.0)];
        assert_eq!(
            split_above(&run, 10.0),
            vec![
                vec![(0.0, 5.0), (5.0, 10.0)],
                vec![(15.0, 10.0), (20.0, 5.0)]
            ]
        );
        // All below, or exactly on the line: nothing in alert.
        assert!(split_above(&[(0.0, 20.0), (10.0, 30.0)], 10.0).is_empty());
        assert!(split_above(&[(0.0, 10.0), (10.0, 10.0)], 10.0).is_empty());
        // A lone sample above.
        assert_eq!(split_above(&[(3.0, 1.0)], 10.0), vec![vec![(3.0, 1.0)]]);
        assert!(split_above(&[], 10.0).is_empty());
    }

    #[test]
    fn split_above_on_a_scaled_series() {
        // 0, 20, 5 on a scale of 25 with the threshold at 10, 100 x 40 px.
        let runs = points(&[0.0, 20.0, 5.0], 3, 25.0, 100.0, 40.0);
        let yt = value_y(10.0, 25.0, 40.0);
        assert_eq!(yt, 24.0);
        let above = split_above(&runs[0], yt);
        assert_eq!(above.len(), 1);
        let piece = &above[0];
        assert_eq!(piece[0], (25.0, 24.0));
        assert_eq!(piece[1], (50.0, 8.0));
        assert!((piece[2].0 - (50.0 + 50.0 * 10.0 / 15.0)).abs() < 1e-4);
        assert_eq!(piece[2].1, 24.0);
    }

    #[test]
    fn stacked_sums_and_keeps_gaps() {
        let s = stacked(&[1.0, 2.0, f32::NAN, 4.0], &[0.5, f32::NAN, 1.0, 0.0]);
        assert_eq!(s[0], 1.5);
        assert!(s[1].is_nan() && s[2].is_nan());
        assert_eq!(s[3], 4.0);
        assert!(stacked(&[], &[]).is_empty());
    }

    #[test]
    fn band_polygon_runs_along_the_total_and_back() {
        let lower = [1.0, 1.0, f32::NAN, 2.0];
        let total = stacked(&lower, &[1.0, 3.0, 1.0, 2.0]);
        assert_eq!(both_finite(&lower, &total), vec![0..2, 3..4]);
        // 4 samples over 30 px (step 10), scale 4 over 40 px.
        let p = band_polygon(&lower, &total, 0..2, 4, 4.0, 30.0, 40.0);
        assert_eq!(p, vec![(0.0, 20.0), (10.0, 0.0), (10.0, 30.0), (0.0, 30.0)]);
        let p = band_polygon(&lower, &total, 3..4, 4, 4.0, 30.0, 40.0);
        assert_eq!(p, vec![(30.0, 0.0), (30.0, 20.0)]);
        assert!(band_polygon(&lower, &total, 2..9, 4, 4.0, 30.0, 40.0).is_empty());
    }

    #[test]
    fn newest_point_sits_on_the_right_edge() {
        assert_eq!(
            newest_point(&[1.0, 2.0], 4.0, 100.0, 40.0),
            Some((100.0, 20.0))
        );
        assert_eq!(
            newest_point(&[1.0, f32::NAN], 4.0, 100.0, 40.0),
            None,
            "gap"
        );
        assert_eq!(newest_point(&[], 4.0, 100.0, 40.0), None);
    }

    #[test]
    fn tick_labels_have_no_trailing_zeros() {
        assert_eq!(tick_label(0.0), "0");
        assert_eq!(tick_label(0.5), "0.5");
        assert_eq!(tick_label(1.0), "1");
        assert_eq!(tick_label(1.5), "1.5");
        assert_eq!(tick_label(12.5), "12.5");
        assert_eq!(tick_label(25.0), "25");
        assert_eq!(tick_label(100.0), "100");
        assert_eq!(tick_label(0.25), "0.25");
    }

    #[test]
    fn axis_labels_for_ten_minutes_and_less() {
        assert_eq!(
            axis_labels(HISTORY_LEN as u64),
            ["\u{2212}10 min", "\u{2212}5", "now"]
        );
        assert_eq!(axis_labels(300), ["\u{2212}5 min", "\u{2212}2.5", "now"]);
        assert_eq!(axis_labels(30), ["\u{2212}30 s", "\u{2212}15", "now"]);
    }

    /// Repaint only when values or scale change; the y labels follow the
    /// scale and the time axis is optional.
    #[test]
    fn set_values_repaints_only_on_change_and_relabels_the_scale() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let t = Timeline::new(DETAIL_SIZE.0, DETAIL_SIZE.1);
            t.set_scale(Scale::Nice {
                floor: 1.0,
                binary: false,
            });
            t.set_y_labels(true);
            t.set_axis(Some(HISTORY_LEN as u64));
            let v = [0.2, f32::NAN, 0.4];
            assert!(t.set_values(&v, None, None, HISTORY_LEN));
            assert!(!t.set_values(&v, None, None, HISTORY_LEN), "same values");
            assert_eq!(t.scale_top(), 1.0);
            let (y, axis) = t.labels();
            assert_eq!(y, ["1", "0.5", "0"]);
            assert_eq!(axis, ["\u{2212}10 min", "\u{2212}5", "now"]);
            // The stacked band raises the scale: 0.4 + 1.2 = 1.6 -> 2.
            assert!(t.set_values(&v, None, Some(&[0.1, 0.1, 1.2]), HISTORY_LEN));
            assert_eq!(t.scale_top(), 2.0);
            assert_eq!(t.labels().0, ["2", "1", "0"]);
            // A second series counts for the scale too.
            assert!(t.set_values(&v, Some(&[3.0, 0.0, 0.0]), None, HISTORY_LEN));
            assert_eq!(t.scale_top(), 5.0);
            t.set_alert(true);
            assert!(t.has_css_class("alert"));
            t.set_alert(false);
            assert!(!t.has_css_class("alert"));
        });
    }

    // ---- RFC-AC-003: colour that carries state --------------------------------------------

    const TOKENS: &str = include_str!("../resources/tokens.css");
    const TIMELINE_CSS: &str = include_str!("../resources/timeline.css");
    const SELF_SRC: &str = include_str!("timeline.rs");

    /// Value of `--name` in the top (light) `:root`, or in the dark
    /// `@media` block when `dark`.
    fn token(name: &str, dark: bool) -> Option<String> {
        let split = TOKENS.find("@media (prefers-color-scheme: dark)")?;
        let block = if dark {
            &TOKENS[split..]
        } else {
            &TOKENS[..split]
        };
        let at = block.find(&format!("{name}:"))? + name.len() + 1;
        let end = block[at..].find(';')?;
        Some(block[at..at + end].trim().to_owned())
    }

    /// Resolves one `var(--x)` hop to the primitive's hex.
    fn hex(name: &str, dark: bool) -> Option<String> {
        let v = token(name, dark).or_else(|| token(name, false))?;
        match v.strip_prefix("var(").and_then(|r| r.strip_suffix(')')) {
            Some(inner) => token(inner.trim(), false),
            None => Some(v),
        }
    }

    #[test]
    fn idle_is_blue_in_both_themes_through_the_token() {
        assert_eq!(
            hex("--ac-mark-fill-idle", false).as_deref(),
            Some("#2b72a8")
        );
        assert_eq!(hex("--ac-mark-fill-idle", true).as_deref(), Some("#2b72a8"));
        assert!(!TOKENS.contains("#6c7286"), "the idle grey is gone");
        assert_eq!(
            token("--ac-on-mark-fill-idle-text", false).as_deref(),
            Some("#ffffff")
        );
    }

    #[test]
    fn the_ok_mark_is_green_dark_and_light() {
        assert_eq!(hex("--ac-mark-ok", true).as_deref(), Some("#3fb37f"));
        assert_eq!(hex("--ac-mark-ok", false).as_deref(), Some("#1e7d4b"));
    }

    #[test]
    fn the_ok_pen_reads_the_ok_token() {
        assert!(
            PEN_CLASSES.contains(&"ok"),
            "a pen node carries the ok colour"
        );
        let pen = TIMELINE_CSS
            .split("timeline > .pen.ok")
            .nth(1)
            .expect("timeline.css styles the ok pen");
        assert!(pen.split('}').next().unwrap().contains("var(--ac-mark-ok)"));
    }

    #[test]
    fn series_are_no_longer_documented_as_monochrome() {
        let old = ["Series are monochrome", " ink; colour only means alert"].concat();
        assert!(!SELF_SRC.contains(&old), "timeline.rs doc comment");
        assert!(
            !TIMELINE_CSS.contains("Series are monochrome"),
            "timeline.css header"
        );
        assert!(
            SELF_SRC.contains("green under the limit"),
            "the doc comment states the two-level rule"
        );
    }

    #[test]
    fn same_counts_missing_samples_as_equal() {
        assert!(same(&[1.0, f32::NAN], &[1.0, f32::NAN]));
        assert!(!same(&[1.0, f32::NAN], &[1.0, 0.0]));
        assert!(!same(&[1.0], &[1.0, 1.0]));
    }
}
