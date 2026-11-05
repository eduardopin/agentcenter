//! Machine strip (direction B top band): CPU, RAM, Swap and Memory stall,
//! then the open-sessions gauge and the context gauge, two ring cells stacked in
//! the last column. Each of the first four
//! has a title, a big readout (Ubuntu Sans Mono 20 bold) beside a 150x40
//! [`Timeline`] of the last 10 minutes from [`MachineHistory`], and one sub
//! line under both, so the chart never cuts it.
//!
//! Swap and Memory stall have a limit ([`SWAP_LIMIT_PCT`], [`STALL_LIMIT_PCT`]):
//! over it the title gets the waiting triangle and the readout turns crimson,
//! so colour is never the only channel. Every chart is drawn in two levels:
//! green under its limit, crimson over it, with a gradient area under the
//! line. Swap and the RAM chart turn crimson as a whole (swap over its limit,
//! RAM while memory stall is over its own, the RAM bar's rule); CPU has no
//! limit and stays green. The stall chart draws its limit as a dashed
//! threshold and paints the part above it crimson.
//!
//! Under 600 px the row reflows to a 2-column grid (the two ring cells share
//! the third row) ([`MachineStrip::set_compact`],
//! meant for an AdwBreakpoint setter): smaller charts, no sub line.
//!
//! The text is pure ([`cells`], [`grid_position`]) and tested without a
//! display.

use crate::context_gauge::ContextGauge;
use crate::format::ABSENT;
use crate::machine_history::{MachineHistory, Readout};
use crate::session_gauge::{Counts, SessionGauge};
use crate::timeline::{STRIP_SIZE, Scale, Timeline};
use agentcenter_core::Snapshot;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::Cell;

/// Swap used above this % is a problem.
pub const SWAP_LIMIT_PCT: f64 = 50.0;
/// Tasks stalled on memory (PSI some avg10) above this % is a problem.
pub const STALL_LIMIT_PCT: f64 = 10.0;
/// Chart size in the 2x2 layout (B mockup, 360 px).
pub const COMPACT_SIZE: (i32, i32) = (64, 28);
/// Chart size in one row below 1000 px (B mockup, 840 px): the four cells
/// and the sessions cell have to fit the window, so the chart gives up
/// width, not the text.
pub const MEDIUM_SIZE: (i32, i32) = (60, 40);
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the window follows its breakpoint; read by tests")
)]
/// Below this window width the strip goes 2x2.
pub const COMPACT_BELOW_PX: i32 = 600;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Text of one cell.
#[derive(Debug, Clone, PartialEq)]
pub struct CellText {
    pub title: &'static str,
    pub value: String,
    pub sub: String,
    /// Over the cell's limit: triangle on the title, readout in alert.
    pub over: bool,
}

/// GiB with one decimal, no unit: `29.3`.
pub fn gib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / GIB)
}

/// Share of `part` in `total`, 0..=1, when both are known and total is not 0.
fn share(part: Option<u64>, total: Option<u64>) -> Option<f64> {
    let (part, total) = (part?, total?);
    (total > 0).then(|| (part as f64 / total as f64).min(1.0))
}

/// The RAM bar gauge: fill is RAM in use over total; the text and the
/// accessible label carry absolute and percent values for both used and
/// available, a dash for what is unknown (never 0).
#[derive(Debug, Clone, PartialEq)]
pub struct RamGauge {
    /// Tooltip and accessible label: `RAM 29.3 / 38.0 GiB used (77 %) \u{b7}
    /// 8.7 GiB available (23 %)`, then `\u{b7} memory stall over limit` under pressure.
    pub text: String,
    /// The line shown under the bar in every layout:
    /// `29.3 / 38.0 GiB \u{b7} 77 % used \u{b7} 8.7 GiB (23 %) free`.
    pub visible: String,
    /// Used share, 0..=1; `None` hides the fill.
    pub used_share: Option<f64>,
    /// Memory stall over its limit: the bar turns to the alert colour.
    pub pressure: bool,
}

pub fn ram_gauge(r: &Readout) -> RamGauge {
    let gib_or = |b: Option<u64>| b.map_or_else(|| ABSENT.to_owned(), gib);
    let pct =
        |s: Option<f64>| s.map_or_else(|| ABSENT.to_owned(), |v| format!("{:.0} %", v * 100.0));
    let used = share(r.mem_used, r.mem_total);
    let avail = share(r.mem_available, r.mem_total);
    let pressure = r.stall_some.is_some_and(|p| p > STALL_LIMIT_PCT);
    let available = r
        .mem_available
        .map_or_else(|| ABSENT.to_owned(), |a| format!("{} GiB", gib(a)));
    let mut text = format!(
        "RAM {} / {} GiB used ({}) \u{b7} {available} available ({})",
        gib_or(r.mem_used),
        gib_or(r.mem_total),
        pct(used),
        pct(avail),
    );
    if pressure {
        // The bar's colour in words, for whoever cannot see it.
        text.push_str(" \u{b7} memory stall over limit");
    }
    let free = match r.mem_available {
        Some(_) => format!("{available} ({})", pct(avail)),
        None => ABSENT.to_owned(),
    };
    let visible = format!(
        "{} / {} GiB \u{b7} {} used \u{b7} {free} free",
        gib_or(r.mem_used),
        gib_or(r.mem_total),
        pct(used),
    );
    RamGauge {
        text,
        visible,
        used_share: used,
        pressure,
    }
}

/// The four cells, in order: CPU, RAM, Swap, Memory stall.
pub fn cells(r: &Readout) -> [CellText; 4] {
    let opt = |v: Option<String>| v.unwrap_or_else(|| ABSENT.to_owned());
    let one = |v: Option<f64>| opt(v.map(|x| format!("{x:.1}")));

    let cores = if r.cores == 1 {
        "1 core".to_owned()
    } else {
        format!("{} cores", r.cores)
    };
    let cpu = CellText {
        title: "CPU",
        value: opt(r.cpu_pct.map(|v| format!("{v:.0} %"))),
        sub: match r.sessions_cores {
            Some(c) => format!("{cores} \u{b7} sessions {c:.1}"),
            None => cores,
        },
        over: false,
    };

    // The RAM cell's numbers are the gauge's visible line, not a sub line.
    let ram = CellText {
        title: "RAM",
        value: opt(r.mem_used.map(|u| format!("{} GiB", gib(u)))),
        sub: String::new(),
        over: false,
    };

    let swap_over = r.swap_pct.is_some_and(|p| p > SWAP_LIMIT_PCT);
    let swap = match (r.swap_total, r.swap_used, r.swap_pct) {
        (Some(0), ..) => CellText {
            title: "Swap",
            value: format!("{} GiB", gib(0)),
            sub: "no swap".to_owned(),
            over: false,
        },
        (total, used, pct) => CellText {
            title: "Swap",
            value: opt(used.map(|u| format!("{} GiB", gib(u)))),
            sub: match (total, pct) {
                (Some(t), Some(p)) => format!("of {} GiB \u{b7} {p:.0} % used", gib(t)),
                (Some(t), None) => format!("of {} GiB", gib(t)),
                (None, _) => String::new(),
            },
            over: swap_over,
        },
    };

    let no_psi = r.stall_some.is_none()
        && r.stall_full.is_none()
        && r.io_some.is_none()
        && r.cpu_some.is_none();
    let stall = CellText {
        title: "Memory stall",
        value: opt(r.stall_some.map(|v| format!("{v:.1} %"))),
        sub: if no_psi {
            "no PSI on this kernel".to_owned()
        } else {
            format!(
                "full {} \u{b7} io {} \u{b7} cpu {}",
                one(r.stall_full),
                one(r.io_some),
                one(r.cpu_some)
            )
        },
        over: r.stall_some.is_some_and(|p| p > STALL_LIMIT_PCT),
    };
    [cpu, ram, swap, stall]
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the window follows its breakpoint; read by tests")
)]
/// Whether a strip `width` px wide should be 2x2 (the AdwBreakpoint
/// condition is `max-width: 599px`).
pub fn compact_for(width: i32) -> bool {
    width < COMPACT_BELOW_PX
}

/// Grid column and row of cell `i`. Row: the fifth and sixth, the two ring
/// gauges (sessions, context), share the last column, one above the other.
/// 2 columns when compact: the ring gauges sit side by side in the third row,
/// so the strip is no taller than with the sessions gauge alone.
pub fn grid_position(i: usize, compact: bool) -> (i32, i32) {
    match (i, compact) {
        (4, false) => (4, 0),
        (5, false) => (4, 1),
        (_, false) => (i as i32, 0),
        (_, true) => ((i % 2) as i32, if i < 4 { (i / 2) as i32 } else { 2 }),
    }
}

/// Rows a cell spans in the row layout: the four stat cells stand beside the
/// two stacked ring gauges.
fn row_span(i: usize, compact: bool) -> i32 {
    if compact || i >= 4 { 1 } else { 2 }
}

// ---- Triangle glyph ---------------------------------------------------------

mod glyph {
    use super::*;

    #[derive(Default)]
    pub struct Triangle;

    #[glib::object_subclass]
    impl ObjectSubclass for Triangle {
        const NAME: &'static str = "AcStripTriangle";
        type Type = super::Triangle;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("glyph");
            klass.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for Triangle {}

    impl WidgetImpl for Triangle {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (10, 10, -1, -1)
        }

        fn snapshot(&self, s: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let side = w.min(h);
            let (x, y) = ((w - side) / 2.0, (h - side) / 2.0);
            let b = gtk::gsk::PathBuilder::new();
            b.move_to(x + side / 2.0, y);
            b.line_to(x + side, y + side * 0.9);
            b.line_to(x, y + side * 0.9);
            b.close();
            s.append_fill(&b.to_path(), gtk::gsk::FillRule::Winding, &obj.color());
        }
    }
}

glib::wrapper! {
    /// The "needs you" triangle of direction B. CSS: `glyph`.
    pub struct Triangle(ObjectSubclass<glyph::Triangle>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Triangle {
    fn default() -> Self {
        glib::Object::new()
    }
}

// ---- Strip ------------------------------------------------------------------

struct StripCell {
    root: gtk::Grid,
    glyph: Triangle,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the window follows its breakpoint; read by tests")
    )]
    title: gtk::Label,
    value: gtk::Label,
    sub: gtk::Label,
    chart: Timeline,
    /// The RAM cell's bar gauge; hidden in the others.
    gauge: gtk::LevelBar,
    /// The RAM cell's visible numbers, under the bar.
    gauge_text: gtk::Label,
    compact: Cell<bool>,
}

impl StripCell {
    fn new(title: &str, last: bool) -> Self {
        // Title and readout beside the chart, the sub line under both: the
        // chart never takes width from the sub line, which only ellipsizes
        // when the whole cell is narrower than it.
        let root = gtk::Grid::new();
        root.set_column_spacing(8);
        root.add_css_class("cell");
        if last {
            root.add_css_class("last");
        }
        root.set_hexpand(true);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.add_css_class("text");
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let glyph = Triangle::default();
        glyph.set_valign(gtk::Align::Center);
        glyph.set_visible(false);
        let title_l = gtk::Label::new(Some(title));
        title_l.add_css_class("title");
        title_l.set_xalign(0.0);
        head.append(&glyph);
        head.append(&title_l);
        let value = gtk::Label::new(Some(ABSENT));
        value.add_css_class("value");
        value.set_xalign(0.0);
        let sub = gtk::Label::new(None);
        sub.add_css_class("sub");
        sub.set_xalign(0.0);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&head);
        text.append(&value);
        let chart = Timeline::new(STRIP_SIZE.0, STRIP_SIZE.1);
        chart.set_valign(gtk::Align::Center);
        chart.update_property(&[gtk::accessible::Property::Label(&format!(
            "{title}, last 10 minutes"
        ))]);
        root.attach(&text, 0, 0, 1, 1);
        root.attach(&chart, 1, 0, 1, 1);
        let gauge = gtk::LevelBar::for_interval(0.0, 1.0);
        gauge.add_css_class("ram-gauge");
        gauge.set_visible(false);
        gauge.set_hexpand(true);
        root.attach(&gauge, 0, 1, 2, 1);
        let gauge_text = gtk::Label::new(None);
        gauge_text.add_css_class("sub");
        gauge_text.add_css_class("gauge-text");
        gauge_text.set_xalign(0.0);
        gauge_text.set_wrap(true);
        gauge_text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        gauge_text.set_max_width_chars(24);
        gauge_text.set_visible(false);
        root.attach(&gauge_text, 0, 2, 2, 1);
        root.attach(&sub, 0, 3, 2, 1);
        Self {
            root,
            glyph,
            title: title_l,
            value,
            sub,
            chart,
            gauge,
            gauge_text,
            compact: Cell::new(false),
        }
    }

    fn show(&self, t: &CellText) {
        self.value.set_label(&t.value);
        self.sub.set_label(&t.sub);
        self.sub
            .set_visible(!t.sub.is_empty() && !self.compact.get());
        self.glyph.set_visible(t.over);
        if t.over {
            self.root.add_css_class("over");
        } else {
            self.root.remove_css_class("over");
        }
    }
}

/// The open-sessions cell: title and sub line beside the ring button.
struct SessionsCell {
    root: gtk::Grid,
    gauge: SessionGauge,
    sub: gtk::Label,
}

impl SessionsCell {
    fn new() -> Self {
        let root = gtk::Grid::new();
        root.set_column_spacing(8);
        root.add_css_class("cell");
        root.add_css_class("last");
        root.add_css_class("ring-cell");
        root.set_hexpand(true);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        let title = gtk::Label::new(Some("Sessions"));
        title.add_css_class("title");
        title.set_xalign(0.0);
        let sub = gtk::Label::new(None);
        sub.add_css_class("sub");
        sub.set_xalign(0.0);
        sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&title);
        text.append(&sub);
        let gauge = SessionGauge::new();
        gauge.button.set_halign(gtk::Align::End);
        gauge.button.set_valign(gtk::Align::Center);
        root.attach(&text, 0, 0, 1, 1);
        root.attach(&gauge.button, 1, 0, 1, 1);
        Self { root, gauge, sub }
    }
}

pub struct MachineStrip {
    pub widget: gtk::Grid,
    cells: [StripCell; 4],
    sessions: SessionsCell,
    context: ContextGauge,
    compact: Cell<bool>,
    medium: Cell<bool>,
}

impl Default for MachineStrip {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineStrip {
    pub fn new() -> Self {
        let widget = gtk::Grid::new();
        widget.add_css_class("machine-strip");
        // Row: the small sessions cell keeps to its own width, the four
        // stat cells share the rest; 2 columns: equal.
        widget.set_column_homogeneous(false);
        widget.set_hexpand(true);
        let titles = ["CPU", "RAM", "Swap", "Memory stall"];
        let cells = titles.map(|t| StripCell::new(t, false));
        for (i, c) in cells.iter().enumerate() {
            let (col, row) = grid_position(i, false);
            widget.attach(&c.root, col, row, 1, row_span(i, false));
            c.chart.set_scale(Scale::Fixed(100.0));
            c.chart.add_css_class("area");
        }
        let sessions = SessionsCell::new();
        let (col, row) = grid_position(4, false);
        widget.attach(&sessions.root, col, row, 1, 1);
        let context = ContextGauge::new();
        let (col, row) = grid_position(5, false);
        widget.attach(&context.root, col, row, 1, 1);
        let stall = &cells[3].chart;
        stall.set_scale(Scale::Nice {
            floor: 20.0,
            binary: false,
        });
        stall.set_threshold(Some(STALL_LIMIT_PCT));
        Self {
            widget,
            cells,
            sessions,
            context,
            compact: Cell::new(false),
            medium: Cell::new(false),
        }
    }

    /// Text from the newest snapshot, charts from the history.
    pub fn update(&self, snap: &Snapshot, history: &MachineHistory) {
        let texts = cells(&Readout::of(snap));
        for (c, t) in self.cells.iter().zip(&texts) {
            c.show(t);
        }
        let cap = history.capacity();
        let series = [
            &history.cpu_pct,
            &history.ram_pct,
            &history.swap_pct,
            &history.stall_pct,
        ];
        for (c, ring) in self.cells.iter().zip(series) {
            c.chart.set_values(&ring.to_vec(), None, None, cap);
        }
        let r = Readout::of(snap);
        let ram = ram_gauge(&r);
        // Two levels: green is the `ok` class, crimson is `alert`; the stall
        // chart stays green and its threshold split paints the excess. A
        // reading that is unknown gets neither: no data is not "fine".
        let alert = [false, ram.pressure, texts[2].over, false];
        let known = [
            r.cpu_pct.is_some(),
            r.stall_some.is_some(),
            r.swap_pct.is_some(),
            r.stall_some.is_some(),
        ];
        for ((c, a), k) in self.cells.iter().zip(alert).zip(known) {
            c.chart.set_alert(a);
            if k && !a {
                c.chart.add_css_class("ok");
            } else {
                c.chart.remove_css_class("ok");
            }
        }
        self.show_ram_gauge(&ram);
        self.sessions.gauge.update(snap);
        self.context.update(snap);
        self.sessions.sub.set_label(&Counts::of(snap).slots());
    }

    fn show_ram_gauge(&self, g: &RamGauge) {
        let bar = &self.cells[1].gauge;
        bar.set_visible(g.used_share.is_some());
        bar.set_value(g.used_share.unwrap_or(0.0));
        if g.pressure {
            bar.add_css_class("pressure");
        } else {
            bar.remove_css_class("pressure");
        }
        let text = &self.cells[1].gauge_text;
        text.set_visible(g.used_share.is_some() || !g.visible.is_empty());
        text.set_label(&g.visible);
        bar.set_tooltip_text(Some(&g.text));
        bar.update_property(&[gtk::accessible::Property::Label(&g.text)]);
        self.cells[1].value.set_tooltip_text(Some(&g.text));
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    pub fn context(&self) -> &ContextGauge {
        &self.context
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    pub fn ram_gauge_text(&self) -> &gtk::Label {
        &self.cells[1].gauge_text
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    pub fn ram_gauge_widget(&self) -> &gtk::LevelBar {
        &self.cells[1].gauge
    }

    /// Below 1000 px, one row: narrower charts so four cells fit.
    pub fn set_medium(&self, medium: bool) {
        if self.medium.replace(medium) != medium {
            self.apply_chart_size();
        }
    }

    /// 2x2 wins over the narrower row, which wins over the full chart.
    fn apply_chart_size(&self) {
        let (w, h) = if self.compact.get() {
            COMPACT_SIZE
        } else if self.medium.get() {
            MEDIUM_SIZE
        } else {
            STRIP_SIZE
        };
        for c in &self.cells {
            c.chart.set_size_request(w, h);
        }
    }

    /// 2x2 (true) or one row (false); smaller charts and no sub line in 2x2.
    pub fn set_compact(&self, compact: bool) {
        if self.compact.replace(compact) == compact {
            return;
        }
        for (i, c) in self.cells.iter().enumerate() {
            self.widget.remove(&c.root);
            let (col, row) = grid_position(i, compact);
            self.widget
                .attach(&c.root, col, row, 1, row_span(i, compact));
            c.compact.set(compact);
            c.sub.set_visible(!compact && !c.sub.label().is_empty());
        }
        self.widget.remove(&self.sessions.root);
        let (col, row) = grid_position(4, compact);
        self.widget.attach(&self.sessions.root, col, row, 1, 1);
        self.sessions.sub.set_visible(!compact);
        self.widget.remove(&self.context.root);
        let (col, row) = grid_position(5, compact);
        self.widget.attach(&self.context.root, col, row, 1, 1);
        self.widget.set_column_homogeneous(compact);
        self.apply_chart_size();
        if compact {
            self.widget.add_css_class("compact");
        } else {
            self.widget.remove_css_class("compact");
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the window follows its breakpoint; read by tests")
    )]
    pub fn is_compact(&self) -> bool {
        self.compact.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_history::tests::fixture;

    fn texts(name: &str) -> [CellText; 4] {
        cells(&Readout::of(&fixture(name)))
    }

    #[test]
    fn gib_has_one_decimal() {
        assert_eq!(gib(31_460_635_444), "29.3");
        assert_eq!(gib(40_802_189_312), "38.0");
        assert_eq!(gib(0), "0.0");
    }

    /// The B mockup's top band, from tipico-40's machine values.
    #[test]
    fn typical_fixture_reads_like_the_mockup() {
        let [cpu, ram, swap, stall] = texts("tipico-40");
        assert_eq!(cpu.title, "CPU");
        assert_eq!(cpu.value, "42 %");
        assert_eq!(cpu.sub, "16 cores \u{b7} sessions 6.2");
        assert!(!cpu.over);
        assert_eq!(ram.title, "RAM");
        assert_eq!(ram.value, "29.3 GiB");
        assert_eq!(ram.sub, "", "the numbers are the gauge line");
        assert!(!ram.over);
        assert_eq!(swap.title, "Swap");
        assert_eq!(swap.value, "19.7 GiB");
        assert_eq!(swap.sub, "of 27.0 GiB \u{b7} 73 % used");
        assert!(swap.over, "73 % used is over 50 %");
        assert_eq!(stall.title, "Memory stall");
        assert_eq!(stall.value, "12.5 %");
        assert_eq!(stall.sub, "full 4.2 \u{b7} io 3.1 \u{b7} cpu 1.1");
        assert!(stall.over, "12.5 % is over 10 %");
    }

    #[test]
    fn ram_gauge_text_has_absolute_and_percent_values() {
        let g = ram_gauge(&Readout::of(&fixture("tipico-40")));
        assert_eq!(
            g.text,
            "RAM 29.3 / 38.0 GiB used (77 %) \u{b7} 8.7 GiB available (23 %) \u{b7} memory stall over limit"
        );
        assert_eq!(
            g.visible,
            "29.3 / 38.0 GiB \u{b7} 77 % used \u{b7} 8.7 GiB (23 %) free"
        );
        let used = g.used_share.expect("known");
        assert!((used - 0.771).abs() < 0.001, "fill is in use over total");
        assert!(g.pressure, "stall 12.5 % is over its limit");
    }

    #[test]
    fn ram_gauge_unknowns_are_a_dash_never_zero() {
        let mut snap = fixture("tipico-40");
        snap.machine.mem_available_bytes = None;
        let g = ram_gauge(&Readout::of(&snap));
        assert_eq!(
            g.text,
            "RAM - / 38.0 GiB used (-) \u{b7} - available (-) \u{b7} memory stall over limit"
        );
        assert_eq!(g.visible, "- / 38.0 GiB \u{b7} - used \u{b7} - free");
        assert_eq!(g.used_share, None);
        snap.machine.mem_total_bytes = None;
        snap.machine.mem_available_bytes = Some(1 << 30);
        let g = ram_gauge(&Readout::of(&snap));
        assert_eq!(
            g.text,
            "RAM - / - GiB used (-) \u{b7} 1.0 GiB available (-) \u{b7} memory stall over limit"
        );
        assert_eq!(g.visible, "- / - GiB \u{b7} - used \u{b7} 1.0 GiB (-) free");
        assert_eq!(g.used_share, None);
        snap.machine.mem_total_bytes = Some(0);
        assert_eq!(
            ram_gauge(&Readout::of(&snap)).used_share,
            None,
            "zero total"
        );
    }

    #[test]
    fn ram_gauge_pressure_follows_the_stall_limit() {
        let mut r = Readout::of(&fixture("tipico-40"));
        r.stall_some = Some(STALL_LIMIT_PCT);
        assert!(!ram_gauge(&r).pressure, "at the limit is not over");
        r.stall_some = None;
        assert!(!ram_gauge(&r).pressure);
    }

    #[test]
    fn limits_are_strictly_over() {
        let mut r = Readout::of(&fixture("tipico-40"));
        r.swap_pct = Some(SWAP_LIMIT_PCT);
        r.stall_some = Some(STALL_LIMIT_PCT);
        let [_, _, swap, stall] = cells(&r);
        assert!(!swap.over && !stall.over, "at the limit is not over");
        r.swap_pct = Some(50.1);
        r.stall_some = Some(10.1);
        let [_, _, swap, stall] = cells(&r);
        assert!(swap.over && stall.over);
    }

    #[test]
    fn missing_readings_print_a_dash_never_zero() {
        let mut snap = fixture("tipico-40");
        let m = &mut snap.machine;
        m.cpu_pct = None;
        m.mem_available_bytes = None;
        m.psi = None;
        snap.summary.sessions_cpu_pct = None;
        let [cpu, ram, _, stall] = cells(&Readout::of(&snap));
        assert_eq!(cpu.value, ABSENT);
        assert_eq!(cpu.sub, "16 cores");
        assert_eq!(ram.value, ABSENT);
        assert_eq!(ram.sub, "");
        assert_eq!(stall.value, ABSENT);
        assert_eq!(stall.sub, "no PSI on this kernel");
        assert!(!stall.over);
    }

    #[test]
    fn a_machine_without_swap_says_so() {
        let mut snap = fixture("tipico-40");
        snap.machine.swap_total_bytes = Some(0);
        snap.machine.swap_free_bytes = Some(0);
        let [_, _, swap, _] = cells(&Readout::of(&snap));
        assert_eq!(swap.value, "0.0 GiB");
        assert_eq!(swap.sub, "no swap");
        assert!(!swap.over);
    }

    #[test]
    fn partial_psi_prints_a_dash_per_missing_reading() {
        let mut snap = fixture("tipico-40");
        if let Some(p) = snap.machine.psi.as_mut() {
            p.io = None;
        }
        let [.., stall] = cells(&Readout::of(&snap));
        assert_eq!(stall.sub, "full 4.2 \u{b7} io - \u{b7} cpu 1.1");
    }

    #[test]
    fn compact_reflows_to_two_by_two() {
        assert!(compact_for(360) && compact_for(599));
        assert!(!compact_for(600) && !compact_for(1280));
        let wide: Vec<_> = (0..4).map(|i| grid_position(i, false)).collect();
        assert_eq!(wide, [(0, 0), (1, 0), (2, 0), (3, 0)]);
        let compact: Vec<_> = (0..4).map(|i| grid_position(i, true)).collect();
        assert_eq!(compact, [(0, 0), (1, 0), (0, 1), (1, 1)]);
        assert_eq!(grid_position(4, false), (4, 0), "sessions cell, row");
        assert_eq!(grid_position(4, true), (0, 2), "sessions cell, 2x2");
    }

    /// The layout tests assume a desktop-sized screen. A bare Broadway server is
    /// 1024x768 and stalls frames, so fail here, naming the cause, rather than in
    /// a width assertion elsewhere (scripts/broadway-browser.py fixes it in CI).
    #[test]
    fn the_test_display_is_at_least_1600_by_800() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let monitors = gtk::gdk::Display::default().unwrap().monitors();
            let m = monitors
                .item(0)
                .and_downcast::<gtk::gdk::Monitor>()
                .expect("test display has no monitor");
            let g = m.geometry();
            assert!(
                g.width() >= 1600 && g.height() >= 800,
                "test display is {}x{}, GUI tests need >= 1600x800: run through \
                 scripts/ci.sh (it attaches scripts/broadway-browser.py to Broadway) \
                 or point DISPLAY at a larger screen",
                g.width(),
                g.height()
            );
        });
    }

    fn history_of(snap: &Snapshot, n: usize) -> MachineHistory {
        let mut h = MachineHistory::new();
        let mut s = snap.clone();
        for i in 0..n {
            let k = i as f64 / n as f64;
            s.machine.cpu_pct = snap.machine.cpu_pct.map(|v| v * (0.6 + 0.4 * k));
            h.record(&s);
        }
        h.record(snap);
        h
    }

    fn attach(child: &gtk::Grid) -> gtk::Window {
        let w = gtk::Window::new();
        w.set_child(Some(child));
        w
    }

    #[test]
    fn update_sets_labels_glyphs_and_charts() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            let _w = attach(&strip.widget);
            let h = history_of(&snap, 30);
            strip.update(&snap, &h);
            let got: Vec<_> = strip
                .cells
                .iter()
                .map(|c| (c.title.label(), c.value.label(), c.sub.label()))
                .collect();
            assert_eq!(got[0].1, "42 %");
            assert_eq!(got[1].1, "29.3 GiB");
            assert_eq!(got[2].2, "of 27.0 GiB \u{b7} 73 % used");
            assert_eq!(got[3].0, "Memory stall");
            assert_eq!(got[3].1, "12.5 %");
            let glyphs: Vec<_> = strip.cells.iter().map(|c| c.glyph.get_visible()).collect();
            assert_eq!(glyphs, [false, false, true, true]);
            assert!(strip.cells[3].root.has_css_class("over"));
            assert!(!strip.cells[0].root.has_css_class("over"));
            assert!(strip.cells[2].chart.has_css_class("alert"));
            assert!(!strip.cells[3].chart.has_css_class("alert"));
            assert_eq!(strip.cells[3].chart.threshold(), Some(STALL_LIMIT_PCT));
            assert_eq!(strip.cells[0].chart.scale_top(), 100.0);
            assert_eq!(
                strip.cells[3].chart.scale_top(),
                20.0,
                "12.5 on a nice scale"
            );
            // Same snapshot and history again: nothing to repaint.
            assert!(!strip.cells[0].chart.set_values(
                &h.cpu_pct.to_vec(),
                None,
                None,
                h.capacity()
            ));
            // A calmer machine clears the alerts.
            let mut calm = snap.clone();
            calm.machine.swap_free_bytes = calm.machine.swap_total_bytes;
            if let Some(p) = calm.machine.psi.as_mut() {
                p.memory.as_mut().unwrap().some_avg10 = Some(0.5);
            }
            strip.update(&calm, &h);
            assert!(!strip.cells[2].glyph.get_visible());
            assert!(!strip.cells[2].chart.has_css_class("alert"));
            assert!(!strip.cells[3].root.has_css_class("over"));
        });
    }

    /// RFC-AC-003: every machine chart is green under its limit and
    /// crimson over it, and has a gradient area. CPU has no alert rule, so it
    /// stays green; RAM follows the bar gauge's pressure (stall over 10 %);
    /// swap turns crimson over 50 %;
    /// the stall chart stays "ok" as a whole and the threshold split paints
    /// the part above 10 % crimson.
    #[test]
    fn machine_charts_are_green_under_the_limit_and_crimson_over_it() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let snap = fixture("tipico-40"); // swap 73 %, stall 12.5 %
            let strip = MachineStrip::new();
            let _w = attach(&strip.widget);
            let h = history_of(&snap, 30);
            strip.update(&snap, &h);
            let ok = |i: usize| strip.cells[i].chart.has_css_class("ok");
            let alert = |i: usize| strip.cells[i].chart.has_css_class("alert");
            assert!(ok(0) && !alert(0), "CPU: no alert rule, green");
            assert!(!ok(1) && alert(1), "RAM: pressure (stall over 10 %) is on");
            assert!(!ok(2) && alert(2), "swap over 50 %: crimson, not green");
            assert!(
                ok(3) && !alert(3),
                "stall: green, the split paints the excess"
            );
            // A calmer machine: swap back under its limit turns green again.
            let mut calm = snap.clone();
            calm.machine.swap_free_bytes = calm.machine.swap_total_bytes;
            if let Some(p) = calm.machine.psi.as_mut() {
                p.memory.as_mut().unwrap().some_avg10 = Some(0.5);
            }
            strip.update(&calm, &h);
            assert!(ok(2) && !alert(2), "swap under 50 %: green");
            assert!(ok(1) && !alert(1), "no pressure: RAM green");
        });
    }

    /// The direction the other tests never see: a chart that starts calm
    /// (green), turns crimson when the machine is stressed, and goes back.
    #[test]
    fn a_chart_goes_green_then_crimson_then_green_again() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let stressed = fixture("tipico-40"); // swap 73 %, stall 12.5 %
            let mut calm = stressed.clone();
            calm.machine.swap_free_bytes = calm.machine.swap_total_bytes;
            if let Some(p) = calm.machine.psi.as_mut() {
                p.memory.as_mut().unwrap().some_avg10 = Some(0.5);
            }
            let strip = MachineStrip::new();
            let _w = attach(&strip.widget);
            let h = history_of(&stressed, 30);
            let state = |i: usize| {
                let c = &strip.cells[i].chart;
                (c.has_css_class("ok"), c.has_css_class("alert"))
            };
            strip.update(&calm, &h);
            assert_eq!(state(2), (true, false), "calm swap: green");
            assert_eq!(state(1), (true, false), "calm RAM: green");
            strip.update(&stressed, &h);
            assert_eq!(state(2), (false, true), "stressed swap: crimson, not green");
            assert_eq!(state(1), (false, true), "stressed RAM: crimson, not green");
            strip.update(&calm, &h);
            assert_eq!(state(2), (true, false), "calm again: green");
            assert_eq!(state(1), (true, false), "calm again: green");
        });
    }

    /// No reading is neither fine nor over its limit: no chart is painted
    /// green or crimson.
    #[test]
    fn a_chart_with_an_unknown_reading_is_neither_green_nor_crimson() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let mut snap = fixture("tipico-40");
            snap.machine.cpu_pct = None;
            snap.machine.psi = None;
            snap.machine.swap_total_bytes = None;
            snap.machine.swap_free_bytes = None;
            let r = Readout::of(&snap);
            assert!(r.cpu_pct.is_none() && r.swap_pct.is_none() && r.stall_some.is_none());
            let strip = MachineStrip::new();
            let _w = attach(&strip.widget);
            // Start from a known-green state so a stale `ok` would show.
            let calm = fixture("tipico-40");
            strip.update(&calm, &history_of(&calm, 30));
            strip.update(&snap, &history_of(&snap, 30));
            for c in &strip.cells {
                let ch = &c.chart;
                assert!(
                    !ch.has_css_class("ok") && !ch.has_css_class("alert"),
                    "{}",
                    c.title.label()
                );
            }
        });
    }

    /// Drawing, not class names: a pixel on the line is green in a calm chart
    /// and crimson over the limit, and the area under it is a gradient that
    /// fades toward the baseline.
    #[test]
    fn the_chart_draws_a_green_or_crimson_line_over_a_fading_area() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let provider = gtk::CssProvider::new();
            provider.load_from_string(concat!(
                include_str!("../resources/tokens.css"),
                include_str!("../resources/timeline.css"),
            ));
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            // Swap history is flat at the last reading: a plateau well above
            // the baseline, 73 % (stressed) or 40 % (calm).
            let swap_at = |used: f64| {
                let mut s = fixture("tipico-40");
                let total = s.machine.swap_total_bytes.expect("swap total");
                s.machine.swap_free_bytes = Some((total as f64 * (1.0 - used)) as u64);
                (s.clone(), history_of(&s, 30))
            };
            let strip = MachineStrip::new();
            let chart = strip.cells[2].chart.clone();
            let (snap, h) = swap_at(0.73);
            strip.update(&snap, &h);
            let win = present_at(strip.widget.upcast_ref(), 1280);
            let (_, line, _, _) = texture_probe(&chart, &win);
            assert!(chart.has_css_class("alert"));
            assert!(
                dominant(line.0, line.1, line.2),
                "over the limit the line is crimson: {line:?}"
            );

            let (snap, h) = swap_at(0.40);
            strip.update(&snap, &h);
            assert!(chart.has_css_class("ok") && !chart.has_css_class("alert"));
            let (_, line, under, bottom) = texture_probe(&chart, &win);
            assert!(
                dominant(line.1, line.0, line.2),
                "under the limit the line is green: {line:?}"
            );
            assert!(under > 0.1, "the area under the line is painted: {under}");
            assert!(
                under > bottom + 0.1,
                "the area fades toward the baseline: {under} near the line, {bottom} at the bottom"
            );
            win.destroy();
        });
    }

    /// `a` clearly above both `b` and `c`.
    fn dominant(a: u8, b: u8, c: u8) -> bool {
        u16::from(a) > u16::from(b) + 40 && u16::from(a) > u16::from(c) + 40
    }

    /// Renders `chart` (transparent background) to a texture and returns the
    /// probed column, the (r, g, b) of a pixel on the line, and the alpha
    /// (0..=1) just under the line and near the bottom of the chart.
    fn texture_probe(chart: &Timeline, win: &gtk::Window) -> (usize, (u8, u8, u8), f32, f32) {
        // Let the new classes and values reach the screen first.
        let until = std::time::Instant::now() + std::time::Duration::from_millis(150);
        let ctx = glib::MainContext::default();
        while std::time::Instant::now() < until {
            while ctx.pending() {
                ctx.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let (w, h) = (chart.width(), chart.height());
        let paintable = gtk::WidgetPaintable::new(Some(chart));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
        let node = snapshot.to_node().expect("rendered node");
        let tex = win
            .renderer()
            .expect("renderer")
            .render_texture(&node, None);
        let (w, h) = (tex.width() as usize, tex.height() as usize);
        let stride = w * 4;
        let mut px = vec![0u8; stride * h];
        tex.download(&mut px, stride); // B8G8R8A8 premultiplied
        let at = |x: usize, y: usize| {
            let i = y * stride + x * 4;
            (px[i + 2], px[i + 1], px[i], px[i + 3])
        };
        // The line is the only opaque, coloured thing (the grid is grey):
        // take its topmost such pixel, a few columns in from where it
        // starts (the newest-sample dot is at the other end).
        let hit = |x: usize| {
            (0..h).find(|&y| {
                let (r, g, b, a) = at(x, y);
                a >= 242 && r.max(g).max(b) - r.min(g).min(b) > 40
            })
        };
        let first = (0..w).find(|&x| hit(x).is_some()).expect("a line");
        let x = first + 3;
        let y = hit(x).expect("line at x");
        let (r, g, b, _) = at(x, y);
        let alpha = |y: usize| f32::from(at(x, y).3) / 255.0;
        (x, (r, g, b), alpha((y + 4).min(h - 1)), alpha(h - 2))
    }

    #[test]
    fn every_machine_chart_has_a_gradient_area() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let strip = MachineStrip::new();
            for c in &strip.cells {
                assert!(c.chart.has_css_class("area"), "{}", c.title.label());
            }
        });
    }

    /// The limits are unchanged, and the module doc no longer says the chart
    /// turns crimson as a whole.
    #[test]
    fn the_limits_the_charts_colour_by_are_swap_50_and_stall_10() {
        assert_eq!(SWAP_LIMIT_PCT, 50.0);
        assert_eq!(STALL_LIMIT_PCT, 10.0);
        let src = include_str!("machine_strip.rs");
        assert!(
            !src.contains(&["the swap chart turns crimson", " as a whole while swap is"].concat()),
            "stale doc: charts are green under the limit now"
        );
    }

    /// The RAM cell's gauge shows the numbers as tooltip and accessible
    /// label and the fill; the other cells have none; an unknown reading
    /// hides the fill.
    #[test]
    fn the_ram_cell_has_a_bar_gauge_with_numbers() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            let _w = attach(&strip.widget);
            let h = history_of(&snap, 30);
            strip.update(&snap, &h);
            let bar = strip.ram_gauge_widget();
            assert!(bar.get_visible());
            assert_eq!(
                bar.tooltip_text().as_deref(),
                Some(
                    "RAM 29.3 / 38.0 GiB used (77 %) \u{b7} 8.7 GiB available (23 %) \u{b7} memory stall over limit"
                )
            );
            assert!((bar.value() - 0.771).abs() < 0.001);
            assert!(bar.has_css_class("pressure"));
            let line = &strip.cells[1].gauge_text;
            assert!(line.get_visible());
            assert_eq!(
                line.label(),
                "29.3 / 38.0 GiB \u{b7} 77 % used \u{b7} 8.7 GiB (23 %) free"
            );
            strip.set_compact(true);
            assert!(line.get_visible(), "visible in 2x2 too");
            strip.set_compact(false);
            assert!(
                strip
                    .cells
                    .iter()
                    .enumerate()
                    .all(|(i, c)| i == 1 || !c.gauge.get_visible())
            );
            let mut calm = snap.clone();
            if let Some(p) = calm.machine.psi.as_mut() {
                p.memory.as_mut().unwrap().some_avg10 = Some(0.5);
            }
            strip.update(&calm, &h);
            assert!(!bar.has_css_class("pressure"), "calm: neutral");
            assert_eq!(
                bar.tooltip_text().as_deref(),
                Some("RAM 29.3 / 38.0 GiB used (77 %) \u{b7} 8.7 GiB available (23 %)"),
                "no pressure words when calm"
            );
            let mut unknown = snap.clone();
            unknown.machine.mem_available_bytes = None;
            strip.update(&unknown, &h);
            assert!(!bar.get_visible(), "no fill that would read as 0");
        });
    }

    /// Card 4d96cc0b: the sessions cell's sub line, and the compact layout's
    /// equal columns with the cell on the third row beside the context cell.
    #[test]
    fn the_sessions_cell_has_a_sub_line_and_spans_the_third_row_when_compact() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            strip.update(&snap, &history_of(&snap, 3));
            assert_eq!(
                strip.sessions.sub.label(),
                Counts::of(&snap).slots(),
                "the sub line is the slots text"
            );
            assert!(strip.sessions.sub.get_visible());
            let at = |s: &MachineStrip| s.widget.query_child(&s.sessions.root);
            assert_eq!(at(&strip), (4, 0, 1, 1));
            assert!(!strip.widget.is_column_homogeneous(), "row: own widths");
            strip.set_compact(true);
            assert_eq!(
                at(&strip),
                (0, 2, 1, 1),
                "third row, first column: the context cell takes the second"
            );
            assert!(strip.widget.is_column_homogeneous(), "2x2: equal columns");
            assert!(!strip.sessions.sub.get_visible(), "no sub line in 2x2");
            strip.set_compact(false);
            assert_eq!(at(&strip), (4, 0, 1, 1));
            assert!(!strip.widget.is_column_homogeneous());
            assert!(strip.sessions.sub.get_visible());
        });
    }

    /// Card 93dc0213: the sixth cell, the context gauge, after the sessions cell.
    #[test]
    fn grid_position_places_the_context_cell_after_sessions() {
        assert_eq!(grid_position(4, false), (4, 0));
        assert_eq!(
            grid_position(5, false),
            (4, 1),
            "under sessions, same column"
        );
        assert_eq!(grid_position(4, true), (0, 2));
        assert_eq!(grid_position(5, true), (1, 2), "beside sessions, same row");
    }

    #[test]
    fn the_context_cell_sits_under_sessions_in_the_row_and_beside_it_when_compact() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            strip.update(&snap, &history_of(&snap, 3));
            let at = |s: &MachineStrip| s.widget.query_child(&s.context.root);
            assert_eq!(at(&strip), (4, 1, 1, 1));
            assert_eq!(
                strip.widget.query_child(&strip.sessions.root),
                (4, 0, 1, 1),
                "sessions keeps its cell"
            );
            let cpu = |s: &MachineStrip| s.widget.query_child(&s.cells[0].root);
            assert_eq!(cpu(&strip), (0, 0, 1, 2), "stat cells span both ring rows");
            strip.set_compact(true);
            assert_eq!(at(&strip), (1, 2, 1, 1), "third row, second column");
            assert_eq!(
                strip.widget.query_child(&strip.sessions.root),
                (0, 2, 1, 1),
                "sessions on the third row too"
            );
            assert_eq!(cpu(&strip), (0, 0, 1, 1), "2x2: one row each");
            strip.set_compact(false);
            assert_eq!(at(&strip), (4, 1, 1, 1));
            assert_eq!(cpu(&strip), (0, 0, 1, 2));
            assert!(
                strip.sessions.root.has_css_class("last"),
                "both ring cells end the row"
            );
            assert!(strip.context.root.has_css_class("last"));
        });
    }

    /// Owner rule: the % and the session name are on screen, not only in the
    /// tooltip, in the row and in 2x2.
    #[test]
    fn the_context_cell_shows_the_percent_and_the_session_name_in_both_layouts() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            strip.update(&snap, &history_of(&snap, 3));
            let (value, name) = (strip.context.value_label(), strip.context.name_label());
            for compact in [false, true] {
                strip.set_compact(compact);
                assert_eq!(value.label(), "96%", "compact: {compact}");
                assert_eq!(name.label(), "sable-export-csv", "compact: {compact}");
                assert!(
                    value.get_visible() && name.get_visible(),
                    "compact: {compact}"
                );
            }
            assert!(value.is_ancestor(&strip.context.root));
            assert!(name.is_ancestor(&strip.context.root));
        });
    }

    #[test]
    fn set_compact_moves_cells_and_hides_the_sub_line() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let strip = MachineStrip::new();
            let snap = fixture("tipico-40");
            strip.update(&snap, &history_of(&snap, 3));
            let pos = |s: &MachineStrip| -> Vec<(i32, i32)> {
                s.cells
                    .iter()
                    .map(|c| {
                        let (col, row, _, _) = s.widget.query_child(&c.root);
                        (col, row)
                    })
                    .collect()
            };
            assert!(!strip.is_compact());
            assert_eq!(pos(&strip), [(0, 0), (1, 0), (2, 0), (3, 0)]);
            strip.set_compact(true);
            assert!(strip.is_compact());
            assert!(strip.widget.has_css_class("compact"));
            assert_eq!(pos(&strip), [(0, 0), (1, 0), (0, 1), (1, 1)]);
            assert!(strip.cells.iter().all(|c| !c.sub.get_visible()));
            assert_eq!(strip.cells[0].chart.size_request(), COMPACT_SIZE);
            strip.set_compact(true);
            assert_eq!(pos(&strip), [(0, 0), (1, 0), (0, 1), (1, 1)], "idempotent");
            strip.set_compact(false);
            assert_eq!(pos(&strip), [(0, 0), (1, 0), (2, 0), (3, 0)]);
            // The RAM cell has no sub line: its numbers are the gauge line.
            assert!(
                strip
                    .cells
                    .iter()
                    .all(|c| c.sub.get_visible() == !c.sub.label().is_empty())
            );
            assert!(strip.cells[0].sub.get_visible() && !strip.cells[1].sub.get_visible());
            assert_eq!(strip.cells[0].chart.size_request(), STRIP_SIZE);
            assert!(!strip.widget.has_css_class("compact"));
        });
    }

    #[test]
    fn the_row_fits_840_px_when_medium() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let provider = gtk::CssProvider::new();
            provider.load_from_string(concat!(
                include_str!("../resources/tokens.css"),
                include_str!("../resources/timeline.css"),
            ));
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            strip.update(&snap, &history_of(&snap, 30));
            let _w = attach(&strip.widget);
            let wide = strip.widget.measure(gtk::Orientation::Horizontal, -1).0;
            strip.set_medium(true);
            let medium = strip.widget.measure(gtk::Orientation::Horizontal, -1).0;
            assert!(medium <= 840, "medium row needs {medium} px (wide: {wide})");
            assert_eq!(strip.cells[0].chart.size_request(), MEDIUM_SIZE);
            strip.set_medium(false);
            assert_eq!(strip.cells[0].chart.size_request(), STRIP_SIZE);
            strip.set_compact(true);
            strip.set_medium(true);
            assert_eq!(
                strip.cells[0].chart.size_request(),
                COMPACT_SIZE,
                "2x2 wins"
            );
        });
    }

    #[test]
    fn sub_lines_are_never_cut_at_1280() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            // The real fonts and paddings decide the widths.
            let provider = gtk::CssProvider::new();
            provider.load_from_string(concat!(
                include_str!("../resources/tokens.css"),
                include_str!("../resources/timeline.css"),
            ));
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            let snap = fixture("tipico-40");
            let strip = MachineStrip::new();
            strip.update(&snap, &history_of(&snap, 30));
            let w = present_at(strip.widget.upcast_ref(), 1280);
            for c in &strip.cells {
                let (_, natural, _, _) = c.sub.measure(gtk::Orientation::Horizontal, -1);
                assert!(
                    c.sub.width() >= natural,
                    "{:?} gets {} px of the {natural} px it needs",
                    c.sub.label(),
                    c.sub.width()
                );
            }
            w.destroy();
        });
    }

    /// Visual proof, not a check: renders the strip at 1280 and 360 px and a
    /// 360x112 detail Timeline (hatched band and threshold) in both themes
    /// into target/design/impl/timeline-strip/. Run with
    /// `cargo test -p agentcenter render_timeline_strip_pngs -- --ignored`.
    #[test]
    #[ignore = "writes PNGs for the design review; run on demand"]
    fn render_timeline_strip_pngs() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/design/impl/timeline-strip");
            std::fs::create_dir_all(&out).unwrap();
            let provider = gtk::CssProvider::new();
            provider.load_from_string(concat!(
                include_str!("../resources/tokens.css"),
                include_str!("../resources/timeline.css"),
            ));
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            let snap = fixture("tipico-40");
            let h = synthetic_history(&snap);
            for (theme, scheme, pref) in [
                (
                    "dark",
                    adw::ColorScheme::ForceDark,
                    gtk::InterfaceColorScheme::Dark,
                ),
                (
                    "light",
                    adw::ColorScheme::ForceLight,
                    gtk::InterfaceColorScheme::Light,
                ),
            ] {
                adw::StyleManager::default().set_color_scheme(scheme);
                provider.set_prefers_color_scheme(pref);
                for (width, compact) in [(1280, false), (360, true)] {
                    let strip = MachineStrip::new();
                    strip.set_compact(compact);
                    strip.update(&snap, &h);
                    let png = out.join(format!("strip-{width}-{theme}.png"));
                    render(strip.widget.upcast_ref(), width, &png);
                }
                let t = Timeline::new(
                    crate::timeline::DETAIL_SIZE.0,
                    crate::timeline::DETAIL_SIZE.1,
                );
                t.set_scale(Scale::Nice {
                    floor: 1.0,
                    binary: false,
                });
                t.set_y_labels(true);
                t.set_axis(Some(h.capacity() as u64));
                t.set_threshold(Some(0.7));
                let n = h.capacity();
                let pss: Vec<f32> = (0..n)
                    .map(|i| 0.78 - 0.23 * i as f32 / n as f32 + 0.02 * wave(i, 7))
                    .collect();
                let swap: Vec<f32> = (0..n)
                    .map(|i| 0.30 + 0.25 * i as f32 / n as f32 + 0.015 * wave(i, 11))
                    .collect();
                t.set_values(&pss, None, Some(&swap), n);
                let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
                card.set_margin_top(16);
                card.set_margin_bottom(16);
                card.set_margin_start(16);
                card.set_margin_end(16);
                card.append(&t);
                render(
                    card.upcast_ref(),
                    392,
                    &out.join(format!("detail-timeline-{theme}.png")),
                );
            }
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
            gtk::style_context_remove_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
            );
        });
    }

    /// Deterministic noise in -1..=1 (an integer hash of `i` and `seed`).
    fn wave(i: usize, seed: usize) -> f32 {
        let mut x = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (seed as u64) << 32;
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 32;
        (x % 2001) as f32 / 1000.0 - 1.0
    }

    /// Ten minutes that end on the fixture's readings: CPU around 40 %, RAM
    /// near 77 %, swap climbing to 73 %, memory stall in spikes.
    fn synthetic_history(snap: &Snapshot) -> MachineHistory {
        let mut h = MachineHistory::new();
        let n = h.capacity();
        let m = &snap.machine;
        let (total, swap_total) = (
            m.mem_total_bytes.unwrap() as f64,
            m.swap_total_bytes.unwrap() as f64,
        );
        let mut s = snap.clone();
        let mut cpu = 40.0;
        for i in 0..n - 1 {
            let k = i as f64 / n as f64;
            cpu = (cpu + 2.5 * f64::from(wave(i, 1))).clamp(30.0, 55.0);
            s.machine.cpu_pct = Some(cpu);
            s.machine.mem_available_bytes =
                Some((total * (0.255 - 0.01 * k + 0.004 * f64::from(wave(i, 2)))) as u64);
            s.machine.swap_free_bytes =
                Some((swap_total * (0.34 - 0.07 * k + 0.005 * f64::from(wave(i, 3)))) as u64);
            let spike = if (i / 6) % 9 == 0 {
                12.0 + 9.0 * f64::from(wave(i / 6, 4)).abs()
            } else {
                3.0 + 2.0 * f64::from(wave(i, 5)).abs()
            };
            if let Some(p) = s.machine.psi.as_mut() {
                p.memory.as_mut().unwrap().some_avg10 = Some(spike);
            }
            h.record(&s);
        }
        h.record(snap);
        h
    }

    /// Presents `widget` `width` px wide in an undecorated window and pumps
    /// the main loop until it is laid out.
    fn present_at(widget: &gtk::Widget, width: i32) -> gtk::Window {
        let w = gtk::Window::new();
        w.set_decorated(false);
        widget.set_size_request(width, -1);
        w.set_child(Some(widget));
        w.present();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        let ctx = glib::MainContext::default();
        while std::time::Instant::now() < until {
            while ctx.pending() {
                ctx.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        w
    }

    fn render(widget: &gtk::Widget, width: i32, path: &std::path::Path) {
        let w = present_at(widget, width);
        let (pw, ph) = (w.width(), w.height());
        let paintable = gtk::WidgetPaintable::new(Some(&w));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(pw), f64::from(ph));
        let node = snapshot.to_node().expect("rendered node");
        let texture = w.renderer().expect("renderer").render_texture(&node, None);
        texture.save_to_png(path).expect("png");
        w.destroy();
    }
}
